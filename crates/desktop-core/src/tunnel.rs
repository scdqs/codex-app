use std::{
    env,
    path::PathBuf,
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

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
const LOCAL_URL_PLACEHOLDER: &str = "{local_url}";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelStatus {
    Stopped,
    Starting,
    Ready,
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
                &[
                    PathBuf::from("/opt/homebrew/bin/cloudflared"),
                    PathBuf::from("/usr/local/bin/cloudflared"),
                ],
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
}

#[derive(Debug, Clone)]
struct TunnelState {
    status: TunnelStatus,
    session: Option<TunnelSession>,
    detail: Option<String>,
}

pub struct QuickTunnelManager {
    config: QuickTunnelConfig,
    state: TunnelState,
    child: Option<Child>,
}

impl QuickTunnelManager {
    pub fn new(config: QuickTunnelConfig) -> Self {
        Self {
            config,
            state: TunnelState {
                status: TunnelStatus::Stopped,
                session: None,
                detail: None,
            },
            child: None,
        }
    }

    pub fn config(&self) -> &QuickTunnelConfig {
        &self.config
    }

    pub fn status(&mut self) -> TunnelSnapshot {
        self.reconcile_child_status();
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

        let local_url = local_url.into();
        let args = self.launch_args(&local_url);
        self.state = TunnelState {
            status: TunnelStatus::Starting,
            session: None,
            detail: None,
        };

        let mut command = Command::new(&self.config.binary);
        command
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

        match self.wait_for_public_url(&mut child, &mut output).await {
            Ok(public_url) => {
                drain_tunnel_output(output);
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
                self.child = Some(child);
                Ok(self.snapshot())
            }
            Err(error) => {
                self.state = TunnelState {
                    status: TunnelStatus::Failed,
                    session: None,
                    detail: Some(error.to_string()),
                };
                let _ = child.kill().await;
                Err(error)
            }
        }
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
        Ok(self.snapshot())
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
    use tempfile::tempdir;

    fn shell_config(script: &str) -> QuickTunnelConfig {
        QuickTunnelConfig {
            binary: PathBuf::from("/bin/sh"),
            args_template: vec!["-c".to_string(), script.to_string()],
            startup_timeout: Duration::from_millis(300),
            poll_interval: Duration::from_millis(10),
        }
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
        let mut manager = QuickTunnelManager::new(shell_config(
            "echo 'INF https://first.trycloudflare.com' >&2; sleep 5",
        ));

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
        let mut manager = QuickTunnelManager::new(shell_config(
            "echo 'INF https://done.trycloudflare.com' >&2; sleep 0.05",
        ));

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
        let mut manager = QuickTunnelManager::new(shell_config(
            "echo 'INF https://steady.trycloudflare.com' >&2; i=0; while [ $i -lt 30 ]; do echo \"INF still running $i\" >&2; i=$((i+1)); sleep 0.01; done; sleep 5",
        ));

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
        let mut manager = QuickTunnelManager::new(shell_config(
            "echo 'INF https://rotated.trycloudflare.com' >&2; sleep 5",
        ));

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
}
