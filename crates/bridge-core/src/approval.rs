use serde_json::{Value, json};

use crate::{
    codex_rpc::CodexPendingApproval,
    protocol::{ApprovalKind, ApprovalRequest},
};

pub struct ApprovalDetector;

impl ApprovalDetector {
    pub fn detect(thread_id: &str, raw: &Value, created_at: u64) -> Option<ApprovalRequest> {
        if !looks_like_approval(raw) {
            return None;
        }

        let kind = approval_kind(raw);
        let id = string_field(raw, &["approvalId", "approval_id", "requestId", "id"])
            .unwrap_or_else(|| format!("{thread_id}:approval:{created_at}"));
        let title = string_field(raw, &["title", "name"])
            .or_else(|| title_for_kind(kind).map(ToString::to_string))
            .unwrap_or_else(|| "Approval required".to_string());
        let detail = string_field(
            raw,
            &[
                "detail",
                "command",
                "cmd",
                "path",
                "url",
                "message",
                "summary",
                "description",
            ],
        )
        .unwrap_or_else(|| raw.to_string());
        let risk_hint = string_field(raw, &["riskHint", "risk_hint", "risk", "warning"]);
        let expires_at = number_field(raw, &["expiresAt", "expires_at"]);

        Some(ApprovalRequest {
            id,
            thread_id: thread_id.to_string(),
            kind,
            title,
            detail,
            risk_hint,
            raw: Some(raw.clone()),
            created_at,
            expires_at,
        })
    }

    pub fn detect_pending(
        pending: &CodexPendingApproval,
        created_at: u64,
    ) -> Option<ApprovalRequest> {
        let kind = match pending.method.as_str() {
            "item/commandExecution/requestApproval" => ApprovalKind::Command,
            "item/fileChange/requestApproval" => ApprovalKind::FileEdit,
            "item/permissions/requestApproval" => approval_kind(&pending.params),
            "mcpServer/elicitation/request" => ApprovalKind::Mcp,
            _ => return None,
        };
        let title = match kind {
            ApprovalKind::Command => "Run command".to_string(),
            ApprovalKind::FileEdit => "Apply file edit".to_string(),
            ApprovalKind::Network => "Allow network access".to_string(),
            ApprovalKind::Mcp => mcp_tool_name(&pending.params)
                .map(|tool_name| format!("Allow {tool_name}"))
                .unwrap_or_else(|| "Allow MCP tool".to_string()),
            ApprovalKind::Unknown => "Approval required".to_string(),
        };
        let detail = pending_detail(&pending.method, &pending.params);
        let risk_hint = if kind == ApprovalKind::Mcp {
            string_field(&pending.params, &["serverName", "server_name"])
                .map(|server| format!("MCP server: {server}"))
        } else {
            string_field(
                &pending.params,
                &["riskHint", "risk_hint", "reason", "warning"],
            )
        };
        let raw = json!({
            "requestId": pending.request_id,
            "method": pending.method,
            "params": pending.params,
        });

        Some(ApprovalRequest {
            id: format!("{}:{}", pending.thread_id, pending.request_id),
            thread_id: pending.thread_id.clone(),
            kind,
            title,
            detail,
            risk_hint,
            raw: Some(raw),
            created_at,
            expires_at: number_field(&pending.params, &["expiresAt", "expires_at"]),
        })
    }
}

fn pending_detail(method: &str, params: &Value) -> String {
    match method {
        "item/commandExecution/requestApproval" => command_detail(params),
        "mcpServer/elicitation/request" => mcp_params_detail(params)
            .or_else(|| string_field(params, &["message", "reason"]))
            .unwrap_or_else(|| params.to_string()),
        "item/fileChange/requestApproval" => string_field(
            params,
            &["reason", "grantRoot", "grant_root", "itemId", "item_id"],
        )
        .unwrap_or_else(|| params.to_string()),
        "item/permissions/requestApproval" => string_field(params, &["reason", "message"])
            .or_else(|| params.get("permissions").map(Value::to_string))
            .unwrap_or_else(|| params.to_string()),
        _ => params.to_string(),
    }
}

fn command_detail(params: &Value) -> String {
    if let Some(actions) = params.get("commandActions").and_then(Value::as_array) {
        let commands = actions
            .iter()
            .filter_map(|action| string_field(action, &["cmd", "command"]))
            .collect::<Vec<_>>();
        if !commands.is_empty() {
            return commands.join(" && ");
        }
    }

    string_field(params, &["command", "cmd", "reason"]).unwrap_or_else(|| params.to_string())
}

fn mcp_tool_name(params: &Value) -> Option<String> {
    params
        .pointer("/_meta/tool_title")
        .or_else(|| params.pointer("/_meta/tool_name"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            let message = params.get("message").and_then(Value::as_str)?;
            let start = message.find("tool \"")? + "tool \"".len();
            let end = message[start..].find('"')? + start;
            Some(message[start..end].to_string())
        })
}

fn mcp_params_detail(params: &Value) -> Option<String> {
    if let Some(items) = params
        .pointer("/_meta/tool_params_display")
        .and_then(Value::as_array)
    {
        let details = items
            .iter()
            .filter_map(|item| {
                let name = string_field(item, &["display_name", "displayName", "name"])?;
                let value = item.get("value")?;
                Some(format!("{name}: {}", display_json_value(value)))
            })
            .collect::<Vec<_>>();
        if !details.is_empty() {
            return Some(details.join(", "));
        }
    }

    params
        .pointer("/_meta/tool_params")
        .and_then(Value::as_object)
        .map(|tool_params| {
            tool_params
                .iter()
                .map(|(name, value)| format!("{name}: {}", display_json_value(value)))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|detail| !detail.is_empty())
}

fn display_json_value(value: &Value) -> String {
    value
        .as_str()
        .map(ToString::to_string)
        .unwrap_or_else(|| value.to_string())
}

fn looks_like_approval(raw: &Value) -> bool {
    if string_field(raw, &["approvalId", "approval_id", "riskHint", "risk_hint"]).is_some() {
        return true;
    }

    let haystack = [
        string_field(raw, &["type", "kind", "status", "state", "action"]),
        string_field(raw, &["title", "message", "summary", "description"]),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
    .to_ascii_lowercase();

    haystack.contains("approval")
        || haystack.contains("approve")
        || haystack.contains("confirm")
        || haystack.contains("permission")
}

fn approval_kind(raw: &Value) -> ApprovalKind {
    let haystack = [
        string_field(raw, &["kind", "type", "action", "category"]),
        string_field(
            raw,
            &["title", "detail", "message", "command", "path", "url"],
        ),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
    .to_ascii_lowercase();

    if haystack.contains("command") || haystack.contains("shell") || haystack.contains("exec") {
        ApprovalKind::Command
    } else if haystack.contains("file")
        || haystack.contains("edit")
        || haystack.contains("patch")
        || haystack.contains("write")
    {
        ApprovalKind::FileEdit
    } else if haystack.contains("network")
        || haystack.contains("http")
        || haystack.contains("url")
        || haystack.contains("fetch")
    {
        ApprovalKind::Network
    } else if haystack.contains("mcp") || haystack.contains("tool") {
        ApprovalKind::Mcp
    } else {
        ApprovalKind::Unknown
    }
}

fn title_for_kind(kind: ApprovalKind) -> Option<&'static str> {
    match kind {
        ApprovalKind::Command => Some("Run command"),
        ApprovalKind::FileEdit => Some("Apply file edit"),
        ApprovalKind::Network => Some("Allow network access"),
        ApprovalKind::Mcp => Some("Allow tool call"),
        ApprovalKind::Unknown => None,
    }
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(ToString::to_string)
}

fn number_field(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex_rpc::CodexPendingApproval;
    use serde_json::json;

    #[test]
    fn detects_command_approval_request_from_raw_payload() {
        let raw = json!({
            "approvalId": "approval-1",
            "type": "approval_request",
            "kind": "command",
            "title": "Run cargo test",
            "command": "cargo test -p bridge-core",
            "riskHint": "Executes a local shell command"
        });

        let request = ApprovalDetector::detect("thread-1", &raw, 1_725_000_000_000)
            .expect("approval is detected");

        assert_eq!(request.id, "approval-1");
        assert_eq!(request.thread_id, "thread-1");
        assert_eq!(request.kind, ApprovalKind::Command);
        assert_eq!(request.title, "Run cargo test");
        assert_eq!(request.detail, "cargo test -p bridge-core");
        assert_eq!(
            request.risk_hint.as_deref(),
            Some("Executes a local shell command")
        );
        assert_eq!(request.raw, Some(raw));
    }

    #[test]
    fn detects_mcp_tool_approval_from_desktop_pending_request() {
        let pending = CodexPendingApproval {
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
        };

        let request = ApprovalDetector::detect_pending(&pending, 1_725_000_000_000)
            .expect("MCP approval is detected");

        assert_eq!(request.id, "thread-approval:7");
        assert_eq!(request.thread_id, "thread-approval");
        assert_eq!(request.kind, ApprovalKind::Mcp);
        assert_eq!(request.title, "Allow read_memory");
        assert_eq!(request.detail, "uri: system://boot");
        assert_eq!(request.risk_hint.as_deref(), Some("MCP server: mcpServers"));
        assert_eq!(
            request.raw.as_ref().and_then(|raw| raw.get("method")),
            Some(&json!("mcpServer/elicitation/request"))
        );
    }
}
