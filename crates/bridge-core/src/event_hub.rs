use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use thiserror::Error;
use tokio::sync::{RwLock, broadcast};

use crate::protocol::{ServerEnvelope, SessionSnapshot};

const DEFAULT_EVENT_BUFFER_CAPACITY: usize = 256;

#[derive(Debug, Clone)]
pub struct EventHub {
    inner: Arc<EventHubInner>,
}

pub struct EventSubscriber {
    replay: VecDeque<ServerEnvelope>,
    receiver: broadcast::Receiver<PublishedEvent>,
    device_id: Option<String>,
}

#[derive(Debug)]
struct EventHubInner {
    events: broadcast::Sender<PublishedEvent>,
    snapshots: Arc<RwLock<HashMap<String, SessionSnapshot>>>,
}

#[derive(Debug, Clone)]
enum PublishedEvent {
    Envelope {
        target_device_id: Option<String>,
        envelope: ServerEnvelope,
    },
    DisconnectDevice {
        device_id: String,
    },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EventReceiveError {
    #[error("device was disconnected")]
    DeviceDisconnected,
    #[error("event stream lagged by {0} messages")]
    Lagged(u64),
    #[error("event stream closed")]
    Closed,
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
        self.inner
            .events
            .send(PublishedEvent::Envelope {
                target_device_id: None,
                envelope,
            })
            .unwrap_or(0)
    }

    pub fn publish_to_device(
        &self,
        device_id: impl Into<String>,
        envelope: ServerEnvelope,
    ) -> usize {
        self.inner
            .events
            .send(PublishedEvent::Envelope {
                target_device_id: Some(device_id.into()),
                envelope,
            })
            .unwrap_or(0)
    }

    pub fn disconnect_device(&self, device_id: impl Into<String>) -> usize {
        self.inner
            .events
            .send(PublishedEvent::DisconnectDevice {
                device_id: device_id.into(),
            })
            .unwrap_or(0)
    }

    pub async fn subscribe(&self) -> EventSubscriber {
        self.subscribe_with_device(None).await
    }

    pub async fn subscribe_for_device(&self, device_id: impl Into<String>) -> EventSubscriber {
        self.subscribe_with_device(Some(device_id.into())).await
    }

    async fn subscribe_with_device(&self, device_id: Option<String>) -> EventSubscriber {
        let receiver = self.inner.events.subscribe();
        let replay = self
            .all_snapshots()
            .await
            .into_iter()
            .map(ServerEnvelope::SessionSnapshot)
            .collect();

        EventSubscriber {
            replay,
            receiver,
            device_id,
        }
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

impl EventSubscriber {
    pub async fn recv(&mut self) -> Result<ServerEnvelope, EventReceiveError> {
        if let Some(envelope) = self.replay.pop_front() {
            return Ok(envelope);
        }

        loop {
            match self.receiver.recv().await {
                Ok(PublishedEvent::Envelope {
                    target_device_id: None,
                    envelope,
                }) => return Ok(envelope),
                Ok(PublishedEvent::Envelope {
                    target_device_id: Some(target),
                    envelope,
                }) if self.device_id.as_deref() == Some(target.as_str()) => return Ok(envelope),
                Ok(PublishedEvent::DisconnectDevice { device_id })
                    if self.device_id.as_deref() == Some(device_id.as_str()) =>
                {
                    return Err(EventReceiveError::DeviceDisconnected);
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    return Err(EventReceiveError::Lagged(count));
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(EventReceiveError::Closed);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{AlertEvent, AlertKind, SessionEvent, SessionEventType, SessionStatus};
    use serde_json::json;
    use std::time::Duration;

    #[tokio::test]
    async fn subscriber_receives_published_session_event() {
        let hub = EventHub::new();
        let mut subscriber = hub.subscribe().await;
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
        let same_time_snapshot = snapshot("thread-alpha", 1_725_000_000_100);

        hub.set_snapshot(older_snapshot.clone()).await;
        hub.set_snapshot(newest_snapshot.clone()).await;
        hub.set_snapshot(same_time_snapshot.clone()).await;

        let mut subscriber = hub.subscribe().await;

        assert_eq!(
            subscriber.recv().await.expect("newest snapshot replays"),
            ServerEnvelope::SessionSnapshot(same_time_snapshot.clone())
        );
        assert_eq!(
            subscriber
                .recv()
                .await
                .expect("same-time snapshot replays by thread id"),
            ServerEnvelope::SessionSnapshot(newest_snapshot.clone())
        );
        assert_eq!(
            subscriber.recv().await.expect("older snapshot replays"),
            ServerEnvelope::SessionSnapshot(older_snapshot.clone())
        );
        assert_eq!(
            hub.all_snapshots().await,
            vec![same_time_snapshot, newest_snapshot, older_snapshot]
        );
    }

    #[tokio::test]
    async fn targeted_alert_is_only_received_by_the_matching_device() {
        let hub = EventHub::new();
        let mut phone_a = hub.subscribe_for_device("phone-a").await;
        let mut phone_b = hub.subscribe_for_device("phone-b").await;
        let alert = ServerEnvelope::AlertEvent(alert("alert-1"));

        hub.publish_to_device("phone-a", alert.clone());

        assert_eq!(phone_a.recv().await.expect("phone A receives alert"), alert);
        assert!(
            tokio::time::timeout(Duration::from_millis(25), phone_b.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn global_session_events_still_reach_every_device() {
        let hub = EventHub::new();
        let mut phone_a = hub.subscribe_for_device("phone-a").await;
        let mut phone_b = hub.subscribe_for_device("phone-b").await;
        let envelope = ServerEnvelope::SessionEvent(session_event());

        hub.publish(envelope.clone());

        assert_eq!(
            phone_a.recv().await.expect("phone A receives event"),
            envelope
        );
        assert_eq!(
            phone_b.recv().await.expect("phone B receives event"),
            envelope
        );
    }

    #[tokio::test]
    async fn disconnect_signal_closes_only_the_revoked_device_subscriber() {
        let hub = EventHub::new();
        let mut phone_a = hub.subscribe_for_device("phone-a").await;
        let mut phone_b = hub.subscribe_for_device("phone-b").await;

        hub.disconnect_device("phone-a");

        assert_eq!(
            phone_a.recv().await,
            Err(EventReceiveError::DeviceDisconnected)
        );
        hub.publish(ServerEnvelope::SessionEvent(session_event()));
        assert!(matches!(
            phone_b.recv().await,
            Ok(ServerEnvelope::SessionEvent(_))
        ));
    }

    fn alert(event_id: &str) -> AlertEvent {
        AlertEvent {
            event_id: event_id.to_string(),
            kind: AlertKind::Completed,
            thread_id: "thread-1".to_string(),
            thread_title: "Task".to_string(),
            occurred_at: 1,
        }
    }

    fn session_event() -> SessionEvent {
        SessionEvent {
            id: "event-global".to_string(),
            thread_id: "thread-1".to_string(),
            event_type: SessionEventType::Message,
            payload: json!({ "role": "assistant", "text": "done" }),
            created_at: 1,
        }
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
