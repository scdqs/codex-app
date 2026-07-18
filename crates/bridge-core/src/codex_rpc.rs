use std::{
    collections::HashSet,
    sync::atomic::{AtomicU64, Ordering},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    cdp::{CdpClient, CdpTarget},
    protocol::ApprovalDecision,
};

const THREAD_LIST_PAGE_SIZE: u32 = 100;
const MAX_THREAD_LIST_PAGES: usize = 20;
const MAX_THREAD_LIST_ITEMS: usize = THREAD_LIST_PAGE_SIZE as usize * MAX_THREAD_LIST_PAGES;

#[async_trait]
pub trait CodexAdapter: Send + Sync {
    async fn list_threads(&self) -> Result<Vec<CodexThread>, CodexRpcError>;
    async fn start_thread(
        &self,
        cwd: &str,
        text: &str,
        attachments: &[UserImageAttachment],
    ) -> Result<CodexThread, CodexRpcError>;
    async fn resume_thread(&self, thread_id: &str) -> Result<Option<CodexThread>, CodexRpcError>;
    async fn list_turns(&self, thread_id: &str) -> Result<Vec<CodexTurn>, CodexRpcError>;
    async fn list_turns_page(
        &self,
        thread_id: &str,
        cursor: Option<&str>,
    ) -> Result<CodexTurnPage, CodexRpcError> {
        if cursor.is_some() {
            return Err(CodexRpcError::Unsupported {
                method: "thread/turns/list cursor",
            });
        }
        Ok(CodexTurnPage {
            turns: self.list_turns(thread_id).await?,
            next_cursor: None,
            backwards_cursor: None,
        })
    }
    async fn send_user_message(
        &self,
        thread_id: &str,
        text: &str,
        attachments: &[UserImageAttachment],
    ) -> Result<(), CodexRpcError>;
    async fn list_pending_approvals(&self) -> Result<Vec<CodexPendingApproval>, CodexRpcError> {
        Ok(Vec::new())
    }
    async fn subscribe_events(&self, thread_id: Option<&str>) -> Result<(), CodexRpcError>;
    async fn respond_approval(
        &self,
        approval_id: &str,
        decision: &ApprovalDecision,
    ) -> Result<(), CodexRpcError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserImageAttachment {
    pub path: String,
}

#[async_trait]
pub trait JsonRpcTransport: Send + Sync {
    async fn send_request(&self, request: JsonRpcRequest) -> Result<Value, CodexRpcError>;
}

#[derive(Debug)]
pub struct AppServerJsonRpcClient<T> {
    transport: T,
    next_request_id: AtomicU64,
    next_client_message_id: AtomicU64,
}

#[derive(Debug, Clone)]
pub struct CdpAppServerTransport {
    cdp: CdpClient,
    target: CdpTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CdpRpcResponseMode {
    Full,
    CompactThread,
    Acknowledge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexThread {
    pub id: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub model_provider: Option<String>,
    pub preview: Option<String>,
    pub created_at: Option<u64>,
    pub updated_at: Option<u64>,
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexTurn {
    pub id: Option<String>,
    pub thread_id: Option<String>,
    pub created_at: Option<u64>,
    pub updated_at: Option<u64>,
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexTurnPage {
    pub turns: Vec<CodexTurn>,
    pub next_cursor: Option<String>,
    pub backwards_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexRawEvent {
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexPendingApproval {
    pub thread_id: String,
    pub request_id: String,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Error)]
pub enum CodexRpcError {
    #[error("json-rpc transport failed: {0}")]
    Transport(String),
    #[error("json-rpc method {method} returned invalid response: {reason}")]
    InvalidResponse {
        method: &'static str,
        reason: &'static str,
    },
    #[error("json-rpc method {method} is not implemented yet")]
    Unsupported { method: &'static str },
}

impl<T> AppServerJsonRpcClient<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            next_request_id: AtomicU64::new(1),
            next_client_message_id: AtomicU64::new(1),
        }
    }

    pub fn into_transport(self) -> T {
        self.transport
    }
}

impl<T> AppServerJsonRpcClient<T>
where
    T: JsonRpcTransport,
{
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, CodexRpcError> {
        let id = self.next_request_id.fetch_add(1, Ordering::SeqCst);
        self.transport
            .send_request(JsonRpcRequest {
                jsonrpc: "2.0",
                id,
                method: method.to_string(),
                params,
            })
            .await
    }

    pub async fn probe_turn_start(&self, thread_id: &str) -> Result<(), CodexRpcError> {
        self.call(
            "turn/start",
            json!({
                "threadId": thread_id,
                "clientUserMessageId": "codex-mobile-healthcheck",
                "input": [],
                "dryRun": true,
            }),
        )
        .await
        .map(|_| ())
    }
}

impl CdpAppServerTransport {
    pub fn new(cdp: CdpClient, target: CdpTarget) -> Self {
        Self { cdp, target }
    }
}

#[async_trait]
impl JsonRpcTransport for CdpAppServerTransport {
    async fn send_request(&self, request: JsonRpcRequest) -> Result<Value, CodexRpcError> {
        let request_json = serde_json::to_string(&request)
            .map_err(|error| CodexRpcError::Transport(error.to_string()))?;
        let expression = cdp_rpc_expression(&request_json, cdp_rpc_response_mode(&request.method));

        self.cdp
            .evaluate_on_target(&self.target, &expression)
            .await
            .map_err(|error| CodexRpcError::Transport(error.to_string()))
    }
}

fn cdp_rpc_response_mode(method: &str) -> CdpRpcResponseMode {
    match method {
        "thread/resume" => CdpRpcResponseMode::CompactThread,
        "turn/start" | "codex-mobile/respond-approval" => CdpRpcResponseMode::Acknowledge,
        _ => CdpRpcResponseMode::Full,
    }
}

fn cdp_rpc_expression(request_json: &str, response_mode: CdpRpcResponseMode) -> String {
    let result_expression = match response_mode {
        CdpRpcResponseMode::Full => "return result;",
        CdpRpcResponseMode::Acknowledge => "return { accepted: true };",
        CdpRpcResponseMode::CompactThread => {
            r#"
  const thread = result?.thread ?? result?.data?.thread ?? result?.data ?? result;
  if (!thread || typeof thread !== "object") {
    return result;
  }
  return {
    thread: {
      id: thread.id ?? thread.threadId ?? thread.thread_id ?? request.params?.threadId ?? null,
      title: thread.title ?? thread.name ?? null,
      cwd: thread.cwd ?? thread.workingDirectory ?? thread.working_directory ?? result?.cwd ?? null,
      modelProvider: thread.modelProvider ?? thread.model_provider ?? result?.modelProvider ?? null,
      preview: thread.preview ?? thread.summary ?? null,
      createdAt: thread.createdAt ?? thread.created_at ?? null,
      updatedAt: thread.updatedAt ?? thread.updated_at ?? null,
    },
  };
"#
        }
    };

    format!(
        r#"(async () => {{
  const bridge = globalThis.__codexMobileBridge;
  if (!bridge || typeof bridge.rpc !== "function") {{
    throw new Error("Codex mobile bridge is not injected");
  }}
  const request = {request_json};
  const result = await bridge.rpc(request);
  {result_expression}
}})()"#
    )
}

#[async_trait]
impl<T> CodexAdapter for AppServerJsonRpcClient<T>
where
    T: JsonRpcTransport,
{
    async fn list_threads(&self) -> Result<Vec<CodexThread>, CodexRpcError> {
        let mut threads = Vec::new();
        let mut seen_thread_ids = HashSet::new();
        let mut seen_cursors = HashSet::new();
        let mut cursor: Option<String> = None;

        for _ in 0..MAX_THREAD_LIST_PAGES {
            let mut params = json!({
                "limit": THREAD_LIST_PAGE_SIZE,
                "sortKey": "recency_at",
                "sortDirection": "desc",
            });
            if let Some(cursor) = cursor.as_deref() {
                params["cursor"] = json!(cursor);
            }

            let result = self.call("thread/list", params).await?;
            let items = extract_array(&result, &["data", "threads", "items"]).ok_or(
                CodexRpcError::InvalidResponse {
                    method: "thread/list",
                    reason: "missing thread array",
                },
            )?;

            for item in items {
                let thread = map_thread(item)?;
                if seen_thread_ids.insert(thread.id.clone()) {
                    threads.push(thread);
                    if threads.len() == MAX_THREAD_LIST_ITEMS {
                        return Ok(threads);
                    }
                }
            }

            let Some(next_cursor) = cursor_field(&result, &["nextCursor", "next_cursor"])
                .filter(|next_cursor| !next_cursor.is_empty())
            else {
                break;
            };
            if !seen_cursors.insert(next_cursor.clone()) {
                break;
            }
            cursor = Some(next_cursor);
        }

        Ok(threads)
    }

    async fn resume_thread(&self, thread_id: &str) -> Result<Option<CodexThread>, CodexRpcError> {
        let result = self
            .call("thread/resume", json!({ "threadId": thread_id }))
            .await?;
        Ok(extract_thread_value(&result).map(map_thread).transpose()?)
    }

    async fn start_thread(
        &self,
        cwd: &str,
        text: &str,
        attachments: &[UserImageAttachment],
    ) -> Result<CodexThread, CodexRpcError> {
        let result = self
            .call(
                "codex-mobile/start-conversation",
                json!({
                    "input": host_conversation_input(text, attachments),
                    "cwd": cwd,
                    "workspaceRoots": [cwd],
                    "workspaceKind": "project",
                    "collaborationMode": null,
                    "serviceTier": null,
                    "threadSource": "user",
                    "approvalsReviewer": "user",
                }),
            )
            .await?;
        let thread_value = extract_thread_value(&result).ok_or(CodexRpcError::InvalidResponse {
            method: "codex-mobile/start-conversation",
            reason: "missing thread",
        })?;

        map_thread(thread_value)
    }

    async fn list_turns(&self, thread_id: &str) -> Result<Vec<CodexTurn>, CodexRpcError> {
        Ok(self.list_turns_page(thread_id, None).await?.turns)
    }

    async fn list_turns_page(
        &self,
        thread_id: &str,
        cursor: Option<&str>,
    ) -> Result<CodexTurnPage, CodexRpcError> {
        let mut params = json!({ "threadId": thread_id });
        if let Some(cursor) = cursor {
            params["cursor"] = json!(cursor);
        }
        let result = self.call("thread/turns/list", params).await?;
        let items = extract_turn_values(&result).ok_or(CodexRpcError::InvalidResponse {
            method: "thread/turns/list",
            reason: "missing turn array",
        })?;

        Ok(CodexTurnPage {
            turns: items.into_iter().map(map_turn).collect(),
            next_cursor: cursor_field(&result, &["nextCursor", "next_cursor"]),
            backwards_cursor: cursor_field(&result, &["backwardsCursor", "backwards_cursor"]),
        })
    }

    async fn send_user_message(
        &self,
        thread_id: &str,
        text: &str,
        attachments: &[UserImageAttachment],
    ) -> Result<(), CodexRpcError> {
        self.resume_thread(thread_id).await?;
        self.start_turn_without_resume(thread_id, text, attachments)
            .await
    }

    async fn list_pending_approvals(&self) -> Result<Vec<CodexPendingApproval>, CodexRpcError> {
        let result = self
            .call("codex-mobile/list-pending-approvals", json!({}))
            .await?;
        let items = extract_array(&result, &["approvals", "items"]).ok_or(
            CodexRpcError::InvalidResponse {
                method: "codex-mobile/list-pending-approvals",
                reason: "missing approval array",
            },
        )?;

        items
            .iter()
            .cloned()
            .map(|item| {
                serde_json::from_value(item).map_err(|_| CodexRpcError::InvalidResponse {
                    method: "codex-mobile/list-pending-approvals",
                    reason: "invalid pending approval",
                })
            })
            .collect()
    }

    async fn subscribe_events(&self, _thread_id: Option<&str>) -> Result<(), CodexRpcError> {
        Err(CodexRpcError::Unsupported {
            method: "subscribe_events",
        })
    }

    async fn respond_approval(
        &self,
        approval_id: &str,
        decision: &ApprovalDecision,
    ) -> Result<(), CodexRpcError> {
        self.call(
            "codex-mobile/respond-approval",
            json!({
                "approvalId": approval_id,
                "decision": decision.decision,
            }),
        )
        .await?;
        Ok(())
    }
}

impl<T> AppServerJsonRpcClient<T>
where
    T: JsonRpcTransport,
{
    async fn start_turn_without_resume(
        &self,
        thread_id: &str,
        text: &str,
        attachments: &[UserImageAttachment],
    ) -> Result<(), CodexRpcError> {
        let client_user_message_id = format!(
            "codex-mobile-{}",
            self.next_client_message_id.fetch_add(1, Ordering::SeqCst)
        );
        let input = turn_start_input(text, attachments);
        self.call(
            "turn/start",
            json!({
                "threadId": thread_id,
                "clientUserMessageId": client_user_message_id,
                "input": input,
            }),
        )
        .await?;

        Ok(())
    }
}

fn turn_start_input(text: &str, attachments: &[UserImageAttachment]) -> Vec<Value> {
    let mut input = Vec::new();
    if !text.trim().is_empty() {
        input.push(json!({ "type": "text", "text": text }));
    }
    input.extend(
        attachments
            .iter()
            .map(|attachment| json!({ "type": "localImage", "path": attachment.path })),
    );
    input
}

fn host_conversation_input(text: &str, attachments: &[UserImageAttachment]) -> Vec<Value> {
    let mut input = Vec::new();
    if !text.trim().is_empty() {
        input.push(json!({ "type": "text", "text": text, "text_elements": [] }));
    }
    input.extend(
        attachments
            .iter()
            .map(|attachment| json!({ "type": "localImage", "path": attachment.path })),
    );
    input
}

fn map_thread(value: &Value) -> Result<CodexThread, CodexRpcError> {
    let id = string_field(value, &["id", "threadId", "thread_id"]).ok_or(
        CodexRpcError::InvalidResponse {
            method: "thread/list",
            reason: "thread id is missing",
        },
    )?;

    Ok(CodexThread {
        id,
        title: string_field(value, &["title", "name"]),
        cwd: string_field(value, &["cwd", "workingDirectory", "working_directory"]),
        model_provider: string_field(value, &["modelProvider", "model_provider"]),
        preview: string_field(value, &["preview", "summary"]),
        created_at: number_field(value, &["createdAt", "created_at"]),
        updated_at: number_field(
            value,
            &["recencyAt", "recency_at", "updatedAt", "updated_at"],
        ),
        raw: value.clone(),
    })
}

fn map_turn(value: Value) -> CodexTurn {
    CodexTurn {
        id: string_field(&value, &["id", "turnId", "turn_id"]),
        thread_id: string_field(&value, &["threadId", "thread_id"]),
        created_at: number_field(
            &value,
            &["createdAt", "created_at", "startedAt", "started_at"],
        ),
        updated_at: number_field(
            &value,
            &["updatedAt", "updated_at", "completedAt", "completed_at"],
        ),
        raw: value,
    }
}

fn extract_array<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Vec<Value>> {
    if let Value::Array(items) = value {
        return Some(items);
    }

    for key in keys {
        if let Some(Value::Array(items)) = value.get(*key) {
            return Some(items);
        }
    }

    None
}

fn extract_thread_value(value: &Value) -> Option<&Value> {
    if string_field(value, &["id", "threadId", "thread_id"]).is_some() {
        return Some(value);
    }
    if let Some(data) = value.get("data")
        && string_field(data, &["id", "threadId", "thread_id"]).is_some()
    {
        return Some(data);
    }
    if let Some(result) = value.get("result")
        && string_field(result, &["id", "threadId", "thread_id"]).is_some()
    {
        return Some(result);
    }
    if let Some(thread) = value.get("thread") {
        return Some(thread);
    }
    if let Some(thread) = value.pointer("/data/thread") {
        return Some(thread);
    }
    if let Some(thread) = value.pointer("/result/thread") {
        return Some(thread);
    }
    value.get("conversation")
}

fn extract_turn_values(value: &Value) -> Option<Vec<Value>> {
    if let Some(items) = extract_array(value, &["data", "turns", "messages"]) {
        return Some(items.clone());
    }
    if let Some(items) = value.pointer("/thread/turns").and_then(Value::as_array) {
        return Some(items.clone());
    }
    if let Some(items) = value.pointer("/data/turns").and_then(Value::as_array) {
        return Some(items.clone());
    }
    if let Some(items) = value
        .pointer("/conversation/turns")
        .and_then(Value::as_array)
    {
        return Some(items.clone());
    }
    if let Some(items) = value.get("items").and_then(Value::as_array) {
        return Some(vec![json!({
            "items": items,
            "createdAt": value.get("createdAt").cloned().unwrap_or(Value::Null),
            "updatedAt": value.get("updatedAt").cloned().unwrap_or(Value::Null),
        })]);
    }

    None
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

fn cursor_field(value: &Value, keys: &[&str]) -> Option<String> {
    string_field(value, keys).or_else(|| {
        value
            .get("result")
            .and_then(|result| string_field(result, keys))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    #[tokio::test]
    async fn json_rpc_request_uses_incrementing_ids() {
        let transport = RecordingTransport::new(vec![json!({ "data": [] }), json!({})]);
        let requests = transport.requests();
        let client = AppServerJsonRpcClient::new(transport);

        client.list_threads().await.expect("thread list succeeds");
        client
            .resume_thread("thread-1")
            .await
            .expect("thread resume succeeds");

        let requests = requests.lock().expect("requests lock");
        assert_eq!(requests[0].id, 1);
        assert_eq!(requests[0].method, "thread/list");
        assert_eq!(requests[1].id, 2);
        assert_eq!(requests[1].method, "thread/resume");
    }

    #[test]
    fn cdp_transport_compacts_large_resume_and_write_responses() {
        assert_eq!(
            cdp_rpc_response_mode("thread/resume"),
            CdpRpcResponseMode::CompactThread
        );
        assert_eq!(
            cdp_rpc_response_mode("turn/start"),
            CdpRpcResponseMode::Acknowledge
        );
        assert_eq!(
            cdp_rpc_response_mode("thread/turns/list"),
            CdpRpcResponseMode::Full
        );

        let expression = cdp_rpc_expression(
            r#"{"jsonrpc":"2.0","id":1,"method":"thread/resume","params":{"threadId":"thread-1"}}"#,
            CdpRpcResponseMode::CompactThread,
        );
        assert!(expression.contains("const thread = result?.thread"));
        assert!(expression.contains("request.params?.threadId"));
        assert!(!expression.contains("initialTurnsPage"));
    }

    #[tokio::test]
    async fn adapter_maps_thread_list_response() {
        let transport = RecordingTransport::new(vec![json!({
            "data": [{
                "id": "thread-1",
                "preview": "Build bridge",
                "cwd": "/repo",
                "modelProvider": "OpenAI",
                "updatedAt": 1_725_000_000_000_u64
            }]
        })]);
        let client = AppServerJsonRpcClient::new(transport);

        let threads = client.list_threads().await.expect("threads map");

        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].id, "thread-1");
        assert_eq!(threads[0].preview.as_deref(), Some("Build bridge"));
        assert_eq!(threads[0].cwd.as_deref(), Some("/repo"));
        assert_eq!(threads[0].model_provider.as_deref(), Some("OpenAI"));
        assert_eq!(threads[0].updated_at, Some(1_725_000_000_000));
    }

    #[tokio::test]
    async fn adapter_paginates_thread_list_and_deduplicates_threads() {
        let transport = RecordingTransport::new(vec![
            json!({
                "data": [
                    { "id": "thread-new", "updatedAt": 300_u64 },
                    { "id": "thread-shared", "updatedAt": 200_u64 }
                ],
                "nextCursor": "older-cursor"
            }),
            json!({
                "data": [
                    { "id": "thread-shared", "updatedAt": 200_u64 },
                    { "id": "thread-old", "updatedAt": 100_u64 }
                ]
            }),
        ]);
        let requests = transport.requests();
        let client = AppServerJsonRpcClient::new(transport);

        let threads = client.list_threads().await.expect("thread pages map");

        assert_eq!(
            threads
                .iter()
                .map(|thread| thread.id.as_str())
                .collect::<Vec<_>>(),
            vec!["thread-new", "thread-shared", "thread-old"]
        );
        let requests = requests.lock().expect("requests lock");
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].params,
            json!({
                "limit": 100,
                "sortKey": "recency_at",
                "sortDirection": "desc"
            })
        );
        assert_eq!(
            requests[1].params,
            json!({
                "limit": 100,
                "sortKey": "recency_at",
                "sortDirection": "desc",
                "cursor": "older-cursor"
            })
        );
    }

    #[tokio::test]
    async fn adapter_stops_thread_pagination_when_cursor_repeats() {
        let transport = RecordingTransport::new(vec![
            json!({
                "data": [{ "id": "thread-1" }],
                "nextCursor": "loop-cursor"
            }),
            json!({
                "data": [{ "id": "thread-2" }],
                "nextCursor": "loop-cursor"
            }),
        ]);
        let requests = transport.requests();
        let client = AppServerJsonRpcClient::new(transport);

        let threads = client.list_threads().await.expect("cursor loop is bounded");

        assert_eq!(threads.len(), 2);
        assert_eq!(requests.lock().expect("requests lock").len(), 2);
    }

    #[tokio::test]
    async fn adapter_maps_turn_page_cursors_and_forwards_cursor_parameter() {
        let transport = RecordingTransport::new(vec![json!({
            "data": [{
                "id": "turn-1",
                "threadId": "thread-1",
                "items": []
            }],
            "nextCursor": "older-cursor",
            "backwardsCursor": "newer-cursor"
        })]);
        let requests = transport.requests();
        let client = AppServerJsonRpcClient::new(transport);

        let page = client
            .list_turns_page("thread-1", Some("current-cursor"))
            .await
            .expect("turn page maps");

        assert_eq!(page.turns.len(), 1);
        assert_eq!(page.turns[0].id.as_deref(), Some("turn-1"));
        assert_eq!(page.next_cursor.as_deref(), Some("older-cursor"));
        assert_eq!(page.backwards_cursor.as_deref(), Some("newer-cursor"));
        let requests = requests.lock().expect("requests lock");
        assert_eq!(requests[0].method, "thread/turns/list");
        assert_eq!(
            requests[0].params,
            json!({ "threadId": "thread-1", "cursor": "current-cursor" })
        );
    }

    #[tokio::test]
    async fn adapter_sends_turn_start_for_user_text() {
        let transport = RecordingTransport::new(vec![json!({}), json!({ "accepted": true })]);
        let requests = transport.requests();
        let client = AppServerJsonRpcClient::new(transport);

        client
            .send_user_message("thread-1", "same text", &[])
            .await
            .expect("message sends");

        let requests = requests.lock().expect("requests lock");
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "thread/resume");
        assert_eq!(requests[0].params, json!({ "threadId": "thread-1" }));
        assert_eq!(requests[1].method, "turn/start");
        assert_eq!(
            requests[1].params,
            json!({
                "threadId": "thread-1",
                "clientUserMessageId": "codex-mobile-1",
                "input": [{ "type": "text", "text": "same text" }]
            })
        );
    }

    #[tokio::test]
    async fn adapter_sends_turn_start_with_local_image_attachments() {
        let transport = RecordingTransport::new(vec![json!({}), json!({ "accepted": true })]);
        let requests = transport.requests();
        let client = AppServerJsonRpcClient::new(transport);

        client
            .send_user_message(
                "thread-1",
                "what is in this image?",
                &[UserImageAttachment {
                    path: "/tmp/codex-mobile/image-1.png".to_string(),
                }],
            )
            .await
            .expect("message sends");

        let requests = requests.lock().expect("requests lock");
        assert_eq!(requests[1].method, "turn/start");
        assert_eq!(
            requests[1].params,
            json!({
                "threadId": "thread-1",
                "clientUserMessageId": "codex-mobile-1",
                "input": [
                    { "type": "text", "text": "what is in this image?" },
                    { "type": "localImage", "path": "/tmp/codex-mobile/image-1.png" }
                ]
            })
        );
    }

    #[tokio::test]
    async fn adapter_starts_thread_through_mobile_host_signal() {
        let transport = RecordingTransport::new(vec![json!({
            "thread": {
                "id": "thread-new",
                "title": "New mobile task",
                "cwd": "/repo/mobile",
                "updatedAt": 1_725_000_000_000_u64
            }
        })]);
        let requests = transport.requests();
        let client = AppServerJsonRpcClient::new(transport);

        let thread = client
            .start_thread(
                "/repo/mobile",
                "start this from phone",
                &[UserImageAttachment {
                    path: "/tmp/codex-mobile/image-1.png".to_string(),
                }],
            )
            .await
            .expect("thread starts");

        assert_eq!(thread.id, "thread-new");
        assert_eq!(thread.title.as_deref(), Some("New mobile task"));
        let requests = requests.lock().expect("requests lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "codex-mobile/start-conversation");
        assert_eq!(
            requests[0].params,
            json!({
                "input": [
                    { "type": "text", "text": "start this from phone", "text_elements": [] },
                    { "type": "localImage", "path": "/tmp/codex-mobile/image-1.png" }
                ],
                "cwd": "/repo/mobile",
                "workspaceRoots": ["/repo/mobile"],
                "workspaceKind": "project",
                "collaborationMode": null,
                "serviceTier": null,
                "threadSource": "user",
                "approvalsReviewer": "user"
            })
        );
    }

    #[tokio::test]
    async fn adapter_maps_top_level_thread_start_response() {
        let transport = RecordingTransport::new(vec![json!({
            "id": "thread-top-level",
            "preview": "Top level thread",
            "updatedAt": 1_725_000_000_000_u64
        })]);
        let client = AppServerJsonRpcClient::new(transport);

        let thread = client
            .start_thread("/repo", "phone task", &[])
            .await
            .expect("thread starts");

        assert_eq!(thread.id, "thread-top-level");
        assert_eq!(thread.preview.as_deref(), Some("Top level thread"));
    }

    #[tokio::test]
    async fn adapter_maps_data_thread_start_response() {
        let transport = RecordingTransport::new(vec![json!({
            "data": {
                "id": "thread-data",
                "preview": "Data thread",
                "updatedAt": 1_725_000_000_000_u64
            }
        })]);
        let client = AppServerJsonRpcClient::new(transport);

        let thread = client
            .start_thread("/repo", "phone task", &[])
            .await
            .expect("thread starts");

        assert_eq!(thread.id, "thread-data");
        assert_eq!(thread.preview.as_deref(), Some("Data thread"));
    }

    #[tokio::test]
    async fn adapter_lists_pending_approvals_from_mobile_host_bridge() {
        let transport = RecordingTransport::new(vec![json!([
            {
                "threadId": "thread-approval",
                "requestId": "7",
                "method": "mcpServer/elicitation/request",
                "params": {
                    "message": "Allow read_memory?",
                    "_meta": {
                        "codex_approval_kind": "mcp_tool_call",
                        "tool_params": { "uri": "system://boot" }
                    }
                }
            }
        ])]);
        let requests = transport.requests();
        let client = AppServerJsonRpcClient::new(transport);

        let approvals = client
            .list_pending_approvals()
            .await
            .expect("pending approvals map");

        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].thread_id, "thread-approval");
        assert_eq!(approvals[0].request_id, "7");
        assert_eq!(approvals[0].method, "mcpServer/elicitation/request");
        assert_eq!(approvals[0].params["message"], json!("Allow read_memory?"));
        let requests = requests.lock().expect("requests lock");
        assert_eq!(requests[0].method, "codex-mobile/list-pending-approvals");
        assert_eq!(requests[0].params, json!({}));
    }

    #[tokio::test]
    async fn adapter_routes_approval_decision_through_mobile_host_bridge() {
        let transport = RecordingTransport::new(vec![json!({ "accepted": true })]);
        let requests = transport.requests();
        let client = AppServerJsonRpcClient::new(transport);
        let decision = ApprovalDecision {
            approval_id: "thread-approval:7".to_string(),
            decision: crate::protocol::DecisionKind::Approve,
            comment: None,
            device_id: "phone-1".to_string(),
            decided_at: 1_725_000_000_000,
        };

        client
            .respond_approval(&decision.approval_id, &decision)
            .await
            .expect("approval response sends");

        let requests = requests.lock().expect("requests lock");
        assert_eq!(requests[0].method, "codex-mobile/respond-approval");
        assert_eq!(
            requests[0].params,
            json!({
                "approvalId": "thread-approval:7",
                "decision": "approve"
            })
        );
    }

    #[derive(Clone)]
    struct RecordingTransport {
        requests: Arc<Mutex<Vec<JsonRpcRequest>>>,
        responses: Arc<Mutex<VecDeque<Value>>>,
    }

    impl RecordingTransport {
        fn new(responses: Vec<Value>) -> Self {
            Self {
                requests: Arc::new(Mutex::new(Vec::new())),
                responses: Arc::new(Mutex::new(responses.into())),
            }
        }

        fn requests(&self) -> Arc<Mutex<Vec<JsonRpcRequest>>> {
            self.requests.clone()
        }
    }

    #[async_trait]
    impl JsonRpcTransport for RecordingTransport {
        async fn send_request(&self, request: JsonRpcRequest) -> Result<Value, CodexRpcError> {
            self.requests.lock().expect("requests lock").push(request);
            self.responses
                .lock()
                .expect("responses lock")
                .pop_front()
                .ok_or_else(|| CodexRpcError::Transport("missing mock response".to_string()))
        }
    }
}
