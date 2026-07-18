use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

const REDACTED_SECRET: &str = "[REDACTED]";
const REDACTED_PATH: &str = "[LOCAL_PATH]";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsBundle {
    pub generated_at: u64,
    pub app_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidecar_version: Option<String>,
    pub codex_adapter: DiagnosticCheck,
    pub bridge: DiagnosticCheck,
    pub tunnel: DiagnosticCheck,
    pub recent_connection_states: Vec<String>,
    pub logs: Vec<DiagnosticLog>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticCheck {
    pub status: DiagnosticStatus,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStatus {
    Ok,
    Degraded,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticLog {
    pub source: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct DiagnosticsBundleInput {
    pub app_version: String,
    pub sidecar_version: Option<String>,
    pub codex_adapter: DiagnosticCheck,
    pub bridge: DiagnosticCheck,
    pub tunnel: DiagnosticCheck,
    pub recent_connection_states: Vec<String>,
    pub logs: Vec<DiagnosticLog>,
}

pub fn build_diagnostics_bundle(input: DiagnosticsBundleInput) -> DiagnosticsBundle {
    DiagnosticsBundle {
        generated_at: current_time_ms(),
        app_version: input.app_version,
        sidecar_version: input.sidecar_version,
        codex_adapter: input.codex_adapter.redacted(),
        bridge: input.bridge.redacted(),
        tunnel: input.tunnel.redacted(),
        recent_connection_states: input
            .recent_connection_states
            .into_iter()
            .map(|state| redact_sensitive_text(&state))
            .collect(),
        logs: input
            .logs
            .into_iter()
            .map(|log| DiagnosticLog {
                source: log.source,
                text: redact_sensitive_text(&log.text),
            })
            .collect(),
    }
}

impl DiagnosticCheck {
    pub fn ok(label: impl Into<String>) -> Self {
        Self {
            status: DiagnosticStatus::Ok,
            label: label.into(),
            detail: None,
        }
    }

    pub fn degraded(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            status: DiagnosticStatus::Degraded,
            label: label.into(),
            detail: Some(detail.into()),
        }
    }

    pub fn failed(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            status: DiagnosticStatus::Failed,
            label: label.into(),
            detail: Some(detail.into()),
        }
    }

    pub fn unknown(label: impl Into<String>) -> Self {
        Self {
            status: DiagnosticStatus::Unknown,
            label: label.into(),
            detail: None,
        }
    }

    fn redacted(self) -> Self {
        Self {
            label: redact_sensitive_text(&self.label),
            detail: self.detail.map(|detail| redact_sensitive_text(&detail)),
            ..self
        }
    }
}

pub fn redact_sensitive_text(text: &str) -> String {
    let mut redacted = text.to_string();
    redacted = redact_header_value(&redacted, "authorization");
    redacted = redact_header_value(&redacted, "x-bridge-control-token");
    redacted = redact_assignment_value(&redacted, "CODEX_MOBILE_BRIDGE_CONTROL_TOKEN");
    redacted = redact_assignment_value(&redacted, "CLOUDFLARE_TUNNEL_TOKEN");
    redacted = redact_assignment_value(&redacted, "CLOUDFLARE_TUNNEL_TOKEN_FILE_CONTENTS");
    redacted = redact_assignment_value(&redacted, "TUNNEL_TOKEN");
    redacted = redact_assignment_value(&redacted, "token_file_contents");
    redacted = redact_assignment_value(&redacted, "VAPID_PRIVATE_KEY");
    redacted = redact_assignment_value(&redacted, "VAPID_SECRET");
    redacted = redact_assignment_value(&redacted, "vapid-private-key");
    redacted = redact_after_marker_with_replacement(
        &redacted,
        "CODEX_MOBILE_BRIDGE_VAPID_KEY_FILE=",
        false,
        REDACTED_PATH,
    );
    redacted = redact_assignment_value(&redacted, "OPENAI_API_KEY");
    redacted = redact_assignment_value(&redacted, "ANTHROPIC_API_KEY");
    redacted = redact_after_marker(&redacted, "--token ", false);
    redacted = redact_after_marker(&redacted, "--token=", false);
    redacted =
        redact_after_marker_with_replacement(&redacted, "--token-file ", false, REDACTED_PATH);
    redacted =
        redact_after_marker_with_replacement(&redacted, "--token-file=", false, REDACTED_PATH);
    redacted = redact_assignment_value(&redacted, "p256dh");
    redacted = redact_assignment_value(&redacted, "auth");
    redacted = redact_after_marker(&redacted, "\"p256dh\":\"", false);
    redacted = redact_after_marker(&redacted, "\"p256dh\": \"", false);
    redacted = redact_after_marker(&redacted, "\"auth\":\"", false);
    redacted = redact_after_marker(&redacted, "\"auth\": \"", false);
    redacted = redact_push_endpoints(&redacted);
    redacted = redact_api_key_like_tokens(&redacted);
    redacted = redact_uuid_like_tokens(&redacted);
    redact_local_paths(&redacted)
}

fn redact_push_endpoints(text: &str) -> String {
    let mut redacted = text.to_string();
    for (marker, quoted) in [
        ("endpoint=", false),
        ("\"endpoint\":\"", true),
        ("\"endpoint\": \"", true),
    ] {
        redacted = redact_endpoint_after_marker(&redacted, marker, quoted);
    }
    redacted
}

fn redact_endpoint_after_marker(text: &str, marker: &str, quoted: bool) -> String {
    let mut output = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(index) = remaining.find(marker) {
        let value_start = index + marker.len();
        output.push_str(&remaining[..value_start]);
        let value = &remaining[value_start..];
        let value_end = if quoted {
            value.find('"').unwrap_or(value.len())
        } else {
            value.find(char::is_whitespace).unwrap_or(value.len())
        };
        let endpoint = &value[..value_end];
        let host = url::Url::parse(endpoint)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
            .unwrap_or_else(|| "invalid-endpoint".to_string());
        output.push_str("https://");
        output.push_str(&host);
        output.push_str("/[REDACTED]");
        remaining = &value[value_end..];
    }
    output.push_str(remaining);
    output
}

fn redact_header_value(text: &str, header_name: &str) -> String {
    redact_after_marker(text, &format!("{header_name}:"), true)
}

fn redact_assignment_value(text: &str, key: &str) -> String {
    redact_after_marker(text, &format!("{key}="), false)
}

fn redact_after_marker(text: &str, marker: &str, case_insensitive: bool) -> String {
    redact_after_marker_with_replacement(text, marker, case_insensitive, REDACTED_SECRET)
}

fn redact_after_marker_with_replacement(
    text: &str,
    marker: &str,
    case_insensitive: bool,
    replacement: &str,
) -> String {
    let mut output = String::with_capacity(text.len());
    for line in text.lines() {
        let haystack = if case_insensitive {
            line.to_ascii_lowercase()
        } else {
            line.to_string()
        };
        let needle = if case_insensitive {
            marker.to_ascii_lowercase()
        } else {
            marker.to_string()
        };
        if let Some(index) = haystack.find(&needle) {
            let value_start = index + marker.len();
            output.push_str(&line[..value_start]);
            output.push(' ');
            output.push_str(replacement);
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    if !text.ends_with('\n') {
        output.pop();
    }
    output
}

fn redact_api_key_like_tokens(text: &str) -> String {
    redact_matching_tokens(text, |token| {
        token.starts_with("sk-")
            || token.starts_with("sk-ant-")
            || token.starts_with("ghp_")
            || token.starts_with("gho_")
    })
}

fn redact_uuid_like_tokens(text: &str) -> String {
    redact_matching_tokens(text, is_uuid_like)
}

fn redact_matching_tokens(text: &str, is_secret: impl Fn(&str) -> bool) -> String {
    let mut output = String::with_capacity(text.len());
    let mut token = String::new();

    for character in text.chars() {
        if is_token_character(character) {
            token.push(character);
            continue;
        }

        flush_token(&mut output, &mut token, &is_secret);
        output.push(character);
    }
    flush_token(&mut output, &mut token, &is_secret);
    output
}

fn flush_token(output: &mut String, token: &mut String, is_secret: &impl Fn(&str) -> bool) {
    if token.is_empty() {
        return;
    }
    if is_secret(token) {
        output.push_str(REDACTED_SECRET);
    } else {
        output.push_str(token);
    }
    token.clear();
}

fn is_token_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
}

fn is_uuid_like(token: &str) -> bool {
    let parts = token.split('-').collect::<Vec<_>>();
    if parts.len() != 5 {
        return false;
    }
    let lengths = [8, 4, 4, 4, 12];
    parts.iter().zip(lengths).all(|(part, length)| {
        part.len() == length && part.chars().all(|char| char.is_ascii_hexdigit())
    })
}

fn redact_local_paths(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let remainder = &text[index..];
        let Some(path_start_offset) = first_local_path_offset(remainder) else {
            output.push_str(remainder);
            break;
        };
        let path_start = index + path_start_offset;
        output.push_str(&text[index..path_start]);
        let path_end = path_start + local_path_len(&text[path_start..]);
        output.push_str(REDACTED_PATH);
        index = path_end;
    }
    output
}

fn first_local_path_offset(text: &str) -> Option<usize> {
    ["/Users/", "/var/folders/", "/private/var/folders/"]
        .into_iter()
        .filter_map(|prefix| text.find(prefix))
        .min()
}

fn local_path_len(text: &str) -> usize {
    text.char_indices()
        .find_map(|(index, character)| (index > 0 && is_path_boundary(character)).then_some(index))
        .unwrap_or(text.len())
}

fn is_path_boundary(character: char) -> bool {
    character.is_whitespace()
        || matches!(
            character,
            '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';'
        )
}

fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time is after unix epoch")
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_auth_headers_and_control_tokens() {
        let input = concat!(
            "Authorization: Bearer session-token-value\n",
            "x-bridge-control-token: control-token-value\n",
            "CODEX_MOBILE_BRIDGE_CONTROL_TOKEN=control-token-value"
        );

        let redacted = redact_sensitive_text(input);

        assert!(redacted.contains("Authorization: [REDACTED]"));
        assert!(redacted.contains("x-bridge-control-token: [REDACTED]"));
        assert!(redacted.contains("CODEX_MOBILE_BRIDGE_CONTROL_TOKEN= [REDACTED]"));
        assert!(!redacted.contains("session-token-value"));
        assert!(!redacted.contains("control-token-value"));
    }

    #[test]
    fn redacts_cloudflare_tunnel_credentials_and_secret_file_paths() {
        let input = concat!(
            "CLOUDFLARE_TUNNEL_TOKEN=eyJhIjoiMTIzIn0.long-secret\n",
            "TUNNEL_TOKEN=standard-cloudflare-token\n",
            "token_file_contents=token-file-secret\n",
            "cloudflared tunnel run --token-file /Users/damon/token-file\n",
            "cloudflared tunnel run --token direct-token-value\n",
            "cloudflared tunnel run --token-file=/tmp/token-file --url http://localhost:57324"
        );

        let redacted = redact_sensitive_text(input);

        assert!(!redacted.contains("eyJhIjoiMTIzIn0.long-secret"));
        assert!(!redacted.contains("standard-cloudflare-token"));
        assert!(!redacted.contains("token-file-secret"));
        assert!(!redacted.contains("/Users/damon/token-file"));
        assert!(!redacted.contains("direct-token-value"));
        assert!(!redacted.contains("/tmp/token-file"));
        assert!(redacted.contains("[REDACTED]"));
        assert!(redacted.contains("[LOCAL_PATH]"));
    }

    #[test]
    fn redacts_push_notification_credentials() {
        let input = concat!(
            "VAPID_PRIVATE_KEY=vapid-private-secret\n",
            "p256dh=push-public-key auth=push-auth-secret\n",
            "{\"keys\":{\"p256dh\":\"json-public-key\",\"auth\":\"json-auth-secret\"}}"
        );

        let redacted = redact_sensitive_text(input);

        for secret in [
            "vapid-private-secret",
            "push-public-key",
            "push-auth-secret",
            "json-public-key",
            "json-auth-secret",
        ] {
            assert!(!redacted.contains(secret));
        }
        assert!(redacted.contains(REDACTED_SECRET));
    }

    #[test]
    fn redacts_vapid_and_subscription_material() {
        let input = concat!(
            "CODEX_MOBILE_BRIDGE_VAPID_KEY_FILE=/Users/damon/vapid-secret\n",
            "VAPID_PRIVATE_KEY=private-base64-value\n",
            "p256dh=public-client-key auth=client-auth-secret\n",
            "endpoint=https://fcm.googleapis.com/fcm/send/private-path?token=private-query"
        );

        let redacted = redact_sensitive_text(input);

        for secret in [
            "private-base64-value",
            "public-client-key",
            "client-auth-secret",
            "private-path",
            "private-query",
        ] {
            assert!(!redacted.contains(secret));
        }
        assert!(redacted.contains("fcm.googleapis.com"));
    }

    #[test]
    fn redacts_api_keys_and_uuid_tokens() {
        let input = concat!(
            "OPENAI_API_KEY=sk-test1234567890abcdef\n",
            "anthropic=sk-ant-api03-1234567890abcdef\n",
            "pairing=46c84976-74e0-46bd-b193-dfcb41dba342"
        );

        let redacted = redact_sensitive_text(input);

        assert!(!redacted.contains("sk-test1234567890abcdef"));
        assert!(!redacted.contains("sk-ant-api03-1234567890abcdef"));
        assert!(!redacted.contains("46c84976-74e0-46bd-b193-dfcb41dba342"));
        assert_eq!(redacted.matches(REDACTED_SECRET).count(), 3);
    }

    #[test]
    fn redacts_local_paths_without_hiding_file_context() {
        let input =
            "asset=/Users/damon/Documents/my_ai/codex-app/file.png log=/var/folders/yl/token.log";

        let redacted = redact_sensitive_text(input);

        assert_eq!(redacted, "asset=[LOCAL_PATH] log=[LOCAL_PATH]");
    }

    #[test]
    fn diagnostics_bundle_redacts_logs_and_details() {
        let bundle = build_diagnostics_bundle(DiagnosticsBundleInput {
            app_version: "0.1.0".to_string(),
            sidecar_version: Some("0.1.0".to_string()),
            codex_adapter: DiagnosticCheck::degraded(
                "inject failed",
                "path=/Users/damon/Codex token=46c84976-74e0-46bd-b193-dfcb41dba342",
            ),
            bridge: DiagnosticCheck::ok("ready"),
            tunnel: DiagnosticCheck::unknown("not enabled"),
            recent_connection_states: vec!["Authorization: Bearer session-token".to_string()],
            logs: vec![DiagnosticLog {
                source: "sidecar".to_string(),
                text: "OPENAI_API_KEY=sk-test1234567890abcdef".to_string(),
            }],
        });

        assert_eq!(bundle.codex_adapter.status, DiagnosticStatus::Degraded);
        assert_eq!(
            bundle.codex_adapter.detail.as_deref(),
            Some("path=[LOCAL_PATH] token=[REDACTED]")
        );
        assert_eq!(
            bundle.recent_connection_states,
            vec!["Authorization: [REDACTED]"]
        );
        assert_eq!(bundle.logs[0].text, "OPENAI_API_KEY= [REDACTED]");
    }
}
