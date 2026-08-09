use std::{collections::VecDeque, time::Duration};

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_millis(1_500);
const CDP_MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const CDP_MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
const CODEX_NOTIFICATION_BINDING: &str = "__codexMobileBridgeNotification";

const CODEX_NOTIFICATION_SUBSCRIBE_EXPRESSION: &str = r#"(async () => {
  const bridge = globalThis.__codexMobileBridge;
  if (!bridge || typeof bridge.subscribeNotifications !== "function") {
    throw new Error("Codex mobile notification bridge is not injected");
  }
  return bridge.subscribeNotifications("__codexMobileBridgeNotification");
})()"#;

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CdpNotification {
    pub method: String,
    pub params: Value,
}

pub struct CdpNotificationStream {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    pending: VecDeque<CdpNotification>,
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
        let (mut socket, _response) =
            connect_async_with_config(websocket_url, Some(cdp_websocket_config()), false).await?;
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

impl CdpNotificationStream {
    pub async fn connect(target: &CdpTarget) -> Result<Self, CdpError> {
        let websocket_url = target.web_socket_debugger_url.as_deref().ok_or_else(|| {
            CdpError::MissingWebSocketUrl {
                target_id: target.id.clone(),
            }
        })?;
        let (socket, _response) =
            connect_async_with_config(websocket_url, Some(cdp_websocket_config()), false).await?;
        let mut stream = Self {
            socket,
            pending: VecDeque::new(),
        };

        stream.send_command(1, "Runtime.enable", json!({})).await?;
        stream
            .send_command(
                2,
                "Runtime.addBinding",
                json!({ "name": CODEX_NOTIFICATION_BINDING }),
            )
            .await?;
        stream
            .send_command(
                3,
                "Runtime.evaluate",
                json!({
                    "expression": CODEX_NOTIFICATION_SUBSCRIBE_EXPRESSION,
                    "awaitPromise": true,
                    "returnByValue": true,
                }),
            )
            .await?;

        Ok(stream)
    }

    pub async fn next_notification(&mut self) -> Result<Option<CdpNotification>, CdpError> {
        if let Some(notification) = self.pending.pop_front() {
            return Ok(Some(notification));
        }

        while let Some(message) = self.socket.next().await {
            match message? {
                Message::Text(text) => {
                    if let Some(notification) = notification_from_binding(&text)? {
                        return Ok(Some(notification));
                    }
                }
                Message::Ping(payload) => {
                    self.socket.send(Message::Pong(payload)).await?;
                }
                Message::Close(_) => return Ok(None),
                _ => {}
            }
        }

        Ok(None)
    }

    async fn send_command(&mut self, id: u64, method: &str, params: Value) -> Result<(), CdpError> {
        self.socket
            .send(Message::Text(
                json!({ "id": id, "method": method, "params": params }).to_string(),
            ))
            .await?;

        while let Some(message) = self.socket.next().await {
            match message? {
                Message::Text(text) => {
                    if let Some(notification) = notification_from_binding(&text)? {
                        self.pending.push_back(notification);
                        continue;
                    }
                    let value: Value = serde_json::from_str(&text)?;
                    if value.get("id").and_then(Value::as_u64) != Some(id) {
                        continue;
                    }
                    if let Some(error) = value.get("error") {
                        return Err(CdpError::Protocol {
                            code: error
                                .get("code")
                                .and_then(Value::as_i64)
                                .unwrap_or_default(),
                            message: error
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown CDP protocol error")
                                .to_string(),
                        });
                    }
                    if let Some(exception) = value.pointer("/result/exceptionDetails") {
                        return Err(CdpError::RuntimeException(exception.to_string()));
                    }
                    return Ok(());
                }
                Message::Ping(payload) => {
                    self.socket.send(Message::Pong(payload)).await?;
                }
                Message::Close(_) => return Err(CdpError::WebSocketClosed),
                _ => {}
            }
        }

        Err(CdpError::WebSocketClosed)
    }
}

fn notification_from_binding(text: &str) -> Result<Option<CdpNotification>, CdpError> {
    let value: Value = serde_json::from_str(text)?;
    if value.get("method").and_then(Value::as_str) != Some("Runtime.bindingCalled")
        || value.pointer("/params/name").and_then(Value::as_str) != Some(CODEX_NOTIFICATION_BINDING)
    {
        return Ok(None);
    }

    let payload = value
        .pointer("/params/payload")
        .and_then(Value::as_str)
        .ok_or(CdpError::MalformedResponse(
            "Runtime.bindingCalled missing payload",
        ))?;
    Ok(Some(serde_json::from_str(payload)?))
}

fn cdp_websocket_config() -> WebSocketConfig {
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(CDP_MAX_MESSAGE_BYTES);
    config.max_frame_size = Some(CDP_MAX_FRAME_BYTES);
    config
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
  if (
    globalThis.__codexMobileBridge &&
    typeof globalThis.__codexMobileBridge.rpc === "function" &&
    typeof globalThis.__codexMobileBridge.subscribeNotifications === "function" &&
    globalThis.__codexMobileBridge.supportsMobileStartConversation === true &&
    globalThis.__codexMobileBridge.supportsMobileApprovals === true &&
    globalThis.__codexMobileBridge.supportsNativeApprovalRequestIds === true
  ) {
    return true;
  }

  const APPROVAL_METHODS = new Set([
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
    "item/permissions/requestApproval",
    "mcpServer/elicitation/request",
  ]);
  const NOTIFICATION_METHODS = [
    "thread/started",
    "thread/status/changed",
    "turn/started",
    "turn/completed",
    "turn/failed",
    "item/started",
    "item/completed",
    "item/agentMessage/delta",
    "item/reasoning/summaryTextDelta",
    "item/reasoning/summaryPartAdded",
    "item/plan/delta",
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
    "item/permissions/requestApproval",
    "mcpServer/elicitation/request",
    "item/tool/requestUserInput",
  ];
  let cachedAppServerManager = null;
  let notificationUnsubscribers = [];

  const findScopeNode = () => {
    const root = globalThis.__codexRoot?._internalRoot?.current;
    if (!root) {
      return null;
    }

    const queue = [root];
    const seen = new WeakSet();
    let visited = 0;
    while (queue.length > 0 && visited < 5000) {
      const object = queue.shift();
      if (
        !object ||
        (typeof object !== "object" && typeof object !== "function") ||
        seen.has(object)
      ) {
        continue;
      }
      seen.add(object);
      visited += 1;

      if (
        object.familyBindings instanceof Map &&
        object.cachedBindings instanceof Map &&
        object.signalBindings instanceof Map &&
        object.store &&
        typeof object.store.get === "function"
      ) {
        return object;
      }

      let keys = [];
      try {
        keys = Reflect.ownKeys(object);
      } catch (_error) {
        continue;
      }
      for (const key of keys) {
        if (
          typeof key !== "string" ||
          ["return", "stateNode", "alternate", "_debugOwner"].includes(key)
        ) {
          continue;
        }
        let value;
        try {
          value = object[key];
        } catch (_error) {
          continue;
        }
        if (
          value &&
          (typeof value === "object" || typeof value === "function") &&
          !(typeof Node !== "undefined" && value instanceof Node)
        ) {
          queue.push(value);
        }
      }
      if (object.child) {
        queue.push(object.child);
      }
      if (object.sibling) {
        queue.push(object.sibling);
      }
    }

    return null;
  };

  const isAppServerManager = (candidate) =>
    candidate &&
    typeof candidate.getHostId === "function" &&
    typeof candidate.getConversation === "function" &&
    typeof candidate.replyWithCommandExecutionApprovalDecision === "function" &&
    typeof candidate.replyWithFileChangeApprovalDecision === "function" &&
    typeof candidate.replyWithPermissionsRequestApprovalResponse === "function" &&
    typeof candidate.replyWithMcpServerElicitationResponse === "function";

  const findAppServerManager = () => {
    if (isAppServerManager(cachedAppServerManager)) {
      return cachedAppServerManager;
    }

    const scopeNode = findScopeNode();
    if (!scopeNode) {
      return null;
    }

    const atoms = new Set([
      ...scopeNode.cachedBindings.values(),
      ...scopeNode.signalBindings.values(),
    ]);
    for (const familyMap of scopeNode.familyBindings.values()) {
      if (!(familyMap instanceof Map)) {
        continue;
      }
      for (const atom of familyMap.values()) {
        atoms.add(atom);
      }
    }

    for (const atom of atoms) {
      let value;
      try {
        value = scopeNode.store.get(atom);
      } catch (_error) {
        continue;
      }
      const candidates = Array.isArray(value) ? value : [value];
      for (const candidate of candidates) {
        if (isAppServerManager(candidate) && candidate.getHostId() === "local") {
          cachedAppServerManager = candidate;
          return candidate;
        }
      }
    }

    return null;
  };

  const listPendingApprovals = () => {
    const manager = findAppServerManager();
    if (!manager || !(manager.conversations instanceof Map)) {
      return [];
    }

    const approvals = [];
    for (const [threadId, conversation] of manager.conversations.entries()) {
      for (const request of conversation?.requests || []) {
        if (!request || !APPROVAL_METHODS.has(request.method)) {
          continue;
        }
        approvals.push({
          threadId,
          requestId: String(request.id),
          method: request.method,
          params: request.params || {},
        });
      }
    }
    return approvals;
  };

  const parseApprovalId = (approvalId) => {
    if (typeof approvalId !== "string" || approvalId.length === 0) {
      throw new Error("Invalid Codex approval id");
    }
    const separator = approvalId.lastIndexOf(":");
    if (separator <= 0 || separator === approvalId.length - 1) {
      return { threadId: null, requestId: approvalId };
    }
    return {
      threadId: approvalId.slice(0, separator),
      requestId: approvalId.slice(separator + 1),
    };
  };

  const respondToApproval = async (params) => {
    const manager = findAppServerManager();
    if (!manager || !(manager.conversations instanceof Map)) {
      throw new Error("ChatGPT approval manager is unavailable");
    }

    const { threadId: requestedThreadId, requestId } = parseApprovalId(params?.approvalId);
    const decision = params?.decision === "approve" ? "approve" : "reject";
    const conversations = requestedThreadId
      ? [[requestedThreadId, manager.getConversation(requestedThreadId)]]
      : Array.from(manager.conversations.entries());

    for (const [threadId, conversation] of conversations) {
      const request = (conversation?.requests || []).find(
        (candidate) => String(candidate?.id) === requestId && APPROVAL_METHODS.has(candidate?.method),
      );
      if (!request) {
        continue;
      }

      // ChatGPT keeps request ids as numbers in the renderer. Its approval
      // helpers use strict equality, so forwarding the mobile string id is a
      // silent no-op. Preserve the native id after locating the request.
      const nativeRequestId = request.id;

      switch (request.method) {
        case "item/commandExecution/requestApproval":
          await manager.replyWithCommandExecutionApprovalDecision(
            threadId,
            nativeRequestId,
            decision === "approve" ? "accept" : "decline",
          );
          break;
        case "item/fileChange/requestApproval":
          await manager.replyWithFileChangeApprovalDecision(
            threadId,
            nativeRequestId,
            decision === "approve" ? "accept" : "decline",
          );
          break;
        case "item/permissions/requestApproval":
          await manager.replyWithPermissionsRequestApprovalResponse(threadId, nativeRequestId, {
            permissions: decision === "approve" ? request.params?.permissions || {} : {},
            scope: "turn",
          });
          break;
        case "mcpServer/elicitation/request":
          await manager.replyWithMcpServerElicitationResponse(threadId, nativeRequestId, {
            action: decision === "approve" ? "accept" : "decline",
            content: decision === "approve" ? {} : null,
            _meta: null,
          });
          break;
        default:
          throw new Error(`Unsupported Codex approval method: ${request.method}`);
      }

      return {
        accepted: true,
        threadId,
        requestId,
        method: request.method,
      };
    }

    throw new Error(`Pending Codex approval not found: ${params?.approvalId || "unknown"}`);
  };

  const subscribeNotifications = (bindingName) => {
    const manager = findAppServerManager();
    const emit = globalThis[bindingName];
    if (!manager || typeof manager.addNotificationCallback !== "function") {
      throw new Error("ChatGPT notification manager is unavailable");
    }
    if (typeof emit !== "function") {
      throw new Error("Codex mobile CDP notification binding is unavailable");
    }

    for (const unsubscribe of notificationUnsubscribers) {
      try {
        unsubscribe();
      } catch (_error) {
        // A renderer reload may invalidate the previous callback set.
      }
    }
    notificationUnsubscribers = NOTIFICATION_METHODS.map((method) =>
      manager.addNotificationCallback(method, (notification) => {
        const params =
          notification && typeof notification === "object" && "params" in notification
            ? notification.params
            : notification;
        emit(JSON.stringify({ method, params: params || {} }));
      })
    );
    return true;
  };

  const installDirectClientBridge = (client) => {
    if (!client || typeof client.sendRequest !== "function") {
      return false;
    }
    globalThis.__codexMobileBridge = {
      mode: "direct-client",
      supportsMobileStartConversation: false,
      supportsMobileApprovals: false,
      supportsNativeApprovalRequestIds: false,
      subscribeNotifications,
      rpc: async (request) => {
        if (request?.method === "codex-mobile/start-conversation") {
          throw new Error("Codex mobile start-conversation requires a host bridge");
        }
        return client.sendRequest(request.method, request.params || {});
      },
    };
    return true;
  };

  const installHostBridge = (sendRequest, mode = "host-module") => {
    if (typeof sendRequest !== "function") {
      return false;
    }
    globalThis.__codexMobileBridge = {
      mode,
      supportsMobileStartConversation: true,
      supportsMobileApprovals: true,
      supportsNativeApprovalRequestIds: true,
      subscribeNotifications,
      rpc: async (request) => {
        if (!request || typeof request.method !== "string") {
          throw new Error("Invalid Codex mobile bridge request");
        }
        if (request.method === "codex-mobile/start-conversation") {
          const params = request.params || {};
          const result = await sendRequest("start-conversation", {
            hostId: "local",
            preparePrimaryRuntimeForFirstTurn: false,
            ...params,
          });
          const threadId =
            typeof result === "string"
              ? result
              : result?.threadId || result?.conversationId || result?.thread?.id || result?.id;
          if (typeof threadId !== "string" || threadId.length === 0) {
            return result;
          }
          const firstText =
            Array.isArray(params.input)
              ? params.input.find((part) => part?.type === "text" && typeof part.text === "string")?.text || ""
              : "";
          return {
            thread: {
              id: threadId,
              title: firstText,
              preview: firstText,
              cwd: typeof params.cwd === "string" ? params.cwd : null,
            },
          };
        }
        if (request.method === "codex-mobile/list-pending-approvals") {
          return listPendingApprovals();
        }
        if (request.method === "codex-mobile/respond-approval") {
          return await respondToApproval(request.params || {});
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

  const moduleUrls = Array.from(document.querySelectorAll('link[rel="modulepreload"], script[type="module"], script[src]'))
    .map((element) => element.href || element.src)
    .filter(Boolean);

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

  const hostModuleUrl =
    moduleUrls.find((url) => url.includes("new-thread-panel-page")) ||
    moduleUrls.find((url) => url.includes("app-server-manager-signals"));
  if (hostModuleUrl) {
    try {
      const hostModule = await import(hostModuleUrl);
      if (installHostBridge(hostModule.pv)) {
        return true;
      }
    } catch (error) {
      globalThis.__codexMobileBridgeLastError = error?.message || String(error);
    }
  }

  const isExportedAppServerSender = (candidate) => {
    if (typeof candidate !== "function" || candidate.length < 2) {
      return false;
    }
    let source;
    try {
      source = Function.prototype.toString.call(candidate);
    } catch (_error) {
      return false;
    }
    const directSender =
      /return\s+[A-Za-z0-9_$]+\.sendRequest\([A-Za-z0-9_$]+,[A-Za-z0-9_$]+\)/;
    const optionalRequestOptionsSender =
      /^function\s+[^()]*\(\s*([A-Za-z0-9_$]+)\s*,\s*([A-Za-z0-9_$]+)\s*,\s*([A-Za-z0-9_$]+)\s*\)\s*\{\s*return\s+\3\s*==\s*null\s*\?\s*([A-Za-z0-9_$]+)\.sendRequest\(\s*\1\s*,\s*\2\s*\)\s*:\s*\4\.sendRequest\(\s*\1\s*,\s*\2\s*,\s*\3\s*\)\s*;?\s*\}$/;
    return directSender.test(source) || optionalRequestOptionsSender.test(source);
  };

  const findExportedAppServerSender = (module) => {
    for (const value of Object.values(module || {})) {
      if (isExportedAppServerSender(value)) {
        return value;
      }
    }
    return null;
  };

  const findRpcHostModuleUrls = async () => {
    const candidates = new Set(
      moduleUrls.filter((url) =>
        url.includes("app-server") ||
        url.includes("pull-request-code-review") ||
        url.includes("hotkey-window-thread-page") ||
        /\/rpc-[^/]+\.js$/.test(url)
      )
    );

    await Promise.all(
      moduleUrls.map(async (url) => {
        try {
          const text = await fetch(url).then((response) => response.text());
          if (
            text.includes("Missing AppServer request message handler") &&
            text.includes("send-cli-request-for-host")
          ) {
            candidates.add(url);
          }
          if (/\/rpc-[^/]+\.js$/.test(url)) {
            for (const match of text.matchAll(/from\s*["'](\.\/[^"']+\.js)["']/g)) {
              candidates.add(new URL(match[1], url).href);
            }
          }
        } catch (_error) {
          // Best effort only; some lazy modules may not be fetchable in every build.
        }
      })
    );

    return Array.from(candidates);
  };

  try {
    for (const url of await findRpcHostModuleUrls()) {
      try {
        const module = await import(url);
        const sender = findExportedAppServerSender(module);
        if (installHostBridge(sender, "exported-host-sender")) {
          return true;
        }
      } catch (error) {
        globalThis.__codexMobileBridgeLastError = error?.message || String(error);
      }
    }
  } catch (error) {
    globalThis.__codexMobileBridgeLastError = error?.message || String(error);
  }

  return false;
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
    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use std::{fs, process::Command};
    use tempfile::TempDir;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

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
    fn bridge_script_discovers_chatgpt_exported_host_sender() {
        assert!(CODEX_APP_SERVER_BRIDGE_SCRIPT.contains("exported-host-sender"));
        assert!(
            CODEX_APP_SERVER_BRIDGE_SCRIPT.contains("Missing AppServer request message handler")
        );
    }

    #[test]
    fn bridge_script_injects_current_chatgpt_three_argument_host_sender() {
        let temp = TempDir::new().expect("temporary module fixture is created");
        let bridge_path = temp.path().join("bridge.js");
        let app_module_path = temp.path().join("app-initial-fixture.js");
        let rpc_module_path = temp.path().join("rpc-fixture.js");
        let harness_path = temp.path().join("harness.mjs");

        fs::write(temp.path().join("package.json"), r#"{"type":"module"}"#)
            .expect("module package fixture is written");
        fs::write(&bridge_path, CODEX_APP_SERVER_BRIDGE_SCRIPT)
            .expect("bridge script fixture is written");
        fs::write(
            &app_module_path,
            r#"
const client = { sendRequest: (...args) => args };

export function aUnrelated(first, second, third) {
  return client.sendRequest("wrong-method", first, second, third);
}

export function zAppServerSender(method, params, options) {
  return options == null
    ? client.sendRequest(method, params)
    : client.sendRequest(method, params, options);
}
"#,
        )
        .expect("app module fixture is written");
        fs::write(
            &rpc_module_path,
            r#"
import { zAppServerSender } from "./app-initial-fixture.js";
export function initializeAppHostServices() {
  return typeof zAppServerSender;
}
"#,
        )
        .expect("rpc module fixture is written");
        fs::write(
            &harness_path,
            r#"
import fs from "node:fs";
import { pathToFileURL } from "node:url";

const [bridgePath, rpcModulePath] = process.argv.slice(2);
const rpcModuleUrl = pathToFileURL(rpcModulePath).href;
globalThis.document = {
  querySelectorAll() {
    return [{ href: rpcModuleUrl }];
  },
};
globalThis.fetch = async (url) => ({
  text: async () => fs.readFileSync(new URL(url), "utf8"),
});

const injected = await eval(fs.readFileSync(bridgePath, "utf8"));
if (injected !== true) {
  throw new Error("bridge injection returned false");
}
if (globalThis.__codexMobileBridge?.mode !== "exported-host-sender") {
  throw new Error(`unexpected bridge mode: ${globalThis.__codexMobileBridge?.mode}`);
}
const response = await globalThis.__codexMobileBridge.rpc({
  method: "thread/list",
  params: { limit: 1 },
});
if (response[0] !== "send-cli-request-for-host") {
  throw new Error(`selected unrelated sender: ${JSON.stringify(response)}`);
}
"#,
        )
        .expect("node harness is written");

        let output = Command::new("node")
            .arg(&harness_path)
            .arg(&bridge_path)
            .arg(&rpc_module_path)
            .output()
            .expect("node is available to execute the bridge script");

        assert!(
            output.status.success(),
            "bridge harness failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn bridge_script_routes_mobile_thread_start_through_host_signal() {
        assert!(CODEX_APP_SERVER_BRIDGE_SCRIPT.contains("codex-mobile/start-conversation"));
        assert!(CODEX_APP_SERVER_BRIDGE_SCRIPT.contains("start-conversation"));
    }

    #[test]
    fn bridge_script_supports_real_mobile_approvals() {
        assert!(CODEX_APP_SERVER_BRIDGE_SCRIPT.contains("supportsMobileApprovals"));
        assert!(CODEX_APP_SERVER_BRIDGE_SCRIPT.contains("supportsNativeApprovalRequestIds"));
        assert!(CODEX_APP_SERVER_BRIDGE_SCRIPT.contains("codex-mobile/list-pending-approvals"));
        assert!(CODEX_APP_SERVER_BRIDGE_SCRIPT.contains("codex-mobile/respond-approval"));
        assert!(CODEX_APP_SERVER_BRIDGE_SCRIPT.contains("replyWithMcpServerElicitationResponse"));
        assert!(CODEX_APP_SERVER_BRIDGE_SCRIPT.contains("mcpServer/elicitation/request"));
        assert!(CODEX_APP_SERVER_BRIDGE_SCRIPT.contains("const nativeRequestId = request.id"));
        assert!(
            !CODEX_APP_SERVER_BRIDGE_SCRIPT
                .contains("replyWithMcpServerElicitationResponse(threadId, requestId")
        );
    }

    #[test]
    fn bridge_script_subscribes_to_public_notification_summaries() {
        assert!(CODEX_APP_SERVER_BRIDGE_SCRIPT.contains("subscribeNotifications"));
        assert!(CODEX_APP_SERVER_BRIDGE_SCRIPT.contains("addNotificationCallback"));
        assert!(CODEX_APP_SERVER_BRIDGE_SCRIPT.contains("item/agentMessage/delta"));
        assert!(CODEX_APP_SERVER_BRIDGE_SCRIPT.contains("item/reasoning/summaryTextDelta"));
        assert!(CODEX_APP_SERVER_BRIDGE_SCRIPT.contains("item/plan/delta"));
        assert!(!CODEX_APP_SERVER_BRIDGE_SCRIPT.contains("item/reasoning/textDelta"));
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

    #[tokio::test]
    async fn notification_stream_installs_runtime_binding_and_yields_notifications() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test websocket binds");
        let address = listener.local_addr().expect("test websocket address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("websocket accepts");
            let mut socket = accept_async(stream)
                .await
                .expect("websocket handshake succeeds");

            for (id, expected_method) in [
                (1, "Runtime.enable"),
                (2, "Runtime.addBinding"),
                (3, "Runtime.evaluate"),
            ] {
                let request = socket
                    .next()
                    .await
                    .expect("CDP request arrives")
                    .expect("CDP request is valid");
                let request: Value = serde_json::from_str(request.to_text().expect("text request"))
                    .expect("request JSON parses");
                assert_eq!(request["id"], json!(id));
                assert_eq!(request["method"], json!(expected_method));
                if expected_method == "Runtime.addBinding" {
                    assert_eq!(request["params"]["name"], json!(CODEX_NOTIFICATION_BINDING));
                }
                if expected_method == "Runtime.evaluate" {
                    assert!(
                        request["params"]["expression"]
                            .as_str()
                            .expect("expression")
                            .contains("subscribeNotifications")
                    );
                }
                socket
                    .send(Message::Text(json!({ "id": id, "result": {} }).to_string()))
                    .await
                    .expect("CDP response writes");
            }

            let payload = json!({
                "method": "item/agentMessage/delta",
                "params": {
                    "threadId": "thread-1",
                    "turnId": "turn-1",
                    "itemId": "item-1",
                    "delta": "hello"
                }
            });
            socket
                .send(Message::Text(
                    json!({
                        "method": "Runtime.bindingCalled",
                        "params": {
                            "name": CODEX_NOTIFICATION_BINDING,
                            "payload": payload.to_string()
                        }
                    })
                    .to_string(),
                ))
                .await
                .expect("binding notification writes");
        });
        let target = CdpTarget {
            id: "notification-stream".to_string(),
            target_type: "page".to_string(),
            title: "ChatGPT".to_string(),
            url: "app://-/index.html".to_string(),
            web_socket_debugger_url: Some(format!("ws://{address}")),
            devtools_frontend_url: None,
        };

        let mut stream = CdpNotificationStream::connect(&target)
            .await
            .expect("notification stream connects");
        let notification = stream
            .next_notification()
            .await
            .expect("notification reads")
            .expect("notification is present");

        assert_eq!(notification.method, "item/agentMessage/delta");
        assert_eq!(notification.params["delta"], json!("hello"));
        server.await.expect("websocket server finishes");
    }

    #[tokio::test]
    async fn evaluate_accepts_single_cdp_frame_larger_than_16_mib() {
        const PAYLOAD_BYTES: usize = 17 * 1024 * 1024;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test websocket binds");
        let address = listener.local_addr().expect("test websocket address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("websocket accepts");
            let mut socket = accept_async(stream)
                .await
                .expect("websocket handshake succeeds");
            let _request = socket
                .next()
                .await
                .expect("runtime evaluate request arrives")
                .expect("runtime evaluate request is valid");
            let response = json!({
                "id": 1,
                "result": {
                    "result": {
                        "type": "string",
                        "value": "x".repeat(PAYLOAD_BYTES)
                    }
                }
            });
            socket
                .send(Message::Text(response.to_string()))
                .await
                .expect("large CDP response writes");
        });
        let client = CdpClient::with_base_url("http://127.0.0.1:1").expect("client builds");
        let target = CdpTarget {
            id: "large-frame".to_string(),
            target_type: "page".to_string(),
            title: "Codex".to_string(),
            url: "app://-/index.html".to_string(),
            web_socket_debugger_url: Some(format!("ws://{address}")),
            devtools_frontend_url: None,
        };

        let value = client
            .evaluate_on_target(&target, "'large response'")
            .await
            .expect("large CDP frame is accepted");

        assert_eq!(value.as_str().map(str::len), Some(PAYLOAD_BYTES));
        server.await.expect("websocket server finishes");
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
