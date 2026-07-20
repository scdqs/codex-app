use std::path::Path;

use serde_json::{Value, json};

use crate::{
    codex_rpc::{CodexRawEvent, CodexThread, CodexTurn},
    protocol::{SessionEvent, SessionEventType, SessionSnapshot, SessionStatus},
};

pub struct Normalizer;

#[derive(Clone, Copy)]
enum ToolLifecycle {
    Started,
    Completed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolStatus {
    Running,
    Completed,
    Failed,
    Declined,
}

impl ToolStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Declined => "declined",
        }
    }
}

struct ToolActivity {
    kind: &'static str,
    status: ToolStatus,
    title: String,
    detail: Option<String>,
}

impl Normalizer {
    pub fn is_subagent_thread(thread: &CodexThread) -> bool {
        string_field(
            &thread.raw,
            &[
                "parentThreadId",
                "parent_thread_id",
                "parentId",
                "parent_id",
            ],
        )
        .is_some()
            || string_field(&thread.raw, &["threadSource", "thread_source", "source"])
                .is_some_and(|source| source.eq_ignore_ascii_case("subagent"))
            || subagent_title(&thread.raw).is_some()
    }

    pub fn snapshot_from_thread(thread: &CodexThread) -> SessionSnapshot {
        SessionSnapshot {
            thread_id: thread.id.clone(),
            title: thread
                .title
                .clone()
                .filter(|title| !title.trim().is_empty())
                .or_else(|| {
                    thread
                        .preview
                        .clone()
                        .filter(|preview| !preview.trim().is_empty())
                })
                .or_else(|| subagent_title(&thread.raw))
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
            &[
                "createdAt",
                "created_at",
                "startedAtMs",
                "completedAtMs",
                "timestamp",
                "time",
            ],
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
            let text = delta_text_from_value(&notification.params)?;
            if text.trim().is_empty() {
                return None;
            }
            let turn_id = turn_id_from_value(&notification.params);
            return Some(SessionEvent {
                id: item_event_id(&thread_id, &notification.params, "delta"),
                thread_id,
                event_type,
                payload: json!({
                    "role": role,
                    "text": text,
                    "turnId": turn_id,
                    "raw": notification.params,
                }),
                created_at,
            });
        }

        if method == "item/reasoning/summaryPartAdded" {
            let text = summary_part_text(&notification.params)?;
            if text.trim().is_empty() {
                return None;
            }
            return Some(SessionEvent {
                id: item_event_id(&thread_id, &notification.params, "reasoning-summary"),
                thread_id,
                event_type: SessionEventType::ReasoningSummary,
                payload: json!({
                    "role": "reasoning",
                    "text": text,
                    "turnId": turn_id_from_value(&notification.params),
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
            let lifecycle = if method == "item/started" {
                ToolLifecycle::Started
            } else {
                ToolLifecycle::Completed
            };
            return event_from_item(&thread_id, 0, 0, item, &turn, Some(lifecycle));
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
        .filter_map(|(item_index, item)| {
            event_from_item(thread_id, turn_index, item_index, &item, turn, None)
        })
        .collect()
}

fn event_from_item(
    thread_id: &str,
    turn_index: usize,
    item_index: usize,
    item: &Value,
    turn: &CodexTurn,
    lifecycle: Option<ToolLifecycle>,
) -> Option<SessionEvent> {
    let role = role_from_item(item);
    let mut event_type = event_type_for_role(role)?;
    let tool_activity = matches!(
        event_type,
        SessionEventType::ToolCall | SessionEventType::ToolResult
    )
    .then(|| tool_activity_from_item(item, turn, lifecycle));
    if let Some(activity) = tool_activity.as_ref() {
        event_type = if activity.status == ToolStatus::Running {
            SessionEventType::ToolCall
        } else {
            SessionEventType::ToolResult
        };
    }
    let attachments = image_attachments_from_value(item);
    let text = tool_activity
        .as_ref()
        .map(tool_activity_text)
        .or_else(|| text_for_item(item, event_type))
        .map(|text| scrub_attachment_paths_from_text(&text, &attachments))
        .unwrap_or_default();
    let has_attachments = !attachments.is_empty();
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
    let payload_role = if event_type == SessionEventType::ToolResult {
        "tool_result"
    } else {
        role
    };
    let mut payload = json!({
        "role": payload_role,
        "text": text,
        "turnId": turn.id.clone(),
        "raw": item,
    });
    if let Some(activity) = tool_activity
        && let Some(payload) = payload.as_object_mut()
    {
        payload.insert("toolKind".to_string(), json!(activity.kind));
        payload.insert("toolStatus".to_string(), json!(activity.status.as_str()));
        payload.insert("title".to_string(), json!(activity.title));
        if let Some(detail) = activity.detail {
            payload.insert("detail".to_string(), json!(detail));
        }
    }
    if has_attachments {
        if let Some(payload) = payload.as_object_mut() {
            payload.insert("attachments".to_string(), Value::Array(attachments));
        }
    }
    if text.trim().is_empty()
        && !has_attachments
        && (event_type == SessionEventType::ReasoningSummary
            || event_type == SessionEventType::Plan
            || (event_type == SessionEventType::Message && role == "assistant"))
    {
        return None;
    }

    Some(SessionEvent {
        id,
        thread_id: thread_id.to_string(),
        event_type,
        payload,
        created_at,
    })
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

fn event_type_for_role(role: &str) -> Option<SessionEventType> {
    match role {
        "reasoning" => Some(SessionEventType::ReasoningSummary),
        "plan" => Some(SessionEventType::Plan),
        "tool" => Some(SessionEventType::ToolCall),
        "tool_result" => Some(SessionEventType::ToolResult),
        "error" => Some(SessionEventType::Error),
        "context_compaction" | "unknown" => None,
        _ => Some(SessionEventType::Message),
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
        "contextcompaction" | "context_compaction" => "context_compaction",
        "error" | "errorevent" | "error_event" => "error",
        "toolcall"
        | "tool_call"
        | "function_call"
        | "commandexecution"
        | "command_execution"
        | "filechange"
        | "file_change"
        | "mcptoolcall"
        | "mcp_tool_call"
        | "dynamictoolcall"
        | "dynamic_tool_call"
        | "collabagenttoolcall"
        | "collab_agent_tool_call"
        | "subagentactivity"
        | "sub_agent_activity"
        | "websearch"
        | "web_search"
        | "imageview"
        | "image_view"
        | "imagegeneration"
        | "image_generation"
        | "sleep"
        | "enteredreviewmode"
        | "entered_review_mode"
        | "exitedreviewmode"
        | "exited_review_mode" => "tool",
        "toolresult" | "tool_result" | "function_call_output" => "tool_result",
        _ => "unknown",
    }
}

fn tool_activity_from_item(
    item: &Value,
    turn: &CodexTurn,
    lifecycle: Option<ToolLifecycle>,
) -> ToolActivity {
    let status = tool_status_from_item(item, turn, lifecycle);
    let item_type = string_field(item, &["type", "itemType", "item_type"])
        .unwrap_or_default()
        .to_ascii_lowercase();

    match item_type.as_str() {
        "commandexecution" | "command_execution" => command_activity(item, status),
        "filechange" | "file_change" => ToolActivity {
            kind: "file_change",
            status,
            title: activity_title(
                status,
                "Updating files",
                "Updated files",
                "File update failed",
                "Skipped file update",
            ),
            detail: changed_file_detail(item),
        },
        "mcptoolcall" | "mcp_tool_call" => ToolActivity {
            kind: "mcp",
            status,
            title: activity_title(
                status,
                "Using tool",
                "Used tool",
                "Tool failed",
                "Skipped tool",
            ),
            detail: tool_name_detail(item, "server"),
        },
        "dynamictoolcall" | "dynamic_tool_call" => ToolActivity {
            kind: "tool",
            status,
            title: activity_title(
                status,
                "Using tool",
                "Used tool",
                "Tool failed",
                "Skipped tool",
            ),
            detail: tool_name_detail(item, "namespace"),
        },
        "websearch" | "web_search" => ToolActivity {
            kind: "web_search",
            status,
            title: activity_title(
                status,
                "Searching the web",
                "Searched the web",
                "Web search failed",
                "Skipped web search",
            ),
            detail: string_field(item, &["query"])
                .or_else(|| string_field_at(item, &["/action/query"]))
                .and_then(|query| bounded_safe_detail(&query, 180)),
        },
        "imageview" | "image_view" => ToolActivity {
            kind: "image",
            status,
            title: activity_title(
                status,
                "Viewing image",
                "Viewed image",
                "Image view failed",
                "Skipped image view",
            ),
            detail: string_field(item, &["path"]).map(|path| safe_path_label(&path)),
        },
        "imagegeneration" | "image_generation" => ToolActivity {
            kind: "image",
            status,
            title: activity_title(
                status,
                "Generating image",
                "Generated image",
                "Image generation failed",
                "Skipped image generation",
            ),
            detail: None,
        },
        "collabagenttoolcall" | "collab_agent_tool_call" => subagent_activity(item, status),
        "subagentactivity" | "sub_agent_activity" => ToolActivity {
            kind: "subagent",
            status,
            title: activity_title(
                status,
                "Coordinating subtask",
                "Coordinated subtask",
                "Subtask failed",
                "Skipped subtask",
            ),
            detail: string_field(item, &["agentPath", "agent_path"])
                .and_then(|path| agent_task_label(&path)),
        },
        "sleep" => ToolActivity {
            kind: "wait",
            status,
            title: activity_title(status, "Waiting", "Waited", "Wait failed", "Skipped wait"),
            detail: item
                .get("durationMs")
                .or_else(|| item.get("duration_ms"))
                .and_then(Value::as_u64)
                .map(duration_label),
        },
        "enteredreviewmode" | "entered_review_mode" => ToolActivity {
            kind: "review",
            status,
            title: "Entered review mode".to_string(),
            detail: string_field(item, &["review"])
                .and_then(|value| bounded_safe_detail(&value, 180)),
        },
        "exitedreviewmode" | "exited_review_mode" => ToolActivity {
            kind: "review",
            status,
            title: "Exited review mode".to_string(),
            detail: string_field(item, &["review"])
                .and_then(|value| bounded_safe_detail(&value, 180)),
        },
        _ => ToolActivity {
            kind: "tool",
            status,
            title: activity_title(
                status,
                "Working",
                "Finished work",
                "Tool failed",
                "Skipped tool",
            ),
            detail: string_field(item, &["name", "tool"])
                .and_then(|value| humanize_identifier(&value)),
        },
    }
}

fn command_activity(item: &Value, status: ToolStatus) -> ToolActivity {
    let action = item
        .get("commandActions")
        .or_else(|| item.get("command_actions"))
        .and_then(Value::as_array)
        .and_then(|actions| actions.first());
    let action_type = action
        .and_then(|action| string_field(action, &["type"]))
        .unwrap_or_default()
        .to_ascii_lowercase();

    match action_type.as_str() {
        "read" => ToolActivity {
            kind: "read",
            status,
            title: activity_title(
                status,
                "Reading file",
                "Read file",
                "File read failed",
                "Skipped file read",
            ),
            detail: action.and_then(|action| {
                string_field(action, &["name"])
                    .filter(|name| !name.trim().is_empty())
                    .and_then(|name| bounded_safe_detail(&name, 120))
                    .or_else(|| string_field(action, &["path"]).map(|path| safe_path_label(&path)))
            }),
        },
        "listfiles" | "list_files" => ToolActivity {
            kind: "list_files",
            status,
            title: activity_title(
                status,
                "Listing files",
                "Listed files",
                "File listing failed",
                "Skipped file listing",
            ),
            detail: action
                .and_then(|action| string_field(action, &["path"]))
                .map(|path| safe_path_label(&path)),
        },
        "search" => ToolActivity {
            kind: "search",
            status,
            title: activity_title(
                status,
                "Searching files",
                "Searched files",
                "File search failed",
                "Skipped file search",
            ),
            detail: action.and_then(search_action_detail),
        },
        _ => command_fallback_activity(item, action, status),
    }
}

fn command_fallback_activity(
    item: &Value,
    action: Option<&Value>,
    status: ToolStatus,
) -> ToolActivity {
    let command = action
        .and_then(|action| string_field(action, &["command"]))
        .or_else(|| string_field(item, &["command"]))
        .unwrap_or_default();
    let lower = command.to_ascii_lowercase();
    let (kind, running, completed, failed, declined) = if command_runs_tests(&lower) {
        (
            "test",
            "Running tests",
            "Ran tests",
            "Tests failed",
            "Skipped tests",
        )
    } else if command_builds_project(&lower) {
        (
            "build",
            "Building project",
            "Built project",
            "Build failed",
            "Skipped build",
        )
    } else if command_inspects_git(&lower) {
        (
            "git",
            "Inspecting Git changes",
            "Inspected Git changes",
            "Git inspection failed",
            "Skipped Git inspection",
        )
    } else if command_searches_files(&lower) {
        (
            "search",
            "Searching files",
            "Searched files",
            "File search failed",
            "Skipped file search",
        )
    } else if command_reads_files(&lower) {
        (
            "read",
            "Reading files",
            "Read files",
            "File read failed",
            "Skipped file read",
        )
    } else if command_lists_files(&lower) {
        (
            "list_files",
            "Listing files",
            "Listed files",
            "File listing failed",
            "Skipped file listing",
        )
    } else {
        (
            "command",
            "Running command",
            "Ran command",
            "Command failed",
            "Skipped command",
        )
    };

    ToolActivity {
        kind,
        status,
        title: activity_title(status, running, completed, failed, declined),
        detail: None,
    }
}

fn subagent_activity(item: &Value, status: ToolStatus) -> ToolActivity {
    let tool = string_field(item, &["tool"])
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (running, completed) = if tool.contains("spawn") {
        ("Starting subtask", "Started subtask")
    } else if tool.contains("wait") {
        ("Waiting for subtasks", "Finished waiting for subtasks")
    } else if tool.contains("close") || tool.contains("interrupt") {
        ("Stopping subtask", "Stopped subtask")
    } else {
        ("Coordinating subtasks", "Coordinated subtasks")
    };
    ToolActivity {
        kind: "subagent",
        status,
        title: activity_title(
            status,
            running,
            completed,
            "Subtask failed",
            "Skipped subtask",
        ),
        detail: string_field(item, &["prompt"])
            .filter(|prompt| !prompt.trim().is_empty())
            .and_then(|prompt| bounded_safe_detail(&prompt, 180)),
    }
}

fn tool_status_from_item(
    item: &Value,
    turn: &CodexTurn,
    lifecycle: Option<ToolLifecycle>,
) -> ToolStatus {
    let raw_status = string_field(item, &["status", "state"])
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(raw_status.as_str(), "failed" | "failure" | "error")
        || item.get("success").and_then(Value::as_bool) == Some(false)
    {
        return ToolStatus::Failed;
    }
    if matches!(raw_status.as_str(), "declined" | "rejected" | "denied") {
        return ToolStatus::Declined;
    }
    if matches!(lifecycle, Some(ToolLifecycle::Started)) {
        return ToolStatus::Running;
    }
    if matches!(lifecycle, Some(ToolLifecycle::Completed))
        || matches!(
            raw_status.as_str(),
            "completed" | "complete" | "done" | "success"
        )
    {
        return ToolStatus::Completed;
    }
    if matches!(
        raw_status.as_str(),
        "inprogress" | "in_progress" | "running" | "started"
    ) {
        return ToolStatus::Running;
    }

    match status_from_value(&turn.raw) {
        Some(SessionStatus::Running)
        | Some(SessionStatus::WaitingForApproval)
        | Some(SessionStatus::WaitingForInput) => ToolStatus::Running,
        Some(SessionStatus::Error) => ToolStatus::Failed,
        _ => ToolStatus::Completed,
    }
}

fn activity_title(
    status: ToolStatus,
    running: &str,
    completed: &str,
    failed: &str,
    declined: &str,
) -> String {
    match status {
        ToolStatus::Running => running,
        ToolStatus::Completed => completed,
        ToolStatus::Failed => failed,
        ToolStatus::Declined => declined,
    }
    .to_string()
}

fn tool_activity_text(activity: &ToolActivity) -> String {
    match activity.detail.as_deref() {
        Some(detail) if !detail.trim().is_empty() => format!("{}: {detail}", activity.title),
        _ => activity.title.clone(),
    }
}

fn search_action_detail(action: &Value) -> Option<String> {
    let query = string_field(action, &["query"])
        .filter(|query| !query.trim().is_empty())
        .and_then(|query| bounded_safe_detail(&query, 140));
    let path = string_field(action, &["path"])
        .filter(|path| !path.trim().is_empty())
        .map(|path| safe_path_label(&path));
    match (query, path) {
        (Some(query), Some(path)) => Some(format!("{query} in {path}")),
        (Some(query), None) => Some(query),
        (None, Some(path)) => Some(path),
        (None, None) => None,
    }
}

fn changed_file_detail(item: &Value) -> Option<String> {
    let changes = item.get("changes")?.as_array()?;
    let mut labels = Vec::new();
    for change in changes {
        let Some(path) = string_field(change, &["path"]) else {
            continue;
        };
        let label = safe_path_label(&path);
        if !labels.iter().any(|current| current == &label) {
            labels.push(label);
        }
    }
    match labels.len() {
        0 => None,
        1..=3 => Some(labels.join(", ")),
        count => Some(format!("{count} files")),
    }
}

fn tool_name_detail(item: &Value, namespace_key: &str) -> Option<String> {
    let tool = string_field(item, &["tool", "name"])
        .filter(|tool| !tool.trim().is_empty())
        .and_then(|tool| humanize_identifier(&tool));
    let namespace = string_field(item, &[namespace_key])
        .filter(|namespace| !namespace.trim().is_empty())
        .and_then(|namespace| bounded_safe_detail(&namespace, 80));
    match (namespace, tool) {
        (Some(namespace), Some(tool)) => Some(format!("{namespace} · {tool}")),
        (None, Some(tool)) => Some(tool),
        (Some(namespace), None) => Some(namespace),
        (None, None) => None,
    }
}

fn safe_path_label(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(|name| bounded_plain_text(name, 120))
        .unwrap_or_else(|| "workspace".to_string())
}

fn humanize_identifier(value: &str) -> Option<String> {
    bounded_safe_detail(&value.replace(['_', '-'], " "), 120)
}

fn bounded_safe_detail(value: &str, max_chars: usize) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() || contains_absolute_path(&normalized) {
        return None;
    }
    Some(bounded_plain_text(&normalized, max_chars))
}

fn contains_absolute_path(value: &str) -> bool {
    value
        .split(|character: char| {
            character.is_whitespace()
                || matches!(
                    character,
                    '\"' | '\''
                        | '`'
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | '<'
                        | '>'
                        | ','
                        | ';'
                        | '='
                )
        })
        .filter(|candidate| !candidate.is_empty())
        .any(|candidate| {
            Path::new(candidate).is_absolute()
                || candidate
                    .as_bytes()
                    .get(1..3)
                    .is_some_and(|separator| separator == b":\\" || separator == b":/")
                || candidate.starts_with("\\\\")
                || candidate.split_once(':').is_some_and(|(prefix, path)| {
                    !matches!(prefix.to_ascii_lowercase().as_str(), "http" | "https")
                        && Path::new(path).is_absolute()
                })
        })
}

fn bounded_plain_text(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let bounded = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

fn duration_label(duration_ms: u64) -> String {
    if duration_ms < 1_000 {
        format!("{duration_ms} ms")
    } else if duration_ms % 1_000 == 0 {
        format!("{} s", duration_ms / 1_000)
    } else {
        format!("{:.1} s", duration_ms as f64 / 1_000.0)
    }
}

fn command_runs_tests(command: &str) -> bool {
    [
        "cargo test",
        "npm test",
        "pnpm test",
        "yarn test",
        "pytest",
        "vitest",
    ]
    .iter()
    .any(|needle| command.contains(needle))
}

fn command_builds_project(command: &str) -> bool {
    [
        "cargo build",
        "npm run build",
        "pnpm build",
        "yarn build",
        "tauri build",
    ]
    .iter()
    .any(|needle| command.contains(needle))
}

fn command_inspects_git(command: &str) -> bool {
    ["git status", "git diff", "git log", "git show"]
        .iter()
        .any(|needle| command.contains(needle))
}

fn command_searches_files(command: &str) -> bool {
    command.starts_with("rg ")
        || command.contains(" rg ")
        || command.starts_with("grep ")
        || command.contains(" grep ")
}

fn command_reads_files(command: &str) -> bool {
    ["sed ", "cat ", "head ", "tail "]
        .iter()
        .any(|needle| command.starts_with(needle) || command.contains(&format!(" {needle}")))
}

fn command_lists_files(command: &str) -> bool {
    ["find ", "ls "]
        .iter()
        .any(|needle| command.starts_with(needle) || command.contains(&format!(" {needle}")))
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

fn subagent_title(value: &Value) -> Option<String> {
    let agent_path = string_field(value, &["agentPath", "agent_path"]).or_else(|| {
        [
            "/source/subagent/threadSpawn/agentPath",
            "/source/subagent/thread_spawn/agent_path",
            "/source/subAgent/threadSpawn/agentPath",
            "/source/subAgent/thread_spawn/agent_path",
        ]
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(Value::as_str))
        .map(ToString::to_string)
    });
    let agent_nickname = string_field(value, &["agentNickname", "agent_nickname"]).or_else(|| {
        [
            "/source/subagent/threadSpawn/agentNickname",
            "/source/subagent/thread_spawn/agent_nickname",
            "/source/subAgent/threadSpawn/agentNickname",
            "/source/subAgent/thread_spawn/agent_nickname",
        ]
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(Value::as_str))
        .map(ToString::to_string)
    });
    let task_label = agent_path.as_deref().and_then(agent_task_label);
    let nickname = agent_nickname.filter(|nickname| !nickname.trim().is_empty());

    match (task_label, nickname) {
        (Some(task_label), Some(nickname)) => Some(format!("{task_label} · {nickname}")),
        (Some(task_label), None) => Some(task_label),
        (None, Some(nickname)) => Some(format!("Subtask · {nickname}")),
        (None, None) => None,
    }
}

fn agent_task_label(path: &str) -> Option<String> {
    let leaf = path.rsplit('/').find(|part| !part.trim().is_empty())?;
    let mut words = Vec::new();
    let mut current = String::new();
    let mut previous_was_digit: Option<bool> = None;

    for character in leaf.chars() {
        if !character.is_ascii_alphanumeric() {
            push_agent_word(&mut words, &mut current);
            previous_was_digit = None;
            continue;
        }

        let is_digit = character.is_ascii_digit();
        if previous_was_digit.is_some_and(|previous| previous != is_digit) {
            push_agent_word(&mut words, &mut current);
        }
        current.push(character);
        previous_was_digit = Some(is_digit);
    }
    push_agent_word(&mut words, &mut current);

    (!words.is_empty()).then(|| words.join(" "))
}

fn push_agent_word(words: &mut Vec<String>, current: &mut String) {
    if current.is_empty() {
        return;
    }

    let raw = std::mem::take(current);
    let lower = raw.to_ascii_lowercase();
    let display = match lower.as_str() {
        "api" => "API".to_string(),
        "mvp" => "MVP".to_string(),
        "pwa" => "PWA".to_string(),
        "qa" => "QA".to_string(),
        "tdd" => "TDD".to_string(),
        "ui" => "UI".to_string(),
        _ => {
            let mut characters = lower.chars();
            match characters.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + characters.as_str(),
                None => return,
            }
        }
    };
    words.push(display);
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

fn turn_id_from_value(value: &Value) -> Option<String> {
    string_field(value, &["turnId", "turn_id"]).or_else(|| {
        value
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .map(ToString::to_string)
    })
}

fn event_id(thread_id: &str, value: &Value, suffix: &str) -> String {
    if let Some(id) = string_field(value, &["eventId", "event_id", "id"]) {
        return id;
    }

    let turn_id = turn_id_from_value(value).unwrap_or_else(|| "turn".to_string());
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
    let turn_id = turn_id_from_value(value);
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

fn string_field_at(value: &Value, pointers: &[&str]) -> Option<String> {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(Value::as_str))
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
    fn uses_subagent_metadata_when_thread_has_no_title_or_preview() {
        let thread = CodexThread {
            id: "019f75c5-91ec-72c2-b835-4540fc97dd2b".to_string(),
            title: None,
            cwd: Some("/repo".to_string()),
            model_provider: Some("custom".to_string()),
            preview: Some("".to_string()),
            created_at: None,
            updated_at: Some(1_725_000_000_000),
            raw: json!({
                "source": {
                    "subAgent": {
                        "thread_spawn": {
                            "agent_path": "/root/task5_implementer",
                            "agent_nickname": "Darwin"
                        }
                    }
                },
                "thread_source": "subagent",
                "agentNickname": "Darwin"
            }),
        };

        let snapshot = Normalizer::snapshot_from_thread(&thread);

        assert_eq!(snapshot.title, "Task 5 Implementer · Darwin");
        assert!(Normalizer::is_subagent_thread(&thread));
    }

    #[test]
    fn keeps_root_threads_out_of_subagent_classification() {
        let thread = CodexThread {
            id: "thread-root".to_string(),
            title: Some("Main conversation".to_string()),
            cwd: Some("/repo".to_string()),
            model_provider: Some("custom".to_string()),
            preview: Some("Continue the main task".to_string()),
            created_at: None,
            updated_at: Some(1_725_000_000_000),
            raw: json!({
                "id": "thread-root",
                "threadSource": "user"
            }),
        };

        assert!(!Normalizer::is_subagent_thread(&thread));
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
        assert_eq!(event.payload["turnId"], json!("turn-1"));
        assert_eq!(event.payload["raw"]["delta"], json!("hello"));
        assert_eq!(event.created_at, 1_725_000_000_000);
    }

    #[test]
    fn ignores_empty_live_stream_events() {
        for method in [
            "item/agentMessage/delta",
            "item/reasoning/summaryTextDelta",
            "item/plan/delta",
            "item/reasoning/summaryPartAdded",
        ] {
            let event = Normalizer::event_from_raw_notification(&CodexRawEvent {
                method: method.to_string(),
                params: json!({
                    "threadId": "thread-1",
                    "turnId": "turn-1",
                    "itemId": "item-empty",
                    "delta": "",
                    "text": "",
                    "part": { "text": "" }
                }),
            });

            assert!(
                event.is_none(),
                "{method} must not create an empty mobile card"
            );
        }
    }

    #[test]
    fn ignores_empty_in_progress_assistant_reasoning_and_plan_items() {
        for item_type in ["agentMessage", "reasoning", "plan"] {
            let event = Normalizer::event_from_raw_notification(&CodexRawEvent {
                method: "item/started".to_string(),
                params: json!({
                    "threadId": "thread-1",
                    "turnId": "turn-1",
                    "item": {
                        "id": format!("{item_type}-empty"),
                        "type": item_type,
                        "content": []
                    }
                }),
            });

            assert!(
                event.is_none(),
                "empty {item_type} must stay hidden until text arrives"
            );
        }
    }

    #[test]
    fn normalizes_live_file_search_as_a_safe_tool_activity() {
        let started = Normalizer::event_from_raw_notification(&CodexRawEvent {
            method: "item/started".to_string(),
            params: json!({
                "threadId": "thread-1",
                "turnId": "turn-1",
                "item": {
                    "id": "search-1",
                    "type": "commandExecution",
                    "command": "rg --files /Users/damon/Documents/my_ai/codex-manual.md",
                    "cwd": "/Users/damon/Documents/my_ai",
                    "status": "inProgress",
                    "commandActions": [{
                        "type": "search",
                        "command": "rg --files",
                        "query": "codex-manual.md",
                        "path": "/Users/damon/Documents/my_ai"
                    }]
                }
            }),
        })
        .expect("started search is public progress");
        let completed = Normalizer::event_from_raw_notification(&CodexRawEvent {
            method: "item/completed".to_string(),
            params: json!({
                "threadId": "thread-1",
                "turnId": "turn-1",
                "item": {
                    "id": "search-1",
                    "type": "commandExecution",
                    "command": "rg --files /Users/damon/Documents/my_ai/codex-manual.md",
                    "cwd": "/Users/damon/Documents/my_ai",
                    "status": "completed",
                    "commandActions": [{
                        "type": "search",
                        "command": "rg --files",
                        "query": "codex-manual.md",
                        "path": "/Users/damon/Documents/my_ai"
                    }]
                }
            }),
        })
        .expect("completed search is public progress");

        assert_eq!(started.id, "turn-1:search-1");
        assert_eq!(completed.id, started.id);
        assert_eq!(started.event_type, SessionEventType::ToolCall);
        assert_eq!(completed.event_type, SessionEventType::ToolResult);
        assert_eq!(started.payload["toolKind"], json!("search"));
        assert_eq!(started.payload["toolStatus"], json!("running"));
        assert_eq!(started.payload["title"], json!("Searching files"));
        assert_eq!(started.payload["detail"], json!("codex-manual.md in my_ai"));
        assert_eq!(completed.payload["toolStatus"], json!("completed"));
        assert_eq!(completed.payload["title"], json!("Searched files"));
        assert!(
            !started.payload["text"]
                .as_str()
                .unwrap()
                .contains("/Users/damon")
        );
    }

    #[test]
    fn historical_tool_items_keep_meaningful_completed_processes() {
        let turns = vec![CodexTurn {
            id: Some("turn-1".to_string()),
            thread_id: Some("thread-1".to_string()),
            created_at: Some(1_725_000_000_000),
            updated_at: None,
            raw: json!({
                "status": "completed",
                "items": [
                    {
                        "id": "read-1",
                        "type": "commandExecution",
                        "command": "sed -n 1,120p /Users/damon/Documents/my_ai/codex-app/AGENTS.md",
                        "status": "completed",
                        "commandActions": [{
                            "type": "read",
                            "command": "sed",
                            "name": "AGENTS.md",
                            "path": "/Users/damon/Documents/my_ai/codex-app/AGENTS.md"
                        }]
                    },
                    {
                        "id": "edit-1",
                        "type": "fileChange",
                        "status": "completed",
                        "changes": [
                            { "path": "/Users/damon/Documents/my_ai/codex-app/src/App.tsx", "kind": "update", "diff": "large" },
                            { "path": "/Users/damon/Documents/my_ai/codex-app/src/styles.css", "kind": "update", "diff": "large" }
                        ]
                    },
                    {
                        "id": "mcp-1",
                        "type": "mcpToolCall",
                        "server": "nocturne-memory",
                        "tool": "read_memory",
                        "status": "completed",
                        "arguments": { "uri": "system://boot" }
                    },
                    {
                        "id": "web-1",
                        "type": "webSearch",
                        "query": "Codex app-server protocol",
                        "action": { "type": "search", "query": "Codex app-server protocol" }
                    }
                ]
            }),
        }];

        let events = Normalizer::events_from_turns("thread-1", &turns);

        assert_eq!(events.len(), 4);
        assert!(
            events
                .iter()
                .all(|event| event.event_type == SessionEventType::ToolResult)
        );
        assert_eq!(events[0].payload["title"], json!("Read file"));
        assert_eq!(events[0].payload["detail"], json!("AGENTS.md"));
        assert_eq!(events[1].payload["title"], json!("Updated files"));
        assert_eq!(events[1].payload["detail"], json!("App.tsx, styles.css"));
        assert_eq!(events[2].payload["title"], json!("Used tool"));
        assert_eq!(
            events[2].payload["detail"],
            json!("nocturne-memory · read memory")
        );
        assert_eq!(events[3].payload["title"], json!("Searched the web"));
        assert_eq!(
            events[3].payload["detail"],
            json!("Codex app-server protocol")
        );
        assert!(events.iter().all(|event| {
            !event.payload["text"]
                .as_str()
                .unwrap_or_default()
                .contains("/Users/damon")
        }));
    }

    #[test]
    fn free_form_tool_details_do_not_expose_absolute_paths() {
        let turns = vec![CodexTurn {
            id: Some("turn-private-paths".to_string()),
            thread_id: Some("thread-1".to_string()),
            created_at: Some(1_725_000_000_000),
            updated_at: None,
            raw: json!({
                "status": "completed",
                "items": [
                    {
                        "id": "web-1",
                        "type": "webSearch",
                        "query": "inspect /Users/damon/Documents/private-notes.md"
                    },
                    {
                        "id": "read-1",
                        "type": "commandExecution",
                        "commandActions": [{
                            "type": "read",
                            "name": "/Users/damon/Documents/private-notes.md",
                            "path": "/Users/damon/Documents/private-notes.md"
                        }]
                    },
                    {
                        "id": "subagent-1",
                        "type": "collabAgentToolCall",
                        "tool": "spawn_agent",
                        "prompt": "Review /Users/damon/Documents/private-notes.md"
                    },
                    {
                        "id": "review-1",
                        "type": "enteredReviewMode",
                        "review": "Inspect /Users/damon/Documents/private-notes.md"
                    },
                    {
                        "id": "tool-1",
                        "type": "dynamicToolCall",
                        "tool": "/Users/damon/bin/private-tool"
                    },
                    {
                        "id": "web-2",
                        "type": "webSearch",
                        "query": "cwd=/Users/damon/Documents/private-notes.md"
                    },
                    {
                        "id": "web-3",
                        "type": "webSearch",
                        "query": "file:///Users/damon/Documents/private-notes.md"
                    },
                    {
                        "id": "web-4",
                        "type": "webSearch",
                        "query": "https://example.com/docs"
                    }
                ]
            }),
        }];

        let events = Normalizer::events_from_turns("thread-1", &turns);

        assert_eq!(events.len(), 8);
        assert_eq!(events[0].payload.get("detail"), None);
        assert_eq!(events[1].payload["detail"], json!("private-notes.md"));
        assert_eq!(events[2].payload.get("detail"), None);
        assert_eq!(events[3].payload.get("detail"), None);
        assert_eq!(events[4].payload.get("detail"), None);
        assert_eq!(events[5].payload.get("detail"), None);
        assert_eq!(events[6].payload.get("detail"), None);
        assert_eq!(
            events[7].payload["detail"],
            json!("https://example.com/docs")
        );
        assert!(events.iter().all(|event| {
            let mut public_payload = event.payload.clone();
            public_payload
                .as_object_mut()
                .expect("tool payload is an object")
                .remove("raw");
            !serde_json::to_string(&public_payload)
                .expect("payload serializes")
                .contains("/Users/damon")
        }));
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
    fn ignores_live_context_compaction_items_without_emitting_errors() {
        for item_type in ["contextCompaction", "futureProtocolItem"] {
            for method in ["item/started", "item/completed"] {
                let notification = CodexRawEvent {
                    method: method.to_string(),
                    params: json!({
                        "threadId": "thread-1",
                        "turnId": "turn-1",
                        "item": {
                            "id": "item-493",
                            "type": item_type
                        }
                    }),
                };

                assert!(
                    Normalizer::event_from_raw_notification(&notification).is_none(),
                    "{method} {item_type} must not become a mobile error"
                );
            }
        }
    }

    #[test]
    fn keeps_explicit_turn_failures_as_error_status() {
        let event = Normalizer::event_from_raw_notification(&CodexRawEvent {
            method: "turn/failed".to_string(),
            params: json!({
                "threadId": "thread-1",
                "turnId": "turn-1",
                "error": { "message": "model request failed" }
            }),
        })
        .expect("turn failure is public");

        assert_eq!(event.event_type, SessionEventType::StatusChanged);
        assert_eq!(event.payload["status"], json!("error"));
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
    fn ignores_historical_context_compaction_items_without_emitting_errors() {
        let turns = vec![CodexTurn {
            id: Some("turn-1".to_string()),
            thread_id: Some("thread-1".to_string()),
            created_at: Some(1_725_000_000_000),
            updated_at: None,
            raw: json!({
                "items": [
                    {
                        "id": "item-1",
                        "type": "userMessage",
                        "content": "Run the drawer tests"
                    },
                    {
                        "id": "item-493",
                        "type": "contextCompaction"
                    },
                    {
                        "id": "future-item",
                        "type": "futureProtocolItem"
                    },
                    {
                        "id": "item-494",
                        "type": "reasoning",
                        "summary": [{ "type": "summary_text", "text": "Continuing after compaction" }]
                    }
                ]
            }),
        }];

        let events = Normalizer::events_from_turns("thread-1", &turns);

        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .all(|event| event.event_type != SessionEventType::Error)
        );
        assert_eq!(events[0].id, "turn-1:item-1");
        assert_eq!(events[1].id, "turn-1:item-494");
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
