use std::path::Path;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub device_id: String,
    pub display_name: String,
    pub secret_hash: String,
    pub paired_origin: Option<String>,
    pub created_at: u64,
    pub last_seen_at: u64,
    pub revoked_at: Option<u64>,
}

pub struct Storage {
    conn: Connection,
}

impl Storage {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let storage = Self {
            conn: Connection::open(path)?,
        };
        storage.migrate()?;
        Ok(storage)
    }

    pub fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS devices (
                device_id TEXT PRIMARY KEY NOT NULL,
                display_name TEXT NOT NULL,
                secret_hash TEXT NOT NULL,
                paired_origin TEXT,
                created_at INTEGER NOT NULL,
                last_seen_at INTEGER NOT NULL,
                revoked_at INTEGER
            );

            CREATE TABLE IF NOT EXISTS event_cursors (
                thread_id TEXT PRIMARY KEY NOT NULL,
                cursor TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS session_snapshots (
                thread_id TEXT PRIMARY KEY NOT NULL,
                snapshot_json TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );
            "#,
        )?;
        self.ensure_column("devices", "paired_origin", "TEXT")?;

        Ok(())
    }

    fn ensure_column(&self, table: &str, column: &str, definition: &str) -> Result<()> {
        let mut statement = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        if !columns.iter().any(|value| value == column) {
            self.conn.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
                [],
            )?;
        }

        Ok(())
    }

    pub fn insert_device(&self, device: &Device) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO devices (
                device_id,
                display_name,
                secret_hash,
                paired_origin,
                created_at,
                last_seen_at,
                revoked_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(device_id) DO UPDATE SET
                display_name = excluded.display_name,
                secret_hash = excluded.secret_hash,
                paired_origin = excluded.paired_origin,
                last_seen_at = excluded.last_seen_at,
                revoked_at = excluded.revoked_at
            "#,
            params![
                device.device_id,
                device.display_name,
                device.secret_hash,
                device.paired_origin,
                device.created_at,
                device.last_seen_at,
                device.revoked_at,
            ],
        )?;

        Ok(())
    }

    pub fn revoke_device(&self, device_id: &str, revoked_at: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE devices SET revoked_at = ?1 WHERE device_id = ?2",
            params![revoked_at, device_id],
        )?;

        Ok(())
    }

    pub fn device_by_id(&self, device_id: &str) -> Result<Option<Device>> {
        let device = self
            .conn
            .query_row(
                r#"
                SELECT
                    device_id,
                    display_name,
                    secret_hash,
                    paired_origin,
                    created_at,
                    last_seen_at,
                    revoked_at
                FROM devices
                WHERE device_id = ?1
                "#,
                params![device_id],
                |row| {
                    Ok(Device {
                        device_id: row.get(0)?,
                        display_name: row.get(1)?,
                        secret_hash: row.get(2)?,
                        paired_origin: row.get(3)?,
                        created_at: row.get(4)?,
                        last_seen_at: row.get(5)?,
                        revoked_at: row.get(6)?,
                    })
                },
            )
            .optional()?;

        Ok(device)
    }

    pub fn active_devices(&self) -> Result<Vec<Device>> {
        let mut statement = self.conn.prepare(
            r#"
            SELECT
                device_id,
                display_name,
                secret_hash,
                paired_origin,
                created_at,
                last_seen_at,
                revoked_at
            FROM devices
            WHERE revoked_at IS NULL
            ORDER BY created_at ASC, device_id ASC
            "#,
        )?;

        let devices = statement
            .query_map([], |row| {
                Ok(Device {
                    device_id: row.get(0)?,
                    display_name: row.get(1)?,
                    secret_hash: row.get(2)?,
                    paired_origin: row.get(3)?,
                    created_at: row.get(4)?,
                    last_seen_at: row.get(5)?,
                    revoked_at: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok(devices)
    }

    pub fn record_event_cursor(
        &self,
        thread_id: &str,
        cursor: &str,
        updated_at: u64,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO event_cursors (thread_id, cursor, updated_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(thread_id) DO UPDATE SET
                cursor = excluded.cursor,
                updated_at = excluded.updated_at
            "#,
            params![thread_id, cursor, updated_at],
        )?;

        Ok(())
    }

    pub fn latest_event_cursor(&self, thread_id: &str) -> Result<Option<String>> {
        let cursor = self
            .conn
            .query_row(
                "SELECT cursor FROM event_cursors WHERE thread_id = ?1",
                params![thread_id],
                |row| row.get(0),
            )
            .optional()?;

        Ok(cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::{TempDir, tempdir};

    fn temp_db_path() -> (TempDir, PathBuf) {
        let dir = tempdir().expect("tempdir is created");
        let path = dir.path().join("bridge.sqlite");

        (dir, path)
    }

    #[test]
    fn migrations_create_devices_and_events_tables() {
        let (_dir, path) = temp_db_path();
        let storage = Storage::open(path).expect("storage opens");

        let tables = ["devices", "event_cursors", "session_snapshots"];
        for table in tables {
            let exists: i64 = storage
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .expect("table query succeeds");

            assert_eq!(exists, 1, "{table} table should exist");
        }
    }

    #[test]
    fn migration_adds_paired_origin_to_existing_devices_table() {
        let (_dir, path) = temp_db_path();
        let conn = Connection::open(&path).expect("old database opens");
        conn.execute_batch(
            r#"
            CREATE TABLE devices (
                device_id TEXT PRIMARY KEY NOT NULL,
                display_name TEXT NOT NULL,
                secret_hash TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                last_seen_at INTEGER NOT NULL,
                revoked_at INTEGER
            );
            INSERT INTO devices (
                device_id,
                display_name,
                secret_hash,
                created_at,
                last_seen_at,
                revoked_at
            ) VALUES (
                'phone-old',
                'Old phone',
                'old-hash',
                1,
                1,
                NULL
            );
            "#,
        )
        .expect("old schema is created");
        drop(conn);

        let storage = Storage::open(&path).expect("old database migrates");
        let new_device = Device {
            device_id: "phone-new".into(),
            display_name: "New phone".into(),
            secret_hash: "new-hash".into(),
            paired_origin: Some("https://codex.example.com".into()),
            created_at: 2,
            last_seen_at: 2,
            revoked_at: None,
        };
        storage
            .insert_device(&new_device)
            .expect("new device inserts");
        drop(storage);

        let storage = Storage::open(&path).expect("migration is idempotent");

        assert_eq!(
            storage.device_by_id("phone-new").expect("new device loads"),
            Some(new_device.clone())
        );
        assert_eq!(
            storage.active_devices().expect("active devices load"),
            vec![
                Device {
                    device_id: "phone-old".into(),
                    display_name: "Old phone".into(),
                    secret_hash: "old-hash".into(),
                    paired_origin: None,
                    created_at: 1,
                    last_seen_at: 1,
                    revoked_at: None,
                },
                new_device,
            ]
        );
    }

    #[test]
    fn device_upsert_updates_paired_origin() {
        let (_dir, path) = temp_db_path();
        let storage = Storage::open(path).expect("storage opens");
        let mut device = Device {
            device_id: "phone-1".into(),
            display_name: "Phone".into(),
            secret_hash: "hash".into(),
            paired_origin: Some("http://bridge.local:4545".into()),
            created_at: 1,
            last_seen_at: 1,
            revoked_at: None,
        };
        storage.insert_device(&device).expect("device inserts");

        device.paired_origin = Some("https://codex.example.com".into());
        device.last_seen_at = 2;
        storage.insert_device(&device).expect("device upserts");

        assert_eq!(
            storage.device_by_id("phone-1").expect("device loads"),
            Some(device)
        );
    }

    #[test]
    fn revoked_device_is_not_returned_as_active() {
        let (_dir, path) = temp_db_path();
        let storage = Storage::open(path).expect("storage opens");
        let active_device = Device {
            device_id: "device-active".to_string(),
            display_name: "Active phone".to_string(),
            secret_hash: "active-secret-hash".to_string(),
            paired_origin: None,
            created_at: 1_725_000_000_000,
            last_seen_at: 1_725_000_000_100,
            revoked_at: None,
        };
        let revoked_device = Device {
            device_id: "device-revoked".to_string(),
            display_name: "Old phone".to_string(),
            secret_hash: "revoked-secret-hash".to_string(),
            paired_origin: None,
            created_at: 1_725_000_000_001,
            last_seen_at: 1_725_000_000_101,
            revoked_at: None,
        };

        storage
            .insert_device(&active_device)
            .expect("active device inserts");
        storage
            .insert_device(&revoked_device)
            .expect("revoked device inserts");
        storage
            .revoke_device("device-revoked", 1_725_000_000_200)
            .expect("device revokes");

        let active_devices = storage.active_devices().expect("active devices load");

        assert_eq!(active_devices, vec![active_device]);
    }

    #[test]
    fn event_cursor_upserts_by_thread_id() {
        let (_dir, path) = temp_db_path();
        let storage = Storage::open(path).expect("storage opens");

        storage
            .record_event_cursor("thread-1", "cursor-1", 1_725_000_000_000)
            .expect("cursor records");
        storage
            .record_event_cursor("thread-1", "cursor-2", 1_725_000_000_100)
            .expect("cursor updates");

        assert_eq!(
            storage
                .latest_event_cursor("thread-1")
                .expect("cursor loads"),
            Some("cursor-2".to_string())
        );
        assert_eq!(
            storage
                .latest_event_cursor("missing-thread")
                .expect("missing cursor loads"),
            None
        );
    }
}
