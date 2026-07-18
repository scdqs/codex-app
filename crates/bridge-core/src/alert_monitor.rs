use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::Result;
use tokio::{sync::Mutex, time::sleep};

use crate::{
    alert_detector::detect_alerts, codex_rpc::CodexAdapter, normalizer::Normalizer,
    notification_dispatcher::NotificationDispatcher, notification_store::NotificationStore,
    protocol::SessionStatus,
};

#[derive(Debug, Clone)]
pub struct AlertMonitorConfig {
    pub active_poll: Duration,
    pub idle_poll: Duration,
    pub max_error_backoff: Duration,
}

impl Default for AlertMonitorConfig {
    fn default() -> Self {
        Self {
            active_poll: Duration::from_secs(5),
            idle_poll: Duration::from_secs(30),
            max_error_backoff: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorCycleOutcome {
    pub next_delay: Duration,
}

pub struct AlertMonitor {
    adapter: Arc<dyn CodexAdapter>,
    store: Arc<Mutex<NotificationStore>>,
    dispatcher: NotificationDispatcher,
    config: AlertMonitorConfig,
    consecutive_failures: u32,
}

impl AlertMonitor {
    pub fn new(
        adapter: Arc<dyn CodexAdapter>,
        store: Arc<Mutex<NotificationStore>>,
        dispatcher: NotificationDispatcher,
        config: AlertMonitorConfig,
    ) -> Self {
        Self {
            adapter,
            store,
            dispatcher,
            config,
            consecutive_failures: 0,
        }
    }

    pub async fn run_cycle(&mut self) -> Result<MonitorCycleOutcome> {
        let enabled = self.store.lock().await.enabled_settings()?;
        if enabled.is_empty() {
            return Ok(MonitorCycleOutcome {
                next_delay: self.config.idle_poll,
            });
        }

        let threads = self.adapter.list_threads().await?;
        let approvals = if enabled
            .iter()
            .any(|settings| settings.alert_kinds.approval_required)
        {
            self.adapter.list_pending_approvals().await?
        } else {
            Vec::new()
        };
        let mut approvals_by_thread: HashMap<String, Vec<String>> = HashMap::new();
        for approval in approvals {
            approvals_by_thread
                .entry(approval.thread_id)
                .or_default()
                .push(approval.request_id);
        }
        let snapshots = threads
            .iter()
            .map(Normalizer::snapshot_from_thread)
            .collect::<Vec<_>>();

        for snapshot in &snapshots {
            let events = {
                let store = self.store.lock().await;
                let previous = store.alert_state_for_thread(&snapshot.thread_id)?;
                let result = detect_alerts(
                    previous.as_ref(),
                    snapshot,
                    approvals_by_thread
                        .get(&snapshot.thread_id)
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                );
                if !result.ignored_as_stale {
                    store.save_alert_state(&result.next_state)?;
                }
                result.events
            };
            for event in events {
                self.dispatcher.dispatch(event).await?;
            }
        }

        let any_active = snapshots.iter().any(|snapshot| {
            matches!(
                snapshot.status,
                SessionStatus::Running
                    | SessionStatus::WaitingForInput
                    | SessionStatus::WaitingForApproval
                    | SessionStatus::Error
            )
        });
        Ok(MonitorCycleOutcome {
            next_delay: if any_active {
                self.config.active_poll
            } else {
                self.config.idle_poll
            },
        })
    }

    pub async fn run(mut self) {
        loop {
            let delay = match self.run_cycle().await {
                Ok(outcome) => {
                    self.consecutive_failures = 0;
                    outcome.next_delay
                }
                Err(error) => {
                    self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                    let seconds = 5_u64.saturating_mul(
                        1_u64 << self.consecutive_failures.saturating_sub(1).min(3),
                    );
                    eprintln!("alert monitor cycle failed: {}", error_category(&error));
                    Duration::from_secs(seconds).min(self.config.max_error_backoff)
                }
            };
            sleep(delay).await;
        }
    }
}

fn error_category(error: &anyhow::Error) -> &'static str {
    if error.downcast_ref::<rusqlite::Error>().is_some() {
        "notification_store"
    } else {
        "alert_monitor"
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Mutex as StdMutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::{
        codex_rpc::{
            CodexPendingApproval, CodexRpcError, CodexThread, CodexTurn, UserImageAttachment,
        },
        event_hub::EventHub,
        notification_store::{AlertKindSettings, DeviceNotificationSettings, SessionAlertState},
        protocol::{AlertKind, ApprovalDecision, ServerEnvelope, SessionSnapshot},
        storage::{Device, Storage},
    };

    #[tokio::test]
    async fn no_enabled_device_skips_adapter_and_uses_idle_poll() {
        let (_dir, store) = notification_store(false, true);
        let adapter = Arc::new(MonitorAdapter::default());
        let mut monitor = monitor(adapter.clone(), store, EventHub::new());

        let outcome = monitor.run_cycle().await.expect("cycle succeeds");

        assert_eq!(outcome.next_delay, Duration::from_secs(30));
        assert_eq!(adapter.thread_calls.load(Ordering::SeqCst), 0);
        assert_eq!(adapter.approval_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn active_and_idle_threads_select_the_expected_poll_interval() {
        let (_dir, store) = notification_store(true, false);
        let adapter = Arc::new(MonitorAdapter::with_threads(vec![thread(
            SessionStatus::Running,
            10,
        )]));
        let mut monitor = monitor(adapter.clone(), store, EventHub::new());

        let active = monitor.run_cycle().await.expect("active cycle succeeds");
        adapter.set_threads(vec![thread(SessionStatus::Idle, 20)]);
        let idle = monitor.run_cycle().await.expect("idle cycle succeeds");

        assert_eq!(active.next_delay, Duration::from_secs(5));
        assert_eq!(idle.next_delay, Duration::from_secs(30));
        assert_eq!(adapter.approval_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn native_approval_and_thread_snapshot_are_processed_in_one_cycle() {
        let (_dir, store) = notification_store(true, true);
        let adapter = Arc::new(MonitorAdapter::with_threads(vec![thread(
            SessionStatus::Running,
            10,
        )]));
        let hub = EventHub::new();
        let mut subscriber = hub.subscribe_for_device("phone-1").await;
        let mut monitor = monitor(adapter.clone(), store.clone(), hub);
        monitor.run_cycle().await.expect("baseline cycle succeeds");

        adapter.set_threads(vec![thread(SessionStatus::WaitingForApproval, 20)]);
        adapter.set_approvals(vec![CodexPendingApproval {
            thread_id: "thread-1".into(),
            request_id: "approval-1".into(),
            method: "item/commandExecution/requestApproval".into(),
            params: json!({}),
        }]);
        monitor.run_cycle().await.expect("approval cycle succeeds");

        let event = subscriber.recv().await.expect("approval alert arrives");
        assert!(matches!(
            event,
            ServerEnvelope::AlertEvent(ref alert) if alert.kind == AlertKind::ApprovalRequired
        ));
        let state = store
            .lock()
            .await
            .alert_state_for_thread("thread-1")
            .expect("state loads")
            .expect("state exists");
        assert_eq!(state.known_approval_ids, vec!["approval-1"]);
    }

    #[tokio::test]
    async fn adapter_error_does_not_write_partial_alert_state() {
        let (_dir, store) = notification_store(true, true);
        let adapter = Arc::new(MonitorAdapter::with_threads(vec![thread(
            SessionStatus::Running,
            10,
        )]));
        adapter.set_approval_error("approval RPC unavailable");
        let mut monitor = monitor(adapter, store.clone(), EventHub::new());

        assert!(monitor.run_cycle().await.is_err());
        assert!(
            store
                .lock()
                .await
                .alert_state_for_thread("thread-1")
                .expect("state query succeeds")
                .is_none()
        );
    }

    #[tokio::test]
    async fn realtime_state_write_deduplicates_the_following_poll_cycle() {
        let (_dir, store) = notification_store(true, false);
        let running = SessionAlertState {
            thread_id: "thread-1".into(),
            status: SessionStatus::Running,
            updated_at: 10,
            state_cycle: 0,
            known_approval_ids: Vec::new(),
            fallback_approval_cycle: None,
        };
        let idle_snapshot = snapshot(SessionStatus::Idle, 20);
        {
            let locked = store.lock().await;
            locked
                .save_alert_state(&running)
                .expect("baseline state saves");
            let realtime = detect_alerts(Some(&running), &idle_snapshot, &[]);
            assert_eq!(realtime.events.len(), 1);
            locked
                .save_alert_state(&realtime.next_state)
                .expect("realtime state saves");
        }

        let adapter = Arc::new(MonitorAdapter::with_threads(vec![thread(
            SessionStatus::Idle,
            20,
        )]));
        let hub = EventHub::new();
        let mut subscriber = hub.subscribe_for_device("phone-1").await;
        let mut monitor = monitor(adapter, store, hub);
        monitor.run_cycle().await.expect("poll cycle succeeds");

        assert!(
            tokio::time::timeout(Duration::from_millis(25), subscriber.recv())
                .await
                .is_err()
        );
    }

    fn monitor(
        adapter: Arc<MonitorAdapter>,
        store: Arc<Mutex<NotificationStore>>,
        hub: EventHub,
    ) -> AlertMonitor {
        AlertMonitor::new(
            adapter,
            store.clone(),
            NotificationDispatcher::new(store, hub),
            AlertMonitorConfig::default(),
        )
    }

    fn notification_store(
        enabled: bool,
        approval_required: bool,
    ) -> (TempDir, Arc<Mutex<NotificationStore>>) {
        let dir = tempfile::tempdir().expect("tempdir creates");
        let path = dir.path().join("bridge.sqlite");
        let storage = Storage::open(&path).expect("storage opens");
        storage
            .insert_device(&Device {
                device_id: "phone-1".into(),
                display_name: "Phone".into(),
                secret_hash: "hash".into(),
                paired_origin: None,
                created_at: 1,
                last_seen_at: 1,
                revoked_at: None,
            })
            .expect("device inserts");
        drop(storage);

        let store = NotificationStore::open(&path).expect("notification store opens");
        store
            .save_settings(&DeviceNotificationSettings {
                device_id: "phone-1".into(),
                enabled,
                alert_kinds: AlertKindSettings {
                    approval_required,
                    ..AlertKindSettings::default()
                },
                sound_enabled: true,
                vibration_enabled: true,
                updated_at: 1,
            })
            .expect("settings save");
        (dir, Arc::new(Mutex::new(store)))
    }

    fn thread(status: SessionStatus, updated_at: u64) -> CodexThread {
        let status = match status {
            SessionStatus::Idle => "idle",
            SessionStatus::Running => "running",
            SessionStatus::WaitingForInput => "waiting_for_input",
            SessionStatus::WaitingForApproval => "waiting_for_approval",
            SessionStatus::Error => "error",
        };
        CodexThread {
            id: "thread-1".into(),
            title: Some("Release".into()),
            cwd: None,
            model_provider: None,
            preview: None,
            created_at: Some(updated_at),
            updated_at: Some(updated_at),
            raw: json!({ "status": status }),
        }
    }

    fn snapshot(status: SessionStatus, updated_at: u64) -> SessionSnapshot {
        SessionSnapshot {
            thread_id: "thread-1".into(),
            title: "Release".into(),
            cwd: None,
            model_provider: None,
            preview: None,
            updated_at,
            status,
            pending_approval_ids: Vec::new(),
        }
    }

    #[derive(Default)]
    struct MonitorAdapter {
        threads: StdMutex<Vec<CodexThread>>,
        approvals: StdMutex<Vec<CodexPendingApproval>>,
        approval_error: StdMutex<Option<String>>,
        thread_calls: AtomicUsize,
        approval_calls: AtomicUsize,
    }

    impl MonitorAdapter {
        fn with_threads(threads: Vec<CodexThread>) -> Self {
            Self {
                threads: StdMutex::new(threads),
                ..Self::default()
            }
        }

        fn set_threads(&self, threads: Vec<CodexThread>) {
            *self.threads.lock().expect("threads lock") = threads;
        }

        fn set_approvals(&self, approvals: Vec<CodexPendingApproval>) {
            *self.approvals.lock().expect("approvals lock") = approvals;
            *self.approval_error.lock().expect("approval error lock") = None;
        }

        fn set_approval_error(&self, message: &str) {
            *self.approval_error.lock().expect("approval error lock") = Some(message.into());
        }
    }

    #[async_trait]
    impl CodexAdapter for MonitorAdapter {
        async fn list_threads(&self) -> Result<Vec<CodexThread>, CodexRpcError> {
            self.thread_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.threads.lock().expect("threads lock").clone())
        }

        async fn start_thread(
            &self,
            _cwd: &str,
            _text: &str,
            _attachments: &[UserImageAttachment],
        ) -> Result<CodexThread, CodexRpcError> {
            Err(unsupported("thread/start"))
        }

        async fn resume_thread(
            &self,
            _thread_id: &str,
        ) -> Result<Option<CodexThread>, CodexRpcError> {
            Err(unsupported("thread/resume"))
        }

        async fn list_turns(&self, _thread_id: &str) -> Result<Vec<CodexTurn>, CodexRpcError> {
            Err(unsupported("thread/turns/list"))
        }

        async fn send_user_message(
            &self,
            _thread_id: &str,
            _text: &str,
            _attachments: &[UserImageAttachment],
        ) -> Result<(), CodexRpcError> {
            Err(unsupported("turn/start"))
        }

        async fn list_pending_approvals(&self) -> Result<Vec<CodexPendingApproval>, CodexRpcError> {
            self.approval_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(message) = self
                .approval_error
                .lock()
                .expect("approval error lock")
                .clone()
            {
                return Err(CodexRpcError::Transport(message));
            }
            Ok(self.approvals.lock().expect("approvals lock").clone())
        }

        async fn subscribe_events(&self, _thread_id: Option<&str>) -> Result<(), CodexRpcError> {
            Err(unsupported("subscribe"))
        }

        async fn respond_approval(
            &self,
            _approval_id: &str,
            _decision: &ApprovalDecision,
        ) -> Result<(), CodexRpcError> {
            Err(unsupported("approval/respond"))
        }
    }

    fn unsupported(method: &'static str) -> CodexRpcError {
        CodexRpcError::Unsupported { method }
    }
}
