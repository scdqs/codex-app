use std::{env, net::SocketAddr, path::PathBuf};

use anyhow::Context;
use bridge_core::{
    cdp::CdpClient,
    diagnostics::diagnose_cdp_app_server,
    event_hub::EventHub,
    http_api::{AppState, serve},
    pairing::PairingManager,
    storage::Storage,
};
use uuid::Uuid;

const DEFAULT_BIND_ADDR: &str = "0.0.0.0:57324";
const DEFAULT_DEBUG_PORT: u16 = 9229;

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
    let storage = Storage::open(db_path).context("open bridge storage")?;
    let control_token = Uuid::new_v4().to_string();
    println!("Codex mobile bridge listening on {bind_addr}");
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
    let state = AppState::new(PairingManager::new(storage), EventHub::new(), control_token)
        .with_diagnostics(diagnostics);

    serve(bind_addr, state).await
}
