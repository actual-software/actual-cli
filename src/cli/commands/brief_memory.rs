//! Per-session memory for `rules brief`: which decisions the agent has
//! already been briefed on, so a decision is stated once per session.
//!
//! # Design
//!
//! **Its own file, not `governance_session`.** That store holds the plan-check
//! revision loop and the append-only record of human overrides, and an
//! override is a fact that must outlive a context compaction. This memory is
//! the opposite: it is true only while the brief is still in the agent's
//! context, so [`reset`] must be able to wipe it wholesale. Sharing a file
//! would make that wipe either lose overrides or need to pick fields apart.
//! The construction is otherwise the same: under the user's config directory
//! (never the governed repository), one JSON file per key, best-effort I/O
//! that degrades to "nothing briefed yet".
//!
//! **Fail open means over-brief.** An unreadable, corrupt or foreign-version
//! file is an empty session, so the agent is briefed again. The cost of that
//! is some repeated tokens; the cost of the opposite error is an agent that
//! never sees a rule.
//!
//! **Identity is `(session_id, agent_id, rules_dir)`.** `rules_dir` is keyed
//! for the reason `governance_session` gives: one conversation can govern
//! several rule sets. `agent_id` is the hook envelope's subagent id when
//! there is one. A subagent runs with its own context, so a decision briefed
//! to the parent has not been shown to it; if the envelope gives a subagent
//! the parent's `session_id`, keying on `session_id` alone would suppress a
//! brief it never saw. When the envelope carries no `agent_id` the key is the
//! session's alone, so this costs nothing where it is not needed.
//!
//! **The unit is the decision (ADR), not the document.** Sibling documents of
//! one decision restate each other, and the brief already merges them.
//!
//! **Pruning.** There is no `SessionEnd` hook to clean up after, so every
//! store removes files older than [`SESSION_MAX_AGE`].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Bumped whenever [`BriefSession`]'s on-disk shape changes incompatibly. A
/// mismatch reads as an empty session.
const FORMAT_VERSION: u32 = 1;

/// Subdirectory of the config directory holding brief state.
const SESSIONS_DIR_NAME: &str = "brief-sessions";

/// A state file older than this is pruned the next time any is stored.
const SESSION_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// What identifies one agent's context within one rule set.
#[derive(Debug, Clone, Copy)]
pub struct SessionKey<'a> {
    pub session_id: &'a str,
    /// The subagent's id, absent for the main agent.
    pub agent_id: Option<&'a str>,
    pub rules_dir: &'a Path,
}

/// The decisions already briefed in one context.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BriefSession {
    format_version: u32,
    /// `AdrGroup::key` of every decision shown.
    briefed: BTreeSet<String>,
}

impl BriefSession {
    pub fn has_briefed(&self, decision_key: &str) -> bool {
        self.briefed.contains(decision_key)
    }

    pub fn record(&mut self, decision_key: &str) {
        self.briefed.insert(decision_key.to_string());
    }
}

/// The directory brief state lives in, under the user's config directory.
pub fn sessions_dir() -> Option<PathBuf> {
    crate::config::paths::config_dir()
        .ok()
        .map(|dir| dir.join(SESSIONS_DIR_NAME))
}

fn session_path(dir: &Path, key: &SessionKey<'_>) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(key.session_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(key.agent_id.unwrap_or("").as_bytes());
    hasher.update(b"\0");
    hasher.update(key.rules_dir.as_os_str().as_encoded_bytes());
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    dir.join(format!("{hex}.json"))
}

/// The session for `key`, or an empty one when absent, unreadable, corrupt or
/// written by another format version.
pub fn load(dir: &Path, key: &SessionKey<'_>) -> BriefSession {
    std::fs::read_to_string(session_path(dir, key))
        .ok()
        .and_then(|text| serde_json::from_str::<BriefSession>(&text).ok())
        .filter(|session| session.format_version == FORMAT_VERSION)
        .unwrap_or_default()
}

/// Persist `session`, best effort, and prune stale files while there.
pub fn store(dir: &Path, key: &SessionKey<'_>, session: &BriefSession) {
    let mut to_write = session.clone();
    to_write.format_version = FORMAT_VERSION;
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    // Serializing a version and a set of strings cannot fail, so there is no
    // error path to take here; writing nothing is still the right fallback.
    if let Ok(json) = serde_json::to_string(&to_write) {
        let _ = crate::config::paths::write_secure(&session_path(dir, key), json.as_bytes());
    }
    prune_stale(dir);
}

/// Forget what one agent of `session_id` was briefed on, because that
/// context is gone: the main agent when `agent_id` is `None`, else that
/// subagent. Every other context is separate and untouched.
pub fn reset(dir: &Path, session_id: &str, agent_id: Option<&str>, rules_dir: &Path) {
    let key = SessionKey {
        session_id,
        agent_id,
        rules_dir,
    };
    let _ = std::fs::remove_file(session_path(dir, &key));
}

fn prune_stale(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        // A file whose age cannot be read is kept: pruning is a convenience,
        // and deleting state on a failed stat would silently over-brief.
        let stale = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .is_some_and(|modified| {
                now.duration_since(modified).unwrap_or_default() > SESSION_MAX_AGE
            });
        if stale {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::tempdir;

    fn key<'a>(session_id: &'a str, agent_id: Option<&'a str>, rules: &'a Path) -> SessionKey<'a> {
        SessionKey {
            session_id,
            agent_id,
            rules_dir: rules,
        }
    }

    #[test]
    fn test_a_new_session_has_briefed_nothing() {
        let dir = tempdir().unwrap();
        let rules = Path::new("/repo/.actual/rules");
        assert!(!load(dir.path(), &key("s1", None, rules)).has_briefed("Adopt RS256"));
    }

    #[test]
    fn test_recorded_decisions_survive_a_reload() {
        let dir = tempdir().unwrap();
        let rules = Path::new("/repo/.actual/rules");
        let k = key("s1", None, rules);
        let mut session = load(dir.path(), &k);
        session.record("Adopt RS256");
        store(dir.path(), &k, &session);

        let again = load(dir.path(), &k);
        assert!(again.has_briefed("Adopt RS256"));
        assert!(!again.has_briefed("Pin Providers"));
    }

    /// One conversation over two rule sets keeps two memories.
    #[test]
    fn test_state_is_keyed_by_session_agent_and_rules_dir() {
        let dir = tempdir().unwrap();
        let a = Path::new("/repo/a/.actual/rules");
        let b = Path::new("/repo/b/.actual/rules");
        let mut session = BriefSession::default();
        session.record("Adopt RS256");
        store(dir.path(), &key("s1", None, a), &session);

        assert!(load(dir.path(), &key("s1", None, a)).has_briefed("Adopt RS256"));
        assert!(!load(dir.path(), &key("s1", None, b)).has_briefed("Adopt RS256"));
        assert!(!load(dir.path(), &key("s2", None, a)).has_briefed("Adopt RS256"));
        assert!(!load(dir.path(), &key("s1", Some("sub-1"), a)).has_briefed("Adopt RS256"));
    }

    #[test]
    fn test_reset_forgets_the_main_agent_only() {
        let dir = tempdir().unwrap();
        let rules = Path::new("/repo/.actual/rules");
        let mut session = BriefSession::default();
        session.record("Adopt RS256");
        store(dir.path(), &key("s1", None, rules), &session);
        store(dir.path(), &key("s1", Some("sub-1"), rules), &session);

        reset(dir.path(), "s1", None, rules);

        assert!(!load(dir.path(), &key("s1", None, rules)).has_briefed("Adopt RS256"));
        assert!(load(dir.path(), &key("s1", Some("sub-1"), rules)).has_briefed("Adopt RS256"));
    }

    /// Resetting what was never stored is not an error.
    #[test]
    fn test_reset_of_an_unknown_session_is_harmless() {
        let dir = tempdir().unwrap();
        reset(
            dir.path(),
            "never-seen",
            None,
            Path::new("/repo/.actual/rules"),
        );
    }

    /// Fail open: a damaged file is an empty session, so the agent is briefed
    /// again rather than never.
    #[test]
    fn test_a_corrupt_state_file_reads_as_empty() {
        let dir = tempdir().unwrap();
        let rules = Path::new("/repo/.actual/rules");
        let k = key("s1", None, rules);
        std::fs::write(session_path(dir.path(), &k), "{ not json").unwrap();
        assert!(!load(dir.path(), &k).has_briefed("Adopt RS256"));

        std::fs::write(
            session_path(dir.path(), &k),
            r#"{"format_version":99,"briefed":["Adopt RS256"]}"#,
        )
        .unwrap();
        assert!(!load(dir.path(), &k).has_briefed("Adopt RS256"));

        std::fs::write(
            session_path(dir.path(), &k),
            r#"{"briefed":["Adopt RS256"]}"#,
        )
        .unwrap();
        assert!(!load(dir.path(), &k).has_briefed("Adopt RS256"));
    }

    #[test]
    fn test_storing_into_an_unwritable_place_is_silent() {
        let dir = tempdir().unwrap();
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, "x").unwrap();
        let rules = Path::new("/repo/.actual/rules");
        // A directory cannot be created beneath a regular file.
        store(
            &blocker.join("sub"),
            &key("s1", None, rules),
            &BriefSession::default(),
        );
    }

    /// Pruning a directory that cannot be read is not an error: there is
    /// nothing to clean up, and the caller's own write has already happened.
    #[test]
    fn test_pruning_an_unreadable_directory_is_harmless() {
        let dir = tempdir().unwrap();
        prune_stale(&dir.path().join("never-created"));
    }

    /// Only this store's own files are candidates. Anything else in the
    /// directory is left alone, however old it is.
    #[test]
    fn test_pruning_leaves_files_it_does_not_own() {
        let dir = tempdir().unwrap();
        let rules = Path::new("/repo/.actual/rules");
        let foreign = dir.path().join("notes.txt");
        std::fs::write(&foreign, "not ours").unwrap();
        let eight_days = std::time::Duration::from_secs(8 * 24 * 60 * 60);
        let file = std::fs::File::options().write(true).open(&foreign).unwrap();
        file.set_modified(std::time::SystemTime::now() - eight_days)
            .unwrap();
        drop(file);

        store(
            dir.path(),
            &key("s1", None, rules),
            &BriefSession::default(),
        );

        assert!(foreign.exists(), "a foreign file was pruned");
    }

    #[test]
    fn test_stale_files_are_pruned_and_fresh_ones_kept() {
        let dir = tempdir().unwrap();
        let rules = Path::new("/repo/.actual/rules");
        let old = key("old", None, rules);
        let mut session = BriefSession::default();
        session.record("Adopt RS256");
        store(dir.path(), &old, &session);
        let old_path = session_path(dir.path(), &old);
        let eight_days = std::time::Duration::from_secs(8 * 24 * 60 * 60);
        let file = std::fs::File::options()
            .write(true)
            .open(&old_path)
            .unwrap();
        file.set_modified(std::time::SystemTime::now() - eight_days)
            .unwrap();
        drop(file);

        let fresh = key("fresh", None, rules);
        store(dir.path(), &fresh, &session);

        assert!(!old_path.exists());
        assert!(session_path(dir.path(), &fresh).exists());
    }
}
