use std::path::PathBuf;

use color_eyre::eyre::{Result, WrapErr};
use rusqlite::{Connection, OptionalExtension};

pub const KEY_LAST_CHANNEL: &str = "last_channel_id";

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open() -> Result<Self> {
        let db_path = db_path();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).wrap_err("Failed to create data directory")?;
        }

        let conn = Connection::open(&db_path)
            .wrap_err_with(|| format!("Failed to open database: {}", db_path.display()))?;

        Self::init_schema(conn)
    }

    /// In-memory store for tests — no filesystem side-effects.
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().wrap_err("Failed to open in-memory database")?;
        Self::init_schema(conn)
    }

    fn init_schema(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;

             CREATE TABLE IF NOT EXISTS read_state (
                 channel_id  TEXT PRIMARY KEY,
                 last_msg_ts TEXT NOT NULL,
                 updated_at  INTEGER NOT NULL
                     DEFAULT (strftime('%s','now'))
             );

             CREATE TABLE IF NOT EXISTS session (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );",
        )
        .wrap_err("Failed to initialize database schema")?;

        Ok(Self { conn })
    }

    /// Record the newest message ts the user has seen in a channel.
    pub fn mark_read(&self, channel_id: &str, last_msg_ts: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO read_state (channel_id, last_msg_ts)
                 VALUES (?1, ?2)
                 ON CONFLICT(channel_id) DO UPDATE
                 SET last_msg_ts = excluded.last_msg_ts,
                     updated_at  = strftime('%s','now')",
                (channel_id, last_msg_ts),
            )
            .wrap_err("Failed to mark channel as read")?;
        Ok(())
    }

    /// Mark many channels read in one transaction — one WAL sync
    /// total instead of one per channel.
    pub fn mark_read_many<'a, I>(&self, entries: I) -> Result<()>
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        let tx = self
            .conn
            .unchecked_transaction()
            .wrap_err("Failed to start read-state transaction")?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO read_state (channel_id, last_msg_ts)
                 VALUES (?1, ?2)
                 ON CONFLICT(channel_id) DO UPDATE
                 SET last_msg_ts = excluded.last_msg_ts,
                     updated_at  = strftime('%s','now')",
            )?;
            for (channel_id, last_msg_ts) in entries {
                stmt.execute((channel_id, last_msg_ts))
                    .wrap_err("Failed to mark channel as read")?;
            }
        }
        tx.commit().wrap_err("Failed to commit read state")?;
        Ok(())
    }

    /// Bulk-load all read state into a map.
    pub fn all_read_state(&self) -> Result<std::collections::HashMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT channel_id, last_msg_ts FROM read_state")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut map = std::collections::HashMap::new();
        for row in rows {
            let (channel_id, ts) = row?;
            map.insert(channel_id, ts);
        }
        Ok(map)
    }

    pub fn set_session(&self, key: &str, value: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO session (key, value)
                 VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE
                 SET value = excluded.value",
                (key, value),
            )
            .wrap_err("Failed to set session value")?;
        Ok(())
    }

    pub fn get_session(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT value FROM session WHERE key = ?1")?;
        stmt.query_row((key,), |row| row.get(0))
            .optional()
            .wrap_err("Failed to read session value")
    }
}

fn db_path() -> PathBuf {
    let base = dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("slack-tooy").join("slack-tooy.db")
}
