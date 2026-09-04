//! Per-session state for the `plan-check --claude-hook` revision loop: which
//! rules are already settled, how many rounds have run, and the durable,
//! append-only record of every explicit override and round-limit pass.
//!
//! # Design
//!
//! Mirrors `crate::rules::scope::cache`'s established pattern exactly: state
//! lives under the user's config directory, **never** inside the governed
//! repository, keyed by a hash of an identifier (there, the rules directory
//! path; here, Claude Code's `session_id`), one JSON file per key, best-effort
//! I/O that degrades to "start fresh" rather than surfacing an error — a
//! revision loop that cannot read its own memory should behave as if this
//! were round one, not fail the hook.
//!
//! **Identity.** `session_id` comes from the `PreToolUse` hook envelope: it is
//! stable for an entire Claude Code conversation and a fresh id per session,
//! so it is exactly "one plan-revision loop" with no extra correlation
//! needed. Direct-mode (`actual plan-check` with no `--claude-hook`) has no
//! `session_id` and so never engages this module at all — the caller passes
//! an empty exclude set and skips loading/storing a session, the same
//! fail-open posture as every other hook-only feature in this command.
//!
//! **Keying within a session.** A rule id is only unique within its document
//! (`check::CHECK_OUTPUT_SCHEMA`'s own doc notes the corpus repeats ids across
//! documents), so every key here is `"{doc_slug}::{rule_id}"`, never a bare
//! rule id.
//!
//! **Two stores, two lifetimes.** The session file (`cleared`, `overrides`,
//! `rounds`) is mutable, per-conversation, and pruned after
//! [`SESSION_MAX_AGE`] — it is a cache of "what has this loop already settled
//! or been told to skip," not a record of anything happening. The audit log
//! (`plan-check-overrides.log`) is append-only and never pruned: it is the
//! durable answer to "recorded, not silent" for both an explicit override and
//! a round-limit pass, and must outlive the session cache entry that
//! triggered it.

use std::collections::BTreeSet;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Bumped whenever [`PlanCheckSession`]'s on-disk shape changes incompatibly.
/// A mismatched version is treated as a miss (start fresh), the same
/// tolerance `rules::scope::cache` gives `INDEX_FORMAT_VERSION`.
const FORMAT_VERSION: u32 = 1;

/// Subdirectory of the config directory holding per-session state.
const SESSIONS_DIR_NAME: &str = "plan-check-sessions";

/// Filename of the append-only override/round-limit audit log, directly
/// under the config directory (not the sessions subdirectory: it must
/// outlive any single session's cache entry).
const AUDIT_LOG_NAME: &str = "plan-check-overrides.log";

/// A session file older than this is pruned the next time any session is
/// stored. Bounds disk usage without needing a `SessionEnd` hook, which
/// Claude Code does not offer here.
const SESSION_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// One plan-revision loop's memory: what has already been judged conforming
/// or explicitly overridden, and how many rounds have run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlanCheckSession {
    format_version: u32,
    /// How many times a real judge call has completed for this session.
    /// Fail-open outcomes (no runner, no applicable rules, a crashed judge
    /// call) never increment this — nothing was actually checked.
    pub rounds: u32,
    /// `"{doc_slug}::{rule_id}"` for every rule ever judged
    /// [`crate::rules::check::Verdict::Conforming`] in this session. Never
    /// re-judged: see the module doc's "recorded, not silent" note for why a
    /// judge flip-flop must not be able to re-raise these.
    pub cleared: BTreeSet<String>,
    /// Every explicit, human-issued override recorded against this session.
    pub overrides: Vec<Override>,
}

/// One explicit override: a human, outside the agent's control, telling this
/// specific rule to stop blocking this specific session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Override {
    /// `"{doc_slug}::{rule_id}"`.
    pub key: String,
    pub reason: String,
    pub at: DateTime<Utc>,
    /// The round this override was recorded during, for the audit trail.
    pub round: u32,
}

impl PlanCheckSession {
    /// Every key this session must never send to the judge again: cleared by
    /// a prior conforming verdict, or explicitly overridden.
    pub fn settled(&self) -> BTreeSet<String> {
        let mut settled = self.cleared.clone();
        settled.extend(self.overrides.iter().map(|o| o.key.clone()));
        settled
    }

    /// True when `key` was explicitly overridden (as opposed to merely
    /// cleared by the judge) — used to decide whether a round owes the human
    /// a reminder notice.
    pub fn is_overridden(&self, key: &str) -> bool {
        self.overrides.iter().any(|o| o.key == key)
    }
}

/// The settled-rule key: a rule id is only unique within its document.
pub fn key(doc_slug: &str, rule_id: &str) -> String {
    format!("{doc_slug}::{rule_id}")
}

fn sessions_dir() -> Option<PathBuf> {
    crate::config::paths::config_dir()
        .ok()
        .map(|dir| dir.join(SESSIONS_DIR_NAME))
}

fn session_path(session_id: &str) -> Option<PathBuf> {
    let mut hasher = Sha256::new();
    hasher.update(session_id.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    sessions_dir().map(|dir| dir.join(format!("{hex}.json")))
}

/// Load the session for `session_id`, or a fresh empty one when absent,
/// unreadable, unparseable, or written by an incompatible format version.
/// Every failure mode degrades to "start fresh" — the same tolerance
/// `rules::scope::cache::load` gives a stale or corrupt entry.
pub fn load(session_id: &str) -> PlanCheckSession {
    let Some(path) = session_path(session_id) else {
        return PlanCheckSession::default();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return PlanCheckSession::default();
    };
    let Ok(session) = serde_json::from_str::<PlanCheckSession>(&text) else {
        return PlanCheckSession::default();
    };
    if session.format_version != FORMAT_VERSION {
        return PlanCheckSession::default();
    }
    session
}

/// Persist `session` for `session_id`. Best-effort: a write failure costs
/// this session's memory, not the hook call.
///
/// Also opportunistically prunes session files older than [`SESSION_MAX_AGE`]
/// — bounded by one directory listing, so the cost stays proportional to how
/// many sessions are actually on disk rather than growing unbounded.
pub fn store(session_id: &str, session: &PlanCheckSession) {
    let Some(path) = session_path(session_id) else {
        return;
    };
    let mut to_write = session.clone();
    to_write.format_version = FORMAT_VERSION;
    let Ok(json) = serde_json::to_string(&to_write) else {
        return;
    };
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let _ = crate::config::paths::write_secure(&path, json.as_bytes());
    prune_stale(parent);
}

/// Remove every session file under `dir` whose modification time is older
/// than [`SESSION_MAX_AGE`]. Best-effort: an unreadable directory or entry is
/// skipped, never an error.
fn prune_stale(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if now.duration_since(modified).unwrap_or_default() > SESSION_MAX_AGE {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// What triggered a durable audit-log entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditKind {
    /// A human ran `actual plan-check-override`.
    Override,
    /// The round limit was hit with a rule still conflicting, and the gate
    /// stopped blocking rather than denying indefinitely.
    RoundLimit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AuditEntry {
    at: DateTime<Utc>,
    session_id: String,
    key: String,
    reason: String,
    round: u32,
    kind: AuditKind,
}

/// `pub(crate)` rather than private so integration-style tests in
/// `plan_check.rs` can assert an audit entry actually landed, without this
/// module exposing the log's location as part of its public API.
pub(crate) fn audit_log_path() -> Option<PathBuf> {
    crate::config::paths::config_dir()
        .ok()
        .map(|dir| dir.join(AUDIT_LOG_NAME))
}

/// Append one line to the durable, never-pruned audit log. Best-effort: this
/// is a trace for a human to read, not something the hook can act on if it
/// fails, so a write failure is silent rather than fatal.
fn append_audit(entry: &AuditEntry) {
    let Some(path) = audit_log_path() else { return };
    let Ok(mut line) = serde_json::to_string(entry) else {
        return;
    };
    line.push('\n');
    let _ = crate::config::paths::append_secure(&path, line.as_bytes());
}

/// Record an explicit, human-issued override: mark `keys` settled for
/// `session_id` and append one audit-log entry per key. Loads-and-stores the
/// session itself, so the caller does not need to separately `load`/`store`.
pub fn record_override(session_id: &str, keys: &[String], reason: &str) {
    let mut session = load(session_id);
    let at = Utc::now();
    for key in keys {
        session.overrides.push(Override {
            key: key.clone(),
            reason: reason.to_string(),
            at,
            round: session.rounds,
        });
        append_audit(&AuditEntry {
            at,
            session_id: session_id.to_string(),
            key: key.clone(),
            reason: reason.to_string(),
            round: session.rounds,
            kind: AuditKind::Override,
        });
    }
    store(session_id, &session);
}

/// Append one round-limit audit-log entry per still-conflicting key. Does
/// not touch the session file itself — the caller has already incremented
/// `rounds` and will `store` it.
pub fn record_round_limit(session_id: &str, round: u32, keys: &[String], reason: &str) {
    let at = Utc::now();
    for key in keys {
        append_audit(&AuditEntry {
            at,
            session_id: session_id.to_string(),
            key: key.clone(),
            reason: reason.to_string(),
            round,
            kind: AuditKind::RoundLimit,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use tempfile::tempdir;

    fn with_config_dir() -> (tempfile::TempDir, EnvGuard, EnvGuard) {
        let home = tempdir().unwrap();
        let g1 = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let g2 = EnvGuard::remove("ACTUAL_CONFIG");
        (home, g1, g2)
    }

    #[test]
    fn test_load_absent_session_is_default() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = with_config_dir();
        let session = load("brand-new-session");
        assert_eq!(session, PlanCheckSession::default());
    }

    #[test]
    fn test_store_then_load_roundtrips() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = with_config_dir();

        let mut session = PlanCheckSession {
            rounds: 2,
            ..Default::default()
        };
        session.cleared.insert(key("doc-a", "R-001"));
        store("session-1", &session);

        let loaded = load("session-1");
        assert_eq!(loaded.rounds, 2);
        assert!(loaded.cleared.contains(&key("doc-a", "R-001")));
    }

    #[test]
    fn test_different_session_ids_do_not_collide() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = with_config_dir();

        let mut a = PlanCheckSession::default();
        a.cleared.insert(key("doc", "R-A"));
        store("session-a", &a);

        let mut b = PlanCheckSession::default();
        b.cleared.insert(key("doc", "R-B"));
        store("session-b", &b);

        assert!(load("session-a").cleared.contains(&key("doc", "R-A")));
        assert!(!load("session-a").cleared.contains(&key("doc", "R-B")));
        assert!(load("session-b").cleared.contains(&key("doc", "R-B")));
    }

    #[test]
    fn test_load_ignores_a_stale_format_version() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = with_config_dir();

        let path = session_path("session-x").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::json!({
                "format_version": FORMAT_VERSION + 1,
                "rounds": 5,
                "cleared": [],
                "overrides": [],
            })
            .to_string(),
        )
        .unwrap();

        assert_eq!(load("session-x"), PlanCheckSession::default());
    }

    #[test]
    fn test_load_ignores_corrupt_json() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = with_config_dir();

        let path = session_path("session-corrupt").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();

        assert_eq!(load("session-corrupt"), PlanCheckSession::default());
    }

    #[test]
    fn test_settled_includes_both_cleared_and_overridden() {
        let mut session = PlanCheckSession::default();
        session.cleared.insert(key("doc", "R-clear"));
        session.overrides.push(Override {
            key: key("doc", "R-over"),
            reason: "reviewed".to_string(),
            at: Utc::now(),
            round: 1,
        });

        let settled = session.settled();
        assert!(settled.contains(&key("doc", "R-clear")));
        assert!(settled.contains(&key("doc", "R-over")));
        assert!(session.is_overridden(&key("doc", "R-over")));
        assert!(!session.is_overridden(&key("doc", "R-clear")));
    }

    #[test]
    fn test_record_override_marks_settled_and_writes_audit_log() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _g1, _g2) = with_config_dir();

        record_override(
            "session-override",
            &[key("doc", "R-001")],
            "reviewed by security team",
        );

        let session = load("session-override");
        assert!(session.settled().contains(&key("doc", "R-001")));
        assert!(session.is_overridden(&key("doc", "R-001")));

        let log_path = audit_log_path().unwrap();
        let log = std::fs::read_to_string(log_path).unwrap();
        assert!(log.contains("R-001"));
        assert!(log.contains("reviewed by security team"));
        assert!(log.contains("\"kind\":\"override\""));
    }

    #[test]
    fn test_record_round_limit_writes_audit_log_without_touching_session() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _g1, _g2) = with_config_dir();

        record_round_limit(
            "session-limit",
            3,
            &[key("doc", "R-002")],
            "still conflicting after the round limit",
        );

        // The session file itself was never created by record_round_limit.
        assert_eq!(load("session-limit"), PlanCheckSession::default());

        let log = std::fs::read_to_string(audit_log_path().unwrap()).unwrap();
        assert!(log.contains("R-002"));
        assert!(log.contains("\"kind\":\"round_limit\""));
    }

    #[test]
    fn test_audit_log_is_append_only_across_multiple_events() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _g1, _g2) = with_config_dir();

        record_override("s1", &[key("doc", "R-001")], "first");
        record_override("s1", &[key("doc", "R-002")], "second");

        let log = std::fs::read_to_string(audit_log_path().unwrap()).unwrap();
        assert_eq!(log.lines().count(), 2);
        assert!(log.contains("R-001"));
        assert!(log.contains("R-002"));
    }

    #[cfg(unix)]
    #[test]
    fn test_session_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = with_config_dir();

        store("session-perms", &PlanCheckSession::default());
        let mode = std::fs::metadata(session_path("session-perms").unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn test_session_path_is_outside_the_repository_config_dir_pattern() {
        // Same guarantee as rules::scope::cache: state never lands anywhere
        // under a target repo, only under the resolved config directory.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (home, _g1, _g2) = with_config_dir();
        let path = session_path("session-loc").unwrap();
        assert!(path.starts_with(home.path()));
        assert_eq!(
            path.parent().unwrap().file_name().unwrap(),
            SESSIONS_DIR_NAME
        );
    }
}
