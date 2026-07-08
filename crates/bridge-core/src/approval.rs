use serde_json::Value;

use crate::protocol::{ApprovalKind, ApprovalRequest};

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
}
