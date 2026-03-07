//! SQLite-backed persistence for session metrics.
//!
//! Provides a [`StateStore`] trait and a [`SqliteStore`] implementation that
//! persists session data, event logs, agent totals, and completed issue IDs
//! across orchestrator restarts.

use crate::model::{AgentTotals, LogEntry, TokenSnapshot};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;
use thiserror::Error;
use tracing::info;

// ── Error type ──────────────────────────────────────────────────────────

/// Persistence-specific errors.
#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("lock poisoned")]
    LockPoisoned,
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, PersistenceError>;

// ── PersistedSession ────────────────────────────────────────────────────

/// Serializable session record for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedSession {
    pub issue_id: String,
    pub identifier: String,
    pub session_id: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub turn_count: u32,
    /// ISO 8601 timestamp.
    pub started_at: String,
    pub ended_at: Option<String>,
    pub last_event: Option<String>,
    pub last_event_at: Option<String>,
    pub last_message: Option<String>,
    pub retry_attempt: Option<u32>,
}

// ── StateStore trait ────────────────────────────────────────────────────

/// Abstraction over session metric persistence.
pub trait StateStore: Send + Sync {
    fn save_session(&self, session: &PersistedSession) -> Result<()>;
    fn load_sessions(&self) -> Result<Vec<PersistedSession>>;
    fn delete_session(&self, issue_id: &str) -> Result<()>;
    fn save_log_entry(&self, issue_id: &str, entry: &LogEntry) -> Result<()>;
    fn load_log_entries(&self, issue_id: &str) -> Result<Vec<LogEntry>>;
    fn delete_log_entries(&self, issue_id: &str) -> Result<()>;
    fn save_agent_totals(&self, totals: &AgentTotals) -> Result<()>;
    fn load_agent_totals(&self) -> Result<AgentTotals>;
    fn mark_completed(&self, issue_id: &str) -> Result<()>;
    fn load_completed_ids(&self) -> Result<Vec<String>>;
}

// ── SqliteStore ─────────────────────────────────────────────────────────

/// SQLite-backed [`StateStore`].
pub struct SqliteStore {
    conn: Mutex<Connection>,
}

impl SqliteStore {
    /// Open (or create) a database at `db_path`.
    pub fn new(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        Self::apply_pragmas_and_schema(&conn)?;
        info!(path = %db_path.display(), "opened persistence store");
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Create an in-memory store (useful for tests).
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::apply_pragmas_and_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn apply_pragmas_and_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;",
        )?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                issue_id TEXT PRIMARY KEY,
                identifier TEXT NOT NULL,
                session_id TEXT,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                turn_count INTEGER NOT NULL DEFAULT 0,
                started_at TEXT NOT NULL,
                ended_at TEXT,
                last_event TEXT,
                last_event_at TEXT,
                last_message TEXT,
                retry_attempt INTEGER
            );

            CREATE TABLE IF NOT EXISTS event_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                issue_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                timestamp TEXT NOT NULL,
                event_type TEXT NOT NULL,
                message TEXT,
                input_tokens INTEGER,
                output_tokens INTEGER,
                total_tokens INTEGER
            );

            CREATE TABLE IF NOT EXISTS agent_totals (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                seconds_running REAL NOT NULL DEFAULT 0.0
            );

            CREATE TABLE IF NOT EXISTS completed_issues (
                issue_id TEXT PRIMARY KEY,
                completed_at TEXT NOT NULL
            );",
        )?;

        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| PersistenceError::LockPoisoned)
    }
}

impl StateStore for SqliteStore {
    fn save_session(&self, session: &PersistedSession) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR REPLACE INTO sessions
             (issue_id, identifier, session_id, input_tokens, output_tokens, total_tokens,
              turn_count, started_at, ended_at, last_event, last_event_at, last_message, retry_attempt)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                session.issue_id,
                session.identifier,
                session.session_id,
                session.input_tokens as i64,
                session.output_tokens as i64,
                session.total_tokens as i64,
                session.turn_count as i64,
                session.started_at,
                session.ended_at,
                session.last_event,
                session.last_event_at,
                session.last_message,
                session.retry_attempt.map(|v| v as i64),
            ],
        )?;
        Ok(())
    }

    fn load_sessions(&self) -> Result<Vec<PersistedSession>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT issue_id, identifier, session_id, input_tokens, output_tokens, total_tokens,
                    turn_count, started_at, ended_at, last_event, last_event_at, last_message, retry_attempt
             FROM sessions ORDER BY started_at",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(PersistedSession {
                issue_id: row.get(0)?,
                identifier: row.get(1)?,
                session_id: row.get(2)?,
                input_tokens: row.get::<_, i64>(3)? as u64,
                output_tokens: row.get::<_, i64>(4)? as u64,
                total_tokens: row.get::<_, i64>(5)? as u64,
                turn_count: row.get::<_, i64>(6)? as u32,
                started_at: row.get(7)?,
                ended_at: row.get(8)?,
                last_event: row.get(9)?,
                last_event_at: row.get(10)?,
                last_message: row.get(11)?,
                retry_attempt: row.get::<_, Option<i64>>(12)?.map(|v| v as u32),
            })
        })?;

        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        Ok(sessions)
    }

    fn delete_session(&self, issue_id: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "DELETE FROM sessions WHERE issue_id = ?1",
            params![issue_id],
        )?;
        Ok(())
    }

    fn save_log_entry(&self, issue_id: &str, entry: &LogEntry) -> Result<()> {
        let conn = self.lock()?;
        let (input, output, total) = match &entry.tokens {
            Some(t) => (
                Some(t.input_tokens as i64),
                Some(t.output_tokens as i64),
                Some(t.total_tokens as i64),
            ),
            None => (None, None, None),
        };

        conn.execute(
            "INSERT INTO event_logs (issue_id, seq, timestamp, event_type, message,
                                     input_tokens, output_tokens, total_tokens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                issue_id,
                entry.seq as i64,
                entry.timestamp.to_rfc3339(),
                entry.event_type,
                entry.message,
                input,
                output,
                total,
            ],
        )?;
        Ok(())
    }

    fn load_log_entries(&self, issue_id: &str) -> Result<Vec<LogEntry>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT seq, timestamp, event_type, message, input_tokens, output_tokens, total_tokens
             FROM event_logs WHERE issue_id = ?1 ORDER BY id",
        )?;

        let rows = stmt.query_map(params![issue_id], |row| {
            let ts_str: String = row.get(1)?;
            let timestamp = chrono::DateTime::parse_from_rfc3339(&ts_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now());

            let input: Option<i64> = row.get(4)?;
            let output: Option<i64> = row.get(5)?;
            let total: Option<i64> = row.get(6)?;

            let tokens = match (input, output, total) {
                (Some(i), Some(o), Some(t)) => Some(TokenSnapshot {
                    input_tokens: i as u64,
                    output_tokens: o as u64,
                    total_tokens: t as u64,
                }),
                _ => None,
            };

            Ok(LogEntry {
                seq: row.get::<_, i64>(0)? as u64,
                timestamp,
                event_type: row.get(2)?,
                message: row.get(3)?,
                tokens,
            })
        })?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        Ok(entries)
    }

    fn delete_log_entries(&self, issue_id: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "DELETE FROM event_logs WHERE issue_id = ?1",
            params![issue_id],
        )?;
        Ok(())
    }

    fn save_agent_totals(&self, totals: &AgentTotals) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR REPLACE INTO agent_totals (id, input_tokens, output_tokens, total_tokens, seconds_running)
             VALUES (1, ?1, ?2, ?3, ?4)",
            params![
                totals.input_tokens as i64,
                totals.output_tokens as i64,
                totals.total_tokens as i64,
                totals.seconds_running,
            ],
        )?;
        Ok(())
    }

    fn load_agent_totals(&self) -> Result<AgentTotals> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT input_tokens, output_tokens, total_tokens, seconds_running
             FROM agent_totals WHERE id = 1",
        )?;

        let mut rows = stmt.query_map([], |row| {
            Ok(AgentTotals {
                input_tokens: row.get::<_, i64>(0)? as u64,
                output_tokens: row.get::<_, i64>(1)? as u64,
                total_tokens: row.get::<_, i64>(2)? as u64,
                seconds_running: row.get(3)?,
            })
        })?;

        match rows.next() {
            Some(row) => Ok(row?),
            None => Ok(AgentTotals::default()),
        }
    }

    fn mark_completed(&self, issue_id: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR IGNORE INTO completed_issues (issue_id, completed_at)
             VALUES (?1, ?2)",
            params![issue_id, chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    fn load_completed_ids(&self) -> Result<Vec<String>> {
        let conn = self.lock()?;
        let mut stmt =
            conn.prepare("SELECT issue_id FROM completed_issues ORDER BY completed_at")?;

        let rows = stmt.query_map([], |row| row.get(0))?;

        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }
}

// Ensure SqliteStore is Send + Sync (Mutex<Connection> provides this).
// The static assertions catch at compile time if we break the contract.
const _: () = {
    fn assert_send_sync<T: Send + Sync>() {}
    fn assert_all() {
        assert_send_sync::<SqliteStore>();
    }
    let _ = assert_all;
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentTotals, LogEntry, TokenSnapshot};
    use chrono::Utc;
    use tempfile::NamedTempFile;

    // ── SqliteStore construction ────────────────────────────────────────

    #[test]
    fn new_creates_file_db() {
        let tmp = NamedTempFile::new().unwrap();
        let store = SqliteStore::new(tmp.path()).unwrap();
        // Verify tables exist by running a query
        let sessions = store.load_sessions().unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn in_memory_creates_store() {
        let store = SqliteStore::in_memory().unwrap();
        let sessions = store.load_sessions().unwrap();
        assert!(sessions.is_empty());
    }

    // ── Session CRUD ────────────────────────────────────────────────────

    #[test]
    fn save_and_load_session() {
        let store = SqliteStore::in_memory().unwrap();
        let session = make_session("issue-1", "TST-1");
        store.save_session(&session).unwrap();

        let loaded = store.load_sessions().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].issue_id, "issue-1");
        assert_eq!(loaded[0].identifier, "TST-1");
        assert_eq!(loaded[0].input_tokens, 100);
        assert_eq!(loaded[0].output_tokens, 50);
        assert_eq!(loaded[0].total_tokens, 150);
        assert_eq!(loaded[0].turn_count, 3);
    }

    #[test]
    fn save_session_upserts_on_duplicate() {
        let store = SqliteStore::in_memory().unwrap();
        let mut session = make_session("issue-1", "TST-1");
        store.save_session(&session).unwrap();

        session.turn_count = 10;
        session.input_tokens = 500;
        store.save_session(&session).unwrap();

        let loaded = store.load_sessions().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].turn_count, 10);
        assert_eq!(loaded[0].input_tokens, 500);
    }

    #[test]
    fn delete_session_removes_it() {
        let store = SqliteStore::in_memory().unwrap();
        store
            .save_session(&make_session("issue-1", "TST-1"))
            .unwrap();
        store
            .save_session(&make_session("issue-2", "TST-2"))
            .unwrap();

        store.delete_session("issue-1").unwrap();

        let loaded = store.load_sessions().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].issue_id, "issue-2");
    }

    #[test]
    fn delete_session_nonexistent_is_ok() {
        let store = SqliteStore::in_memory().unwrap();
        // Should not error
        store.delete_session("nonexistent").unwrap();
    }

    #[test]
    fn load_sessions_empty() {
        let store = SqliteStore::in_memory().unwrap();
        let loaded = store.load_sessions().unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn save_session_with_all_none_optionals() {
        let store = SqliteStore::in_memory().unwrap();
        let session = PersistedSession {
            issue_id: "id-none".to_string(),
            identifier: "TST-N".to_string(),
            session_id: None,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            turn_count: 0,
            started_at: "2025-01-01T00:00:00Z".to_string(),
            ended_at: None,
            last_event: None,
            last_event_at: None,
            last_message: None,
            retry_attempt: None,
        };
        store.save_session(&session).unwrap();

        let loaded = store.load_sessions().unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].session_id.is_none());
        assert!(loaded[0].ended_at.is_none());
        assert!(loaded[0].last_event.is_none());
        assert!(loaded[0].last_event_at.is_none());
        assert!(loaded[0].last_message.is_none());
        assert!(loaded[0].retry_attempt.is_none());
    }

    #[test]
    fn save_session_with_empty_strings() {
        let store = SqliteStore::in_memory().unwrap();
        let session = PersistedSession {
            issue_id: "".to_string(),
            identifier: "".to_string(),
            session_id: Some("".to_string()),
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            turn_count: 0,
            started_at: "".to_string(),
            ended_at: Some("".to_string()),
            last_event: Some("".to_string()),
            last_event_at: Some("".to_string()),
            last_message: Some("".to_string()),
            retry_attempt: Some(0),
        };
        store.save_session(&session).unwrap();

        let loaded = store.load_sessions().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].issue_id, "");
        assert_eq!(loaded[0].session_id, Some("".to_string()));
    }

    #[test]
    fn save_multiple_sessions_ordered_by_started_at() {
        let store = SqliteStore::in_memory().unwrap();
        let mut s1 = make_session("id-a", "TST-A");
        s1.started_at = "2025-01-03T00:00:00Z".to_string();
        let mut s2 = make_session("id-b", "TST-B");
        s2.started_at = "2025-01-01T00:00:00Z".to_string();
        let mut s3 = make_session("id-c", "TST-C");
        s3.started_at = "2025-01-02T00:00:00Z".to_string();

        store.save_session(&s1).unwrap();
        store.save_session(&s2).unwrap();
        store.save_session(&s3).unwrap();

        let loaded = store.load_sessions().unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].issue_id, "id-b"); // earliest
        assert_eq!(loaded[1].issue_id, "id-c");
        assert_eq!(loaded[2].issue_id, "id-a"); // latest
    }

    // ── Event log CRUD ──────────────────────────────────────────────────

    #[test]
    fn save_and_load_log_entries() {
        let store = SqliteStore::in_memory().unwrap();
        let e1 = make_log_entry(1, "session_started", Some("started"), None);
        let e2 = make_log_entry(2, "token_usage", None, Some((100, 50, 150)));

        store.save_log_entry("issue-1", &e1).unwrap();
        store.save_log_entry("issue-1", &e2).unwrap();

        let loaded = store.load_log_entries("issue-1").unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].seq, 1);
        assert_eq!(loaded[0].event_type, "session_started");
        assert_eq!(loaded[0].message, Some("started".to_string()));
        assert!(loaded[0].tokens.is_none());

        assert_eq!(loaded[1].seq, 2);
        assert_eq!(loaded[1].event_type, "token_usage");
        assert!(loaded[1].message.is_none());
        let tokens = loaded[1].tokens.as_ref().unwrap();
        assert_eq!(tokens.input_tokens, 100);
        assert_eq!(tokens.output_tokens, 50);
        assert_eq!(tokens.total_tokens, 150);
    }

    #[test]
    fn load_log_entries_empty() {
        let store = SqliteStore::in_memory().unwrap();
        let loaded = store.load_log_entries("nonexistent").unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn log_entries_are_per_issue() {
        let store = SqliteStore::in_memory().unwrap();
        store
            .save_log_entry("issue-1", &make_log_entry(1, "event-a", None, None))
            .unwrap();
        store
            .save_log_entry("issue-2", &make_log_entry(1, "event-b", None, None))
            .unwrap();

        let logs1 = store.load_log_entries("issue-1").unwrap();
        let logs2 = store.load_log_entries("issue-2").unwrap();
        assert_eq!(logs1.len(), 1);
        assert_eq!(logs1[0].event_type, "event-a");
        assert_eq!(logs2.len(), 1);
        assert_eq!(logs2[0].event_type, "event-b");
    }

    #[test]
    fn delete_log_entries_removes_all_for_issue() {
        let store = SqliteStore::in_memory().unwrap();
        store
            .save_log_entry("issue-1", &make_log_entry(1, "a", None, None))
            .unwrap();
        store
            .save_log_entry("issue-1", &make_log_entry(2, "b", None, None))
            .unwrap();
        store
            .save_log_entry("issue-2", &make_log_entry(1, "c", None, None))
            .unwrap();

        store.delete_log_entries("issue-1").unwrap();

        assert!(store.load_log_entries("issue-1").unwrap().is_empty());
        assert_eq!(store.load_log_entries("issue-2").unwrap().len(), 1);
    }

    #[test]
    fn delete_log_entries_nonexistent_is_ok() {
        let store = SqliteStore::in_memory().unwrap();
        store.delete_log_entries("nonexistent").unwrap();
    }

    #[test]
    fn log_entry_preserves_ordering() {
        let store = SqliteStore::in_memory().unwrap();
        for i in 0..10 {
            store
                .save_log_entry(
                    "issue-1",
                    &make_log_entry(i, &format!("event-{i}"), None, None),
                )
                .unwrap();
        }
        let loaded = store.load_log_entries("issue-1").unwrap();
        assert_eq!(loaded.len(), 10);
        for (i, entry) in loaded.iter().enumerate() {
            assert_eq!(entry.seq, i as u64);
        }
    }

    // ── Agent totals ────────────────────────────────────────────────────

    #[test]
    fn load_agent_totals_empty_returns_default() {
        let store = SqliteStore::in_memory().unwrap();
        let totals = store.load_agent_totals().unwrap();
        assert_eq!(totals.input_tokens, 0);
        assert_eq!(totals.output_tokens, 0);
        assert_eq!(totals.total_tokens, 0);
        assert_eq!(totals.seconds_running, 0.0);
    }

    #[test]
    fn save_and_load_agent_totals() {
        let store = SqliteStore::in_memory().unwrap();
        let totals = AgentTotals {
            input_tokens: 1000,
            output_tokens: 500,
            total_tokens: 1500,
            seconds_running: 123.456,
        };
        store.save_agent_totals(&totals).unwrap();

        let loaded = store.load_agent_totals().unwrap();
        assert_eq!(loaded.input_tokens, 1000);
        assert_eq!(loaded.output_tokens, 500);
        assert_eq!(loaded.total_tokens, 1500);
        assert!((loaded.seconds_running - 123.456).abs() < 0.001);
    }

    #[test]
    fn save_agent_totals_upserts() {
        let store = SqliteStore::in_memory().unwrap();
        let t1 = AgentTotals {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            seconds_running: 10.0,
        };
        store.save_agent_totals(&t1).unwrap();

        let t2 = AgentTotals {
            input_tokens: 200,
            output_tokens: 100,
            total_tokens: 300,
            seconds_running: 20.0,
        };
        store.save_agent_totals(&t2).unwrap();

        let loaded = store.load_agent_totals().unwrap();
        assert_eq!(loaded.input_tokens, 200);
        assert_eq!(loaded.total_tokens, 300);
    }

    #[test]
    fn save_agent_totals_zero_values() {
        let store = SqliteStore::in_memory().unwrap();
        let totals = AgentTotals::default();
        store.save_agent_totals(&totals).unwrap();

        let loaded = store.load_agent_totals().unwrap();
        assert_eq!(loaded.input_tokens, 0);
        assert_eq!(loaded.seconds_running, 0.0);
    }

    // ── Completed issues ────────────────────────────────────────────────

    #[test]
    fn mark_completed_and_load() {
        let store = SqliteStore::in_memory().unwrap();
        store.mark_completed("issue-1").unwrap();
        store.mark_completed("issue-2").unwrap();

        let ids = store.load_completed_ids().unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"issue-1".to_string()));
        assert!(ids.contains(&"issue-2".to_string()));
    }

    #[test]
    fn mark_completed_duplicate_is_idempotent() {
        let store = SqliteStore::in_memory().unwrap();
        store.mark_completed("issue-1").unwrap();
        store.mark_completed("issue-1").unwrap();

        let ids = store.load_completed_ids().unwrap();
        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn load_completed_ids_empty() {
        let store = SqliteStore::in_memory().unwrap();
        let ids = store.load_completed_ids().unwrap();
        assert!(ids.is_empty());
    }

    // ── PersistedSession serde ──────────────────────────────────────────

    #[test]
    fn persisted_session_serialize_deserialize_roundtrip() {
        let session = make_session("id-1", "TST-1");
        let json = serde_json::to_string(&session).unwrap();
        let deserialized: PersistedSession = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.issue_id, "id-1");
        assert_eq!(deserialized.identifier, "TST-1");
        assert_eq!(deserialized.input_tokens, 100);
    }

    #[test]
    fn persisted_session_debug() {
        let session = make_session("id-1", "TST-1");
        let debug = format!("{:?}", session);
        assert!(debug.contains("PersistedSession"));
    }

    #[test]
    fn persisted_session_clone() {
        let session = make_session("id-1", "TST-1");
        let cloned = session.clone();
        assert_eq!(session.issue_id, cloned.issue_id);
        assert_eq!(session.identifier, cloned.identifier);
    }

    // ── PersistenceError ────────────────────────────────────────────────

    #[test]
    fn persistence_error_display_sqlite() {
        let err = PersistenceError::Sqlite(rusqlite::Error::InvalidQuery);
        let msg = err.to_string();
        assert!(msg.contains("sqlite error"));
    }

    #[test]
    fn persistence_error_display_lock() {
        let err = PersistenceError::LockPoisoned;
        let msg = err.to_string();
        assert!(msg.contains("lock poisoned"));
    }

    #[test]
    fn persistence_error_debug() {
        let err = PersistenceError::LockPoisoned;
        let debug = format!("{:?}", err);
        assert!(debug.contains("LockPoisoned"));
    }

    // ── File-based DB persistence across re-opens ───────────────────────

    #[test]
    fn data_persists_across_reopen() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_owned();

        // Write data
        {
            let store = SqliteStore::new(&path).unwrap();
            store.save_session(&make_session("id-1", "TST-1")).unwrap();
            store
                .save_agent_totals(&AgentTotals {
                    input_tokens: 42,
                    output_tokens: 0,
                    total_tokens: 42,
                    seconds_running: 1.0,
                })
                .unwrap();
            store.mark_completed("id-2").unwrap();
            store
                .save_log_entry("id-1", &make_log_entry(1, "test", None, None))
                .unwrap();
        }

        // Re-open and verify
        {
            let store = SqliteStore::new(&path).unwrap();
            let sessions = store.load_sessions().unwrap();
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].issue_id, "id-1");

            let totals = store.load_agent_totals().unwrap();
            assert_eq!(totals.input_tokens, 42);

            let completed = store.load_completed_ids().unwrap();
            assert_eq!(completed, vec!["id-2".to_string()]);

            let logs = store.load_log_entries("id-1").unwrap();
            assert_eq!(logs.len(), 1);
        }
    }

    // ── Log entry with partial token data ───────────────────────────────

    #[test]
    fn log_entry_with_no_tokens() {
        let store = SqliteStore::in_memory().unwrap();
        let entry = make_log_entry(1, "notification", Some("hello"), None);
        store.save_log_entry("id-1", &entry).unwrap();

        let loaded = store.load_log_entries("id-1").unwrap();
        assert!(loaded[0].tokens.is_none());
        assert_eq!(loaded[0].message, Some("hello".to_string()));
    }

    // ── Helpers ─────────────────────────────────────────────────────────

    fn make_session(issue_id: &str, identifier: &str) -> PersistedSession {
        PersistedSession {
            issue_id: issue_id.to_string(),
            identifier: identifier.to_string(),
            session_id: Some("sess-abc".to_string()),
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            turn_count: 3,
            started_at: Utc::now().to_rfc3339(),
            ended_at: None,
            last_event: Some("turn_completed".to_string()),
            last_event_at: Some(Utc::now().to_rfc3339()),
            last_message: Some("Done".to_string()),
            retry_attempt: Some(1),
        }
    }

    fn make_log_entry(
        seq: u64,
        event_type: &str,
        message: Option<&str>,
        tokens: Option<(u64, u64, u64)>,
    ) -> LogEntry {
        LogEntry {
            seq,
            timestamp: Utc::now(),
            event_type: event_type.to_string(),
            message: message.map(|s| s.to_string()),
            tokens: tokens.map(|(i, o, t)| TokenSnapshot {
                input_tokens: i,
                output_tokens: o,
                total_tokens: t,
            }),
        }
    }
}
