use std::{
    env,
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    path::PathBuf,
    process::Command,
    sync::Arc,
};

use anyhow::Context;
use bridge_core::{
    cdp::{CdpClient, select_codex_target},
    codex_rpc::{AppServerJsonRpcClient, CdpAppServerTransport, CodexAdapter},
    diagnostics::diagnose_cdp_app_server,
    event_hub::EventHub,
    http_api::{AppState, serve_with_static_dir},
    pairing::PairingManager,
    storage::Storage,
};
use uuid::Uuid;

const DEFAULT_BIND_ADDR: &str = "0.0.0.0:57324";
const DEFAULT_DEBUG_PORT: u16 = 9229;
const DEFAULT_PWA_DIR: &str = "apps/mobile-pwa/dist";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bind_addr = env::var("CODEX_MOBILE_BRIDGE_BIND")
        .unwrap_or_else(|_| DEFAULT_BIND_ADDR.to_string())
        .parse::<SocketAddr>()
        .context("CODEX_MOBILE_BRIDGE_BIND must be a socket address")?;
    let debug_port = match env::var("CODEX_MOBILE_BRIDGE_DEBUG_PORT") {
        Ok(value) => value
            .parse::<u16>()
            .context("CODEX_MOBILE_BRIDGE_DEBUG_PORT must be a TCP port")?,
        Err(env::VarError::NotPresent) => DEFAULT_DEBUG_PORT,
        Err(error) => return Err(error).context("read CODEX_MOBILE_BRIDGE_DEBUG_PORT"),
    };
    let db_path = env::var_os("CODEX_MOBILE_BRIDGE_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("bridge.sqlite"));
    let pwa_dir = env::var_os("CODEX_MOBILE_BRIDGE_PWA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_PWA_DIR));
    let storage = Storage::open(db_path).context("open bridge storage")?;
    let control_token = env::var("CODEX_MOBILE_BRIDGE_CONTROL_TOKEN")
        .unwrap_or_else(|_| Uuid::new_v4().to_string());
    let mut pairing = PairingManager::new(storage);
    let startup_pairing_token = pairing
        .create_token()
        .context("create startup pairing token")?;
    let bridge_url = bridge_url_for_bind_addr(bind_addr);
    let pairing_url = format!(
        "{bridge_url}/?pairingToken={}&bridgeUrl={}",
        url_encode_component(&startup_pairing_token),
        url_encode_component(&bridge_url)
    );
    println!("Codex mobile bridge listening on {bind_addr}");
    println!("Serving PWA from {}", pwa_dir.display());
    println!("PWA pairing URL: {pairing_url}");
    println!("QR text: {pairing_url}");
    let cdp_client = CdpClient::new(debug_port).context("create cdp client")?;
    let diagnostics = diagnose_cdp_app_server(&cdp_client).await;
    println!(
        "Codex CDP debug port {debug_port}: {} ({}){}",
        diagnostics.status.as_str(),
        diagnostics.connection_state.as_str(),
        diagnostics
            .detail
            .as_deref()
            .map(|detail| format!(" detail={detail}"))
            .unwrap_or_default()
    );
    println!(
        "Local control token for starting device pairing: {control_token}. Keep this token on this machine; it is not exposed by the HTTP API."
    );
    let codex_adapter = cdp_app_server_adapter(&cdp_client).await;
    let mut state =
        AppState::new(pairing, EventHub::new(), control_token).with_diagnostics(diagnostics);
    if let Some(adapter) = codex_adapter {
        state = state.with_codex_adapter(adapter);
    }

    serve_with_static_dir(bind_addr, state, pwa_dir).await
}

async fn cdp_app_server_adapter(cdp_client: &CdpClient) -> Option<Arc<dyn CodexAdapter>> {
    let targets = cdp_client.list_targets().await.ok()?;
    let target = select_codex_target(&targets).ok()?;
    cdp_client.inject_app_server_bridge(&target).await.ok()?;
    Some(Arc::new(AppServerJsonRpcClient::new(
        CdpAppServerTransport::new(cdp_client.clone(), target),
    )))
}

fn bridge_url_for_bind_addr(bind_addr: SocketAddr) -> String {
    let ip = if bind_addr.ip().is_unspecified() {
        macos_wifi_ip()
            .or_else(lan_ip)
            .filter(|ip| is_phone_reachable_ip(*ip))
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
    } else {
        bind_addr.ip()
    };
    format!("http://{}:{}", host_for_url(ip), bind_addr.port())
}

fn macos_wifi_ip() -> Option<IpAddr> {
    let output = Command::new("ipconfig")
        .args(["getifaddr", "en0"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    text.trim().parse().ok()
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
