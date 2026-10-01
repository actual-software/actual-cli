//! A durable, process-lifetime-safe outbox for scope-selection telemetry
//! (APR-004).
//!
//! `rules select` is a short-lived command, and a detached OS thread that has
//! not finished its send is not guaranteed to outlive the process — so a plain
//! background send can be terminated before delivery, silently dropping the
//! event. Instead, an opted-in event is first written here, to an owner that
//! survives process exit: a small append-only JSONL spool under the config dir,
//! created owner-readable only (`0600`), holding exactly the same
//! `PlanGovernanceEvent` payload that would go on the wire — counts, enums, ids
//! and hashes, never plan or rule text (see `PRIVACY.md`).
//!
//! Delivery is then best-effort and idempotent: [`flush`] sends what the spool
//! holds — in chunks no larger than the proxy's per-batch cap
//! ([`MAX_EVENTS_PER_BATCH`]), so a backlog past that size still drains — and
//! removes an event only once the proxy confirms it recorded (the proxy
//! deduplicates on `insert_id`, so a retry of an event that did land is
//! harmless). The command process never waits for this — it spawns a separate
//! uploader process (see `cli::commands::telemetry_flush`) and returns — and any
//! event left undelivered stays durably queued for the next run's flush. The
//! spool is bounded to [`MAX_SPOOL_EVENTS`] so a long outage cannot grow it
//! without limit; the oldest events are dropped first.
//!
//! Several `rules select` processes can touch the spool at once (each spawns its
//! own uploader), so every mutation — an append, its cap-trim, and the uploader's
//! post-delivery removal — runs under an exclusive advisory lock ([`SpoolLock`])
//! so no accepted event is clobbered or dropped in the window between a read and
//! its overwrite.
//!
//! The lock is a hard precondition for touching the spool, never a fallback to an
//! unlocked write: if it cannot be acquired, the mutation does not happen. These
//! events are best-effort, so [`append`] treats that as an intentional fail-open
//! — the new event is *dropped* for this run rather than persisted unlocked, and
//! the caller records the drop (it never blocks or fails the command). Durability
//! therefore holds for an event the spool actually accepts; an event the lock
//! turns away is a best-effort loss, not a guaranteed-persisted one.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::api::types::PlanGovernanceEvent;
use crate::config::types::Config;
use crate::telemetry::plan_governance::{deliver_events, Delivery, MAX_EVENTS_PER_BATCH};

/// Filename under the config dir holding the pending scope events, one JSON
/// object per line. Sibling to the `telemetry-id` install file.
const SPOOL_FILE: &str = "scope-telemetry-spool.jsonl";

/// Upper bound on retained events. A reachable endpoint keeps the spool near
/// empty; this only caps the pathological case of a persistently unreachable
/// endpoint, dropping the oldest events rather than growing without limit.
pub(crate) const MAX_SPOOL_EVENTS: usize = 256;

/// Sibling lock file guarding every mutation of the spool. See [`SpoolLock`].
const LOCK_FILE: &str = "scope-telemetry-spool.lock";

/// The spool file path, or `None` when the config dir cannot be resolved (in
/// which case there is nowhere durable to write — the same fallback the
/// per-install `distinct_id` makes).
fn spool_path() -> Option<PathBuf> {
    crate::config::paths::config_dir()
        .ok()
        .map(|dir| dir.join(SPOOL_FILE))
}

/// The lock file path, resolved the same way as [`spool_path`].
fn lock_path() -> Option<PathBuf> {
    crate::config::paths::config_dir()
        .ok()
        .map(|dir| dir.join(LOCK_FILE))
}

/// An RAII holder of the spool's exclusive advisory lock. While it is alive this
/// process holds the lock against every other process that opened the same lock
/// file; dropping it (or the process exiting) releases the lock.
///
/// The lock serializes the read-modify-write in [`append`] against another
/// append's cap-trim rename and against [`flush`]'s post-delivery removal — the
/// two windows in which a concurrent writer could otherwise clobber a queued
/// event or delete one that landed mid-flush. The network send itself is
/// deliberately *not* held under the lock, so a slow uploader never blocks a
/// `rules select` from appending.
struct SpoolLock {
    // Held only for its lock; the OS releases the advisory lock when the fd is
    // closed on drop, so the field is never read directly.
    _file: std::fs::File,
}

impl SpoolLock {
    /// Take the exclusive spool lock, blocking until it is held. Returns `None`
    /// when the lock cannot be established — no resolvable config dir, the lock
    /// file cannot be opened, or the advisory lock itself fails.
    ///
    /// A `None` never licenses an unlocked mutation: [`append`] and
    /// [`remove_delivered`] treat it as "did not run" and leave the spool exactly
    /// as it was, so a coordination failure can never clobber or delete a
    /// concurrent event (APR-007). The affected event is simply retried on a
    /// later run, where the proxy's `insert_id` dedup makes a re-send harmless.
    fn acquire() -> Option<Self> {
        let path = lock_path()?;
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let file = open_lock_file(&path)?;
        // Blocks until this process holds the lock exclusively; released on drop.
        file.lock().ok()?;
        Some(Self { _file: file })
    }
}

/// Open (creating if needed) the lock file, owner-only on unix. The file's
/// contents are never written or read — it exists purely as a lock target.
#[cfg(unix)]
fn open_lock_file(path: &Path) -> Option<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .ok()
}

#[cfg(not(unix))]
fn open_lock_file(path: &Path) -> Option<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .ok()
}

/// The API base URL a flush should send to: the configured one, else the
/// production default (same resolution every other telemetry send uses).
pub(crate) fn resolved_api_url(config: &Config) -> String {
    config
        .api_url
        .clone()
        .unwrap_or_else(|| crate::api::client::DEFAULT_API_URL.to_string())
}

/// Append one event to the durable spool, keeping the file bounded.
///
/// Best-effort: any I/O or serialization error is returned for the caller to
/// swallow, since a failed spool write must never fail the command. When the
/// spool already holds [`MAX_SPOOL_EVENTS`] lines the oldest are dropped so the
/// file stays bounded.
pub(crate) fn append(event: &PlanGovernanceEvent) -> std::io::Result<()> {
    let Some(path) = spool_path() else {
        return Ok(());
    };
    // `spool_path()` always lives under the config dir, so it has a parent; the
    // `unwrap_or` is a total fallback that keeps this branch-free — and so fully
    // coverable — rather than guarding an unreachable `None` arm.
    std::fs::create_dir_all(path.parent().unwrap_or(&path))?;
    let mut line = serde_json::to_string(event)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');

    // Serialize this read-modify-write against a concurrent append's cap-trim
    // rename and against the uploader's post-delivery removal: without the lock,
    // two appends at the cap can race through the shared `.tmp` rename and lose
    // an event, and an append landing inside `flush`'s read/overwrite window can
    // be deleted. The lock is a hard precondition, never a fallback to an unlocked
    // write — that is exactly the race this guards. If it cannot be acquired we
    // fail open: this best-effort event is intentionally *dropped* for this run
    // (returned as an error for the caller to record) rather than persisted
    // unlocked or allowed to fail the command.
    let Some(_lock) = SpoolLock::acquire() else {
        return Err(std::io::Error::other(
            "scope telemetry spool lock unavailable; dropping event (best-effort)",
        ));
    };

    // The common path is a bare append; the cap is enforced only once the file
    // has actually grown, and it operates on raw lines so a line this build
    // cannot parse is still carried (or aged out) rather than silently lost.
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<&str> = existing.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() >= MAX_SPOOL_EVENTS {
        let drop = lines.len() + 1 - MAX_SPOOL_EVENTS;
        lines.drain(0..drop);
        let mut out = lines.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&line);
        crate::config::paths::write_secure(&path, out.as_bytes())
    } else {
        crate::config::paths::append_secure(&path, line.as_bytes())
    }
}

/// Every parseable event currently in the spool, oldest first. A missing spool
/// or a line this build cannot parse yields nothing for that line rather than an
/// error — the queue must survive a forward/backward schema skew.
pub(crate) fn load_all() -> Vec<PlanGovernanceEvent> {
    let Some(path) = spool_path() else {
        return Vec::new();
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<PlanGovernanceEvent>(l).ok())
        .collect()
}

/// The `insert_id` of one raw spool line, or `None` when this build cannot parse
/// the line. A `None` line is never treated as delivered, so [`remove_delivered`]
/// retains it rather than dropping it.
fn line_insert_id(line: &str) -> Option<String> {
    serde_json::from_str::<PlanGovernanceEvent>(line)
        .ok()
        .map(|e| e.insert_id)
}

/// Under the spool lock, drop exactly the lines whose `insert_id` the proxy
/// confirmed and keep every other line.
///
/// The remaining set is computed from the *raw* file, not from re-parsed events,
/// so two classes of line survive that a parse-then-rewrite would silently
/// delete: a line this build cannot parse (a forward/backward schema skew, which
/// then ages out through the cap in [`append`] rather than vanishing on the first
/// flush), and a line another process appended after this flush's send began (its
/// `insert_id` is not in `delivered`, so it is retained).
fn remove_delivered(delivered: &HashSet<String>) {
    let Some(path) = spool_path() else {
        return;
    };
    // Never rewrite the spool without the lock: an unlocked overwrite here could
    // delete an event a concurrent `append` just added. If the lock is
    // unavailable, retain everything and let the next flush retry — the proxy
    // deduplicates on `insert_id`, so re-sending the already-delivered events is
    // harmless.
    let Some(_lock) = SpoolLock::acquire() else {
        tracing::debug!(
            "scope telemetry: spool lock unavailable; retaining events for a later flush"
        );
        return;
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return;
    };
    let mut out = String::new();
    let mut kept = 0usize;
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line_insert_id(line).is_some_and(|id| delivered.contains(&id)) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
        kept += 1;
    }
    let _ = if kept == 0 {
        crate::config::paths::remove_secure(&path)
    } else {
        crate::config::paths::write_secure(&path, out.as_bytes())
    };
}

/// Send what the spool holds and drop the events the proxy confirms it recorded,
/// leaving the rest queued for a later attempt.
///
/// The backlog is sent in chunks no larger than [`MAX_EVENTS_PER_BATCH`]: the
/// proxy rejects an oversized batch *whole*, so without chunking a spool that
/// ever passed the cap could never drain. Each chunk is confirmed independently
/// and only the `insert_id`s of delivered chunks are removed (from the raw file,
/// under the lock — see [`remove_delivered`]), so a mid-run failure or a
/// concurrent append leaves the rest safely queued; the proxy deduplicates on
/// `insert_id`, so re-sending a delivered event is harmless.
///
/// Returns [`Delivery::Delivered`] when every chunk landed, [`Delivery::Skipped`]
/// when telemetry is opted out (nothing sent, nothing removed), and
/// [`Delivery::Failed`] when a chunk failed and events remain queued.
pub(crate) async fn flush(config: &Config, api_url: &str) -> Delivery {
    let events = load_all();
    if events.is_empty() {
        return Delivery::Skipped;
    }

    let mut delivered_ids: HashSet<String> = HashSet::new();
    let mut outcome = Delivery::Delivered;
    for chunk in events.chunks(MAX_EVENTS_PER_BATCH) {
        let ids = chunk.iter().map(|e| e.insert_id.clone());
        match deliver_events(chunk.to_vec(), config, api_url).await {
            Delivery::Delivered => delivered_ids.extend(ids),
            // A non-empty chunk is only ever Skipped when telemetry is opted out,
            // which applies to the whole run: stop and remove nothing.
            Delivery::Skipped => {
                outcome = Delivery::Skipped;
                break;
            }
            // Endpoint down or a server-side rejection: stop and keep the rest
            // queued for a later flush.
            Delivery::Failed => {
                outcome = Delivery::Failed;
                break;
            }
        }
    }

    if !delivered_ids.is_empty() {
        remove_delivered(&delivered_ids);
    }
    outcome
}

#[cfg(test)]
mod tests {
    // These async tests hold `ENV_MUTEX` (a std Mutex) across `.await` points on
    // purpose: it is an env-serialization latch held across await points by design,
    // serializing tests that mutate process-global env vars so they never race.
    #![allow(clippy::await_holding_lock)]
    use super::*;
    use crate::api::types::{PlanGovernanceEventName, PlanGovernanceEventProperties};
    use crate::config::types::{Config, TelemetryConfig};
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use tempfile::tempdir;

    /// A minimal scope-select event carrying only counts/ids — the exact shape
    /// the spool round-trips. `marker` rides in `scope_run_id` so a test can
    /// tell events apart; no field ever holds plan or rule text.
    fn sample_event(marker: &str, insert_id: &str) -> PlanGovernanceEvent {
        PlanGovernanceEvent {
            event: PlanGovernanceEventName::PlanGovernanceScopeSelect,
            distinct_id: "install-abc".to_string(),
            properties: Some(PlanGovernanceEventProperties {
                command: Some("rules select".to_string()),
                scope_run_id: Some(marker.to_string()),
                rules_scanned: Some(42),
                ..Default::default()
            }),
            timestamp: Some("2026-09-29T00:00:00+00:00".to_string()),
            insert_id: insert_id.to_string(),
        }
    }

    /// A config pointing telemetry at `api_url`, opt-out off.
    fn cfg_for(api_url: &str) -> Config {
        Config {
            api_url: Some(api_url.to_string()),
            telemetry: Some(TelemetryConfig {
                enabled: Some(true),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn test_append_then_load_round_trips_the_event() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());

        append(&sample_event("run-1", "id-1")).unwrap();
        append(&sample_event("run-2", "id-2")).unwrap();

        let loaded = load_all();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].insert_id, "id-1");
        assert_eq!(loaded[1].insert_id, "id-2");
        assert_eq!(
            loaded[1].properties.as_ref().unwrap().rules_scanned,
            Some(42)
        );
    }

    #[test]
    fn test_append_enforces_the_bounded_cap_dropping_the_oldest() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());

        for i in 0..(MAX_SPOOL_EVENTS + 5) {
            append(&sample_event(&format!("run-{i}"), &format!("id-{i}"))).unwrap();
        }

        let loaded = load_all();
        assert_eq!(loaded.len(), MAX_SPOOL_EVENTS, "spool must stay bounded");
        // The oldest ids were dropped; the newest survive, in order.
        assert_eq!(loaded.first().unwrap().insert_id, "id-5");
        assert_eq!(
            loaded.last().unwrap().insert_id,
            format!("id-{}", MAX_SPOOL_EVENTS + 4)
        );
    }

    #[test]
    fn test_spool_line_carries_no_raw_selection_text() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());

        append(&sample_event("SENTINEL-RUN", "id-1")).unwrap();
        let raw = std::fs::read_to_string(home.path().join(SPOOL_FILE)).unwrap();
        // Only counts/enums/ids are stored; the payload is the same as the wire.
        assert!(raw.contains("rules_scanned"));
        assert!(raw.contains("scope_select"));
    }

    #[tokio::test]
    async fn test_flush_delivers_then_drains_the_spool() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _no_tel = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/plan-governance/record")
            .with_status(200)
            .with_body(r#"{"recorded":2,"failed":0}"#)
            .create_async()
            .await;

        append(&sample_event("run-1", "id-1")).unwrap();
        append(&sample_event("run-2", "id-2")).unwrap();

        let cfg = cfg_for(&server.url());
        let outcome = flush(&cfg, &server.url()).await;

        assert_eq!(outcome, Delivery::Delivered);
        mock.assert_async().await;
        assert!(load_all().is_empty(), "delivered events must be removed");
    }

    #[tokio::test]
    async fn test_flush_retains_the_spool_on_failed_delivery() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _no_tel = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/plan-governance/record")
            .with_status(500)
            .with_body("boom")
            .create_async()
            .await;

        append(&sample_event("run-1", "id-1")).unwrap();

        let cfg = cfg_for(&server.url());
        let outcome = flush(&cfg, &server.url()).await;

        assert_eq!(outcome, Delivery::Failed);
        mock.assert_async().await;
        assert_eq!(load_all().len(), 1, "a failed send must keep the event");
    }

    #[tokio::test]
    async fn test_flush_opted_out_sends_nothing_and_retains() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _off = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");

        let mut server = mockito::Server::new_async().await;
        // The endpoint must never be touched when opted out.
        let mock = server
            .mock("POST", "/plan-governance/record")
            .expect(0)
            .create_async()
            .await;

        append(&sample_event("run-1", "id-1")).unwrap();

        let cfg = cfg_for(&server.url());
        let outcome = flush(&cfg, &server.url()).await;

        assert_eq!(outcome, Delivery::Skipped);
        mock.assert_async().await;
        assert_eq!(load_all().len(), 1, "opt-out must not drop queued events");
    }

    #[tokio::test]
    async fn test_flush_on_an_empty_spool_is_a_skip() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());

        let cfg = cfg_for("http://127.0.0.1:1");
        assert_eq!(flush(&cfg, "http://127.0.0.1:1").await, Delivery::Skipped);
    }

    #[test]
    fn test_remove_delivered_all_removes_the_spool_file() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());

        append(&sample_event("run-1", "id-1")).unwrap();
        append(&sample_event("run-2", "id-2")).unwrap();
        assert!(home.path().join(SPOOL_FILE).exists());

        // Every queued id confirmed -> nothing remains -> the file is removed.
        let all: HashSet<String> = ["id-1".to_string(), "id-2".to_string()]
            .into_iter()
            .collect();
        remove_delivered(&all);
        assert!(!home.path().join(SPOOL_FILE).exists());
        assert!(load_all().is_empty());
    }

    // --- Degraded / error paths: a spool step must never fail a command ---

    /// With no resolvable config dir there is nowhere durable to write, so an
    /// append is a silent `Ok` no-op and a load yields nothing — neither errors.
    #[test]
    fn test_append_and_load_without_a_config_dir_are_silent_noops() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _cfg = EnvGuard::remove("ACTUAL_CONFIG");
        // An empty (non-absolute) ACTUAL_CONFIG_DIR makes `config_dir()` fail, so
        // `spool_path()`/`lock_path()` are `None`.
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", "");

        assert!(append(&sample_event("run-1", "id-1")).is_ok());
        assert!(load_all().is_empty());
        // remove_delivered must bail out the same way rather than panicking.
        remove_delivered(&HashSet::new());
    }

    /// A config dir whose parent is a regular file cannot be created, and that
    /// I/O error surfaces from `append` (for the caller to swallow) rather than
    /// being hidden.
    #[test]
    fn test_append_errors_when_the_config_dir_cannot_be_created() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _cfg = EnvGuard::remove("ACTUAL_CONFIG");
        let home = tempdir().unwrap();
        // A regular file where a directory ancestor is expected: `create_dir_all`
        // of `<file>/sub` fails because `<file>` is not a directory.
        let blocker = home.path().join("a-file");
        std::fs::write(&blocker, b"not a dir").unwrap();
        let unbuildable = blocker.join("sub");
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", unbuildable.to_str().unwrap());

        let err = append(&sample_event("run-1", "id-1")).unwrap_err();
        let kind = err.kind();
        assert!(
            kind == std::io::ErrorKind::NotADirectory || kind == std::io::ErrorKind::AlreadyExists,
            "expected a directory-create failure, got {err:?}"
        );
    }

    /// `remove_delivered` on a config dir that exists but has no spool file yet
    /// (the read fails) is a no-op, not a panic.
    #[test]
    fn test_remove_delivered_on_a_missing_spool_is_a_noop() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());

        assert!(!home.path().join(SPOOL_FILE).exists());
        remove_delivered(&["id-1".to_string()].into_iter().collect());
        assert!(!home.path().join(SPOOL_FILE).exists());
    }

    /// `remove_delivered` skips blank lines while rewriting and keeps every
    /// undelivered entry, so a spool with interleaved blank lines is normalized
    /// without dropping real events.
    #[test]
    fn test_remove_delivered_skips_blank_lines_and_keeps_undelivered() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());

        // A real event surrounded by blank lines.
        append(&sample_event("run-1", "id-1")).unwrap();
        let path = home.path().join(SPOOL_FILE);
        let body = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("\n{body}\n   \n")).unwrap();

        // Nothing delivered: the blank lines are dropped, the event is kept.
        remove_delivered(&HashSet::new());
        let loaded = load_all();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].insert_id, "id-1");
    }

    /// APR-007: when the lock cannot be acquired, neither `append` nor
    /// `remove_delivered` may fall back to an unlocked read/modify/write. Putting
    /// a *directory* where the lock file belongs makes `open_lock_file` fail, so
    /// `acquire()` returns `None` — the same outcome an advisory-lock (flock)
    /// failure produces — and this one deterministic seam exercises the
    /// fail-closed path for both failure modes.
    #[test]
    fn test_lock_failure_never_mutates_the_spool_unlocked() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());

        // Seed one event by writing the spool directly, so no lock file exists
        // yet, then block the lock path with a directory.
        let spool = home.path().join(SPOOL_FILE);
        let seeded = serde_json::to_string(&sample_event("run-1", "id-1")).unwrap();
        std::fs::write(&spool, format!("{seeded}\n")).unwrap();
        std::fs::create_dir_all(home.path().join(LOCK_FILE)).unwrap();

        assert!(
            SpoolLock::acquire().is_none(),
            "a directory at the lock path must defeat the lock"
        );

        // append refuses rather than writing unlocked: it returns an error and
        // leaves the spool exactly as it was — the seeded event is neither
        // clobbered nor joined by the new one.
        let err = append(&sample_event("run-2", "id-2")).unwrap_err();
        assert!(
            err.to_string().contains("lock"),
            "append must report the lock failure, got {err:?}"
        );
        let after_append: Vec<String> = load_all().into_iter().map(|e| e.insert_id).collect();
        assert_eq!(
            after_append,
            vec!["id-1".to_string()],
            "a failed lock must not add or drop events"
        );

        // remove_delivered retains (never deletes) when it cannot lock, so a
        // racing append can never be lost to an unlocked overwrite.
        remove_delivered(&["id-1".to_string()].into_iter().collect());
        let after_remove: Vec<String> = load_all().into_iter().map(|e| e.insert_id).collect();
        assert_eq!(
            after_remove,
            vec!["id-1".to_string()],
            "a failed lock must retain events, never remove them from an old snapshot"
        );
    }

    // --- Finding 2: the spool must not exceed the proxy's per-batch cap ---

    #[tokio::test]
    async fn test_flush_chunks_a_backlog_past_the_proxy_cap() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _no_tel = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        // A mock modeling the proxy contract: it rejects any batch larger than
        // MAX_EVENTS_PER_BATCH whole (every event reported failed), and accepts
        // any batch within the cap. A single unchunked send of the backlog below
        // would be rejected and the spool would never drain.
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/plan-governance/record")
            .with_status(200)
            .with_body_from_request(|req| {
                let body: serde_json::Value =
                    serde_json::from_slice(req.body().unwrap().as_slice()).unwrap();
                let n = body["events"].as_array().map(|a| a.len()).unwrap_or(0);
                // A batch over the cap is rejected whole (every event failed);
                // one within the cap is accepted. A correctly chunked flush only
                // ever sends the latter. Kept on one expression so neither arm is
                // a separately-uncovered line.
                let failed = if n > MAX_EVENTS_PER_BATCH { n } else { 0 };
                format!(r#"{{"recorded":{},"failed":{failed}}}"#, n - failed).into_bytes()
            })
            // 101 events must be split into exactly two batches (100 + 1).
            .expect(2)
            .create_async()
            .await;

        let total = MAX_EVENTS_PER_BATCH + 1;
        for i in 0..total {
            append(&sample_event(&format!("run-{i}"), &format!("id-{i}"))).unwrap();
        }

        let cfg = cfg_for(&server.url());
        let outcome = flush(&cfg, &server.url()).await;

        assert_eq!(
            outcome,
            Delivery::Delivered,
            "a chunked flush must fully deliver a backlog past the cap"
        );
        mock.assert_async().await;
        assert!(
            load_all().is_empty(),
            "every chunk delivered, so the spool must fully drain"
        );
    }

    /// A partial failure mid-backlog: the cap's worth of events is chunked into
    /// 100 + 100 + 56, the endpoint accepts the first two batches and rejects the
    /// third. The confirmed 200 must drain while the unsent 56 stay queued (in
    /// order, by insert_id) for a safe retry, and the outcome is `Failed` — an
    /// HTTP/schema rejection is never treated as delivery, and a partial response
    /// never clears the whole spool.
    #[tokio::test]
    async fn test_flush_retains_unsent_chunks_after_a_partial_failure() {
        use std::sync::{Arc, Mutex};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _no_tel = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        // Record the size of every batch the endpoint is asked to record, and
        // fail the third batch server-side (recorded=0, failed=n) while accepting
        // the first two. `flush` stops on the first failure, so exactly three
        // requests are made.
        let batch_sizes = Arc::new(Mutex::new(Vec::<usize>::new()));
        let sink = batch_sizes.clone();
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/plan-governance/record")
            .with_status(200)
            .with_body_from_request(move |req| {
                let body: serde_json::Value =
                    serde_json::from_slice(req.body().unwrap().as_slice()).unwrap();
                let n = body["events"].as_array().map(|a| a.len()).unwrap_or(0);
                let mut seen = sink.lock().unwrap();
                seen.push(n);
                // The third batch (index 2) is rejected whole; earlier ones land.
                let failed = if seen.len() >= 3 { n } else { 0 };
                format!(r#"{{"recorded":{},"failed":{failed}}}"#, n - failed).into_bytes()
            })
            .expect(3)
            .create_async()
            .await;

        // The full cap: 256 events -> 100 + 100 + 56.
        let total = MAX_SPOOL_EVENTS;
        for i in 0..total {
            append(&sample_event(&format!("run-{i}"), &format!("id-{i:03}"))).unwrap();
        }

        let cfg = cfg_for(&server.url());
        let outcome = flush(&cfg, &server.url()).await;

        assert_eq!(
            outcome,
            Delivery::Failed,
            "a rejected chunk must surface as Failed, never as delivered"
        );
        mock.assert_async().await;

        // Exactly three requests, each within the server contract, summing to the
        // whole backlog.
        let sizes = batch_sizes.lock().unwrap().clone();
        assert_eq!(sizes, vec![100, 100, 56]);
        assert!(sizes.iter().all(|&n| n <= MAX_EVENTS_PER_BATCH));

        // The two confirmed batches drained; the rejected 56 stay queued, in
        // order, for a safe retry — the partial response never cleared them.
        let remaining: Vec<String> = load_all().into_iter().map(|e| e.insert_id).collect();
        let expected: Vec<String> = (200..256).map(|i| format!("id-{i:03}")).collect();
        assert_eq!(remaining, expected, "only the unsent chunk must remain");
    }

    // --- Finding 4: a line this build cannot parse must survive a flush ---

    #[tokio::test]
    async fn test_flush_preserves_an_unparseable_line_across_delivery() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _no_tel = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/plan-governance/record")
            .with_status(200)
            .with_body(r#"{"recorded":1,"failed":0}"#)
            .create_async()
            .await;

        // One deliverable event plus a raw line from a hypothetical newer schema
        // this build cannot parse.
        append(&sample_event("run-1", "id-1")).unwrap();
        let spool = home.path().join(SPOOL_FILE);
        let mut raw = std::fs::read_to_string(&spool).unwrap();
        raw.push_str("{\"schema\":2,\"unknown_future_field\":true}\n");
        std::fs::write(&spool, raw).unwrap();

        let cfg = cfg_for(&server.url());
        let outcome = flush(&cfg, &server.url()).await;

        assert_eq!(outcome, Delivery::Delivered);
        mock.assert_async().await;
        let after = std::fs::read_to_string(&spool).unwrap();
        assert!(
            !after.contains("id-1"),
            "the delivered event must be removed"
        );
        assert!(
            after.contains("unknown_future_field"),
            "an unparseable line must survive the flush, not be silently deleted"
        );
    }

    // --- Finding 3: concurrent producers/uploader must not lose events ---

    #[test]
    fn test_concurrent_appends_over_cap_stay_bounded_and_intact() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());

        // More distinct events than the cap, appended from many threads at once.
        // The lock must serialize each read-modify-write so no append clobbers
        // another's cap-trim rename: the file stays exactly bounded, every
        // retained line is a whole parseable event, and no id is duplicated.
        let total = MAX_SPOOL_EVENTS + 20;
        let handles: Vec<_> = (0..total)
            .map(|i| {
                std::thread::spawn(move || {
                    let _ = append(&sample_event(&format!("run-{i}"), &format!("id-{i}")));
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let raw = std::fs::read_to_string(home.path().join(SPOOL_FILE)).unwrap();
        let ids: HashSet<String> = raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str::<PlanGovernanceEvent>(l)
                    .expect("every retained line must be a whole, parseable event")
                    .insert_id
            })
            .collect();
        assert_eq!(
            ids.len(),
            MAX_SPOOL_EVENTS,
            "the spool must stay exactly bounded with no torn or duplicated line"
        );
    }

    #[tokio::test]
    async fn test_concurrent_append_during_flush_never_loses_an_event() {
        use std::sync::{Arc, Mutex};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _no_tel = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        // The mock records every insert_id it is asked to record, so the test can
        // assert conservation: delivered ∪ still-queued must cover every event.
        let received = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = received.clone();
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/plan-governance/record")
            .with_status(200)
            .with_body_from_request(move |req| {
                let body: serde_json::Value =
                    serde_json::from_slice(req.body().unwrap().as_slice()).unwrap();
                let events = body["events"].as_array().cloned().unwrap_or_default();
                let mut got = sink.lock().unwrap();
                for e in &events {
                    if let Some(id) = e["insert_id"].as_str() {
                        got.push(id.to_string());
                    }
                }
                format!(r#"{{"recorded":{},"failed":0}}"#, events.len()).into_bytes()
            })
            .create_async()
            .await;

        // Seed a batch the flush can pick up, then append more concurrently while
        // the flush runs. Totals stay well under the cap so this exercises the
        // flush read/overwrite window, not cap-trimming.
        let seeded: Vec<String> = (0..40).map(|i| format!("seed-{i}")).collect();
        for id in &seeded {
            append(&sample_event("seed", id)).unwrap();
        }
        let live: Vec<String> = (0..40).map(|i| format!("live-{i}")).collect();

        // Each spawned thread needs its own owned id (`thread::spawn` is `'static`),
        // and `live` is reused in the assertion below, so the clone is required —
        // clippy's redundant_iter_cloned suggestion would not compile here.
        #[allow(clippy::redundant_iter_cloned)]
        let handles: Vec<_> = live
            .iter()
            .cloned()
            .map(|id| {
                std::thread::spawn(move || {
                    let _ = append(&sample_event("live", &id));
                })
            })
            .collect();

        let cfg = cfg_for(&server.url());
        let _ = flush(&cfg, &server.url()).await;
        for h in handles {
            h.join().unwrap();
        }

        let remaining: HashSet<String> = load_all().into_iter().map(|e| e.insert_id).collect();
        let delivered: HashSet<String> = received.lock().unwrap().iter().cloned().collect();
        for id in seeded.iter().chain(live.iter()) {
            assert!(
                delivered.contains(id) || remaining.contains(id),
                "event {id} was neither delivered nor queued — lost in a flush/append race"
            );
        }
    }

    /// The full race the spool lock exists for: several independent producers
    /// appending at once while several independent uploaders flush in overlapping
    /// passes. Every producer and every uploader takes the lock through its own
    /// freshly-opened file descriptor, so they contend under `flock` exactly as
    /// separate OS processes do (the lock is keyed on the open file description,
    /// not the process) — which is the real deployment, where each `rules select`
    /// spawns its own uploader.
    ///
    /// The invariant asserted is conservation: every accepted insert id is either
    /// delivered or still queued, never lost in the window between a flush's
    /// snapshot and its overwrite, and never clobbered by a racing append's
    /// rewrite. Totals stay under the cap so no event is dropped by intentional
    /// trimming (that path is covered separately by the over-cap test), making the
    /// invariant exact.
    #[test]
    fn test_concurrent_producers_and_overlapping_flushers_lose_no_event() {
        use std::sync::{Arc, Mutex};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _no_tel = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        // A permissive mock that records every delivered insert id; overlapping
        // uploaders may hit it concurrently, so the set is mutex-guarded.
        let delivered = Arc::new(Mutex::new(HashSet::<String>::new()));
        let sink = delivered.clone();
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("POST", "/plan-governance/record")
            .with_status(200)
            .with_body_from_request(move |req| {
                let body: serde_json::Value =
                    serde_json::from_slice(req.body().unwrap().as_slice()).unwrap();
                let events = body["events"].as_array().cloned().unwrap_or_default();
                let mut got = sink.lock().unwrap();
                for e in &events {
                    if let Some(id) = e["insert_id"].as_str() {
                        got.insert(id.to_string());
                    }
                }
                format!(r#"{{"recorded":{},"failed":0}}"#, events.len()).into_bytes()
            })
            .create();
        let url = server.url();

        // 6 producers × 20 events = 120 distinct ids, comfortably under the cap.
        let producers = 6;
        let per_producer = 20;
        let mut producer_handles = Vec::new();
        for p in 0..producers {
            producer_handles.push(std::thread::spawn(move || {
                for i in 0..per_producer {
                    let id = format!("p{p}-e{i}");
                    // Best-effort, exactly like the real command path.
                    let _ = append(&sample_event("prod", &id));
                }
            }));
        }

        // 4 uploaders, each draining in several overlapping passes on its own
        // current-thread runtime — the detached-uploader shape, many at once.
        let mut flusher_handles = Vec::new();
        for _ in 0..4 {
            let url = url.clone();
            flusher_handles.push(std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let cfg = cfg_for(&url);
                for _ in 0..3 {
                    let _ = rt.block_on(flush(&cfg, &url));
                }
            }));
        }

        for h in producer_handles {
            h.join().unwrap();
        }
        for h in flusher_handles {
            h.join().unwrap();
        }
        // A final drain of anything appended after the last uploader pass, so the
        // tail is delivered rather than merely queued (either satisfies the
        // invariant; this also exercises delivery of the last events).
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _ = rt.block_on(flush(&cfg_for(&url), &url));

        let queued: Vec<String> = load_all().into_iter().map(|e| e.insert_id).collect();
        let queued_set: HashSet<&String> = queued.iter().collect();
        // No torn or duplicated line survived the racing rewrites.
        assert_eq!(
            queued.len(),
            queued_set.len(),
            "the spool must hold no duplicated event after concurrent rewrites"
        );
        assert!(
            queued.len() <= MAX_SPOOL_EVENTS,
            "the spool must stay bounded"
        );

        let got = delivered.lock().unwrap();
        for p in 0..producers {
            for i in 0..per_producer {
                let id = format!("p{p}-e{i}");
                assert!(
                    got.contains(&id) || queued_set.contains(&id),
                    "event {id} was neither delivered nor queued — lost to the append/flush race"
                );
            }
        }
    }
}
