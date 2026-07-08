use serde_json::{Value, json};

use crate::{
    codex_rpc::{CodexRawEvent, CodexThread, CodexTurn},
    protocol::{SessionEvent, SessionEventType, SessionSnapshot, SessionStatus},
};

pub struct Normalizer;

impl Normalizer {
    pub fn snapshot_from_thread(thread: &CodexThread) -> SessionSnapshot {
        SessionSnapshot {
            thread_id: thread.id.clone(),
            title: thread
                .title
                .clone()
                .or_else(|| thread.preview.clone())
                .unwrap_or_else(|| thread.id.clone()),
            cwd: thread.cwd.clone(),
            model_provider: thread.model_provider.clone(),
            preview: thread.preview.clone(),
            updated_at: thread
                .updated_at
                .or(thread.created_at)
                .or_else(|| {
                    timestamp_field(
                        &thread.raw,
                        &["updatedAt", "updated_at", "createdAt", "created_at"],
                    )
                })
                .unwrap_or_default(),
            status: status_from_value(&thread.raw).unwrap_or(SessionStatus::Idle),
            pending_approval_ids: approval_ids_from_value(&thread.raw),
        }
    }

    pub fn events_from_turns(thread_id: &str, turns: &[CodexTurn]) -> Vec<SessionEvent> {
        turns
            .iter()
            .enumerate()
            .flat_map(|(turn_index, turn)| events_from_turn(thread_id, turn_index, turn))
            .collect()
    }

    pub fn event_from_raw_notification(notification: &CodexRawEvent) -> SessionEvent {
        let thread_id = thread_id_from_value(&notification.params).unwrap_or_default();
        let created_at = timestamp_field(
            &notification.params,
            &["createdAt", "created_at", "timestamp", "time"],
        )
        .unwrap_or_default();
        let raw = notification.params.clone();
        let method = notification.method.as_str();

        if method.contains("delta") {
            let text = text_from_value(&notification.params).unwrap_or_default();
            return SessionEvent {
                id: event_id(&thread_id, &notification.params, "delta"),
                thread_id,
                event_type: SessionEventType::MessageDelta,
                payload: json!({
                    "role": "assistant",
                    "text": text,
                    "raw": raw,
                }),
                created_at,
            };
        }

        if method == "turn/started" {
            return SessionEvent {
                id: event_id(&thread_id, &notification.params, "turn-started"),
                thread_id,
                event_type: SessionEventType::StatusChanged,
                payload: json!({
                    "status": "running",
                    "raw": raw,
                }),
                created_at,
            };
        }

        if let Some(status) = status_from_value(&notification.params) {
            return SessionEvent {
                id: event_id(&thread_id, &notification.params, "status"),
                thread_id,
                event_type: SessionEventType::StatusChanged,
                payload: json!({
                    "status": status,
                    "raw": raw,
                }),
                created_at,
            };
        }

        SessionEvent {
            id: event_id(&thread_id, &notification.params, "unknown"),
            thread_id,
            event_type: SessionEventType::StatusChanged,
            payload: json!({
                "method": notification.method,
                "raw": raw,
            }),
            created_at,
        }
    }
}

fn events_from_turn(thread_id: &str, turn_index: usize, turn: &CodexTurn) -> Vec<SessionEvent> {
    turn_items(&turn.raw)
        .into_iter()
        .enumerate()
        .map(|(item_index, item)| event_from_item(thread_id, turn_index, item_index, &item, turn))
        .collect()
}

fn event_from_item(
    thread_id: &str,
    turn_index: usize,
    item_index: usize,
    item: &Value,
    turn: &CodexTurn,
) -> SessionEvent {
    let role = role_from_item(item);
    let event_type = event_type_for_role(&role);
    let text = text_from_value(item).unwrap_or_default();
    let created_at = timestamp_field(item, &["createdAt", "created_at", "timestamp"])
        .or(turn.created_at)
        .or(turn.updated_at)
        .unwrap_or_default();
    let id = string_field(item, &["id", "itemId", "item_id"])
        .or_else(|| turn.id.clone().map(|id| format!("{id}:{item_index}")))
        .unwrap_or_else(|| format!("{thread_id}:turn-{turn_index}:item-{item_index}"));

    SessionEvent {
        id,
        thread_id: thread_id.to_string(),
        event_type,
        payload: json!({
            "role": role,
            "text": text,
            "raw": item,
        }),
        created_at,
    }
}

fn turn_items(value: &Value) -> Vec<Value> {
    if let Some(items) = value.get("items").and_then(Value::as_array) {
        return items.clone();
    }
    if let Some(items) = value.get("messages").and_then(Value::as_array) {
        return items.clone();
    }

    let mut items = Vec::new();
    if let Some(input) = value.get("input") {
        items.push(json!({ "type": "userMessage", "content": input }));
    }
    if let Some(output) = value.get("output") {
        items.push(json!({ "type": "agentMessage", "content": output }));
    }
    if let Some(request) = value.get("request") {
        items.push(json!({ "type": "userMessage", "content": request }));
    }
    if let Some(response) = value.get("response") {
        items.push(json!({ "type": "agentMessage", "content": response }));
    }

    if items.is_empty() {
        vec![value.clone()]
    } else {
        items
    }
}

fn event_type_for_role(role: &str) -> SessionEventType {
    match role {
        "tool" => SessionEventType::ToolCall,
        "tool_result" => SessionEventType::ToolResult,
        "unknown" => SessionEventType::Error,
        _ => SessionEventType::Message,
    }
}

fn role_from_item(item: &Value) -> &'static str {
    let raw = string_field(
        item,
        &[
            "role",
            "type",
            "authorRole",
            "author_role",
            "itemType",
            "item_type",
        ],
    )
    .or_else(|| {
        item.pointer("/author/role")
            .and_then(Value::as_str)
            .map(ToString::to_string)
    })
    .unwrap_or_default()
    .to_ascii_lowercase();

    match raw.as_str() {
        "user" | "usermessage" | "user_message" | "input_text" | "input" => "user",
        "assistant" | "agent" | "codex" | "agentmessage" | "agent_message" | "assistantmessage"
        | "assistant_message" | "output_text" | "output" => "assistant",
        "toolcall" | "tool_call" | "function_call" => "tool",
        "toolresult" | "tool_result" | "function_call_output" => "tool_result",
        _ => "unknown",
    }
}

fn status_from_value(value: &Value) -> Option<SessionStatus> {
    let raw = string_field(
        value,
        &["status", "state", "connectionState", "connection_state"],
    )?
    .to_ascii_lowercase();
    match raw.as_str() {
        "idle" | "completed" | "complete" | "done" => Some(SessionStatus::Idle),
        "running" | "in_progress" | "streaming" | "working" => Some(SessionStatus::Running),
        "waiting_for_input" | "needs_input" | "input_required" | "awaiting_input"
        | "requires_input" => Some(SessionStatus::WaitingForInput),
        "waiting_for_approval" | "approval_required" | "waiting_approval" | "needs_approval" => {
            Some(SessionStatus::WaitingForApproval)
        }
        "error" | "failed" | "failure" => Some(SessionStatus::Error),
        _ => None,
    }
}

fn approval_ids_from_value(value: &Value) -> Vec<String> {
    array_strings(
        value,
        &["pendingApprovalIds", "pending_approval_ids", "approvalIds"],
    )
    .unwrap_or_default()
}

fn thread_id_from_value(value: &Value) -> Option<String> {
    string_field(value, &["threadId", "thread_id"])
        .or_else(|| {
            value
                .pointer("/thread/id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            value
                .pointer("/turn/threadId")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            value
                .pointer("/turn/thread_id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            value
                .pointer("/item/threadId")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            value
                .pointer("/item/thread_id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
}

fn event_id(thread_id: &str, value: &Value, suffix: &str) -> String {
    if let Some(id) = string_field(value, &["eventId", "event_id", "id"]) {
        return id;
    }

    let turn_id = string_field(value, &["turnId", "turn_id"])
        .or_else(|| {
            value
                .pointer("/turn/id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| "turn".to_string());
    let item_id = string_field(value, &["itemId", "item_id"])
        .or_else(|| {
            value
                .pointer("/item/id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| "item".to_string());

    format!("{thread_id}:{turn_id}:{item_id}:{suffix}")
}

fn text_from_value(value: &Value) -> Option<String> {
    text_from_value_with_depth(value, 0).filter(|text| !text.trim().is_empty())
}

fn text_from_value_with_depth(value: &Value, depth: usize) -> Option<String> {
    if depth > 6 {
        return None;
    }
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let text = items
                .iter()
                .filter_map(|item| text_from_value_with_depth(item, depth + 1))
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        Value::Object(_) => {
            for key in [
                "text",
                "output_text",
                "input_text",
                "markdown",
                "value",
                "delta",
                "content",
                "message",
                "output",
                "input",
                "parts",
                "payload",
                "item",
                "data",
            ] {
                if let Some(child) = value.get(key) {
                    if let Some(text) = text_from_value_with_depth(child, depth + 1) {
                        return Some(text);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(ToString::to_string)
}

fn timestamp_field(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_u64))
        .map(normalize_timestamp_ms)
}

fn normalize_timestamp_ms(value: u64) -> u64 {
    if value < 100_000_000_000 {
        value.saturating_mul(1_000)
    } else {
        value
    }
}

fn array_strings(value: &Value, keys: &[&str]) -> Option<Vec<String>> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(Value::as_array).map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_thread_list_to_session_snapshots() {
        let thread = CodexThread {
            id: "thread-1".to_string(),
            title: None,
            cwd: Some("/repo".to_string()),
            model_provider: Some("OpenAI".to_string()),
            preview: Some("Build bridge".to_string()),
            created_at: None,
            updated_at: Some(1_725_000_000_000),
            raw: json!({
                "status": "running",
                "pendingApprovalIds": ["approval-1"]
            }),
        };

        let snapshot = Normalizer::snapshot_from_thread(&thread);

        assert_eq!(snapshot.thread_id, "thread-1");
        assert_eq!(snapshot.title, "Build bridge");
        assert_eq!(snapshot.cwd.as_deref(), Some("/repo"));
        assert_eq!(snapshot.model_provider.as_deref(), Some("OpenAI"));
        assert_eq!(snapshot.updated_at, 1_725_000_000_000);
        assert_eq!(snapshot.status, SessionStatus::Running);
        assert_eq!(snapshot.pending_approval_ids, vec!["approval-1"]);
    }

    #[test]
    fn normalizes_assistant_delta_to_message_delta_event() {
        let notification = CodexRawEvent {
            method: "item/agentMessage/delta".to_string(),
            params: json!({
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": "item-1",
                "delta": "hello",
                "createdAt": 1_725_000_000
            }),
        };

        let event = Normalizer::event_from_raw_notification(&notification);

        assert_eq!(event.id, "thread-1:turn-1:item-1:delta");
        assert_eq!(event.thread_id, "thread-1");
        assert_eq!(event.event_type, SessionEventType::MessageDelta);
        assert_eq!(event.payload["role"], json!("assistant"));
        assert_eq!(event.payload["text"], json!("hello"));
        assert_eq!(event.payload["raw"]["delta"], json!("hello"));
        assert_eq!(event.created_at, 1_725_000_000_000);
    }

    #[test]
    fn normalizes_waiting_for_input_status() {
        let thread = CodexThread {
            id: "thread-1".to_string(),
            title: Some("Needs input".to_string()),
            cwd: None,
            model_provider: None,
            preview: None,
            created_at: Some(1_725_000_000_000),
            updated_at: None,
            raw: json!({ "state": "waiting_for_input" }),
        };

        let snapshot = Normalizer::snapshot_from_thread(&thread);

        assert_eq!(snapshot.status, SessionStatus::WaitingForInput);
    }
}
