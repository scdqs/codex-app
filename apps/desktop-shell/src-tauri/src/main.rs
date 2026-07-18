use std::{
    env,
    io::Write,
    net::{IpAddr, Ipv4Addr, UdpSocket},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[cfg(test)]
use desktop_core::MemorySecretStore;
use desktop_core::{
    BridgeProcessConfig, BridgeProcessManager, BridgeProcessSnapshot, BridgeProcessStatus,
    CLOUDFLARE_TUNNEL_TOKEN_KEY, CodexLaunchCommand, CodexLaunchConfig, CodexLaunchManager,
    CodexLaunchOutcome, DiagnosticCheck, DiagnosticLog, DiagnosticsBundleInput, KeyringSecretStore,
    NamedTunnelConfig, NamedTunnelFailureKind, NamedTunnelManager, NamedTunnelProfile,
    NamedTunnelSnapshot, NamedTunnelStatus, PortPolicy, QuickTunnelConfig, QuickTunnelManager,
    RemoteAccessConfigStore, RemoteAccessPreferences, SecretStore, TemporarySecretFile,
    TunnelSnapshot, TunnelStatus, VapidKeyManager, build_diagnostics_bundle, redact_sensitive_text,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager, State};
use tokio::sync::{Mutex, Notify};

const DEFAULT_DEBUG_PORT: u16 = 9229;
const DEFAULT_BRIDGE_PORT: u16 = 57324;
const CONTROL_TOKEN_HEADER: &str = "x-bridge-control-token";
const SECRET_STORE_SERVICE: &str = "com.codex.mobile.bridge";
const NAMED_TUNNEL_SUPERVISOR_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteAccessMode {
    None,
    Quick,
    Named,
    NamedFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteAccessAction {
    StartNamed,
    StartTemporary,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionResult {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RemoteAccessTransition {
    stop_quick: bool,
    stop_named: bool,
    start_quick: bool,
    start_named: bool,
    resulting_mode: RemoteAccessMode,
}

fn transition_remote_access(
    current: RemoteAccessMode,
    action: RemoteAccessAction,
    result: ActionResult,
) -> RemoteAccessTransition {
    let stop_quick = action == RemoteAccessAction::StartNamed && current == RemoteAccessMode::Quick;
    let stop_named = matches!(
        current,
        RemoteAccessMode::Named | RemoteAccessMode::NamedFailed
    ) && matches!(
        action,
        RemoteAccessAction::StartNamed
            | RemoteAccessAction::StartTemporary
            | RemoteAccessAction::Stop
    );
    let (start_quick, start_named, resulting_mode) = match (action, result) {
        (RemoteAccessAction::StartNamed, ActionResult::Succeeded) => {
            (false, true, RemoteAccessMode::Named)
        }
        (RemoteAccessAction::StartNamed, ActionResult::Failed) => {
            (false, true, RemoteAccessMode::NamedFailed)
        }
        (RemoteAccessAction::StartTemporary, ActionResult::Succeeded) => {
            (true, false, RemoteAccessMode::Quick)
        }
        (RemoteAccessAction::StartTemporary, ActionResult::Failed) => {
            (true, false, RemoteAccessMode::NamedFailed)
        }
        (RemoteAccessAction::Stop, _) => (false, false, RemoteAccessMode::None),
    };
    RemoteAccessTransition {
        stop_quick,
        stop_named,
        start_quick,
        start_named,
        resulting_mode,
    }
}

#[derive(Debug, Clone)]
struct NamedFailureSnapshot {
    local_url: Option<String>,
    public_url: Option<String>,
    failure_kind: String,
    detail: String,
}

struct ShellState {
    bridge: Mutex<Option<BridgeProcessManager>>,
    quick_tunnel: Mutex<QuickTunnelManager>,
    named_tunnel: Mutex<Option<NamedTunnelManager>>,
    remote_preferences: Mutex<RemoteAccessConfigStore>,
    secret_store: Arc<dyn SecretStore>,
    vapid_keys: VapidKeyManager,
    pending_vapid_secret: Mutex<Option<TemporarySecretFile>>,
    active_remote_mode: Mutex<RemoteAccessMode>,
    remote_access_operation: Mutex<()>,
    last_named_failure: Mutex<Option<NamedFailureSnapshot>>,
    last_pairing_link: Mutex<Option<String>>,
    last_pairing_source: Mutex<Option<PairingLinkSource>>,
    exit_cleanup_started: AtomicBool,
    supervisor_shutdown: Notify,
}

impl ShellState {
    fn new(
        remote_preferences: RemoteAccessConfigStore,
        secret_store: Arc<dyn SecretStore>,
    ) -> Self {
        let vapid_keys = VapidKeyManager::new(Arc::clone(&secret_store));
        Self {
            bridge: Mutex::new(None),
            quick_tunnel: Mutex::new(QuickTunnelManager::new(QuickTunnelConfig::default())),
            named_tunnel: Mutex::new(None),
            remote_preferences: Mutex::new(remote_preferences),
            secret_store,
            vapid_keys,
            pending_vapid_secret: Mutex::new(None),
            active_remote_mode: Mutex::new(RemoteAccessMode::None),
            remote_access_operation: Mutex::new(()),
            last_named_failure: Mutex::new(None),
            last_pairing_link: Mutex::new(None),
            last_pairing_source: Mutex::new(None),
            exit_cleanup_started: AtomicBool::new(false),
            supervisor_shutdown: Notify::new(),
        }
    }
}

#[cfg(test)]
impl Default for ShellState {
    fn default() -> Self {
        Self::new(
            RemoteAccessConfigStore::new(
                env::temp_dir().join("codex-mobile-bridge-remote-access.json"),
            ),
            Arc::new(MemorySecretStore::default()),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PairingLinkSource {
    Local,
    QuickTunnel,
    NamedTunnel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NamedPairingEvent {
    StartAttempt,
    StartFailed,
    PairingFailed,
    RuntimeFailed,
    RuntimeDegraded,
}

fn should_clear_named_pairing(event: NamedPairingEvent) -> bool {
    !matches!(event, NamedPairingEvent::RuntimeDegraded)
}

fn named_runtime_mode(current: RemoteAccessMode, status: NamedTunnelStatus) -> RemoteAccessMode {
    match (current, status) {
        (RemoteAccessMode::Named, NamedTunnelStatus::Failed) => RemoteAccessMode::NamedFailed,
        (RemoteAccessMode::Named, NamedTunnelStatus::Ready | NamedTunnelStatus::Degraded) => {
            RemoteAccessMode::Named
        }
        (RemoteAccessMode::NamedFailed, _) => RemoteAccessMode::NamedFailed,
        _ => current,
    }
}

fn should_supervisor_refresh_named(status: NamedTunnelStatus) -> bool {
    matches!(
        status,
        NamedTunnelStatus::Ready | NamedTunnelStatus::Degraded
    )
}

fn legacy_quick_start_allowed(mode: RemoteAccessMode) -> bool {
    !matches!(
        mode,
        RemoteAccessMode::Named | RemoteAccessMode::NamedFailed
    )
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ShellStatusDto {
    app_version: String,
    bridge: BridgeProcessSnapshotDto,
    tunnel: TunnelSnapshotDto,
    remote_access: RemoteAccessStatusDto,
    last_pairing_link: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BridgeProcessSnapshotDto {
    status: String,
    pid: Option<u32>,
    port: Option<u16>,
    health_url: Option<String>,
    detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TunnelSnapshotDto {
    status: String,
    public_url: Option<String>,
    local_url: Option<String>,
    detail: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteAccessStatusDto {
    mode: String,
    named_profile: Option<NamedTunnelProfile>,
    named: NamedTunnelSnapshotDto,
    quick: TunnelSnapshotDto,
    fixed_origin_ready: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NamedTunnelSnapshotDto {
    status: String,
    pid: Option<u32>,
    local_url: Option<String>,
    public_url: Option<String>,
    retry_attempt: u8,
    failure_kind: Option<String>,
    detail: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteAccessPreferencesDto {
    named_profile: Option<NamedTunnelProfile>,
    token_stored: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteAccessDiagnosticsDto {
    mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    named_failure_kind: Option<String>,
    retry_count: u8,
    public_health_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cloudflared_exit_category: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BridgePortMode {
    Flexible,
    Fixed(u16),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexLaunchOutcomeDto {
    status: String,
    debug_port: u16,
    app_path: Option<String>,
    launch_command: Option<CodexLaunchCommandDto>,
    detail: Option<String>,
    instructions: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexLaunchCommandDto {
    program: String,
    args: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceDto {
    device_id: String,
    display_name: String,
    paired_origin: Option<String>,
    created_at: u64,
    last_seen_at: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ControlDiagnosticsDto {
    status: String,
    connection_state: String,
    detail: Option<String>,
    #[serde(default)]
    push_subscriptions: Vec<PushSubscriptionDiagnosticDto>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PushSubscriptionDiagnosticDto {
    subscription_state: String,
    endpoint_host: String,
    last_success_at: Option<u64>,
    last_error_category: Option<String>,
}

#[tauri::command]
async fn get_app_status(state: State<'_, ShellState>) -> Result<ShellStatusDto, String> {
    let bridge = {
        let bridge = state.bridge.lock().await;
        bridge
            .as_ref()
            .map(BridgeProcessManager::status)
            .unwrap_or_else(stopped_bridge_snapshot)
    };
    let named_snapshot = refresh_named_runtime(&state, false).await?;
    let quick_snapshot = {
        let mut quick_tunnel = state.quick_tunnel.lock().await;
        quick_tunnel.refresh_status().await
    };
    clear_stale_quick_pairing_link(&state, &quick_snapshot).await;
    let preferences = load_remote_access_preferences(&state).await?;
    let mode = *state.active_remote_mode.lock().await;
    let failure = state.last_named_failure.lock().await.clone();
    let named = if mode == RemoteAccessMode::NamedFailed {
        failure
            .map(NamedTunnelSnapshotDto::failed)
            .or_else(|| named_snapshot.map(Into::into))
            .unwrap_or_else(NamedTunnelSnapshotDto::stopped)
    } else {
        named_snapshot
            .map(Into::into)
            .unwrap_or_else(NamedTunnelSnapshotDto::stopped)
    };
    let quick = TunnelSnapshotDto::from(quick_snapshot);
    let fixed_origin_ready = mode == RemoteAccessMode::Named && named.status == "ready";
    let last_pairing_link = state.last_pairing_link.lock().await.clone();

    Ok(ShellStatusDto {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        bridge: BridgeProcessSnapshotDto::from(bridge),
        tunnel: quick.clone(),
        remote_access: RemoteAccessStatusDto {
            mode: remote_access_mode(mode).to_string(),
            named_profile: preferences.named_tunnel,
            named,
            quick,
            fixed_origin_ready,
        },
        last_pairing_link,
    })
}

#[tauri::command]
async fn get_remote_access_preferences(
    state: State<'_, ShellState>,
) -> Result<RemoteAccessPreferencesDto, String> {
    remote_access_preferences_dto(&state).await
}

#[tauri::command]
async fn save_named_tunnel_profile(
    hostname: String,
    local_port: u16,
    token: Option<String>,
    state: State<'_, ShellState>,
) -> Result<RemoteAccessPreferencesDto, String> {
    let _operation = state.remote_access_operation.lock().await;
    let profile = NamedTunnelProfile::new(&hostname, local_port)
        .map_err(|error| redact_sensitive_text(&error.to_string()))?;
    if let Some(token) = token.filter(|value| !value.trim().is_empty()) {
        state
            .secret_store
            .set(CLOUDFLARE_TUNNEL_TOKEN_KEY, token.trim())
            .map_err(|error| redact_sensitive_text(&error.to_string()))?;
    } else if state
        .secret_store
        .get(CLOUDFLARE_TUNNEL_TOKEN_KEY)
        .map_err(|error| redact_sensitive_text(&error.to_string()))?
        .is_none()
    {
        return Err("Tunnel Token is required for the first setup".to_string());
    }

    let current_mode = *state.active_remote_mode.lock().await;
    if matches!(
        current_mode,
        RemoteAccessMode::Named | RemoteAccessMode::NamedFailed
    ) {
        stop_named_process(&state).await?;
        clear_pairing_link_for_source(&state, PairingLinkSource::NamedTunnel).await;
        *state.active_remote_mode.lock().await = RemoteAccessMode::None;
        *state.last_named_failure.lock().await = None;
        let _ = sync_active_bridge_remote_access_context(&state).await;
    }

    state
        .remote_preferences
        .lock()
        .await
        .save(&RemoteAccessPreferences {
            named_tunnel: Some(profile),
        })
        .map_err(|error| redact_sensitive_text(&error.to_string()))?;
    remote_access_preferences_dto(&state).await
}

#[tauri::command]
async fn delete_named_tunnel_profile(state: State<'_, ShellState>) -> Result<(), String> {
    let _operation = state.remote_access_operation.lock().await;
    stop_named_process(&state).await?;
    clear_pairing_link_for_source(&state, PairingLinkSource::NamedTunnel).await;
    state
        .remote_preferences
        .lock()
        .await
        .delete()
        .map_err(|error| redact_sensitive_text(&error.to_string()))?;
    state
        .secret_store
        .delete(CLOUDFLARE_TUNNEL_TOKEN_KEY)
        .map_err(|error| redact_sensitive_text(&error.to_string()))?;
    let current_mode = *state.active_remote_mode.lock().await;
    if matches!(
        current_mode,
        RemoteAccessMode::Named | RemoteAccessMode::NamedFailed
    ) {
        *state.active_remote_mode.lock().await = RemoteAccessMode::None;
    }
    *state.last_named_failure.lock().await = None;
    let _ = sync_active_bridge_remote_access_context(&state).await;
    Ok(())
}

#[tauri::command]
async fn ensure_codex_ready() -> CodexLaunchOutcomeDto {
    let config = CodexLaunchConfig {
        debug_port: debug_port(),
        ..CodexLaunchConfig::default()
    };
    CodexLaunchManager::mac_default(config)
        .ensure_ready()
        .await
        .into()
}

#[tauri::command]
async fn start_bridge(
    app: AppHandle,
    state: State<'_, ShellState>,
) -> Result<BridgeProcessSnapshotDto, String> {
    let _operation = state.remote_access_operation.lock().await;
    let mode = *state.active_remote_mode.lock().await;
    let port_mode = if matches!(
        mode,
        RemoteAccessMode::Named | RemoteAccessMode::NamedFailed
    ) {
        let preferences = load_remote_access_preferences(&state).await?;
        let profile = preferences
            .named_tunnel
            .ok_or_else(|| "Named Tunnel is not configured".to_string())?;
        BridgePortMode::Fixed(profile.local_port)
    } else {
        BridgePortMode::Flexible
    };
    let snapshot = ensure_bridge_for_mode(&app, &state, port_mode).await?;
    let _ = sync_active_bridge_remote_access_context(&state).await;
    Ok(snapshot.into())
}

#[tauri::command]
async fn stop_bridge(state: State<'_, ShellState>) -> Result<BridgeProcessSnapshotDto, String> {
    let _operation = state.remote_access_operation.lock().await;
    stop_remote_access_inner(&state).await?;
    let mut bridge = state.bridge.lock().await;
    let manager = bridge
        .as_mut()
        .ok_or_else(|| "bridge service is not initialized".to_string())?;
    let snapshot = manager
        .stop()
        .await
        .map_err(|error| redact_sensitive_text(&error.to_string()))?;
    set_pairing_link(&state, None, None).await;
    Ok(snapshot.into())
}

#[tauri::command]
async fn create_pairing_link(state: State<'_, ShellState>) -> Result<String, String> {
    let _operation = state.remote_access_operation.lock().await;
    let mode = *state.active_remote_mode.lock().await;
    let (public_url, source) = match mode {
        RemoteAccessMode::None => (None, PairingLinkSource::Local),
        RemoteAccessMode::Quick => {
            let snapshot = state.quick_tunnel.lock().await.status();
            let public_url = snapshot
                .session
                .map(|session| session.public_url)
                .ok_or_else(|| "Quick Tunnel public URL is not available".to_string())?;
            (Some(public_url), PairingLinkSource::QuickTunnel)
        }
        RemoteAccessMode::Named => {
            let snapshot = {
                let mut named_tunnel = state.named_tunnel.lock().await;
                named_tunnel
                    .as_mut()
                    .map(NamedTunnelManager::status)
                    .ok_or_else(|| "Named Tunnel is not initialized".to_string())?
            };
            if !matches!(
                snapshot.status,
                NamedTunnelStatus::Ready | NamedTunnelStatus::Degraded
            ) {
                return Err("Named Tunnel is not ready for pairing".to_string());
            }
            let public_url = snapshot
                .public_url
                .ok_or_else(|| "Named Tunnel public URL is not available".to_string())?;
            (Some(public_url), PairingLinkSource::NamedTunnel)
        }
        RemoteAccessMode::NamedFailed => {
            return Err(
                "Named Tunnel has failed; retry it or start a temporary tunnel".to_string(),
            );
        }
    };
    let link = {
        let bridge = state.bridge.lock().await;
        let manager = bridge
            .as_ref()
            .ok_or_else(|| "bridge service is not initialized".to_string())?;
        match public_url {
            Some(public_url) => {
                manager
                    .create_pairing_link_for_bridge_url(&public_url)
                    .await
            }
            None => manager.create_pairing_link().await,
        }
        .map_err(|error| redact_sensitive_text(&error.to_string()))?
    };
    set_pairing_link(&state, Some(link.clone()), Some(source)).await;
    Ok(link)
}

#[tauri::command]
async fn start_quick_tunnel(
    app: AppHandle,
    state: State<'_, ShellState>,
) -> Result<TunnelSnapshotDto, String> {
    let _operation = state.remote_access_operation.lock().await;
    let current = *state.active_remote_mode.lock().await;
    start_quick_access(&app, &state, current).await
}

#[tauri::command]
async fn rotate_quick_tunnel(state: State<'_, ShellState>) -> Result<TunnelSnapshotDto, String> {
    let _operation = state.remote_access_operation.lock().await;
    if *state.active_remote_mode.lock().await != RemoteAccessMode::Quick {
        return Err("Quick Tunnel is not the active remote access mode".to_string());
    }
    let snapshot = {
        let mut quick_tunnel = state.quick_tunnel.lock().await;
        quick_tunnel
            .rotate()
            .await
            .map_err(|error| redact_sensitive_text(&error.to_string()))?
    };
    let public_url = match snapshot.session.as_ref() {
        Some(session) => session.public_url.as_str(),
        None => {
            let _ = state.quick_tunnel.lock().await.stop().await;
            *state.active_remote_mode.lock().await = RemoteAccessMode::None;
            clear_pairing_link_for_source(&state, PairingLinkSource::QuickTunnel).await;
            let _ = sync_active_bridge_remote_access_context(&state).await;
            return Err("Quick Tunnel public URL is not available".to_string());
        }
    };
    let pairing_link = match pairing_link_for_public_url(&state, public_url).await {
        Ok(link) => link,
        Err(error) => {
            let _ = state.quick_tunnel.lock().await.stop().await;
            *state.active_remote_mode.lock().await = RemoteAccessMode::None;
            clear_pairing_link_for_source(&state, PairingLinkSource::QuickTunnel).await;
            let _ = sync_active_bridge_remote_access_context(&state).await;
            return Err(error);
        }
    };
    set_pairing_link(
        &state,
        Some(pairing_link),
        Some(PairingLinkSource::QuickTunnel),
    )
    .await;
    let _ = sync_bridge_remote_access_context(&state, "quick", Some(public_url)).await;
    Ok(snapshot.into())
}

#[tauri::command]
async fn stop_quick_tunnel(state: State<'_, ShellState>) -> Result<TunnelSnapshotDto, String> {
    let _operation = state.remote_access_operation.lock().await;
    let snapshot = {
        let mut quick_tunnel = state.quick_tunnel.lock().await;
        quick_tunnel
            .stop()
            .await
            .map_err(|error| redact_sensitive_text(&error.to_string()))?
    };
    if *state.active_remote_mode.lock().await == RemoteAccessMode::Quick {
        *state.active_remote_mode.lock().await = RemoteAccessMode::None;
    }
    clear_pairing_link_for_source(&state, PairingLinkSource::QuickTunnel).await;
    let _ = sync_active_bridge_remote_access_context(&state).await;
    Ok(snapshot.into())
}

#[tauri::command]
async fn start_named_tunnel(
    app: AppHandle,
    state: State<'_, ShellState>,
) -> Result<NamedTunnelSnapshotDto, String> {
    let _operation = state.remote_access_operation.lock().await;
    start_named_tunnel_inner(&app, &state).await
}

#[tauri::command]
async fn retry_named_tunnel(
    app: AppHandle,
    state: State<'_, ShellState>,
) -> Result<NamedTunnelSnapshotDto, String> {
    let _operation = state.remote_access_operation.lock().await;
    start_named_tunnel_inner(&app, &state).await
}

#[tauri::command]
async fn recheck_named_tunnel_health(
    state: State<'_, ShellState>,
) -> Result<NamedTunnelSnapshotDto, String> {
    let _operation = state.remote_access_operation.lock().await;
    let snapshot = refresh_named_runtime(&state, true)
        .await?
        .ok_or_else(|| "Named Tunnel is not initialized".to_string())?;
    if snapshot.status == NamedTunnelStatus::Stopped {
        return Err("Named Tunnel is not running".to_string());
    }
    Ok(snapshot.into())
}

#[tauri::command]
async fn stop_named_tunnel(state: State<'_, ShellState>) -> Result<NamedTunnelSnapshotDto, String> {
    let _operation = state.remote_access_operation.lock().await;
    let snapshot = stop_named_process(&state).await?;
    let current = *state.active_remote_mode.lock().await;
    if matches!(
        current,
        RemoteAccessMode::Named | RemoteAccessMode::NamedFailed
    ) {
        *state.active_remote_mode.lock().await = RemoteAccessMode::None;
    }
    *state.last_named_failure.lock().await = None;
    clear_pairing_link_for_source(&state, PairingLinkSource::NamedTunnel).await;
    let _ = sync_active_bridge_remote_access_context(&state).await;
    Ok(snapshot.into())
}

#[tauri::command]
async fn start_temporary_tunnel(
    app: AppHandle,
    state: State<'_, ShellState>,
) -> Result<TunnelSnapshotDto, String> {
    let _operation = state.remote_access_operation.lock().await;
    let current = *state.active_remote_mode.lock().await;
    let profile = load_remote_access_preferences(&state)
        .await
        .ok()
        .and_then(|preferences| preferences.named_tunnel);
    if let Err(error) = stop_named_process(&state).await {
        let error = fail_temporary_start(&state, current, profile.as_ref(), &error).await;
        return Err(error);
    }
    clear_pairing_link_for_source(&state, PairingLinkSource::NamedTunnel).await;
    if let Err(error) = stop_quick_if_running(&state).await {
        let error = fail_temporary_start(&state, current, profile.as_ref(), &error).await;
        return Err(error);
    }
    let bridge_snapshot = match ensure_bridge_for_mode(&app, &state, BridgePortMode::Flexible).await
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let error = fail_temporary_start(&state, current, profile.as_ref(), &error).await;
            return Err(error);
        }
    };
    let local_url = match bridge_snapshot.port {
        Some(port) => format!("http://127.0.0.1:{port}"),
        None => {
            let error = fail_temporary_start(
                &state,
                current,
                profile.as_ref(),
                "bridge port is not available",
            )
            .await;
            return Err(error);
        }
    };
    let start_result = {
        let mut quick_tunnel = state.quick_tunnel.lock().await;
        quick_tunnel.start(local_url).await
    };
    let snapshot = match start_result {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let error =
                fail_temporary_start(&state, current, profile.as_ref(), &error.to_string()).await;
            return Err(error);
        }
    };
    let public_url = match snapshot.session.as_ref() {
        Some(session) => session.public_url.as_str(),
        None => {
            let _ = state.quick_tunnel.lock().await.stop().await;
            let error = fail_temporary_start(
                &state,
                current,
                profile.as_ref(),
                "Quick Tunnel public URL is not available",
            )
            .await;
            return Err(error);
        }
    };
    let pairing_link = match pairing_link_for_public_url(&state, public_url).await {
        Ok(link) => link,
        Err(error) => {
            let _ = state.quick_tunnel.lock().await.stop().await;
            let error = fail_temporary_start(&state, current, profile.as_ref(), &error).await;
            return Err(error);
        }
    };
    activate_quick_remote_access(&state, current, pairing_link).await;
    let _ = sync_bridge_remote_access_context(&state, "quick", Some(public_url)).await;
    Ok(snapshot.into())
}

#[tauri::command]
async fn stop_remote_access(state: State<'_, ShellState>) -> Result<(), String> {
    let _operation = state.remote_access_operation.lock().await;
    stop_remote_access_inner(&state).await
}

async fn start_named_tunnel_inner(
    app: &AppHandle,
    state: &ShellState,
) -> Result<NamedTunnelSnapshotDto, String> {
    let current = *state.active_remote_mode.lock().await;
    let preflight = transition_remote_access(
        current,
        RemoteAccessAction::StartNamed,
        ActionResult::Failed,
    );
    handle_named_pairing_event(state, NamedPairingEvent::StartAttempt).await;
    let quick_stop = stop_quick_if_running(state).await;
    require_quick_stopped_for_named_start(state, current, quick_stop).await?;
    if preflight.stop_named
        && let Err(error) = stop_named_process(state).await
    {
        let error =
            fail_named_start(state, current, None, "named_tunnel_stop_failed", &error).await;
        return Err(error);
    }

    let preferences = match load_remote_access_preferences(state).await {
        Ok(preferences) => preferences,
        Err(error) => {
            let error =
                fail_named_start(state, current, None, "invalid_configuration", &error).await;
            return Err(error);
        }
    };
    let profile = match preferences.named_tunnel {
        Some(profile) => profile,
        None => {
            let error = fail_named_start(
                state,
                current,
                None,
                "invalid_configuration",
                "Named Tunnel is not configured",
            )
            .await;
            return Err(error);
        }
    };
    if let Err(error) =
        ensure_bridge_for_mode(app, state, BridgePortMode::Fixed(profile.local_port)).await
    {
        let failure_kind = if error.starts_with("Local port unavailable:") {
            "local_port_unavailable"
        } else {
            "local_health_unavailable"
        };
        let error = fail_named_start(state, current, Some(&profile), failure_kind, &error).await;
        return Err(error);
    }
    let token = match state.secret_store.get(CLOUDFLARE_TUNNEL_TOKEN_KEY) {
        Ok(Some(token)) => token,
        Ok(None) => {
            let error = fail_named_start(
                state,
                current,
                Some(&profile),
                "token_missing",
                "Tunnel Token is missing from Keychain",
            )
            .await;
            return Err(error);
        }
        Err(error) => {
            let error = fail_named_start(
                state,
                current,
                Some(&profile),
                "secret_store_unavailable",
                &error.to_string(),
            )
            .await;
            return Err(error);
        }
    };

    if !preflight.stop_named
        && let Err(error) = stop_named_process(state).await
    {
        let error = fail_named_start(
            state,
            current,
            Some(&profile),
            "named_tunnel_stop_failed",
            &error,
        )
        .await;
        return Err(error);
    }
    if let Err(error) = ensure_named_manager(app, state, profile.clone()).await {
        let error = fail_named_start(
            state,
            current,
            Some(&profile),
            "invalid_configuration",
            &error,
        )
        .await;
        return Err(error);
    }
    let start_result = {
        let mut named_tunnel = state.named_tunnel.lock().await;
        named_tunnel
            .as_mut()
            .expect("named tunnel manager exists")
            .start(&token)
            .await
    };
    let snapshot = match start_result {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let detail = redact_sensitive_text(&error.to_string());
            handle_named_pairing_event(state, NamedPairingEvent::StartFailed).await;
            let failed_snapshot = {
                let mut named_tunnel = state.named_tunnel.lock().await;
                named_tunnel
                    .as_mut()
                    .expect("named tunnel manager exists")
                    .status()
            };
            set_transition_mode(
                state,
                current,
                RemoteAccessAction::StartNamed,
                ActionResult::Failed,
            )
            .await;
            record_named_failure_from_snapshot(state, &failed_snapshot, &detail).await;
            let _ = sync_active_bridge_remote_access_context(state).await;
            return Err(detail);
        }
    };
    let public_url = match snapshot.public_url.as_deref() {
        Some(public_url) => public_url,
        None => {
            let _ = stop_named_process(state).await;
            let error = fail_named_start(
                state,
                current,
                Some(&profile),
                "invalid_configuration",
                "Named Tunnel public URL is not available",
            )
            .await;
            return Err(error);
        }
    };
    let pairing_link = match pairing_link_for_public_url(state, public_url).await {
        Ok(link) => link,
        Err(error) => {
            let _ = stop_named_process(state).await;
            handle_named_pairing_event(state, NamedPairingEvent::PairingFailed).await;
            let error = fail_remote_action(
                state,
                current,
                RemoteAccessAction::StartNamed,
                Some(&profile),
                "pairing_failed",
                &error,
            )
            .await;
            return Err(error);
        }
    };
    set_pairing_link(
        state,
        Some(pairing_link),
        Some(PairingLinkSource::NamedTunnel),
    )
    .await;
    set_transition_mode(
        state,
        current,
        RemoteAccessAction::StartNamed,
        ActionResult::Succeeded,
    )
    .await;
    *state.last_named_failure.lock().await = None;
    let _ = sync_bridge_remote_access_context(state, "named", Some(public_url)).await;
    Ok(snapshot.into())
}

async fn start_quick_access(
    app: &AppHandle,
    state: &ShellState,
    current: RemoteAccessMode,
) -> Result<TunnelSnapshotDto, String> {
    if !legacy_quick_start_allowed(current) {
        return Err("Use start_temporary_tunnel to leave Named Tunnel mode explicitly".to_string());
    }
    let bridge_snapshot = ensure_bridge_for_mode(app, state, BridgePortMode::Flexible).await?;
    let local_url = bridge_snapshot
        .port
        .map(|port| format!("http://127.0.0.1:{port}"))
        .ok_or_else(|| "bridge port is not available".to_string())?;
    let snapshot = {
        let mut quick_tunnel = state.quick_tunnel.lock().await;
        quick_tunnel
            .start(local_url)
            .await
            .map_err(|error| redact_sensitive_text(&error.to_string()))?
    };
    let public_url = match snapshot.session.as_ref() {
        Some(session) => session.public_url.as_str(),
        None => {
            let _ = state.quick_tunnel.lock().await.stop().await;
            return Err("Quick Tunnel public URL is not available".to_string());
        }
    };
    let pairing_link = match pairing_link_for_public_url(state, public_url).await {
        Ok(link) => link,
        Err(error) => {
            let _ = state.quick_tunnel.lock().await.stop().await;
            return Err(error);
        }
    };
    activate_quick_remote_access(state, current, pairing_link).await;
    let _ = sync_bridge_remote_access_context(state, "quick", Some(public_url)).await;
    Ok(snapshot.into())
}

async fn stop_remote_access_inner(state: &ShellState) -> Result<(), String> {
    let current = *state.active_remote_mode.lock().await;
    stop_named_process(state).await?;
    stop_quick_if_running(state).await?;
    *state.active_remote_mode.lock().await =
        transition_remote_access(current, RemoteAccessAction::Stop, ActionResult::Succeeded)
            .resulting_mode;
    *state.last_named_failure.lock().await = None;
    clear_pairing_link_for_source(state, PairingLinkSource::NamedTunnel).await;
    clear_pairing_link_for_source(state, PairingLinkSource::QuickTunnel).await;
    let _ = sync_bridge_remote_access_context(state, "local", None).await;
    Ok(())
}

async fn ensure_bridge_for_mode(
    app: &AppHandle,
    state: &ShellState,
    mode: BridgePortMode,
) -> Result<BridgeProcessSnapshot, String> {
    let mut bridge = state.bridge.lock().await;
    let current = bridge.as_ref().map(BridgeProcessManager::status);
    let needs_rebuild = match (current.as_ref(), mode) {
        (Some(snapshot), BridgePortMode::Fixed(port)) => {
            snapshot.port != Some(port)
                || snapshot.port_policy != PortPolicy::Fixed
                || !matches!(
                    snapshot.status,
                    BridgeProcessStatus::Ready | BridgeProcessStatus::Degraded
                )
        }
        (Some(snapshot), BridgePortMode::Flexible) => {
            snapshot.port_policy != PortPolicy::Flexible
                || !matches!(
                    snapshot.status,
                    BridgeProcessStatus::Ready | BridgeProcessStatus::Degraded
                )
        }
        (None, _) => true,
    };
    if needs_rebuild {
        if let Some(manager) = bridge.as_mut() {
            let _ = manager.stop().await;
        }
        *bridge = Some(build_bridge_manager_for_mode(app, state, mode).await?);
    }
    let manager = bridge.as_mut().expect("bridge manager exists");
    if !matches!(
        manager.status().status,
        BridgeProcessStatus::Ready | BridgeProcessStatus::Degraded
    ) {
        let start_result = manager.start().await;
        drop(state.pending_vapid_secret.lock().await.take());
        start_result.map_err(|error| map_bridge_start_error(error.to_string(), mode))?;
    }
    Ok(manager.status())
}

async fn build_bridge_manager_for_mode(
    app: &AppHandle,
    state: &ShellState,
    mode: BridgePortMode,
) -> Result<BridgeProcessManager, String> {
    let mut config = bridge_config(app)?;
    match mode {
        BridgePortMode::Flexible => config.port_policy = PortPolicy::Flexible,
        BridgePortMode::Fixed(port) => {
            config.preferred_port = Some(port);
            config.port_policy = PortPolicy::Fixed;
        }
    }

    let vapid = state
        .vapid_keys
        .load_or_create()
        .map_err(|error| redact_sensitive_text(&error.to_string()))?;
    let secret_file = TemporarySecretFile::create(
        &config.app_data_dir.join("runtime"),
        "vapid-key",
        vapid.private_key_base64.as_bytes(),
    )
    .map_err(|error| redact_sensitive_text(&error.to_string()))?;
    config.extra_env.push((
        "CODEX_MOBILE_BRIDGE_VAPID_KEY_FILE".to_string(),
        secret_file.path().display().to_string(),
    ));
    drop(state.pending_vapid_secret.lock().await.replace(secret_file));

    Ok(BridgeProcessManager::new(config))
}

async fn ensure_named_manager(
    app: &AppHandle,
    state: &ShellState,
    profile: NamedTunnelProfile,
) -> Result<(), String> {
    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("resolve app data dir: {error}"))?;
    let mut named_tunnel = state.named_tunnel.lock().await;
    let needs_rebuild = named_tunnel
        .as_ref()
        .is_none_or(|manager| manager.config().profile != profile);
    if needs_rebuild {
        if let Some(manager) = named_tunnel.as_mut() {
            let _ = manager.stop().await;
        }
        let config = NamedTunnelConfig {
            binary: QuickTunnelConfig::default().binary,
            profile,
            runtime_dir: app_data_dir.join("runtime/named-tunnel"),
            ..NamedTunnelConfig::default()
        };
        *named_tunnel = Some(NamedTunnelManager::new(config));
    }
    Ok(())
}

async fn load_remote_access_preferences(
    state: &ShellState,
) -> Result<RemoteAccessPreferences, String> {
    state
        .remote_preferences
        .lock()
        .await
        .load()
        .map_err(|error| redact_sensitive_text(&error.to_string()))
}

async fn remote_access_preferences_dto(
    state: &ShellState,
) -> Result<RemoteAccessPreferencesDto, String> {
    let preferences = load_remote_access_preferences(state).await?;
    let token_stored = state
        .secret_store
        .get(CLOUDFLARE_TUNNEL_TOKEN_KEY)
        .map_err(|error| redact_sensitive_text(&error.to_string()))?
        .is_some();
    Ok(RemoteAccessPreferencesDto {
        named_profile: preferences.named_tunnel,
        token_stored,
    })
}

async fn stop_quick_if_running(state: &ShellState) -> Result<TunnelSnapshot, String> {
    let mut quick_tunnel = state.quick_tunnel.lock().await;
    let snapshot = quick_tunnel.status();
    if snapshot.status == TunnelStatus::Stopped {
        return Ok(snapshot);
    }
    quick_tunnel
        .stop()
        .await
        .map_err(|error| redact_sensitive_text(&error.to_string()))
}

async fn stop_named_process(state: &ShellState) -> Result<NamedTunnelSnapshot, String> {
    let mut named_tunnel = state.named_tunnel.lock().await;
    let Some(manager) = named_tunnel.as_mut() else {
        return Ok(stopped_named_snapshot());
    };
    if manager.status().status == NamedTunnelStatus::Stopped {
        return Ok(manager.status());
    }
    manager
        .stop()
        .await
        .map_err(|error| redact_sensitive_text(&error.to_string()))
}

async fn pairing_link_for_public_url(
    state: &ShellState,
    public_url: &str,
) -> Result<String, String> {
    let bridge = state.bridge.lock().await;
    let manager = bridge
        .as_ref()
        .ok_or_else(|| "bridge service is not initialized".to_string())?;
    manager
        .create_pairing_link_for_bridge_url(public_url)
        .await
        .map_err(|error| redact_sensitive_text(&error.to_string()))
}

async fn set_pairing_link(
    state: &ShellState,
    link: Option<String>,
    source: Option<PairingLinkSource>,
) {
    let mut pairing_link = state.last_pairing_link.lock().await;
    let mut pairing_source = state.last_pairing_source.lock().await;
    *pairing_link = link;
    *pairing_source = source;
}

async fn clear_pairing_link_for_source(state: &ShellState, expected: PairingLinkSource) {
    let mut pairing_link = state.last_pairing_link.lock().await;
    let mut pairing_source = state.last_pairing_source.lock().await;
    if *pairing_source == Some(expected) {
        *pairing_link = None;
        *pairing_source = None;
    }
}

async fn handle_named_pairing_event(state: &ShellState, event: NamedPairingEvent) {
    if should_clear_named_pairing(event) {
        clear_pairing_link_for_source(state, PairingLinkSource::NamedTunnel).await;
    }
}

async fn set_transition_mode(
    state: &ShellState,
    current: RemoteAccessMode,
    action: RemoteAccessAction,
    result: ActionResult,
) {
    *state.active_remote_mode.lock().await =
        transition_remote_access(current, action, result).resulting_mode;
}

async fn fail_remote_action(
    state: &ShellState,
    current: RemoteAccessMode,
    action: RemoteAccessAction,
    profile: Option<&NamedTunnelProfile>,
    failure_kind: &str,
    detail: &str,
) -> String {
    let detail = redact_sensitive_text(detail);
    set_transition_mode(state, current, action, ActionResult::Failed).await;
    record_named_failure(state, profile, failure_kind, &detail).await;
    let _ = sync_active_bridge_remote_access_context(state).await;
    detail
}

async fn fail_named_start(
    state: &ShellState,
    current: RemoteAccessMode,
    profile: Option<&NamedTunnelProfile>,
    failure_kind: &str,
    detail: &str,
) -> String {
    handle_named_pairing_event(state, NamedPairingEvent::StartFailed).await;
    fail_remote_action(
        state,
        current,
        RemoteAccessAction::StartNamed,
        profile,
        failure_kind,
        detail,
    )
    .await
}

async fn fail_temporary_start(
    state: &ShellState,
    current: RemoteAccessMode,
    profile: Option<&NamedTunnelProfile>,
    detail: &str,
) -> String {
    clear_pairing_link_for_source(state, PairingLinkSource::NamedTunnel).await;
    clear_pairing_link_for_source(state, PairingLinkSource::QuickTunnel).await;
    fail_remote_action(
        state,
        current,
        RemoteAccessAction::StartTemporary,
        profile,
        "temporary_tunnel_failed",
        detail,
    )
    .await
}

async fn require_quick_stopped_for_named_start(
    state: &ShellState,
    current: RemoteAccessMode,
    result: Result<TunnelSnapshot, String>,
) -> Result<(), String> {
    clear_pairing_link_for_source(state, PairingLinkSource::QuickTunnel).await;
    match result {
        Ok(_) => Ok(()),
        Err(error) => {
            Err(fail_named_start(state, current, None, "quick_tunnel_stop_failed", &error).await)
        }
    }
}

async fn activate_quick_remote_access(
    state: &ShellState,
    current: RemoteAccessMode,
    pairing_link: String,
) {
    set_pairing_link(
        state,
        Some(pairing_link),
        Some(PairingLinkSource::QuickTunnel),
    )
    .await;
    set_transition_mode(
        state,
        current,
        RemoteAccessAction::StartTemporary,
        ActionResult::Succeeded,
    )
    .await;
    *state.last_named_failure.lock().await = None;
}

async fn record_named_failure(
    state: &ShellState,
    profile: Option<&NamedTunnelProfile>,
    failure_kind: &str,
    detail: &str,
) {
    let (local_url, public_url) = profile
        .map(|profile| (Some(profile.local_url()), Some(profile.public_url())))
        .unwrap_or((None, None));
    *state.last_named_failure.lock().await = Some(NamedFailureSnapshot {
        local_url,
        public_url,
        failure_kind: failure_kind.to_string(),
        detail: redact_sensitive_text(detail),
    });
}

async fn record_named_failure_from_snapshot(
    state: &ShellState,
    snapshot: &NamedTunnelSnapshot,
    detail: &str,
) {
    *state.last_named_failure.lock().await = Some(NamedFailureSnapshot {
        local_url: snapshot.local_url.clone(),
        public_url: snapshot.public_url.clone(),
        failure_kind: snapshot
            .failure_kind
            .map(named_tunnel_failure_kind)
            .unwrap_or_else(|| "invalid_configuration".to_string()),
        detail: redact_sensitive_text(detail),
    });
}

async fn apply_named_runtime_snapshot(state: &ShellState, snapshot: &NamedTunnelSnapshot) {
    let current = *state.active_remote_mode.lock().await;
    if !matches!(
        current,
        RemoteAccessMode::Named | RemoteAccessMode::NamedFailed
    ) {
        return;
    }

    let next = named_runtime_mode(current, snapshot.status);
    if next != current {
        *state.active_remote_mode.lock().await = next;
    }
    match snapshot.status {
        NamedTunnelStatus::Ready if current == RemoteAccessMode::Named => {
            *state.last_named_failure.lock().await = None;
        }
        NamedTunnelStatus::Degraded if current == RemoteAccessMode::Named => {
            handle_named_pairing_event(state, NamedPairingEvent::RuntimeDegraded).await;
            *state.last_named_failure.lock().await = None;
        }
        NamedTunnelStatus::Failed => {
            handle_named_pairing_event(state, NamedPairingEvent::RuntimeFailed).await;
            let detail = snapshot.detail.as_deref().unwrap_or("Named Tunnel failed");
            record_named_failure_from_snapshot(state, snapshot, detail).await;
        }
        _ => {}
    }
    let _ = sync_active_bridge_remote_access_context(state).await;
}

async fn refresh_named_runtime(
    state: &ShellState,
    force: bool,
) -> Result<Option<NamedTunnelSnapshot>, String> {
    let snapshot = {
        let mut named_tunnel = state.named_tunnel.lock().await;
        match named_tunnel.as_mut() {
            Some(manager) => Some(
                manager
                    .refresh_runtime_health(force)
                    .await
                    .map_err(|error| redact_sensitive_text(&error.to_string()))?,
            ),
            None => None,
        }
    };
    if let Some(snapshot) = snapshot.as_ref() {
        apply_named_runtime_snapshot(state, snapshot).await;
    }
    Ok(snapshot)
}

async fn clear_stale_quick_pairing_link(state: &ShellState, tunnel: &TunnelSnapshot) {
    if matches!(
        tunnel.status,
        TunnelStatus::Ready | TunnelStatus::Reconnecting
    ) {
        return;
    }
    clear_pairing_link_for_source(state, PairingLinkSource::QuickTunnel).await;
}

#[tauri::command]
async fn get_control_diagnostics(state: State<'_, ShellState>) -> Result<Value, String> {
    control_get(state, "/api/control/diagnostics").await
}

#[tauri::command]
async fn list_devices(state: State<'_, ShellState>) -> Result<Vec<DeviceDto>, String> {
    let value = control_get(state, "/api/control/devices").await?;
    serde_json::from_value(value).map_err(|error| error.to_string())
}

#[tauri::command]
async fn revoke_device(device_id: String, state: State<'_, ShellState>) -> Result<(), String> {
    let (url, token) =
        control_request_parts(&state, &format!("/api/control/devices/{device_id}")).await?;
    reqwest::Client::new()
        .delete(url)
        .header(CONTROL_TOKEN_HEADER, token)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
async fn get_diagnostics_bundle(
    app: AppHandle,
    state: State<'_, ShellState>,
) -> Result<Value, String> {
    let (bridge_snapshot, control_request) = {
        let bridge = state.bridge.lock().await;
        match bridge.as_ref() {
            Some(manager) => (
                manager.status(),
                control_request_parts_from_manager(manager, "/api/control/diagnostics").ok(),
            ),
            None => (stopped_bridge_snapshot(), None),
        }
    };
    let control_diagnostics = match control_request {
        Some((url, token)) => control_get_url(url, token)
            .await
            .ok()
            .and_then(|value| serde_json::from_value::<ControlDiagnosticsDto>(value).ok()),
        None => None,
    };
    let named_snapshot = refresh_named_runtime(&state, false).await?;
    let quick_snapshot = state.quick_tunnel.lock().await.refresh_status().await;
    let mode = *state.active_remote_mode.lock().await;
    let named_failure = state.last_named_failure.lock().await.clone();
    let named_profile = load_remote_access_preferences(&state)
        .await
        .ok()
        .and_then(|preferences| preferences.named_tunnel);
    let logs = diagnostic_logs(&app).await;
    let remote_access = remote_access_diagnostics(
        mode,
        named_profile.as_ref(),
        named_snapshot.as_ref(),
        named_failure.as_ref(),
    );
    let recent_connection_states = recent_connection_states(
        &bridge_snapshot,
        &quick_snapshot,
        &remote_access,
        control_diagnostics.as_ref(),
    );
    let tunnel = match mode {
        RemoteAccessMode::NamedFailed => named_failure
            .as_ref()
            .map(named_failure_check)
            .or_else(|| named_snapshot.as_ref().map(named_tunnel_check))
            .unwrap_or_else(|| DiagnosticCheck::failed("named tunnel failed", "unknown failure")),
        RemoteAccessMode::Named => named_snapshot
            .as_ref()
            .map(named_tunnel_check)
            .unwrap_or_else(|| DiagnosticCheck::unknown("named tunnel unavailable")),
        RemoteAccessMode::Quick | RemoteAccessMode::None => tunnel_check(&quick_snapshot),
    };

    let bundle = build_diagnostics_bundle(DiagnosticsBundleInput {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        sidecar_version: None,
        codex_adapter: codex_adapter_check(control_diagnostics.as_ref()),
        bridge: bridge_check(&bridge_snapshot),
        tunnel,
        recent_connection_states,
        logs,
    });

    serde_json::to_value(bundle).map_err(|error| error.to_string())
}

#[tauri::command]
fn copy_text(text: String) -> Result<(), String> {
    write_text_to_clipboard(&text)
}

#[cfg(target_os = "macos")]
fn write_text_to_clipboard(text: &str) -> Result<(), String> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| format!("launch pbcopy: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "pbcopy stdin is not available".to_string())?;
    stdin
        .write_all(text.as_bytes())
        .map_err(|error| format!("write clipboard text: {error}"))?;
    drop(stdin);

    let status = child
        .wait()
        .map_err(|error| format!("wait for pbcopy: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("pbcopy exited with {status}"))
    }
}

#[cfg(not(target_os = "macos"))]
fn write_text_to_clipboard(_text: &str) -> Result<(), String> {
    Err("clipboard copy is only implemented on macOS".to_string())
}

async fn control_get(state: State<'_, ShellState>, path: &str) -> Result<Value, String> {
    let (url, token) = control_request_parts(&state, path).await?;
    control_get_url(url, token).await
}

async fn control_get_url(url: String, token: String) -> Result<Value, String> {
    reqwest::Client::new()
        .get(url)
        .header(CONTROL_TOKEN_HEADER, token)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())
}

async fn sync_bridge_remote_access_context(
    state: &ShellState,
    mode: &str,
    public_origin: Option<&str>,
) -> Result<(), String> {
    let request = {
        let bridge = state.bridge.lock().await;
        bridge.as_ref().and_then(|manager| {
            control_request_parts_from_manager(manager, "/api/control/remote-access").ok()
        })
    };
    let Some((url, token)) = request else {
        return Ok(());
    };
    reqwest::Client::new()
        .put(url)
        .header(CONTROL_TOKEN_HEADER, token)
        .json(&serde_json::json!({
            "mode": mode,
            "publicOrigin": public_origin,
        }))
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    Ok(())
}

async fn sync_active_bridge_remote_access_context(state: &ShellState) -> Result<(), String> {
    let active_mode = *state.active_remote_mode.lock().await;
    let public_origin = match active_mode {
        RemoteAccessMode::Quick => state
            .quick_tunnel
            .lock()
            .await
            .status()
            .session
            .map(|session| session.public_url),
        RemoteAccessMode::Named => {
            let mut named_tunnel = state.named_tunnel.lock().await;
            named_tunnel
                .as_mut()
                .and_then(|manager| manager.status().public_url)
        }
        RemoteAccessMode::None | RemoteAccessMode::NamedFailed => None,
    };
    let (mode, public_origin) = bridge_remote_access_context(active_mode, public_origin);
    sync_bridge_remote_access_context(state, mode, public_origin.as_deref()).await
}

fn bridge_remote_access_context(
    active_mode: RemoteAccessMode,
    public_origin: Option<String>,
) -> (&'static str, Option<String>) {
    match (active_mode, public_origin) {
        (RemoteAccessMode::Quick, Some(origin)) => ("quick", Some(origin)),
        (RemoteAccessMode::Named, Some(origin)) => ("named", Some(origin)),
        _ => ("local", None),
    }
}

async fn control_request_parts(
    state: &State<'_, ShellState>,
    path: &str,
) -> Result<(String, String), String> {
    let bridge = state.bridge.lock().await;
    let manager = bridge
        .as_ref()
        .ok_or_else(|| "bridge service is not initialized".to_string())?;
    control_request_parts_from_manager(manager, path)
}

fn control_request_parts_from_manager(
    manager: &BridgeProcessManager,
    path: &str,
) -> Result<(String, String), String> {
    let snapshot = manager.status();
    let health_url = snapshot
        .health_url
        .ok_or_else(|| "bridge health URL is not available".to_string())?;
    let base = health_url
        .strip_suffix("/api/health")
        .ok_or_else(|| "bridge health URL has an unexpected shape".to_string())?;
    Ok((format!("{base}{path}"), manager.control_token().to_string()))
}

async fn diagnostic_logs(app: &AppHandle) -> Vec<DiagnosticLog> {
    let Ok(app_data_dir) = app.path().app_data_dir() else {
        return Vec::new();
    };
    let log_dir = app_data_dir.join("logs");
    let mut logs = Vec::new();
    for (source, filename) in [
        ("bridge-sidecar.stdout", "bridge-sidecar.stdout.log"),
        ("bridge-sidecar.stderr", "bridge-sidecar.stderr.log"),
    ] {
        if let Some(text) = read_log_tail(&log_dir.join(filename)).await {
            logs.push(DiagnosticLog {
                source: source.to_string(),
                text,
            });
        }
    }
    logs
}

async fn read_log_tail(path: &Path) -> Option<String> {
    let bytes = tokio::fs::read(path).await.ok()?;
    Some(tail_text(&String::from_utf8_lossy(&bytes), 16_384))
}

fn tail_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut start = text.len().saturating_sub(max_bytes);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    format!("[truncated]\n{}", &text[start..])
}

fn codex_adapter_check(diagnostics: Option<&ControlDiagnosticsDto>) -> DiagnosticCheck {
    let Some(diagnostics) = diagnostics else {
        return DiagnosticCheck::unknown("codex diagnostics unavailable");
    };
    if diagnostics.status == "ok" {
        DiagnosticCheck::ok(format!("codex {}", diagnostics.connection_state))
    } else {
        DiagnosticCheck::degraded(
            format!("codex {}", diagnostics.connection_state),
            diagnostics
                .detail
                .clone()
                .unwrap_or_else(|| "Codex adapter is degraded".to_string()),
        )
    }
}

fn bridge_check(snapshot: &BridgeProcessSnapshot) -> DiagnosticCheck {
    match snapshot.status {
        BridgeProcessStatus::Ready => DiagnosticCheck::ok("bridge ready"),
        BridgeProcessStatus::Degraded => DiagnosticCheck::degraded(
            "bridge degraded",
            snapshot
                .detail
                .clone()
                .unwrap_or_else(|| "Bridge health is degraded".to_string()),
        ),
        BridgeProcessStatus::Failed => DiagnosticCheck::failed(
            "bridge failed",
            snapshot
                .detail
                .clone()
                .unwrap_or_else(|| "Bridge process failed".to_string()),
        ),
        BridgeProcessStatus::Starting => DiagnosticCheck::degraded("bridge starting", "starting"),
        BridgeProcessStatus::Stopping => DiagnosticCheck::degraded("bridge stopping", "stopping"),
        BridgeProcessStatus::Stopped => DiagnosticCheck::unknown("bridge stopped"),
    }
}

fn tunnel_check(snapshot: &TunnelSnapshot) -> DiagnosticCheck {
    match snapshot.status {
        TunnelStatus::Ready => DiagnosticCheck::ok(
            snapshot
                .session
                .as_ref()
                .map(|session| format!("tunnel ready {}", session.public_url))
                .unwrap_or_else(|| "tunnel ready".to_string()),
        ),
        TunnelStatus::Reconnecting => DiagnosticCheck::degraded(
            "tunnel reconnecting",
            snapshot
                .detail
                .clone()
                .unwrap_or_else(|| "Tunnel is retrying the public connection".to_string()),
        ),
        TunnelStatus::Failed => DiagnosticCheck::failed(
            "tunnel failed",
            snapshot
                .detail
                .clone()
                .unwrap_or_else(|| "Tunnel provider failed".to_string()),
        ),
        TunnelStatus::Starting => DiagnosticCheck::degraded("tunnel starting", "starting"),
        TunnelStatus::Stopping => DiagnosticCheck::degraded("tunnel stopping", "stopping"),
        TunnelStatus::Stopped => DiagnosticCheck::unknown("tunnel stopped"),
    }
}

fn named_tunnel_check(snapshot: &NamedTunnelSnapshot) -> DiagnosticCheck {
    match snapshot.status {
        NamedTunnelStatus::Ready => DiagnosticCheck::ok(
            snapshot
                .public_url
                .as_ref()
                .map(|url| format!("named tunnel ready {url}"))
                .unwrap_or_else(|| "named tunnel ready".to_string()),
        ),
        NamedTunnelStatus::Degraded => DiagnosticCheck::degraded(
            "named tunnel degraded",
            format!(
                "status=degraded retry_count={} failure_kind={}",
                snapshot.retry_attempt,
                snapshot
                    .failure_kind
                    .map(named_tunnel_failure_kind)
                    .unwrap_or_else(|| "none".to_string())
            ),
        ),
        NamedTunnelStatus::Failed => DiagnosticCheck::failed(
            snapshot
                .failure_kind
                .map(named_tunnel_failure_kind)
                .map(|kind| format!("named tunnel failed ({kind})"))
                .unwrap_or_else(|| "named tunnel failed".to_string()),
            format!(
                "status=failed retry_count={} cloudflared_exit_category={}",
                snapshot.retry_attempt,
                cloudflared_exit_category(
                    snapshot
                        .failure_kind
                        .map(named_tunnel_failure_kind)
                        .as_deref()
                )
                .unwrap_or("none")
            ),
        ),
        NamedTunnelStatus::VerifyingLocal
        | NamedTunnelStatus::Starting
        | NamedTunnelStatus::VerifyingPublic
        | NamedTunnelStatus::Retrying => DiagnosticCheck::degraded(
            "named tunnel starting",
            named_tunnel_status(snapshot.status),
        ),
        NamedTunnelStatus::Stopping => {
            DiagnosticCheck::degraded("named tunnel stopping", "stopping")
        }
        NamedTunnelStatus::Stopped => DiagnosticCheck::unknown("named tunnel stopped"),
    }
}

fn named_failure_check(failure: &NamedFailureSnapshot) -> DiagnosticCheck {
    DiagnosticCheck::failed(
        format!("named tunnel failed ({})", failure.failure_kind),
        format!(
            "failure_kind={} cloudflared_exit_category={}",
            failure.failure_kind,
            cloudflared_exit_category(Some(&failure.failure_kind)).unwrap_or("none")
        ),
    )
}

fn remote_access_diagnostics(
    mode: RemoteAccessMode,
    profile: Option<&NamedTunnelProfile>,
    named: Option<&NamedTunnelSnapshot>,
    named_failure: Option<&NamedFailureSnapshot>,
) -> RemoteAccessDiagnosticsDto {
    let snapshot_failure_kind = named
        .and_then(|snapshot| snapshot.failure_kind)
        .map(named_tunnel_failure_kind);
    let named_failure_kind = if mode == RemoteAccessMode::NamedFailed {
        named_failure
            .map(|failure| failure.failure_kind.clone())
            .or(snapshot_failure_kind)
    } else {
        snapshot_failure_kind.or_else(|| named_failure.map(|failure| failure.failure_kind.clone()))
    };
    let public_health_status = if mode == RemoteAccessMode::NamedFailed {
        "failed"
    } else {
        match named.map(|snapshot| snapshot.status) {
            Some(NamedTunnelStatus::Ready) => "ready",
            Some(NamedTunnelStatus::Degraded) => "degraded",
            Some(NamedTunnelStatus::Failed) => "failed",
            Some(NamedTunnelStatus::VerifyingPublic | NamedTunnelStatus::Retrying) => "checking",
            Some(
                NamedTunnelStatus::VerifyingLocal
                | NamedTunnelStatus::Starting
                | NamedTunnelStatus::Stopping
                | NamedTunnelStatus::Stopped,
            )
            | None => "not_checked",
        }
    };

    RemoteAccessDiagnosticsDto {
        mode: remote_access_mode(mode).to_string(),
        hostname: profile.map(|profile| profile.hostname.clone()),
        local_port: profile.map(|profile| profile.local_port),
        cloudflared_exit_category: cloudflared_exit_category(named_failure_kind.as_deref())
            .map(str::to_string),
        named_failure_kind,
        retry_count: named.map(|snapshot| snapshot.retry_attempt).unwrap_or(0),
        public_health_status: public_health_status.to_string(),
    }
}

fn cloudflared_exit_category(failure_kind: Option<&str>) -> Option<&'static str> {
    match failure_kind {
        Some("child_exited") => Some("unexpected_exit"),
        Some("token_rejected") => Some("credential_rejected"),
        _ => None,
    }
}

fn recent_connection_states(
    bridge: &BridgeProcessSnapshot,
    quick: &TunnelSnapshot,
    remote_access: &RemoteAccessDiagnosticsDto,
    diagnostics: Option<&ControlDiagnosticsDto>,
) -> Vec<String> {
    let mut states = vec![format!("bridge={}", bridge_status(bridge.status))];
    states.push(format!("quick_tunnel={}", tunnel_status(quick.status)));
    states.push(format!(
        "remote_access={}",
        serde_json::to_string(remote_access)
            .unwrap_or_else(|_| "{\"mode\":\"unknown\"}".to_string())
    ));
    if let Some(diagnostics) = diagnostics {
        states.push(format!(
            "codex={} status={}",
            diagnostics.connection_state, diagnostics.status
        ));
        if !diagnostics.push_subscriptions.is_empty() {
            states.push(format!(
                "push_subscriptions={}",
                serde_json::to_string(&diagnostics.push_subscriptions)
                    .unwrap_or_else(|_| "[]".to_string())
            ));
        }
    }
    states
}

fn bridge_config(app: &AppHandle) -> Result<BridgeProcessConfig, String> {
    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("resolve app data dir: {error}"))?;
    let resource_dir = app.path().resource_dir().ok();
    let workspace_root = workspace_root();
    let mut config = BridgeProcessConfig::new(
        sidecar_binary(resource_dir.as_deref(), workspace_root.as_deref()),
        app_data_dir,
        pwa_dist_dir(resource_dir.as_deref(), workspace_root.as_deref()),
    );
    config.preferred_port = Some(DEFAULT_BRIDGE_PORT);
    config.bind_ip = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
    config.health_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    config.advertised_host = advertised_host();
    config.debug_port = debug_port();
    Ok(config)
}

fn sidecar_binary(resource_dir: Option<&Path>, workspace_root: Option<&Path>) -> PathBuf {
    choose_sidecar_binary(
        env::var_os("CODEX_MOBILE_BRIDGE_SIDECAR_BIN").map(PathBuf::from),
        resource_dir,
        workspace_root,
    )
}

fn pwa_dist_dir(resource_dir: Option<&Path>, workspace_root: Option<&Path>) -> PathBuf {
    choose_pwa_dist_dir(
        env::var_os("CODEX_MOBILE_BRIDGE_PWA_DIR").map(PathBuf::from),
        resource_dir,
        workspace_root,
    )
}

fn choose_sidecar_binary(
    env_path: Option<PathBuf>,
    resource_dir: Option<&Path>,
    workspace_root: Option<&Path>,
) -> PathBuf {
    if let Some(path) = env_path {
        return path;
    }
    if let Some(path) = resource_dir
        .map(|dir| dir.join("bin/bridge-sidecar"))
        .filter(|path| path.is_file())
    {
        return path;
    }
    workspace_path(workspace_root, "target/debug/bridge-sidecar")
}

fn choose_pwa_dist_dir(
    env_path: Option<PathBuf>,
    resource_dir: Option<&Path>,
    workspace_root: Option<&Path>,
) -> PathBuf {
    if let Some(path) = env_path {
        return path;
    }
    if let Some(path) = resource_dir
        .map(|dir| dir.join("mobile-pwa"))
        .filter(|path| path.join("index.html").is_file())
    {
        return path;
    }
    workspace_path(workspace_root, "apps/mobile-pwa/dist")
}

fn workspace_root() -> Option<PathBuf> {
    let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    find_workspace_root(&current_dir)
}

fn workspace_path(workspace_root: Option<&Path>, path: &str) -> PathBuf {
    workspace_root
        .map(Path::to_path_buf)
        .or_else(|| env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
        .join(path)
}

fn find_workspace_root(start: &Path) -> Option<PathBuf> {
    start.ancestors().find_map(|candidate| {
        let has_workspace_manifest = candidate.join("Cargo.toml").is_file();
        let has_desktop_core = candidate.join("crates/desktop-core").is_dir();
        (has_workspace_manifest && has_desktop_core).then(|| candidate.to_path_buf())
    })
}

fn debug_port() -> u16 {
    env::var("CODEX_MOBILE_BRIDGE_DEBUG_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_DEBUG_PORT)
}

fn advertised_host() -> String {
    if let Ok(host) = env::var("CODEX_MOBILE_BRIDGE_ADVERTISED_HOST")
        && !host.trim().is_empty()
    {
        return host;
    }

    macos_wifi_ip()
        .or_else(lan_ip)
        .filter(|ip| is_phone_reachable_ip(*ip))
        .map(host_for_url)
        .unwrap_or_else(|| Ipv4Addr::LOCALHOST.to_string())
}

fn macos_wifi_ip() -> Option<IpAddr> {
    let output = Command::new("ipconfig")
        .args(["getifaddr", "en0"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

fn lan_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(8, 8, 8, 8), 80)).ok()?;
    Some(socket.local_addr().ok()?.ip())
}

fn is_phone_reachable_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            !ip.is_loopback() && !ip.is_link_local() && !matches!(ip.octets(), [198, 18 | 19, _, _])
        }
        IpAddr::V6(ip) => !ip.is_loopback() && !ip.is_unspecified(),
    }
}

fn host_for_url(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    }
}

fn stopped_bridge_snapshot() -> BridgeProcessSnapshot {
    BridgeProcessSnapshot {
        status: BridgeProcessStatus::Stopped,
        pid: None,
        port: None,
        port_policy: PortPolicy::Flexible,
        health_url: None,
        detail: None,
    }
}

fn stopped_named_snapshot() -> NamedTunnelSnapshot {
    NamedTunnelSnapshot {
        status: NamedTunnelStatus::Stopped,
        pid: None,
        local_url: None,
        public_url: None,
        retry_attempt: 0,
        failure_kind: None,
        detail: None,
    }
}

impl From<BridgeProcessSnapshot> for BridgeProcessSnapshotDto {
    fn from(snapshot: BridgeProcessSnapshot) -> Self {
        Self {
            status: bridge_status(snapshot.status).to_string(),
            pid: snapshot.pid,
            port: snapshot.port,
            health_url: snapshot.health_url,
            detail: snapshot.detail.map(|detail| redact_sensitive_text(&detail)),
        }
    }
}

impl From<TunnelSnapshot> for TunnelSnapshotDto {
    fn from(snapshot: TunnelSnapshot) -> Self {
        let (public_url, local_url) = if matches!(
            snapshot.status,
            TunnelStatus::Ready | TunnelStatus::Reconnecting
        ) {
            snapshot
                .session
                .map(|session| (Some(session.public_url), Some(session.local_url)))
                .unwrap_or((None, None))
        } else {
            (None, None)
        };
        Self {
            status: tunnel_status(snapshot.status).to_string(),
            public_url,
            local_url,
            detail: snapshot.detail.map(|detail| redact_sensitive_text(&detail)),
        }
    }
}

impl From<NamedTunnelSnapshot> for NamedTunnelSnapshotDto {
    fn from(snapshot: NamedTunnelSnapshot) -> Self {
        let detail = match snapshot.status {
            NamedTunnelStatus::Degraded => {
                let suffix = snapshot
                    .detail
                    .as_deref()
                    .map(redact_sensitive_text)
                    .filter(|detail| !detail.is_empty())
                    .map(|detail| format!(": {detail}"))
                    .unwrap_or_default();
                Some(format!(
                    "Fixed domain is temporarily unreachable; waiting for cloudflared to recover{suffix}"
                ))
            }
            _ => snapshot.detail.map(|detail| redact_sensitive_text(&detail)),
        };
        Self {
            status: named_tunnel_status(snapshot.status).to_string(),
            pid: snapshot.pid,
            local_url: snapshot.local_url,
            public_url: snapshot.public_url,
            retry_attempt: snapshot.retry_attempt,
            failure_kind: snapshot.failure_kind.map(named_tunnel_failure_kind),
            detail,
        }
    }
}

impl NamedTunnelSnapshotDto {
    fn stopped() -> Self {
        stopped_named_snapshot().into()
    }

    fn failed(failure: NamedFailureSnapshot) -> Self {
        Self {
            status: "failed".to_string(),
            pid: None,
            local_url: failure.local_url,
            public_url: failure.public_url,
            retry_attempt: 0,
            failure_kind: Some(failure.failure_kind),
            detail: Some(redact_sensitive_text(&failure.detail)),
        }
    }
}

impl From<CodexLaunchOutcome> for CodexLaunchOutcomeDto {
    fn from(outcome: CodexLaunchOutcome) -> Self {
        Self {
            status: format!("{:?}", outcome.status),
            debug_port: outcome.debug_port,
            app_path: outcome.app_path.map(|path| path.display().to_string()),
            launch_command: outcome.launch_command.map(Into::into),
            detail: outcome.detail,
            instructions: outcome.instructions,
        }
    }
}

impl From<CodexLaunchCommand> for CodexLaunchCommandDto {
    fn from(command: CodexLaunchCommand) -> Self {
        Self {
            program: command.program,
            args: command.args,
        }
    }
}

fn bridge_status(status: BridgeProcessStatus) -> &'static str {
    match status {
        BridgeProcessStatus::Stopped => "stopped",
        BridgeProcessStatus::Starting => "starting",
        BridgeProcessStatus::Ready => "ready",
        BridgeProcessStatus::Degraded => "degraded",
        BridgeProcessStatus::Failed => "failed",
        BridgeProcessStatus::Stopping => "stopping",
    }
}

fn tunnel_status(status: TunnelStatus) -> &'static str {
    match status {
        TunnelStatus::Stopped => "stopped",
        TunnelStatus::Starting => "starting",
        TunnelStatus::Ready => "ready",
        TunnelStatus::Reconnecting => "reconnecting",
        TunnelStatus::Failed => "failed",
        TunnelStatus::Stopping => "stopping",
    }
}

fn named_tunnel_status(status: NamedTunnelStatus) -> &'static str {
    match status {
        NamedTunnelStatus::Stopped => "stopped",
        NamedTunnelStatus::VerifyingLocal => "verifying_local",
        NamedTunnelStatus::Starting => "starting",
        NamedTunnelStatus::VerifyingPublic => "verifying_public",
        NamedTunnelStatus::Retrying => "retrying",
        NamedTunnelStatus::Ready => "ready",
        NamedTunnelStatus::Degraded => "degraded",
        NamedTunnelStatus::Failed => "failed",
        NamedTunnelStatus::Stopping => "stopping",
    }
}

fn named_tunnel_failure_kind(kind: NamedTunnelFailureKind) -> String {
    match kind {
        NamedTunnelFailureKind::InvalidConfiguration => "invalid_configuration",
        NamedTunnelFailureKind::TokenMissing => "token_missing",
        NamedTunnelFailureKind::TokenRejected => "token_rejected",
        NamedTunnelFailureKind::LocalHealthUnavailable => "local_health_unavailable",
        NamedTunnelFailureKind::DnsNotReady => "dns_not_ready",
        NamedTunnelFailureKind::PublicRouteRejected => "public_route_rejected",
        NamedTunnelFailureKind::WrongBridgeInstance => "wrong_bridge_instance",
        NamedTunnelFailureKind::NetworkUnavailable => "network_unavailable",
        NamedTunnelFailureKind::ChildExited => "child_exited",
    }
    .to_string()
}

fn remote_access_mode(mode: RemoteAccessMode) -> &'static str {
    match mode {
        RemoteAccessMode::None => "none",
        RemoteAccessMode::Quick => "quick",
        RemoteAccessMode::Named => "named",
        RemoteAccessMode::NamedFailed => "named_failed",
    }
}

fn map_bridge_start_error(error: String, mode: BridgePortMode) -> String {
    match mode {
        BridgePortMode::Fixed(port)
            if error == format!("preferred bridge port {port} is unavailable") =>
        {
            format!("Local port unavailable: {port}")
        }
        _ => redact_sensitive_text(&error),
    }
}

fn begin_exit_cleanup(state: &ShellState) -> bool {
    let should_start = !state.exit_cleanup_started.swap(true, Ordering::SeqCst);
    if should_start {
        state.supervisor_shutdown.notify_one();
    }
    should_start
}

fn start_named_tunnel_supervisor(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            {
                let state = app.state::<ShellState>();
                if state.exit_cleanup_started.load(Ordering::SeqCst) {
                    break;
                }
                tokio::select! {
                    _ = tokio::time::sleep(NAMED_TUNNEL_SUPERVISOR_INTERVAL) => {}
                    _ = state.supervisor_shutdown.notified() => break,
                }
            }
            let state = app.state::<ShellState>();
            if state.exit_cleanup_started.load(Ordering::SeqCst) {
                break;
            }
            let snapshot = {
                let mut named_tunnel = state.named_tunnel.lock().await;
                named_tunnel.as_mut().map(NamedTunnelManager::status)
            };
            if let Some(snapshot) = snapshot {
                if should_supervisor_refresh_named(snapshot.status) {
                    let _ = refresh_named_runtime(&state, false).await;
                } else if snapshot.status == NamedTunnelStatus::Failed {
                    apply_named_runtime_snapshot(&state, &snapshot).await;
                }
            }
        }
    });
}

async fn shutdown_managed_processes(state: &ShellState) {
    {
        let mut named_tunnel = state.named_tunnel.lock().await;
        if let Some(manager) = named_tunnel.as_mut() {
            let _ = manager.stop().await;
        }
    }
    {
        let mut quick_tunnel = state.quick_tunnel.lock().await;
        let _ = quick_tunnel.stop().await;
    }
    {
        let mut bridge = state.bridge.lock().await;
        if let Some(manager) = bridge.as_mut() {
            let _ = manager.stop().await;
        }
    }
}

fn terminate_managed_processes_now(state: &ShellState) {
    state.exit_cleanup_started.store(true, Ordering::SeqCst);
    state.supervisor_shutdown.notify_one();
    if let Ok(mut named_tunnel) = state.named_tunnel.try_lock()
        && let Some(manager) = named_tunnel.as_mut()
    {
        manager.terminate_now();
    }
    if let Ok(mut quick_tunnel) = state.quick_tunnel.try_lock() {
        quick_tunnel.terminate_now();
    }
    if let Ok(mut bridge) = state.bridge.try_lock()
        && let Some(manager) = bridge.as_mut()
    {
        manager.terminate_now();
    }
}

fn main() {
    let app = tauri::Builder::default()
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            app.manage(ShellState::new(
                RemoteAccessConfigStore::new(app_data_dir.join("remote-access.json")),
                Arc::new(KeyringSecretStore::new(SECRET_STORE_SERVICE)),
            ));
            start_named_tunnel_supervisor(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_app_status,
            get_remote_access_preferences,
            save_named_tunnel_profile,
            delete_named_tunnel_profile,
            ensure_codex_ready,
            start_bridge,
            stop_bridge,
            create_pairing_link,
            start_quick_tunnel,
            rotate_quick_tunnel,
            stop_quick_tunnel,
            start_named_tunnel,
            retry_named_tunnel,
            recheck_named_tunnel_health,
            stop_named_tunnel,
            start_temporary_tunnel,
            stop_remote_access,
            get_control_diagnostics,
            get_diagnostics_bundle,
            copy_text,
            list_devices,
            revoke_device,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Codex Mobile Bridge desktop shell");
    let exit_code = app.run_return(|app_handle, event| match event {
        tauri::RunEvent::ExitRequested { code, api, .. } => {
            let should_cleanup = {
                let state = app_handle.state::<ShellState>();
                begin_exit_cleanup(&state)
            };
            if should_cleanup {
                api.prevent_exit();
                let app_handle = app_handle.clone();
                let exit_code = code.unwrap_or(0);
                tauri::async_runtime::spawn(async move {
                    {
                        let state = app_handle.state::<ShellState>();
                        shutdown_managed_processes(&state).await;
                    }
                    app_handle.exit(exit_code);
                });
            }
        }
        tauri::RunEvent::Exit => {
            let state = app_handle.state::<ShellState>();
            terminate_managed_processes_now(&state);
        }
        _ => {}
    });
    std::process::exit(exit_code);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, sync::Arc};
    use tempfile::tempdir;

    #[test]
    fn sidecar_binary_prefers_env_path() {
        let resource_dir = tempdir().expect("resource tempdir");
        let workspace_dir = tempdir().expect("workspace tempdir");
        let env_path = PathBuf::from("/custom/bridge-sidecar");

        assert_eq!(
            choose_sidecar_binary(
                Some(env_path.clone()),
                Some(resource_dir.path()),
                Some(workspace_dir.path())
            ),
            env_path
        );
    }

    #[test]
    fn sidecar_binary_prefers_bundled_resource_over_workspace_fallback() {
        let resource_dir = tempdir().expect("resource tempdir");
        let bundled_sidecar = resource_dir.path().join("bin/bridge-sidecar");
        fs::create_dir_all(bundled_sidecar.parent().expect("parent")).expect("bin dir");
        fs::write(&bundled_sidecar, "sidecar").expect("sidecar file");
        let workspace_dir = tempdir().expect("workspace tempdir");

        assert_eq!(
            choose_sidecar_binary(None, Some(resource_dir.path()), Some(workspace_dir.path())),
            bundled_sidecar
        );
    }

    #[test]
    fn pwa_dist_prefers_bundled_resource_with_index() {
        let resource_dir = tempdir().expect("resource tempdir");
        let bundled_pwa = resource_dir.path().join("mobile-pwa");
        fs::create_dir_all(&bundled_pwa).expect("pwa dir");
        fs::write(bundled_pwa.join("index.html"), "<html></html>").expect("index");
        let workspace_dir = tempdir().expect("workspace tempdir");

        assert_eq!(
            choose_pwa_dist_dir(None, Some(resource_dir.path()), Some(workspace_dir.path())),
            bundled_pwa
        );
    }

    #[test]
    fn resource_lookup_falls_back_to_workspace_paths_when_bundle_is_incomplete() {
        let resource_dir = tempdir().expect("resource tempdir");
        let workspace_dir = tempdir().expect("workspace tempdir");

        assert_eq!(
            choose_sidecar_binary(None, Some(resource_dir.path()), Some(workspace_dir.path())),
            workspace_dir.path().join("target/debug/bridge-sidecar")
        );
        assert_eq!(
            choose_pwa_dist_dir(None, Some(resource_dir.path()), Some(workspace_dir.path())),
            workspace_dir.path().join("apps/mobile-pwa/dist")
        );
    }

    #[test]
    fn bridge_check_maps_stopped_and_failed_states() {
        assert_eq!(
            bridge_check(&stopped_bridge_snapshot()).label,
            "bridge stopped"
        );
        let failed = BridgeProcessSnapshot {
            status: BridgeProcessStatus::Failed,
            pid: None,
            port: None,
            port_policy: PortPolicy::Flexible,
            health_url: None,
            detail: Some("Authorization: Bearer secret".to_string()),
        };

        let check = bridge_check(&failed);

        assert_eq!(check.label, "bridge failed");
        assert_eq!(
            check.detail.as_deref(),
            Some("Authorization: Bearer secret")
        );
    }

    #[test]
    fn tunnel_dto_hides_urls_when_tunnel_failed() {
        let dto = TunnelSnapshotDto::from(TunnelSnapshot {
            status: TunnelStatus::Failed,
            session: Some(desktop_core::TunnelSession {
                id: "tunnel-1".to_string(),
                local_url: "http://127.0.0.1:57324".to_string(),
                public_url: "https://stale.trycloudflare.com".to_string(),
                started_at: 1,
            }),
            detail: Some("tunnel provider exited".to_string()),
        });

        assert_eq!(dto.status, "failed");
        assert_eq!(dto.public_url, None);
        assert_eq!(dto.local_url, None);
    }

    #[test]
    fn tunnel_dto_keeps_urls_while_tunnel_reconnects() {
        let dto = TunnelSnapshotDto::from(TunnelSnapshot {
            status: TunnelStatus::Reconnecting,
            session: Some(desktop_core::TunnelSession {
                id: "tunnel-1".to_string(),
                local_url: "http://127.0.0.1:57324".to_string(),
                public_url: "https://active.trycloudflare.com".to_string(),
                started_at: 1,
            }),
            detail: Some("Retrying automatically".to_string()),
        });

        assert_eq!(dto.status, "reconnecting");
        assert_eq!(
            dto.public_url.as_deref(),
            Some("https://active.trycloudflare.com")
        );
        assert_eq!(dto.local_url.as_deref(), Some("http://127.0.0.1:57324"));
    }

    #[test]
    fn bridge_dto_redacts_sensitive_detail() {
        let dto = BridgeProcessSnapshotDto::from(BridgeProcessSnapshot {
            status: BridgeProcessStatus::Failed,
            pid: None,
            port: Some(57324),
            port_policy: PortPolicy::Fixed,
            health_url: None,
            detail: Some("Authorization: Bearer secret-token".to_string()),
        });

        assert_eq!(dto.detail.as_deref(), Some("Authorization: [REDACTED]"));
    }

    #[test]
    fn remote_access_named_dto_keeps_fixed_urls_while_degraded() {
        let dto = NamedTunnelSnapshotDto::from(desktop_core::NamedTunnelSnapshot {
            status: desktop_core::NamedTunnelStatus::Degraded,
            pid: Some(42),
            local_url: Some("http://127.0.0.1:57324".to_string()),
            public_url: Some("https://codex.example.com".to_string()),
            retry_attempt: 0,
            failure_kind: None,
            detail: Some("public network is unavailable".to_string()),
        });

        assert_eq!(dto.status, "degraded");
        assert_eq!(dto.local_url.as_deref(), Some("http://127.0.0.1:57324"));
        assert_eq!(dto.public_url.as_deref(), Some("https://codex.example.com"));
        assert!(
            dto.detail
                .as_deref()
                .is_some_and(|detail| detail.contains("waiting for cloudflared to recover"))
        );
    }

    #[test]
    fn remote_access_preferences_dto_only_exposes_token_presence() {
        let dto = RemoteAccessPreferencesDto {
            named_profile: Some(
                desktop_core::NamedTunnelProfile::new("codex.example.com", 57324).unwrap(),
            ),
            token_stored: true,
        };
        let json = serde_json::to_value(dto).unwrap();

        assert_eq!(json["tokenStored"], true);
        assert!(json.get("token").is_none());
        assert!(!json.to_string().contains("secret-token"));
    }

    #[test]
    fn remote_access_fixed_port_error_is_explicit_and_keeps_port() {
        assert_eq!(
            map_bridge_start_error(
                "preferred bridge port 57324 is unavailable".to_string(),
                BridgePortMode::Fixed(57324),
            ),
            "Local port unavailable: 57324"
        );
    }

    #[test]
    fn remote_access_named_failure_diagnostic_uses_safe_categories_only() {
        let check = named_failure_check(&NamedFailureSnapshot {
            local_url: Some("http://127.0.0.1:57324".to_string()),
            public_url: Some("https://codex.example.com".to_string()),
            failure_kind: "local_port_unavailable".to_string(),
            detail: "Local port unavailable: 57324\nAuthorization: Bearer secret-token".to_string(),
        });

        assert_eq!(check.label, "named tunnel failed (local_port_unavailable)");
        assert_eq!(
            check.detail.as_deref(),
            Some("failure_kind=local_port_unavailable cloudflared_exit_category=none")
        );
        assert!(!format!("{check:?}").contains("secret-token"));
    }

    #[test]
    fn remote_access_diagnostics_only_include_non_sensitive_fields() {
        let profile = NamedTunnelProfile::new("codex.example.com", 57324).unwrap();
        let sensitive_detail = concat!(
            "CLOUDFLARE_TUNNEL_TOKEN=cloudflare-secret\n",
            "token_file_contents=token-file-secret\n",
            "VAPID_PRIVATE_KEY=vapid-secret\n",
            "p256dh=push-public-key auth=push-auth-secret"
        );
        let named = NamedTunnelSnapshot {
            status: NamedTunnelStatus::Failed,
            pid: None,
            local_url: Some(profile.local_url()),
            public_url: Some(profile.public_url()),
            retry_attempt: 3,
            failure_kind: Some(NamedTunnelFailureKind::ChildExited),
            detail: Some(sensitive_detail.to_string()),
        };
        let failure = NamedFailureSnapshot {
            local_url: named.local_url.clone(),
            public_url: named.public_url.clone(),
            failure_kind: "child_exited".to_string(),
            detail: sensitive_detail.to_string(),
        };

        let json = serde_json::to_value(remote_access_diagnostics(
            RemoteAccessMode::NamedFailed,
            Some(&profile),
            Some(&named),
            Some(&failure),
        ))
        .unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "mode": "named_failed",
                "hostname": "codex.example.com",
                "localPort": 57324,
                "namedFailureKind": "child_exited",
                "retryCount": 3,
                "publicHealthStatus": "failed",
                "cloudflaredExitCategory": "unexpected_exit"
            })
        );
        let serialized = json.to_string();
        for secret in [
            "cloudflare-secret",
            "token-file-secret",
            "vapid-secret",
            "push-public-key",
            "push-auth-secret",
        ] {
            assert!(!serialized.contains(secret));
        }
    }

    #[test]
    fn tail_text_truncates_from_end_on_large_logs() {
        let text = "0123456789abcdef";

        assert_eq!(tail_text(text, 6), "[truncated]\nabcdef");
        assert_eq!(tail_text(text, 64), text);
    }

    #[test]
    fn exit_cleanup_only_starts_once() {
        let state = ShellState::default();

        assert!(begin_exit_cleanup(&state));
        assert!(!begin_exit_cleanup(&state));
    }

    #[test]
    fn remote_access_starting_named_stops_quick_without_automatic_fallback() {
        assert_eq!(
            transition_remote_access(
                RemoteAccessMode::Quick,
                RemoteAccessAction::StartNamed,
                ActionResult::Failed,
            ),
            RemoteAccessTransition {
                stop_quick: true,
                stop_named: false,
                start_quick: false,
                start_named: true,
                resulting_mode: RemoteAccessMode::NamedFailed,
            }
        );
    }

    #[test]
    fn remote_access_manual_temporary_is_the_only_named_failure_path_to_quick() {
        let transition = transition_remote_access(
            RemoteAccessMode::NamedFailed,
            RemoteAccessAction::StartTemporary,
            ActionResult::Succeeded,
        );

        assert!(transition.stop_named);
        assert!(transition.start_quick);
        assert!(!transition.start_named);
        assert_eq!(transition.resulting_mode, RemoteAccessMode::Quick);

        for (action, result) in [
            (RemoteAccessAction::StartNamed, ActionResult::Succeeded),
            (RemoteAccessAction::StartNamed, ActionResult::Failed),
            (RemoteAccessAction::Stop, ActionResult::Succeeded),
            (RemoteAccessAction::Stop, ActionResult::Failed),
        ] {
            assert_ne!(
                transition_remote_access(RemoteAccessMode::NamedFailed, action, result)
                    .resulting_mode,
                RemoteAccessMode::Quick
            );
        }
    }

    #[test]
    fn remote_access_failed_manual_temporary_attempt_remains_named_failed() {
        assert_eq!(
            transition_remote_access(
                RemoteAccessMode::NamedFailed,
                RemoteAccessAction::StartTemporary,
                ActionResult::Failed,
            ),
            RemoteAccessTransition {
                stop_quick: false,
                stop_named: true,
                start_quick: true,
                start_named: false,
                resulting_mode: RemoteAccessMode::NamedFailed,
            }
        );
    }

    #[test]
    fn remote_access_stop_clears_every_mode_without_starting_a_tunnel() {
        for current in [
            RemoteAccessMode::None,
            RemoteAccessMode::Quick,
            RemoteAccessMode::Named,
            RemoteAccessMode::NamedFailed,
        ] {
            let transition = transition_remote_access(
                current,
                RemoteAccessAction::Stop,
                ActionResult::Succeeded,
            );

            assert!(!transition.start_quick);
            assert!(!transition.start_named);
            assert_eq!(transition.resulting_mode, RemoteAccessMode::None);
        }
    }

    #[test]
    fn remote_access_retry_from_named_restarts_named_without_quick() {
        assert_eq!(
            transition_remote_access(
                RemoteAccessMode::Named,
                RemoteAccessAction::StartNamed,
                ActionResult::Succeeded,
            ),
            RemoteAccessTransition {
                stop_quick: false,
                stop_named: true,
                start_quick: false,
                start_named: true,
                resulting_mode: RemoteAccessMode::Named,
            }
        );
    }

    #[test]
    fn remote_access_retry_from_named_failure_stays_failed_without_quick() {
        assert_eq!(
            transition_remote_access(
                RemoteAccessMode::NamedFailed,
                RemoteAccessAction::StartNamed,
                ActionResult::Failed,
            ),
            RemoteAccessTransition {
                stop_quick: false,
                stop_named: true,
                start_quick: false,
                start_named: true,
                resulting_mode: RemoteAccessMode::NamedFailed,
            }
        );
    }

    #[test]
    fn remote_access_named_pairing_policy_clears_failures_but_preserves_degraded() {
        for event in [
            NamedPairingEvent::StartAttempt,
            NamedPairingEvent::StartFailed,
            NamedPairingEvent::PairingFailed,
            NamedPairingEvent::RuntimeFailed,
        ] {
            assert!(should_clear_named_pairing(event));
        }
        assert!(!should_clear_named_pairing(
            NamedPairingEvent::RuntimeDegraded
        ));
    }

    #[test]
    fn remote_access_named_failed_latches_against_stale_ready_manager() {
        assert_eq!(
            named_runtime_mode(RemoteAccessMode::NamedFailed, NamedTunnelStatus::Ready),
            RemoteAccessMode::NamedFailed
        );
        assert_eq!(
            named_runtime_mode(RemoteAccessMode::NamedFailed, NamedTunnelStatus::Degraded,),
            RemoteAccessMode::NamedFailed
        );
        assert_eq!(
            named_runtime_mode(RemoteAccessMode::Named, NamedTunnelStatus::Failed),
            RemoteAccessMode::NamedFailed
        );
    }

    #[test]
    fn remote_access_supervisor_refreshes_only_ready_or_degraded_named_manager() {
        for status in [NamedTunnelStatus::Ready, NamedTunnelStatus::Degraded] {
            assert!(should_supervisor_refresh_named(status));
        }
        for status in [
            NamedTunnelStatus::Stopped,
            NamedTunnelStatus::VerifyingLocal,
            NamedTunnelStatus::Starting,
            NamedTunnelStatus::VerifyingPublic,
            NamedTunnelStatus::Retrying,
            NamedTunnelStatus::Failed,
            NamedTunnelStatus::Stopping,
        ] {
            assert!(!should_supervisor_refresh_named(status));
        }
    }

    #[test]
    fn remote_access_legacy_quick_start_cannot_leave_named_modes() {
        assert!(!legacy_quick_start_allowed(RemoteAccessMode::Named));
        assert!(!legacy_quick_start_allowed(RemoteAccessMode::NamedFailed));
        assert!(legacy_quick_start_allowed(RemoteAccessMode::None));
        assert!(legacy_quick_start_allowed(RemoteAccessMode::Quick));
    }

    #[tokio::test]
    async fn remote_access_named_start_quick_stop_failure_latches_and_redacts() {
        let state = ShellState::default();
        *state.active_remote_mode.lock().await = RemoteAccessMode::Quick;
        set_pairing_link(
            &state,
            Some("https://temporary.example/pair".to_string()),
            Some(PairingLinkSource::QuickTunnel),
        )
        .await;

        let result = require_quick_stopped_for_named_start(
            &state,
            RemoteAccessMode::Quick,
            Err("Authorization: Bearer secret-token".to_string()),
        )
        .await;

        assert_eq!(result.unwrap_err(), "Authorization: [REDACTED]");
        assert_eq!(
            *state.active_remote_mode.lock().await,
            RemoteAccessMode::NamedFailed
        );
        let failure = state.last_named_failure.lock().await.clone().unwrap();
        assert_eq!(failure.failure_kind, "quick_tunnel_stop_failed");
        assert!(!failure.detail.contains("secret-token"));
        assert_eq!(*state.last_pairing_link.lock().await, None);
        assert_eq!(*state.last_pairing_source.lock().await, None);
    }

    #[tokio::test]
    async fn remote_access_temporary_success_sets_quick_mode_and_pairing_source() {
        let state = ShellState::default();
        *state.active_remote_mode.lock().await = RemoteAccessMode::NamedFailed;

        activate_quick_remote_access(
            &state,
            RemoteAccessMode::NamedFailed,
            "codex://pair?token=opaque".to_string(),
        )
        .await;

        assert_eq!(
            *state.active_remote_mode.lock().await,
            RemoteAccessMode::Quick
        );
        assert_eq!(
            *state.last_pairing_source.lock().await,
            Some(PairingLinkSource::QuickTunnel)
        );
    }

    #[tokio::test]
    async fn remote_access_temporary_failure_preserves_profile_and_secret() {
        let directory = tempdir().unwrap();
        let profile = NamedTunnelProfile::new("codex.example.com", 57324).unwrap();
        let preferences = RemoteAccessConfigStore::new(directory.path().join("remote-access.json"));
        preferences
            .save(&RemoteAccessPreferences {
                named_tunnel: Some(profile.clone()),
            })
            .unwrap();
        let secrets = Arc::new(MemorySecretStore::default());
        secrets
            .set(CLOUDFLARE_TUNNEL_TOKEN_KEY, "stored-secret")
            .unwrap();
        let state = ShellState::new(preferences, secrets.clone());
        set_pairing_link(
            &state,
            Some("https://codex.example.com/pair".to_string()),
            Some(PairingLinkSource::NamedTunnel),
        )
        .await;

        fail_temporary_start(
            &state,
            RemoteAccessMode::NamedFailed,
            Some(&profile),
            "temporary provider failed",
        )
        .await;

        assert_eq!(
            load_remote_access_preferences(&state)
                .await
                .unwrap()
                .named_tunnel,
            Some(profile)
        );
        assert_eq!(
            secrets.get(CLOUDFLARE_TUNNEL_TOKEN_KEY).unwrap().as_deref(),
            Some("stored-secret")
        );
        assert_eq!(*state.last_pairing_link.lock().await, None);
        assert_eq!(*state.last_pairing_source.lock().await, None);
    }

    #[test]
    fn bridge_remote_access_context_requires_a_live_public_origin() {
        assert_eq!(
            bridge_remote_access_context(
                RemoteAccessMode::Named,
                Some("https://codex.example.com".into())
            ),
            ("named", Some("https://codex.example.com".into()))
        );
        assert_eq!(
            bridge_remote_access_context(
                RemoteAccessMode::Quick,
                Some("https://temporary.example.com".into())
            ),
            ("quick", Some("https://temporary.example.com".into()))
        );
        assert_eq!(
            bridge_remote_access_context(RemoteAccessMode::Named, None),
            ("local", None)
        );
        assert_eq!(
            bridge_remote_access_context(
                RemoteAccessMode::NamedFailed,
                Some("https://stale.example.com".into())
            ),
            ("local", None)
        );
    }
}
