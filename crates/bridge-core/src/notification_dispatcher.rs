use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{Mutex, Notify};

use crate::{
    event_hub::EventHub,
    notification_store::{
        DeliveryStatus, DeviceNotificationSettings, NotificationDelivery, NotificationStore,
    },
    protocol::{AlertEvent, ServerEnvelope},
    public_access::{PublicAccessContext, PublicAccessMode, PublicAccessState},
    web_push::{DeliveryHints, PushPayload},
};

#[derive(Clone)]
pub struct NotificationDispatcher {
    store: Arc<Mutex<NotificationStore>>,
    event_hub: EventHub,
    push_runtime: Option<PushDispatchRuntime>,
}

#[derive(Clone)]
pub struct PushDispatchRuntime {
    public_access: PublicAccessState,
    wake: Arc<Notify>,
}

impl NotificationDispatcher {
    pub fn new(store: Arc<Mutex<NotificationStore>>, event_hub: EventHub) -> Self {
        Self {
            store,
            event_hub,
            push_runtime: None,
        }
    }

    pub fn with_push_runtime(
        mut self,
        public_access: PublicAccessState,
        wake: Arc<Notify>,
    ) -> Self {
        self.push_runtime = Some(PushDispatchRuntime {
            public_access,
            wake,
        });
        self
    }

    pub async fn dispatch(&self, event: AlertEvent) -> Result<usize> {
        let access = self.current_push_access().await;
        let targets = self
            .store
            .lock()
            .await
            .enabled_settings()?
            .into_iter()
            .filter(|settings| settings.kind_enabled(event.kind))
            .collect::<Vec<_>>();
        let mut deliveries = 0;
        for settings in targets {
            deliveries += self.event_hub.publish_to_device(
                settings.device_id.clone(),
                ServerEnvelope::AlertEvent(event.clone()),
            );
            self.enqueue_push(&settings, &event, false, access.as_ref())
                .await?;
        }
        Ok(deliveries)
    }

    pub async fn dispatch_test_to_device(
        &self,
        device_id: &str,
        event: AlertEvent,
    ) -> Result<usize> {
        let access = self.current_push_access().await;
        let settings = self.store.lock().await.settings_for_device(device_id)?;
        if self
            .enqueue_push(&settings, &event, true, access.as_ref())
            .await?
        {
            return Ok(0);
        }
        Ok(self
            .event_hub
            .publish_to_device(device_id.to_string(), ServerEnvelope::AlertEvent(event)))
    }

    async fn current_push_access(&self) -> Option<PublicAccessContext> {
        match self.push_runtime.as_ref() {
            Some(runtime) => Some(runtime.public_access.current().await),
            None => None,
        }
    }

    async fn enqueue_push(
        &self,
        settings: &DeviceNotificationSettings,
        event: &AlertEvent,
        force_system_notification: bool,
        access: Option<&PublicAccessContext>,
    ) -> Result<bool> {
        let Some(runtime) = self.push_runtime.as_ref() else {
            return Ok(false);
        };
        let Some(access) = access else {
            return Ok(false);
        };
        if access.mode != PublicAccessMode::Named {
            return Ok(false);
        }
        let Some(public_origin) = access.public_origin.as_deref() else {
            return Ok(false);
        };
        let now = current_time_ms();
        let payload = PushPayload::for_event(
            event,
            DeliveryHints {
                sound_enabled: settings.sound_enabled,
                vibration_enabled: settings.vibration_enabled,
                force_system_notification,
            },
        );
        let enqueued = {
            let store = self.store.lock().await;
            let Some(subscription) = store.active_subscription(&settings.device_id)? else {
                return Ok(false);
            };
            if subscription.origin != public_origin {
                return Ok(false);
            }
            store.enqueue_delivery(&NotificationDelivery {
                event_id: event.event_id.clone(),
                device_id: settings.device_id.clone(),
                payload_json: serde_json::to_string(&payload)?,
                status: DeliveryStatus::Pending,
                attempt_count: 0,
                next_attempt_at: now,
                last_error_category: None,
                updated_at: now,
            })?
        };
        if enqueued {
            runtime.wake.notify_one();
        }
        Ok(enqueued)
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
    use super::*;
    use crate::{
        notification_store::PushSubscriptionRecord,
        notification_store::{AlertKindSettings, DeviceNotificationSettings},
        protocol::{AlertKind, ServerEnvelope},
        public_access::{PublicAccessContext, PublicAccessMode, PublicAccessState},
        storage::{Device, Storage},
    };
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use std::time::Duration;

    #[tokio::test]
    async fn dispatcher_targets_only_devices_with_master_and_kind_enabled() {
        let dir = tempfile::tempdir().expect("tempdir creates").keep();
        let path = dir.join("bridge.sqlite");
        let storage = Storage::open(&path).expect("storage opens");
        for device_id in ["phone-a", "phone-b", "phone-c"] {
            storage
                .insert_device(&Device {
                    device_id: device_id.into(),
                    display_name: device_id.into(),
                    secret_hash: "hash".into(),
                    paired_origin: None,
                    created_at: 1,
                    last_seen_at: 1,
                    revoked_at: None,
                })
                .expect("device inserts");
        }
        drop(storage);
        let store = NotificationStore::open(&path).expect("notification store opens");
        store
            .save_settings(&settings("phone-a", true, true))
            .unwrap();
        store
            .save_settings(&settings("phone-b", true, false))
            .unwrap();
        store
            .save_settings(&settings("phone-c", false, true))
            .unwrap();
        let store = Arc::new(Mutex::new(store));
        let hub = EventHub::new();
        let mut phone_a = hub.subscribe_for_device("phone-a").await;
        let mut phone_b = hub.subscribe_for_device("phone-b").await;
        let dispatcher = NotificationDispatcher::new(store, hub);

        dispatcher
            .dispatch(alert())
            .await
            .expect("alert dispatches");

        assert!(matches!(
            phone_a.recv().await.expect("phone A receives"),
            ServerEnvelope::AlertEvent(_)
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(25), phone_b.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn named_mode_enqueues_push_and_test_alert_avoids_duplicate_websocket_delivery() {
        let (_dir, store, hub, access) = push_harness().await;
        let mut phone = hub.subscribe_for_device("phone-a").await;
        let dispatcher = NotificationDispatcher::new(Arc::clone(&store), hub)
            .with_push_runtime(access, Arc::new(Notify::new()));

        dispatcher
            .dispatch_test_to_device("phone-a", alert())
            .await
            .expect("test alert dispatches");

        assert!(
            tokio::time::timeout(Duration::from_millis(25), phone.recv())
                .await
                .is_err()
        );
        let delivery = store
            .lock()
            .await
            .delivery_for("alert-1", "phone-a")
            .expect("delivery loads")
            .expect("delivery exists");
        let payload: PushPayload =
            serde_json::from_str(&delivery.payload_json).expect("payload parses");
        assert!(payload.force_system_notification);
    }

    async fn push_harness() -> (
        tempfile::TempDir,
        Arc<Mutex<NotificationStore>>,
        EventHub,
        PublicAccessState,
    ) {
        let dir = tempfile::tempdir().expect("tempdir creates");
        let path = dir.path().join("bridge.sqlite");
        let storage = Storage::open(&path).expect("storage opens");
        storage
            .insert_device(&Device {
                device_id: "phone-a".into(),
                display_name: "Phone A".into(),
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
            .save_settings(&settings("phone-a", true, true))
            .expect("settings save");
        store
            .save_subscription(&PushSubscriptionRecord {
                device_id: "phone-a".into(),
                origin: "https://codex.example.com".into(),
                endpoint: "https://push.example/phone-a".into(),
                p256dh: URL_SAFE_NO_PAD.encode([2_u8; 65]),
                auth: URL_SAFE_NO_PAD.encode([3_u8; 16]),
                created_at: 1,
                last_success_at: None,
                invalidated_at: None,
            })
            .expect("subscription saves");
        let access = PublicAccessState::default();
        access
            .update(PublicAccessContext {
                mode: PublicAccessMode::Named,
                public_origin: Some("https://codex.example.com".into()),
            })
            .await
            .expect("public access updates");
        (dir, Arc::new(Mutex::new(store)), EventHub::new(), access)
    }

    fn settings(device_id: &str, enabled: bool, completed: bool) -> DeviceNotificationSettings {
        DeviceNotificationSettings {
            device_id: device_id.into(),
            enabled,
            alert_kinds: AlertKindSettings {
                completed,
                ..AlertKindSettings::default()
            },
            sound_enabled: true,
            vibration_enabled: true,
            updated_at: 1,
        }
    }

    fn alert() -> AlertEvent {
        AlertEvent {
            event_id: "alert-1".into(),
            kind: AlertKind::Completed,
            thread_id: "thread-1".into(),
            thread_title: "Task".into(),
            occurred_at: 1,
        }
    }
}
