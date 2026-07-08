use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use axum::{
    Extension, Json, Router,
    body::Body,
    extract::{
        Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    event_hub::EventHub,
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
}

#[derive(Debug, Clone)]
struct AuthenticatedDevice {
    device_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    status: &'static str,
    connection_state: &'static str,
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
    pub fn new(pairing: PairingManager, event_hub: EventHub) -> Self {
        Self {
            pairing: Arc::new(Mutex::new(pairing)),
            event_hub,
        }
    }

    pub fn event_hub(&self) -> EventHub {
        self.event_hub.clone()
    }
}

pub fn build_router(state: AppState) -> Router {
    let authenticated_routes = Router::new()
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

    Router::new()
        .route("/api/health", get(health))
        .route("/api/pairing/start", post(start_pairing))
        .route("/api/pairing/complete", post(complete_pairing))
        .route("/ws", get(ws_handler))
        .merge(authenticated_routes)
        .with_state(state)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        connection_state: "mocked",
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

async fn list_sessions(State(state): State<AppState>) -> Json<Vec<SessionSnapshot>> {
    Json(state.event_hub.all_snapshots().await)
}

async fn list_session_events(Path(_thread_id): Path<String>) -> Json<Vec<SessionEvent>> {
    Json(Vec::new())
}

async fn send_message(
    State(state): State<AppState>,
    Path(thread_id): Path<String>,
    Json(request): Json<SendMessageRequest>,
) -> (StatusCode, Json<AcceptedResponse>) {
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
    state.event_hub.publish(ServerEnvelope::SessionEvent(event));

    (
        StatusCode::ACCEPTED,
        Json(AcceptedResponse { accepted: true }),
    )
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

    state
        .event_hub
        .publish(ServerEnvelope::ApprovalResolved(decision));

    Ok((
        StatusCode::ACCEPTED,
        Json(AcceptedResponse { accepted: true }),
    ))
}

async fn ws_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let token = query
        .get("token")
        .map(String::as_str)
        .or_else(|| bearer_token_from_headers(&headers))
        .ok_or(ApiError::Unauthorized)?;
    authenticate_token(&state, token).await?;

    Ok(ws.on_upgrade(move |socket| websocket_stream(state.event_hub, socket)))
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

async fn authenticate_token(state: &AppState, token: &str) -> Result<String, ApiError> {
    let pairing = state.pairing.lock().await;
    pairing
        .validate_session_token(token)
        .map_err(|_| ApiError::Unauthorized)
}

fn bearer_token_from_headers(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .filter(|token| !token.is_empty())
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
    Pairing(PairingError),
}

impl From<PairingError> for ApiError {
    fn from(error: PairingError) -> Self {
        Self::Pairing(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_string()),
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
        };

        (status, Json(ErrorResponse { error: message })).into_response()
    }
}

pub async fn serve(addr: SocketAddr, state: AppState) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, build_router(state)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Method, Request, StatusCode, header},
    };
    use serde_json::{Value, json};
    use std::path::PathBuf;
    use tempfile::{TempDir, tempdir};
    use tower::ServiceExt;

    use crate::{
        pairing::PairingManager,
        protocol::{SessionSnapshot, SessionStatus},
        storage::Storage,
    };

    fn temp_storage() -> (TempDir, Storage) {
        let dir = tempdir().expect("tempdir is created");
        let path: PathBuf = dir.path().join("bridge.sqlite");
        let storage = Storage::open(path).expect("storage opens");

        (dir, storage)
    }

    fn test_state() -> (TempDir, AppState) {
        let (dir, storage) = temp_storage();
        let pairing = PairingManager::with_clock(storage, || 1_725_000_000_000);
        let state = AppState::new(pairing, EventHub::new());

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

    #[tokio::test]
    async fn health_returns_connection_state() {
        let (_dir, state) = test_state();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/health")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_json(response).await,
            json!({
                "status": "ok",
                "connectionState": "mocked",
            })
        );
    }

    #[tokio::test]
    async fn unpaired_request_cannot_read_sessions() {
        let (_dir, state) = test_state();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/sessions")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
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
}
