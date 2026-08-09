use std::{env, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use anyhow::Context;
use bridge_core::{
    alert_monitor::{AlertMonitor, AlertMonitorConfig},
    cdp::{CdpClient, CdpNotificationStream, select_codex_target},
    codex_rpc::{AppServerJsonRpcClient, CdpAppServerTransport, CodexAdapter, CodexRawEvent},
    diagnostics::diagnose_cdp_app_server,
    event_hub::EventHub,
    http_api::{AppState, serve_with_static_dir},
    notification_dispatcher::NotificationDispatcher,
    notification_store::NotificationStore,
    pairing::PairingManager,
    public_access::PublicAccessState,
    push_delivery_worker::PushDeliveryWorker,
    storage::Storage,
    vapid::VapidRuntimeKey,
    web_push::{RustWebPushTransport, WebPushSender},
};
use uuid::Uuid;

const DEFAULT_BIND_ADDR: &str = "0.0.0.0:57324";
const DEFAULT_DEBUG_PORT: u16 = 9229;
const DEFAULT_PWA_DIR: &str = "apps/mobile-pwa/dist";
const NOTIFICATION_RECONNECT_MIN: Duration = Duration::from_millis(250);
const NOTIFICATION_RECONNECT_MAX: Duration = Duration::from_secs(5);

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
    let vapid_key = env::var_os("CODEX_MOBILE_BRIDGE_VAPID_KEY_FILE")
        .map(PathBuf::from)
        .map(|path| VapidRuntimeKey::from_secret_file(&path))
        .transpose()
        .context("load VAPID runtime key")?
        .map(Arc::new);
    let storage = Storage::open(db_path.clone()).context("open bridge storage")?;
    let notification_store = Arc::new(tokio::sync::Mutex::new(
        NotificationStore::open(&db_path).context("open notification storage")?,
    ));
    let control_token = env::var("CODEX_MOBILE_BRIDGE_CONTROL_TOKEN")
        .unwrap_or_else(|_| Uuid::new_v4().to_string());
    let instance_id =
        env::var("CODEX_MOBILE_BRIDGE_INSTANCE_ID").unwrap_or_else(|_| Uuid::new_v4().to_string());
    let pairing = PairingManager::new(storage);
    println!("Codex mobile bridge listening on {bind_addr}");
    println!("Serving PWA assets");
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
    let codex_adapter = cdp_app_server_adapter(&cdp_client).await;
    let event_hub = EventHub::new();
    let public_access = PublicAccessState::default();
    let push_runtime = match vapid_key.as_ref() {
        Some(vapid_key) => {
            let wake = Arc::new(tokio::sync::Notify::new());
            let sender = WebPushSender::new(
                Arc::new(RustWebPushTransport::new().context("create Web Push transport")?),
                Arc::clone(vapid_key),
            );
            Some((wake, sender))
        }
        None => None,
    };
    let notification_dispatcher = match push_runtime.as_ref() {
        Some((wake, _)) => {
            NotificationDispatcher::new(Arc::clone(&notification_store), event_hub.clone())
                .with_push_runtime(public_access.clone(), Arc::clone(wake))
        }
        None => NotificationDispatcher::new(Arc::clone(&notification_store), event_hub.clone()),
    };
    let mut state = AppState::new(pairing, event_hub, control_token)
        .with_instance_id(instance_id)
        .with_notification_store(Arc::clone(&notification_store))
        .with_public_access(public_access.clone())
        .with_diagnostics(diagnostics);
    if let Some(vapid_key) = vapid_key.as_ref() {
        state = state.with_vapid_key(Arc::clone(vapid_key));
    }
    if let Some((wake, sender)) = push_runtime {
        state = state.with_push_runtime(Arc::clone(&wake));
        tokio::spawn(
            PushDeliveryWorker::new(Arc::clone(&notification_store), sender, public_access, wake)
                .run(),
        );
    }
    if let Some(adapter) = codex_adapter {
        tokio::spawn(
            AlertMonitor::new(
                Arc::clone(&adapter),
                Arc::clone(&notification_store),
                notification_dispatcher,
                AlertMonitorConfig::default(),
            )
            .run(),
        );
        state = state.with_codex_adapter(adapter);
        tokio::spawn(run_notification_forwarder(
            cdp_client.clone(),
            state.clone(),
        ));
    }

    serve_with_static_dir(bind_addr, state, pwa_dir).await
}

async fn run_notification_forwarder(cdp_client: CdpClient, state: AppState) {
    let mut reconnect_delay = NOTIFICATION_RECONNECT_MIN;
    loop {
        let connection = async {
            let targets = cdp_client.list_targets().await?;
            let target = select_codex_target(&targets)?;
            cdp_client.inject_app_server_bridge(&target).await?;
            CdpNotificationStream::connect(&target).await
        }
        .await;

        match connection {
            Ok(mut stream) => {
                reconnect_delay = NOTIFICATION_RECONNECT_MIN;
                loop {
                    match stream.next_notification().await {
                        Ok(Some(notification)) => {
                            state
                                .apply_codex_notification(CodexRawEvent {
                                    method: notification.method,
                                    params: notification.params,
                                })
                                .await;
                        }
                        Ok(None) => break,
                        Err(error) => {
                            eprintln!("Codex realtime notification stream disconnected: {error}");
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                eprintln!("Codex realtime notification stream unavailable: {error}");
            }
        }

        tokio::time::sleep(reconnect_delay).await;
        reconnect_delay = reconnect_delay
            .saturating_mul(2)
            .min(NOTIFICATION_RECONNECT_MAX);
    }
}

async fn cdp_app_server_adapter(cdp_client: &CdpClient) -> Option<Arc<dyn CodexAdapter>> {
    let targets = cdp_client.list_targets().await.ok()?;
    let target = select_codex_target(&targets).ok()?;
    cdp_client.inject_app_server_bridge(&target).await.ok()?;
    Some(Arc::new(AppServerJsonRpcClient::new(
        CdpAppServerTransport::new(cdp_client.clone(), target),
    )))
}
