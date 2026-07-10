use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use thiserror::Error;
use tokio::time::sleep;

const DEFAULT_DEBUG_PORT: u16 = 9229;
const DEFAULT_LAUNCH_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_CDP_POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone)]
pub struct CodexLaunchConfig {
    pub app_name: String,
    pub app_path_candidates: Vec<PathBuf>,
    pub process_names: Vec<String>,
    pub debug_port: u16,
    pub launch_timeout: Duration,
    pub cdp_poll_interval: Duration,
}

impl Default for CodexLaunchConfig {
    fn default() -> Self {
        Self {
            app_name: "ChatGPT / Codex".to_string(),
            app_path_candidates: default_codex_app_candidates(),
            process_names: vec!["ChatGPT".to_string(), "Codex".to_string()],
            debug_port: DEFAULT_DEBUG_PORT,
            launch_timeout: DEFAULT_LAUNCH_TIMEOUT,
            cdp_poll_interval: DEFAULT_CDP_POLL_INTERVAL,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexLaunchStatus {
    InstallInstructions,
    Attached,
    Launched,
    NeedsUserRestartConfirmation,
    ManualInstructions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexLaunchOutcome {
    pub status: CodexLaunchStatus,
    pub debug_port: u16,
    pub app_path: Option<PathBuf>,
    pub launch_command: Option<CodexLaunchCommand>,
    pub detail: Option<String>,
    pub instructions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexLaunchCommand {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Error)]
pub enum CodexLaunchHostError {
    #[error("{0}")]
    Message(String),
    #[error("{action} failed: {source}")]
    Io {
        action: &'static str,
        source: std::io::Error,
    },
    #[error("command failed: {program} exited with {status}")]
    CommandFailed { program: String, status: String },
}

#[async_trait]
pub trait CodexDesktopHost: Send + Sync {
    async fn find_app(
        &self,
        config: &CodexLaunchConfig,
    ) -> Result<Option<PathBuf>, CodexLaunchHostError>;

    async fn is_running(&self, config: &CodexLaunchConfig) -> Result<bool, CodexLaunchHostError>;

    async fn cdp_ready(&self, debug_port: u16) -> Result<bool, CodexLaunchHostError>;

    async fn launch_with_debug_port(
        &self,
        command: &CodexLaunchCommand,
    ) -> Result<(), CodexLaunchHostError>;
}

pub struct CodexLaunchManager {
    config: CodexLaunchConfig,
    host: Arc<dyn CodexDesktopHost>,
}

impl CodexLaunchManager {
    pub fn new(config: CodexLaunchConfig, host: Arc<dyn CodexDesktopHost>) -> Self {
        Self { config, host }
    }

    pub fn with_host(host: impl CodexDesktopHost + 'static) -> Self {
        Self::new(CodexLaunchConfig::default(), Arc::new(host))
    }

    pub fn mac_default(config: CodexLaunchConfig) -> Self {
        Self::new(config, Arc::new(MacCodexDesktopHost))
    }

    pub fn config(&self) -> &CodexLaunchConfig {
        &self.config
    }

    pub async fn ensure_ready(&self) -> CodexLaunchOutcome {
        let app_path = match self.host.find_app(&self.config).await {
            Ok(Some(path)) => path,
            Ok(None) => return self.install_instructions(),
            Err(error) => {
                return self.manual_instructions(None, None, error.to_string());
            }
        };

        let is_running = match self.host.is_running(&self.config).await {
            Ok(is_running) => is_running,
            Err(error) => {
                return self.manual_instructions(Some(app_path), None, error.to_string());
            }
        };

        if is_running {
            return self.attach_or_request_restart(app_path).await;
        }

        let command = self.launch_command(&app_path);
        if let Err(error) = self.host.launch_with_debug_port(&command).await {
            return self.manual_instructions(Some(app_path), Some(command), error.to_string());
        }

        if self.wait_for_cdp_ready().await {
            CodexLaunchOutcome {
                status: CodexLaunchStatus::Launched,
                debug_port: self.config.debug_port,
                app_path: Some(app_path),
                launch_command: Some(command),
                detail: None,
                instructions: Vec::new(),
            }
        } else {
            self.manual_instructions(
                Some(app_path),
                Some(command),
                format!(
                    "{} did not expose CDP on port {} within {:?}",
                    self.config.app_name, self.config.debug_port, self.config.launch_timeout
                ),
            )
        }
    }

    async fn attach_or_request_restart(&self, app_path: PathBuf) -> CodexLaunchOutcome {
        match self.host.cdp_ready(self.config.debug_port).await {
            Ok(true) => CodexLaunchOutcome {
                status: CodexLaunchStatus::Attached,
                debug_port: self.config.debug_port,
                app_path: Some(app_path),
                launch_command: None,
                detail: None,
                instructions: Vec::new(),
            },
            Ok(false) => self.needs_restart_confirmation(app_path, None),
            Err(error) => self.needs_restart_confirmation(app_path, Some(error.to_string())),
        }
    }

    async fn wait_for_cdp_ready(&self) -> bool {
        let deadline = Instant::now() + self.config.launch_timeout;
        loop {
            if matches!(self.host.cdp_ready(self.config.debug_port).await, Ok(true)) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            sleep(self.config.cdp_poll_interval).await;
        }
    }

    fn launch_command(&self, app_path: &Path) -> CodexLaunchCommand {
        CodexLaunchCommand {
            program: "open".to_string(),
            args: vec![
                "-n".to_string(),
                app_path.display().to_string(),
                "--args".to_string(),
                format!("--remote-debugging-port={}", self.config.debug_port),
            ],
        }
    }

    fn install_instructions(&self) -> CodexLaunchOutcome {
        CodexLaunchOutcome {
            status: CodexLaunchStatus::InstallInstructions,
            debug_port: self.config.debug_port,
            app_path: None,
            launch_command: None,
            detail: Some(format!("{} is not installed", self.config.app_name)),
            instructions: vec![
                format!("Install {} for macOS.", self.config.app_name),
                "Open it once, sign in, then return to Codex Mobile Bridge.".to_string(),
            ],
        }
    }

    fn needs_restart_confirmation(
        &self,
        app_path: PathBuf,
        detail: Option<String>,
    ) -> CodexLaunchOutcome {
        CodexLaunchOutcome {
            status: CodexLaunchStatus::NeedsUserRestartConfirmation,
            debug_port: self.config.debug_port,
            app_path: Some(app_path),
            launch_command: None,
            detail: detail.or_else(|| {
                Some(format!(
                    "{} is already running without CDP on port {}",
                    self.config.app_name, self.config.debug_port
                ))
            }),
            instructions: vec![
                format!(
                    "Quit {} after current tasks are safe to stop.",
                    self.config.app_name
                ),
                "Then let Codex Mobile Bridge launch it with the debug port enabled.".to_string(),
            ],
        }
    }

    fn manual_instructions(
        &self,
        app_path: Option<PathBuf>,
        launch_command: Option<CodexLaunchCommand>,
        detail: String,
    ) -> CodexLaunchOutcome {
        CodexLaunchOutcome {
            status: CodexLaunchStatus::ManualInstructions,
            debug_port: self.config.debug_port,
            app_path,
            launch_command,
            detail: Some(detail),
            instructions: vec![
                format!("Quit {} if it is currently running.", self.config.app_name),
                format!(
                    "Launch {} with --remote-debugging-port={}.",
                    self.config.app_name, self.config.debug_port
                ),
                "Return to Codex Mobile Bridge and retry attach.".to_string(),
            ],
        }
    }
}

#[derive(Debug, Default)]
pub struct MacCodexDesktopHost;

#[async_trait]
impl CodexDesktopHost for MacCodexDesktopHost {
    async fn find_app(
        &self,
        config: &CodexLaunchConfig,
    ) -> Result<Option<PathBuf>, CodexLaunchHostError> {
        Ok(config
            .app_path_candidates
            .iter()
            .find(|path| path.is_dir())
            .cloned())
    }

    async fn is_running(&self, config: &CodexLaunchConfig) -> Result<bool, CodexLaunchHostError> {
        for process_name in &config.process_names {
            let status = Command::new("/usr/bin/pgrep")
                .args(["-x", process_name])
                .status()
                .map_err(|source| CodexLaunchHostError::Io {
                    action: "detect ChatGPT/Codex process",
                    source,
                })?;
            if status.success() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn cdp_ready(&self, debug_port: u16) -> Result<bool, CodexLaunchHostError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(800))
            .build()
            .map_err(|source| CodexLaunchHostError::Message(source.to_string()))?;
        let url = format!("http://127.0.0.1:{debug_port}/json/list");
        Ok(client
            .get(url)
            .send()
            .await
            .map(|response| response.status().is_success())
            .unwrap_or(false))
    }

    async fn launch_with_debug_port(
        &self,
        command: &CodexLaunchCommand,
    ) -> Result<(), CodexLaunchHostError> {
        let status = Command::new(&command.program)
            .args(&command.args)
            .status()
            .map_err(|source| CodexLaunchHostError::Io {
                action: "launch ChatGPT/Codex",
                source,
            })?;
        if status.success() {
            Ok(())
        } else {
            Err(CodexLaunchHostError::CommandFailed {
                program: command.program.clone(),
                status: status.to_string(),
            })
        }
    }
}

fn default_codex_app_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from("/Applications/ChatGPT.app")];
    if let Some(home) = std::env::var_os("HOME") {
        let user_applications = PathBuf::from(home).join("Applications");
        candidates.push(user_applications.join("ChatGPT.app"));
    }
    candidates.push(PathBuf::from("/Applications/Codex.app"));
    if let Some(home) = std::env::var_os("HOME") {
        let user_applications = PathBuf::from(home).join("Applications");
        candidates.push(user_applications.join("Codex.app"));
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    fn test_config() -> CodexLaunchConfig {
        CodexLaunchConfig {
            app_name: "Codex".to_string(),
            app_path_candidates: vec![PathBuf::from("/Applications/Codex.app")],
            process_names: vec!["Codex".to_string()],
            debug_port: 9333,
            launch_timeout: Duration::from_millis(40),
            cdp_poll_interval: Duration::from_millis(5),
        }
    }

    fn manager(host: MockCodexDesktopHost) -> CodexLaunchManager {
        CodexLaunchManager::new(test_config(), Arc::new(host))
    }

    #[test]
    fn default_config_accepts_chatgpt_and_legacy_codex_names() {
        let config = CodexLaunchConfig::default();

        assert_eq!(config.app_name, "ChatGPT / Codex");
        assert_eq!(
            config.app_path_candidates.first(),
            Some(&PathBuf::from("/Applications/ChatGPT.app"))
        );
        assert!(
            config
                .app_path_candidates
                .contains(&PathBuf::from("/Applications/Codex.app"))
        );
        assert_eq!(config.process_names, ["ChatGPT", "Codex"]);
    }

    #[tokio::test]
    async fn not_installed_returns_install_instructions() {
        let host = MockCodexDesktopHost::default();
        let state = host.state();
        let outcome = manager(host).ensure_ready().await;

        assert_eq!(outcome.status, CodexLaunchStatus::InstallInstructions);
        assert!(outcome.app_path.is_none());
        assert!(outcome.instructions[0].contains("Install Codex"));
        assert!(state.lock().expect("state lock").launches.is_empty());
    }

    #[tokio::test]
    async fn installed_not_running_launches_with_debug_port() {
        let host = MockCodexDesktopHost::default()
            .with_app("/Applications/Codex.app")
            .with_running(false)
            .with_cdp_ready_sequence([false, true]);
        let state = host.state();

        let outcome = manager(host).ensure_ready().await;

        assert_eq!(outcome.status, CodexLaunchStatus::Launched);
        assert_eq!(outcome.debug_port, 9333);
        let launches = &state.lock().expect("state lock").launches;
        assert_eq!(launches.len(), 1);
        assert_eq!(launches[0].program, "open");
        assert_eq!(
            launches[0].args,
            vec![
                "-n",
                "/Applications/Codex.app",
                "--args",
                "--remote-debugging-port=9333"
            ]
        );
    }

    #[tokio::test]
    async fn running_with_cdp_ready_attaches_without_launching() {
        let host = MockCodexDesktopHost::default()
            .with_app("/Applications/Codex.app")
            .with_running(true)
            .with_cdp_ready_sequence([true]);
        let state = host.state();

        let outcome = manager(host).ensure_ready().await;

        assert_eq!(outcome.status, CodexLaunchStatus::Attached);
        assert!(outcome.launch_command.is_none());
        assert!(state.lock().expect("state lock").launches.is_empty());
    }

    #[tokio::test]
    async fn running_without_cdp_requires_explicit_restart_confirmation() {
        let host = MockCodexDesktopHost::default()
            .with_app("/Applications/Codex.app")
            .with_running(true)
            .with_cdp_ready_sequence([false]);
        let state = host.state();

        let outcome = manager(host).ensure_ready().await;

        assert_eq!(
            outcome.status,
            CodexLaunchStatus::NeedsUserRestartConfirmation
        );
        assert!(outcome.detail.expect("detail").contains("already running"));
        assert!(state.lock().expect("state lock").launches.is_empty());
    }

    #[tokio::test]
    async fn launch_failure_returns_manual_instructions() {
        let host = MockCodexDesktopHost::default()
            .with_app("/Applications/Codex.app")
            .with_running(false)
            .with_launch_error("open failed");
        let state = host.state();

        let outcome = manager(host).ensure_ready().await;

        assert_eq!(outcome.status, CodexLaunchStatus::ManualInstructions);
        assert!(outcome.detail.expect("detail").contains("open failed"));
        assert!(outcome.launch_command.is_some());
        assert_eq!(state.lock().expect("state lock").launches.len(), 1);
    }

    #[derive(Clone, Default)]
    struct MockCodexDesktopHost {
        state: Arc<Mutex<MockState>>,
    }

    #[derive(Default)]
    struct MockState {
        app: Option<PathBuf>,
        running: bool,
        cdp_ready: VecDeque<Result<bool, String>>,
        launch_error: Option<String>,
        launches: Vec<CodexLaunchCommand>,
    }

    impl MockCodexDesktopHost {
        fn state(&self) -> Arc<Mutex<MockState>> {
            Arc::clone(&self.state)
        }

        fn with_app(self, path: impl Into<PathBuf>) -> Self {
            self.state.lock().expect("state lock").app = Some(path.into());
            self
        }

        fn with_running(self, running: bool) -> Self {
            self.state.lock().expect("state lock").running = running;
            self
        }

        fn with_cdp_ready_sequence<const N: usize>(self, values: [bool; N]) -> Self {
            self.state.lock().expect("state lock").cdp_ready = values.into_iter().map(Ok).collect();
            self
        }

        fn with_launch_error(self, message: impl Into<String>) -> Self {
            self.state.lock().expect("state lock").launch_error = Some(message.into());
            self
        }
    }

    #[async_trait]
    impl CodexDesktopHost for MockCodexDesktopHost {
        async fn find_app(
            &self,
            _config: &CodexLaunchConfig,
        ) -> Result<Option<PathBuf>, CodexLaunchHostError> {
            Ok(self.state.lock().expect("state lock").app.clone())
        }

        async fn is_running(
            &self,
            _config: &CodexLaunchConfig,
        ) -> Result<bool, CodexLaunchHostError> {
            Ok(self.state.lock().expect("state lock").running)
        }

        async fn cdp_ready(&self, _debug_port: u16) -> Result<bool, CodexLaunchHostError> {
            self.state
                .lock()
                .expect("state lock")
                .cdp_ready
                .pop_front()
                .unwrap_or(Ok(false))
                .map_err(CodexLaunchHostError::Message)
        }

        async fn launch_with_debug_port(
            &self,
            command: &CodexLaunchCommand,
        ) -> Result<(), CodexLaunchHostError> {
            let mut state = self.state.lock().expect("state lock");
            state.launches.push(command.clone());
            match state.launch_error.as_ref() {
                Some(message) => Err(CodexLaunchHostError::Message(message.clone())),
                None => Ok(()),
            }
        }
    }
}
