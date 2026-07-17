use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub thread_id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    pub updated_at: u64,
    pub status: SessionStatus,
    pub pending_approval_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceOption {
    pub cwd: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiErrorCode {
    Unauthorized,
    InvalidRequest,
    Forbidden,
    NotFound,
    UnsupportedMediaType,
    InternalError,
    InvalidPairingToken,
    ExpiredPairingToken,
    DeviceRevoked,
    DeviceNotFound,
    AdapterError,
    WorkspaceRequired,
    WorkspaceNotAllowed,
    WorkspaceUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    Running,
    WaitingForInput,
    WaitingForApproval,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEvent {
    pub id: String,
    pub thread_id: String,
    #[serde(rename = "type")]
    pub event_type: SessionEventType,
    pub payload: Value,
    pub created_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEventType {
    Message,
    MessageDelta,
    ToolCall,
    ToolResult,
    ApprovalRequested,
    ApprovalResolved,
    StatusChanged,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub id: String,
    pub thread_id: String,
    pub kind: ApprovalKind,
    pub title: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
    pub created_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    Command,
    FileEdit,
    Network,
    Mcp,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalDecision {
    pub approval_id: String,
    pub decision: DecisionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    pub device_id: String,
    pub decided_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Approve,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ServerEnvelope {
    SessionSnapshot(SessionSnapshot),
    SessionEvent(SessionEvent),
    ApprovalRequest(ApprovalRequest),
    ApprovalResolved(ApprovalDecision),
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ClientCommand {
    Subscribe { thread_id: Option<String> },
    SendMessage { thread_id: String, text: String },
    ApprovalDecision(ApprovalDecision),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn approval_request_serializes_with_camel_case_fields() {
        let request = ApprovalRequest {
            id: "approval-1".to_string(),
            thread_id: "thread-1".to_string(),
            kind: ApprovalKind::Command,
            title: "Run command".to_string(),
            detail: "cargo test".to_string(),
            risk_hint: Some("Writes build artifacts".to_string()),
            raw: None,
            created_at: 1_725_000_000_000,
            expires_at: None,
        };

        let serialized = serde_json::to_value(request).expect("approval request serializes");

        assert_eq!(serialized["threadId"], json!("thread-1"));
        assert_eq!(serialized["riskHint"], json!("Writes build artifacts"));
        assert_eq!(serialized["kind"], json!("command"));
        assert!(serialized.get("thread_id").is_none());
        assert!(serialized.get("risk_hint").is_none());
    }

    #[test]
    fn websocket_envelope_round_trips() {
        let event = SessionEvent {
            id: "event-1".to_string(),
            thread_id: "thread-1".to_string(),
            event_type: SessionEventType::MessageDelta,
            payload: json!({ "role": "assistant", "text": "hello" }),
            created_at: 1_725_000_000_001,
        };
        let envelope = ServerEnvelope::SessionEvent(event);

        let serialized = serde_json::to_string(&envelope).expect("envelope serializes");
        let deserialized: ServerEnvelope =
            serde_json::from_str(&serialized).expect("envelope deserializes");

        assert_eq!(deserialized, envelope);
    }

    #[test]
    fn workspace_option_serializes_with_camel_case_fields() {
        let workspace = WorkspaceOption {
            cwd: "/Users/damon/Documents/my_ai/codex-app".to_string(),
        };

        assert_eq!(
            serde_json::to_value(workspace).expect("workspace option serializes"),
            json!({ "cwd": "/Users/damon/Documents/my_ai/codex-app" })
        );
    }

    #[test]
    fn api_error_code_serializes_as_stable_snake_case_value() {
        assert_eq!(
            serde_json::to_value(ApiErrorCode::WorkspaceNotAllowed)
                .expect("api error code serializes"),
            json!("workspace_not_allowed")
        );
        assert_eq!(
            serde_json::to_value(ApiErrorCode::InvalidPairingToken)
                .expect("pairing error code serializes"),
            json!("invalid_pairing_token")
        );
    }
}
