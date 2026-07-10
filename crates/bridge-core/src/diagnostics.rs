use serde::Serialize;

use crate::{
    cdp::{BridgeConnectionState, CdpClient, CdpError, select_codex_target},
    codex_rpc::{AppServerJsonRpcClient, CdpAppServerTransport, CodexAdapter, JsonRpcTransport},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsReport {
    pub status: DiagnosticsStatus,
    pub connection_state: BridgeConnectionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticsStatus {
    Ok,
    Degraded,
}

impl DiagnosticsReport {
    pub fn ok(connection_state: BridgeConnectionState) -> Self {
        Self {
            status: DiagnosticsStatus::Ok,
            connection_state,
            detail: None,
        }
    }

    pub fn degraded(connection_state: BridgeConnectionState, detail: impl Into<String>) -> Self {
        Self {
            status: DiagnosticsStatus::Degraded,
            connection_state,
            detail: Some(detail.into()),
        }
    }
}

impl Default for DiagnosticsReport {
    fn default() -> Self {
        Self::degraded(
            BridgeConnectionState::CodexNotRunning,
            "Codex diagnostics have not run yet",
        )
    }
}

impl DiagnosticsStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "degraded",
        }
    }
}

pub async fn diagnose_cdp_app_server(cdp: &CdpClient) -> DiagnosticsReport {
    let targets = match cdp.list_targets().await {
        Ok(targets) => targets,
        Err(error) => {
            return DiagnosticsReport::degraded(
                BridgeConnectionState::CdpUnavailable,
                error.to_string(),
            );
        }
    };
    let target = match select_codex_target(&targets) {
        Ok(target) => target,
        Err(CdpError::NoCodexTarget) => {
            return DiagnosticsReport::degraded(
                BridgeConnectionState::TargetNotFound,
                "No ChatGPT/Codex page target found on the CDP endpoint",
            );
        }
        Err(error) => {
            return DiagnosticsReport::degraded(
                BridgeConnectionState::CdpUnavailable,
                error.to_string(),
            );
        }
    };

    if let Err(error) = cdp.inject_app_server_bridge(&target).await {
        return DiagnosticsReport::degraded(BridgeConnectionState::InjectFailed, error.to_string());
    }

    let transport = CdpAppServerTransport::new(cdp.clone(), target);
    let client = AppServerJsonRpcClient::new(transport);
    diagnose_app_server(&client).await
}

pub async fn diagnose_app_server<T>(client: &AppServerJsonRpcClient<T>) -> DiagnosticsReport
where
    T: JsonRpcTransport,
{
    let threads = match client.list_threads().await {
        Ok(threads) => threads,
        Err(error) => {
            return DiagnosticsReport::degraded(
                BridgeConnectionState::RpcUnavailable,
                error.to_string(),
            );
        }
    };
    if let Some(first_thread) = threads.first() {
        if let Err(error) = client.list_turns(&first_thread.id).await {
            return DiagnosticsReport::degraded(
                BridgeConnectionState::RpcUnavailable,
                error.to_string(),
            );
        }
    }

    DiagnosticsReport::ok(BridgeConnectionState::Writable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::{Value, json};
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    #[tokio::test]
    async fn diagnostics_reports_writable_when_rpc_methods_pass_health_check() {
        let transport = ScriptedTransport::new(vec![
            Ok(json!({ "data": [{ "id": "thread-1" }] })),
            Ok(json!({ "data": [] })),
        ]);
        let client = AppServerJsonRpcClient::new(transport);

        let report = diagnose_app_server(&client).await;

        assert_eq!(report.status, DiagnosticsStatus::Ok);
        assert_eq!(report.connection_state, BridgeConnectionState::Writable);
    }

    #[tokio::test]
    async fn diagnostics_reports_writable_when_thread_list_passes_with_no_threads() {
        let transport = ScriptedTransport::new(vec![Ok(json!({ "data": [] }))]);
        let client = AppServerJsonRpcClient::new(transport);

        let report = diagnose_app_server(&client).await;

        assert_eq!(report.status, DiagnosticsStatus::Ok);
        assert_eq!(report.connection_state, BridgeConnectionState::Writable);
    }

    #[tokio::test]
    async fn diagnostics_reports_rpc_unavailable_when_turns_list_fails() {
        let transport = ScriptedTransport::new(vec![
            Ok(json!({ "data": [{ "id": "thread-1" }] })),
            Err("thread/turns/list unavailable"),
        ]);
        let client = AppServerJsonRpcClient::new(transport);

        let report = diagnose_app_server(&client).await;

        assert_eq!(report.status, DiagnosticsStatus::Degraded);
        assert_eq!(
            report.connection_state,
            BridgeConnectionState::RpcUnavailable
        );
        assert!(
            report
                .detail
                .expect("detail is present")
                .contains("thread/turns/list unavailable")
        );
    }

    #[derive(Clone)]
    struct ScriptedTransport {
        responses: Arc<Mutex<VecDeque<Result<Value, &'static str>>>>,
    }

    impl ScriptedTransport {
        fn new(responses: Vec<Result<Value, &'static str>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
            }
        }
    }

    #[async_trait]
    impl JsonRpcTransport for ScriptedTransport {
        async fn send_request(
            &self,
            _request: crate::codex_rpc::JsonRpcRequest,
        ) -> Result<Value, crate::codex_rpc::CodexRpcError> {
            match self
                .responses
                .lock()
                .expect("responses lock")
                .pop_front()
                .expect("mock response exists")
            {
                Ok(value) => Ok(value),
                Err(message) => Err(crate::codex_rpc::CodexRpcError::Transport(
                    message.to_string(),
                )),
            }
        }
    }
}
