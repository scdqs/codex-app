use std::{sync::Arc, time::Duration};

use anyhow::Result;
use tokio::sync::{Mutex, Notify};

use crate::{
    notification_store::{NotificationDelivery, NotificationStore},
    public_access::{PublicAccessMode, PublicAccessState},
    web_push::{PushFailureClass, PushPayload, WebPushSender},
};

const MAX_IDLE_WAIT: Duration = Duration::from_secs(30);
const RETRY_DELAYS_MS: [u64; 3] = [1_000, 2_000, 4_000];

pub struct PushDeliveryWorker {
    store: Arc<Mutex<NotificationStore>>,
    sender: WebPushSender,
    public_access: PublicAccessState,
    wake: Arc<Notify>,
}

impl PushDeliveryWorker {
    pub fn new(
        store: Arc<Mutex<NotificationStore>>,
        sender: WebPushSender,
        public_access: PublicAccessState,
        wake: Arc<Notify>,
    ) -> Self {
        Self {
            store,
            sender,
            public_access,
            wake,
        }
    }

    pub async fn drain_due_once(&self) -> Result<usize> {
        self.drain_due_once_at(current_time_ms()).await
    }

    async fn drain_due_once_at(&self, now: u64) -> Result<usize> {
        if self.public_access.current().await.mode != PublicAccessMode::Named {
            return Ok(0);
        }
        let mut processed = 0;
        loop {
            let delivery = self.store.lock().await.claim_next_due_delivery(now)?;
            let Some(delivery) = delivery else {
                break;
            };
            processed += 1;
            self.process_delivery(delivery, now).await?;
        }
        Ok(processed)
    }

    async fn process_delivery(&self, delivery: NotificationDelivery, now: u64) -> Result<()> {
        let payload = match serde_json::from_str::<PushPayload>(&delivery.payload_json) {
            Ok(payload) => payload,
            Err(_) => {
                self.store.lock().await.mark_delivery_failed(
                    &delivery.event_id,
                    &delivery.device_id,
                    "invalid_push_payload",
                    now,
                )?;
                return Ok(());
            }
        };
        let subscription = {
            let store = self.store.lock().await;
            match store.active_subscription(&delivery.device_id)? {
                Some(subscription) => subscription,
                None => {
                    store.mark_delivery_invalid_subscription(
                        &delivery.event_id,
                        &delivery.device_id,
                        now,
                    )?;
                    return Ok(());
                }
            }
        };

        let latest_access = self.public_access.current().await;
        if latest_access.mode != PublicAccessMode::Named
            || latest_access.public_origin.as_deref() != Some(subscription.origin.as_str())
        {
            let store = self.store.lock().await;
            store.invalidate_subscription(&delivery.device_id, now)?;
            store.mark_delivery_invalid_subscription(
                &delivery.event_id,
                &delivery.device_id,
                now,
            )?;
            return Ok(());
        }

        let result = self.sender.send(&subscription, &payload).await;
        let store = self.store.lock().await;
        match result {
            Ok(()) => store.mark_delivery_sent(&delivery.event_id, &delivery.device_id, now)?,
            Err(PushFailureClass::InvalidSubscription) => {
                store.invalidate_subscription(&delivery.device_id, now)?;
                store.mark_delivery_invalid_subscription(
                    &delivery.event_id,
                    &delivery.device_id,
                    now,
                )?;
            }
            Err(PushFailureClass::Retryable) if delivery.attempt_count <= 3 => {
                let retry_index = delivery.attempt_count.saturating_sub(1) as usize;
                store.mark_delivery_retry(
                    &delivery.event_id,
                    &delivery.device_id,
                    now.saturating_add(RETRY_DELAYS_MS[retry_index]),
                    PushFailureClass::Retryable.as_str(),
                    now,
                )?;
            }
            Err(class) => store.mark_delivery_failed(
                &delivery.event_id,
                &delivery.device_id,
                class.as_str(),
                now,
            )?,
        }
        Ok(())
    }

    pub async fn run(self) {
        loop {
            if self.drain_due_once().await.is_err() {
                eprintln!("push delivery worker cycle failed: push_delivery_worker");
            }
            let delay = self.next_wait().await;
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = self.wake.notified() => {}
            }
        }
    }

    async fn next_wait(&self) -> Duration {
        if self.public_access.current().await.mode != PublicAccessMode::Named {
            return MAX_IDLE_WAIT;
        }
        let now = current_time_ms();
        match self.store.lock().await.next_delivery_due_at() {
            Ok(Some(due_at)) => {
                Duration::from_millis(due_at.saturating_sub(now)).min(MAX_IDLE_WAIT)
            }
            Ok(None) | Err(_) => MAX_IDLE_WAIT,
        }
    }
}

fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time is after unix epoch")
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, VecDeque},
        sync::Mutex as StdMutex,
    };

    use async_trait::async_trait;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    use super::*;
    use crate::{
        notification_store::{DeliveryStatus, PushSubscriptionRecord},
        protocol::{AlertEvent, AlertKind},
        public_access::PublicAccessContext,
        vapid::VapidRuntimeKey,
        web_push::{DeliveryHints, WebPushTransport, WebPushTransportError},
    };

    #[tokio::test]
    async fn one_invalid_subscription_does_not_block_other_devices() {
        let transport = Arc::new(ScriptedTransport::new([
            ("phone-a", vec![Err(WebPushTransportError::HttpStatus(410))]),
            ("phone-b", vec![Ok(())]),
        ]));
        let harness = WorkerHarness::new(transport.clone()).await;
        harness
            .subscribe("phone-a", "https://codex.example.com")
            .await;
        harness
            .subscribe("phone-b", "https://codex.example.com")
            .await;
        harness.enqueue("event-1", "phone-a", 1).await;
        harness.enqueue("event-1", "phone-b", 1).await;

        harness
            .worker
            .drain_due_once_at(10)
            .await
            .expect("worker drains");

        assert_eq!(
            harness.status("event-1", "phone-a").await,
            DeliveryStatus::InvalidSubscription
        );
        assert_eq!(
            harness.status("event-1", "phone-b").await,
            DeliveryStatus::Sent
        );
        assert!(harness.subscription("phone-a").await.is_none());
        assert!(harness.subscription("phone-b").await.is_some());
        assert_eq!(transport.calls("phone-a"), 1);
        assert_eq!(transport.calls("phone-b"), 1);
    }

    #[tokio::test]
    async fn retryable_failure_sends_at_most_four_times() {
        let transport = Arc::new(ScriptedTransport::new([(
            "phone-a",
            vec![
                Err(WebPushTransportError::HttpStatus(503)),
                Err(WebPushTransportError::HttpStatus(503)),
                Err(WebPushTransportError::HttpStatus(503)),
                Err(WebPushTransportError::HttpStatus(503)),
                Ok(()),
            ],
        )]));
        let harness = WorkerHarness::new(transport.clone()).await;
        harness
            .subscribe("phone-a", "https://codex.example.com")
            .await;
        harness.enqueue("event-1", "phone-a", 100).await;

        for now in [100, 1_100, 3_100, 7_100, 20_000] {
            harness
                .worker
                .drain_due_once_at(now)
                .await
                .expect("worker drains");
        }

        let delivery = harness.delivery("event-1", "phone-a").await;
        assert_eq!(delivery.status, DeliveryStatus::Failed);
        assert_eq!(delivery.attempt_count, 4);
        assert_eq!(transport.calls("phone-a"), 4);
    }

    #[tokio::test]
    async fn origin_change_invalidates_before_transport_send() {
        let transport = Arc::new(ScriptedTransport::default());
        let harness = WorkerHarness::new(transport.clone()).await;
        harness
            .subscribe("phone-a", "https://old.example.com")
            .await;
        harness.enqueue("event-1", "phone-a", 1).await;

        harness
            .worker
            .drain_due_once_at(10)
            .await
            .expect("worker drains");

        assert_eq!(
            harness.status("event-1", "phone-a").await,
            DeliveryStatus::InvalidSubscription
        );
        assert_eq!(transport.calls("phone-a"), 0);
    }

    #[derive(Default)]
    struct ScriptedTransport {
        results: StdMutex<HashMap<String, VecDeque<Result<(), WebPushTransportError>>>>,
        calls: StdMutex<HashMap<String, usize>>,
    }

    impl ScriptedTransport {
        fn new<const N: usize>(
            results: [(&str, Vec<Result<(), WebPushTransportError>>); N],
        ) -> Self {
            Self {
                results: StdMutex::new(
                    results
                        .into_iter()
                        .map(|(device, results)| (device.to_string(), results.into()))
                        .collect(),
                ),
                calls: StdMutex::new(HashMap::new()),
            }
        }

        fn calls(&self, device_id: &str) -> usize {
            self.calls
                .lock()
                .expect("calls lock")
                .get(device_id)
                .copied()
                .unwrap_or_default()
        }
    }

    #[async_trait]
    impl WebPushTransport for ScriptedTransport {
        async fn send(
            &self,
            subscription: &PushSubscriptionRecord,
            _payload: &[u8],
            _vapid_private_key_base64: &str,
        ) -> Result<(), WebPushTransportError> {
            *self
                .calls
                .lock()
                .expect("calls lock")
                .entry(subscription.device_id.clone())
                .or_default() += 1;
            self.results
                .lock()
                .expect("results lock")
                .get_mut(&subscription.device_id)
                .and_then(VecDeque::pop_front)
                .unwrap_or(Ok(()))
        }
    }

    struct WorkerHarness {
        worker: PushDeliveryWorker,
        store: Arc<Mutex<NotificationStore>>,
    }

    impl WorkerHarness {
        async fn new(transport: Arc<dyn WebPushTransport>) -> Self {
            let store = Arc::new(Mutex::new(
                NotificationStore::open_in_memory().expect("store opens"),
            ));
            let access = PublicAccessState::default();
            access
                .update(PublicAccessContext {
                    mode: PublicAccessMode::Named,
                    public_origin: Some("https://codex.example.com".into()),
                })
                .await
                .expect("access updates");
            let sender = WebPushSender::new(transport, test_vapid_key());
            let worker = PushDeliveryWorker::new(
                Arc::clone(&store),
                sender,
                access,
                Arc::new(Notify::new()),
            );
            Self { worker, store }
        }

        async fn subscribe(&self, device_id: &str, origin: &str) {
            self.store
                .lock()
                .await
                .save_subscription(&PushSubscriptionRecord {
                    device_id: device_id.into(),
                    origin: origin.into(),
                    endpoint: format!("https://push.example/{device_id}"),
                    p256dh: URL_SAFE_NO_PAD.encode([2_u8; 65]),
                    auth: URL_SAFE_NO_PAD.encode([3_u8; 16]),
                    created_at: 1,
                    last_success_at: None,
                    invalidated_at: None,
                })
                .expect("subscription saves");
        }

        async fn enqueue(&self, event_id: &str, device_id: &str, due_at: u64) {
            let payload = PushPayload::for_event(
                &AlertEvent {
                    event_id: event_id.into(),
                    kind: AlertKind::Completed,
                    thread_id: "thread-1".into(),
                    thread_title: "Release".into(),
                    occurred_at: due_at,
                },
                DeliveryHints {
                    sound_enabled: true,
                    vibration_enabled: true,
                    force_system_notification: false,
                },
            );
            self.store
                .lock()
                .await
                .enqueue_delivery(&NotificationDelivery {
                    event_id: event_id.into(),
                    device_id: device_id.into(),
                    payload_json: serde_json::to_string(&payload).expect("payload serializes"),
                    status: DeliveryStatus::Pending,
                    attempt_count: 0,
                    next_attempt_at: due_at,
                    last_error_category: None,
                    updated_at: due_at,
                })
                .expect("delivery enqueues");
        }

        async fn delivery(&self, event_id: &str, device_id: &str) -> NotificationDelivery {
            self.store
                .lock()
                .await
                .delivery_for(event_id, device_id)
                .expect("delivery loads")
                .expect("delivery exists")
        }

        async fn status(&self, event_id: &str, device_id: &str) -> DeliveryStatus {
            self.delivery(event_id, device_id).await.status
        }

        async fn subscription(&self, device_id: &str) -> Option<PushSubscriptionRecord> {
            self.store
                .lock()
                .await
                .active_subscription(device_id)
                .expect("subscription loads")
        }
    }

    fn test_vapid_key() -> Arc<VapidRuntimeKey> {
        let dir = tempfile::tempdir().expect("tempdir creates");
        let path = dir.path().join("vapid-key");
        std::fs::write(&path, URL_SAFE_NO_PAD.encode([1_u8; 32])).expect("fixture writes");
        Arc::new(VapidRuntimeKey::from_secret_file(&path).expect("VAPID key loads"))
    }
}
