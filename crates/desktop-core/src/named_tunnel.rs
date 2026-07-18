use std::{
    collections::VecDeque,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::{Child, Command},
    sync::mpsc,
    task::JoinHandle,
    time::{Instant, sleep, timeout},
};
use uuid::Uuid;

use crate::remote_access_config::NamedTunnelProfile;

const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const DEFAULT_RUNTIME_HEALTH_INTERVAL: Duration = Duration::from_secs(15);
const DEFAULT_RUNTIME_HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_AUTHENTICATION_SETTLE_WINDOW: Duration = Duration::from_millis(500);
const DEFAULT_MAX_NETWORK_RETRIES: u8 = 3;
const OUTPUT_RING_CAPACITY: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedTunnelStatus {
    Stopped,
    VerifyingLocal,
    Starting,
    VerifyingPublic,
    Retrying,
    Ready,
    Degraded,
    Failed,
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NamedTunnelFailureKind {
    InvalidConfiguration,
    TokenMissing,
    TokenRejected,
    LocalHealthUnavailable,
    DnsNotReady,
    PublicRouteRejected,
    WrongBridgeInstance,
    NetworkUnavailable,
    ChildExited,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeHealthIdentity {
    pub version: String,
    pub instance_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedTunnelSnapshot {
    pub status: NamedTunnelStatus,
    pub pid: Option<u32>,
    pub local_url: Option<String>,
    pub public_url: Option<String>,
    pub retry_attempt: u8,
    pub failure_kind: Option<NamedTunnelFailureKind>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeFailure {
    Dns,
    Timeout,
    Transport,
    HttpStatus(u16),
    InvalidHealthPayload,
}

impl ProbeFailure {
    fn is_deterministic(&self) -> bool {
        matches!(
            self,
            Self::Dns | Self::HttpStatus(400 | 401 | 403 | 404) | Self::InvalidHealthPayload
        )
    }

    fn public_message(&self) -> String {
        match self {
            Self::Dns => "public hostname DNS is not ready".to_string(),
            Self::Timeout => "public health request timed out".to_string(),
            Self::Transport => "public network is unavailable".to_string(),
            Self::HttpStatus(status) => format!("public health returned HTTP {status}"),
            Self::InvalidHealthPayload => "public health response is invalid".to_string(),
        }
    }
}

#[async_trait]
pub trait NamedTunnelHealthProbe: Send + Sync {
    async fn health(&self, base_url: &str) -> Result<BridgeHealthIdentity, ProbeFailure>;
}

trait NamedTunnelProcessLauncher: Send + Sync {
    fn spawn(&self, binary: &Path, args: &[String]) -> Result<Child, NamedTunnelError>;
}

struct CloudflaredProcessLauncher;

impl NamedTunnelProcessLauncher for CloudflaredProcessLauncher {
    fn spawn(&self, binary: &Path, args: &[String]) -> Result<Child, NamedTunnelError> {
        spawn_cloudflared(binary, args)
    }
}

#[derive(Debug, Error)]
pub enum NamedTunnelError {
    #[error("named tunnel is already running")]
    AlreadyRunning,
    #[error("Tunnel Token is missing")]
    TokenMissing,
    #[error("Cloudflare rejected the Tunnel Token")]
    TokenRejected,
    #[error("local Bridge health is unavailable")]
    LocalHealthUnavailable,
    #[error("public hostname DNS is not ready")]
    DnsNotReady,
    #[error("public route returned HTTP {status}")]
    PublicRouteRejected { status: u16 },
    #[error("public health response is invalid")]
    InvalidPublicHealth,
    #[error("public hostname points to a different Bridge instance")]
    WrongBridgeInstance,
    #[error("public network is temporarily unavailable")]
    NetworkUnavailable,
    #[error("cloudflared exited before the tunnel was ready")]
    ChildExited,
    #[error("named tunnel process I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

impl NamedTunnelError {
    pub fn failure_kind(&self) -> NamedTunnelFailureKind {
        match self {
            Self::AlreadyRunning | Self::Io(_) => NamedTunnelFailureKind::InvalidConfiguration,
            Self::TokenMissing => NamedTunnelFailureKind::TokenMissing,
            Self::TokenRejected => NamedTunnelFailureKind::TokenRejected,
            Self::LocalHealthUnavailable => NamedTunnelFailureKind::LocalHealthUnavailable,
            Self::DnsNotReady => NamedTunnelFailureKind::DnsNotReady,
            Self::PublicRouteRejected { .. } | Self::InvalidPublicHealth => {
                NamedTunnelFailureKind::PublicRouteRejected
            }
            Self::WrongBridgeInstance => NamedTunnelFailureKind::WrongBridgeInstance,
            Self::NetworkUnavailable => NamedTunnelFailureKind::NetworkUnavailable,
            Self::ChildExited => NamedTunnelFailureKind::ChildExited,
        }
    }

    fn from_public_probe(error: ProbeFailure) -> Self {
        match error {
            ProbeFailure::Dns => Self::DnsNotReady,
            ProbeFailure::Timeout | ProbeFailure::Transport => Self::NetworkUnavailable,
            ProbeFailure::HttpStatus(408 | 429 | 500..=599) => Self::NetworkUnavailable,
            ProbeFailure::HttpStatus(status) => Self::PublicRouteRejected { status },
            ProbeFailure::InvalidHealthPayload => Self::InvalidPublicHealth,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NamedTunnelConfig {
    pub binary: PathBuf,
    pub profile: NamedTunnelProfile,
    pub runtime_dir: PathBuf,
    pub startup_timeout: Duration,
    pub poll_interval: Duration,
    pub max_network_retries: u8,
    pub retry_delays: [Duration; 3],
    pub runtime_health_interval: Duration,
    pub runtime_health_timeout: Duration,
}

impl Default for NamedTunnelConfig {
    fn default() -> Self {
        Self {
            binary: std::env::var_os("CODEX_MOBILE_BRIDGE_TUNNEL_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("cloudflared")),
            profile: NamedTunnelProfile {
                hostname: "localhost".to_string(),
                local_port: 57324,
            },
            runtime_dir: std::env::temp_dir().join("codex-mobile-bridge"),
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            poll_interval: DEFAULT_POLL_INTERVAL,
            max_network_retries: DEFAULT_MAX_NETWORK_RETRIES,
            retry_delays: [
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
            ],
            runtime_health_interval: DEFAULT_RUNTIME_HEALTH_INTERVAL,
            runtime_health_timeout: DEFAULT_RUNTIME_HEALTH_TIMEOUT,
        }
    }
}

struct HttpNamedTunnelHealthProbe {
    client: Client,
}

impl HttpNamedTunnelHealthProbe {
    fn new() -> Self {
        Self {
            client: Client::builder()
                .build()
                .expect("named tunnel health client configuration is valid"),
        }
    }
}

#[async_trait]
impl NamedTunnelHealthProbe for HttpNamedTunnelHealthProbe {
    async fn health(&self, base_url: &str) -> Result<BridgeHealthIdentity, ProbeFailure> {
        let health_url = format!("{}/api/health", base_url.trim_end_matches('/'));
        let response = self
            .client
            .get(health_url)
            .header("Cache-Control", "no-cache")
            .send()
            .await
            .map_err(classify_transport_failure)?;
        if !response.status().is_success() {
            return Err(ProbeFailure::HttpStatus(response.status().as_u16()));
        }
        let identity = response
            .json::<BridgeHealthIdentity>()
            .await
            .map_err(|_| ProbeFailure::InvalidHealthPayload)?;
        if identity.version.trim().is_empty() || identity.instance_id.trim().is_empty() {
            return Err(ProbeFailure::InvalidHealthPayload);
        }
        Ok(identity)
    }
}

fn classify_transport_failure(error: reqwest::Error) -> ProbeFailure {
    if error.is_timeout() {
        ProbeFailure::Timeout
    } else if error.is_connect() && error.to_string().to_ascii_lowercase().contains("dns") {
        ProbeFailure::Dns
    } else {
        ProbeFailure::Transport
    }
}

#[derive(Debug, Clone)]
struct NamedTunnelState {
    status: NamedTunnelStatus,
    local_url: Option<String>,
    public_url: Option<String>,
    retry_attempt: u8,
    failure_kind: Option<NamedTunnelFailureKind>,
    detail: Option<String>,
}

impl NamedTunnelState {
    fn at(status: NamedTunnelStatus) -> Self {
        Self {
            status,
            local_url: None,
            public_url: None,
            retry_attempt: 0,
            failure_kind: None,
            detail: None,
        }
    }

    fn ready(local_url: String, public_url: String) -> Self {
        Self {
            status: NamedTunnelStatus::Ready,
            local_url: Some(local_url),
            public_url: Some(public_url),
            retry_attempt: 0,
            failure_kind: None,
            detail: None,
        }
    }
}

enum OutputEvent {
    TokenRejected,
}

struct TunnelOutputMonitor {
    events: mpsc::Receiver<OutputEvent>,
    readers: Vec<JoinHandle<()>>,
    lines: Arc<Mutex<VecDeque<String>>>,
}

impl TunnelOutputMonitor {
    fn token_rejected(&mut self) -> bool {
        let mut rejected = false;
        while let Ok(event) = self.events.try_recv() {
            if matches!(event, OutputEvent::TokenRejected) {
                rejected = true;
            }
        }
        rejected
    }

    async fn shutdown(&mut self) {
        for reader in &self.readers {
            reader.abort();
        }
        while let Some(reader) = self.readers.pop() {
            let _ = reader.await;
        }
    }

    #[allow(dead_code)]
    fn recent_lines(&self) -> Vec<String> {
        self.lines
            .lock()
            .map(|lines| lines.iter().cloned().collect())
            .unwrap_or_default()
    }
}

impl Drop for TunnelOutputMonitor {
    fn drop(&mut self) {
        for reader in &self.readers {
            reader.abort();
        }
    }
}

pub struct NamedTunnelManager {
    config: NamedTunnelConfig,
    probe: Arc<dyn NamedTunnelHealthProbe>,
    launcher: Arc<dyn NamedTunnelProcessLauncher>,
    state: NamedTunnelState,
    child: Option<Child>,
    output_monitor: Option<TunnelOutputMonitor>,
    local_identity: Option<BridgeHealthIdentity>,
    last_runtime_probe: Option<Instant>,
}

impl NamedTunnelManager {
    pub fn new(config: NamedTunnelConfig) -> Self {
        Self::with_health_probe(config, Arc::new(HttpNamedTunnelHealthProbe::new()))
    }

    pub fn with_health_probe(
        config: NamedTunnelConfig,
        probe: Arc<dyn NamedTunnelHealthProbe>,
    ) -> Self {
        Self {
            config,
            probe,
            launcher: Arc::new(CloudflaredProcessLauncher),
            state: NamedTunnelState::at(NamedTunnelStatus::Stopped),
            child: None,
            output_monitor: None,
            local_identity: None,
            last_runtime_probe: None,
        }
    }

    #[cfg(test)]
    fn with_dependencies(
        config: NamedTunnelConfig,
        probe: Arc<dyn NamedTunnelHealthProbe>,
        launcher: Arc<dyn NamedTunnelProcessLauncher>,
    ) -> Self {
        Self {
            config,
            probe,
            launcher,
            state: NamedTunnelState::at(NamedTunnelStatus::Stopped),
            child: None,
            output_monitor: None,
            local_identity: None,
            last_runtime_probe: None,
        }
    }

    pub fn config(&self) -> &NamedTunnelConfig {
        &self.config
    }

    pub fn status(&mut self) -> NamedTunnelSnapshot {
        self.reconcile_child_status();
        self.snapshot()
    }

    pub async fn start(&mut self, token: &str) -> Result<NamedTunnelSnapshot, NamedTunnelError> {
        self.ensure_not_running()?;
        if token.trim().is_empty() {
            let error = NamedTunnelError::TokenMissing;
            self.fail_without_child(&error);
            return Err(error);
        }

        let profile = self.config.profile.clone();
        let local_url = profile.local_url();
        let public_url = profile.public_url();
        self.state = NamedTunnelState {
            status: NamedTunnelStatus::VerifyingLocal,
            local_url: Some(local_url.clone()),
            public_url: Some(public_url.clone()),
            retry_attempt: 0,
            failure_kind: None,
            detail: None,
        };

        let local_identity = match self
            .probe_with_timeout(&local_url, self.config.startup_timeout)
            .await
        {
            Ok(identity) => identity,
            Err(_) => {
                let error = NamedTunnelError::LocalHealthUnavailable;
                self.fail_without_child(&error);
                return Err(error);
            }
        };

        let token_file = match TemporarySecretFile::create(&self.config.runtime_dir, token) {
            Ok(file) => file,
            Err(error) => {
                self.fail_without_child(&error);
                return Err(error);
            }
        };
        let args = named_tunnel_launch_args(token_file.path(), &local_url);
        self.state.status = NamedTunnelStatus::Starting;
        let mut child = match self.launcher.spawn(&self.config.binary, &args) {
            Ok(child) => child,
            Err(error) => {
                self.fail_without_child(&error);
                return Err(error);
            }
        };
        let mut output = tunnel_output_lines(&mut child);
        self.state.status = NamedTunnelStatus::VerifyingPublic;

        let max_network_retries = self
            .config
            .max_network_retries
            .min(DEFAULT_MAX_NETWORK_RETRIES);
        for attempt in 0..=max_network_retries {
            if let Err(error) = fail_if_child_exited_or_token_rejected(&mut child, &mut output) {
                return self.fail_start(child, output, error).await;
            }
            match monitored_probe_with_timeout(
                Arc::clone(&self.probe),
                &public_url,
                self.config.startup_timeout,
                self.config.poll_interval,
                &mut child,
                &mut output,
            )
            .await
            {
                Err(error) => return self.fail_start(child, output, error).await,
                Ok(Ok(public_identity)) if public_identity == local_identity => {
                    if let Err(error) = monitor_child_for_duration(
                        self.config
                            .startup_timeout
                            .min(DEFAULT_AUTHENTICATION_SETTLE_WINDOW),
                        self.config.poll_interval,
                        &mut child,
                        &mut output,
                    )
                    .await
                    {
                        return self.fail_start(child, output, error).await;
                    }
                    drop(token_file);
                    self.child = Some(child);
                    self.output_monitor = Some(output);
                    self.local_identity = Some(local_identity);
                    self.last_runtime_probe = Some(Instant::now());
                    self.state = NamedTunnelState::ready(local_url, public_url);
                    return Ok(self.snapshot());
                }
                Ok(Ok(_)) => {
                    return self
                        .fail_start(child, output, NamedTunnelError::WrongBridgeInstance)
                        .await;
                }
                Ok(Err(error)) if error.is_deterministic() => {
                    return self
                        .fail_start(child, output, NamedTunnelError::from_public_probe(error))
                        .await;
                }
                Ok(Err(error)) if attempt == max_network_retries => {
                    return self
                        .fail_start(child, output, NamedTunnelError::from_public_probe(error))
                        .await;
                }
                Ok(Err(error)) => {
                    self.state.status = NamedTunnelStatus::Retrying;
                    self.state.retry_attempt = attempt + 1;
                    self.state.detail = Some(error.public_message());
                    if let Err(error) = self
                        .wait_for_retry(&mut child, &mut output, self.retry_delay(attempt))
                        .await
                    {
                        return self.fail_start(child, output, error).await;
                    }
                    self.state.status = NamedTunnelStatus::VerifyingPublic;
                }
            }
        }
        unreachable!("retry loop always returns")
    }

    pub async fn stop(&mut self) -> Result<NamedTunnelSnapshot, NamedTunnelError> {
        self.state.status = NamedTunnelStatus::Stopping;
        self.stop_child_and_monitor().await;
        self.state = NamedTunnelState::at(NamedTunnelStatus::Stopped);
        self.local_identity = None;
        self.last_runtime_probe = None;
        Ok(self.snapshot())
    }

    pub fn terminate_now(&mut self) -> NamedTunnelSnapshot {
        self.state.status = NamedTunnelStatus::Stopping;
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
        self.output_monitor = None;
        self.state = NamedTunnelState::at(NamedTunnelStatus::Stopped);
        self.local_identity = None;
        self.last_runtime_probe = None;
        self.snapshot()
    }

    pub async fn refresh_runtime_health(
        &mut self,
        force: bool,
    ) -> Result<NamedTunnelSnapshot, NamedTunnelError> {
        if self.child_exited()? {
            self.mark_child_exited();
            return Ok(self.snapshot());
        }
        if self.child.is_none()
            || !matches!(
                self.state.status,
                NamedTunnelStatus::Ready | NamedTunnelStatus::Degraded
            )
        {
            return Ok(self.snapshot());
        }
        if !force
            && self
                .last_runtime_probe
                .is_some_and(|last| last.elapsed() < self.config.runtime_health_interval)
        {
            return Ok(self.snapshot());
        }
        if self
            .output_monitor
            .as_mut()
            .is_some_and(TunnelOutputMonitor::token_rejected)
        {
            self.fail_runtime(NamedTunnelError::TokenRejected).await;
            return Ok(self.snapshot());
        }

        let (public_url, local_identity) =
            match (self.state.public_url.clone(), self.local_identity.clone()) {
                (Some(public_url), Some(local_identity)) => (public_url, local_identity),
                _ => return Ok(self.snapshot()),
            };
        self.last_runtime_probe = Some(Instant::now());
        let probe_result = {
            let child = self.child.as_mut().expect("child checked above");
            let output = self
                .output_monitor
                .as_mut()
                .expect("running child has an output monitor");
            monitored_probe_with_timeout(
                Arc::clone(&self.probe),
                &public_url,
                self.config.runtime_health_timeout,
                self.config.poll_interval,
                child,
                output,
            )
            .await
        };
        match probe_result {
            Err(error) => self.fail_runtime(error).await,
            Ok(Ok(identity)) if identity == local_identity => {
                self.state.status = NamedTunnelStatus::Ready;
                self.state.failure_kind = None;
                self.state.detail = None;
            }
            Ok(Ok(_)) => {
                self.fail_runtime(NamedTunnelError::WrongBridgeInstance)
                    .await
            }
            Ok(Err(error)) if error.is_deterministic() => {
                self.fail_runtime(NamedTunnelError::from_public_probe(error))
                    .await;
            }
            Ok(Err(error)) => {
                self.state.status = NamedTunnelStatus::Degraded;
                self.state.failure_kind = None;
                self.state.detail = Some(error.public_message());
            }
        }
        Ok(self.snapshot())
    }

    async fn probe_with_timeout(
        &self,
        base_url: &str,
        duration: Duration,
    ) -> Result<BridgeHealthIdentity, ProbeFailure> {
        timeout(duration, self.probe.health(base_url))
            .await
            .unwrap_or(Err(ProbeFailure::Timeout))
    }

    async fn wait_for_retry(
        &self,
        child: &mut Child,
        output: &mut TunnelOutputMonitor,
        delay: Duration,
    ) -> Result<(), NamedTunnelError> {
        let deadline = Instant::now() + delay;
        loop {
            fail_if_child_exited_or_token_rejected(child, output)?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            sleep(remaining.min(self.config.poll_interval)).await;
        }
    }

    async fn fail_start(
        &mut self,
        child: Child,
        mut output: TunnelOutputMonitor,
        error: NamedTunnelError,
    ) -> Result<NamedTunnelSnapshot, NamedTunnelError> {
        self.fail_without_child(&error);
        stop_child(child).await;
        output.shutdown().await;
        Err(error)
    }

    async fn fail_runtime(&mut self, error: NamedTunnelError) {
        self.fail_without_child(&error);
        self.stop_child_and_monitor().await;
    }

    async fn stop_child_and_monitor(&mut self) {
        if let Some(child) = self.child.take() {
            stop_child(child).await;
        }
        if let Some(mut output) = self.output_monitor.take() {
            output.shutdown().await;
        }
    }

    fn retry_delay(&self, attempt: u8) -> Duration {
        self.config
            .retry_delays
            .get(attempt as usize)
            .copied()
            .unwrap_or_else(|| self.config.retry_delays[2])
    }

    fn ensure_not_running(&mut self) -> Result<(), NamedTunnelError> {
        if self.child_exited()? {
            self.mark_child_exited();
        }
        if self.child.is_some() {
            return Err(NamedTunnelError::AlreadyRunning);
        }
        Ok(())
    }

    fn child_exited(&mut self) -> Result<bool, NamedTunnelError> {
        match self.child.as_mut() {
            Some(child) => Ok(child.try_wait()?.is_some()),
            None => Ok(false),
        }
    }

    fn reconcile_child_status(&mut self) {
        if self.child_exited().unwrap_or(false) {
            self.mark_child_exited();
        }
    }

    fn mark_child_exited(&mut self) {
        self.child = None;
        self.output_monitor = None;
        self.fail_without_child(&NamedTunnelError::ChildExited);
    }

    fn fail_without_child(&mut self, error: &NamedTunnelError) {
        self.state.status = NamedTunnelStatus::Failed;
        self.state.retry_attempt = 0;
        self.state.failure_kind = Some(error.failure_kind());
        self.state.detail = Some(error.to_string());
        self.local_identity = None;
        self.last_runtime_probe = None;
    }

    fn snapshot(&self) -> NamedTunnelSnapshot {
        NamedTunnelSnapshot {
            status: self.state.status,
            pid: self.child.as_ref().and_then(Child::id),
            local_url: self.state.local_url.clone(),
            public_url: self.state.public_url.clone(),
            retry_attempt: self.state.retry_attempt,
            failure_kind: self.state.failure_kind,
            detail: self.state.detail.clone(),
        }
    }
}

fn spawn_cloudflared(binary: &Path, args: &[String]) -> Result<Child, NamedTunnelError> {
    Command::new(binary)
        .kill_on_drop(true)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(NamedTunnelError::Io)
}

fn tunnel_output_lines(child: &mut Child) -> TunnelOutputMonitor {
    let (sender, receiver) = mpsc::channel(64);
    let lines = Arc::new(Mutex::new(VecDeque::with_capacity(OUTPUT_RING_CAPACITY)));
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        readers.push(spawn_output_reader(
            stdout,
            sender.clone(),
            Arc::clone(&lines),
        ));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(spawn_output_reader(stderr, sender, Arc::clone(&lines)));
    }
    TunnelOutputMonitor {
        events: receiver,
        readers,
        lines,
    }
}

fn spawn_output_reader<R>(
    reader: R,
    sender: mpsc::Sender<OutputEvent>,
    lines: Arc<Mutex<VecDeque<String>>>,
) -> JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut input = BufReader::new(reader).lines();
        loop {
            match input.next_line().await {
                Ok(Some(line)) => {
                    let token_rejected = contains_token_rejection(&line);
                    push_sanitized_line(
                        &lines,
                        if token_rejected {
                            "cloudflared reported authentication failure"
                        } else {
                            "cloudflared emitted output"
                        },
                    );
                    if token_rejected {
                        let _ = sender.try_send(OutputEvent::TokenRejected);
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    push_sanitized_line(&lines, "cloudflared output read failed");
                    break;
                }
            }
        }
    })
}

fn contains_token_rejection(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("invalid tunnel secret")
        || lower.contains("token is invalid")
        || lower.contains("unauthorized")
}

fn push_sanitized_line(lines: &Arc<Mutex<VecDeque<String>>>, message: &str) {
    if let Ok(mut lines) = lines.lock() {
        if lines.len() == OUTPUT_RING_CAPACITY {
            lines.pop_front();
        }
        lines.push_back(message.to_string());
    }
}

fn fail_if_child_exited_or_token_rejected(
    child: &mut Child,
    output: &mut TunnelOutputMonitor,
) -> Result<(), NamedTunnelError> {
    if output.token_rejected() {
        return Err(NamedTunnelError::TokenRejected);
    }
    if child.try_wait()?.is_some() {
        return Err(NamedTunnelError::ChildExited);
    }
    Ok(())
}

async fn stop_child(mut child: Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

async fn monitor_child_for_duration(
    duration: Duration,
    poll_interval: Duration,
    child: &mut Child,
    output: &mut TunnelOutputMonitor,
) -> Result<(), NamedTunnelError> {
    let poll_interval = if poll_interval.is_zero() {
        Duration::from_millis(1)
    } else {
        poll_interval
    };
    let deadline = Instant::now() + duration;

    loop {
        fail_if_child_exited_or_token_rejected(child, output)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        sleep(remaining.min(poll_interval)).await;
    }

    fail_if_child_exited_or_token_rejected(child, output)
}

async fn monitored_probe_with_timeout(
    probe: Arc<dyn NamedTunnelHealthProbe>,
    base_url: &str,
    duration: Duration,
    poll_interval: Duration,
    child: &mut Child,
    output: &mut TunnelOutputMonitor,
) -> Result<Result<BridgeHealthIdentity, ProbeFailure>, NamedTunnelError> {
    let probe = timeout(duration, probe.health(base_url));
    tokio::pin!(probe);
    let poll_interval = if poll_interval.is_zero() {
        Duration::from_millis(1)
    } else {
        poll_interval
    };

    loop {
        tokio::select! {
            result = &mut probe => {
                tokio::task::yield_now().await;
                fail_if_child_exited_or_token_rejected(child, output)?;
                return Ok(result.unwrap_or(Err(ProbeFailure::Timeout)));
            }
            _ = sleep(poll_interval) => {
                tokio::task::yield_now().await;
                fail_if_child_exited_or_token_rejected(child, output)?;
            }
        }
    }
}

pub fn named_tunnel_launch_args(token_file: &Path, local_url: &str) -> Vec<String> {
    vec![
        "tunnel".to_string(),
        "--no-autoupdate".to_string(),
        "run".to_string(),
        "--token-file".to_string(),
        token_file.display().to_string(),
        "--url".to_string(),
        local_url.to_string(),
    ]
}

struct TemporarySecretFile {
    path: PathBuf,
}

impl TemporarySecretFile {
    fn create(runtime_dir: &Path, secret: &str) -> Result<Self, NamedTunnelError> {
        std::fs::create_dir_all(runtime_dir)?;
        let path = runtime_dir.join(format!("cloudflared-token-{}", Uuid::new_v4()));
        Self::create_at_path(path, secret)
    }

    fn create_at_path(path: PathBuf, secret: &str) -> Result<Self, NamedTunnelError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        let write_result = (|| -> Result<(), std::io::Error> {
            file.write_all(secret.as_bytes())?;
            file.sync_all()
        })();
        if let Err(error) = write_result {
            let _ = std::fs::remove_file(&path);
            return Err(error.into());
        }
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporarySecretFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        fs,
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use std::process::Stdio;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        process::{Child, Command},
        sync::Mutex,
    };

    use super::{
        BridgeHealthIdentity, HttpNamedTunnelHealthProbe, NamedTunnelConfig, NamedTunnelError,
        NamedTunnelFailureKind, NamedTunnelHealthProbe, NamedTunnelManager,
        NamedTunnelProcessLauncher, NamedTunnelStatus, OUTPUT_RING_CAPACITY, ProbeFailure,
        TemporarySecretFile, named_tunnel_launch_args,
    };

    struct ScriptedProbe {
        results: Mutex<VecDeque<Result<BridgeHealthIdentity, ProbeFailure>>>,
        public_attempts: AtomicUsize,
        public_delay: Duration,
    }

    impl ScriptedProbe {
        fn new(
            results: impl IntoIterator<Item = Result<BridgeHealthIdentity, ProbeFailure>>,
        ) -> Self {
            Self {
                results: Mutex::new(results.into_iter().collect()),
                public_attempts: AtomicUsize::new(0),
                public_delay: Duration::ZERO,
            }
        }

        fn with_public_delay(
            results: impl IntoIterator<Item = Result<BridgeHealthIdentity, ProbeFailure>>,
            delay: Duration,
        ) -> Self {
            Self {
                results: Mutex::new(results.into_iter().collect()),
                public_attempts: AtomicUsize::new(0),
                public_delay: delay,
            }
        }

        fn public_attempts(&self) -> usize {
            self.public_attempts.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl NamedTunnelHealthProbe for ScriptedProbe {
        async fn health(&self, base_url: &str) -> Result<BridgeHealthIdentity, ProbeFailure> {
            if base_url.starts_with("https://") {
                self.public_attempts.fetch_add(1, Ordering::SeqCst);
                if !self.public_delay.is_zero() {
                    tokio::time::sleep(self.public_delay).await;
                }
            }
            self.results
                .lock()
                .await
                .pop_front()
                .unwrap_or_else(|| panic!("health probe received more calls than scripted"))
        }
    }

    fn identity(version: &str, instance_id: &str) -> BridgeHealthIdentity {
        BridgeHealthIdentity {
            version: version.to_string(),
            instance_id: instance_id.to_string(),
        }
    }

    #[cfg(unix)]
    fn shell_quote(path: &Path) -> String {
        format!(
            "'{}'",
            path.display().to_string().replace('\'', "'\\\"'\\\"'")
        )
    }

    #[cfg(unix)]
    fn fake_cloudflared(dir: &Path, args_file: &Path, behavior: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let binary = dir.join("fake-cloudflared");
        fs::write(
            &binary,
            format!(
                "#!/bin/sh\n: > {args_file}\nfor argument in \"$@\"; do\n  printf '%s\\n' \"$argument\" >> {args_file}\ndone\n{behavior}\n",
                args_file = shell_quote(args_file),
            ),
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        binary
    }

    #[cfg(unix)]
    fn production_config_with_behavior(behavior: &str) -> (NamedTunnelConfig, PathBuf) {
        let dir = tempfile::tempdir().unwrap().keep();
        let args_file = dir.join("received-args");
        let config = NamedTunnelConfig {
            binary: fake_cloudflared(&dir, &args_file, behavior),
            profile: crate::NamedTunnelProfile::new("codex.example.com", 57324).unwrap(),
            runtime_dir: dir.join("runtime"),
            startup_timeout: Duration::from_millis(50),
            poll_interval: Duration::from_millis(5),
            max_network_retries: 3,
            retry_delays: [
                Duration::from_millis(5),
                Duration::from_millis(10),
                Duration::from_millis(15),
            ],
            runtime_health_interval: Duration::from_millis(20),
            runtime_health_timeout: Duration::from_millis(30),
        };
        (config, args_file)
    }

    fn test_config() -> NamedTunnelConfig {
        let dir = tempfile::tempdir().unwrap().keep();
        NamedTunnelConfig {
            binary: PathBuf::from("test-cloudflared"),
            profile: crate::NamedTunnelProfile::new("codex.example.com", 57324).unwrap(),
            runtime_dir: dir.join("runtime"),
            startup_timeout: Duration::from_millis(50),
            poll_interval: Duration::from_millis(5),
            max_network_retries: 3,
            retry_delays: [
                Duration::from_millis(5),
                Duration::from_millis(10),
                Duration::from_millis(15),
            ],
            runtime_health_interval: Duration::from_millis(20),
            runtime_health_timeout: Duration::from_millis(30),
        }
    }

    struct ShellProcessLauncher {
        behavior: String,
    }

    impl ShellProcessLauncher {
        fn new(behavior: impl Into<String>) -> Self {
            Self {
                behavior: behavior.into(),
            }
        }
    }

    impl NamedTunnelProcessLauncher for ShellProcessLauncher {
        fn spawn(&self, _binary: &Path, _args: &[String]) -> Result<Child, NamedTunnelError> {
            Command::new("/bin/sh")
                .kill_on_drop(true)
                .arg("-c")
                .arg(&self.behavior)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(NamedTunnelError::Io)
        }
    }

    fn test_manager_with_behavior(
        probe: Arc<dyn NamedTunnelHealthProbe>,
        behavior: impl Into<String>,
    ) -> NamedTunnelManager {
        NamedTunnelManager::with_dependencies(
            test_config(),
            probe,
            Arc::new(ShellProcessLauncher::new(behavior)),
        )
    }

    fn test_manager(probe: Arc<dyn NamedTunnelHealthProbe>) -> NamedTunnelManager {
        test_manager_with_behavior(probe, "exec sleep 30")
    }

    async fn running_manager(probe: Arc<dyn NamedTunnelHealthProbe>) -> NamedTunnelManager {
        let mut manager = test_manager(probe);
        manager.start("token-value").await.unwrap();
        manager
    }

    #[test]
    fn launch_args_use_token_file_without_exposing_token() {
        let args =
            named_tunnel_launch_args(Path::new("/tmp/cloudflare-token"), "http://127.0.0.1:57324");

        assert_eq!(
            args,
            vec![
                "tunnel",
                "--no-autoupdate",
                "run",
                "--token-file",
                "/tmp/cloudflare-token",
                "--url",
                "http://127.0.0.1:57324"
            ]
        );
        assert!(!args.join(" ").contains("secret-token-value"));
    }

    #[tokio::test]
    async fn production_start_always_passes_a_token_file_without_exposing_the_token() {
        let probe = Arc::new(ScriptedProbe::new(vec![
            Ok(identity("0.1.5", "instance-1")),
            Ok(identity("0.1.5", "instance-1")),
        ]));
        let (mut config, args_file) = production_config_with_behavior("exec sleep 30");
        config.startup_timeout = Duration::from_secs(1);
        let mut manager = NamedTunnelManager::with_health_probe(config, probe);

        manager.start("token-value").await.unwrap();
        assert!(wait_for_file(&args_file).await);
        let args = fs::read_to_string(&args_file).unwrap();

        assert!(args.lines().any(|arg| arg == "--token-file"));
        assert!(!args.lines().any(|arg| arg == "--token"));
        assert!(!args.contains("token-value"));
        let _ = manager.stop().await;
    }

    #[cfg(unix)]
    #[test]
    fn temporary_token_file_is_mode_0600_and_removed_on_drop() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path;
        {
            let file = TemporarySecretFile::create(dir.path(), "secret-token-value").unwrap();
            path = file.path().to_path_buf();
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(!path.exists());
    }

    #[test]
    fn temporary_secret_file_collision_never_removes_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing-token-file");
        fs::write(&path, "existing-secret").unwrap();

        assert!(TemporarySecretFile::create_at_path(path.clone(), "new-secret").is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "existing-secret");
    }

    #[tokio::test]
    async fn dns_failure_is_deterministic_and_is_not_retried() {
        let probe = Arc::new(ScriptedProbe::new(vec![
            Ok(identity("0.1.5", "instance-1")),
            Err(ProbeFailure::Dns),
        ]));
        let mut manager = test_manager(probe.clone());
        let runtime_dir = manager.config().runtime_dir.clone();

        let result = manager.start("token-value").await;
        if result.is_ok() {
            tokio::time::sleep(Duration::from_millis(20)).await;
            panic!(
                "token rejection was not observed: {:?}",
                manager.output_monitor.as_ref().unwrap().recent_lines()
            );
        }
        let error = result.unwrap_err();

        assert_eq!(error.failure_kind(), NamedTunnelFailureKind::DnsNotReady);
        assert_eq!(probe.public_attempts(), 1);
        assert_eq!(manager.status().status, NamedTunnelStatus::Failed);
        assert!(std::fs::read_dir(runtime_dir).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn transient_public_failure_retries_three_times_then_stops() {
        let probe = Arc::new(ScriptedProbe::new(vec![
            Ok(identity("0.1.5", "instance-1")),
            Err(ProbeFailure::Timeout),
            Err(ProbeFailure::HttpStatus(503)),
            Err(ProbeFailure::Transport),
            Err(ProbeFailure::Timeout),
        ]));
        let mut manager = test_manager(probe.clone());

        let result = manager.start("token-value").await;
        if result.is_ok() {
            tokio::time::sleep(Duration::from_millis(20)).await;
            panic!(
                "token rejection was not observed: {:?}",
                manager.output_monitor.as_ref().unwrap().recent_lines()
            );
        }
        let error = result.unwrap_err();

        assert_eq!(
            error.failure_kind(),
            NamedTunnelFailureKind::NetworkUnavailable
        );
        assert_eq!(probe.public_attempts(), 4);
        assert_eq!(manager.status().status, NamedTunnelStatus::Failed);
    }

    #[tokio::test]
    async fn configured_retries_are_clamped_to_three_public_retries() {
        let probe = Arc::new(ScriptedProbe::new(vec![
            Ok(identity("0.1.5", "instance-1")),
            Err(ProbeFailure::Timeout),
            Err(ProbeFailure::Timeout),
            Err(ProbeFailure::Timeout),
            Err(ProbeFailure::Timeout),
        ]));
        let mut config = test_config();
        config.max_network_retries = 9;
        let mut manager = NamedTunnelManager::with_dependencies(
            config,
            probe.clone(),
            Arc::new(ShellProcessLauncher::new("exec sleep 30")),
        );

        let error = manager.start("token-value").await.unwrap_err();

        assert_eq!(
            error.failure_kind(),
            NamedTunnelFailureKind::NetworkUnavailable
        );
        assert_eq!(probe.public_attempts(), 4);
    }

    #[tokio::test]
    async fn public_health_must_match_local_version_and_instance() {
        let probe = Arc::new(ScriptedProbe::new(vec![
            Ok(identity("0.1.5", "instance-local")),
            Ok(identity("0.1.5", "instance-other")),
        ]));
        let mut manager = test_manager(probe);

        let error = manager.start("token-value").await.unwrap_err();

        assert_eq!(
            error.failure_kind(),
            NamedTunnelFailureKind::WrongBridgeInstance
        );
    }

    #[tokio::test]
    async fn token_rejection_in_provider_output_overrides_a_successful_public_probe() {
        let probe = Arc::new(ScriptedProbe::with_public_delay(
            vec![
                Ok(identity("0.1.5", "instance-1")),
                Ok(identity("0.1.5", "instance-1")),
            ],
            Duration::from_millis(5),
        ));
        let mut manager = test_manager_with_behavior(probe, "echo Unauthorized >&2\nexec sleep 30");

        let error = manager.start("token-value").await.unwrap_err();

        assert_eq!(error.failure_kind(), NamedTunnelFailureKind::TokenRejected);
        assert_eq!(manager.status().status, NamedTunnelStatus::Failed);
        assert!(
            !manager
                .status()
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("token-value")
        );
    }

    #[tokio::test]
    async fn startup_detects_early_token_rejection_while_public_probe_is_slow() {
        let probe = Arc::new(ScriptedProbe::with_public_delay(
            vec![
                Ok(identity("0.1.5", "instance-1")),
                Ok(identity("0.1.5", "instance-1")),
            ],
            Duration::from_millis(500),
        ));
        let mut config = test_config();
        config.startup_timeout = Duration::from_millis(500);
        let mut manager = NamedTunnelManager::with_dependencies(
            config,
            probe,
            Arc::new(ShellProcessLauncher::new(
                "sleep 0.02\necho Unauthorized >&2\nexec sleep 30",
            )),
        );
        let started = std::time::Instant::now();

        let error = manager.start("token-value").await.unwrap_err();

        assert_eq!(error.failure_kind(), NamedTunnelFailureKind::TokenRejected);
        assert!(started.elapsed() < Duration::from_millis(200));
    }

    #[tokio::test]
    async fn blank_token_does_not_create_a_file_or_spawn_a_child() {
        let probe = Arc::new(ScriptedProbe::new(Vec::new()));
        let mut manager = test_manager(probe);
        let runtime_dir = manager.config().runtime_dir.clone();

        let error = manager.start("  \n\t ").await.unwrap_err();

        assert_eq!(error.failure_kind(), NamedTunnelFailureKind::TokenMissing);
        assert!(!runtime_dir.exists() || std::fs::read_dir(runtime_dir).unwrap().next().is_none());
        assert_eq!(manager.status().pid, None);
    }

    #[tokio::test]
    async fn stop_terminates_the_shell_test_child() {
        let probe = Arc::new(ScriptedProbe::new(vec![
            Ok(identity("0.1.5", "instance-1")),
            Ok(identity("0.1.5", "instance-1")),
        ]));
        let mut manager = running_manager(probe).await;
        let pid = manager.status().pid.unwrap();

        let stopped = manager.stop().await.unwrap();

        assert_eq!(stopped.status, NamedTunnelStatus::Stopped);
        assert!(wait_for_process_exit(pid).await);
    }

    #[tokio::test]
    async fn terminate_now_stops_the_child_and_sets_stopped() {
        let probe = Arc::new(ScriptedProbe::new(vec![
            Ok(identity("0.1.5", "instance-1")),
            Ok(identity("0.1.5", "instance-1")),
        ]));
        let mut manager = running_manager(probe).await;
        let pid = manager.status().pid.unwrap();

        let snapshot = manager.terminate_now();

        assert_eq!(snapshot.status, NamedTunnelStatus::Stopped);
        assert!(wait_for_process_exit(pid).await);
    }

    #[tokio::test]
    async fn dropping_the_manager_terminates_the_child() {
        let probe = Arc::new(ScriptedProbe::new(vec![
            Ok(identity("0.1.5", "instance-1")),
            Ok(identity("0.1.5", "instance-1")),
        ]));
        let pid = {
            let mut manager = running_manager(probe).await;
            manager.status().pid.unwrap()
        };

        assert!(wait_for_process_exit(pid).await);
    }

    #[tokio::test]
    async fn continuous_output_is_drained_while_the_ring_remains_bounded() {
        let marker_dir = tempfile::tempdir().unwrap().keep();
        let marker = marker_dir.join("output-drained");
        let behavior = format!(
            "i=0\nwhile [ \"$i\" -lt 5000 ]; do\n  printf 'stdout %s\\n' \"$i\"\n  printf 'stderr %s\\n' \"$i\" >&2\n  i=$((i + 1))\ndone\n: > {}\nexec sleep 30",
            shell_quote(&marker)
        );
        let probe = Arc::new(ScriptedProbe::new(vec![
            Ok(identity("0.1.5", "instance-1")),
            Ok(identity("0.1.5", "instance-1")),
        ]));
        let mut manager = test_manager_with_behavior(probe, behavior);

        assert_eq!(
            manager.start("token-value").await.unwrap().status,
            NamedTunnelStatus::Ready
        );
        assert!(wait_for_file(&marker).await);
        let lines = manager.output_monitor.as_ref().unwrap().recent_lines();

        assert!(lines.len() <= OUTPUT_RING_CAPACITY);
        let _ = manager.stop().await;
    }

    #[tokio::test]
    async fn runtime_network_loss_degrades_then_recovers_without_respawning_cloudflared() {
        let probe = Arc::new(ScriptedProbe::new(vec![
            Ok(identity("0.1.5", "instance-1")),
            Ok(identity("0.1.5", "instance-1")),
            Err(ProbeFailure::Timeout),
            Ok(identity("0.1.5", "instance-1")),
        ]));
        let mut manager = running_manager(probe).await;
        let original_pid = manager.status().pid;

        let degraded = manager.refresh_runtime_health(true).await.unwrap();
        assert_eq!(degraded.status, NamedTunnelStatus::Degraded);
        assert_eq!(degraded.pid, original_pid);

        let recovered = manager.refresh_runtime_health(true).await.unwrap();
        assert_eq!(recovered.status, NamedTunnelStatus::Ready);
        assert_eq!(recovered.pid, original_pid);

        let _ = manager.stop().await;
    }

    #[tokio::test]
    async fn runtime_health_respects_interval_unless_forced() {
        let probe = Arc::new(ScriptedProbe::new(vec![
            Ok(identity("0.1.5", "instance-1")),
            Ok(identity("0.1.5", "instance-1")),
            Ok(identity("0.1.5", "instance-1")),
        ]));
        let mut manager = running_manager(probe.clone()).await;

        manager.refresh_runtime_health(false).await.unwrap();
        assert_eq!(probe.public_attempts(), 1);
        manager.refresh_runtime_health(true).await.unwrap();
        assert_eq!(probe.public_attempts(), 2);
        let _ = manager.stop().await;
    }

    #[tokio::test]
    async fn runtime_child_exit_marks_failed_without_respawning() {
        let probe = Arc::new(ScriptedProbe::new(vec![
            Ok(identity("0.1.5", "instance-1")),
            Ok(identity("0.1.5", "instance-1")),
        ]));
        let mut manager = running_manager(probe).await;
        let pid = manager.status().pid.unwrap();
        std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let snapshot = manager.refresh_runtime_health(true).await.unwrap();

        assert_eq!(snapshot.status, NamedTunnelStatus::Failed);
        assert_eq!(
            snapshot.failure_kind,
            Some(NamedTunnelFailureKind::ChildExited)
        );
        assert_eq!(snapshot.pid, None);
    }

    #[tokio::test]
    async fn runtime_route_rejection_stops_the_existing_child() {
        let probe = Arc::new(ScriptedProbe::new(vec![
            Ok(identity("0.1.5", "instance-1")),
            Ok(identity("0.1.5", "instance-1")),
            Err(ProbeFailure::HttpStatus(403)),
        ]));
        let mut manager = running_manager(probe).await;
        let original_pid = manager.status().pid.unwrap();

        let snapshot = manager.refresh_runtime_health(true).await.unwrap();

        assert_eq!(snapshot.status, NamedTunnelStatus::Failed);
        assert_eq!(snapshot.pid, None);
        assert!(wait_for_process_exit(original_pid).await);
    }

    struct SlowRuntimeProbe {
        public_attempts: AtomicUsize,
    }

    #[async_trait]
    impl NamedTunnelHealthProbe for SlowRuntimeProbe {
        async fn health(&self, base_url: &str) -> Result<BridgeHealthIdentity, ProbeFailure> {
            if base_url.starts_with("https://")
                && self.public_attempts.fetch_add(1, Ordering::SeqCst) > 0
            {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Ok(identity("0.1.5", "instance-1"))
        }
    }

    #[tokio::test]
    async fn runtime_probe_token_rejection_never_recovers_to_ready() {
        let probe = Arc::new(SlowRuntimeProbe {
            public_attempts: AtomicUsize::new(0),
        });
        let mut config = test_config();
        config.runtime_health_timeout = Duration::from_millis(500);
        let mut manager = NamedTunnelManager::with_dependencies(
            config,
            probe,
            Arc::new(ShellProcessLauncher::new(
                "sleep 0.08\necho Unauthorized >&2\nexec sleep 30",
            )),
        );
        manager.start("token-value").await.unwrap();
        let started = std::time::Instant::now();

        let snapshot = manager.refresh_runtime_health(true).await.unwrap();

        assert_eq!(snapshot.status, NamedTunnelStatus::Failed);
        assert_eq!(
            snapshot.failure_kind,
            Some(NamedTunnelFailureKind::TokenRejected)
        );
        assert!(started.elapsed() < Duration::from_millis(200));
    }

    #[tokio::test]
    async fn default_http_probe_disables_cache() {
        let (base_url, server) =
            serve_health_response(r#"{"version":"0.1.5","instanceId":"instance-1"}"#).await;

        let health_identity = HttpNamedTunnelHealthProbe::new()
            .health(&base_url)
            .await
            .unwrap();
        let request = server.await.unwrap();

        assert_eq!(health_identity, identity("0.1.5", "instance-1"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("cache-control: no-cache")
        );
    }

    #[tokio::test]
    async fn default_http_probe_rejects_empty_identity_fields() {
        for body in [
            r#"{"version":"","instanceId":"instance-1"}"#,
            r#"{"version":"0.1.5","instanceId":""}"#,
        ] {
            let (base_url, server) = serve_health_response(body).await;

            let error = HttpNamedTunnelHealthProbe::new()
                .health(&base_url)
                .await
                .unwrap_err();
            let _ = server.await.unwrap();

            assert_eq!(error, ProbeFailure::InvalidHealthPayload);
        }
    }

    async fn serve_health_response(body: &str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let body = body.to_owned();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let read = socket.read(&mut request).await.unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&request[..read]).into_owned()
        });
        (format!("http://{address}"), server)
    }

    async fn wait_for_file(path: &Path) -> bool {
        for _ in 0..500 {
            if path.exists() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        false
    }

    async fn wait_for_process_exit(pid: u32) -> bool {
        for _ in 0..50 {
            if !process_is_running(pid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    fn process_is_running(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
}
