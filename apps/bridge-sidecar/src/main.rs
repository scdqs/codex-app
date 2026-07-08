use std::{env, net::SocketAddr, path::PathBuf};

use anyhow::Context;
use bridge_core::{
    event_hub::EventHub,
    http_api::{AppState, serve},
    pairing::PairingManager,
    storage::Storage,
};
use uuid::Uuid;

const DEFAULT_BIND_ADDR: &str = "0.0.0.0:57324";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bind_addr = env::var("CODEX_MOBILE_BRIDGE_BIND")
        .unwrap_or_else(|_| DEFAULT_BIND_ADDR.to_string())
        .parse::<SocketAddr>()
        .context("CODEX_MOBILE_BRIDGE_BIND must be a socket address")?;
    let db_path = env::var_os("CODEX_MOBILE_BRIDGE_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("bridge.sqlite"));
    let storage = Storage::open(db_path).context("open bridge storage")?;
    let control_token = Uuid::new_v4().to_string();
    println!("Codex mobile bridge listening on {bind_addr}");
    println!(
        "Local control token for starting device pairing: {control_token}. Keep this token on this machine; it is not exposed by the HTTP API."
    );
    let state = AppState::new(PairingManager::new(storage), EventHub::new(), control_token);

    serve(bind_addr, state).await
}
