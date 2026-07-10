use std::{
    env,
    net::{IpAddr, Ipv4Addr, UdpSocket},
    path::{Path, PathBuf},
    process::Command,
};

use desktop_core::{
    BridgeProcessConfig, BridgeProcessManager, BridgeProcessSnapshot, BridgeProcessStatus,
    CodexLaunchCommand, CodexLaunchConfig, CodexLaunchManager, CodexLaunchOutcome, DiagnosticCheck,
    DiagnosticLog, DiagnosticsBundleInput, QuickTunnelConfig, QuickTunnelManager, TunnelSnapshot,
    TunnelStatus, build_diagnostics_bundle,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager, State};
use tokio::sync::Mutex;

const DEFAULT_DEBUG_PORT: u16 = 9229;
const DEFAULT_BRIDGE_PORT: u16 = 57324;
const CONTROL_TOKEN_HEADER: &str = "x-bridge-control-token";

struct ShellState {
    bridge: Mutex<Option<BridgeProcessManager>>,
    tunnel: Mutex<QuickTunnelManager>,
    last_pairing_link: Mutex<Option<String>>,
}

impl Default for ShellState {
    fn default() -> Self {
        Self {
            bridge: Mutex::new(None),
            tunnel: Mutex::new(QuickTunnelManager::new(QuickTunnelConfig::default())),
            last_pairing_link: Mutex::new(None),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ShellStatusDto {
    bridge: BridgeProcessSnapshotDto,
    tunnel: TunnelSnapshotDto,
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TunnelSnapshotDto {
    status: String,
    public_url: Option<String>,
    local_url: Option<String>,
    detail: Option<String>,
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
    created_at: u64,
    last_seen_at: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ControlDiagnosticsDto {
    status: String,
    connection_state: String,
    detail: Option<String>,
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
    let tunnel = state.tunnel.lock().await.status();
    let last_pairing_link = state.last_pairing_link.lock().await.clone();

    Ok(ShellStatusDto {
        bridge: BridgeProcessSnapshotDto::from(bridge),
        tunnel: TunnelSnapshotDto::from(tunnel),
        last_pairing_link,
    })
}

#[tauri::command]
async fn ensure_codex_ready() -> CodexLaunchOutcomeDto {
    let mut config = CodexLaunchConfig::default();
    config.debug_port = debug_port();
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
    let mut bridge = state.bridge.lock().await;
    if bridge.is_none() {
        *bridge = Some(BridgeProcessManager::new(bridge_config(&app)?));
    }

    let manager = bridge.as_mut().expect("bridge manager is initialized");
    let snapshot = manager.start().await.map_err(|error| error.to_string())?;
    Ok(snapshot.into())
}

#[tauri::command]
async fn stop_bridge(state: State<'_, ShellState>) -> Result<BridgeProcessSnapshotDto, String> {
    let mut bridge = state.bridge.lock().await;
    let manager = bridge
        .as_mut()
        .ok_or_else(|| "bridge service is not initialized".to_string())?;
    let snapshot = manager.stop().await.map_err(|error| error.to_string())?;
    *state.last_pairing_link.lock().await = None;
    Ok(snapshot.into())
}

#[tauri::command]
async fn create_pairing_link(state: State<'_, ShellState>) -> Result<String, String> {
    let link = {
        let bridge = state.bridge.lock().await;
        let manager = bridge
            .as_ref()
            .ok_or_else(|| "bridge service is not initialized".to_string())?;
        manager
            .create_pairing_link()
            .await
            .map_err(|error| error.to_string())?
    };
    *state.last_pairing_link.lock().await = Some(link.clone());
    Ok(link)
}

#[tauri::command]
async fn start_quick_tunnel(state: State<'_, ShellState>) -> Result<TunnelSnapshotDto, String> {
    let (snapshot, pairing_link) = {
        let bridge = state.bridge.lock().await;
        let manager = bridge
            .as_ref()
            .ok_or_else(|| "bridge service must be running before tunnel starts".to_string())?;
        let local_url = bridge_local_url(manager)?;
        let mut tunnel = state.tunnel.lock().await;
        let snapshot = tunnel
            .start(local_url)
            .await
            .map_err(|error| error.to_string())?;
        let pairing_link = match snapshot.session.as_ref() {
            Some(session) => Some(
                manager
                    .create_pairing_link_for_bridge_url(&session.public_url)
                    .await
                    .map_err(|error| error.to_string())?,
            ),
            None => None,
        };
        (snapshot, pairing_link)
    };
    *state.last_pairing_link.lock().await = pairing_link;
    Ok(snapshot.into())
}

#[tauri::command]
async fn rotate_quick_tunnel(state: State<'_, ShellState>) -> Result<TunnelSnapshotDto, String> {
    let (snapshot, pairing_link) = {
        let bridge = state.bridge.lock().await;
        let manager = bridge
            .as_ref()
            .ok_or_else(|| "bridge service must be running before tunnel rotates".to_string())?;
        let mut tunnel = state.tunnel.lock().await;
        let snapshot = tunnel.rotate().await.map_err(|error| error.to_string())?;
        let pairing_link = match snapshot.session.as_ref() {
            Some(session) => Some(
                manager
                    .create_pairing_link_for_bridge_url(&session.public_url)
                    .await
                    .map_err(|error| error.to_string())?,
            ),
            None => None,
        };
        (snapshot, pairing_link)
    };
    *state.last_pairing_link.lock().await = pairing_link;
    Ok(snapshot.into())
}

#[tauri::command]
async fn stop_quick_tunnel(state: State<'_, ShellState>) -> Result<TunnelSnapshotDto, String> {
    let mut tunnel = state.tunnel.lock().await;
    let snapshot = tunnel.stop().await.map_err(|error| error.to_string())?;
    *state.last_pairing_link.lock().await = None;
    Ok(snapshot.into())
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
    let tunnel_snapshot = state.tunnel.lock().await.status();
    let logs = diagnostic_logs(&app).await;
    let recent_connection_states = recent_connection_states(
        &bridge_snapshot,
        &tunnel_snapshot,
        control_diagnostics.as_ref(),
    );

    let bundle = build_diagnostics_bundle(DiagnosticsBundleInput {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        sidecar_version: None,
        codex_adapter: codex_adapter_check(control_diagnostics.as_ref()),
        bridge: bridge_check(&bridge_snapshot),
        tunnel: tunnel_check(&tunnel_snapshot),
        recent_connection_states,
        logs,
    });

    serde_json::to_value(bundle).map_err(|error| error.to_string())
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

fn recent_connection_states(
    bridge: &BridgeProcessSnapshot,
    tunnel: &TunnelSnapshot,
    diagnostics: Option<&ControlDiagnosticsDto>,
) -> Vec<String> {
    let mut states = vec![format!("bridge={}", bridge_status(bridge.status))];
    states.push(format!("tunnel={}", tunnel_status(tunnel.status)));
    if let Some(diagnostics) = diagnostics {
        states.push(format!(
            "codex={} status={}",
            diagnostics.connection_state, diagnostics.status
        ));
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

fn bridge_local_url(manager: &BridgeProcessManager) -> Result<String, String> {
    let snapshot = manager.status();
    let port = snapshot
        .port
        .ok_or_else(|| "bridge port is not available".to_string())?;
    Ok(format!("http://127.0.0.1:{port}"))
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
        health_url: None,
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
            detail: snapshot.detail,
        }
    }
}

impl From<TunnelSnapshot> for TunnelSnapshotDto {
    fn from(snapshot: TunnelSnapshot) -> Self {
        let (public_url, local_url) = snapshot
            .session
            .map(|session| (Some(session.public_url), Some(session.local_url)))
            .unwrap_or((None, None));
        Self {
            status: tunnel_status(snapshot.status).to_string(),
            public_url,
            local_url,
            detail: snapshot.detail,
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
        TunnelStatus::Failed => "failed",
        TunnelStatus::Stopping => "stopping",
    }
}

fn main() {
    tauri::Builder::default()
        .manage(ShellState::default())
        .invoke_handler(tauri::generate_handler![
            get_app_status,
            ensure_codex_ready,
            start_bridge,
            stop_bridge,
            create_pairing_link,
            start_quick_tunnel,
            rotate_quick_tunnel,
            stop_quick_tunnel,
            get_control_diagnostics,
            get_diagnostics_bundle,
            list_devices,
            revoke_device,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Codex Mobile Bridge desktop shell");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
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
    fn tail_text_truncates_from_end_on_large_logs() {
        let text = "0123456789abcdef";

        assert_eq!(tail_text(text, 6), "[truncated]\nabcdef");
        assert_eq!(tail_text(text, 64), text);
    }
}
