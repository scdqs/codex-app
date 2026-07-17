use std::{
    env,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use reqwest::Client;
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::{Child, Command},
    sync::mpsc,
    time::{Instant, sleep, timeout},
};
use uuid::Uuid;

const DEFAULT_TUNNEL_STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_TUNNEL_POLL_INTERVAL: Duration = Duration::from_millis(100);
const DEFAULT_TUNNEL_HEALTH_TIMEOUT: Duration = Duration::from_secs(3);
const TUNNEL_HEALTH_FAILURES_BEFORE_ROTATE: u32 = 3;
const LOCAL_URL_PLACEHOLDER: &str = "{local_url}";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelStatus {
    Stopped,
    Starting,
    Ready,
    Reconnecting,
    Failed,
    Stopping,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelSession {
    pub id: String,
    pub local_url: String,
    pub public_url: String,
    pub started_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelSnapshot {
    pub status: TunnelStatus,
    pub session: Option<TunnelSession>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone)]
pub struct QuickTunnelConfig {
    pub binary: PathBuf,
    pub args_template: Vec<String>,
    pub startup_timeout: Duration,
    pub poll_interval: Duration,
}

impl Default for QuickTunnelConfig {
    fn default() -> Self {
        Self {
            binary: choose_tunnel_binary(
                env::var_os("CODEX_MOBILE_BRIDGE_TUNNEL_BIN").map(PathBuf::from),
                &default_tunnel_binary_candidates(),
            ),
            args_template: vec![
                "tunnel".to_string(),
                "--url".to_string(),
                LOCAL_URL_PLACEHOLDER.to_string(),
                "--no-autoupdate".to_string(),
            ],
            startup_timeout: DEFAULT_TUNNEL_STARTUP_TIMEOUT,
            poll_interval: DEFAULT_TUNNEL_POLL_INTERVAL,
        }
    }
}

#[derive(Debug, Error)]
pub enum TunnelError {
    #[error("tunnel is already running")]
    AlreadyRunning,
    #[error("tunnel is not running")]
    NotRunning,
    #[error("failed to spawn tunnel provider: {0}")]
    Spawn(std::io::Error),
    #[error("failed to read tunnel provider output: {0}")]
    Output(std::io::Error),
    #[error("tunnel provider exited before a public URL was available: {0}")]
    ChildExited(String),
    #[error("tunnel provider did not print a trycloudflare.com URL within {0:?}")]
    PublicUrlTimeout(Duration),
    #[error("public tunnel health did not become ready within {0:?}: {1}")]
    PublicHealthTimeout(Duration, String),
}

#[async_trait]
pub trait TunnelHealthProbe: Send + Sync {
    async fn check(&self, public_url: &str) -> Result<(), String>;
}

struct HttpTunnelHealthProbe {
    client: Client,
}

impl HttpTunnelHealthProbe {
    fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(DEFAULT_TUNNEL_HEALTH_TIMEOUT)
                .build()
                .expect("tunnel health HTTP client configuration is valid"),
        }
    }
}

#[async_trait]
impl TunnelHealthProbe for HttpTunnelHealthProbe {
    async fn check(&self, public_url: &str) -> Result<(), String> {
        let health_url = format!("{}/api/health", public_url.trim_end_matches('/'));
        let response = self
            .client
            .get(&health_url)
            .send()
            .await
            .map_err(|error| format!("{health_url}: {error}"))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!("{health_url} returned {}", response.status()))
        }
    }
}

#[derive(Debug, Clone)]
struct TunnelState {
    status: TunnelStatus,
    session: Option<TunnelSession>,
    detail: Option<String>,
}

pub struct QuickTunnelManager {
    config: QuickTunnelConfig,
    health_probe: Arc<dyn TunnelHealthProbe>,
    state: TunnelState,
    child: Option<Child>,
    consecutive_health_failures: u32,
}

impl QuickTunnelManager {
    pub fn new(config: QuickTunnelConfig) -> Self {
        Self::with_health_probe(config, Arc::new(HttpTunnelHealthProbe::new()))
    }

    pub fn with_health_probe(
        config: QuickTunnelConfig,
        health_probe: Arc<dyn TunnelHealthProbe>,
    ) -> Self {
        Self {
            config,
            health_probe,
            state: TunnelState {
                status: TunnelStatus::Stopped,
                session: None,
                detail: None,
            },
            child: None,
            consecutive_health_failures: 0,
        }
    }

    pub fn config(&self) -> &QuickTunnelConfig {
        &self.config
    }

    pub fn status(&mut self) -> TunnelSnapshot {
        self.reconcile_child_status();
        self.snapshot()
    }

    pub async fn refresh_status(&mut self) -> TunnelSnapshot {
        self.reconcile_child_status();
        if self.child.is_none()
            || !matches!(
                self.state.status,
                TunnelStatus::Ready | TunnelStatus::Reconnecting
            )
        {
            return self.snapshot();
        }
        let Some(public_url) = self
            .state
            .session
            .as_ref()
            .map(|session| session.public_url.clone())
        else {
            return self.snapshot();
        };

        match self.health_probe.check(&public_url).await {
            Ok(()) => {
                self.consecutive_health_failures = 0;
                self.state.status = TunnelStatus::Ready;
                self.state.detail = None;
            }
            Err(error) => {
                self.consecutive_health_failures =
                    self.consecutive_health_failures.saturating_add(1);
                self.state.status = TunnelStatus::Reconnecting;
                let guidance =
                    if self.consecutive_health_failures >= TUNNEL_HEALTH_FAILURES_BEFORE_ROTATE {
                        "Use 换链接 if the connection does not recover."
                    } else {
                        "Retrying automatically."
                    };
                self.state.detail = Some(format!(
                    "public tunnel health check failed ({}/{}): {error}. {guidance}",
                    self.consecutive_health_failures, TUNNEL_HEALTH_FAILURES_BEFORE_ROTATE
                ));
            }
        }
        self.snapshot()
    }

    pub async fn start(
        &mut self,
        local_url: impl Into<String>,
    ) -> Result<TunnelSnapshot, TunnelError> {
        if self.running_child()? {
            return Err(TunnelError::AlreadyRunning);
        }
        self.child = None;
        self.consecutive_health_failures = 0;

        let local_url = local_url.into();
        let args = self.launch_args(&local_url);
        self.state = TunnelState {
            status: TunnelStatus::Starting,
            session: None,
            detail: None,
        };

        let mut command = Command::new(&self.config.binary);
        command
            .kill_on_drop(true)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let error = TunnelError::Spawn(error);
                self.state = TunnelState {
                    status: TunnelStatus::Failed,
                    session: None,
                    detail: Some(error.to_string()),
                };
                return Err(error);
            }
        };
        let mut output = tunnel_output_lines(&mut child);

        let public_url = match self.wait_for_public_url(&mut child, &mut output).await {
            Ok(public_url) => public_url,
            Err(error) => return self.fail_start(child, error).await,
        };
        drain_tunnel_output(output);
        if let Err(error) = self.wait_for_public_health(&mut child, &public_url).await {
            return self.fail_start(child, error).await;
        }

        let session = TunnelSession {
            id: Uuid::new_v4().to_string(),
            local_url,
            public_url,
            started_at: current_time_ms(),
        };
        self.state = TunnelState {
            status: TunnelStatus::Ready,
            session: Some(session),
            detail: None,
        };
        self.consecutive_health_failures = 0;
        self.child = Some(child);
        Ok(self.snapshot())
    }

    pub async fn stop(&mut self) -> Result<TunnelSnapshot, TunnelError> {
        self.state.status = TunnelStatus::Stopping;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
        }
        self.state = TunnelState {
            status: TunnelStatus::Stopped,
            session: None,
            detail: None,
        };
        self.consecutive_health_failures = 0;
        Ok(self.snapshot())
    }

    pub fn terminate_now(&mut self) -> TunnelSnapshot {
        self.state.status = TunnelStatus::Stopping;
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
        self.state = TunnelState {
            status: TunnelStatus::Stopped,
            session: None,
            detail: None,
        };
        self.consecutive_health_failures = 0;
        self.snapshot()
    }

    pub async fn rotate(&mut self) -> Result<TunnelSnapshot, TunnelError> {
        let local_url = self
            .state
            .session
            .as_ref()
            .map(|session| session.local_url.clone())
            .ok_or(TunnelError::NotRunning)?;
        let _ = self.stop().await?;
        self.start(local_url).await
    }

    pub fn launch_args(&self, local_url: &str) -> Vec<String> {
        let mut replaced = false;
        let mut args = self
            .config
            .args_template
            .iter()
            .map(|arg| {
                if arg == LOCAL_URL_PLACEHOLDER {
                    replaced = true;
                    local_url.to_string()
                } else {
                    arg.replace(LOCAL_URL_PLACEHOLDER, local_url)
                }
            })
            .collect::<Vec<_>>();

        if !replaced
            && !self
                .config
                .args_template
                .iter()
                .any(|arg| arg.contains(LOCAL_URL_PLACEHOLDER))
        {
            args.push(local_url.to_string());
        }

        args
    }

    async fn wait_for_public_url(
        &self,
        child: &mut Child,
        output: &mut mpsc::Receiver<Result<String, std::io::Error>>,
    ) -> Result<String, TunnelError> {
        let deadline = Instant::now() + self.config.startup_timeout;

        loop {
            if let Some(status) = child.try_wait().map_err(TunnelError::Spawn)? {
                return Err(TunnelError::ChildExited(status.to_string()));
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(TunnelError::PublicUrlTimeout(self.config.startup_timeout));
            }
            let wait_for = remaining.min(self.config.poll_interval);

            match timeout(wait_for, output.recv()).await {
                Ok(Some(Ok(line))) => {
                    if let Some(url) = parse_quick_tunnel_url(&line) {
                        return Ok(url);
                    }
                }
                Ok(Some(Err(error))) => return Err(TunnelError::Output(error)),
                Ok(None) => sleep(self.config.poll_interval).await,
                Err(_) => {}
            }
        }
    }

    async fn wait_for_public_health(
        &self,
        child: &mut Child,
        public_url: &str,
    ) -> Result<(), TunnelError> {
        let deadline = Instant::now() + self.config.startup_timeout;

        loop {
            if let Some(status) = child.try_wait().map_err(TunnelError::Spawn)? {
                return Err(TunnelError::ChildExited(status.to_string()));
            }

            let health_error = match self.health_probe.check(public_url).await {
                Ok(()) => return Ok(()),
                Err(error) => error,
            };

            if Instant::now() >= deadline {
                return Err(TunnelError::PublicHealthTimeout(
                    self.config.startup_timeout,
                    health_error,
                ));
            }
            sleep(self.config.poll_interval).await;
        }
    }

    async fn fail_start(
        &mut self,
        mut child: Child,
        error: TunnelError,
    ) -> Result<TunnelSnapshot, TunnelError> {
        self.state = TunnelState {
            status: TunnelStatus::Failed,
            session: None,
            detail: Some(error.to_string()),
        };
        self.consecutive_health_failures = 0;
        let _ = child.kill().await;
        Err(error)
    }

    fn running_child(&mut self) -> Result<bool, TunnelError> {
        if let Some(child) = self.child.as_mut()
            && child.try_wait().map_err(TunnelError::Spawn)?.is_none()
        {
            return Ok(true);
        }
        Ok(false)
    }

    fn reconcile_child_status(&mut self) {
        if let Some(child) = self.child.as_mut()
            && let Ok(Some(status)) = child.try_wait()
        {
            self.child = None;
            self.state.status = TunnelStatus::Failed;
            self.state.session = None;
            self.state.detail = Some(format!("tunnel provider exited: {status}"));
            self.consecutive_health_failures = 0;
        }
    }

    fn snapshot(&self) -> TunnelSnapshot {
        TunnelSnapshot {
            status: self.state.status,
            session: self.state.session.clone(),
            detail: self.state.detail.clone(),
        }
    }
}

fn choose_tunnel_binary(env_path: Option<PathBuf>, common_paths: &[PathBuf]) -> PathBuf {
    if let Some(path) = env_path {
        return path;
    }
    common_paths
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .unwrap_or_else(|| PathBuf::from("cloudflared"))
}

fn default_tunnel_binary_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(current_exe) = env::current_exe()
        && let Some(bundled) = bundled_tunnel_binary_from_exe(&current_exe)
    {
        candidates.push(bundled);
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin/cloudflared"));
    candidates.push(PathBuf::from("/usr/local/bin/cloudflared"));
    candidates
}

fn bundled_tunnel_binary_from_exe(current_exe: &Path) -> Option<PathBuf> {
    Some(
        current_exe
            .parent()?
            .parent()?
            .join("Resources/bin/cloudflared"),
    )
}

pub fn parse_quick_tunnel_url(line: &str) -> Option<String> {
    line.split(|character: char| {
        character.is_whitespace()
            || matches!(
                character,
                '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}'
            )
    })
    .map(|candidate| {
        candidate
            .trim_matches(|character: char| matches!(character, ',' | '.' | ';' | ':' | '!' | '?'))
    })
    .find(|candidate| {
        candidate.starts_with("https://")
            && (candidate.ends_with(".trycloudflare.com")
                || candidate.contains(".trycloudflare.com/"))
    })
    .map(ToString::to_string)
}

fn tunnel_output_lines(child: &mut Child) -> mpsc::Receiver<Result<String, std::io::Error>> {
    let (sender, receiver) = mpsc::channel(64);
    if let Some(stdout) = child.stdout.take() {
        spawn_line_reader(stdout, sender.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_line_reader(stderr, sender);
    }
    receiver
}

fn drain_tunnel_output(mut output: mpsc::Receiver<Result<String, std::io::Error>>) {
    tokio::spawn(async move { while output.recv().await.is_some() {} });
}

fn spawn_line_reader<R>(reader: R, sender: mpsc::Sender<Result<String, std::io::Error>>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if sender.send(Ok(line)).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    let _ = sender.send(Err(error)).await;
                    break;
                }
            }
        }
    });
}

fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time is after unix epoch")
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::{collections::VecDeque, sync::Arc};
    use tempfile::tempdir;
    use tokio::sync::Mutex;

    struct SequenceHealthProbe {
        results: Mutex<VecDeque<Result<(), String>>>,
    }

    impl SequenceHealthProbe {
        fn new(results: impl IntoIterator<Item = Result<(), String>>) -> Self {
            Self {
                results: Mutex::new(results.into_iter().collect()),
            }
        }
    }

    #[async_trait]
    impl TunnelHealthProbe for SequenceHealthProbe {
        async fn check(&self, _public_url: &str) -> Result<(), String> {
            self.results.lock().await.pop_front().unwrap_or(Ok(()))
        }
    }

    fn shell_config(script: &str) -> QuickTunnelConfig {
        QuickTunnelConfig {
            binary: PathBuf::from("/bin/sh"),
            args_template: vec!["-c".to_string(), script.to_string()],
            startup_timeout: Duration::from_millis(300),
            poll_interval: Duration::from_millis(10),
        }
    }

    fn healthy_shell_manager(script: &str) -> QuickTunnelManager {
        QuickTunnelManager::with_health_probe(
            shell_config(script),
            Arc::new(SequenceHealthProbe::new([Ok(())])),
        )
    }

    #[test]
    fn parses_quick_tunnel_url_from_cloudflared_logs() {
        let line = "INF | https://mobile-codex.trycloudflare.com | Visit it from your phone.";

        assert_eq!(
            parse_quick_tunnel_url(line),
            Some("https://mobile-codex.trycloudflare.com".to_string())
        );
        assert_eq!(parse_quick_tunnel_url("https://example.com"), None);
    }

    #[test]
    fn tunnel_binary_prefers_env_override() {
        assert_eq!(
            choose_tunnel_binary(
                Some(PathBuf::from("/custom/cloudflared")),
                &[PathBuf::from("/opt/homebrew/bin/cloudflared")]
            ),
            PathBuf::from("/custom/cloudflared")
        );
    }

    #[test]
    fn tunnel_binary_uses_first_existing_common_path() {
        let dir = tempdir().expect("tempdir");
        let missing = dir.path().join("missing-cloudflared");
        let existing = dir.path().join("cloudflared");
        std::fs::write(&existing, "provider").expect("write provider");

        assert_eq!(
            choose_tunnel_binary(None, &[missing, existing.clone()]),
            existing
        );
    }

    #[test]
    fn bundled_tunnel_binary_resolves_from_macos_app_executable() {
        let executable =
            PathBuf::from("/Applications/Codex Mobile Bridge.app/Contents/MacOS/desktop-shell");

        assert_eq!(
            bundled_tunnel_binary_from_exe(&executable),
            Some(PathBuf::from(
                "/Applications/Codex Mobile Bridge.app/Contents/Resources/bin/cloudflared"
            ))
        );
    }

    #[test]
    fn launch_args_replace_local_url_placeholder() {
        let manager = QuickTunnelManager::new(QuickTunnelConfig::default());

        assert_eq!(
            manager.launch_args("http://127.0.0.1:57324"),
            vec![
                "tunnel",
                "--url",
                "http://127.0.0.1:57324",
                "--no-autoupdate"
            ]
        );
    }

    #[tokio::test]
    async fn start_returns_ready_session_from_provider_stderr_url() {
        let mut manager =
            healthy_shell_manager("echo 'INF https://first.trycloudflare.com' >&2; sleep 5");

        let snapshot = manager
            .start("http://127.0.0.1:57324")
            .await
            .expect("tunnel starts");

        assert_eq!(snapshot.status, TunnelStatus::Ready);
        let session = snapshot.session.expect("session is present");
        assert_eq!(session.local_url, "http://127.0.0.1:57324");
        assert_eq!(session.public_url, "https://first.trycloudflare.com");

        let stopped = manager.stop().await.expect("tunnel stops");
        assert_eq!(stopped.status, TunnelStatus::Stopped);
    }

    #[tokio::test]
    async fn dropping_manager_terminates_running_provider() {
        let pid = {
            let mut manager = healthy_shell_manager(
                "echo 'INF https://drop-test.trycloudflare.com' >&2; exec sleep 30",
            );
            manager
                .start("http://127.0.0.1:57324")
                .await
                .expect("tunnel starts");
            manager
                .child
                .as_ref()
                .and_then(Child::id)
                .expect("tunnel provider has a pid")
        };

        let stopped = wait_for_process_exit(pid).await;
        if !stopped {
            terminate_process(pid);
        }
        assert!(stopped, "tunnel provider {pid} survived manager drop");
    }

    #[tokio::test]
    async fn terminate_now_stops_running_provider_without_async_wait() {
        let mut manager = healthy_shell_manager(
            "echo 'INF https://terminate-test.trycloudflare.com' >&2; exec sleep 30",
        );
        manager
            .start("http://127.0.0.1:57324")
            .await
            .expect("tunnel starts");
        let pid = manager
            .child
            .as_ref()
            .and_then(Child::id)
            .expect("tunnel provider has a pid");

        let snapshot = manager.terminate_now();

        assert_eq!(snapshot.status, TunnelStatus::Stopped);
        let stopped = wait_for_process_exit(pid).await;
        if !stopped {
            terminate_process(pid);
        }
        assert!(
            stopped,
            "tunnel provider {pid} survived synchronous termination"
        );
    }

    #[tokio::test]
    async fn start_waits_for_public_health_before_ready() {
        let probe = Arc::new(SequenceHealthProbe::new([
            Err("edge returned 502".to_string()),
            Ok(()),
        ]));
        let mut manager = QuickTunnelManager::with_health_probe(
            shell_config("echo 'INF https://warming.trycloudflare.com' >&2; sleep 5"),
            probe.clone(),
        );

        let snapshot = manager
            .start("http://127.0.0.1:57324")
            .await
            .expect("tunnel becomes healthy");

        assert_eq!(snapshot.status, TunnelStatus::Ready);
        assert!(probe.results.lock().await.is_empty());
        let _ = manager.stop().await;
    }

    #[tokio::test]
    async fn running_tunnel_reconnects_after_a_public_health_blip() {
        let probe = Arc::new(SequenceHealthProbe::new([
            Ok(()),
            Err("edge returned 502".to_string()),
            Ok(()),
        ]));
        let mut manager = QuickTunnelManager::with_health_probe(
            shell_config("echo 'INF https://steady.trycloudflare.com' >&2; sleep 5"),
            probe,
        );
        manager
            .start("http://127.0.0.1:57324")
            .await
            .expect("tunnel starts healthy");

        let reconnecting = manager.refresh_status().await;

        assert_eq!(reconnecting.status, TunnelStatus::Reconnecting);
        assert_eq!(
            reconnecting
                .session
                .as_ref()
                .map(|session| session.public_url.as_str()),
            Some("https://steady.trycloudflare.com")
        );
        assert!(
            reconnecting
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("502"))
        );

        let recovered = manager.refresh_status().await;

        assert_eq!(recovered.status, TunnelStatus::Ready);
        assert_eq!(recovered.detail, None);
        let _ = manager.stop().await;
    }

    #[tokio::test]
    async fn start_fails_when_provider_never_prints_public_url() {
        let mut manager = QuickTunnelManager::new(shell_config(
            "while true; do echo 'INF waiting for tunnel' >&2; sleep 0.02; done",
        ));

        let error = manager
            .start("http://127.0.0.1:57324")
            .await
            .expect_err("missing tunnel URL fails");

        assert!(matches!(error, TunnelError::PublicUrlTimeout(_)));
        assert_eq!(manager.status().status, TunnelStatus::Failed);
    }

    #[tokio::test]
    async fn start_marks_failed_when_provider_binary_is_missing() {
        let mut manager = QuickTunnelManager::new(QuickTunnelConfig {
            binary: PathBuf::from("/definitely/missing/cloudflared"),
            args_template: QuickTunnelConfig::default().args_template,
            startup_timeout: Duration::from_millis(300),
            poll_interval: Duration::from_millis(10),
        });

        let error = manager
            .start("http://127.0.0.1:57324")
            .await
            .expect_err("missing provider fails");
        let snapshot = manager.status();

        assert!(matches!(error, TunnelError::Spawn(_)));
        assert_eq!(snapshot.status, TunnelStatus::Failed);
        assert!(snapshot.detail.expect("detail").contains("failed to spawn"));
    }

    #[tokio::test]
    async fn status_clears_session_when_provider_exits_after_ready() {
        let mut manager =
            healthy_shell_manager("echo 'INF https://done.trycloudflare.com' >&2; sleep 0.05");

        let ready = manager
            .start("http://127.0.0.1:57324")
            .await
            .expect("tunnel starts");
        assert_eq!(ready.status, TunnelStatus::Ready);
        assert!(ready.session.is_some());

        sleep(Duration::from_millis(100)).await;
        let failed = manager.status();

        assert_eq!(failed.status, TunnelStatus::Failed);
        assert_eq!(failed.session, None);
        assert!(
            failed
                .detail
                .expect("detail is present")
                .contains("tunnel provider exited")
        );
    }

    #[tokio::test]
    async fn keeps_provider_running_after_public_url_when_logs_continue() {
        let mut manager = healthy_shell_manager(
            "echo 'INF https://steady.trycloudflare.com' >&2; i=0; while [ $i -lt 30 ]; do echo \"INF still running $i\" >&2; i=$((i+1)); sleep 0.01; done; sleep 5",
        );

        let ready = manager
            .start("http://127.0.0.1:57324")
            .await
            .expect("tunnel starts");
        assert_eq!(ready.status, TunnelStatus::Ready);

        sleep(Duration::from_millis(200)).await;
        let snapshot = manager.status();

        assert_eq!(snapshot.status, TunnelStatus::Ready);
        assert_eq!(
            snapshot.session.expect("session remains").public_url,
            "https://steady.trycloudflare.com"
        );

        let _ = manager.stop().await;
    }

    #[tokio::test]
    async fn rotate_restarts_tunnel_for_existing_local_url() {
        let mut manager =
            healthy_shell_manager("echo 'INF https://rotated.trycloudflare.com' >&2; sleep 5");

        let first = manager
            .start("http://127.0.0.1:57324")
            .await
            .expect("first tunnel starts")
            .session
            .expect("first session");
        let rotated = manager
            .rotate()
            .await
            .expect("tunnel rotates")
            .session
            .expect("rotated session");

        assert_eq!(rotated.local_url, first.local_url);
        assert_eq!(rotated.public_url, "https://rotated.trycloudflare.com");
        assert_ne!(rotated.id, first.id);

        let _ = manager.stop().await;
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
}
