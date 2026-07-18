use std::{
    fs::{File, OpenOptions},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use reqwest::header::{HeaderMap, HeaderValue};
use serde::Deserialize;
use thiserror::Error;
use tokio::{process::Child, time::sleep};
use uuid::Uuid;

const BRIDGE_CONTROL_TOKEN_HEADER: &str = "x-bridge-control-token";
const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(250);
const DEFAULT_DEBUG_PORT: u16 = 9229;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeProcessStatus {
    Stopped,
    Starting,
    Ready,
    Degraded,
    Failed,
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortPolicy {
    Flexible,
    Fixed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeProcessSnapshot {
    pub status: BridgeProcessStatus,
    pub pid: Option<u32>,
    pub port: Option<u16>,
    pub port_policy: PortPolicy,
    pub health_url: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BridgeProcessConfig {
    pub sidecar_binary: PathBuf,
    pub sidecar_args: Vec<String>,
    pub app_data_dir: PathBuf,
    pub pwa_dist_dir: PathBuf,
    pub db_path: Option<PathBuf>,
    pub preferred_port: Option<u16>,
    pub port_policy: PortPolicy,
    pub bind_ip: IpAddr,
    pub health_ip: IpAddr,
    pub advertised_host: String,
    pub debug_port: u16,
    pub startup_timeout: Duration,
    pub health_poll_interval: Duration,
}

impl BridgeProcessConfig {
    pub fn new(
        sidecar_binary: impl Into<PathBuf>,
        app_data_dir: impl Into<PathBuf>,
        pwa_dist_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            sidecar_binary: sidecar_binary.into(),
            sidecar_args: Vec::new(),
            app_data_dir: app_data_dir.into(),
            pwa_dist_dir: pwa_dist_dir.into(),
            db_path: None,
            preferred_port: Some(57324),
            port_policy: PortPolicy::Flexible,
            bind_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            health_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            advertised_host: Ipv4Addr::LOCALHOST.to_string(),
            debug_port: DEFAULT_DEBUG_PORT,
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            health_poll_interval: DEFAULT_HEALTH_POLL_INTERVAL,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeLaunchPlan {
    pub instance_id: String,
    pub port: u16,
    pub bind_addr: SocketAddr,
    pub health_url: String,
    pub advertised_bridge_url: String,
    pub pairing_start_url: String,
    pub db_path: PathBuf,
    pub pwa_dist_dir: PathBuf,
    pub log_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub stdout_log: PathBuf,
    pub stderr_log: PathBuf,
    pub control_token: String,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Error)]
pub enum BridgeProcessError {
    #[error("bridge sidecar is already running")]
    AlreadyRunning,
    #[error("sidecar binary not found: {0}")]
    SidecarMissing(PathBuf),
    #[error("PWA dist is missing index.html: {0}")]
    PwaDistMissing(PathBuf),
    #[error("bridge database is not writable: {path}: {source}")]
    DbUnavailable {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("bridge process I/O setup failed: {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("preferred bridge port {port} is unavailable")]
    PreferredPortUnavailable { port: u16 },
    #[error("failed to spawn bridge sidecar: {0}")]
    Spawn(std::io::Error),
    #[error("bridge sidecar exited before health became ready: {0}")]
    ChildExited(String),
    #[error("bridge health did not become ready within {0:?}")]
    HealthTimeout(Duration),
    #[error("bridge health request failed: {0}")]
    HealthRequest(#[from] reqwest::Error),
    #[error("bridge control token cannot be sent as a header")]
    InvalidControlToken,
    #[error("bridge pairing start returned invalid response")]
    InvalidPairingResponse,
}

#[derive(Debug, Clone)]
struct ManagerState {
    status: BridgeProcessStatus,
    plan: Option<BridgeLaunchPlan>,
    detail: Option<String>,
}

pub struct BridgeProcessManager {
    config: BridgeProcessConfig,
    control_token: String,
    state: ManagerState,
    child: Option<Child>,
}

impl BridgeProcessManager {
    pub fn new(config: BridgeProcessConfig) -> Self {
        Self {
            config,
            control_token: Uuid::new_v4().to_string(),
            state: ManagerState {
                status: BridgeProcessStatus::Stopped,
                plan: None,
                detail: None,
            },
            child: None,
        }
    }

    pub fn status(&self) -> BridgeProcessSnapshot {
        self.snapshot(self.child.as_ref().and_then(Child::id))
    }

    pub fn control_token(&self) -> &str {
        &self.control_token
    }

    pub fn prepare_launch_plan(&self) -> Result<BridgeLaunchPlan, BridgeProcessError> {
        if !self.config.sidecar_binary.is_file() {
            return Err(BridgeProcessError::SidecarMissing(
                self.config.sidecar_binary.clone(),
            ));
        }
        validate_pwa_dist(&self.config.pwa_dist_dir)?;

        let log_dir = self.config.app_data_dir.join("logs");
        let runtime_dir = self.config.app_data_dir.join("runtime");
        create_dir_all(&self.config.app_data_dir)?;
        create_dir_all(&log_dir)?;
        create_dir_all(&runtime_dir)?;

        let db_path = self
            .config
            .db_path
            .clone()
            .unwrap_or_else(|| self.config.app_data_dir.join("bridge.sqlite"));
        validate_db_writable(&db_path)?;

        let port = choose_port(
            self.config.bind_ip,
            self.config.preferred_port,
            self.config.port_policy,
        )?;
        let instance_id = Uuid::new_v4().to_string();
        let bind_addr = SocketAddr::new(self.config.bind_ip, port);
        let control_bridge_url = format!("http://{}:{port}", host_for_url(self.config.health_ip));
        let health_url = format!("{control_bridge_url}/api/health");
        let advertised_bridge_url = format!("http://{}:{port}", self.config.advertised_host);
        let pairing_start_url = format!("{control_bridge_url}/api/control/pairing/start");
        let stdout_log = log_dir.join("bridge-sidecar.stdout.log");
        let stderr_log = log_dir.join("bridge-sidecar.stderr.log");
        let env = vec![
            (
                "CODEX_MOBILE_BRIDGE_BIND".to_string(),
                bind_addr.to_string(),
            ),
            (
                "CODEX_MOBILE_BRIDGE_DB".to_string(),
                db_path.display().to_string(),
            ),
            (
                "CODEX_MOBILE_BRIDGE_PWA_DIR".to_string(),
                self.config.pwa_dist_dir.display().to_string(),
            ),
            (
                "CODEX_MOBILE_BRIDGE_DEBUG_PORT".to_string(),
                self.config.debug_port.to_string(),
            ),
            (
                "CODEX_MOBILE_BRIDGE_CONTROL_TOKEN".to_string(),
                self.control_token.clone(),
            ),
            (
                "CODEX_MOBILE_BRIDGE_INSTANCE_ID".to_string(),
                instance_id.clone(),
            ),
        ];

        Ok(BridgeLaunchPlan {
            instance_id,
            port,
            bind_addr,
            health_url,
            advertised_bridge_url,
            pairing_start_url,
            db_path,
            pwa_dist_dir: self.config.pwa_dist_dir.clone(),
            log_dir,
            runtime_dir,
            stdout_log,
            stderr_log,
            control_token: self.control_token.clone(),
            env,
        })
    }

    pub async fn start(&mut self) -> Result<BridgeProcessSnapshot, BridgeProcessError> {
        if let Some(child) = self.child.as_mut() {
            if child
                .try_wait()
                .map_err(BridgeProcessError::Spawn)?
                .is_none()
            {
                return Err(BridgeProcessError::AlreadyRunning);
            }
            self.child = None;
        }

        let plan = self.prepare_launch_plan()?;
        self.state = ManagerState {
            status: BridgeProcessStatus::Starting,
            plan: Some(plan.clone()),
            detail: None,
        };

        let stdout = append_log_file(&plan.stdout_log)?;
        let stderr = append_log_file(&plan.stderr_log)?;
        let mut command = tokio::process::Command::new(&self.config.sidecar_binary);
        command
            .kill_on_drop(true)
            .args(&self.config.sidecar_args)
            .envs(plan.env.iter().map(|(key, value)| (key, value)))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));

        let mut child = command.spawn().map_err(BridgeProcessError::Spawn)?;
        let pid = child.id();
        let health = self.wait_for_health(&mut child, &plan).await;
        match health {
            Ok(status) => {
                self.state.status = status;
                self.child = Some(child);
                Ok(self.snapshot(pid))
            }
            Err(error) => {
                self.state.status = BridgeProcessStatus::Failed;
                self.state.detail = Some(error.to_string());
                let _ = child.kill().await;
                Err(error)
            }
        }
    }

    pub async fn stop(&mut self) -> Result<BridgeProcessSnapshot, BridgeProcessError> {
        self.state.status = BridgeProcessStatus::Stopping;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
        }
        self.state.status = BridgeProcessStatus::Stopped;
        self.state.detail = None;
        Ok(self.snapshot(None))
    }

    pub fn terminate_now(&mut self) -> BridgeProcessSnapshot {
        self.state.status = BridgeProcessStatus::Stopping;
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
        self.state.status = BridgeProcessStatus::Stopped;
        self.state.detail = None;
        self.snapshot(None)
    }

    pub async fn restart(&mut self) -> Result<BridgeProcessSnapshot, BridgeProcessError> {
        let _ = self.stop().await?;
        self.start().await
    }

    pub async fn create_pairing_link(&self) -> Result<String, BridgeProcessError> {
        let advertised_bridge_url = self
            .state
            .plan
            .as_ref()
            .ok_or(BridgeProcessError::InvalidPairingResponse)?
            .advertised_bridge_url
            .clone();
        self.create_pairing_link_for_bridge_url(&advertised_bridge_url)
            .await
    }

    pub async fn create_pairing_link_for_bridge_url(
        &self,
        bridge_url: &str,
    ) -> Result<String, BridgeProcessError> {
        let plan = self
            .state
            .plan
            .as_ref()
            .ok_or(BridgeProcessError::InvalidPairingResponse)?;
        let mut headers = HeaderMap::new();
        headers.insert(
            BRIDGE_CONTROL_TOKEN_HEADER,
            HeaderValue::from_str(&plan.control_token)
                .map_err(|_| BridgeProcessError::InvalidControlToken)?,
        );
        let response = reqwest::Client::new()
            .post(&plan.pairing_start_url)
            .headers(headers)
            .send()
            .await?
            .error_for_status()?
            .json::<PairingStartResponse>()
            .await?;

        if response.pairing_token.is_empty() {
            return Err(BridgeProcessError::InvalidPairingResponse);
        }
        Ok(pairing_link_for_bridge_url(
            bridge_url,
            &response.pairing_token,
        ))
    }

    async fn wait_for_health(
        &self,
        child: &mut Child,
        plan: &BridgeLaunchPlan,
    ) -> Result<BridgeProcessStatus, BridgeProcessError> {
        let deadline = Instant::now() + self.config.startup_timeout;
        let client = reqwest::Client::builder()
            .timeout(self.config.health_poll_interval)
            .build()?;

        loop {
            if let Some(status) = child.try_wait().map_err(BridgeProcessError::Spawn)? {
                return Err(BridgeProcessError::ChildExited(status.to_string()));
            }

            match client.get(&plan.health_url).send().await {
                Ok(response) if response.status().is_success() => {
                    let health = response.json::<HealthResponse>().await?;
                    return Ok(if health.status == "ok" {
                        BridgeProcessStatus::Ready
                    } else {
                        BridgeProcessStatus::Degraded
                    });
                }
                _ => {}
            }

            if Instant::now() >= deadline {
                return Err(BridgeProcessError::HealthTimeout(
                    self.config.startup_timeout,
                ));
            }
            sleep(self.config.health_poll_interval).await;
        }
    }

    fn snapshot(&self, pid: Option<u32>) -> BridgeProcessSnapshot {
        BridgeProcessSnapshot {
            status: self.state.status,
            pid,
            port: self.state.plan.as_ref().map(|plan| plan.port),
            port_policy: self.config.port_policy,
            health_url: self.state.plan.as_ref().map(|plan| plan.health_url.clone()),
            detail: self.state.detail.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    status: String,
    #[allow(dead_code)]
    connection_state: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairingStartResponse {
    pairing_token: String,
    #[allow(dead_code)]
    expires_in_ms: u64,
}

fn validate_pwa_dist(path: &Path) -> Result<(), BridgeProcessError> {
    let index = path.join("index.html");
    if index.is_file() {
        Ok(())
    } else {
        Err(BridgeProcessError::PwaDistMissing(index))
    }
}

fn validate_db_writable(path: &Path) -> Result<(), BridgeProcessError> {
    if let Some(parent) = path.parent() {
        create_dir_all(parent)?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map(|_| ())
        .map_err(|source| BridgeProcessError::DbUnavailable {
            path: path.to_path_buf(),
            source,
        })
}

fn create_dir_all(path: &Path) -> Result<(), BridgeProcessError> {
    std::fs::create_dir_all(path).map_err(|source| BridgeProcessError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn append_log_file(path: &Path) -> Result<File, BridgeProcessError> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| BridgeProcessError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn choose_port(
    bind_ip: IpAddr,
    preferred_port: Option<u16>,
    policy: PortPolicy,
) -> Result<u16, BridgeProcessError> {
    if let Some(port) = preferred_port {
        if TcpListener::bind(SocketAddr::new(bind_ip, port)).is_ok() {
            return Ok(port);
        }
        if policy == PortPolicy::Fixed {
            return Err(BridgeProcessError::PreferredPortUnavailable { port });
        }
    }
    let listener = TcpListener::bind(SocketAddr::new(bind_ip, 0)).map_err(|source| {
        BridgeProcessError::Io {
            path: PathBuf::from(format!("{bind_ip}:0")),
            source,
        }
    })?;
    Ok(listener
        .local_addr()
        .map_err(|source| BridgeProcessError::Io {
            path: PathBuf::from(format!("{bind_ip}:0")),
            source,
        })?
        .port())
}

fn host_for_url(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    }
}

fn url_encode_component(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

pub fn pairing_link_for_bridge_url(bridge_url: &str, pairing_token: &str) -> String {
    let bridge_url = bridge_url.trim_end_matches('/');
    format!(
        "{bridge_url}/?pairingToken={}&bridgeUrl={}",
        url_encode_component(pairing_token),
        url_encode_component(bridge_url)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use tempfile::TempDir;

    fn test_config(temp: &TempDir) -> BridgeProcessConfig {
        let pwa_dist = temp.path().join("pwa-dist");
        std::fs::create_dir_all(&pwa_dist).expect("pwa dist dir creates");
        std::fs::write(pwa_dist.join("index.html"), "").expect("index writes");
        let mut config =
            BridgeProcessConfig::new("/bin/sh", temp.path().join("app-data"), pwa_dist);
        config.preferred_port = None;
        config.startup_timeout = Duration::from_millis(300);
        config.health_poll_interval = Duration::from_millis(25);
        config
    }

    #[test]
    fn launch_plan_uses_free_port_when_preferred_port_is_occupied() {
        let temp = TempDir::new().expect("temp dir creates");
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("test listener binds");
        let occupied_port = listener.local_addr().expect("listener addr").port();
        let mut config = test_config(&temp);
        config.preferred_port = Some(occupied_port);
        config.port_policy = PortPolicy::Flexible;
        let manager = BridgeProcessManager::new(config);

        let plan = manager.prepare_launch_plan().expect("launch plan prepares");

        assert_ne!(plan.port, occupied_port);
        assert_eq!(
            plan.env_value("CODEX_MOBILE_BRIDGE_BIND"),
            Some(format!("127.0.0.1:{}", plan.port))
        );
    }

    #[test]
    fn launch_plan_rejects_an_occupied_fixed_port() {
        let temp = TempDir::new().expect("temp dir creates");
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("test listener binds");
        let occupied_port = listener.local_addr().expect("listener addr").port();
        let mut config = test_config(&temp);
        config.preferred_port = Some(occupied_port);
        config.port_policy = PortPolicy::Fixed;
        let manager = BridgeProcessManager::new(config);

        let error = manager
            .prepare_launch_plan()
            .expect_err("fixed occupied port must fail");

        assert!(matches!(
            error,
            BridgeProcessError::PreferredPortUnavailable { port } if port == occupied_port
        ));
    }

    #[test]
    fn snapshot_reports_configured_port_policy() {
        let flexible_temp = TempDir::new().expect("flexible temp dir creates");
        let flexible_manager = BridgeProcessManager::new(test_config(&flexible_temp));

        assert_eq!(flexible_manager.status().port_policy, PortPolicy::Flexible);

        let fixed_temp = TempDir::new().expect("fixed temp dir creates");
        let mut fixed_config = test_config(&fixed_temp);
        fixed_config.port_policy = PortPolicy::Fixed;
        let fixed_manager = BridgeProcessManager::new(fixed_config);

        assert_eq!(fixed_manager.status().port_policy, PortPolicy::Fixed);
    }

    #[test]
    fn launch_plan_fails_when_pwa_dist_is_missing() {
        let temp = TempDir::new().expect("temp dir creates");
        let config = BridgeProcessConfig::new(
            "/bin/sh",
            temp.path().join("app-data"),
            temp.path().join("missing-dist"),
        );
        let manager = BridgeProcessManager::new(config);

        let error = manager
            .prepare_launch_plan()
            .expect_err("missing PWA dist fails");

        assert!(matches!(error, BridgeProcessError::PwaDistMissing(_)));
    }

    #[test]
    fn launch_plan_fails_when_db_path_is_not_writable() {
        let temp = TempDir::new().expect("temp dir creates");
        let mut config = test_config(&temp);
        let db_as_dir = temp.path().join("db-as-dir");
        std::fs::create_dir_all(&db_as_dir).expect("db dir creates");
        config.db_path = Some(db_as_dir);
        let manager = BridgeProcessManager::new(config);

        let error = manager
            .prepare_launch_plan()
            .expect_err("directory DB path fails");

        assert!(matches!(error, BridgeProcessError::DbUnavailable { .. }));
    }

    #[test]
    fn launch_plan_injects_control_env_without_stdout_protocol() {
        let temp = TempDir::new().expect("temp dir creates");
        let manager = BridgeProcessManager::new(test_config(&temp));

        let plan = manager.prepare_launch_plan().expect("launch plan prepares");

        assert_eq!(
            plan.env_value("CODEX_MOBILE_BRIDGE_CONTROL_TOKEN"),
            Some(manager.control_token().to_string())
        );
        assert_eq!(
            plan.env_value("CODEX_MOBILE_BRIDGE_PWA_DIR"),
            Some(plan.pwa_dist_dir.display().to_string())
        );
        assert_eq!(
            plan.env_value("CODEX_MOBILE_BRIDGE_DB"),
            Some(plan.db_path.display().to_string())
        );
    }

    #[test]
    fn launch_plan_refreshes_instance_id_for_each_prepare() {
        let temp = TempDir::new().expect("temp dir creates");
        let manager = BridgeProcessManager::new(test_config(&temp));

        let first = manager.prepare_launch_plan().expect("first plan prepares");
        let second = manager.prepare_launch_plan().expect("second plan prepares");

        assert_ne!(first.instance_id, second.instance_id);
        assert_eq!(
            first.env_value("CODEX_MOBILE_BRIDGE_INSTANCE_ID"),
            Some(first.instance_id.clone())
        );
        assert_eq!(
            second.env_value("CODEX_MOBILE_BRIDGE_INSTANCE_ID"),
            Some(second.instance_id.clone())
        );
    }

    #[test]
    fn control_routes_use_health_host_not_advertised_phone_host() {
        let temp = TempDir::new().expect("temp dir creates");
        let mut config = test_config(&temp);
        config.advertised_host = "192.168.68.181".to_string();
        let manager = BridgeProcessManager::new(config);

        let plan = manager.prepare_launch_plan().expect("launch plan prepares");

        assert!(plan.health_url.starts_with("http://127.0.0.1:"));
        assert!(plan.pairing_start_url.starts_with("http://127.0.0.1:"));
        assert!(
            plan.advertised_bridge_url
                .starts_with("http://192.168.68.181:")
        );
    }

    #[test]
    fn pairing_link_can_target_rotated_tunnel_url() {
        let link = pairing_link_for_bridge_url(
            "https://mobile-codex.trycloudflare.com/",
            "token with spaces",
        );

        assert_eq!(
            link,
            "https://mobile-codex.trycloudflare.com/?pairingToken=token%20with%20spaces&bridgeUrl=https%3A%2F%2Fmobile-codex.trycloudflare.com"
        );
    }

    #[tokio::test]
    async fn start_marks_failed_when_child_exits_before_health_ready() {
        let temp = TempDir::new().expect("temp dir creates");
        let mut config = test_config(&temp);
        config.sidecar_args = vec!["-c".to_string(), "exit 42".to_string()];
        let mut manager = BridgeProcessManager::new(config);

        let error = manager.start().await.expect_err("dead child fails start");

        assert!(matches!(error, BridgeProcessError::ChildExited(_)));
        let snapshot = manager.status();
        assert_eq!(snapshot.status, BridgeProcessStatus::Failed);
        assert!(snapshot.detail.expect("failure detail").contains("exit"));
    }

    #[tokio::test]
    async fn dropping_manager_terminates_running_sidecar() {
        let temp = TempDir::new().expect("temp dir creates");
        let mut config = test_config(&temp);
        config.sidecar_binary = std::env::current_exe().expect("test executable resolves");
        config.sidecar_args = vec![
            "--exact".to_string(),
            "bridge_process::tests::bridge_process_drop_test_child".to_string(),
            "--nocapture".to_string(),
        ];
        config.startup_timeout = Duration::from_secs(2);

        let pid = {
            let mut manager = BridgeProcessManager::new(config);
            manager
                .start()
                .await
                .expect("test sidecar becomes healthy")
                .pid
                .expect("test sidecar has a pid")
        };

        let stopped = wait_for_process_exit(pid).await;
        if !stopped {
            terminate_process(pid);
        }
        assert!(stopped, "sidecar {pid} survived manager drop");
    }

    #[tokio::test]
    async fn terminate_now_stops_running_sidecar_without_async_wait() {
        let temp = TempDir::new().expect("temp dir creates");
        let mut config = test_config(&temp);
        config.sidecar_binary = std::env::current_exe().expect("test executable resolves");
        config.sidecar_args = vec![
            "--exact".to_string(),
            "bridge_process::tests::bridge_process_drop_test_child".to_string(),
            "--nocapture".to_string(),
        ];
        config.startup_timeout = Duration::from_secs(2);
        let mut manager = BridgeProcessManager::new(config);
        let pid = manager
            .start()
            .await
            .expect("test sidecar becomes healthy")
            .pid
            .expect("test sidecar has a pid");

        let snapshot = manager.terminate_now();

        assert_eq!(snapshot.status, BridgeProcessStatus::Stopped);
        let stopped = wait_for_process_exit(pid).await;
        if !stopped {
            terminate_process(pid);
        }
        assert!(stopped, "sidecar {pid} survived synchronous termination");
    }

    #[test]
    fn bridge_process_drop_test_child() {
        if std::env::var_os("CODEX_MOBILE_BRIDGE_CONTROL_TOKEN").is_none() {
            return;
        }

        let bind_addr = std::env::var("CODEX_MOBILE_BRIDGE_BIND")
            .expect("bridge bind env exists")
            .parse::<SocketAddr>()
            .expect("bridge bind env parses");
        let listener = TcpListener::bind(bind_addr).expect("test sidecar binds");
        let body = r#"{"status":"ok","connectionState":"writable"}"#;

        for stream in listener.incoming() {
            let mut stream = stream.expect("health connection accepts");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("health response writes");
        }
    }

    async fn wait_for_process_exit(pid: u32) -> bool {
        for _ in 0..50 {
            if !process_is_running(pid) {
                return true;
            }
            sleep(Duration::from_millis(20)).await;
        }
        false
    }

    fn process_is_running(pid: u32) -> bool {
        std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn terminate_process(pid: u32) {
        let _ = std::process::Command::new("/bin/kill")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    trait LaunchPlanExt {
        fn env_value(&self, key: &str) -> Option<String>;
    }

    impl LaunchPlanExt for BridgeLaunchPlan {
        fn env_value(&self, key: &str) -> Option<String> {
            self.env
                .iter()
                .find_map(|(env_key, value)| (env_key == key).then(|| value.clone()))
        }
    }
}
