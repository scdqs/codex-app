use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
};

use axum::{
    Extension, Json, Router,
    body::Body,
    extract::{
        Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Mutex, RwLock};
use tower_http::services::{ServeDir, ServeFile};
use uuid::Uuid;

use crate::{
    codex_rpc::{CodexAdapter, CodexRpcError},
    diagnostics::DiagnosticsReport,
    event_hub::EventHub,
    local_assets::LocalAssetRegistry,
    normalizer::Normalizer,
    pairing::{DEFAULT_PAIRING_TOKEN_TTL_MS, PairingError, PairingManager},
    protocol::{
        ApprovalDecision, DecisionKind, ServerEnvelope, SessionEvent, SessionEventType,
        SessionSnapshot,
    },
};

#[derive(Clone)]
pub struct AppState {
    pairing: Arc<Mutex<PairingManager>>,
    event_hub: EventHub,
    event_history: Arc<Mutex<HashMap<String, VecDeque<SessionEvent>>>>,
    refresh_failures: Arc<Mutex<HashMap<String, usize>>>,
    local_assets: Arc<Mutex<LocalAssetRegistry>>,
    control_token: Arc<str>,
    codex_adapter: Option<Arc<dyn CodexAdapter>>,
    diagnostics: Arc<RwLock<DiagnosticsReport>>,
}

const EVENT_HISTORY_LIMIT_PER_THREAD: usize = 256;
#[cfg(test)]
const MAX_REFRESH_FAILURES_PER_DEVICE: usize = 5;
const BRIDGE_CONTROL_TOKEN_HEADER: &str = "x-bridge-control-token";

#[derive(Debug, Clone)]
struct AuthenticatedDevice {
    device_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    status: String,
    connection_state: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingStartResponse {
    pairing_token: String,
    expires_in_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingCompleteRequest {
    pairing_token: String,
    device_id: String,
    display_name: String,
    device_secret: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingCompleteResponse {
    device_id: String,
    session_token: String,
    session_expires_at: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRefreshRequest {
    device_id: String,
    device_secret: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRefreshResponse {
    device_id: String,
    session_token: String,
    session_expires_at: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceResponse {
    device_id: String,
    display_name: String,
    created_at: u64,
    last_seen_at: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageRequest {
    text: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptedResponse {
    accepted: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalDecisionRequest {
    decision: DecisionKind,
    comment: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorResponse {
    error: String,
}

impl AppState {
    pub fn new(
        pairing: PairingManager,
        event_hub: EventHub,
        control_token: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            pairing: Arc::new(Mutex::new(pairing)),
            event_hub,
            event_history: Arc::new(Mutex::new(HashMap::new())),
            refresh_failures: Arc::new(Mutex::new(HashMap::new())),
            local_assets: Arc::new(Mutex::new(LocalAssetRegistry::default())),
            control_token: control_token.into(),
            codex_adapter: None,
            diagnostics: Arc::new(RwLock::new(DiagnosticsReport::default())),
        }
    }

    pub fn with_codex_adapter(mut self, adapter: Arc<dyn CodexAdapter>) -> Self {
        self.codex_adapter = Some(adapter);
        self
    }

    pub fn with_diagnostics(self, diagnostics: DiagnosticsReport) -> Self {
        Self {
            diagnostics: Arc::new(RwLock::new(diagnostics)),
            ..self
        }
    }

    pub fn event_hub(&self) -> EventHub {
        self.event_hub.clone()
    }

    pub async fn publish_session_event(&self, event: SessionEvent) -> usize {
        self.record_session_event(event.clone()).await;
        self.event_hub.publish(ServerEnvelope::SessionEvent(event))
    }

    async fn record_session_event(&self, event: SessionEvent) {
        let mut event_history = self.event_history.lock().await;
        let thread_events = event_history
            .entry(event.thread_id.clone())
            .or_insert_with(VecDeque::new);
        if thread_events.len() == EVENT_HISTORY_LIMIT_PER_THREAD {
            thread_events.pop_front();
        }
        thread_events.push_back(event);
    }

    async fn register_local_assets_for_event(&self, mut event: SessionEvent) -> SessionEvent {
        let local_image_replacements = local_image_attachment_replacements(&event.payload);
        scrub_raw_local_image_paths(&mut event.payload);

        let Some(attachments) = event
            .payload
            .get_mut("attachments")
            .and_then(serde_json::Value::as_array_mut)
        else {
            return event;
        };

        for index in 0..attachments.len() {
            let Some(path) = local_image_attachment_path(&attachments[index]) else {
                continue;
            };
            let token = self.local_assets.lock().await.register_image(path);

            if let Some(object) = attachments[index].as_object_mut() {
                object.remove("path");
                object.insert(
                    "src".to_string(),
                    json!(format!("/api/assets/local-image/{token}")),
                );
            }
        }

        scrub_local_image_path_strings(&mut event.payload, &local_image_replacements);
        event
    }
}

pub fn build_router(state: AppState) -> Router {
    let authenticated_routes = Router::new()
        .route(
            "/api/assets/local-image/:asset_token",
            get(get_local_image_asset),
        )
        .route("/api/devices", get(list_devices))
        .route("/api/devices/:id", delete(revoke_device))
        .route("/api/sessions", get(list_sessions))
        .route("/api/sessions/:thread_id/events", get(list_session_events))
        .route("/api/sessions/:thread_id/messages", post(send_message))
        .route(
            "/api/approvals/:approval_id/decision",
            post(decide_approval),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer_auth,
        ));
    let websocket_route =
        Router::new()
            .route("/ws", get(ws_handler))
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_websocket_auth,
            ));

    Router::new()
        .route("/api/health", get(health))
        .route("/api/pairing/start", post(start_pairing))
        .route("/api/pairing/complete", post(complete_pairing))
        .route("/api/session/refresh", post(refresh_session))
        .merge(authenticated_routes)
        .merge(websocket_route)
        .with_state(state)
}

pub fn build_router_with_static_dir(state: AppState, static_dir: impl Into<PathBuf>) -> Router {
    let static_dir = static_dir.into();
    build_router(state).fallback_service(
        ServeDir::new(&static_dir).fallback(ServeFile::new(static_dir.join("index.html"))),
    )
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let diagnostics = state.diagnostics.read().await;
    Json(HealthResponse {
        status: diagnostics.status.as_str().to_string(),
        connection_state: diagnostics.connection_state.as_str().to_string(),
    })
}

async fn start_pairing(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PairingStartResponse>, ApiError> {
    require_control_token(&state, &headers)?;

    let mut pairing = state.pairing.lock().await;
    let pairing_token = pairing.create_token()?;

    Ok(Json(PairingStartResponse {
        pairing_token,
        expires_in_ms: DEFAULT_PAIRING_TOKEN_TTL_MS,
    }))
}

async fn refresh_session(
    State(state): State<AppState>,
    Json(request): Json<SessionRefreshRequest>,
) -> Result<Json<SessionRefreshResponse>, ApiError> {
    let failure_key = refresh_failure_key(&request.device_id);
    let refresh_result = {
        let mut pairing = state.pairing.lock().await;
        pairing.create_session(&request.device_id, &request.device_secret)
    };

    let registration = match refresh_result {
        Ok(registration) => {
            state.clear_refresh_failures(&failure_key).await;
            registration
        }
        Err(_) => {
            state.record_refresh_failure(failure_key).await;
            return Err(ApiError::Unauthorized);
        }
    };

    Ok(Json(SessionRefreshResponse {
        device_id: registration.device_id,
        session_token: registration.session_token,
        session_expires_at: registration.session_expires_at,
    }))
}

async fn complete_pairing(
    State(state): State<AppState>,
    Json(request): Json<PairingCompleteRequest>,
) -> Result<Json<PairingCompleteResponse>, ApiError> {
    let mut pairing = state.pairing.lock().await;
    let registration = pairing.register_device(
        &request.pairing_token,
        &request.device_id,
        &request.display_name,
        &request.device_secret,
    )?;

    Ok(Json(PairingCompleteResponse {
        device_id: registration.device_id,
        session_token: registration.session_token,
        session_expires_at: registration.session_expires_at,
    }))
}

async fn list_devices(
    State(state): State<AppState>,
) -> Result<Json<Vec<DeviceResponse>>, ApiError> {
    let pairing = state.pairing.lock().await;
    let devices = pairing
        .active_devices()?
        .into_iter()
        .map(|device| DeviceResponse {
            device_id: device.device_id,
            display_name: device.display_name,
            created_at: device.created_at,
            last_seen_at: device.last_seen_at,
        })
        .collect();

    Ok(Json(devices))
}

async fn revoke_device(
    State(state): State<AppState>,
    Path(device_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let pairing = state.pairing.lock().await;
    pairing.revoke_device(&device_id)?;

    Ok(StatusCode::NO_CONTENT)
}

async fn list_sessions(
    State(state): State<AppState>,
) -> Result<Json<Vec<SessionSnapshot>>, ApiError> {
    if let Some(adapter) = state.codex_adapter.as_ref() {
        let threads = adapter.list_threads().await?;
        for thread in threads {
            state
                .event_hub
                .set_snapshot(Normalizer::snapshot_from_thread(&thread))
                .await;
        }
    }

    Ok(Json(state.event_hub.all_snapshots().await))
}

async fn list_session_events(
    State(state): State<AppState>,
    Path(thread_id): Path<String>,
) -> Result<Json<Vec<SessionEvent>>, ApiError> {
    if let Some(adapter) = state.codex_adapter.as_ref() {
        let turns = adapter.list_turns(&thread_id).await?;
        let mut events = Vec::new();
        for event in Normalizer::events_from_turns(&thread_id, &turns) {
            let event = state.register_local_assets_for_event(event).await;
            state.record_session_event(event.clone()).await;
            events.push(event);
        }
        return Ok(Json(events));
    }

    let events = state
        .event_history
        .lock()
        .await
        .get(&thread_id)
        .map(|events| events.iter().cloned().collect())
        .unwrap_or_default();

    Ok(Json(events))
}

async fn get_local_image_asset(
    State(state): State<AppState>,
    Path(asset_token): Path<String>,
) -> Result<Response, ApiError> {
    let path = state
        .local_assets
        .lock()
        .await
        .path_for(&asset_token)
        .ok_or(ApiError::NotFound("local image asset not found"))?;
    let content_type = image_content_type(&path)
        .ok_or(ApiError::UnsupportedMediaType("unsupported media type"))?;
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(ApiError::Forbidden("local image asset permission denied"));
        }
        Err(_) => return Err(ApiError::NotFound("local image asset not found")),
    };

    Ok(([(header::CONTENT_TYPE, content_type)], bytes).into_response())
}

fn local_image_attachment_path(attachment: &serde_json::Value) -> Option<PathBuf> {
    let object = attachment.as_object()?;
    let is_image = object
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(|value| value == "image")
        .unwrap_or(false);
    if !is_image {
        return None;
    }

    object
        .get("path")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
}

fn local_image_attachment_replacements(payload: &serde_json::Value) -> Vec<(String, String)> {
    let Some(attachments) = payload
        .get("attachments")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };

    attachments
        .iter()
        .filter_map(|attachment| {
            let object = attachment.as_object()?;
            let path = object.get("path").and_then(serde_json::Value::as_str)?;
            let name = object
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
                .or_else(|| {
                    FsPath::new(path)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "image".to_string());
            Some((path.to_string(), name))
        })
        .collect()
}

fn scrub_raw_local_image_paths(payload: &mut serde_json::Value) {
    if let Some(raw) = payload.get_mut("raw") {
        scrub_local_image_paths(raw);
    }
}

fn scrub_local_image_paths(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                scrub_local_image_paths(item);
            }
        }
        serde_json::Value::Object(object) => {
            let is_local_image = object
                .get("type")
                .and_then(serde_json::Value::as_str)
                .map(|value| value.eq_ignore_ascii_case("localimage"))
                .unwrap_or(false);
            if is_local_image {
                object.remove("path");
            }

            for child in object.values_mut() {
                scrub_local_image_paths(child);
            }
        }
        _ => {}
    }
}

fn scrub_local_image_path_strings(
    value: &mut serde_json::Value,
    replacements: &[(String, String)],
) {
    if replacements.is_empty() {
        return;
    }

    match value {
        serde_json::Value::String(text) => {
            for (path, name) in replacements {
                if text.contains(path) {
                    *text = text.replace(path, name);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                scrub_local_image_path_strings(item, replacements);
            }
        }
        serde_json::Value::Object(object) => {
            for child in object.values_mut() {
                scrub_local_image_path_strings(child, replacements);
            }
        }
        _ => {}
    }
}

fn image_content_type(path: &FsPath) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?;
    if extension.eq_ignore_ascii_case("png") {
        Some("image/png")
    } else if extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg") {
        Some("image/jpeg")
    } else if extension.eq_ignore_ascii_case("gif") {
        Some("image/gif")
    } else if extension.eq_ignore_ascii_case("webp") {
        Some("image/webp")
    } else {
        None
    }
}

async fn send_message(
    State(state): State<AppState>,
    Path(thread_id): Path<String>,
    Json(request): Json<SendMessageRequest>,
) -> Result<(StatusCode, Json<AcceptedResponse>), ApiError> {
    if let Some(adapter) = state.codex_adapter.as_ref() {
        adapter.send_user_message(&thread_id, &request.text).await?;
    }

    let event = SessionEvent {
        id: Uuid::new_v4().to_string(),
        thread_id,
        event_type: SessionEventType::Message,
        payload: json!({
            "role": "user",
            "text": request.text,
        }),
        created_at: current_time_ms(),
    };
    state.publish_session_event(event).await;

    Ok((
        StatusCode::ACCEPTED,
        Json(AcceptedResponse { accepted: true }),
    ))
}

async fn decide_approval(
    State(state): State<AppState>,
    Path(approval_id): Path<String>,
    Extension(device): Extension<AuthenticatedDevice>,
    Json(request): Json<ApprovalDecisionRequest>,
) -> Result<(StatusCode, Json<AcceptedResponse>), ApiError> {
    let decision = ApprovalDecision {
        approval_id,
        decision: request.decision,
        comment: request.comment,
        device_id: device.device_id,
        decided_at: current_time_ms(),
    };

    if let Some(adapter) = state.codex_adapter.as_ref() {
        adapter
            .respond_approval(&decision.approval_id, &decision)
            .await?;
    }

    state
        .event_hub
        .publish(ServerEnvelope::ApprovalResolved(decision));

    Ok((
        StatusCode::ACCEPTED,
        Json(AcceptedResponse { accepted: true }),
    ))
}

async fn ws_handler(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| websocket_stream(state.event_hub, socket))
}

async fn websocket_stream(event_hub: EventHub, socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();
    tokio::spawn(async move { while receiver.next().await.is_some() {} });

    let mut subscriber = event_hub.subscribe().await;
    while let Ok(envelope) = subscriber.recv().await {
        let Ok(serialized) = serde_json::to_string(&envelope) else {
            continue;
        };
        if sender.send(Message::Text(serialized)).await.is_err() {
            break;
        }
    }
}

async fn require_bearer_auth(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Result<Response, ApiError> {
    let token = bearer_token_from_headers(request.headers()).ok_or(ApiError::Unauthorized)?;
    let device_id = authenticate_token(&state, token).await?;
    request
        .extensions_mut()
        .insert(AuthenticatedDevice { device_id });

    Ok(next.run(request).await)
}

async fn require_websocket_auth(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, ApiError> {
    let token = bearer_token_from_headers(request.headers())
        .or_else(|| token_from_query(request.uri().query().unwrap_or_default()))
        .ok_or(ApiError::Unauthorized)?;
    authenticate_token(&state, token).await?;

    Ok(next.run(request).await)
}

async fn authenticate_token(state: &AppState, token: &str) -> Result<String, ApiError> {
    let pairing = state.pairing.lock().await;
    pairing
        .validate_session_token(token)
        .map_err(|_| ApiError::Unauthorized)
}

fn token_from_query(query: &str) -> Option<&str> {
    query.split('&').find_map(|param| {
        param
            .strip_prefix("token=")
            .filter(|token| !token.is_empty())
    })
}

fn require_control_token(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let token = bearer_token_from_headers(headers)
        .or_else(|| control_token_from_headers(headers))
        .ok_or(ApiError::Unauthorized)?;

    if token == state.control_token.as_ref() {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

fn bearer_token_from_headers(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .filter(|token| !token.is_empty())
}

fn control_token_from_headers(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(BRIDGE_CONTROL_TOKEN_HEADER)?
        .to_str()
        .ok()
        .filter(|token| !token.is_empty())
}

fn refresh_failure_key(device_id: &str) -> String {
    device_id.trim().to_ascii_lowercase()
}

impl AppState {
    async fn record_refresh_failure(&self, failure_key: String) {
        let mut refresh_failures = self.refresh_failures.lock().await;
        let count = refresh_failures.entry(failure_key).or_default();
        *count = count.saturating_add(1);
    }

    async fn clear_refresh_failures(&self, failure_key: &str) {
        self.refresh_failures.lock().await.remove(failure_key);
    }
}

fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time is after unix epoch")
        .as_millis() as u64
}

#[derive(Debug)]
enum ApiError {
    Unauthorized,
    Forbidden(&'static str),
    NotFound(&'static str),
    UnsupportedMediaType(&'static str),
    Pairing(PairingError),
    Adapter(CodexRpcError),
}

impl From<PairingError> for ApiError {
    fn from(error: PairingError) -> Self {
        Self::Pairing(error)
    }
}

impl From<CodexRpcError> for ApiError {
    fn from(error: CodexRpcError) -> Self {
        Self::Adapter(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_string()),
            Self::Forbidden(message) => (StatusCode::FORBIDDEN, message.to_string()),
            Self::NotFound(message) => (StatusCode::NOT_FOUND, message.to_string()),
            Self::UnsupportedMediaType(message) => {
                (StatusCode::UNSUPPORTED_MEDIA_TYPE, message.to_string())
            }
            Self::Pairing(PairingError::InvalidToken | PairingError::TokenAlreadyUsed) => {
                (StatusCode::BAD_REQUEST, "invalid pairing token".to_string())
            }
            Self::Pairing(PairingError::ExpiredToken) => {
                (StatusCode::BAD_REQUEST, "expired token".to_string())
            }
            Self::Pairing(PairingError::DeviceRevoked) => {
                (StatusCode::FORBIDDEN, "device revoked".to_string())
            }
            Self::Pairing(PairingError::DeviceNotFound) => {
                (StatusCode::NOT_FOUND, "device not found".to_string())
            }
            Self::Adapter(error) => (StatusCode::BAD_GATEWAY, error.to_string()),
        };

        (status, Json(ErrorResponse { error: message })).into_response()
    }
}

pub async fn serve(addr: SocketAddr, state: AppState) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, build_router(state)).await?;
    Ok(())
}

pub async fn serve_with_static_dir(
    addr: SocketAddr,
    state: AppState,
    static_dir: impl Into<PathBuf>,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, build_router_with_static_dir(state, static_dir)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::{
        body::{Body, to_bytes},
        http::{Method, Request, StatusCode, header},
    };
    use serde_json::{Value, json};
    use std::{
        collections::HashMap as StdHashMap,
        path::PathBuf,
        sync::{Arc, Mutex as StdMutex},
    };
    use tempfile::{TempDir, tempdir};
    use tower::ServiceExt;

    use crate::{
        codex_rpc::{CodexAdapter, CodexRpcError, CodexThread, CodexTurn},
        pairing::PairingManager,
        protocol::{ApprovalDecision, SessionSnapshot, SessionStatus},
        storage::Storage,
    };

    const TEST_CONTROL_TOKEN: &str = "test-control-token";

    fn temp_storage() -> (TempDir, Storage) {
        let dir = tempdir().expect("tempdir is created");
        let path: PathBuf = dir.path().join("bridge.sqlite");
        let storage = Storage::open(path).expect("storage opens");

        (dir, storage)
    }

    fn temp_db_path() -> (TempDir, PathBuf) {
        let dir = tempdir().expect("tempdir is created");
        let path = dir.path().join("bridge.sqlite");

        (dir, path)
    }

    fn test_state() -> (TempDir, AppState) {
        let (dir, storage) = temp_storage();
        let pairing = PairingManager::with_clock(storage, || 1_725_000_000_000);
        let state = AppState::new(pairing, EventHub::new(), TEST_CONTROL_TOKEN);

        (dir, state)
    }

    async fn response_json(response: Response) -> Value {
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body reads");
        serde_json::from_slice(&body).expect("response body is json")
    }

    async fn pair_device(state: &AppState) -> String {
        let mut pairing = state.pairing.lock().await;
        let token = pairing.create_token().expect("pairing token creates");
        let registration = pairing
            .register_device(&token, "phone-1", "Damon's phone", "phone-secret")
            .expect("device registers");

        registration.session_token
    }

    fn request(method: Method, uri: &str, body: impl Into<Body>) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .body(body.into())
            .expect("request builds")
    }

    fn json_request(method: Method, uri: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("request builds")
    }

    #[tokio::test]
    async fn health_returns_connection_state() {
        let (_dir, state) = test_state();
        let app = build_router(state);

        let response = app
            .oneshot(request(Method::GET, "/api/health", Body::empty()))
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_json(response).await,
            json!({
                "status": "degraded",
                "connectionState": "codex_not_running",
            })
        );
    }

    #[tokio::test]
    async fn unpaired_request_cannot_read_sessions() {
        let (_dir, state) = test_state();
        let app = build_router(state);

        let response = app
            .oneshot(request(Method::GET, "/api/sessions", Body::empty()))
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn pairing_start_requires_control_token() {
        let (_dir, state) = test_state();
        let app = build_router(state);

        for request in [
            request(Method::POST, "/api/pairing/start", Body::empty()),
            Request::builder()
                .method(Method::POST)
                .uri("/api/pairing/start")
                .header(header::AUTHORIZATION, "Bearer wrong-control-token")
                .body(Body::empty())
                .expect("request builds"),
        ] {
            let response = app
                .clone()
                .oneshot(request)
                .await
                .expect("request succeeds");

            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn pairing_start_accepts_control_token() {
        let (_dir, state) = test_state();
        let app = build_router(state);

        for request in [
            Request::builder()
                .method(Method::POST)
                .uri("/api/pairing/start")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {TEST_CONTROL_TOKEN}"),
                )
                .body(Body::empty())
                .expect("request builds"),
            Request::builder()
                .method(Method::POST)
                .uri("/api/pairing/start")
                .header(BRIDGE_CONTROL_TOKEN_HEADER, TEST_CONTROL_TOKEN)
                .body(Body::empty())
                .expect("request builds"),
        ] {
            let response = app
                .clone()
                .oneshot(request)
                .await
                .expect("request succeeds");

            assert_eq!(response.status(), StatusCode::OK);
            let body = response_json(response).await;
            assert!(body["pairingToken"].as_str().is_some());
            assert_eq!(body["expiresInMs"], json!(DEFAULT_PAIRING_TOKEN_TTL_MS));
        }
    }

    #[tokio::test]
    async fn session_refresh_renews_after_pairing_manager_restart() {
        let (_dir, path) = temp_db_path();
        {
            let storage = Storage::open(&path).expect("storage opens");
            let mut pairing = PairingManager::with_clock(storage, || 1_725_000_000_000);
            let token = pairing.create_token().expect("pairing token creates");
            pairing
                .register_device(&token, "phone-1", "Damon's phone", "phone-secret")
                .expect("device registers");
        }
        let storage = Storage::open(path).expect("storage reopens");
        let state = AppState::new(
            PairingManager::with_clock(storage, || 1_725_000_100_000),
            EventHub::new(),
            TEST_CONTROL_TOKEN,
        );
        let app = build_router(state);

        let response = app
            .oneshot(json_request(
                Method::POST,
                "/api/session/refresh",
                json!({
                    "deviceId": "phone-1",
                    "deviceSecret": "phone-secret",
                }),
            ))
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["deviceId"], json!("phone-1"));
        assert!(body["sessionToken"].as_str().is_some());
        assert_eq!(body["sessionExpiresAt"], json!(1_725_086_500_000u64));
    }

    #[tokio::test]
    async fn session_refresh_rejects_wrong_secret() {
        let (_dir, state) = test_state();
        pair_device(&state).await;
        let app = build_router(state);

        let response = app
            .oneshot(json_request(
                Method::POST,
                "/api/session/refresh",
                json!({
                    "deviceId": "phone-1",
                    "deviceSecret": "wrong-secret",
                }),
            ))
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response_json(response).await,
            json!({ "error": "unauthorized" })
        );
    }

    #[tokio::test]
    async fn session_refresh_unknown_device_and_wrong_secret_have_same_response() {
        let (_dir, state) = test_state();
        pair_device(&state).await;
        let app = build_router(state);

        let wrong_secret_response = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/api/session/refresh",
                json!({
                    "deviceId": "phone-1",
                    "deviceSecret": "wrong-secret",
                }),
            ))
            .await
            .expect("request succeeds");
        let unknown_device_response = app
            .oneshot(json_request(
                Method::POST,
                "/api/session/refresh",
                json!({
                    "deviceId": "missing-phone",
                    "deviceSecret": "wrong-secret",
                }),
            ))
            .await
            .expect("request succeeds");

        assert_eq!(wrong_secret_response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(unknown_device_response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response_json(wrong_secret_response).await,
            response_json(unknown_device_response).await
        );
    }

    #[tokio::test]
    async fn session_refresh_allows_valid_secret_after_repeated_wrong_secret_attempts() {
        let (_dir, state) = test_state();
        pair_device(&state).await;
        let app = build_router(state.clone());

        for attempt in 1..=MAX_REFRESH_FAILURES_PER_DEVICE {
            let response = app
                .clone()
                .oneshot(json_request(
                    Method::POST,
                    "/api/session/refresh",
                    json!({
                        "deviceId": "PHONE-1 ",
                        "deviceSecret": "wrong-secret",
                    }),
                ))
                .await
                .expect("request succeeds");

            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "attempt {attempt}"
            );
            let body = response_json(response).await;
            assert_eq!(body, json!({ "error": "unauthorized" }));
            assert!(body.get("sessionToken").is_none());
        }

        assert_eq!(
            state
                .refresh_failures
                .lock()
                .await
                .get(&refresh_failure_key("phone-1"))
                .copied(),
            Some(MAX_REFRESH_FAILURES_PER_DEVICE)
        );

        let valid_secret_response = app
            .oneshot(json_request(
                Method::POST,
                "/api/session/refresh",
                json!({
                    "deviceId": "phone-1",
                    "deviceSecret": "phone-secret",
                }),
            ))
            .await
            .expect("request succeeds");

        assert_eq!(valid_secret_response.status(), StatusCode::OK);
        let body = response_json(valid_secret_response).await;
        assert_eq!(body["deviceId"], json!("phone-1"));
        assert!(body["sessionToken"].as_str().is_some());
        assert_eq!(
            state
                .refresh_failures
                .lock()
                .await
                .get(&refresh_failure_key("phone-1"))
                .copied(),
            None
        );
    }

    #[tokio::test]
    async fn protected_rest_routes_reject_missing_and_invalid_tokens() {
        struct ProtectedRoute {
            method: Method,
            uri: &'static str,
            body: Option<Value>,
        }

        let routes = [
            ProtectedRoute {
                method: Method::GET,
                uri: "/api/devices",
                body: None,
            },
            ProtectedRoute {
                method: Method::DELETE,
                uri: "/api/devices/phone-1",
                body: None,
            },
            ProtectedRoute {
                method: Method::GET,
                uri: "/api/sessions",
                body: None,
            },
            ProtectedRoute {
                method: Method::GET,
                uri: "/api/sessions/thread-1/events",
                body: None,
            },
            ProtectedRoute {
                method: Method::POST,
                uri: "/api/sessions/thread-1/messages",
                body: Some(json!({ "text": "hello" })),
            },
            ProtectedRoute {
                method: Method::POST,
                uri: "/api/approvals/approval-1/decision",
                body: Some(json!({ "decision": "approve", "comment": null })),
            },
        ];

        for route in routes {
            let (_dir, state) = test_state();
            let app = build_router(state);
            let missing_token_request = match &route.body {
                Some(body) => json_request(route.method.clone(), route.uri, body.clone()),
                None => request(route.method.clone(), route.uri, Body::empty()),
            };

            let response = app
                .clone()
                .oneshot(missing_token_request)
                .await
                .expect("request succeeds");
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{} without token",
                route.uri
            );

            let invalid_token_request = match &route.body {
                Some(body) => Request::builder()
                    .method(route.method.clone())
                    .uri(route.uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, "Bearer invalid-session-token")
                    .body(Body::from(body.to_string()))
                    .expect("request builds"),
                None => Request::builder()
                    .method(route.method.clone())
                    .uri(route.uri)
                    .header(header::AUTHORIZATION, "Bearer invalid-session-token")
                    .body(Body::empty())
                    .expect("request builds"),
            };

            let response = app
                .oneshot(invalid_token_request)
                .await
                .expect("request succeeds");
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{} with invalid token",
                route.uri
            );
        }
    }

    #[tokio::test]
    async fn approval_decision_routes_to_adapter() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let adapter = Arc::new(RecordingAdapter::default());
        let decisions = adapter.decisions();
        let app = build_router(state.with_codex_adapter(adapter));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/approvals/approval-1/decision")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "decision": "approve", "comment": "ship it" }).to_string(),
                    ))
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let decisions = decisions.lock().expect("decisions lock");
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].approval_id, "approval-1");
        assert_eq!(decisions[0].device_id, "phone-1");
        assert_eq!(decisions[0].decision, DecisionKind::Approve);
        assert_eq!(decisions[0].comment.as_deref(), Some("ship it"));
    }

    #[tokio::test]
    async fn websocket_route_rejects_missing_and_invalid_tokens_before_upgrade() {
        let (_dir, state) = test_state();
        let app = build_router(state);

        for uri in ["/ws", "/ws?token=invalid-session-token"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::GET)
                        .uri(uri)
                        .header(header::CONNECTION, "upgrade")
                        .header(header::UPGRADE, "websocket")
                        .header(header::SEC_WEBSOCKET_VERSION, "13")
                        .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
                        .body(Body::empty())
                        .expect("request builds"),
                )
                .await
                .expect("request succeeds");

            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn paired_device_can_list_snapshots() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        state
            .event_hub()
            .set_snapshot(SessionSnapshot {
                thread_id: "thread-1".to_string(),
                title: "Mobile bridge".to_string(),
                cwd: Some("/tmp/codex-app".to_string()),
                model_provider: Some("openai".to_string()),
                preview: Some("Latest response".to_string()),
                updated_at: 1_725_000_000_100,
                status: SessionStatus::Idle,
                pending_approval_ids: Vec::new(),
            })
            .await;
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/sessions")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_json(response).await,
            json!([
                {
                    "threadId": "thread-1",
                    "title": "Mobile bridge",
                    "cwd": "/tmp/codex-app",
                    "modelProvider": "openai",
                    "preview": "Latest response",
                    "updatedAt": 1725000000100u64,
                    "status": "idle",
                    "pendingApprovalIds": [],
                }
            ])
        );
    }

    #[tokio::test]
    async fn paired_device_can_list_live_codex_threads() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let adapter = Arc::new(RecordingAdapter::with_threads(vec![CodexThread {
            id: "thread-live".to_string(),
            title: Some("Live Codex thread".to_string()),
            cwd: Some("/repo".to_string()),
            model_provider: Some("openai".to_string()),
            preview: Some("Latest live message".to_string()),
            created_at: None,
            updated_at: Some(1_725_000_000_200),
            raw: json!({ "id": "thread-live", "status": "running" }),
        }]));
        let app = build_router(state.with_codex_adapter(adapter));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/sessions")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_json(response).await,
            json!([
                {
                    "threadId": "thread-live",
                    "title": "Live Codex thread",
                    "cwd": "/repo",
                    "modelProvider": "openai",
                    "preview": "Latest live message",
                    "updatedAt": 1725000000200u64,
                    "status": "running",
                    "pendingApprovalIds": [],
                }
            ])
        );
    }

    #[tokio::test]
    async fn paired_device_receives_scrubbed_image_asset_url_and_can_fetch_image() {
        let (dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let image_path = dir.path().join("codex-clipboard.png");
        tokio::fs::write(&image_path, b"png-bytes")
            .await
            .expect("image bytes write");
        let image_text = format!("look at this: {}", image_path.to_string_lossy());
        let adapter = Arc::new(RecordingAdapter::with_turns(
            "thread-image",
            vec![CodexTurn {
                id: Some("turn-image".to_string()),
                thread_id: Some("thread-image".to_string()),
                created_at: Some(1_725_000_000_000),
                updated_at: None,
                raw: json!({
                    "items": [
                        {
                            "id": "item-1",
                            "type": "userMessage",
                            "content": [
                                { "type": "input_text", "text": image_text },
                                {
                                    "type": "localImage",
                                    "path": image_path.to_string_lossy(),
                                    "detail": null
                                }
                            ]
                        }
                    ]
                }),
            }],
        ));
        let app = build_router(state.with_codex_adapter(adapter));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/sessions/thread-image/events")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        let serialized_body =
            serde_json::to_string(&body).expect("response body serializes to json");
        assert!(!serialized_body.contains(image_path.to_string_lossy().as_ref()));
        let attachment = &body[0]["payload"]["attachments"][0];
        assert_eq!(
            body[0]["payload"]["text"],
            json!("look at this: codex-clipboard.png")
        );
        assert_eq!(attachment["type"], json!("image"));
        assert_eq!(attachment["name"], json!("codex-clipboard.png"));
        assert!(attachment.get("path").is_none());
        let src = attachment["src"].as_str().expect("asset src is present");
        assert!(src.starts_with("/api/assets/local-image/"));

        let asset_response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(src)
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(asset_response.status(), StatusCode::OK);
        assert_eq!(
            asset_response.headers().get(header::CONTENT_TYPE),
            Some(&header::HeaderValue::from_static("image/png"))
        );
        let bytes = to_bytes(asset_response.into_body(), usize::MAX)
            .await
            .expect("asset response body reads");
        assert_eq!(&bytes[..], b"png-bytes");
    }

    #[tokio::test]
    async fn local_image_asset_route_rejects_missing_token() {
        let (_dir, state) = test_state();
        let app = build_router(state);

        let response = app
            .oneshot(request(
                Method::GET,
                "/api/assets/local-image/missing",
                Body::empty(),
            ))
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn unregistered_local_image_asset_returns_not_found() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/assets/local-image/missing")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn permission_denied_local_image_asset_returns_forbidden() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let asset_path = dir.path().join("unreadable.png");
        tokio::fs::write(&asset_path, b"png-bytes")
            .await
            .expect("asset bytes write");
        let mut permissions = tokio::fs::metadata(&asset_path)
            .await
            .expect("asset metadata reads")
            .permissions();
        permissions.set_mode(0o000);
        tokio::fs::set_permissions(&asset_path, permissions)
            .await
            .expect("asset permissions update");
        let adapter = Arc::new(RecordingAdapter::with_turns(
            "thread-image",
            vec![CodexTurn {
                id: Some("turn-image".to_string()),
                thread_id: Some("thread-image".to_string()),
                created_at: Some(1_725_000_000_000),
                updated_at: None,
                raw: json!({
                    "items": [
                        {
                            "id": "item-1",
                            "type": "userMessage",
                            "content": [
                                {
                                    "type": "localImage",
                                    "path": asset_path.to_string_lossy(),
                                    "detail": null
                                }
                            ]
                        }
                    ]
                }),
            }],
        ));
        let app = build_router(state.with_codex_adapter(adapter));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/sessions/thread-image/events")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        let src = body[0]["payload"]["attachments"][0]["src"]
            .as_str()
            .expect("asset src is present");

        let asset_response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(src)
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(asset_response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn non_image_local_asset_returns_unsupported_media_type() {
        let (dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let asset_path = dir.path().join("not-image.txt");
        tokio::fs::write(&asset_path, b"text-bytes")
            .await
            .expect("asset bytes write");
        let adapter = Arc::new(RecordingAdapter::with_turns(
            "thread-image",
            vec![CodexTurn {
                id: Some("turn-image".to_string()),
                thread_id: Some("thread-image".to_string()),
                created_at: Some(1_725_000_000_000),
                updated_at: None,
                raw: json!({
                    "items": [
                        {
                            "id": "item-1",
                            "type": "userMessage",
                            "content": [
                                {
                                    "type": "localImage",
                                    "path": asset_path.to_string_lossy(),
                                    "detail": null
                                }
                            ]
                        }
                    ]
                }),
            }],
        ));
        let app = build_router(state.with_codex_adapter(adapter));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/sessions/thread-image/events")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        let src = body[0]["payload"]["attachments"][0]["src"]
            .as_str()
            .expect("asset src is present");

        let asset_response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(src)
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(asset_response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn paired_device_can_read_message_events_published_through_api() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let app = build_router(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/sessions/thread-1/messages")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "text": "hello" }).to_string()))
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/sessions/thread-1/events")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body[0]["threadId"], json!("thread-1"));
        assert_eq!(body[0]["type"], json!("message"));
        assert_eq!(body[0]["payload"]["text"], json!("hello"));
    }

    #[tokio::test]
    async fn paired_device_send_message_routes_to_codex_adapter() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let adapter = Arc::new(RecordingAdapter::default());
        let messages = adapter.messages();
        let app = build_router(state.with_codex_adapter(adapter));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/sessions/thread-1/messages")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "text": "hello Codex" }).to_string()))
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(
            messages.lock().expect("messages lock").as_slice(),
            &[("thread-1".to_string(), "hello Codex".to_string())]
        );
    }

    #[tokio::test]
    async fn app_state_publish_session_event_records_history_and_broadcasts() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let mut subscriber = state.event_hub().subscribe().await;
        let event = SessionEvent {
            id: "event-1".to_string(),
            thread_id: "thread-1".to_string(),
            event_type: SessionEventType::Message,
            payload: json!({ "role": "assistant", "text": "published" }),
            created_at: 1_725_000_000_100,
        };

        let receivers = state.publish_session_event(event.clone()).await;

        assert_eq!(receivers, 1);
        assert_eq!(
            subscriber.recv().await.expect("subscriber receives event"),
            ServerEnvelope::SessionEvent(event)
        );

        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/sessions/thread-1/events")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body[0]["id"], json!("event-1"));
        assert_eq!(body[0]["payload"]["text"], json!("published"));
    }

    #[derive(Default)]
    struct RecordingAdapter {
        decisions: Arc<StdMutex<Vec<ApprovalDecision>>>,
        messages: Arc<StdMutex<Vec<(String, String)>>>,
        threads: Arc<StdMutex<Vec<CodexThread>>>,
        turns: Arc<StdMutex<StdHashMap<String, Vec<CodexTurn>>>>,
    }

    impl RecordingAdapter {
        fn with_threads(threads: Vec<CodexThread>) -> Self {
            Self {
                threads: Arc::new(StdMutex::new(threads)),
                ..Self::default()
            }
        }

        fn with_turns(thread_id: impl Into<String>, turns: Vec<CodexTurn>) -> Self {
            let mut turns_by_thread = StdHashMap::new();
            turns_by_thread.insert(thread_id.into(), turns);
            Self {
                turns: Arc::new(StdMutex::new(turns_by_thread)),
                ..Self::default()
            }
        }

        fn decisions(&self) -> Arc<StdMutex<Vec<ApprovalDecision>>> {
            self.decisions.clone()
        }

        fn messages(&self) -> Arc<StdMutex<Vec<(String, String)>>> {
            self.messages.clone()
        }
    }

    #[async_trait]
    impl CodexAdapter for RecordingAdapter {
        async fn list_threads(&self) -> Result<Vec<CodexThread>, CodexRpcError> {
            Ok(self.threads.lock().expect("threads lock").clone())
        }

        async fn resume_thread(
            &self,
            _thread_id: &str,
        ) -> Result<Option<CodexThread>, CodexRpcError> {
            Ok(None)
        }

        async fn list_turns(&self, thread_id: &str) -> Result<Vec<CodexTurn>, CodexRpcError> {
            Ok(self
                .turns
                .lock()
                .expect("turns lock")
                .get(thread_id)
                .cloned()
                .unwrap_or_default())
        }

        async fn send_user_message(
            &self,
            thread_id: &str,
            text: &str,
        ) -> Result<(), CodexRpcError> {
            self.messages
                .lock()
                .expect("messages lock")
                .push((thread_id.to_string(), text.to_string()));
            Ok(())
        }

        async fn subscribe_events(&self, _thread_id: Option<&str>) -> Result<(), CodexRpcError> {
            Ok(())
        }

        async fn respond_approval(
            &self,
            _approval_id: &str,
            decision: &ApprovalDecision,
        ) -> Result<(), CodexRpcError> {
            self.decisions
                .lock()
                .expect("decisions lock")
                .push(decision.clone());
            Ok(())
        }
    }
}
