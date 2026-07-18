use std::path::Path;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::protocol::{AlertKind, SessionStatus};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct AlertKindSettings {
    pub completed: bool,
    pub approval_required: bool,
    pub input_required: bool,
    pub error: bool,
}

impl Default for AlertKindSettings {
    fn default() -> Self {
        Self {
            completed: true,
            approval_required: true,
            input_required: true,
            error: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceNotificationSettings {
    pub device_id: String,
    pub enabled: bool,
    pub alert_kinds: AlertKindSettings,
    pub sound_enabled: bool,
    pub vibration_enabled: bool,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAlertState {
    pub thread_id: String,
    pub status: SessionStatus,
    pub updated_at: u64,
    pub state_cycle: u64,
    pub known_approval_ids: Vec<String>,
    pub fallback_approval_cycle: Option<u64>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct PushSubscriptionRecord {
    pub device_id: String,
    pub origin: String,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub created_at: u64,
    pub last_success_at: Option<u64>,
    pub invalidated_at: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryStatus {
    Pending,
    Sending,
    Sent,
    InvalidSubscription,
    Failed,
}

impl DeliveryStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sending => "sending",
            Self::Sent => "sent",
            Self::InvalidSubscription => "invalid_subscription",
            Self::Failed => "failed",
        }
    }

    fn from_str(value: &str) -> rusqlite::Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "sending" => Ok(Self::Sending),
            "sent" => Ok(Self::Sent),
            "invalid_subscription" => Ok(Self::InvalidSubscription),
            "failed" => Ok(Self::Failed),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationDelivery {
    pub event_id: String,
    pub device_id: String,
    pub payload_json: String,
    pub status: DeliveryStatus,
    pub attempt_count: u32,
    pub next_attempt_at: u64,
    pub last_error_category: Option<String>,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PushSubscriptionDiagnostic {
    pub subscription_state: String,
    pub endpoint_host: String,
    pub last_success_at: Option<u64>,
    pub last_error_category: Option<String>,
}

impl std::fmt::Debug for PushSubscriptionRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let endpoint_host = url::Url::parse(&self.endpoint)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string));
        formatter
            .debug_struct("PushSubscriptionRecord")
            .field("device_id", &self.device_id)
            .field("origin", &self.origin)
            .field("endpoint_host", &endpoint_host)
            .field("p256dh", &"[REDACTED]")
            .field("auth", &"[REDACTED]")
            .field("created_at", &self.created_at)
            .field("last_success_at", &self.last_success_at)
            .field("invalidated_at", &self.invalidated_at)
            .finish()
    }
}

impl DeviceNotificationSettings {
    pub fn defaults_for(device_id: impl Into<String>) -> Self {
        Self {
            device_id: device_id.into(),
            enabled: false,
            alert_kinds: AlertKindSettings::default(),
            sound_enabled: true,
            vibration_enabled: true,
            updated_at: 0,
        }
    }

    pub fn kind_enabled(&self, kind: AlertKind) -> bool {
        match kind {
            AlertKind::Completed => self.alert_kinds.completed,
            AlertKind::ApprovalRequired => self.alert_kinds.approval_required,
            AlertKind::InputRequired => self.alert_kinds.input_required,
            AlertKind::Error => self.alert_kinds.error,
        }
    }
}

pub struct NotificationStore {
    conn: Connection,
}

impl NotificationStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let store = Self {
            conn: Connection::open(path)?,
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        let store = Self {
            conn: Connection::open_in_memory()?,
        };
        store.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS devices (device_id TEXT PRIMARY KEY NOT NULL, revoked_at INTEGER);",
        )?;
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS device_notification_settings (
                device_id TEXT PRIMARY KEY NOT NULL,
                enabled INTEGER NOT NULL,
                completed_enabled INTEGER NOT NULL,
                approval_required_enabled INTEGER NOT NULL,
                input_required_enabled INTEGER NOT NULL,
                error_enabled INTEGER NOT NULL,
                sound_enabled INTEGER NOT NULL,
                vibration_enabled INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS session_alert_state (
                thread_id TEXT PRIMARY KEY NOT NULL,
                status TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                state_cycle INTEGER NOT NULL,
                known_approval_ids_json TEXT NOT NULL,
                fallback_approval_cycle INTEGER
            );
            CREATE TABLE IF NOT EXISTS push_subscriptions (
                device_id TEXT PRIMARY KEY NOT NULL,
                origin TEXT NOT NULL,
                endpoint TEXT NOT NULL,
                p256dh TEXT NOT NULL,
                auth TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                last_success_at INTEGER,
                invalidated_at INTEGER
            );
            CREATE TABLE IF NOT EXISTS notification_deliveries (
                event_id TEXT NOT NULL,
                device_id TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                status TEXT NOT NULL,
                attempt_count INTEGER NOT NULL,
                next_attempt_at INTEGER NOT NULL,
                last_error_category TEXT,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (event_id, device_id)
            );
            "#,
        )?;
        self.conn.execute(
            "UPDATE notification_deliveries SET status = 'pending' WHERE status = 'sending'",
            [],
        )?;
        Ok(())
    }

    pub fn settings_for_device(&self, device_id: &str) -> Result<DeviceNotificationSettings> {
        Ok(self
            .conn
            .query_row(
                r#"
                SELECT enabled, completed_enabled, approval_required_enabled,
                       input_required_enabled, error_enabled, sound_enabled,
                       vibration_enabled, updated_at
                FROM device_notification_settings
                WHERE device_id = ?1
                "#,
                params![device_id],
                |row| {
                    Ok(DeviceNotificationSettings {
                        device_id: device_id.to_string(),
                        enabled: row.get(0)?,
                        alert_kinds: AlertKindSettings {
                            completed: row.get(1)?,
                            approval_required: row.get(2)?,
                            input_required: row.get(3)?,
                            error: row.get(4)?,
                        },
                        sound_enabled: row.get(5)?,
                        vibration_enabled: row.get(6)?,
                        updated_at: row.get(7)?,
                    })
                },
            )
            .optional()?
            .unwrap_or_else(|| DeviceNotificationSettings::defaults_for(device_id)))
    }

    pub fn save_settings(&self, settings: &DeviceNotificationSettings) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO device_notification_settings (
                device_id, enabled, completed_enabled, approval_required_enabled,
                input_required_enabled, error_enabled, sound_enabled,
                vibration_enabled, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(device_id) DO UPDATE SET
                enabled = excluded.enabled,
                completed_enabled = excluded.completed_enabled,
                approval_required_enabled = excluded.approval_required_enabled,
                input_required_enabled = excluded.input_required_enabled,
                error_enabled = excluded.error_enabled,
                sound_enabled = excluded.sound_enabled,
                vibration_enabled = excluded.vibration_enabled,
                updated_at = excluded.updated_at
            "#,
            params![
                settings.device_id,
                settings.enabled,
                settings.alert_kinds.completed,
                settings.alert_kinds.approval_required,
                settings.alert_kinds.input_required,
                settings.alert_kinds.error,
                settings.sound_enabled,
                settings.vibration_enabled,
                settings.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn enabled_settings(&self) -> Result<Vec<DeviceNotificationSettings>> {
        let mut statement = self.conn.prepare(
            r#"
            SELECT settings.device_id, settings.enabled, settings.completed_enabled,
                   settings.approval_required_enabled, settings.input_required_enabled,
                   settings.error_enabled, settings.sound_enabled,
                   settings.vibration_enabled, settings.updated_at
            FROM device_notification_settings settings
            JOIN devices ON devices.device_id = settings.device_id
            WHERE devices.revoked_at IS NULL AND settings.enabled = 1
            ORDER BY settings.device_id
            "#,
        )?;
        Ok(statement
            .query_map([], settings_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn enabled_devices_for_kind(&self, kind: AlertKind) -> Result<Vec<String>> {
        Ok(self
            .enabled_settings()?
            .into_iter()
            .filter(|settings| settings.kind_enabled(kind))
            .map(|settings| settings.device_id)
            .collect())
    }

    pub fn any_device_wants_approval_alerts(&self) -> Result<bool> {
        Ok(self
            .enabled_settings()?
            .iter()
            .any(|settings| settings.alert_kinds.approval_required))
    }

    pub fn delete_device_notification_data(&self, device_id: &str) -> Result<()> {
        let transaction = self.conn.unchecked_transaction()?;
        transaction.execute(
            "DELETE FROM device_notification_settings WHERE device_id = ?1",
            params![device_id],
        )?;
        transaction.execute(
            "DELETE FROM push_subscriptions WHERE device_id = ?1",
            params![device_id],
        )?;
        transaction.execute(
            "DELETE FROM notification_deliveries WHERE device_id = ?1",
            params![device_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn save_subscription(&self, subscription: &PushSubscriptionRecord) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO push_subscriptions (
                device_id, origin, endpoint, p256dh, auth, created_at,
                last_success_at, invalidated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(device_id) DO UPDATE SET
                origin = excluded.origin,
                endpoint = excluded.endpoint,
                p256dh = excluded.p256dh,
                auth = excluded.auth,
                created_at = excluded.created_at,
                last_success_at = excluded.last_success_at,
                invalidated_at = excluded.invalidated_at
            "#,
            params![
                subscription.device_id,
                subscription.origin,
                subscription.endpoint,
                subscription.p256dh,
                subscription.auth,
                subscription.created_at,
                subscription.last_success_at,
                subscription.invalidated_at,
            ],
        )?;
        Ok(())
    }

    pub fn subscription_for_device(
        &self,
        device_id: &str,
    ) -> Result<Option<PushSubscriptionRecord>> {
        Ok(self
            .conn
            .query_row(
                r#"
                SELECT origin, endpoint, p256dh, auth, created_at,
                       last_success_at, invalidated_at
                FROM push_subscriptions
                WHERE device_id = ?1
                "#,
                params![device_id],
                |row| subscription_from_row(device_id, row),
            )
            .optional()?)
    }

    pub fn active_subscription(&self, device_id: &str) -> Result<Option<PushSubscriptionRecord>> {
        Ok(self
            .conn
            .query_row(
                r#"
                SELECT origin, endpoint, p256dh, auth, created_at,
                       last_success_at, invalidated_at
                FROM push_subscriptions
                WHERE device_id = ?1 AND invalidated_at IS NULL
                "#,
                params![device_id],
                |row| subscription_from_row(device_id, row),
            )
            .optional()?)
    }

    pub fn push_subscription_diagnostics(&self) -> Result<Vec<PushSubscriptionDiagnostic>> {
        let mut statement = self.conn.prepare(
            r#"
            SELECT endpoint, last_success_at, invalidated_at,
                   (
                       SELECT last_error_category
                       FROM notification_deliveries
                       WHERE notification_deliveries.device_id = push_subscriptions.device_id
                         AND last_error_category IS NOT NULL
                       ORDER BY updated_at DESC
                       LIMIT 1
                   )
            FROM push_subscriptions
            ORDER BY created_at DESC
            "#,
        )?;
        let rows = statement.query_map([], |row| {
            let endpoint: String = row.get(0)?;
            let invalidated_at: Option<u64> = row.get(2)?;
            Ok(PushSubscriptionDiagnostic {
                subscription_state: if invalidated_at.is_some() {
                    "needs_repair".to_string()
                } else {
                    "active".to_string()
                },
                endpoint_host: url::Url::parse(&endpoint)
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_string))
                    .unwrap_or_else(|| "invalid-endpoint".to_string()),
                last_success_at: row.get(1)?,
                last_error_category: row.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn invalidate_subscription(&self, device_id: &str, invalidated_at: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE push_subscriptions SET invalidated_at = ?2 WHERE device_id = ?1",
            params![device_id, invalidated_at],
        )?;
        Ok(())
    }

    pub fn delete_subscription(&self, device_id: &str) -> Result<()> {
        let transaction = self.conn.unchecked_transaction()?;
        transaction.execute(
            "DELETE FROM push_subscriptions WHERE device_id = ?1",
            params![device_id],
        )?;
        transaction.execute(
            "DELETE FROM notification_deliveries WHERE device_id = ?1",
            params![device_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn enqueue_delivery(&self, delivery: &NotificationDelivery) -> Result<bool> {
        Ok(self.conn.execute(
            r#"
            INSERT OR IGNORE INTO notification_deliveries (
                event_id, device_id, payload_json, status, attempt_count,
                next_attempt_at, last_error_category, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![
                delivery.event_id,
                delivery.device_id,
                delivery.payload_json,
                delivery.status.as_str(),
                delivery.attempt_count,
                delivery.next_attempt_at,
                delivery.last_error_category,
                delivery.updated_at,
            ],
        )? > 0)
    }

    pub fn claim_next_due_delivery(&self, now: u64) -> Result<Option<NotificationDelivery>> {
        let transaction = self.conn.unchecked_transaction()?;
        let claimed = {
            let mut statement = transaction.prepare(
                r#"
                SELECT event_id, device_id, payload_json, status, attempt_count,
                       next_attempt_at, last_error_category, updated_at
                FROM notification_deliveries
                WHERE status = 'pending' AND next_attempt_at <= ?1
                ORDER BY next_attempt_at, updated_at, event_id, device_id
                LIMIT 1
                "#,
            )?;
            statement
                .query_row(params![now], delivery_from_row)
                .optional()?
        };
        let claimed = match claimed {
            Some(mut delivery) => {
                delivery.status = DeliveryStatus::Sending;
                delivery.attempt_count = delivery.attempt_count.saturating_add(1);
                delivery.updated_at = now;
                transaction.execute(
                    r#"
                    UPDATE notification_deliveries
                    SET status = 'sending', attempt_count = ?3, updated_at = ?4
                    WHERE event_id = ?1 AND device_id = ?2
                    "#,
                    params![
                        delivery.event_id,
                        delivery.device_id,
                        delivery.attempt_count,
                        now,
                    ],
                )?;
                Some(delivery)
            }
            None => None,
        };
        transaction.commit()?;
        Ok(claimed)
    }

    pub fn next_delivery_due_at(&self) -> Result<Option<u64>> {
        Ok(self.conn.query_row(
            "SELECT MIN(next_attempt_at) FROM notification_deliveries WHERE status = 'pending'",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn mark_delivery_sent(&self, event_id: &str, device_id: &str, now: u64) -> Result<()> {
        let transaction = self.conn.unchecked_transaction()?;
        transaction.execute(
            r#"
            UPDATE notification_deliveries
            SET status = 'sent', last_error_category = NULL, updated_at = ?3
            WHERE event_id = ?1 AND device_id = ?2
            "#,
            params![event_id, device_id, now],
        )?;
        transaction.execute(
            r#"
            UPDATE push_subscriptions
            SET last_success_at = ?2
            WHERE device_id = ?1 AND invalidated_at IS NULL
            "#,
            params![device_id, now],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_delivery_retry(
        &self,
        event_id: &str,
        device_id: &str,
        next_attempt_at: u64,
        category: &str,
        now: u64,
    ) -> Result<()> {
        self.update_delivery_status(
            event_id,
            device_id,
            DeliveryStatus::Pending,
            Some(next_attempt_at),
            Some(category),
            now,
        )
    }

    pub fn mark_delivery_invalid_subscription(
        &self,
        event_id: &str,
        device_id: &str,
        now: u64,
    ) -> Result<()> {
        self.update_delivery_status(
            event_id,
            device_id,
            DeliveryStatus::InvalidSubscription,
            None,
            Some("push_invalid_subscription"),
            now,
        )
    }

    pub fn mark_delivery_failed(
        &self,
        event_id: &str,
        device_id: &str,
        category: &str,
        now: u64,
    ) -> Result<()> {
        self.update_delivery_status(
            event_id,
            device_id,
            DeliveryStatus::Failed,
            None,
            Some(category),
            now,
        )
    }

    pub fn fail_pending_deliveries(&self, category: &str, now: u64) -> Result<usize> {
        Ok(self.conn.execute(
            r#"
            UPDATE notification_deliveries
            SET status = 'failed', last_error_category = ?1, updated_at = ?2
            WHERE status IN ('pending', 'sending')
            "#,
            params![category, now],
        )?)
    }

    pub fn delivery_for(
        &self,
        event_id: &str,
        device_id: &str,
    ) -> Result<Option<NotificationDelivery>> {
        Ok(self
            .conn
            .query_row(
                r#"
                SELECT event_id, device_id, payload_json, status, attempt_count,
                       next_attempt_at, last_error_category, updated_at
                FROM notification_deliveries
                WHERE event_id = ?1 AND device_id = ?2
                "#,
                params![event_id, device_id],
                delivery_from_row,
            )
            .optional()?)
    }

    pub fn delivery_count(&self, device_id: &str) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM notification_deliveries WHERE device_id = ?1",
            params![device_id],
            |row| row.get(0),
        )?)
    }

    fn update_delivery_status(
        &self,
        event_id: &str,
        device_id: &str,
        status: DeliveryStatus,
        next_attempt_at: Option<u64>,
        category: Option<&str>,
        now: u64,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            UPDATE notification_deliveries
            SET status = ?3,
                next_attempt_at = COALESCE(?4, next_attempt_at),
                last_error_category = ?5,
                updated_at = ?6
            WHERE event_id = ?1 AND device_id = ?2
            "#,
            params![
                event_id,
                device_id,
                status.as_str(),
                next_attempt_at,
                category,
                now,
            ],
        )?;
        Ok(())
    }

    pub fn alert_state_for_thread(&self, thread_id: &str) -> Result<Option<SessionAlertState>> {
        Ok(self
            .conn
            .query_row(
                r#"
                SELECT status, updated_at, state_cycle, known_approval_ids_json,
                       fallback_approval_cycle
                FROM session_alert_state
                WHERE thread_id = ?1
                "#,
                params![thread_id],
                |row| {
                    let status_json: String = row.get(0)?;
                    let approvals_json: String = row.get(3)?;
                    let status = serde_json::from_str(&status_json).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                    let known_approval_ids =
                        serde_json::from_str(&approvals_json).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                3,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?;
                    Ok(SessionAlertState {
                        thread_id: thread_id.to_string(),
                        status,
                        updated_at: row.get(1)?,
                        state_cycle: row.get(2)?,
                        known_approval_ids,
                        fallback_approval_cycle: row.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn save_alert_state(&self, state: &SessionAlertState) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO session_alert_state (
                thread_id, status, updated_at, state_cycle,
                known_approval_ids_json, fallback_approval_cycle
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(thread_id) DO UPDATE SET
                status = excluded.status,
                updated_at = excluded.updated_at,
                state_cycle = excluded.state_cycle,
                known_approval_ids_json = excluded.known_approval_ids_json,
                fallback_approval_cycle = excluded.fallback_approval_cycle
            "#,
            params![
                state.thread_id,
                serde_json::to_string(&state.status)?,
                state.updated_at,
                state.state_cycle,
                serde_json::to_string(&state.known_approval_ids)?,
                state.fallback_approval_cycle,
            ],
        )?;
        Ok(())
    }
}

fn settings_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeviceNotificationSettings> {
    Ok(DeviceNotificationSettings {
        device_id: row.get(0)?,
        enabled: row.get(1)?,
        alert_kinds: AlertKindSettings {
            completed: row.get(2)?,
            approval_required: row.get(3)?,
            input_required: row.get(4)?,
            error: row.get(5)?,
        },
        sound_enabled: row.get(6)?,
        vibration_enabled: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn subscription_from_row(
    device_id: &str,
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<PushSubscriptionRecord> {
    Ok(PushSubscriptionRecord {
        device_id: device_id.to_string(),
        origin: row.get(0)?,
        endpoint: row.get(1)?,
        p256dh: row.get(2)?,
        auth: row.get(3)?,
        created_at: row.get(4)?,
        last_success_at: row.get(5)?,
        invalidated_at: row.get(6)?,
    })
}

fn delivery_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NotificationDelivery> {
    let status: String = row.get(3)?;
    Ok(NotificationDelivery {
        event_id: row.get(0)?,
        device_id: row.get(1)?,
        payload_json: row.get(2)?,
        status: DeliveryStatus::from_str(&status)?,
        attempt_count: row.get(4)?,
        next_attempt_at: row.get(5)?,
        last_error_category: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_disabled_with_all_kinds_preselected() {
        let store = NotificationStore::open_in_memory().expect("store opens");

        let settings = store.settings_for_device("phone-1").expect("settings load");

        assert!(!settings.enabled);
        assert!(settings.alert_kinds.completed);
        assert!(settings.alert_kinds.approval_required);
        assert!(settings.alert_kinds.input_required);
        assert!(settings.alert_kinds.error);
        assert!(settings.sound_enabled);
        assert!(settings.vibration_enabled);
    }

    #[test]
    fn settings_replace_the_complete_boolean_document() {
        let store = NotificationStore::open_in_memory().expect("store opens");
        let settings = DeviceNotificationSettings {
            device_id: "phone-1".into(),
            enabled: true,
            alert_kinds: AlertKindSettings {
                completed: true,
                approval_required: false,
                input_required: true,
                error: false,
            },
            sound_enabled: false,
            vibration_enabled: true,
            updated_at: 10,
        };

        store.save_settings(&settings).expect("settings save");

        assert_eq!(
            store.settings_for_device("phone-1").expect("settings load"),
            settings
        );
    }

    #[test]
    fn persisted_alert_state_prevents_duplicate_after_store_reopen() {
        let dir = tempfile::tempdir().expect("tempdir creates");
        let path = dir.path().join("bridge.sqlite");
        let store = NotificationStore::open(&path).expect("store opens");
        let state = SessionAlertState {
            thread_id: "thread-1".into(),
            status: SessionStatus::WaitingForInput,
            updated_at: 20,
            state_cycle: 1,
            known_approval_ids: Vec::new(),
            fallback_approval_cycle: None,
        };
        store.save_alert_state(&state).expect("state saves");
        drop(store);

        let reopened = NotificationStore::open(&path).expect("store reopens");

        assert_eq!(
            reopened
                .alert_state_for_thread("thread-1")
                .expect("state loads"),
            Some(state)
        );
    }

    #[test]
    fn subscription_is_replaced_per_device_and_can_be_invalidated() {
        let store = NotificationStore::open_in_memory().expect("store opens");
        store
            .save_subscription(&subscription("phone-1", "https://push.example/one"))
            .expect("first subscription saves");
        store
            .save_subscription(&subscription("phone-1", "https://push.example/two"))
            .expect("replacement saves");

        let current = store
            .active_subscription("phone-1")
            .expect("subscription loads")
            .expect("subscription exists");
        assert_eq!(current.endpoint, "https://push.example/two");
        assert!(!format!("{current:?}").contains("client-public-key"));
        assert!(!format!("{current:?}").contains("client-auth-secret"));

        store
            .invalidate_subscription("phone-1", 20)
            .expect("subscription invalidates");
        assert_eq!(
            store
                .active_subscription("phone-1")
                .expect("active subscription queries"),
            None
        );
        assert_eq!(
            store
                .subscription_for_device("phone-1")
                .expect("subscription row queries")
                .expect("invalidated row remains")
                .invalidated_at,
            Some(20)
        );
    }

    #[test]
    fn deleting_device_notification_data_removes_all_notification_rows() {
        let store = NotificationStore::open_in_memory().expect("store opens");
        store
            .save_settings(&DeviceNotificationSettings {
                device_id: "phone-1".into(),
                enabled: true,
                alert_kinds: AlertKindSettings::default(),
                sound_enabled: true,
                vibration_enabled: true,
                updated_at: 1,
            })
            .expect("settings save");
        store
            .save_subscription(&subscription("phone-1", "https://push.example/one"))
            .expect("subscription saves");
        store
            .conn
            .execute(
                r#"
                INSERT INTO notification_deliveries (
                    event_id, device_id, payload_json, status, attempt_count,
                    next_attempt_at, last_error_category, updated_at
                ) VALUES ('event-1', 'phone-1', '{}', 'pending', 0, 1, NULL, 1)
                "#,
                [],
            )
            .expect("delivery seeds");

        store
            .delete_device_notification_data("phone-1")
            .expect("notification data deletes");

        assert_eq!(
            store
                .settings_for_device("phone-1")
                .expect("settings query"),
            DeviceNotificationSettings::defaults_for("phone-1")
        );
        assert!(
            store
                .subscription_for_device("phone-1")
                .expect("subscription query")
                .is_none()
        );
        let delivery_count: u64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM notification_deliveries WHERE device_id = 'phone-1'",
                [],
                |row| row.get(0),
            )
            .expect("delivery count loads");
        assert_eq!(delivery_count, 0);
    }

    #[test]
    fn enqueue_is_idempotent_per_event_and_device() {
        let store = NotificationStore::open_in_memory().expect("store opens");
        let delivery = delivery("event-1", "phone-1");

        assert!(
            store
                .enqueue_delivery(&delivery)
                .expect("first enqueue succeeds")
        );
        assert!(
            !store
                .enqueue_delivery(&delivery)
                .expect("duplicate enqueue succeeds")
        );
        assert_eq!(store.delivery_count("phone-1").expect("count loads"), 1);
    }

    #[test]
    fn reopening_store_recovers_sending_rows_to_pending() {
        let dir = tempfile::tempdir().expect("tempdir creates");
        let path = dir.path().join("bridge.sqlite");
        let store = NotificationStore::open(&path).expect("store opens");
        store
            .enqueue_delivery(&delivery("event-1", "phone-1"))
            .expect("delivery enqueues");
        let claimed = store
            .claim_next_due_delivery(10)
            .expect("delivery claims")
            .expect("delivery is due");
        assert_eq!(claimed.status, DeliveryStatus::Sending);
        assert_eq!(claimed.attempt_count, 1);
        drop(store);

        let reopened = NotificationStore::open(&path).expect("store reopens");
        let recovered = reopened
            .claim_next_due_delivery(20)
            .expect("recovered delivery claims")
            .expect("recovered delivery is due");
        assert_eq!(recovered.status, DeliveryStatus::Sending);
        assert_eq!(recovered.attempt_count, 2);
    }

    #[test]
    fn marking_delivery_sent_updates_subscription_last_success() {
        let store = NotificationStore::open_in_memory().expect("store opens");
        store
            .save_subscription(&subscription("phone-1", "https://push.example/one"))
            .expect("subscription saves");
        store
            .enqueue_delivery(&delivery("event-1", "phone-1"))
            .expect("delivery enqueues");
        store
            .claim_next_due_delivery(10)
            .expect("delivery claims")
            .expect("delivery is due");

        store
            .mark_delivery_sent("event-1", "phone-1", 20)
            .expect("delivery marks sent");

        assert_eq!(
            store
                .delivery_for("event-1", "phone-1")
                .expect("delivery loads")
                .expect("delivery exists")
                .status,
            DeliveryStatus::Sent
        );
        assert_eq!(
            store
                .active_subscription("phone-1")
                .expect("subscription loads")
                .expect("subscription exists")
                .last_success_at,
            Some(20)
        );
    }

    #[test]
    fn push_diagnostics_expose_host_and_safe_delivery_metadata_only() {
        let store = NotificationStore::open_in_memory().expect("store opens");
        store
            .save_subscription(&PushSubscriptionRecord {
                device_id: "phone-1".to_string(),
                origin: "https://codex.example.com".to_string(),
                endpoint: "https://fcm.googleapis.com/fcm/send/private-path?token=secret"
                    .to_string(),
                p256dh: "client-public-key".to_string(),
                auth: "client-auth-secret".to_string(),
                created_at: 1,
                last_success_at: Some(20),
                invalidated_at: None,
            })
            .expect("subscription saves");
        store
            .enqueue_delivery(&NotificationDelivery {
                event_id: "event-1".to_string(),
                device_id: "phone-1".to_string(),
                payload_json: "{\"private\":\"payload\"}".to_string(),
                status: DeliveryStatus::Failed,
                attempt_count: 4,
                next_attempt_at: 0,
                last_error_category: Some("network".to_string()),
                updated_at: 30,
            })
            .expect("delivery saves");

        let diagnostics = store
            .push_subscription_diagnostics()
            .expect("diagnostics load");
        let value = serde_json::to_value(&diagnostics[0]).expect("diagnostic serializes");

        assert_eq!(
            value,
            serde_json::json!({
                "subscriptionState": "active",
                "endpointHost": "fcm.googleapis.com",
                "lastSuccessAt": 20,
                "lastErrorCategory": "network"
            })
        );
        let serialized = value.to_string();
        for secret in [
            "private-path",
            "secret",
            "client-public-key",
            "client-auth-secret",
            "payload",
        ] {
            assert!(!serialized.contains(secret));
        }
    }

    fn subscription(device_id: &str, endpoint: &str) -> PushSubscriptionRecord {
        PushSubscriptionRecord {
            device_id: device_id.into(),
            origin: "https://codex.example.com".into(),
            endpoint: endpoint.into(),
            p256dh: "client-public-key".into(),
            auth: "client-auth-secret".into(),
            created_at: 10,
            last_success_at: None,
            invalidated_at: None,
        }
    }

    fn delivery(event_id: &str, device_id: &str) -> NotificationDelivery {
        NotificationDelivery {
            event_id: event_id.into(),
            device_id: device_id.into(),
            payload_json: "{}".into(),
            status: DeliveryStatus::Pending,
            attempt_count: 0,
            next_attempt_at: 1,
            last_error_category: None,
            updated_at: 1,
        }
    }
}
