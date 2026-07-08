use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    cdp::{CdpClient, CdpTarget},
    protocol::ApprovalDecision,
};

#[async_trait]
pub trait CodexAdapter: Send + Sync {
    async fn list_threads(&self) -> Result<Vec<CodexThread>, CodexRpcError>;
    async fn resume_thread(&self, thread_id: &str) -> Result<Option<CodexThread>, CodexRpcError>;
    async fn list_turns(&self, thread_id: &str) -> Result<Vec<CodexTurn>, CodexRpcError>;
    async fn send_user_message(&self, thread_id: &str, text: &str) -> Result<(), CodexRpcError>;
    async fn subscribe_events(&self, thread_id: Option<&str>) -> Result<(), CodexRpcError>;
    async fn respond_approval(
        &self,
        approval_id: &str,
        decision: &ApprovalDecision,
    ) -> Result<(), CodexRpcError>;
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
pub struct CodexRawEvent {
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
        let expression = format!(
            r#"(async () => {{
  const bridge = globalThis.__codexMobileBridge;
  if (!bridge || typeof bridge.rpc !== "function") {{
    throw new Error("Codex mobile bridge is not injected");
  }}
  return await bridge.rpc({request_json});
}})()"#
        );

        self.cdp
            .evaluate_on_target(&self.target, &expression)
            .await
            .map_err(|error| CodexRpcError::Transport(error.to_string()))
    }
}

#[async_trait]
impl<T> CodexAdapter for AppServerJsonRpcClient<T>
where
    T: JsonRpcTransport,
{
    async fn list_threads(&self) -> Result<Vec<CodexThread>, CodexRpcError> {
        let result = self.call("thread/list", json!({})).await?;
        let items = extract_array(&result, &["data", "threads", "items"]).ok_or(
            CodexRpcError::InvalidResponse {
                method: "thread/list",
                reason: "missing thread array",
            },
        )?;

        items.iter().map(map_thread).collect::<Result<Vec<_>, _>>()
    }

    async fn resume_thread(&self, thread_id: &str) -> Result<Option<CodexThread>, CodexRpcError> {
        let result = self
            .call("thread/resume", json!({ "threadId": thread_id }))
            .await?;
        Ok(extract_thread_value(&result).map(map_thread).transpose()?)
    }

    async fn list_turns(&self, thread_id: &str) -> Result<Vec<CodexTurn>, CodexRpcError> {
        let result = self
            .call("thread/turns/list", json!({ "threadId": thread_id }))
            .await?;
        let items = extract_turn_values(&result).ok_or(CodexRpcError::InvalidResponse {
            method: "thread/turns/list",
            reason: "missing turn array",
        })?;

        Ok(items.into_iter().map(map_turn).collect())
    }

    async fn send_user_message(&self, thread_id: &str, text: &str) -> Result<(), CodexRpcError> {
        self.resume_thread(thread_id).await?;
        let client_user_message_id = format!(
            "codex-mobile-{}",
            self.next_client_message_id.fetch_add(1, Ordering::SeqCst)
        );
        self.call(
            "turn/start",
            json!({
                "threadId": thread_id,
                "clientUserMessageId": client_user_message_id,
                "input": [{ "type": "text", "text": text }],
            }),
        )
        .await?;

        Ok(())
    }

    async fn subscribe_events(&self, _thread_id: Option<&str>) -> Result<(), CodexRpcError> {
        Err(CodexRpcError::Unsupported {
            method: "subscribe_events",
        })
    }

    async fn respond_approval(
        &self,
        _approval_id: &str,
        _decision: &ApprovalDecision,
    ) -> Result<(), CodexRpcError> {
        Err(CodexRpcError::Unsupported {
            method: "respond_approval",
        })
    }
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
        updated_at: number_field(value, &["updatedAt", "updated_at"]),
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
    async fn adapter_sends_turn_start_for_user_text() {
        let transport = RecordingTransport::new(vec![json!({}), json!({ "accepted": true })]);
        let requests = transport.requests();
        let client = AppServerJsonRpcClient::new(transport);

        client
            .send_user_message("thread-1", "same text")
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
