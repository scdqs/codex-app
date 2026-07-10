use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_millis(1_500);

#[derive(Debug, Clone)]
pub struct CdpClient {
    base_url: String,
    http: reqwest::Client,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CdpTarget {
    pub id: String,
    #[serde(rename = "type")]
    pub target_type: String,
    pub title: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_socket_debugger_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devtools_frontend_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeHealth {
    pub status: BridgeHealthStatus,
    pub connection_state: BridgeConnectionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeHealthStatus {
    Ok,
    Degraded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeConnectionState {
    Connected,
    CodexNotRunning,
    CdpUnavailable,
    TargetNotFound,
    InjectFailed,
    RpcUnavailable,
    ReadOnly,
    Writable,
}

#[derive(Debug, Error)]
pub enum CdpError {
    #[error("cdp http request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("cdp websocket failed: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("cdp json failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("codex page target not found")]
    NoCodexTarget,
    #[error("target {target_id} does not expose webSocketDebuggerUrl")]
    MissingWebSocketUrl { target_id: String },
    #[error("cdp websocket closed before evaluation response")]
    WebSocketClosed,
    #[error("cdp protocol error {code}: {message}")]
    Protocol { code: i64, message: String },
    #[error("runtime evaluation failed: {0}")]
    RuntimeException(String),
    #[error("malformed cdp response: {0}")]
    MalformedResponse(&'static str),
    #[error("codex app-server bridge injection failed")]
    InjectFailed,
}

impl CdpClient {
    pub fn new(debug_port: u16) -> Result<Self, CdpError> {
        Self::with_base_url(format!("http://127.0.0.1:{debug_port}"))
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Result<Self, CdpError> {
        let http = reqwest::Client::builder()
            .timeout(DEFAULT_HTTP_TIMEOUT)
            .build()?;

        Ok(Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http,
        })
    }

    pub async fn list_targets(&self) -> Result<Vec<CdpTarget>, CdpError> {
        let url = format!("{}/json/list", self.base_url);
        Ok(self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub async fn evaluate_on_target(
        &self,
        target: &CdpTarget,
        expression: &str,
    ) -> Result<Value, CdpError> {
        let websocket_url = target.web_socket_debugger_url.as_deref().ok_or_else(|| {
            CdpError::MissingWebSocketUrl {
                target_id: target.id.clone(),
            }
        })?;
        let (mut socket, _response) = connect_async(websocket_url).await?;
        let request_id = 1_u64;
        let request = json!({
            "id": request_id,
            "method": "Runtime.evaluate",
            "params": {
                "expression": expression,
                "awaitPromise": true,
                "returnByValue": true,
            },
        });

        socket.send(Message::Text(request.to_string())).await?;

        while let Some(message) = socket.next().await {
            let message = message?;
            let Message::Text(text) = message else {
                continue;
            };
            if let Some(value) = evaluate_response_value(request_id, &text)? {
                return Ok(value);
            }
        }

        Err(CdpError::WebSocketClosed)
    }

    pub async fn bridge_health(&self) -> BridgeHealth {
        match self.list_targets().await {
            Ok(targets) => bridge_health_from_targets(&targets),
            Err(error) => BridgeHealth::degraded(
                BridgeConnectionState::CdpUnavailable,
                Some(error.to_string()),
            ),
        }
    }

    pub async fn inject_app_server_bridge(&self, target: &CdpTarget) -> Result<(), CdpError> {
        let injected = self
            .evaluate_on_target(target, CODEX_APP_SERVER_BRIDGE_SCRIPT)
            .await?;
        if injected.as_bool() == Some(true) {
            Ok(())
        } else {
            Err(CdpError::InjectFailed)
        }
    }
}

pub fn select_codex_target(targets: &[CdpTarget]) -> Result<CdpTarget, CdpError> {
    targets
        .iter()
        .filter(|target| target.target_type.eq_ignore_ascii_case("page"))
        .filter_map(|target| codex_target_score(target).map(|score| (target, score)))
        .max_by_key(|(_target, score)| *score)
        .map(|(target, _score)| target.clone())
        .ok_or(CdpError::NoCodexTarget)
}

impl BridgeHealth {
    fn connected(target: CdpTarget) -> Self {
        Self {
            status: BridgeHealthStatus::Ok,
            connection_state: BridgeConnectionState::Connected,
            target_id: Some(target.id),
            target_title: Some(target.title),
            reason: None,
        }
    }

    fn degraded(connection_state: BridgeConnectionState, reason: Option<String>) -> Self {
        Self {
            status: BridgeHealthStatus::Degraded,
            connection_state,
            target_id: None,
            target_title: None,
            reason,
        }
    }
}

impl BridgeHealthStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "degraded",
        }
    }
}

impl BridgeConnectionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::CodexNotRunning => "codex_not_running",
            Self::CdpUnavailable => "cdp_unavailable",
            Self::TargetNotFound => "target_not_found",
            Self::InjectFailed => "inject_failed",
            Self::RpcUnavailable => "rpc_unavailable",
            Self::ReadOnly => "read_only",
            Self::Writable => "writable",
        }
    }
}

const CODEX_APP_SERVER_BRIDGE_SCRIPT: &str = r#"
(async () => {
  if (globalThis.__codexMobileBridge && typeof globalThis.__codexMobileBridge.rpc === "function") {
    return true;
  }

  const installDirectClientBridge = (client) => {
    if (!client || typeof client.sendRequest !== "function") {
      return false;
    }
    globalThis.__codexMobileBridge = {
      mode: "direct-client",
      rpc: async (request) => client.sendRequest(request.method, request.params || {}),
    };
    return true;
  };

  const installHostBridge = (sendRequest) => {
    if (typeof sendRequest !== "function") {
      return false;
    }
    globalThis.__codexMobileBridge = {
      mode: "host-module",
      rpc: async (request) => {
        if (!request || typeof request.method !== "string") {
          throw new Error("Invalid Codex mobile bridge request");
        }
        return await sendRequest("send-cli-request-for-host", {
          hostId: "local",
          method: request.method,
          params: request.params || {},
          timeoutMs: request.method === "turn/start" ? 60000 : 30000,
        });
      },
    };
    return true;
  };

  const candidates = [
    globalThis.__codexAppServerClient,
    globalThis.__codex?.appServerClient,
    globalThis.codex?.appServerClient,
    globalThis.appServerClient,
  ];
  const client = candidates.find((candidate) => candidate && typeof candidate.sendRequest === "function");
  if (installDirectClientBridge(client)) {
    return true;
  }

  const moduleUrls = Array.from(document.querySelectorAll('link[rel="modulepreload"], script[type="module"], script[src]'))
    .map((element) => element.href || element.src)
    .filter(Boolean);
  const hostModuleUrl =
    moduleUrls.find((url) => url.includes("new-thread-panel-page")) ||
    moduleUrls.find((url) => url.includes("app-server-manager-signals"));
  if (!hostModuleUrl) {
    return false;
  }

  try {
    const hostModule = await import(hostModuleUrl);
    return installHostBridge(hostModule.pv);
  } catch (error) {
    globalThis.__codexMobileBridgeLastError = error?.message || String(error);
    return false;
  }
})()
"#;

fn bridge_health_from_targets(targets: &[CdpTarget]) -> BridgeHealth {
    match select_codex_target(targets) {
        Ok(target) => BridgeHealth::connected(target),
        Err(CdpError::NoCodexTarget) => BridgeHealth::degraded(
            BridgeConnectionState::TargetNotFound,
            Some("No ChatGPT/Codex page target found on the CDP endpoint".to_string()),
        ),
        Err(error) => BridgeHealth::degraded(
            BridgeConnectionState::CdpUnavailable,
            Some(error.to_string()),
        ),
    }
}

fn codex_target_score(target: &CdpTarget) -> Option<u8> {
    let title = target.title.to_ascii_lowercase();
    let url = target.url.to_ascii_lowercase();
    let devtools_url = target
        .devtools_frontend_url
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut score = 0_u8;
    let mut matched_codex = false;

    if title.contains("codex mobile")
        || title.contains("chatgpt classic")
        || url.contains("chatgpt classic.app")
        || url.contains("chatgpt%20classic.app")
    {
        return None;
    }
    if url == "app://-/index.html" {
        score = score.saturating_add(80);
        matched_codex = true;
    } else if url.starts_with("app://-") {
        score = score.saturating_add(60);
        matched_codex = true;
    }
    if url.contains("codex.app") {
        score = score.saturating_add(50);
        matched_codex = true;
    }
    if url.contains("chatgpt.app") {
        score = score.saturating_add(50);
        matched_codex = true;
    }
    if title == "codex" {
        score = score.saturating_add(30);
        matched_codex = true;
    } else if title == "chatgpt" {
        score = score.saturating_add(30);
        matched_codex = true;
    } else if title.contains("codex") {
        score = score.saturating_add(20);
        matched_codex = true;
    } else if title.contains("chatgpt") {
        score = score.saturating_add(20);
        matched_codex = true;
    }
    if url.contains("codex") {
        score = score.saturating_add(15);
        matched_codex = true;
    }
    if url.contains("chatgpt") {
        score = score.saturating_add(15);
        matched_codex = true;
    }
    if !matched_codex {
        return None;
    }
    if title.contains("chatgpt") && (url.contains("codex") || url.contains("chatgpt")) {
        score = score.saturating_add(5);
    }
    if target.web_socket_debugger_url.is_some() {
        score = score.saturating_add(1);
    }
    if devtools_url.contains("devtools") && score > 0 {
        score = score.saturating_sub(1);
    }

    Some(score)
}

fn evaluate_response_value(request_id: u64, text: &str) -> Result<Option<Value>, CdpError> {
    let value: Value = serde_json::from_str(text)?;
    if value.get("id").and_then(Value::as_u64) != Some(request_id) {
        return Ok(None);
    }

    let response: CdpEvaluateResponse = serde_json::from_value(value)?;
    if let Some(error) = response.error {
        return Err(CdpError::Protocol {
            code: error.code,
            message: error.message,
        });
    }
    let result = response
        .result
        .ok_or(CdpError::MalformedResponse("missing result"))?;
    if let Some(exception) = result.exception_details {
        return Err(CdpError::RuntimeException(exception.to_string()));
    }

    Ok(Some(result.result.value.unwrap_or(Value::Null)))
}

#[derive(Debug, Deserialize)]
struct CdpEvaluateResponse {
    result: Option<CdpRuntimeEvaluateResult>,
    error: Option<CdpProtocolError>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CdpRuntimeEvaluateResult {
    result: CdpRemoteObject,
    exception_details: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct CdpRemoteObject {
    value: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct CdpProtocolError {
    code: i64,
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn selects_codex_page_target_from_cdp_targets() {
        let targets = vec![
            target(
                "worker-1",
                "service_worker",
                "Codex worker",
                "file:///codex/worker.js",
            ),
            target("page-1", "page", "Dashboard", "https://example.com"),
            target(
                "page-2",
                "page",
                "Codex",
                "file:///Applications/Codex.app/index.html",
            ),
        ];

        let selected = select_codex_target(&targets).expect("codex target is selected");

        assert_eq!(selected.id, "page-2");
    }

    #[test]
    fn prefers_codex_desktop_app_over_mobile_bridge_page() {
        let targets = vec![
            target(
                "page-1",
                "page",
                "Codex Mobile",
                "http://192.168.1.166:57324/",
            ),
            target("page-2", "page", "Codex", "app://-/index.html"),
        ];

        let selected = select_codex_target(&targets).expect("codex target is selected");

        assert_eq!(selected.id, "page-2");
    }

    #[test]
    fn selects_chatgpt_desktop_target_after_codex_rename() {
        let targets = vec![
            target("page-1", "page", "Dashboard", "https://example.com"),
            target("page-2", "page", "ChatGPT", "app://-/index.html"),
        ];

        let selected = select_codex_target(&targets).expect("chatgpt target is selected");

        assert_eq!(selected.id, "page-2");
    }

    #[test]
    fn ignores_chatgpt_classic_target() {
        let targets = vec![target(
            "page-1",
            "page",
            "ChatGPT Classic",
            "file:///Applications/ChatGPT%20Classic.app/index.html",
        )];

        let error = select_codex_target(&targets).expect_err("classic app is ignored");

        assert!(matches!(error, CdpError::NoCodexTarget));
    }

    #[test]
    fn ignores_codex_mobile_bridge_page_when_desktop_app_is_absent() {
        let targets = vec![target(
            "page-1",
            "page",
            "Codex Mobile",
            "http://192.168.1.166:57324/",
        )];

        let error = select_codex_target(&targets).expect_err("mobile bridge is ignored");

        assert!(matches!(error, CdpError::NoCodexTarget));
    }

    #[test]
    fn bridge_script_uses_current_codex_host_module_bridge() {
        assert!(CODEX_APP_SERVER_BRIDGE_SCRIPT.contains("new-thread-panel-page"));
        assert!(CODEX_APP_SERVER_BRIDGE_SCRIPT.contains("send-cli-request-for-host"));
    }

    #[test]
    fn reports_missing_target_as_degraded() {
        let targets = vec![target(
            "page-1",
            "page",
            "Other Electron App",
            "file:///Applications/Other.app/index.html",
        )];

        let health = bridge_health_from_targets(&targets);

        assert_eq!(health.status, BridgeHealthStatus::Degraded);
        assert_eq!(
            health.connection_state,
            BridgeConnectionState::TargetNotFound
        );
        assert!(health.reason.expect("reason is present").contains("Codex"));
    }

    #[test]
    fn evaluate_response_parser_ignores_cdp_events() {
        assert_eq!(
            evaluate_response_value(
                1,
                r#"{"method":"Runtime.consoleAPICalled","params":{"type":"log"}}"#
            )
            .expect("event parses"),
            None
        );

        assert_eq!(
            evaluate_response_value(
                1,
                r#"{"id":1,"result":{"result":{"type":"string","value":"ok"}}}"#
            )
            .expect("response parses"),
            Some(json!("ok"))
        );
    }

    fn target(id: &str, target_type: &str, title: &str, url: &str) -> CdpTarget {
        CdpTarget {
            id: id.to_string(),
            target_type: target_type.to_string(),
            title: title.to_string(),
            url: url.to_string(),
            web_socket_debugger_url: Some(format!("ws://127.0.0.1/devtools/page/{id}")),
            devtools_frontend_url: None,
        }
    }
}
