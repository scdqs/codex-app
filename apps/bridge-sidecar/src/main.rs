use std::{env, net::SocketAddr, path::PathBuf};

use anyhow::Context;
use bridge_core::{
    event_hub::EventHub,
    http_api::{AppState, serve},
    pairing::PairingManager,
    storage::Storage,
};

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
    let state = AppState::new(PairingManager::new(storage), EventHub::new());

    serve(bind_addr, state).await
}
