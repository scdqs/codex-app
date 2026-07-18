use std::path::Path;

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
                .map(normalize_timestamp_ms)
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

    pub fn event_from_raw_notification(notification: &CodexRawEvent) -> Option<SessionEvent> {
        let thread_id = thread_id_from_value(&notification.params).unwrap_or_default();
        let created_at = timestamp_field(
            &notification.params,
            &["createdAt", "created_at", "timestamp", "time"],
        )
        .unwrap_or_default();
        let method = notification.method.as_str();

        let delta_type = match method {
            "item/agentMessage/delta" => Some((SessionEventType::MessageDelta, "assistant")),
            "item/reasoning/summaryTextDelta" => {
                Some((SessionEventType::ReasoningSummaryDelta, "reasoning"))
            }
            "item/plan/delta" => Some((SessionEventType::PlanDelta, "plan")),
            "item/reasoning/textDelta" => return None,
            _ => None,
        };
        if let Some((event_type, role)) = delta_type {
            let text = delta_text_from_value(&notification.params).unwrap_or_default();
            return Some(SessionEvent {
                id: item_event_id(&thread_id, &notification.params, "delta"),
                thread_id,
                event_type,
                payload: json!({
                    "role": role,
                    "text": text,
                    "raw": notification.params,
                }),
                created_at,
            });
        }

        if method == "item/reasoning/summaryPartAdded" {
            return Some(SessionEvent {
                id: item_event_id(&thread_id, &notification.params, "reasoning-summary"),
                thread_id,
                event_type: SessionEventType::ReasoningSummary,
                payload: json!({
                    "role": "reasoning",
                    "text": summary_part_text(&notification.params).unwrap_or_default(),
                    "raw": notification.params,
                }),
                created_at,
            });
        }

        if matches!(method, "item/started" | "item/completed")
            && let Some(item) = notification.params.get("item")
        {
            let turn = CodexTurn {
                id: string_field(&notification.params, &["turnId", "turn_id"]),
                thread_id: Some(thread_id.clone()),
                created_at: Some(created_at),
                updated_at: Some(created_at),
                raw: json!({ "items": [item] }),
            };
            let mut event = event_from_item(&thread_id, 0, 0, item, &turn);
            if method == "item/completed" && event.event_type == SessionEventType::ToolCall {
                event.event_type = SessionEventType::ToolResult;
            }
            return Some(event);
        }

        if method == "turn/started" {
            return Some(SessionEvent {
                id: event_id(&thread_id, &notification.params, "turn-started"),
                thread_id,
                event_type: SessionEventType::StatusChanged,
                payload: json!({
                    "status": "running",
                    "raw": notification.params,
                }),
                created_at,
            });
        }

        if method == "turn/completed" {
            return Some(status_event(
                thread_id,
                &notification.params,
                "turn-completed",
                SessionStatus::Idle,
                created_at,
            ));
        }

        if method == "turn/failed" {
            return Some(status_event(
                thread_id,
                &notification.params,
                "turn-failed",
                SessionStatus::Error,
                created_at,
            ));
        }

        if matches!(
            method,
            "item/commandExecution/requestApproval"
                | "item/fileChange/requestApproval"
                | "item/permissions/requestApproval"
                | "mcpServer/elicitation/request"
        ) {
            return Some(status_event(
                thread_id,
                &notification.params,
                "approval-required",
                SessionStatus::WaitingForApproval,
                created_at,
            ));
        }

        if method == "item/tool/requestUserInput" {
            return Some(status_event(
                thread_id,
                &notification.params,
                "input-required",
                SessionStatus::WaitingForInput,
                created_at,
            ));
        }

        if let Some(status) = status_from_value(&notification.params) {
            return Some(SessionEvent {
                id: event_id(&thread_id, &notification.params, "status"),
                thread_id,
                event_type: SessionEventType::StatusChanged,
                payload: json!({
                    "status": status,
                    "raw": notification.params,
                }),
                created_at,
            });
        }

        Some(SessionEvent {
            id: event_id(&thread_id, &notification.params, "unknown"),
            thread_id,
            event_type: SessionEventType::StatusChanged,
            payload: json!({
                "method": notification.method,
                "raw": notification.params,
            }),
            created_at,
        })
    }
}

fn status_event(
    thread_id: String,
    params: &Value,
    suffix: &str,
    status: SessionStatus,
    created_at: u64,
) -> SessionEvent {
    SessionEvent {
        id: event_id(&thread_id, params, suffix),
        thread_id,
        event_type: SessionEventType::StatusChanged,
        payload: json!({ "status": status, "raw": params }),
        created_at,
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
    let event_type = event_type_for_role(role);
    let attachments = image_attachments_from_value(item);
    let text = text_for_item(item, event_type)
        .map(|text| scrub_attachment_paths_from_text(&text, &attachments))
        .unwrap_or_default();
    let created_at = timestamp_field(item, &["createdAt", "created_at", "timestamp"])
        .or_else(|| turn.created_at.map(normalize_timestamp_ms))
        .or_else(|| turn.updated_at.map(normalize_timestamp_ms))
        .unwrap_or_default();
    let id = string_field(item, &["id", "itemId", "item_id"])
        .map(|item_id| {
            turn.id
                .as_ref()
                .map(|turn_id| format!("{turn_id}:{item_id}"))
                .unwrap_or(item_id)
        })
        .or_else(|| turn.id.clone().map(|id| format!("{id}:{item_index}")))
        .unwrap_or_else(|| format!("{thread_id}:turn-{turn_index}:item-{item_index}"));
    let mut payload = json!({
        "role": role,
        "text": text,
        "raw": item,
    });
    if !attachments.is_empty() {
        if let Some(payload) = payload.as_object_mut() {
            payload.insert("attachments".to_string(), Value::Array(attachments));
        }
    }

    SessionEvent {
        id,
        thread_id: thread_id.to_string(),
        event_type,
        payload,
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
        "reasoning" => SessionEventType::ReasoningSummary,
        "plan" => SessionEventType::Plan,
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
        "reasoning" | "reasoning_summary" | "reasoningsummary" => "reasoning",
        "plan" | "plan_update" | "planupdate" => "plan",
        "toolcall" | "tool_call" | "function_call" | "commandexecution" | "command_execution"
        | "filechange" | "file_change" | "mcptoolcall" | "mcp_tool_call" | "dynamictoolcall"
        | "dynamic_tool_call" | "websearch" | "web_search" => "tool",
        "toolresult" | "tool_result" | "function_call_output" => "tool_result",
        _ => "unknown",
    }
}

fn text_for_item(item: &Value, event_type: SessionEventType) -> Option<String> {
    match event_type {
        SessionEventType::ReasoningSummary => reasoning_summary_text(item),
        _ => text_from_value(item),
    }
}

fn reasoning_summary_text(item: &Value) -> Option<String> {
    for key in ["summary", "summaryText", "summary_text"] {
        if let Some(summary) = item.get(key)
            && let Some(text) = text_from_value(summary)
        {
            return Some(text);
        }
    }
    None
}

fn delta_text_from_value(value: &Value) -> Option<String> {
    string_field(value, &["delta", "text", "summaryText", "summary_text"]).or_else(|| {
        value
            .pointer("/part/text")
            .and_then(Value::as_str)
            .map(ToString::to_string)
    })
}

fn summary_part_text(value: &Value) -> Option<String> {
    value
        .get("part")
        .or_else(|| value.get("summaryPart"))
        .and_then(text_from_value)
        .or_else(|| delta_text_from_value(value))
}

fn status_from_value(value: &Value) -> Option<SessionStatus> {
    let status = ["status", "state", "connectionState", "connection_state"]
        .iter()
        .find_map(|key| value.get(*key))?;

    if let Some(raw) = status.as_str() {
        return status_from_str(raw);
    }

    let status = status.as_object()?;
    let active_flags = status
        .get("activeFlags")
        .or_else(|| status.get("active_flags"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|flag| flag.to_ascii_lowercase())
        .collect::<Vec<_>>();

    if active_flags.iter().any(|flag| flag == "waitingonapproval") {
        return Some(SessionStatus::WaitingForApproval);
    }
    if active_flags.iter().any(|flag| flag == "waitingonuserinput") {
        return Some(SessionStatus::WaitingForInput);
    }

    status
        .get("type")
        .or_else(|| status.get("status"))
        .and_then(Value::as_str)
        .and_then(status_from_str)
}

fn status_from_str(raw: &str) -> Option<SessionStatus> {
    let raw = raw.to_ascii_lowercase();
    match raw.as_str() {
        "idle" | "completed" | "complete" | "done" => Some(SessionStatus::Idle),
        "active" | "running" | "in_progress" | "streaming" | "working" => {
            Some(SessionStatus::Running)
        }
        "waiting_for_input" | "needs_input" | "input_required" | "awaiting_input"
        | "requires_input" => Some(SessionStatus::WaitingForInput),
        "waiting_for_approval" | "approval_required" | "waiting_approval" | "needs_approval" => {
            Some(SessionStatus::WaitingForApproval)
        }
        "error" | "failed" | "failure" | "systemerror" | "system_error" => {
            Some(SessionStatus::Error)
        }
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

fn item_event_id(thread_id: &str, value: &Value, suffix: &str) -> String {
    let turn_id = string_field(value, &["turnId", "turn_id"]).or_else(|| {
        value
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .map(ToString::to_string)
    });
    let item_id = string_field(value, &["itemId", "item_id"]).or_else(|| {
        value
            .pointer("/item/id")
            .and_then(Value::as_str)
            .map(ToString::to_string)
    });
    match (turn_id, item_id) {
        (Some(turn_id), Some(item_id)) => format!("{turn_id}:{item_id}"),
        _ => event_id(thread_id, value, suffix),
    }
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

fn image_attachments_from_value(value: &Value) -> Vec<Value> {
    let mut attachments = Vec::new();
    collect_image_attachments(value, 0, &mut attachments);
    attachments
}

fn collect_image_attachments(value: &Value, depth: usize, attachments: &mut Vec<Value>) {
    if depth > 6 {
        return;
    }

    match value {
        Value::Array(items) => {
            for item in items {
                collect_image_attachments(item, depth + 1, attachments);
            }
        }
        Value::Object(object) => {
            let is_local_image = object
                .get("type")
                .and_then(Value::as_str)
                .map(|value| value.eq_ignore_ascii_case("localimage"))
                .unwrap_or(false);
            if is_local_image {
                if let Some(path) = object.get("path").and_then(Value::as_str) {
                    attachments.push(json!({
                        "type": "image",
                        "path": path,
                        "name": file_name_from_path(path),
                    }));
                }
                return;
            }

            for child in object.values() {
                collect_image_attachments(child, depth + 1, attachments);
            }
        }
        _ => {}
    }
}

fn file_name_from_path(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "image".to_string())
}

fn scrub_attachment_paths_from_text(text: &str, attachments: &[Value]) -> String {
    let mut scrubbed = text.to_string();
    for attachment in attachments {
        let Some(path) = attachment.get("path").and_then(Value::as_str) else {
            continue;
        };
        if path.is_empty() || !scrubbed.contains(path) {
            continue;
        }
        let name = attachment
            .get("name")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| file_name_from_path(path));
        scrubbed = scrubbed.replace(path, &name);
    }
    scrubbed
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
    fn maps_active_waiting_on_approval_status_object() {
        let thread = CodexThread {
            id: "thread-approval".to_string(),
            title: Some("Approval needed".to_string()),
            cwd: Some("/repo".to_string()),
            model_provider: Some("custom".to_string()),
            preview: None,
            created_at: Some(1_725_000_000),
            updated_at: Some(1_725_000_001),
            raw: json!({
                "status": {
                    "type": "active",
                    "activeFlags": ["waitingOnApproval"]
                }
            }),
        };

        let snapshot = Normalizer::snapshot_from_thread(&thread);

        assert_eq!(snapshot.status, SessionStatus::WaitingForApproval);
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

        let event = Normalizer::event_from_raw_notification(&notification)
            .expect("assistant delta is public");

        assert_eq!(event.id, "turn-1:item-1");
        assert_eq!(event.thread_id, "thread-1");
        assert_eq!(event.event_type, SessionEventType::MessageDelta);
        assert_eq!(event.payload["role"], json!("assistant"));
        assert_eq!(event.payload["text"], json!("hello"));
        assert_eq!(event.payload["raw"]["delta"], json!("hello"));
        assert_eq!(event.created_at, 1_725_000_000_000);
    }

    #[test]
    fn separates_reasoning_summary_and_plan_deltas_without_hidden_reasoning() {
        let summary = Normalizer::event_from_raw_notification(&CodexRawEvent {
            method: "item/reasoning/summaryTextDelta".to_string(),
            params: json!({
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": "reasoning-1",
                "delta": "Checking the implementation"
            }),
        })
        .expect("summary delta is public");
        let plan = Normalizer::event_from_raw_notification(&CodexRawEvent {
            method: "item/plan/delta".to_string(),
            params: json!({
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": "plan-1",
                "delta": "Run focused tests"
            }),
        })
        .expect("plan delta is public");
        let hidden = Normalizer::event_from_raw_notification(&CodexRawEvent {
            method: "item/reasoning/textDelta".to_string(),
            params: json!({
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": "reasoning-1",
                "delta": "private chain of thought"
            }),
        });

        assert_eq!(summary.id, "turn-1:reasoning-1");
        assert_eq!(summary.event_type, SessionEventType::ReasoningSummaryDelta);
        assert_eq!(
            summary.payload["text"],
            json!("Checking the implementation")
        );
        assert_eq!(plan.event_type, SessionEventType::PlanDelta);
        assert_eq!(plan.payload["text"], json!("Run focused tests"));
        assert!(hidden.is_none());
    }

    #[test]
    fn normalizes_historical_reasoning_summary_and_plan_items() {
        let turns = vec![CodexTurn {
            id: Some("turn-1".to_string()),
            thread_id: Some("thread-1".to_string()),
            created_at: Some(1_725_000_000_000),
            updated_at: None,
            raw: json!({
                "items": [
                    {
                        "id": "reasoning-1",
                        "type": "reasoning",
                        "summary": [{ "type": "summary_text", "text": "Reviewed the bridge boundary" }],
                        "content": [{ "type": "reasoning_text", "text": "private chain of thought" }]
                    },
                    {
                        "id": "plan-1",
                        "type": "plan",
                        "content": "Run the regression suite"
                    }
                ]
            }),
        }];

        let events = Normalizer::events_from_turns("thread-1", &turns);

        assert_eq!(events[0].event_type, SessionEventType::ReasoningSummary);
        assert_eq!(
            events[0].payload["text"],
            json!("Reviewed the bridge boundary")
        );
        assert!(
            !events[0].payload["text"]
                .as_str()
                .unwrap()
                .contains("private")
        );
        assert_eq!(events[1].event_type, SessionEventType::Plan);
        assert_eq!(events[1].payload["text"], json!("Run the regression suite"));
    }

    #[test]
    fn prefixes_turn_id_to_turn_item_event_ids() {
        let turns = vec![
            CodexTurn {
                id: Some("turn-new".to_string()),
                thread_id: Some("thread-1".to_string()),
                created_at: Some(1_725_000_000_000),
                updated_at: None,
                raw: json!({
                    "items": [
                        { "id": "item-1", "type": "userMessage", "content": "new prompt" }
                    ]
                }),
            },
            CodexTurn {
                id: Some("turn-old".to_string()),
                thread_id: Some("thread-1".to_string()),
                created_at: Some(1_724_999_000_000),
                updated_at: None,
                raw: json!({
                    "items": [
                        { "id": "item-1", "type": "userMessage", "content": "old prompt" }
                    ]
                }),
            },
        ];

        let events = Normalizer::events_from_turns("thread-1", &turns);

        assert_eq!(events[0].id, "turn-new:item-1");
        assert_eq!(events[1].id, "turn-old:item-1");
    }

    #[test]
    fn normalizes_second_precision_turn_timestamps_to_milliseconds() {
        let turns = vec![CodexTurn {
            id: Some("turn-seconds".to_string()),
            thread_id: Some("thread-1".to_string()),
            created_at: Some(1_783_574_153),
            updated_at: None,
            raw: json!({
                "items": [
                    { "id": "item-1", "type": "assistantMessage", "content": "hello from seconds" }
                ]
            }),
        }];

        let events = Normalizer::events_from_turns("thread-1", &turns);

        assert_eq!(events[0].created_at, 1_783_574_153_000);
    }

    #[test]
    fn normalizes_second_precision_thread_timestamps_to_milliseconds() {
        let thread = CodexThread {
            id: "thread-seconds".to_string(),
            title: Some("Thread seconds".to_string()),
            cwd: None,
            model_provider: None,
            preview: None,
            created_at: Some(1_783_574_153),
            updated_at: None,
            raw: json!({}),
        };

        let snapshot = Normalizer::snapshot_from_thread(&thread);

        assert_eq!(snapshot.updated_at, 1_783_574_153_000);
    }

    #[test]
    fn normalizes_local_image_parts_to_internal_attachments() {
        let turns = vec![CodexTurn {
            id: Some("turn-image".to_string()),
            thread_id: Some("thread-1".to_string()),
            created_at: Some(1_725_000_000_000),
            updated_at: None,
            raw: json!({
                "items": [
                    {
                        "id": "item-1",
                        "type": "userMessage",
                        "content": [
                            { "type": "input_text", "text": "look at this" },
                            {
                                "type": "localImage",
                                "path": "/var/folders/codex-clipboard.png",
                                "detail": null
                            }
                        ]
                    }
                ]
            }),
        }];

        let events = Normalizer::events_from_turns("thread-1", &turns);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["text"], json!("look at this"));
        assert_eq!(
            events[0].payload["attachments"],
            json!([
                {
                    "type": "image",
                    "path": "/var/folders/codex-clipboard.png",
                    "name": "codex-clipboard.png"
                }
            ])
        );
    }

    #[test]
    fn scrubs_local_image_paths_from_display_text() {
        let turns = vec![CodexTurn {
            id: Some("turn-image".to_string()),
            thread_id: Some("thread-1".to_string()),
            created_at: Some(1_725_000_000_000),
            updated_at: None,
            raw: json!({
                "items": [
                    {
                        "id": "item-1",
                        "type": "userMessage",
                        "content": [
                            {
                                "type": "input_text",
                                "text": "attached image: /var/folders/codex-clipboard.png"
                            },
                            {
                                "type": "localImage",
                                "path": "/var/folders/codex-clipboard.png",
                                "detail": null
                            }
                        ]
                    }
                ]
            }),
        }];

        let events = Normalizer::events_from_turns("thread-1", &turns);

        assert_eq!(
            events[0].payload["text"],
            json!("attached image: codex-clipboard.png")
        );
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
