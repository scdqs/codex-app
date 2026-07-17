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
        DefaultBodyLimit, Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Mutex, Notify, RwLock};
use tower_http::{
    compression::CompressionLayer,
    services::{ServeDir, ServeFile},
};
use uuid::Uuid;

use crate::{
    approval::ApprovalDetector,
    codex_rpc::{CodexAdapter, CodexRpcError, UserImageAttachment},
    diagnostics::DiagnosticsReport,
    event_hub::EventHub,
    local_assets::LocalAssetRegistry,
    normalizer::Normalizer,
    pairing::{DEFAULT_PAIRING_TOKEN_TTL_MS, PairingError, PairingManager},
    protocol::{
        ApprovalDecision, ApprovalKind, ApprovalRequest, DecisionKind, ServerEnvelope,
        SessionEvent, SessionEventType, SessionSnapshot, SessionStatus,
    },
};

#[derive(Clone)]
pub struct AppState {
    pairing: Arc<Mutex<PairingManager>>,
    event_hub: EventHub,
    event_history: Arc<Mutex<HashMap<String, VecDeque<SessionEvent>>>>,
    adapter_event_cache: Arc<Mutex<HashMap<String, AdapterEventCache>>>,
    pending_approvals: Arc<Mutex<HashMap<String, ApprovalRequest>>>,
    message_dedupe: Arc<Mutex<MessageDedupeCache>>,
    refresh_failures: Arc<Mutex<HashMap<String, usize>>>,
    local_assets: Arc<Mutex<LocalAssetRegistry>>,
    control_token: Arc<str>,
    codex_adapter: Option<Arc<dyn CodexAdapter>>,
    diagnostics: Arc<RwLock<DiagnosticsReport>>,
}

#[derive(Debug, Default)]
struct AdapterEventCache {
    events: Vec<SessionEvent>,
    next_cursor: Option<String>,
    loaded_older: bool,
}

#[derive(Debug, Default)]
struct MessageDedupeCache {
    entries: HashMap<String, MessageDedupeEntry>,
    completed_order: VecDeque<String>,
}

#[derive(Debug)]
enum MessageDedupeEntry {
    InFlight(Arc<Notify>),
    Completed,
}

enum MessageDedupeClaim {
    Owner { key: String, notify: Arc<Notify> },
    Completed,
}

const EVENT_HISTORY_LIMIT_PER_THREAD: usize = 256;
const MAX_ADAPTER_HISTORY_PAGES_PER_REQUEST: usize = 4;
const DEFAULT_EVENT_PAGE_LIMIT: usize = 50;
const MAX_EVENT_PAGE_LIMIT: usize = 100;
const MAX_UPLOAD_IMAGE_ATTACHMENTS: usize = 4;
const MAX_UPLOAD_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_AUTHENTICATED_JSON_BODY_BYTES: usize = 48 * 1024 * 1024;
const MAX_MESSAGE_DEDUPE_ENTRIES: usize = 1_024;
#[cfg(test)]
const MAX_REFRESH_FAILURES_PER_DEVICE: usize = 5;
const BRIDGE_CONTROL_TOKEN_HEADER: &str = "x-bridge-control-token";
const EVENT_LIMIT_HEADER: &str = "x-codex-events-limit";
const EVENT_BEFORE_HEADER: &str = "x-codex-events-before";
const EVENT_SINCE_HEADER: &str = "x-codex-events-since";
const CLIENT_MESSAGE_ID_HEADER: &str = "x-codex-client-message-id";

#[derive(Debug, Clone)]
struct AuthenticatedDevice {
    device_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    status: String,
    connection_state: String,
    version: &'static str,
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
    #[serde(default)]
    attachments: Vec<IncomingImageAttachment>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionRequest {
    text: String,
    #[serde(default)]
    attachments: Vec<IncomingImageAttachment>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionEventsQuery {
    limit: Option<usize>,
    before: Option<String>,
    since: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionEventsPage {
    events: Vec<SessionEvent>,
    before_cursor: Option<String>,
    after_cursor: Option<String>,
    has_more_before: bool,
    has_more_after: bool,
    reset: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomingImageAttachment {
    name: String,
    mime_type: String,
    data_base64: String,
}

#[derive(Debug)]
struct StoredImageAttachment {
    name: String,
    path: PathBuf,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DevApprovalRequest {
    thread_id: Option<String>,
    kind: Option<ApprovalKind>,
    title: Option<String>,
    detail: Option<String>,
    risk_hint: Option<String>,
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
            adapter_event_cache: Arc::new(Mutex::new(HashMap::new())),
            pending_approvals: Arc::new(Mutex::new(HashMap::new())),
            message_dedupe: Arc::new(Mutex::new(MessageDedupeCache::default())),
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

    pub fn with_local_asset_registry(self, registry: LocalAssetRegistry) -> Self {
        Self {
            local_assets: Arc::new(Mutex::new(registry)),
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

    async fn replace_session_event_history(&self, thread_id: &str, events: &[SessionEvent]) {
        let start = events.len().saturating_sub(EVENT_HISTORY_LIMIT_PER_THREAD);
        self.event_history.lock().await.insert(
            thread_id.to_string(),
            events[start..].iter().cloned().collect(),
        );
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

        for attachment in attachments {
            let Some(path) = local_image_attachment_path(attachment) else {
                continue;
            };
            let token = self.local_assets.lock().await.register_image(path);

            if let Some(object) = attachment.as_object_mut() {
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
    phone_routes(state.clone())
        .merge(control_routes(state.clone()))
        .with_state(state)
}

pub fn build_phone_router(state: AppState) -> Router {
    phone_routes(state.clone()).with_state(state)
}

pub fn build_control_router(state: AppState) -> Router {
    control_routes(state.clone()).with_state(state)
}

fn phone_routes(state: AppState) -> Router<AppState> {
    let authenticated_phone_routes = Router::new()
        .route(
            "/api/assets/local-image/:asset_token",
            get(get_local_image_asset),
        )
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route("/api/sessions/:thread_id/events", get(list_session_events))
        .route("/api/sessions/:thread_id/messages", post(send_message))
        .route("/api/approvals", get(list_approvals))
        .route(
            "/api/approvals/:approval_id/decision",
            post(decide_approval),
        )
        .layer(DefaultBodyLimit::max(MAX_AUTHENTICATED_JSON_BODY_BYTES))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer_auth,
        ))
        .layer(CompressionLayer::new());
    let websocket_route =
        Router::new()
            .route("/ws", get(ws_handler))
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_websocket_auth,
            ));

    Router::new()
        .route("/api/health", get(health))
        .route("/api/pairing/complete", post(complete_pairing))
        .route("/api/session/refresh", post(refresh_session))
        .merge(authenticated_phone_routes)
        .merge(websocket_route)
}

fn control_routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/api/control/pairing/start", post(start_pairing))
        .route("/api/control/diagnostics", get(control_diagnostics))
        .route("/api/control/devices", get(list_devices))
        .route("/api/control/devices/:id", delete(revoke_device))
        .route("/api/control/dev/approvals", post(trigger_dev_approval))
        .route_layer(middleware::from_fn_with_state(state, require_control_auth))
}

pub fn build_router_with_static_dir(state: AppState, static_dir: impl Into<PathBuf>) -> Router {
    let static_dir = static_dir.into();
    build_router(state).fallback_service(
        ServeDir::new(&static_dir).fallback(ServeFile::new(static_dir.join("index.html"))),
    )
}

pub fn build_phone_router_with_static_dir(
    state: AppState,
    static_dir: impl Into<PathBuf>,
) -> Router {
    let static_dir = static_dir.into();
    build_phone_router(state).fallback_service(
        ServeDir::new(&static_dir).fallback(ServeFile::new(static_dir.join("index.html"))),
    )
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let diagnostics = state.diagnostics.read().await;
    Json(HealthResponse {
        status: diagnostics.status.as_str().to_string(),
        connection_state: diagnostics.connection_state.as_str().to_string(),
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn start_pairing(
    State(state): State<AppState>,
) -> Result<Json<PairingStartResponse>, ApiError> {
    let mut pairing = state.pairing.lock().await;
    let pairing_token = pairing.create_token()?;

    Ok(Json(PairingStartResponse {
        pairing_token,
        expires_in_ms: DEFAULT_PAIRING_TOKEN_TTL_MS,
    }))
}

async fn control_diagnostics(State(state): State<AppState>) -> Json<DiagnosticsReport> {
    let diagnostics = state.diagnostics.read().await;
    Json(diagnostics.clone())
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

async fn list_approvals(
    State(state): State<AppState>,
) -> Result<Json<Vec<ApprovalRequest>>, ApiError> {
    let created_at = current_time_ms();
    let adapter_approvals = if let Some(adapter) = state.codex_adapter.as_ref() {
        adapter
            .list_pending_approvals()
            .await?
            .iter()
            .filter_map(|pending| ApprovalDetector::detect_pending(pending, created_at))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let mut pending_approvals = state.pending_approvals.lock().await;
    pending_approvals.retain(|approval_id, _| is_dev_approval_id(approval_id));
    for approval in adapter_approvals {
        pending_approvals.insert(approval.id.clone(), approval);
    }
    let mut approvals = pending_approvals.values().cloned().collect::<Vec<_>>();
    approvals.sort_by(|left, right| {
        left.thread_id
            .cmp(&right.thread_id)
            .then_with(|| left.id.cmp(&right.id))
    });

    Ok(Json(approvals))
}

async fn create_session(
    State(state): State<AppState>,
    Json(request): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<SessionSnapshot>), ApiError> {
    let text = request.text.trim().to_string();
    if text.is_empty() && request.attachments.is_empty() {
        return Err(ApiError::BadRequest(
            "session text or attachment is required",
        ));
    }
    let attachments = store_incoming_image_attachments(&request.attachments).await?;
    let adapter_attachments = codex_image_attachments(&attachments);
    let display_text = preview_text(&text, &attachments);

    let now = current_time_ms();
    let thread = if let Some(adapter) = state.codex_adapter.as_ref() {
        adapter.start_thread(&text, &adapter_attachments).await?
    } else {
        local_thread_for_created_session(&display_text, now)
    };
    let mut snapshot = Normalizer::snapshot_from_thread(&thread);
    if snapshot.updated_at == 0 {
        snapshot.updated_at = now;
    }
    if snapshot.preview.as_deref().unwrap_or_default().is_empty() {
        snapshot.preview = Some(display_text.clone());
    }
    if snapshot.title == snapshot.thread_id || snapshot.title.trim().is_empty() {
        snapshot.title = session_title_from_text(&display_text);
    }
    snapshot.status = SessionStatus::Running;

    state.event_hub.set_snapshot(snapshot.clone()).await;
    let event = event_for_user_message(snapshot.thread_id.clone(), text, attachments, now, None);
    let event = state.register_local_assets_for_event(event).await;
    state.publish_session_event(event).await;

    Ok((StatusCode::CREATED, Json(snapshot)))
}

async fn list_session_events(
    State(state): State<AppState>,
    Path(thread_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<SessionEventsQuery>,
) -> Result<Response, ApiError> {
    let query = query.with_headers(&headers)?;
    if query.is_paginated() {
        query.validate()?;
    }
    let (events, adapter_has_more_older) = if let Some(adapter) = state.codex_adapter.as_ref() {
        if query.is_paginated() {
            adapter_events_for_query(&state, adapter, &thread_id, &query).await?
        } else {
            let turns = adapter.list_turns(&thread_id).await?;
            let events = Normalizer::events_from_turns(&thread_id, &turns);
            state
                .replace_session_event_history(&thread_id, &events)
                .await;
            (events, false)
        }
    } else {
        (
            state
                .event_history
                .lock()
                .await
                .get(&thread_id)
                .map(|events| events.iter().cloned().collect())
                .unwrap_or_default(),
            false,
        )
    };

    if !query.is_paginated() {
        let events = mobile_session_events(&state, events).await;
        return Ok(Json(events).into_response());
    }

    let mut page = paginate_session_events(&events, &query)?;
    if adapter_has_more_older
        && page.before_cursor.as_deref() == events.first().map(|event| event.id.as_str())
    {
        page.has_more_before = true;
    }
    page.events = mobile_session_events(&state, page.events).await;
    Ok(Json(page).into_response())
}

async fn adapter_events_for_query(
    state: &AppState,
    adapter: &Arc<dyn CodexAdapter>,
    thread_id: &str,
    query: &SessionEventsQuery,
) -> Result<(Vec<SessionEvent>, bool), ApiError> {
    let first_page = adapter.list_turns_page(thread_id, None).await?;
    let first_page_turn_ids = first_page
        .turns
        .iter()
        .filter_map(|turn| turn.id.clone())
        .collect::<Vec<_>>();
    let first_page_events = Normalizer::events_from_turns(thread_id, &first_page.turns);
    let limit = query.page_limit()?;
    let mut caches = state.adapter_event_cache.lock().await;
    let cache = caches.entry(thread_id.to_string()).or_default();
    replace_adapter_turn_events(&mut cache.events, first_page_events, &first_page_turn_ids);
    if !cache.loaded_older {
        cache.next_cursor = first_page.next_cursor;
    }

    if let Some(before) = query.before.as_deref() {
        for _ in 0..MAX_ADAPTER_HISTORY_PAGES_PER_REQUEST {
            let Some(cursor_index) = cache.events.iter().position(|event| event.id == before)
            else {
                break;
            };
            if cursor_index >= limit {
                break;
            }
            let Some(cursor) = cache.next_cursor.clone() else {
                break;
            };
            let older_page = adapter.list_turns_page(thread_id, Some(&cursor)).await?;
            let older_events = Normalizer::events_from_turns(thread_id, &older_page.turns);
            let added = merge_adapter_events(&mut cache.events, older_events);
            cache.next_cursor = older_page.next_cursor;
            cache.loaded_older = true;
            if added == 0 && cache.next_cursor.as_deref() == Some(cursor.as_str()) {
                break;
            }
        }
    }

    let events = cache.events.clone();
    let has_more_older = cache.next_cursor.is_some();
    drop(caches);
    state
        .replace_session_event_history(thread_id, &events)
        .await;
    Ok((events, has_more_older))
}

fn replace_adapter_turn_events(
    existing: &mut Vec<SessionEvent>,
    incoming: Vec<SessionEvent>,
    turn_ids: &[String],
) {
    existing.retain(|event| {
        !turn_ids
            .iter()
            .any(|turn_id| event_belongs_to_turn(&event.id, turn_id))
    });
    merge_adapter_events(existing, incoming);
}

fn event_belongs_to_turn(event_id: &str, turn_id: &str) -> bool {
    event_id
        .strip_prefix(turn_id)
        .is_some_and(|suffix| suffix.starts_with(':'))
}

fn merge_adapter_events(existing: &mut Vec<SessionEvent>, incoming: Vec<SessionEvent>) -> usize {
    let mut added = 0;
    for event in incoming {
        if let Some(index) = existing.iter().position(|current| current.id == event.id) {
            existing[index] = event;
        } else {
            existing.push(event);
            added += 1;
        }
    }
    existing.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    added
}

async fn mobile_session_events(state: &AppState, events: Vec<SessionEvent>) -> Vec<SessionEvent> {
    let mut mobile_events = Vec::with_capacity(events.len());
    for event in events {
        let event = state.register_local_assets_for_event(event).await;
        mobile_events.push(session_event_for_mobile(event));
    }
    mobile_events
}

impl SessionEventsQuery {
    fn with_headers(mut self, headers: &HeaderMap) -> Result<Self, ApiError> {
        if self.limit.is_none() {
            self.limit = optional_event_header(headers, EVENT_LIMIT_HEADER)?
                .map(|value| {
                    value
                        .parse::<usize>()
                        .map_err(|_| ApiError::BadRequest("invalid event page limit"))
                })
                .transpose()?;
        }
        if self.before.is_none() {
            self.before = optional_event_header(headers, EVENT_BEFORE_HEADER)?;
        }
        if self.since.is_none() {
            self.since = optional_event_header(headers, EVENT_SINCE_HEADER)?;
        }
        Ok(self)
    }

    fn is_paginated(&self) -> bool {
        self.limit.is_some() || self.before.is_some() || self.since.is_some()
    }

    fn validate(&self) -> Result<(), ApiError> {
        if self.before.is_some() && self.since.is_some() {
            return Err(ApiError::BadRequest(
                "before and since cursors cannot be combined",
            ));
        }
        self.page_limit().map(|_| ())
    }

    fn page_limit(&self) -> Result<usize, ApiError> {
        let requested_limit = self.limit.unwrap_or(DEFAULT_EVENT_PAGE_LIMIT);
        if requested_limit == 0 {
            return Err(ApiError::BadRequest("event page limit must be positive"));
        }
        Ok(requested_limit.min(MAX_EVENT_PAGE_LIMIT))
    }
}

fn optional_event_header(
    headers: &HeaderMap,
    name: &'static str,
) -> Result<Option<String>, ApiError> {
    let Some(value) = headers.get(name) else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|_| ApiError::BadRequest("invalid event pagination header"))?
        .trim();
    if value.is_empty() {
        return Err(ApiError::BadRequest("event pagination header is empty"));
    }
    Ok(Some(value.to_string()))
}

fn paginate_session_events(
    events: &[SessionEvent],
    query: &SessionEventsQuery,
) -> Result<SessionEventsPage, ApiError> {
    query.validate()?;
    let limit = query.page_limit()?;
    let latest_window = || {
        let end = events.len();
        (end.saturating_sub(limit), end)
    };

    let mut reset = false;
    let (start, end) = if let Some(before) = query.before.as_deref() {
        if let Some(cursor_index) = events.iter().position(|event| event.id == before) {
            let end = cursor_index;
            (end.saturating_sub(limit), end)
        } else {
            reset = true;
            latest_window()
        }
    } else if let Some(since) = query.since.as_deref() {
        if let Some(cursor_index) = events.iter().rposition(|event| event.id == since) {
            let end = (cursor_index + limit.max(2)).min(events.len());
            (cursor_index, end)
        } else {
            reset = true;
            latest_window()
        }
    } else {
        latest_window()
    };

    let page_events = events[start..end].to_vec();
    Ok(SessionEventsPage {
        before_cursor: page_events.first().map(|event| event.id.clone()),
        after_cursor: page_events.last().map(|event| event.id.clone()),
        has_more_before: start > 0,
        has_more_after: end < events.len(),
        events: page_events,
        reset,
    })
}

fn session_event_for_mobile(mut event: SessionEvent) -> SessionEvent {
    if let Some(payload) = event.payload.as_object_mut() {
        payload.remove("raw");
    }
    event
}

async fn get_local_image_asset(
    State(state): State<AppState>,
    Path(asset_token): Path<String>,
) -> Result<Response, ApiError> {
    let path = {
        let mut local_assets = state.local_assets.lock().await;
        local_assets.path_for(&asset_token)
    }
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

fn local_thread_for_created_session(text: &str, now: u64) -> crate::codex_rpc::CodexThread {
    let id = format!("local-{}", Uuid::new_v4());
    crate::codex_rpc::CodexThread {
        id: id.clone(),
        title: Some(session_title_from_text(text)),
        cwd: None,
        model_provider: None,
        preview: Some(text.to_string()),
        created_at: Some(now),
        updated_at: Some(now),
        raw: json!({
            "id": id,
            "title": session_title_from_text(text),
            "preview": text,
            "updatedAt": now,
            "status": "running",
        }),
    }
}

fn session_title_from_text(text: &str) -> String {
    let title = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(text)
        .trim();
    if title.chars().count() <= 80 {
        return title.to_string();
    }

    let mut shortened = title.chars().take(80).collect::<String>();
    shortened.push_str("...");
    shortened
}

fn preview_text(text: &str, attachments: &[StoredImageAttachment]) -> String {
    if !text.trim().is_empty() {
        return text.to_string();
    }
    if attachments.len() == 1 {
        return format!("Image: {}", attachments[0].name);
    }
    format!("{} images from phone", attachments.len())
}

fn event_for_user_message(
    thread_id: String,
    text: String,
    attachments: Vec<StoredImageAttachment>,
    created_at: u64,
    client_message_id: Option<&str>,
) -> SessionEvent {
    let mut payload = json!({
        "role": "user",
        "text": text,
    });

    if !attachments.is_empty()
        && let Some(object) = payload.as_object_mut()
    {
        object.insert(
            "attachments".to_string(),
            json!(
                attachments
                    .into_iter()
                    .map(|attachment| {
                        json!({
                            "type": "image",
                            "path": attachment.path,
                            "name": attachment.name,
                        })
                    })
                    .collect::<Vec<_>>()
            ),
        );
    }

    if let Some(client_message_id) = client_message_id
        && let Some(object) = payload.as_object_mut()
    {
        object.insert("bridgeEcho".to_string(), json!(true));
        object.insert("clientMessageId".to_string(), json!(client_message_id));
    }

    SessionEvent {
        id: client_message_id
            .map(ToString::to_string)
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        thread_id,
        event_type: SessionEventType::Message,
        payload,
        created_at,
    }
}

fn codex_image_attachments(attachments: &[StoredImageAttachment]) -> Vec<UserImageAttachment> {
    attachments
        .iter()
        .map(|attachment| UserImageAttachment {
            path: attachment.path.to_string_lossy().into_owned(),
        })
        .collect()
}

async fn store_incoming_image_attachments(
    attachments: &[IncomingImageAttachment],
) -> Result<Vec<StoredImageAttachment>, ApiError> {
    if attachments.len() > MAX_UPLOAD_IMAGE_ATTACHMENTS {
        return Err(ApiError::BadRequest("too many image attachments"));
    }

    let mut stored = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        let bytes = decode_image_attachment_data(&attachment.data_base64)?;
        if bytes.is_empty() {
            return Err(ApiError::BadRequest("image attachment is empty"));
        }
        if bytes.len() > MAX_UPLOAD_IMAGE_BYTES {
            return Err(ApiError::BadRequest("image attachment is too large"));
        }

        let format = supported_image_format(&attachment.mime_type, &bytes)?;
        let dir = std::env::temp_dir()
            .join("codex-mobile-bridge")
            .join("uploads");
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|_| ApiError::Internal("failed to prepare image attachment storage"))?;
        let path = dir.join(format!("{}.{}", Uuid::new_v4(), format.extension));
        tokio::fs::write(&path, bytes)
            .await
            .map_err(|_| ApiError::Internal("failed to store image attachment"))?;

        stored.push(StoredImageAttachment {
            name: display_name_for_attachment(&attachment.name, format.extension),
            path,
        });
    }

    Ok(stored)
}

fn decode_image_attachment_data(data_base64: &str) -> Result<Vec<u8>, ApiError> {
    let encoded = data_base64
        .split_once(',')
        .map(|(_, data)| data)
        .unwrap_or(data_base64)
        .trim();
    BASE64_STANDARD
        .decode(encoded)
        .map_err(|_| ApiError::BadRequest("invalid image attachment data"))
}

#[derive(Debug, Clone, Copy)]
struct SupportedImageFormat {
    mime_type: &'static str,
    extension: &'static str,
}

fn supported_image_format(mime_type: &str, bytes: &[u8]) -> Result<SupportedImageFormat, ApiError> {
    let normalized = mime_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let declared = match normalized.as_str() {
        "image/png" => SupportedImageFormat {
            mime_type: "image/png",
            extension: "png",
        },
        "image/jpeg" | "image/jpg" => SupportedImageFormat {
            mime_type: "image/jpeg",
            extension: "jpg",
        },
        "image/gif" => SupportedImageFormat {
            mime_type: "image/gif",
            extension: "gif",
        },
        "image/webp" => SupportedImageFormat {
            mime_type: "image/webp",
            extension: "webp",
        },
        _ => {
            return Err(ApiError::UnsupportedMediaType(
                "unsupported image attachment type",
            ));
        }
    };

    if image_bytes_match(declared.mime_type, bytes) {
        Ok(declared)
    } else {
        Err(ApiError::UnsupportedMediaType(
            "image attachment bytes do not match declared type",
        ))
    }
}

fn image_bytes_match(mime_type: &str, bytes: &[u8]) -> bool {
    match mime_type {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        _ => false,
    }
}

fn display_name_for_attachment(name: &str, fallback_extension: &str) -> String {
    let trimmed = name.trim();
    let filename = FsPath::new(trimmed)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("image");
    if FsPath::new(filename).extension().is_some() {
        filename.to_string()
    } else {
        format!("{filename}.{fallback_extension}")
    }
}

async fn send_message(
    State(state): State<AppState>,
    Path(thread_id): Path<String>,
    Extension(device): Extension<AuthenticatedDevice>,
    headers: HeaderMap,
    Json(request): Json<SendMessageRequest>,
) -> Result<(StatusCode, Json<AcceptedResponse>), ApiError> {
    let text = request.text.trim().to_string();
    if text.is_empty() && request.attachments.is_empty() {
        return Err(ApiError::BadRequest(
            "message text or attachment is required",
        ));
    }
    let client_message_id = client_message_id_from_headers(&headers)?;
    let claim = if let Some(client_message_id) = client_message_id.as_deref() {
        claim_message_dedupe(
            &state.message_dedupe,
            message_dedupe_key(&device.device_id, &thread_id, client_message_id),
        )
        .await
    } else {
        None
    };
    match claim {
        Some(MessageDedupeClaim::Completed) => {}
        Some(MessageDedupeClaim::Owner { key, notify }) => {
            let task_state = state.clone();
            let task = tokio::spawn(async move {
                let result = deliver_user_message(
                    task_state.clone(),
                    thread_id,
                    text,
                    request.attachments,
                    client_message_id,
                )
                .await;
                if result.is_ok() {
                    complete_message_dedupe(&task_state.message_dedupe, key, &notify).await;
                } else {
                    fail_message_dedupe(&task_state.message_dedupe, &key, &notify).await;
                }
                result
            });
            task.await
                .map_err(|_| ApiError::Internal("message delivery task failed"))??;
        }
        None => {
            deliver_user_message(
                state,
                thread_id,
                text,
                request.attachments,
                client_message_id,
            )
            .await?;
        }
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(AcceptedResponse { accepted: true }),
    ))
}

async fn deliver_user_message(
    state: AppState,
    thread_id: String,
    text: String,
    incoming_attachments: Vec<IncomingImageAttachment>,
    client_message_id: Option<String>,
) -> Result<(), ApiError> {
    let attachments = store_incoming_image_attachments(&incoming_attachments).await?;
    let adapter_attachments = codex_image_attachments(&attachments);

    if let Some(adapter) = state.codex_adapter.as_ref() {
        adapter
            .send_user_message(&thread_id, &text, &adapter_attachments)
            .await?;
    }

    let event = event_for_user_message(
        thread_id,
        text,
        attachments,
        current_time_ms(),
        client_message_id.as_deref(),
    );
    let event = state.register_local_assets_for_event(event).await;
    state.publish_session_event(event).await;
    Ok(())
}

fn client_message_id_from_headers(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    let Some(value) = headers.get(CLIENT_MESSAGE_ID_HEADER) else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|_| ApiError::BadRequest("invalid client message id"))?
        .trim();
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(ApiError::BadRequest("invalid client message id"));
    }
    Ok(Some(value.to_string()))
}

fn message_dedupe_key(device_id: &str, thread_id: &str, client_message_id: &str) -> String {
    format!("{device_id}\n{thread_id}\n{client_message_id}")
}

async fn claim_message_dedupe(
    cache: &Mutex<MessageDedupeCache>,
    key: String,
) -> Option<MessageDedupeClaim> {
    loop {
        let waiter = {
            let mut cache = cache.lock().await;
            match cache.entries.get(&key) {
                Some(MessageDedupeEntry::Completed) => {
                    return Some(MessageDedupeClaim::Completed);
                }
                Some(MessageDedupeEntry::InFlight(notify)) => Some(notify.clone().notified_owned()),
                None => {
                    let notify = Arc::new(Notify::new());
                    cache
                        .entries
                        .insert(key.clone(), MessageDedupeEntry::InFlight(notify.clone()));
                    return Some(MessageDedupeClaim::Owner { key, notify });
                }
            }
        };
        if let Some(waiter) = waiter {
            waiter.await;
        }
    }
}

async fn complete_message_dedupe(cache: &Mutex<MessageDedupeCache>, key: String, notify: &Notify) {
    let mut cache = cache.lock().await;
    cache
        .entries
        .insert(key.clone(), MessageDedupeEntry::Completed);
    cache.completed_order.push_back(key);
    while cache.completed_order.len() > MAX_MESSAGE_DEDUPE_ENTRIES {
        if let Some(expired_key) = cache.completed_order.pop_front()
            && matches!(
                cache.entries.get(&expired_key),
                Some(MessageDedupeEntry::Completed)
            )
        {
            cache.entries.remove(&expired_key);
        }
    }
    drop(cache);
    notify.notify_waiters();
}

async fn fail_message_dedupe(cache: &Mutex<MessageDedupeCache>, key: &str, notify: &Notify) {
    cache.lock().await.entries.remove(key);
    notify.notify_waiters();
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

    if let Some(adapter) = state.codex_adapter.as_ref()
        && !is_dev_approval_id(&decision.approval_id)
    {
        adapter
            .respond_approval(&decision.approval_id, &decision)
            .await?;
    }

    state
        .pending_approvals
        .lock()
        .await
        .remove(&decision.approval_id);

    state
        .event_hub
        .publish(ServerEnvelope::ApprovalResolved(decision));

    Ok((
        StatusCode::ACCEPTED,
        Json(AcceptedResponse { accepted: true }),
    ))
}

async fn trigger_dev_approval(
    State(state): State<AppState>,
    Json(request): Json<DevApprovalRequest>,
) -> Result<(StatusCode, Json<ApprovalRequest>), ApiError> {
    if !cfg!(debug_assertions) {
        return Err(ApiError::NotFound("dev approval trigger not available"));
    }

    let now = current_time_ms();
    let thread_id = match request.thread_id {
        Some(thread_id) if !thread_id.trim().is_empty() => thread_id,
        _ => state
            .event_hub
            .all_snapshots()
            .await
            .into_iter()
            .next()
            .map(|snapshot| snapshot.thread_id)
            .unwrap_or_else(|| "dev-thread".to_string()),
    };
    let approval = ApprovalRequest {
        id: format!("dev-approval-{now}"),
        thread_id: thread_id.clone(),
        kind: request.kind.unwrap_or(ApprovalKind::Command),
        title: request
            .title
            .unwrap_or_else(|| "Dev approval smoke test".to_string()),
        detail: request
            .detail
            .unwrap_or_else(|| "echo approval smoke test".to_string()),
        risk_hint: request
            .risk_hint
            .or_else(|| Some("Debug-only fake approval from local bridge.".to_string())),
        raw: Some(json!({ "source": "dev_approval_trigger" })),
        created_at: now,
        expires_at: None,
    };
    state
        .pending_approvals
        .lock()
        .await
        .insert(approval.id.clone(), approval.clone());

    if let Some(mut snapshot) = state.event_hub.snapshot_for_thread(&thread_id).await {
        if !snapshot.pending_approval_ids.contains(&approval.id) {
            snapshot.pending_approval_ids.push(approval.id.clone());
        }
        snapshot.status = SessionStatus::WaitingForApproval;
        snapshot.updated_at = now;
        state.event_hub.set_snapshot(snapshot).await;
    }

    state
        .event_hub
        .publish(ServerEnvelope::ApprovalRequest(approval.clone()));

    Ok((StatusCode::ACCEPTED, Json(approval)))
}

fn is_dev_approval_id(approval_id: &str) -> bool {
    approval_id.starts_with("dev-approval-")
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

async fn require_control_auth(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, ApiError> {
    require_control_token(&state, request.headers())?;

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
    BadRequest(&'static str),
    Forbidden(&'static str),
    NotFound(&'static str),
    UnsupportedMediaType(&'static str),
    Internal(&'static str),
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
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message.to_string()),
            Self::Forbidden(message) => (StatusCode::FORBIDDEN, message.to_string()),
            Self::NotFound(message) => (StatusCode::NOT_FOUND, message.to_string()),
            Self::UnsupportedMediaType(message) => {
                (StatusCode::UNSUPPORTED_MEDIA_TYPE, message.to_string())
            }
            Self::Internal(message) => (StatusCode::INTERNAL_SERVER_ERROR, message.to_string()),
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

pub async fn serve_phone_with_static_dir(
    addr: SocketAddr,
    state: AppState,
    static_dir: impl Into<PathBuf>,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        build_phone_router_with_static_dir(state, static_dir),
    )
    .await?;
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
        cdp::BridgeConnectionState,
        codex_rpc::{
            CodexAdapter, CodexPendingApproval, CodexRpcError, CodexThread, CodexTurn,
            CodexTurnPage,
        },
        diagnostics::DiagnosticsReport,
        local_assets::LocalAssetRegistryConfig,
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
                "version": env!("CARGO_PKG_VERSION"),
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
    async fn unpaired_request_cannot_create_session() {
        let (_dir, state) = test_state();
        let app = build_router(state);

        let response = app
            .oneshot(json_request(
                Method::POST,
                "/api/sessions",
                json!({ "text": "start from phone" }),
            ))
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn pairing_start_requires_control_token() {
        let (_dir, state) = test_state();
        let app = build_router(state);

        for request in [
            request(Method::POST, "/api/control/pairing/start", Body::empty()),
            Request::builder()
                .method(Method::POST)
                .uri("/api/control/pairing/start")
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
                .uri("/api/control/pairing/start")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {TEST_CONTROL_TOKEN}"),
                )
                .body(Body::empty())
                .expect("request builds"),
            Request::builder()
                .method(Method::POST)
                .uri("/api/control/pairing/start")
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
    async fn control_diagnostics_returns_detail_to_control_clients() {
        let (_dir, state) = test_state();
        let state = state.with_diagnostics(DiagnosticsReport::degraded(
            BridgeConnectionState::InjectFailed,
            "app-server bridge module was not found",
        ));
        let app = build_control_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/control/diagnostics")
                    .header(BRIDGE_CONTROL_TOKEN_HEADER, TEST_CONTROL_TOKEN)
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_json(response).await,
            json!({
                "status": "degraded",
                "connectionState": "inject_failed",
                "detail": "app-server bridge module was not found",
            })
        );
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
    async fn phone_router_does_not_mount_local_control_routes() {
        let (_dir, state) = test_state();
        let app = build_phone_router(state);

        for request in [
            request(Method::POST, "/api/control/pairing/start", Body::empty()),
            request(Method::GET, "/api/control/diagnostics", Body::empty()),
            request(Method::GET, "/api/control/devices", Body::empty()),
            request(
                Method::DELETE,
                "/api/control/devices/phone-1",
                Body::empty(),
            ),
            json_request(
                Method::POST,
                "/api/control/dev/approvals",
                json!({ "threadId": "thread-1" }),
            ),
            request(Method::POST, "/api/pairing/start", Body::empty()),
            request(Method::GET, "/api/devices", Body::empty()),
            json_request(
                Method::POST,
                "/api/dev/approvals",
                json!({ "threadId": "thread-1" }),
            ),
        ] {
            let response = app
                .clone()
                .oneshot(request)
                .await
                .expect("request succeeds");

            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
    }

    #[tokio::test]
    async fn control_routes_reject_phone_session_tokens() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let app = build_control_router(state);

        for request in [
            Request::builder()
                .method(Method::GET)
                .uri("/api/control/devices")
                .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                .body(Body::empty())
                .expect("request builds"),
            Request::builder()
                .method(Method::GET)
                .uri("/api/control/diagnostics")
                .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                .body(Body::empty())
                .expect("request builds"),
            Request::builder()
                .method(Method::POST)
                .uri("/api/control/dev/approvals")
                .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "threadId": "thread-1" }).to_string()))
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
    async fn control_routes_accept_control_token_for_device_management() {
        let (_dir, state) = test_state();
        pair_device(&state).await;
        let app = build_control_router(state);

        let list_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/control/devices")
                    .header(BRIDGE_CONTROL_TOKEN_HEADER, TEST_CONTROL_TOKEN)
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(list_response.status(), StatusCode::OK);
        let body = response_json(list_response).await;
        assert_eq!(body[0]["deviceId"], json!("phone-1"));

        let revoke_response = app
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/api/control/devices/phone-1")
                    .header(BRIDGE_CONTROL_TOKEN_HEADER, TEST_CONTROL_TOKEN)
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(revoke_response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn dev_approval_trigger_broadcasts_approval_request() {
        let (_dir, state) = test_state();
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
        let mut subscriber = state.event_hub().subscribe().await;
        assert!(matches!(
            subscriber.recv().await.expect("initial snapshot replays"),
            ServerEnvelope::SessionSnapshot(_)
        ));
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/control/dev/approvals")
                    .header(BRIDGE_CONTROL_TOKEN_HEADER, TEST_CONTROL_TOKEN)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "threadId": "thread-1",
                            "kind": "command",
                            "title": "Smoke approval",
                            "detail": "echo approval smoke"
                        })
                        .to_string(),
                    ))
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = response_json(response).await;
        let approval_id = body["id"].as_str().expect("approval id is returned");
        assert_eq!(body["threadId"], json!("thread-1"));
        assert_eq!(body["title"], json!("Smoke approval"));

        match subscriber.recv().await.expect("snapshot update broadcasts") {
            ServerEnvelope::SessionSnapshot(snapshot) => {
                assert_eq!(snapshot.thread_id, "thread-1");
                assert_eq!(snapshot.status, SessionStatus::WaitingForApproval);
                assert_eq!(snapshot.pending_approval_ids, vec![approval_id.to_string()]);
            }
            envelope => panic!("expected session snapshot, got {envelope:?}"),
        }
        match subscriber
            .recv()
            .await
            .expect("approval request broadcasts")
        {
            ServerEnvelope::ApprovalRequest(approval) => {
                assert_eq!(approval.id, approval_id);
                assert_eq!(approval.thread_id, "thread-1");
                assert_eq!(approval.title, "Smoke approval");
            }
            envelope => panic!("expected approval request, got {envelope:?}"),
        }
    }

    #[tokio::test]
    async fn lists_real_pending_approvals_from_desktop_adapter() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let adapter = Arc::new(RecordingAdapter::with_pending_approvals(vec![
            CodexPendingApproval {
                thread_id: "thread-approval".to_string(),
                request_id: "7".to_string(),
                method: "mcpServer/elicitation/request".to_string(),
                params: json!({
                    "serverName": "mcpServers",
                    "message": "Allow the mcpServers MCP server to run tool \"read_memory\"?",
                    "_meta": {
                        "codex_approval_kind": "mcp_tool_call",
                        "tool_params": { "uri": "system://boot" },
                        "tool_params_display": [
                            { "name": "uri", "value": "system://boot", "display_name": "uri" }
                        ]
                    }
                }),
            },
        ]));
        let app = build_router(state.with_codex_adapter(adapter));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/approvals")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body[0]["id"], json!("thread-approval:7"));
        assert_eq!(body[0]["threadId"], json!("thread-approval"));
        assert_eq!(body[0]["kind"], json!("mcp"));
        assert_eq!(body[0]["title"], json!("Allow read_memory"));
        assert_eq!(body[0]["detail"], json!("uri: system://boot"));
    }

    #[tokio::test]
    async fn approval_list_preserves_debug_approvals() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let app = build_router(state);

        let trigger_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/control/dev/approvals")
                    .header(BRIDGE_CONTROL_TOKEN_HEADER, TEST_CONTROL_TOKEN)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "threadId": "thread-debug",
                            "title": "Debug approval",
                            "detail": "echo debug"
                        })
                        .to_string(),
                    ))
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");
        assert_eq!(trigger_response.status(), StatusCode::ACCEPTED);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/approvals")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body.as_array().map(Vec::len), Some(1));
        assert_eq!(body[0]["threadId"], json!("thread-debug"));
        assert_eq!(body[0]["title"], json!("Debug approval"));
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
    async fn dev_approval_decision_resolves_without_adapter() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let adapter = Arc::new(RecordingAdapter::default());
        let decisions = adapter.decisions();
        let mut subscriber = state.event_hub().subscribe().await;
        let app = build_router(state.with_codex_adapter(adapter));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/approvals/dev-approval-1/decision")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "decision": "approve", "comment": "ok" }).to_string(),
                    ))
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert!(decisions.lock().expect("decisions lock").is_empty());
        match subscriber
            .recv()
            .await
            .expect("approval resolution broadcasts")
        {
            ServerEnvelope::ApprovalResolved(decision) => {
                assert_eq!(decision.approval_id, "dev-approval-1");
                assert_eq!(decision.decision, DecisionKind::Approve);
            }
            envelope => panic!("expected approval resolved, got {envelope:?}"),
        }
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

    #[tokio::test]
    async fn evicted_local_image_asset_returns_not_found_and_can_be_registered_again() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let first_path = PathBuf::from("/var/folders/first.png");
        let second_path = PathBuf::from("/var/folders/second.png");
        let adapter = Arc::new(RecordingAdapter::with_turns_by_thread(vec![
            (
                "thread-first",
                vec![CodexTurn {
                    id: Some("turn-first".to_string()),
                    thread_id: Some("thread-first".to_string()),
                    created_at: Some(1_725_000_000_000),
                    updated_at: None,
                    raw: json!({
                        "items": [
                            {
                                "id": "item-first",
                                "type": "userMessage",
                                "content": [
                                    {
                                        "type": "localImage",
                                        "path": first_path.to_string_lossy(),
                                        "detail": null
                                    }
                                ]
                            }
                        ]
                    }),
                }],
            ),
            (
                "thread-second",
                vec![CodexTurn {
                    id: Some("turn-second".to_string()),
                    thread_id: Some("thread-second".to_string()),
                    created_at: Some(1_725_000_000_001),
                    updated_at: None,
                    raw: json!({
                        "items": [
                            {
                                "id": "item-second",
                                "type": "userMessage",
                                "content": [
                                    {
                                        "type": "localImage",
                                        "path": second_path.to_string_lossy(),
                                        "detail": null
                                    }
                                ]
                            }
                        ]
                    }),
                }],
            ),
        ]));
        let registry = LocalAssetRegistry::with_config(LocalAssetRegistryConfig {
            max_entries: 1,
            ttl_ms: 60_000,
        });
        let app = build_router(
            state
                .with_local_asset_registry(registry)
                .with_codex_adapter(adapter),
        );

        let first_src = first_asset_src(
            app.clone()
                .oneshot(
                    Request::builder()
                        .method(Method::GET)
                        .uri("/api/sessions/thread-first/events")
                        .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                        .body(Body::empty())
                        .expect("request builds"),
                )
                .await
                .expect("request succeeds"),
        )
        .await;
        let second_src = first_asset_src(
            app.clone()
                .oneshot(
                    Request::builder()
                        .method(Method::GET)
                        .uri("/api/sessions/thread-second/events")
                        .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                        .body(Body::empty())
                        .expect("request builds"),
                )
                .await
                .expect("request succeeds"),
        )
        .await;
        assert_ne!(first_src, second_src);

        let evicted_asset_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(&first_src)
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");
        assert_eq!(evicted_asset_response.status(), StatusCode::NOT_FOUND);

        let first_src_after_eviction = first_asset_src(
            app.oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/sessions/thread-first/events")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds"),
        )
        .await;
        assert_ne!(first_src, first_src_after_eviction);
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
    async fn paginated_event_request_returns_latest_bounded_page() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        for index in 1..=4 {
            state
                .publish_session_event(SessionEvent {
                    id: format!("event-{index}"),
                    thread_id: "thread-1".to_string(),
                    event_type: SessionEventType::Message,
                    payload: json!({ "role": "assistant", "text": format!("message-{index}") }),
                    created_at: 1_725_000_000_000 + index,
                })
                .await;
        }
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/sessions/thread-1/events?limit=2")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["events"][0]["id"], json!("event-3"));
        assert_eq!(body["events"][1]["id"], json!("event-4"));
        assert_eq!(body["beforeCursor"], json!("event-3"));
        assert_eq!(body["afterCursor"], json!("event-4"));
        assert_eq!(body["hasMoreBefore"], json!(true));
        assert_eq!(body["hasMoreAfter"], json!(false));
        assert_eq!(body["reset"], json!(false));
    }

    #[tokio::test]
    async fn event_since_cursor_returns_tail_overlap_and_new_events() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        for index in 1..=5 {
            state
                .publish_session_event(SessionEvent {
                    id: format!("event-{index}"),
                    thread_id: "thread-1".to_string(),
                    event_type: SessionEventType::Message,
                    payload: json!({ "role": "assistant", "text": format!("message-{index}") }),
                    created_at: 1_725_000_000_000 + index,
                })
                .await;
        }
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/sessions/thread-1/events?limit=3&since=event-2")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["events"][0]["id"], json!("event-2"));
        assert_eq!(body["events"][2]["id"], json!("event-4"));
        assert_eq!(body["afterCursor"], json!("event-4"));
        assert_eq!(body["hasMoreAfter"], json!(true));
    }

    #[tokio::test]
    async fn event_before_cursor_returns_previous_history_page() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        for index in 1..=5 {
            state
                .publish_session_event(SessionEvent {
                    id: format!("event-{index}"),
                    thread_id: "thread-1".to_string(),
                    event_type: SessionEventType::Message,
                    payload: json!({ "role": "assistant", "text": format!("message-{index}") }),
                    created_at: 1_725_000_000_000 + index,
                })
                .await;
        }
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/sessions/thread-1/events?limit=2&before=event-4")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["events"][0]["id"], json!("event-2"));
        assert_eq!(body["events"][1]["id"], json!("event-3"));
        assert_eq!(body["beforeCursor"], json!("event-2"));
        assert_eq!(body["hasMoreBefore"], json!(true));
        assert_eq!(body["hasMoreAfter"], json!(true));
    }

    #[tokio::test]
    async fn adapter_cursor_fetches_older_turns_for_history_page() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let adapter = Arc::new(RecordingAdapter::with_turn_pages(
            "thread-1",
            CodexTurnPage {
                turns: vec![message_turn(4), message_turn(3)],
                next_cursor: Some("older-cursor".to_string()),
                backwards_cursor: Some("newer-cursor".to_string()),
            },
            vec![(
                "older-cursor",
                CodexTurnPage {
                    turns: vec![message_turn(2), message_turn(1)],
                    next_cursor: None,
                    backwards_cursor: Some("page-2-newer".to_string()),
                },
            )],
        ));
        let requested_cursors = adapter.turn_page_cursors();
        let app = build_router(state.with_codex_adapter(adapter));

        let initial = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/sessions/thread-1/events?limit=2")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");
        let initial = response_json(initial).await;
        assert_eq!(initial["events"][0]["id"], json!("turn-3:item-3"));
        assert_eq!(initial["events"][1]["id"], json!("turn-4:item-4"));
        assert_eq!(initial["hasMoreBefore"], json!(true));

        let older = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/sessions/thread-1/events?limit=2&before=turn-3%3Aitem-3")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");
        let older = response_json(older).await;
        assert_eq!(older["events"][0]["id"], json!("turn-1:item-1"));
        assert_eq!(older["events"][1]["id"], json!("turn-2:item-2"));
        assert_eq!(older["hasMoreBefore"], json!(false));
        assert_eq!(
            requested_cursors.lock().expect("cursor lock").as_slice(),
            &[None, None, Some("older-cursor".to_string())]
        );
    }

    #[tokio::test]
    async fn event_pagination_headers_enable_bounded_page_response() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        for index in 1..=3 {
            state
                .publish_session_event(SessionEvent {
                    id: format!("event-{index}"),
                    thread_id: "thread-1".to_string(),
                    event_type: SessionEventType::Message,
                    payload: json!({ "role": "assistant", "text": format!("message-{index}") }),
                    created_at: 1_725_000_000_000 + index,
                })
                .await;
        }
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/sessions/thread-1/events")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .header("x-codex-events-limit", "2")
                    .header("x-codex-events-since", "event-1")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["events"][0]["id"], json!("event-1"));
        assert_eq!(body["events"][1]["id"], json!("event-2"));
        assert_eq!(body["hasMoreAfter"], json!(true));
    }

    #[tokio::test]
    async fn paginated_event_response_supports_gzip_compression() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        for index in 1..=20 {
            state
                .publish_session_event(SessionEvent {
                    id: format!("event-{index}"),
                    thread_id: "thread-1".to_string(),
                    event_type: SessionEventType::Message,
                    payload: json!({
                        "role": "assistant",
                        "text": format!("message-{index}-{}", "compressible".repeat(100)),
                    }),
                    created_at: 1_725_000_000_000 + index,
                })
                .await;
        }
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/sessions/thread-1/events?limit=20")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .header(header::ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_ENCODING),
            Some(&header::HeaderValue::from_static("gzip"))
        );
    }

    #[tokio::test]
    async fn repeated_adapter_polls_replace_cached_history_instead_of_duplicating_events() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let adapter = Arc::new(RecordingAdapter::with_turns(
            "thread-1",
            vec![CodexTurn {
                id: Some("turn-1".to_string()),
                thread_id: Some("thread-1".to_string()),
                created_at: Some(1_725_000_000_000),
                updated_at: None,
                raw: json!({
                    "items": [{
                        "id": "item-1",
                        "type": "agentMessage",
                        "text": "Stable reply",
                    }],
                }),
            }],
        ));
        let state = state.with_codex_adapter(adapter);
        let app = build_router(state.clone());

        for _ in 0..2 {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::GET)
                        .uri("/api/sessions/thread-1/events?limit=50")
                        .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                        .body(Body::empty())
                        .expect("request builds"),
                )
                .await
                .expect("request succeeds");
            assert_eq!(response.status(), StatusCode::OK);
        }

        assert_eq!(
            state
                .event_history
                .lock()
                .await
                .get("thread-1")
                .map(VecDeque::len),
            Some(1)
        );
    }

    #[tokio::test]
    async fn repeated_adapter_polls_replace_changed_items_within_same_turn() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let adapter = Arc::new(RecordingAdapter::with_turns(
            "thread-1",
            vec![CodexTurn {
                id: Some("turn-changing".to_string()),
                thread_id: Some("thread-1".to_string()),
                created_at: Some(1_725_000_000_000),
                updated_at: None,
                raw: json!({
                    "items": [{
                        "id": "item-1",
                        "type": "userMessage",
                        "text": "same prompt",
                    }],
                }),
            }],
        ));
        let state = state.with_codex_adapter(adapter.clone());
        let app = build_router(state.clone());

        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/sessions/thread-1/events?limit=50")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");
        assert_eq!(
            response_json(first).await["events"][0]["id"],
            json!("turn-changing:item-1")
        );

        adapter.turns.lock().expect("turns lock").insert(
            "thread-1".to_string(),
            vec![CodexTurn {
                id: Some("turn-changing".to_string()),
                thread_id: Some("thread-1".to_string()),
                created_at: Some(1_725_000_000_000),
                updated_at: None,
                raw: json!({
                    "items": [
                        {
                            "id": "item-5",
                            "type": "userMessage",
                            "text": "same prompt",
                        },
                        {
                            "id": "item-7",
                            "type": "agentMessage",
                            "text": "final answer",
                        }
                    ],
                }),
            }],
        );

        let second = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/sessions/thread-1/events?limit=50&since=turn-changing%3Aitem-1")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");
        let second = response_json(second).await;

        assert_eq!(second["reset"], json!(true));
        assert_eq!(
            second["events"]
                .as_array()
                .expect("events are returned")
                .iter()
                .map(|event| event["id"].as_str().expect("event id"))
                .collect::<Vec<_>>(),
            vec!["turn-changing:item-5", "turn-changing:item-7"]
        );
        assert_eq!(
            state
                .event_history
                .lock()
                .await
                .get("thread-1")
                .map(VecDeque::len),
            Some(2)
        );
    }

    #[tokio::test]
    async fn paired_device_event_response_omits_large_adapter_raw_payload() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let adapter = Arc::new(RecordingAdapter::with_turns(
            "thread-large",
            vec![CodexTurn {
                id: Some("turn-large".to_string()),
                thread_id: Some("thread-large".to_string()),
                created_at: Some(1_725_000_000_000),
                updated_at: Some(1_725_000_000_000),
                raw: json!({
                    "id": "turn-large",
                    "items": [{
                        "id": "item-large",
                        "type": "agentMessage",
                        "text": "Visible reply",
                        "debugTrace": "x".repeat(500_000),
                    }],
                }),
            }],
        ));
        let app = build_router(state.with_codex_adapter(adapter));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/sessions/thread-large/events")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body[0]["payload"]["text"], json!("Visible reply"));
        assert_eq!(body[0]["payload"].get("raw"), None);
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
            &[("thread-1".to_string(), "hello Codex".to_string(), vec![])]
        );
    }

    #[tokio::test]
    async fn paired_device_message_retries_are_idempotent_by_client_message_id() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let adapter = Arc::new(RecordingAdapter::default());
        let messages = adapter.messages();
        let mut subscriber = state.event_hub().subscribe().await;
        let app = build_router(state.with_codex_adapter(adapter));
        let request = || {
            Request::builder()
                .method(Method::POST)
                .uri("/api/sessions/thread-1/messages")
                .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-codex-client-message-id", "client-message-1")
                .body(Body::from(json!({ "text": "retry safely" }).to_string()))
                .expect("request builds")
        };

        let first = app
            .clone()
            .oneshot(request())
            .await
            .expect("first request succeeds");
        let second = app
            .oneshot(request())
            .await
            .expect("retry request succeeds");

        assert_eq!(first.status(), StatusCode::ACCEPTED);
        assert_eq!(second.status(), StatusCode::ACCEPTED);
        assert_eq!(messages.lock().expect("messages lock").len(), 1);
        assert!(matches!(
            subscriber.recv().await.expect("message event broadcasts"),
            ServerEnvelope::SessionEvent(_)
        ));
        let unexpected =
            tokio::time::timeout(std::time::Duration::from_millis(20), subscriber.recv()).await;
        assert!(matches!(
            unexpected,
            Err(_) | Ok(Err(tokio::sync::broadcast::error::RecvError::Closed))
        ));
    }

    #[tokio::test]
    async fn idempotent_retry_survives_first_http_request_cancellation() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let send_started = Arc::new(tokio::sync::Semaphore::new(0));
        let send_release = Arc::new(tokio::sync::Semaphore::new(0));
        let adapter = Arc::new(RecordingAdapter::with_send_gate(
            send_started.clone(),
            send_release.clone(),
        ));
        let messages = adapter.messages();
        let app = build_router(state.with_codex_adapter(adapter));
        let request = || {
            Request::builder()
                .method(Method::POST)
                .uri("/api/sessions/thread-1/messages")
                .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-codex-client-message-id", "client-message-cancelled")
                .body(Body::from(
                    json!({ "text": "retry after disconnect" }).to_string(),
                ))
                .expect("request builds")
        };

        let first_app = app.clone();
        let first_request = request();
        let second_request = request();
        let first = tokio::spawn(async move { first_app.oneshot(first_request).await });
        send_started
            .acquire()
            .await
            .expect("first send starts")
            .forget();
        first.abort();
        let _ = first.await;

        let second = tokio::spawn(async move { app.oneshot(second_request).await });
        send_release.add_permits(1);
        let second = tokio::time::timeout(std::time::Duration::from_secs(1), second)
            .await
            .expect("retry completes")
            .expect("retry task joins")
            .expect("retry request succeeds");

        assert_eq!(second.status(), StatusCode::ACCEPTED);
        assert_eq!(messages.lock().expect("messages lock").len(), 1);
    }

    #[tokio::test]
    async fn paired_device_send_message_with_image_routes_to_adapter_and_scrubs_event_path() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let adapter = Arc::new(RecordingAdapter::default());
        let messages = adapter.messages();
        let mut subscriber = state.event_hub().subscribe().await;
        let app = build_router(state.with_codex_adapter(adapter));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/sessions/thread-1/messages")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "text": "what is in this image?",
                            "attachments": [{
                                "name": "phone.png",
                                "mimeType": "image/png",
                                "dataBase64": "iVBORw0KGgo="
                            }]
                        })
                        .to_string(),
                    ))
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let recorded = messages.lock().expect("messages lock").clone();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "thread-1");
        assert_eq!(recorded[0].1, "what is in this image?");
        assert_eq!(recorded[0].2.len(), 1);
        let image_path = PathBuf::from(&recorded[0].2[0].path);
        assert!(
            image_path
                .parent()
                .expect("stored image has parent")
                .ends_with("codex-mobile-bridge/uploads")
        );
        assert_eq!(
            tokio::fs::read(&image_path)
                .await
                .expect("stored image reads"),
            b"\x89PNG\r\n\x1a\n"
        );

        match subscriber.recv().await.expect("message event broadcasts") {
            ServerEnvelope::SessionEvent(event) => {
                let serialized_event =
                    serde_json::to_string(&event).expect("event serializes to json");
                assert!(!serialized_event.contains(image_path.to_string_lossy().as_ref()));
                assert_eq!(event.payload["text"], json!("what is in this image?"));
                assert_eq!(event.payload["attachments"][0]["name"], json!("phone.png"));
                let src = event.payload["attachments"][0]["src"]
                    .as_str()
                    .expect("asset src is returned");
                assert!(src.starts_with("/api/assets/local-image/"));
            }
            envelope => panic!("expected session event, got {envelope:?}"),
        }
    }

    #[tokio::test]
    async fn paired_device_create_session_routes_to_codex_adapter_and_records_first_message() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let adapter = Arc::new(RecordingAdapter::default());
        let started_threads = adapter.started_threads();
        let app = build_router(state.with_codex_adapter(adapter));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/sessions")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "text": "start a fresh task from phone" }).to_string(),
                    ))
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = response_json(response).await;
        assert_eq!(body["threadId"], json!("thread-created-1"));
        assert_eq!(body["title"], json!("start a fresh task from phone"));
        assert_eq!(body["preview"], json!("start a fresh task from phone"));
        assert_eq!(body["status"], json!("running"));
        assert_eq!(
            started_threads
                .lock()
                .expect("started threads lock")
                .as_slice(),
            &["start a fresh task from phone".to_string()]
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/sessions/thread-created-1/events")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body[0]["threadId"], json!("thread-created-1"));
        assert_eq!(
            body[0]["payload"]["text"],
            json!("start a fresh task from phone")
        );
    }

    #[tokio::test]
    async fn paired_device_create_session_rejects_blank_text() {
        let (_dir, state) = test_state();
        let session_token = pair_device(&state).await;
        let adapter = Arc::new(RecordingAdapter::default());
        let started_threads = adapter.started_threads();
        let app = build_router(state.with_codex_adapter(adapter));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/sessions")
                    .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "text": "   " }).to_string()))
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(
            started_threads
                .lock()
                .expect("started threads lock")
                .is_empty()
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

    async fn first_asset_src(response: Response) -> String {
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        body[0]["payload"]["attachments"][0]["src"]
            .as_str()
            .expect("asset src is present")
            .to_string()
    }

    fn message_turn(index: u64) -> CodexTurn {
        CodexTurn {
            id: Some(format!("turn-{index}")),
            thread_id: Some("thread-1".to_string()),
            created_at: Some(1_725_000_000_000 + index),
            updated_at: None,
            raw: json!({
                "items": [{
                    "id": format!("item-{index}"),
                    "type": "agentMessage",
                    "text": format!("message-{index}"),
                }],
            }),
        }
    }

    #[derive(Default)]
    struct RecordingAdapter {
        decisions: Arc<StdMutex<Vec<ApprovalDecision>>>,
        messages: Arc<StdMutex<Vec<(String, String, Vec<UserImageAttachment>)>>>,
        pending_approvals: Arc<StdMutex<Vec<CodexPendingApproval>>>,
        send_release: Option<Arc<tokio::sync::Semaphore>>,
        send_started: Option<Arc<tokio::sync::Semaphore>>,
        started_threads: Arc<StdMutex<Vec<String>>>,
        threads: Arc<StdMutex<Vec<CodexThread>>>,
        turns: Arc<StdMutex<StdHashMap<String, Vec<CodexTurn>>>>,
        turn_pages: Arc<StdMutex<StdHashMap<String, CodexTurnPage>>>,
        turn_page_cursors: Arc<StdMutex<Vec<Option<String>>>>,
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

        fn with_turns_by_thread(threads: Vec<(&'static str, Vec<CodexTurn>)>) -> Self {
            let turns_by_thread = threads
                .into_iter()
                .map(|(thread_id, turns)| (thread_id.to_string(), turns))
                .collect();
            Self {
                turns: Arc::new(StdMutex::new(turns_by_thread)),
                ..Self::default()
            }
        }

        fn with_turn_pages(
            thread_id: &str,
            first_page: CodexTurnPage,
            cursor_pages: Vec<(&str, CodexTurnPage)>,
        ) -> Self {
            let mut turn_pages = StdHashMap::new();
            turn_pages.insert(turn_page_key(thread_id, None), first_page);
            for (cursor, page) in cursor_pages {
                turn_pages.insert(turn_page_key(thread_id, Some(cursor)), page);
            }
            Self {
                turn_pages: Arc::new(StdMutex::new(turn_pages)),
                ..Self::default()
            }
        }

        fn with_pending_approvals(pending_approvals: Vec<CodexPendingApproval>) -> Self {
            Self {
                pending_approvals: Arc::new(StdMutex::new(pending_approvals)),
                ..Self::default()
            }
        }

        fn with_send_gate(
            send_started: Arc<tokio::sync::Semaphore>,
            send_release: Arc<tokio::sync::Semaphore>,
        ) -> Self {
            Self {
                send_release: Some(send_release),
                send_started: Some(send_started),
                ..Self::default()
            }
        }

        fn decisions(&self) -> Arc<StdMutex<Vec<ApprovalDecision>>> {
            self.decisions.clone()
        }

        fn messages(&self) -> Arc<StdMutex<Vec<(String, String, Vec<UserImageAttachment>)>>> {
            self.messages.clone()
        }

        fn started_threads(&self) -> Arc<StdMutex<Vec<String>>> {
            self.started_threads.clone()
        }

        fn turn_page_cursors(&self) -> Arc<StdMutex<Vec<Option<String>>>> {
            self.turn_page_cursors.clone()
        }
    }

    #[async_trait]
    impl CodexAdapter for RecordingAdapter {
        async fn list_threads(&self) -> Result<Vec<CodexThread>, CodexRpcError> {
            Ok(self.threads.lock().expect("threads lock").clone())
        }

        async fn start_thread(
            &self,
            text: &str,
            _attachments: &[UserImageAttachment],
        ) -> Result<CodexThread, CodexRpcError> {
            let id = {
                let mut started_threads =
                    self.started_threads.lock().expect("started threads lock");
                started_threads.push(text.to_string());
                format!("thread-created-{}", started_threads.len())
            };
            self.turns.lock().expect("turns lock").insert(
                id.clone(),
                vec![CodexTurn {
                    id: Some("turn-created-1".to_string()),
                    thread_id: Some(id.clone()),
                    created_at: Some(1_725_000_000_000),
                    updated_at: Some(1_725_000_000_000),
                    raw: json!({
                        "id": "turn-created-1",
                        "items": [{
                            "id": "item-0",
                            "type": "userMessage",
                            "text": text,
                            "createdAt": 1_725_000_000_000_u64,
                        }],
                    }),
                }],
            );
            Ok(CodexThread {
                id: id.clone(),
                title: None,
                cwd: Some("/repo".to_string()),
                model_provider: Some("OpenAI".to_string()),
                preview: None,
                created_at: None,
                updated_at: None,
                raw: json!({ "id": id }),
            })
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

        async fn list_turns_page(
            &self,
            thread_id: &str,
            cursor: Option<&str>,
        ) -> Result<CodexTurnPage, CodexRpcError> {
            self.turn_page_cursors
                .lock()
                .expect("turn page cursor lock")
                .push(cursor.map(ToString::to_string));
            if let Some(page) = self
                .turn_pages
                .lock()
                .expect("turn pages lock")
                .get(&turn_page_key(thread_id, cursor))
                .cloned()
            {
                return Ok(page);
            }
            Ok(CodexTurnPage {
                turns: self.list_turns(thread_id).await?,
                next_cursor: None,
                backwards_cursor: None,
            })
        }

        async fn send_user_message(
            &self,
            thread_id: &str,
            text: &str,
            attachments: &[UserImageAttachment],
        ) -> Result<(), CodexRpcError> {
            self.messages.lock().expect("messages lock").push((
                thread_id.to_string(),
                text.to_string(),
                attachments.to_vec(),
            ));
            if let Some(send_started) = &self.send_started {
                send_started.add_permits(1);
            }
            if let Some(send_release) = &self.send_release {
                send_release
                    .acquire()
                    .await
                    .expect("send release semaphore stays open")
                    .forget();
            }
            Ok(())
        }

        async fn list_pending_approvals(&self) -> Result<Vec<CodexPendingApproval>, CodexRpcError> {
            Ok(self
                .pending_approvals
                .lock()
                .expect("pending approvals lock")
                .clone())
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

    fn turn_page_key(thread_id: &str, cursor: Option<&str>) -> String {
        format!("{thread_id}\u{0}{}", cursor.unwrap_or_default())
    }
}
