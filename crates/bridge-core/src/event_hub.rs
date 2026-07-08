use std::{collections::HashMap, sync::Arc};

use tokio::sync::{RwLock, broadcast};

use crate::protocol::{ServerEnvelope, SessionSnapshot};

const DEFAULT_EVENT_BUFFER_CAPACITY: usize = 256;

#[derive(Debug, Clone)]
pub struct EventHub {
    inner: Arc<EventHubInner>,
}

#[derive(Debug)]
struct EventHubInner {
    events: broadcast::Sender<ServerEnvelope>,
    snapshots: Arc<RwLock<HashMap<String, SessionSnapshot>>>,
}

impl EventHub {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_EVENT_BUFFER_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let (events, _) = broadcast::channel(capacity);

        Self {
            inner: Arc::new(EventHubInner {
                events,
                snapshots: Arc::new(RwLock::new(HashMap::new())),
            }),
        }
    }

    pub fn publish(&self, envelope: ServerEnvelope) -> usize {
        self.inner.events.send(envelope).unwrap_or(0)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ServerEnvelope> {
        self.inner.events.subscribe()
    }

    pub async fn set_snapshot(&self, snapshot: SessionSnapshot) -> usize {
        self.inner
            .snapshots
            .write()
            .await
            .insert(snapshot.thread_id.clone(), snapshot.clone());

        self.publish(ServerEnvelope::SessionSnapshot(snapshot))
    }

    pub async fn snapshot_for_thread(&self, thread_id: &str) -> Option<SessionSnapshot> {
        self.inner.snapshots.read().await.get(thread_id).cloned()
    }

    pub async fn all_snapshots(&self) -> Vec<SessionSnapshot> {
        let mut snapshots = self
            .inner
            .snapshots
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();

        snapshots.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.thread_id.cmp(&right.thread_id))
        });

        snapshots
    }
}

impl Default for EventHub {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{SessionEvent, SessionEventType, SessionStatus};
    use serde_json::json;

    #[tokio::test]
    async fn subscriber_receives_published_session_event() {
        let hub = EventHub::new();
        let mut subscriber = hub.subscribe();
        let event = SessionEvent {
            id: "event-1".to_string(),
            thread_id: "thread-1".to_string(),
            event_type: SessionEventType::Message,
            payload: json!({ "role": "assistant", "text": "hello" }),
            created_at: 1_725_000_000_000,
        };
        let envelope = ServerEnvelope::SessionEvent(event);

        let receivers = hub.publish(envelope.clone());

        assert_eq!(receivers, 1);
        assert_eq!(
            subscriber.recv().await.expect("subscriber receives event"),
            envelope
        );
    }

    #[tokio::test]
    async fn latest_snapshot_is_replayed_to_new_subscriber() {
        let hub = EventHub::new();
        let older_snapshot = snapshot("thread-old", 1_725_000_000_000);
        let newest_snapshot = snapshot("thread-new", 1_725_000_000_100);

        hub.set_snapshot(older_snapshot.clone()).await;
        hub.set_snapshot(newest_snapshot.clone()).await;

        let _subscriber = hub.subscribe();

        assert_eq!(
            hub.snapshot_for_thread("thread-new").await,
            Some(newest_snapshot.clone())
        );
        assert_eq!(hub.snapshot_for_thread("missing").await, None);
        assert_eq!(
            hub.all_snapshots().await,
            vec![newest_snapshot, older_snapshot]
        );
    }

    fn snapshot(thread_id: &str, updated_at: u64) -> SessionSnapshot {
        SessionSnapshot {
            thread_id: thread_id.to_string(),
            title: format!("Session {thread_id}"),
            cwd: Some("/tmp/project".to_string()),
            model_provider: Some("openai".to_string()),
            preview: Some("Latest message".to_string()),
            updated_at,
            status: SessionStatus::Idle,
            pending_approval_ids: Vec::new(),
        }
    }
}
