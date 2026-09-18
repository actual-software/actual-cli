//! Per-session state for the `--claude-hook` revision loop (`plan-check
//! --claude-hook` and `impl-check --claude-hook` both drive it): which rules
//! are already settled, how many rounds have run, and the durable,
//! append-only record of every explicit override and round-limit pass.
//!
//! # Design
//!
//! Mirrors `crate::rules::scope::cache`'s established pattern exactly: state
//! lives under the user's config directory, **never** inside the governed
//! repository, keyed by a hash of an identifier, one JSON file per key,
//! best-effort I/O that degrades to "start fresh" rather than surfacing an
//! error — a revision loop that cannot read its own memory should behave as
//! if this were round one, not fail the hook.
//!
//! **Identity is `(session_id, rules_dir)`, not `session_id` alone.**
//! `session_id` comes from the `PreToolUse` hook envelope: it is stable for
//! an entire Claude Code conversation, but one conversation can legitimately
//! govern more than one context — `ACTUAL_RULES_DIR` exists precisely so a
//! monorepo's subprojects each have their own `.actual/rules/`, and those are
//! plausibly synced from the same central ADR bank, so identical
//! `doc_slug::rule_id` pairs across two subprojects (or two entirely separate
//! repos sharing a synced corpus) are the expected case, not a coincidence.
//! Keying on `session_id` alone would let a pass in one context silently
//! suppress judging in the other. `rules_dir`'s *path* is hashed into the key
//! (the same construction `rules::scope::cache::cache_path` uses for its own
//! key) rather than its content: two repos with byte-identical rule text
//! must still get independent state, because a clearance is a fact about
//! "this artifact, in this governed context," never about the rule's wording.
//! Direct-mode (e.g. `actual plan-check` with no `--claude-hook`) has no
//! `session_id` and so never engages this module at all — the caller passes
//! an empty, default session and skips loading/storing one, the same
//! fail-open posture as every other hook-only feature in this command.
//!
//! **Loop memory is per [`crate::rules::check::ArtifactKind`], overrides are
//! not.** `plan-check` and `impl-check` share one file so a human
//! `check-override` still settles a rule for both gates of the same effort.
//! Deny counts, round counters, and content-scoped clearances live in
//! per-kind [`LoopState`] maps inside that file: a plan-stage denial must
//! not spend the implementation-stage `--max-rounds` budget, and a plan
//! round must not rotate impl-check's judged window. `ACTUAL_IMPL_CHECK_MAX_ROUNDS`
//! as its own env var only makes sense under that split.
//!
//! **Keying within a session.** A rule id is only unique within its document
//! (`check::CHECK_OUTPUT_SCHEMA`'s own doc notes the corpus repeats ids across
//! documents), so every key here is `"{doc_slug}::{rule_id}"`, never a bare
//! rule id.
//!
//! **`cleared` is scoped to the artifact text that earned it, `overrides` are
//! not — deliberately different.** A cleared rule guards against exactly one
//! thing: a non-deterministic judge asked the *same* question twice giving a
//! different answer. It is not a standing pass. So each loop's `cleared` map
//! keys a rule to the digest of the plan or diff that was judged conforming
//! for it ([`content_digest`]), and [`GovernanceSession::excludes`] only
//! honors that entry for *that* [`crate::rules::check::ArtifactKind`] while
//! the *current* artifact's digest still matches — edit the plan or the
//! working tree and every rule whose relevant text might have changed is
//! judged fresh, never silently waved through on a stale verdict. A plan
//! clearance therefore cannot skip an impl-check of the same rule (the
//! digests differ, and the maps are separate). An override is the opposite
//! kind of fact: a human decided a specific rule does not block this
//! *effort*, not that one exact wording was fine, so it stays keyed to the
//! rule alone, is shared by both loops, and survives any number of plan or
//! diff edits until the human revokes it (there is no revoke command yet —
//! out of scope for this pass).
//!
//! **Two stores, two lifetimes.** The session file (`overrides` plus per-kind
//! `cleared`/`deny_counts`/`rounds`) is mutable, per-conversation, and pruned
//! after [`SESSION_MAX_AGE`] — it is a cache of "what has this loop already
//! settled or been told to skip," not a record of anything happening. The
//! audit log (`plan-check-overrides.log`) is append-only and never pruned: it
//! is the durable answer to "recorded, not silent" for both an explicit
//! override and a round-limit pass, and must outlive the session cache entry
//! that triggered it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::rules::check::ArtifactKind;

/// Bumped whenever [`GovernanceSession`]'s on-disk shape changes incompatibly.
/// A mismatched version is treated as a miss (start fresh), the same
/// tolerance `rules::scope::cache` gives `INDEX_FORMAT_VERSION`.
///
/// 2: `cleared` changed from a flat set of rule keys to a map of rule key ->
/// the plan digest that cleared it (see the module doc's "scoped to the
/// artifact text" note) — an incompatible shape change, not just a new field.
///
/// 3: loop memory (`rounds`, `cleared`, `deny_counts`) nested per
/// [`ArtifactKind`], with `overrides` remaining shared. v2 files are migrated
/// on load (top-level fields become the plan loop; the diff loop starts
/// empty; overrides are kept) rather than discarded — `plan-check` already
/// shipped format 2.
const FORMAT_VERSION: u32 = 3;
const V2_FORMAT_VERSION: u32 = 2;

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

/// One artifact kind's revision-loop memory inside a [`GovernanceSession`].
///
/// `plan-check` and `impl-check` each own one of these so a plan-stage denial
/// cannot spend the implementation-stage budget, and a plan-stage round
/// cannot rotate impl-check's judged window. Overrides live on the parent
/// session, not here — they are a fact about the effort, not about one gate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoopState {
    /// How many times a real judge call has completed for this loop, of any
    /// verdict. Informational only — a round-limit decision is never made
    /// from this alone (see [`deny_counts`](Self::deny_counts)): three clean
    /// or mixed rounds must not spend down a budget meant for "how many
    /// times has this specific rule actually been denied." Fail-open
    /// outcomes (no runner, no applicable rules, a crashed judge call) never
    /// increment this — nothing was actually checked.
    pub rounds: u32,
    /// `"{doc_slug}::{rule_id}"` -> the digest of the artifact text that was
    /// last judged [`crate::rules::check::Verdict::Conforming`] for it (see
    /// [`content_digest`]). A later clearance for the same rule simply
    /// overwrites the entry — only the most recent judgment matters, so this
    /// holds one entry per rule ever cleared, not one per round.
    pub cleared: BTreeMap<String, String>,
    /// `"{doc_slug}::{rule_id}"` -> how many times that specific rule has
    /// been denied (judged [`crate::rules::check::Verdict::Conflicting`]) in
    /// this loop. This is what the round limit actually counts against, per
    /// rule rather than per session: a brand-new conflict always starts at
    /// zero and gets its own full budget, no matter how exhausted some other
    /// rule's count already is — see the module doc.
    pub deny_counts: BTreeMap<String, u32>,
}

impl LoopState {
    /// Record one more denial of `key` in this loop and return its new total.
    /// Called exactly once per conflicting rule per round.
    pub fn record_denial(&mut self, key: &str) -> u32 {
        let count = self.deny_counts.entry(key.to_string()).or_insert(0);
        *count += 1;
        *count
    }

    /// True when `key` has already been denied more times than `max_rounds`
    /// allows — this specific rule's round budget is spent in this loop,
    /// regardless of how many rounds the loop has run in total or how any
    /// other rule's count stands.
    pub fn deny_limit_exceeded(&self, key: &str, max_rounds: u32) -> bool {
        self.deny_counts.get(key).is_some_and(|&n| n > max_rounds)
    }
}

/// One conversation's governance memory for a single `(session_id, rules_dir)`:
/// shared human overrides, plus independent loop state for each artifact kind.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GovernanceSession {
    format_version: u32,
    /// Every explicit, human-issued override recorded against this session.
    /// Shared by both loops: a human decided this rule does not block this
    /// *effort*.
    pub overrides: Vec<Override>,
    /// `plan-check --claude-hook`'s deny counts, rounds, and clearances.
    pub plan: LoopState,
    /// `impl-check --claude-hook`'s deny counts, rounds, and clearances.
    pub diff: LoopState,
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

impl GovernanceSession {
    /// The loop memory for `kind`. `plan-check` always passes
    /// [`ArtifactKind::Plan`]; `impl-check` always passes
    /// [`ArtifactKind::Diff`].
    pub fn loop_state(&self, kind: ArtifactKind) -> &LoopState {
        match kind {
            ArtifactKind::Plan => &self.plan,
            ArtifactKind::Diff => &self.diff,
        }
    }

    /// True when `key` must never be sent to the judge again for an artifact
    /// of `kind` whose digest is `artifact_digest`: either explicitly
    /// overridden (session-scoped, regardless of kind or wording), or judged
    /// conforming against this *exact* artifact text already in *this* loop
    /// (content-scoped — see the module doc). A rule cleared against a plan
    /// that has since been edited, or cleared in the other loop, is not
    /// excluded: `artifact_digest` / `kind` will not match, so it is judged
    /// fresh.
    pub fn excludes(&self, kind: ArtifactKind, key: &str, artifact_digest: &str) -> bool {
        self.is_overridden(key)
            || self
                .loop_state(kind)
                .cleared
                .get(key)
                .is_some_and(|d| d == artifact_digest)
    }

    /// True when `key` was explicitly overridden (as opposed to merely
    /// cleared by the judge) — used to decide whether a round owes the human
    /// a reminder notice.
    pub fn is_overridden(&self, key: &str) -> bool {
        self.overrides.iter().any(|o| o.key == key)
    }

    /// Record one more denial of `key` in `kind`'s loop and return its new
    /// total. Called exactly once per conflicting rule per round.
    pub fn record_denial(&mut self, kind: ArtifactKind, key: &str) -> u32 {
        match kind {
            ArtifactKind::Plan => self.plan.record_denial(key),
            ArtifactKind::Diff => self.diff.record_denial(key),
        }
    }

    /// True when `key` has already been denied more times than `max_rounds`
    /// allows in `kind`'s loop — this specific rule's round budget is spent
    /// for that gate, regardless of how the other gate's count stands.
    pub fn deny_limit_exceeded(&self, kind: ArtifactKind, key: &str, max_rounds: u32) -> bool {
        self.loop_state(kind).deny_limit_exceeded(key, max_rounds)
    }

    /// Informational round for audit entries not tied to a single loop
    /// (`check-override`). Uses the higher of the two loops' round counts.
    fn effort_rounds(&self) -> u32 {
        self.plan.rounds.max(self.diff.rounds)
    }
}

/// The settled-rule key: a rule id is only unique within its document.
pub fn key(doc_slug: &str, rule_id: &str) -> String {
    format!("{doc_slug}::{rule_id}")
}

/// A content digest of `text` (a plan's or a diff's), for scoping a
/// [`GovernanceSession`]'s `cleared` entries to the exact wording that earned
/// them. Same construction as `rules::scope::cache`'s content-hashed keys
/// (SHA-256, hex-encoded) — deliberately the raw text's hash, not a
/// normalized or excerpted one: isolating which rule's *relevant* text
/// changed would need per-rule span tracking this module does not have, so
/// any edit at all is treated as "re-judge everything previously cleared in
/// scope," which is the safe direction to err in.
pub fn content_digest(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn sessions_dir() -> Option<PathBuf> {
    crate::config::paths::config_dir()
        .ok()
        .map(|dir| dir.join(SESSIONS_DIR_NAME))
}

/// The on-disk key for `(session_id, rules_dir)`: `rules_dir`'s *path* is
/// hashed in alongside `session_id`, not its content — see the module doc's
/// "identity" note for why two repos with identical rule text must still
/// resolve to independent state. A NUL separator between the two inputs
/// avoids a session/path split ambiguity (`"ab"` + `"/c"` must not collide
/// with `"a"` + `"b/c"`).
fn session_path(session_id: &str, rules_dir: &std::path::Path) -> Option<PathBuf> {
    let mut hasher = Sha256::new();
    hasher.update(session_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(rules_dir.as_os_str().as_encoded_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    sessions_dir().map(|dir| dir.join(format!("{hex}.json")))
}

/// Load the session for `(session_id, rules_dir)`, or a fresh empty one when
/// absent, unreadable, unparseable, or written by an incompatible format
/// version. Format 2 (flat `rounds`/`cleared`/`deny_counts`) is migrated
/// into the plan loop rather than discarded — see [`FORMAT_VERSION`]. Every
/// other failure mode degrades to "start fresh" — the same tolerance
/// `rules::scope::cache::load` gives a stale or corrupt entry.
pub fn load(session_id: &str, rules_dir: &std::path::Path) -> GovernanceSession {
    let Some(path) = session_path(session_id, rules_dir) else {
        return GovernanceSession::default();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return GovernanceSession::default();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return GovernanceSession::default();
    };
    let Some(version) = value.get("format_version").and_then(|v| v.as_u64()) else {
        return GovernanceSession::default();
    };
    match version {
        v if v == u64::from(FORMAT_VERSION) => serde_json::from_value(value).unwrap_or_default(),
        v if v == u64::from(V2_FORMAT_VERSION) => serde_json::from_value::<V2Session>(value)
            .map(migrate_v2)
            .unwrap_or_default(),
        _ => GovernanceSession::default(),
    }
}

/// Format 2's on-disk shape: loop memory was a single flat set of fields,
/// shared accidentally by both gates. Migrated into the plan loop; the diff
/// loop starts empty so in-flight plan-check sessions keep their budget and
/// impl-check starts with a fresh one.
#[derive(Deserialize)]
struct V2Session {
    #[serde(default)]
    rounds: u32,
    #[serde(default)]
    cleared: BTreeMap<String, String>,
    #[serde(default)]
    deny_counts: BTreeMap<String, u32>,
    #[serde(default)]
    overrides: Vec<Override>,
}

fn migrate_v2(v2: V2Session) -> GovernanceSession {
    GovernanceSession {
        format_version: FORMAT_VERSION,
        overrides: v2.overrides,
        plan: LoopState {
            rounds: v2.rounds,
            cleared: v2.cleared,
            deny_counts: v2.deny_counts,
        },
        diff: LoopState::default(),
    }
}

/// Persist `session` for `(session_id, rules_dir)`. Best-effort: a write
/// failure costs this session's memory, not the hook call.
///
/// Also opportunistically prunes session files older than [`SESSION_MAX_AGE`]
/// — bounded by one directory listing, so the cost stays proportional to how
/// many sessions are actually on disk rather than growing unbounded.
pub fn store(session_id: &str, rules_dir: &std::path::Path, session: &GovernanceSession) {
    let Some(path) = session_path(session_id, rules_dir) else {
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
    /// A human ran `actual check-override`.
    Override,
    /// The round limit was hit with a rule still conflicting, and the gate
    /// stopped blocking rather than denying indefinitely.
    RoundLimit,
    /// A round judged only a prefix of the rules that applied (more
    /// candidates than `plan_check`'s own rule-judging cap allows) and found
    /// nothing blocking in that prefix, so the tool call went through with
    /// the rest never evaluated. Unlike [`Override`] and
    /// [`RoundLimit`], this is not a human or a policy acting — it is a
    /// disclosed gap in coverage, recorded so it is inspectable later even
    /// though the hook's own response, which the agent (not a human) is the
    /// one actually reading, is the only place it would otherwise appear.
    PartialCoverage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AuditEntry {
    at: DateTime<Utc>,
    session_id: String,
    /// The rules directory this event applies to — part of the session's
    /// identity (see the module doc), and included here so a human reading
    /// the log can tell two same-named rules in different repos apart.
    rules_dir: String,
    /// `"{doc_slug}::{rule_id}"` for [`AuditKind::Override`] and
    /// [`AuditKind::RoundLimit`]. Empty for [`AuditKind::PartialCoverage`],
    /// which is not about any one rule — see `judged`/`total` instead.
    key: String,
    reason: String,
    round: u32,
    kind: AuditKind,
    /// How many candidate rules were actually judged this round. Present
    /// only for [`AuditKind::PartialCoverage`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    judged: Option<usize>,
    /// How many candidate rules applied in total, including the unjudged
    /// tail. Present only for [`AuditKind::PartialCoverage`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    total: Option<usize>,
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
/// `(session_id, rules_dir)` and append one audit-log entry per key. Loads-
/// and-stores the session itself, so the caller does not need to separately
/// `load`/`store`.
pub fn record_override(
    session_id: &str,
    rules_dir: &std::path::Path,
    keys: &[String],
    reason: &str,
) {
    let mut session = load(session_id, rules_dir);
    let at = Utc::now();
    let rules_dir_str = rules_dir.display().to_string();
    let round = session.effort_rounds();
    for key in keys {
        session.overrides.push(Override {
            key: key.clone(),
            reason: reason.to_string(),
            at,
            round,
        });
        append_audit(&AuditEntry {
            at,
            session_id: session_id.to_string(),
            rules_dir: rules_dir_str.clone(),
            key: key.clone(),
            reason: reason.to_string(),
            round,
            kind: AuditKind::Override,
            judged: None,
            total: None,
        });
    }
    store(session_id, rules_dir, &session);
}

/// Append one round-limit audit-log entry per still-conflicting key. Does
/// not touch the session file itself — the caller has already incremented
/// `rounds` and will `store` it.
pub fn record_round_limit(
    session_id: &str,
    rules_dir: &std::path::Path,
    round: u32,
    keys: &[String],
    reason: &str,
) {
    let at = Utc::now();
    let rules_dir_str = rules_dir.display().to_string();
    for key in keys {
        append_audit(&AuditEntry {
            at,
            session_id: session_id.to_string(),
            rules_dir: rules_dir_str.clone(),
            key: key.clone(),
            reason: reason.to_string(),
            round,
            kind: AuditKind::RoundLimit,
            judged: None,
            total: None,
        });
    }
}

/// Append one audit-log entry recording that a round found nothing blocking
/// but judged only `judged` of `total` applicable rules. Unlike
/// [`record_override`] and [`record_round_limit`], this names no specific
/// rule key — the whole point is that the unjudged tail was never
/// individually identified, only counted — so it is not keyed to any one
/// rule and does not touch the session file itself. Exists so a coverage gap
/// is inspectable after the fact from the durable log, not only visible in
/// the one hook response the agent happened to receive; see
/// `plan_check`'s module doc, "advisory gate" section.
pub fn record_partial_coverage(
    session_id: &str,
    rules_dir: &std::path::Path,
    round: u32,
    judged: usize,
    total: usize,
) {
    append_audit(&AuditEntry {
        at: Utc::now(),
        session_id: session_id.to_string(),
        rules_dir: rules_dir.display().to_string(),
        key: String::new(),
        reason: format!(
            "only {judged} of {total} applicable rules were judged this round; the rest were \
             not evaluated"
        ),
        round,
        kind: AuditKind::PartialCoverage,
        judged: Some(judged),
        total: Some(total),
    });
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
    fn test_content_digest_is_stable_for_identical_text() {
        assert_eq!(
            content_digest("Add caching."),
            content_digest("Add caching.")
        );
    }

    #[test]
    fn test_content_digest_differs_for_different_text() {
        assert_ne!(
            content_digest("Add caching."),
            content_digest("Add logging.")
        );
    }

    /// A fake rules directory path for tests that don't care which one, just
    /// that they use one consistently.
    fn rd() -> std::path::PathBuf {
        std::path::PathBuf::from("/repo-a/.actual/rules")
    }

    #[test]
    fn test_load_absent_session_is_default() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = with_config_dir();
        let session = load("brand-new-session", &rd());
        assert_eq!(session, GovernanceSession::default());
    }

    #[test]
    fn test_store_then_load_roundtrips() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = with_config_dir();

        let mut session = GovernanceSession {
            plan: LoopState {
                rounds: 2,
                ..Default::default()
            },
            ..Default::default()
        };
        session
            .plan
            .cleared
            .insert(key("doc-a", "R-001"), "digest-v1".to_string());
        store("session-1", &rd(), &session);

        let loaded = load("session-1", &rd());
        assert_eq!(loaded.plan.rounds, 2);
        assert_eq!(
            loaded.plan.cleared.get(&key("doc-a", "R-001")),
            Some(&"digest-v1".to_string())
        );
        assert!(loaded.diff.deny_counts.is_empty());
        assert_eq!(loaded.diff.rounds, 0);
    }

    #[test]
    fn test_different_session_ids_do_not_collide() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = with_config_dir();

        let mut a = GovernanceSession::default();
        a.plan.cleared.insert(key("doc", "R-A"), "d".to_string());
        store("session-a", &rd(), &a);

        let mut b = GovernanceSession::default();
        b.plan.cleared.insert(key("doc", "R-B"), "d".to_string());
        store("session-b", &rd(), &b);

        assert!(load("session-a", &rd())
            .plan
            .cleared
            .contains_key(&key("doc", "R-A")));
        assert!(!load("session-a", &rd())
            .plan
            .cleared
            .contains_key(&key("doc", "R-B")));
        assert!(load("session-b", &rd())
            .plan
            .cleared
            .contains_key(&key("doc", "R-B")));
    }

    /// The gap this guards: one Claude Code session can govern more than one
    /// repository or monorepo subproject, so the same `session_id` with two
    /// different `rules_dir` values must resolve to independent state --
    /// otherwise a pass in one context could silently suppress judging the
    /// same `doc_slug::rule_id` in an unrelated one.
    #[test]
    fn test_same_session_id_different_rules_dir_do_not_collide() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = with_config_dir();
        let rules_dir_a = std::path::PathBuf::from("/repo-a/.actual/rules");
        let rules_dir_b = std::path::PathBuf::from("/repo-b/.actual/rules");

        let mut a = GovernanceSession::default();
        a.plan
            .cleared
            .insert(key("cross-cutting-shared-abcd", "R-001"), "d".to_string());
        store("shared-session", &rules_dir_a, &a);

        // Same session_id, different rules_dir, same doc_slug::rule_id: must
        // not see repo A's clearance.
        let loaded_b = load("shared-session", &rules_dir_b);
        assert!(!loaded_b
            .plan
            .cleared
            .contains_key(&key("cross-cutting-shared-abcd", "R-001")));
        // Repo A's own state is untouched.
        assert!(load("shared-session", &rules_dir_a)
            .plan
            .cleared
            .contains_key(&key("cross-cutting-shared-abcd", "R-001")));
    }

    #[test]
    fn test_load_ignores_a_stale_format_version() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = with_config_dir();

        let path = session_path("session-x", &rd()).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::json!({
                "format_version": FORMAT_VERSION + 1,
                "overrides": [],
                "plan": {"rounds": 5, "cleared": {}, "deny_counts": {}},
                "diff": {"rounds": 0, "cleared": {}, "deny_counts": {}},
            })
            .to_string(),
        )
        .unwrap();

        assert_eq!(load("session-x", &rd()), GovernanceSession::default());
    }

    #[test]
    fn test_load_ignores_corrupt_json() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = with_config_dir();

        let path = session_path("session-corrupt", &rd()).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();

        assert_eq!(load("session-corrupt", &rd()), GovernanceSession::default());
    }

    #[test]
    fn test_record_denial_increments_and_returns_the_new_total() {
        let mut session = GovernanceSession::default();
        assert_eq!(
            session.record_denial(ArtifactKind::Plan, &key("doc", "R-A")),
            1
        );
        assert_eq!(
            session.record_denial(ArtifactKind::Plan, &key("doc", "R-A")),
            2
        );
        assert_eq!(
            session.record_denial(ArtifactKind::Plan, &key("doc", "R-A")),
            3
        );
    }

    /// The gap this guards: a per-session (not per-rule) counter would let a
    /// long-exhausted rule's history bleed into an unrelated, brand-new
    /// conflict. Each key's count must be independent.
    #[test]
    fn test_record_denial_is_independent_per_key() {
        let mut session = GovernanceSession::default();
        session.record_denial(ArtifactKind::Plan, &key("doc", "R-A"));
        session.record_denial(ArtifactKind::Plan, &key("doc", "R-A"));
        session.record_denial(ArtifactKind::Plan, &key("doc", "R-A"));
        session.record_denial(ArtifactKind::Plan, &key("doc", "R-A"));
        // A brand-new key must start at zero, not inherit R-A's count.
        assert_eq!(
            session.record_denial(ArtifactKind::Plan, &key("doc", "R-B")),
            1
        );
        assert!(!session.deny_limit_exceeded(ArtifactKind::Plan, &key("doc", "R-B"), 3));
        assert!(session.deny_limit_exceeded(ArtifactKind::Plan, &key("doc", "R-A"), 3));
    }

    #[test]
    fn test_deny_limit_exceeded_false_for_a_never_denied_key() {
        let session = GovernanceSession::default();
        assert!(!session.deny_limit_exceeded(ArtifactKind::Plan, &key("doc", "R-A"), 3));
    }

    #[test]
    fn test_deny_limit_exceeded_true_only_once_the_count_exceeds_max_rounds() {
        let mut session = GovernanceSession::default();
        session.record_denial(ArtifactKind::Plan, &key("doc", "R-A"));
        session.record_denial(ArtifactKind::Plan, &key("doc", "R-A"));
        session.record_denial(ArtifactKind::Plan, &key("doc", "R-A"));
        assert!(!session.deny_limit_exceeded(ArtifactKind::Plan, &key("doc", "R-A"), 3));
        session.record_denial(ArtifactKind::Plan, &key("doc", "R-A"));
        assert!(session.deny_limit_exceeded(ArtifactKind::Plan, &key("doc", "R-A"), 3));
    }

    #[test]
    fn test_excludes_true_for_both_cleared_and_overridden() {
        let mut session = GovernanceSession::default();
        session
            .plan
            .cleared
            .insert(key("doc", "R-clear"), "digest-v1".to_string());
        session.overrides.push(Override {
            key: key("doc", "R-over"),
            reason: "reviewed".to_string(),
            at: Utc::now(),
            round: 1,
        });

        assert!(session.excludes(ArtifactKind::Plan, &key("doc", "R-clear"), "digest-v1"));
        assert!(session.excludes(
            ArtifactKind::Plan,
            &key("doc", "R-over"),
            "any-digest-at-all"
        ));
        assert!(session.is_overridden(&key("doc", "R-over")));
        assert!(!session.is_overridden(&key("doc", "R-clear")));
    }

    /// The gap this guards: a rule cleared against one plan text must not be
    /// excluded once the plan has changed — a stale clearance must not mask
    /// a fresh violation. An override, in contrast, is not scoped to plan
    /// text at all: it stays excluded regardless of which digest is asked.
    #[test]
    fn test_excludes_false_for_a_cleared_rule_once_the_plan_digest_changes() {
        let mut session = GovernanceSession::default();
        session
            .plan
            .cleared
            .insert(key("doc", "R-clear"), "digest-v1".to_string());
        session.overrides.push(Override {
            key: key("doc", "R-over"),
            reason: "reviewed".to_string(),
            at: Utc::now(),
            round: 1,
        });

        assert!(!session.excludes(ArtifactKind::Plan, &key("doc", "R-clear"), "digest-v2"));
        assert!(session.excludes(ArtifactKind::Plan, &key("doc", "R-over"), "digest-v2"));
    }

    #[test]
    fn test_record_override_excludes_regardless_of_plan_digest_and_writes_audit_log() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _g1, _g2) = with_config_dir();

        record_override(
            "session-override",
            &rd(),
            &[key("doc", "R-001")],
            "reviewed by security team",
        );

        let session = load("session-override", &rd());
        assert!(session.excludes(ArtifactKind::Plan, &key("doc", "R-001"), "whatever-digest"));
        assert!(session.excludes(ArtifactKind::Diff, &key("doc", "R-001"), "whatever-digest"));
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
            &rd(),
            3,
            &[key("doc", "R-002")],
            "still conflicting after the round limit",
        );

        // The session file itself was never created by record_round_limit.
        assert_eq!(load("session-limit", &rd()), GovernanceSession::default());

        let log = std::fs::read_to_string(audit_log_path().unwrap()).unwrap();
        assert!(log.contains("R-002"));
        assert!(log.contains("\"kind\":\"round_limit\""));
    }

    #[test]
    fn test_record_partial_coverage_writes_audit_log_without_touching_session() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _g1, _g2) = with_config_dir();

        record_partial_coverage("session-partial", &rd(), 1, 40, 45);

        // Same as record_round_limit: a disclosed coverage gap is not a
        // session-state fact, only a durable log entry.
        assert_eq!(load("session-partial", &rd()), GovernanceSession::default());

        let log = std::fs::read_to_string(audit_log_path().unwrap()).unwrap();
        assert!(log.contains("\"kind\":\"partial_coverage\""));
        assert!(log.contains("\"judged\":40"));
        assert!(log.contains("\"total\":45"));
        // No rule key applies to this kind of event.
        assert!(log.contains("\"key\":\"\""));
    }

    #[test]
    fn test_audit_log_is_append_only_across_multiple_events() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _g1, _g2) = with_config_dir();

        record_override("s1", &rd(), &[key("doc", "R-001")], "first");
        record_override("s1", &rd(), &[key("doc", "R-002")], "second");

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

        store("session-perms", &rd(), &GovernanceSession::default());
        let mode = std::fs::metadata(session_path("session-perms", &rd()).unwrap())
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
        let path = session_path("session-loc", &rd()).unwrap();
        assert!(path.starts_with(home.path()));
        assert_eq!(
            path.parent().unwrap().file_name().unwrap(),
            SESSIONS_DIR_NAME
        );
    }

    #[test]
    fn test_session_path_differs_for_different_rules_dirs() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = with_config_dir();
        let a = session_path(
            "same-session",
            std::path::Path::new("/repo-a/.actual/rules"),
        )
        .unwrap();
        let b = session_path(
            "same-session",
            std::path::Path::new("/repo-b/.actual/rules"),
        )
        .unwrap();
        assert_ne!(a, b);
    }

    /// The gap this guards: plan-check denials used to live in the same
    /// `deny_counts` map impl-check read, so three plan-stage denials of a
    /// rule already exhausted `ACTUAL_IMPL_CHECK_MAX_ROUNDS` before the first
    /// impl-check ran.
    #[test]
    fn test_record_denial_is_independent_per_artifact_kind() {
        let mut session = GovernanceSession::default();
        let key = key("doc", "R-A");
        session.record_denial(ArtifactKind::Plan, &key);
        session.record_denial(ArtifactKind::Plan, &key);
        session.record_denial(ArtifactKind::Plan, &key);
        session.record_denial(ArtifactKind::Plan, &key);
        assert!(session.deny_limit_exceeded(ArtifactKind::Plan, &key, 3));
        assert!(!session.deny_limit_exceeded(ArtifactKind::Diff, &key, 3));
        assert_eq!(session.record_denial(ArtifactKind::Diff, &key), 1);
        assert!(!session.deny_limit_exceeded(ArtifactKind::Diff, &key, 3));
        assert_eq!(session.plan.deny_counts.get(&key), Some(&4));
        assert_eq!(session.diff.deny_counts.get(&key), Some(&1));
    }

    /// A plan clearance must not skip impl-check of the same rule. An
    /// override must skip both, regardless of digest.
    #[test]
    fn test_excludes_cleared_is_per_kind_overrides_are_shared() {
        let mut session = GovernanceSession::default();
        let key = key("doc", "R-A");
        session
            .plan
            .cleared
            .insert(key.clone(), "plan-digest".to_string());

        assert!(session.excludes(ArtifactKind::Plan, &key, "plan-digest"));
        assert!(!session.excludes(ArtifactKind::Diff, &key, "plan-digest"));
        assert!(!session.excludes(ArtifactKind::Diff, &key, "diff-digest"));

        session.overrides.push(Override {
            key: key.clone(),
            reason: "reviewed".to_string(),
            at: Utc::now(),
            round: 1,
        });
        assert!(session.excludes(ArtifactKind::Plan, &key, "other-plan"));
        assert!(session.excludes(ArtifactKind::Diff, &key, "other-diff"));
    }

    /// Format 2 (flat loop memory) must land in the plan loop, leave the
    /// diff loop empty, and keep overrides — in-flight plan-check sessions
    /// and existing human overrides survive the upgrade.
    #[test]
    fn test_load_migrates_v2_into_the_plan_loop_and_leaves_diff_empty() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = with_config_dir();

        let path = session_path("session-v2", &rd()).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::json!({
                "format_version": 2,
                "rounds": 4,
                "cleared": { "doc::R-A": "plan-digest" },
                "deny_counts": { "doc::R-A": 4 },
                "overrides": [{
                    "key": "doc::R-B",
                    "reason": "reviewed",
                    "at": "2026-01-01T00:00:00Z",
                    "round": 2
                }],
            })
            .to_string(),
        )
        .unwrap();

        let loaded = load("session-v2", &rd());
        assert_eq!(loaded.plan.rounds, 4);
        assert_eq!(
            loaded.plan.cleared.get("doc::R-A"),
            Some(&"plan-digest".to_string())
        );
        assert_eq!(loaded.plan.deny_counts.get("doc::R-A"), Some(&4));
        assert_eq!(loaded.diff.rounds, 0);
        assert!(loaded.diff.cleared.is_empty());
        assert!(loaded.diff.deny_counts.is_empty());
        assert_eq!(loaded.overrides.len(), 1);
        assert_eq!(loaded.overrides[0].key, "doc::R-B");
        assert!(loaded.excludes(ArtifactKind::Plan, "doc::R-B", "anything"));
        assert!(loaded.excludes(ArtifactKind::Diff, "doc::R-B", "anything"));
        assert!(loaded.deny_limit_exceeded(ArtifactKind::Plan, "doc::R-A", 3));
        assert!(!loaded.deny_limit_exceeded(ArtifactKind::Diff, "doc::R-A", 3));
    }

    #[test]
    fn test_store_writes_v3_nested_loops() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = with_config_dir();

        let mut session = GovernanceSession::default();
        session.plan.rounds = 2;
        session.diff.rounds = 1;
        session.diff.deny_counts.insert(key("doc", "R-A"), 1);
        store("session-v3", &rd(), &session);

        let raw: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(session_path("session-v3", &rd()).unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(raw["format_version"], 3);
        assert_eq!(raw["plan"]["rounds"], 2);
        assert_eq!(raw["diff"]["rounds"], 1);
        assert_eq!(raw["diff"]["deny_counts"]["doc::R-A"], 1);
        assert!(raw.get("rounds").is_none());
        assert!(raw.get("deny_counts").is_none());
    }
}
