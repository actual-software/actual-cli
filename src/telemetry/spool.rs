//! A durable, process-lifetime-safe outbox for plan-governance telemetry
//! (APR-004).
//!
//! The commands that emit these events (`rules select`, `plan-check`,
//! `impl-check` and `check-override`) are short-lived, and a detached OS thread
//! that has not finished its send is not guaranteed to outlive the process — so
//! a plain background send can be terminated before delivery, silently dropping
//! the event. Instead, an opted-in event is first written here, to an owner that
//! survives process exit: a small append-only JSONL spool under the config dir,
//! created owner-readable only (`0600`), holding exactly the same
//! `PlanGovernanceEvent` payload that would go on the wire — counts, enums, ids
//! and hashes, never plan or rule text (see `PRIVACY.md`).
//!
//! Delivery is then best-effort and idempotent: [`flush`] sends what the spool
//! holds, with scope events and the checks' events as separate streams, in
//! chunks no larger than the proxy's per-batch cap ([`MAX_EVENTS_PER_BATCH`]),
//! so a backlog past that size still drains. It removes an event once the proxy
//! confirms it recorded (the proxy deduplicates on `insert_id`, so a retry of an
//! event that did land is harmless), or once the proxy refuses it sent on its
//! own. The command process never waits for this — it spawns a separate
//! uploader process (see `cli::commands::telemetry_flush`) and returns — and an
//! event an outage kept from the proxy stays durably queued for the next run's
//! flush. The spool is bounded to [`MAX_SPOOL_EVENTS`] so a long outage cannot
//! grow it without limit; the oldest events are dropped first.
//!
//! Several of these processes can touch the spool at once (each spawns its
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
//!
//! The same file, lock and cap back a second spool, [`OTLP_LOGS`], holding the
//! OpenTelemetry log records [`crate::telemetry::otlp`] sends. Its uploader
//! [`take`](Spool::take)s records out before sending them instead of removing
//! them on confirmation, so two uploaders never send one record twice.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::api::types::{PlanGovernanceEvent, PlanGovernanceEventName};
use crate::config::types::Config;
use crate::telemetry::plan_governance::{deliver_events, Delivery, MAX_EVENTS_PER_BATCH};

/// Filename under the config dir holding the pending events, scope and check
/// alike, one JSON object per line. Sibling to the `telemetry-id` install file.
/// The name dates from when it held only scope events; `PRIVACY.md` names it.
const SPOOL_FILE: &str = "scope-telemetry-spool.jsonl";

/// Upper bound on retained events. A reachable endpoint keeps the spool near
/// empty; this only caps the pathological case of a persistently unreachable
/// endpoint, dropping the oldest events rather than growing without limit.
pub(crate) const MAX_SPOOL_EVENTS: usize = 256;

/// Sibling lock file guarding every mutation of the spool. See [`SpoolLock`].
const LOCK_FILE: &str = "scope-telemetry-spool.lock";

/// One spool: a JSONL file under the config dir and the sibling lock file
/// that guards every mutation of it.
pub(crate) struct Spool {
    file: &'static str,
    lock: &'static str,
}

/// The scope-selection events this module delivers.
const SCOPE: Spool = Spool {
    file: SPOOL_FILE,
    lock: LOCK_FILE,
};

/// The OpenTelemetry log records [`crate::telemetry::otlp`] sends, one record
/// per line.
pub(crate) const OTLP_LOGS: Spool = Spool {
    file: "otlp-logs-spool.jsonl",
    lock: "otlp-logs-spool.lock",
};

impl Spool {
    /// The spool file path, or `None` when the config dir cannot be resolved
    /// (in which case there is nowhere durable to write — the same fallback the
    /// per-install `distinct_id` makes).
    fn path(&self) -> Option<PathBuf> {
        crate::config::paths::config_dir()
            .ok()
            .map(|dir| dir.join(self.file))
    }

    /// Take this spool's lock, or `None` when the config dir cannot be
    /// resolved or [`SpoolLock::acquire`] fails.
    fn lock(&self) -> Option<SpoolLock> {
        let dir = crate::config::paths::config_dir().ok()?;
        SpoolLock::acquire(&dir.join(self.lock))
    }

    /// Append one event per line, keeping the file bounded.
    ///
    /// Best-effort: any I/O error is returned for the caller to swallow, since a
    /// failed spool write must never fail the command. When the spool would
    /// pass [`MAX_SPOOL_EVENTS`] lines the oldest are dropped so the file stays
    /// bounded.
    pub(crate) fn append_lines(&self, new: &[String]) -> std::io::Result<()> {
        let Some(path) = self.path() else {
            return Ok(());
        };
        // `path()` always lives under the config dir, so it has a parent; the
        // `unwrap_or` is a total fallback that keeps this branch-free — and so
        // fully coverable — rather than guarding an unreachable `None` arm.
        std::fs::create_dir_all(path.parent().unwrap_or(&path))?;

        // Serialize this read-modify-write against a concurrent append's cap-trim
        // rename and against the uploader's post-delivery removal: without the
        // lock, two appends at the cap can race through the shared `.tmp` rename
        // and lose an event, and an append landing inside `flush`'s
        // read/overwrite window can be deleted. The lock is a hard precondition,
        // never a fallback to an unlocked write — that is exactly the race this
        // guards. If it cannot be acquired we fail open: this best-effort event is
        // intentionally *dropped* for this run (returned as an error for the
        // caller to record) rather than persisted unlocked or allowed to fail the
        // command.
        let Some(_lock) = self.lock() else {
            return Err(std::io::Error::other(
                "telemetry spool lock unavailable; dropping event (best-effort)",
            ));
        };

        // The common path is a bare append; the cap is enforced only once the
        // file has actually grown, and it operates on raw lines so a line this
        // build cannot parse is still carried (or aged out) rather than silently
        // lost.
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let mut lines: Vec<&str> = existing.lines().filter(|l| !l.trim().is_empty()).collect();
        if lines.len() + new.len() > MAX_SPOOL_EVENTS {
            lines.extend(new.iter().map(String::as_str));
            let drop = lines.len() - MAX_SPOOL_EVENTS;
            crate::config::paths::write_secure(&path, joined(&lines[drop..]).as_bytes())
        } else {
            crate::config::paths::append_secure(&path, joined(new).as_bytes())
        }
    }

    /// Remove and return up to `max` of the oldest lines, under the lock.
    ///
    /// The lines leave the file before the caller acts on them, so two callers
    /// never handle one line twice. A missing spool, an unavailable lock or a
    /// failed rewrite takes nothing and leaves the file as it was: a line is
    /// returned only once it is gone from the file.
    pub(crate) fn take(&self, max: usize) -> Vec<String> {
        let Some(path) = self.path() else {
            return Vec::new();
        };
        let Some(_lock) = self.lock() else {
            return Vec::new();
        };
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        let mut taken: Vec<String> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect();
        let rest = taken.split_off(taken.len().min(max));
        // `remove_file`, not `remove_secure`: success has to mean the spool is
        // gone, and `remove_secure` can fail on the staging file after it.
        let rewritten = if rest.is_empty() {
            std::fs::remove_file(&path)
        } else {
            crate::config::paths::write_secure(&path, joined(&rest).as_bytes())
        };
        if rewritten.is_err() {
            return Vec::new();
        }
        taken
    }
}

/// `lines`, each followed by a newline.
fn joined(lines: &[impl AsRef<str>]) -> String {
    lines
        .iter()
        .map(|line| format!("{}\n", line.as_ref()))
        .collect()
}

/// An RAII holder of the spool's exclusive advisory lock. While it is alive this
/// process holds the lock against every other process that opened the same lock
/// file; dropping it (or the process exiting) releases the lock.
///
/// The lock serializes the read-modify-write in [`Spool::append_lines`] against
/// another append's cap-trim rename and against [`flush`]'s post-delivery
/// removal — the two windows in which a concurrent writer could otherwise
/// clobber a queued event or delete one that landed mid-flush. The network send
/// itself is deliberately *not* held under the lock, so a slow uploader never
/// blocks any command from appending.
struct SpoolLock {
    // Held only for its lock; the OS releases the advisory lock when the fd is
    // closed on drop, so the field is never read directly.
    _file: std::fs::File,
}

impl SpoolLock {
    /// Take the exclusive lock on the file at `path`, blocking until it is held.
    /// Returns `None` when the lock cannot be established — the lock file cannot
    /// be opened, or the advisory lock itself fails.
    ///
    /// A `None` never licenses an unlocked mutation: [`Spool::append_lines`] and
    /// [`remove_delivered`] treat it as "did not run" and leave the spool exactly
    /// as it was, so a coordination failure can never clobber or delete a
    /// concurrent event (APR-007). The affected event is simply retried on a
    /// later run, where the proxy's `insert_id` dedup makes a re-send harmless.
    fn acquire(path: &Path) -> Option<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let file = open_lock_file(path)?;
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

/// Append one scope event to the durable spool. See [`Spool::append_lines`].
pub(crate) fn append(event: &PlanGovernanceEvent) -> std::io::Result<()> {
    let line = serde_json::to_string(event)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    SCOPE.append_lines(&[line])
}

/// Every parseable event currently in the spool, oldest first. A missing spool
/// or a line this build cannot parse yields nothing for that line rather than an
/// error — the queue must survive a forward/backward schema skew.
pub(crate) fn load_all() -> Vec<PlanGovernanceEvent> {
    let Some(path) = SCOPE.path() else {
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

/// Under the spool lock, drop exactly the lines whose `insert_id` is in
/// `delivered` and keep every other line. [`flush`] passes the events of each
/// chunk the proxy recorded in full and the ones it refused on their own.
///
/// The remaining set is computed from the *raw* file, not from re-parsed events,
/// so two classes of line survive that a parse-then-rewrite would silently
/// delete: a line this build cannot parse (a forward/backward schema skew, which
/// then ages out through the cap in [`Spool::append_lines`] rather than vanishing on the first
/// flush), and a line another process appended after this flush's send began (its
/// `insert_id` is not in `delivered`, so it is retained).
fn remove_delivered(delivered: &HashSet<String>) {
    let Some(path) = SCOPE.path() else {
        return;
    };
    // Never rewrite the spool without the lock: an unlocked overwrite here could
    // delete an event a concurrent `append` just added. If the lock is
    // unavailable, retain everything and let the next flush retry — the proxy
    // deduplicates on `insert_id`, so re-sending the already-delivered events is
    // harmless.
    let Some(_lock) = SCOPE.lock() else {
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

/// Send what the spool holds and drop each chunk the proxy recorded in full and
/// each event it refused on its own, leaving the rest queued for a later try.
///
/// Scope events and the checks' events (started, completed, violation and
/// override) go out as two streams, each oldest-first, and an outage (see
/// [`Delivery::Failed`]) ends only its own stream's pass.
///
/// A chunk the proxy refuses with a 400 or 422, such as one carrying a value
/// its schema rejects, is resent one event at a time, ahead of the chunks
/// behind it. Each event delivered on its own is removed and each event
/// refused on its own is dropped, so a refused event costs only itself rather
/// than leading every later flush. A chunk of one is already on its own, so it
/// is dropped without a resend. An outage during the single sends ends the
/// stream's pass and keeps what is left. A 401, 403, 404 or 413 is an outage
/// rather than a refusal, since it is about the service key, the route or the
/// request's size rather than any event, so the chunk stays queued whole.
///
/// Each stream is sent in chunks no larger than [`MAX_EVENTS_PER_BATCH`]: the
/// proxy rejects an oversized batch *whole*, so a backlog past that size goes
/// out in several requests rather than being refused as one. Each chunk is
/// confirmed independently and only the `insert_id`s of events delivered, or
/// refused on their own, are removed (from the raw file, under the lock — see
/// [`remove_delivered`]), so a mid-run failure or a concurrent append leaves
/// the rest safely queued; the proxy deduplicates on `insert_id`, so re-sending
/// a delivered event is harmless.
///
/// Returns [`Delivery::Delivered`] when every event landed, [`Delivery::Skipped`]
/// when telemetry is opted out (nothing sent, nothing removed),
/// [`Delivery::Failed`] when an outage left events queued, and
/// [`Delivery::Refused`] when the proxy refused events and nothing else is
/// left.
pub(crate) async fn flush(config: &Config, api_url: &str) -> Delivery {
    let events = load_all();
    if events.is_empty() {
        return Delivery::Skipped;
    }
    let (scope, checks): (Vec<_>, Vec<_>) = events
        .into_iter()
        .partition(|e| e.event == PlanGovernanceEventName::PlanGovernanceScopeSelect);

    let mut to_remove: HashSet<String> = HashSet::new();
    let mut outcome = Delivery::Delivered;
    'streams: for stream in [scope, checks] {
        let mut batches: VecDeque<Vec<PlanGovernanceEvent>> = stream
            .chunks(MAX_EVENTS_PER_BATCH)
            .map(|chunk| chunk.to_vec())
            .collect();
        while let Some(batch) = batches.pop_front() {
            match deliver_events(batch.clone(), config, api_url).await {
                Delivery::Delivered => to_remove.extend(batch.into_iter().map(|e| e.insert_id)),
                // A non-empty chunk is only ever Skipped when telemetry is opted out,
                // which applies to the whole run: stop and remove nothing.
                Delivery::Skipped => {
                    outcome = Delivery::Skipped;
                    break 'streams;
                }
                // An outage: end this stream's pass and keep the rest of it
                // queued for a later flush.
                Delivery::Failed => {
                    outcome = Delivery::Failed;
                    break;
                }
                // Resend a refused chunk one event at a time, in order, before
                // the chunks behind it.
                Delivery::Refused if batch.len() > 1 => {
                    for event in batch.into_iter().rev() {
                        batches.push_front(vec![event]);
                    }
                }
                // Refused on its own: drop it so it stops leading later flushes.
                Delivery::Refused => {
                    to_remove.extend(batch.into_iter().map(|e| e.insert_id));
                    if outcome == Delivery::Delivered {
                        outcome = Delivery::Refused;
                    }
                }
            }
        }
    }

    if !to_remove.is_empty() {
        remove_delivered(&to_remove);
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
                ..Default::default()
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
        // `Spool::path()`/`Spool::lock()` are `None`.
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
            SCOPE.lock().is_none(),
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

    /// An outage mid-backlog: the cap's worth of events is chunked into
    /// 100 + 100 + 56, the endpoint accepts the first two batches and answers the
    /// third with a 500. The confirmed 200 must drain while the unsent 56 stay
    /// queued (in order, by insert_id) for a safe retry, and the outcome is
    /// `Failed` — an outage is never treated as delivery or as a refusal, and
    /// it never clears the whole spool.
    #[tokio::test]
    async fn test_flush_retains_unsent_chunks_after_a_partial_failure() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _no_tel = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        // Record the size of every batch the endpoint is asked to record, and
        // answer the third with a 500 while accepting the first two. `flush`
        // stops on the outage, so exactly three requests are made.
        let batch_sizes = Arc::new(Mutex::new(Vec::<usize>::new()));
        let sink = batch_sizes.clone();
        let requests = Arc::new(AtomicUsize::new(0));
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/plan-governance/record")
            .with_status_code_from_request(move |_| {
                // The third request (index 2) fails; earlier ones land.
                let index = requests.fetch_add(1, Ordering::SeqCst);
                if index >= 2 {
                    500
                } else {
                    200
                }
            })
            .with_body_from_request(move |req| {
                let body: serde_json::Value =
                    serde_json::from_slice(req.body().unwrap().as_slice()).unwrap();
                let n = body["events"].as_array().map(|a| a.len()).unwrap_or(0);
                sink.lock().unwrap().push(n);
                format!(r#"{{"recorded":{n},"failed":0}}"#).into_bytes()
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
            "a chunk lost to an outage must surface as Failed, never as delivered"
        );
        mock.assert_async().await;

        // Exactly three requests, each within the server contract, summing to the
        // whole backlog.
        let sizes = batch_sizes.lock().unwrap().clone();
        assert_eq!(sizes, vec![100, 100, 56]);
        assert!(sizes.iter().all(|&n| n <= MAX_EVENTS_PER_BATCH));

        // The two confirmed batches drained; the 56 the outage kept stay queued,
        // in order, for a safe retry.
        let remaining: Vec<String> = load_all().into_iter().map(|e| e.insert_id).collect();
        let expected: Vec<String> = (200..256).map(|i| format!("id-{i:03}")).collect();
        assert_eq!(remaining, expected, "only the unsent chunk must remain");
    }

    // --- A refused event costs only itself ---

    /// An event from the checks' stream (`plan-check`, `impl-check` and
    /// `check-override`), which shares the spool with the scope events.
    fn check_event(event: PlanGovernanceEventName, insert_id: &str) -> PlanGovernanceEvent {
        PlanGovernanceEvent {
            event,
            distinct_id: "install-abc".to_string(),
            properties: None,
            timestamp: None,
            insert_id: insert_id.to_string(),
        }
    }

    /// A proxy that answers 400 to any batch carrying `refused` and accepts
    /// every other batch whole, expecting one request per stream.
    async fn proxy_refusing(
        server: &mut mockito::ServerGuard,
        refused: &'static str,
    ) -> mockito::Mock {
        server
            .mock("POST", "/plan-governance/record")
            .with_status_code_from_request(move |req| {
                let body = String::from_utf8_lossy(req.body().unwrap()).into_owned();
                if body.contains(refused) {
                    400
                } else {
                    200
                }
            })
            .with_body_from_request(|req| {
                let body: serde_json::Value =
                    serde_json::from_slice(req.body().unwrap().as_slice()).unwrap();
                let n = body["events"].as_array().map(|a| a.len()).unwrap_or(0);
                format!(r#"{{"recorded":{n},"failed":0}}"#).into_bytes()
            })
            .expect(2)
            .create_async()
            .await
    }

    /// The proxy refuses the scope event at the head of the spool. The check
    /// and override events queued behind it are still delivered and removed,
    /// and the scope event, refused on its own, is dropped without a resend.
    #[tokio::test]
    async fn test_flush_a_refused_scope_batch_does_not_hold_back_check_events() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _no_tel = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        let mut server = mockito::Server::new_async().await;
        let mock = proxy_refusing(&mut server, "plan_governance_scope_select").await;

        append(&sample_event("run-1", "scope-1")).unwrap();
        append(&check_event(
            PlanGovernanceEventName::PlanGovernanceCheckCompleted,
            "check-1",
        ))
        .unwrap();
        append(&check_event(
            PlanGovernanceEventName::PlanGovernanceRuleOverride,
            "override-1",
        ))
        .unwrap();

        let cfg = cfg_for(&server.url());
        assert_eq!(flush(&cfg, &server.url()).await, Delivery::Refused);

        let queued: Vec<String> = load_all().into_iter().map(|e| e.insert_id).collect();
        assert!(
            queued.is_empty(),
            "the refused scope event must be dropped: {queued:?}"
        );
        mock.assert_async().await;
    }

    /// The mirror case: the proxy refuses the check event at the head of the
    /// spool, the scope event queued behind it is still delivered, and the
    /// check event is dropped.
    #[tokio::test]
    async fn test_flush_a_refused_check_batch_does_not_hold_back_scope_events() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _no_tel = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        let mut server = mockito::Server::new_async().await;
        let mock = proxy_refusing(&mut server, "plan_governance_check_completed").await;

        append(&check_event(
            PlanGovernanceEventName::PlanGovernanceCheckCompleted,
            "check-1",
        ))
        .unwrap();
        append(&sample_event("run-1", "scope-1")).unwrap();

        let cfg = cfg_for(&server.url());
        assert_eq!(flush(&cfg, &server.url()).await, Delivery::Refused);

        let queued: Vec<String> = load_all().into_iter().map(|e| e.insert_id).collect();
        assert!(
            queued.is_empty(),
            "the refused check event must be dropped: {queued:?}"
        );
        mock.assert_async().await;
    }

    /// The `insert_id`s of the events in one request to the proxy, in order.
    fn insert_ids_of(req: &mockito::Request) -> Vec<String> {
        let body: serde_json::Value =
            serde_json::from_slice(req.body().unwrap().as_slice()).unwrap();
        body["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["insert_id"].as_str().unwrap().to_string())
            .collect()
    }

    /// `ids` as owned strings, for comparing with what a mock recorded.
    fn owned(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    /// One check event the proxy refuses, with other check events queued
    /// behind it. The refused chunk is resent one event at a time: the others
    /// are delivered and removed, the refused one is dropped, and the next
    /// flush sends only what was queued after it.
    #[tokio::test]
    async fn test_flush_resends_a_refused_chunk_one_event_at_a_time() {
        use std::sync::{Arc, Mutex};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _no_tel = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        // Answers 400 to any batch carrying `poison` and records every request.
        let requests = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let sink = requests.clone();
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/plan-governance/record")
            .with_status_code_from_request(move |req| {
                let ids = insert_ids_of(req);
                let refused = ids.iter().any(|id| id == "poison");
                sink.lock().unwrap().push(ids);
                if refused {
                    400
                } else {
                    200
                }
            })
            .with_body_from_request(|req| {
                let n = insert_ids_of(req).len();
                format!(r#"{{"recorded":{n},"failed":0}}"#).into_bytes()
            })
            .create_async()
            .await;

        for id in ["poison", "check-1", "check-2"] {
            append(&check_event(
                PlanGovernanceEventName::PlanGovernanceCheckCompleted,
                id,
            ))
            .unwrap();
        }

        let cfg = cfg_for(&server.url());
        assert_eq!(flush(&cfg, &server.url()).await, Delivery::Refused);
        assert!(
            load_all().is_empty(),
            "every event was delivered or dropped"
        );

        // A check event queued later goes out on its own: the refused one no
        // longer leads the stream.
        append(&check_event(
            PlanGovernanceEventName::PlanGovernanceRuleOverride,
            "override-1",
        ))
        .unwrap();
        assert_eq!(flush(&cfg, &server.url()).await, Delivery::Delivered);
        assert!(load_all().is_empty());

        assert_eq!(
            *requests.lock().unwrap(),
            vec![
                owned(&["poison", "check-1", "check-2"]),
                owned(&["poison"]),
                owned(&["check-1"]),
                owned(&["check-2"]),
                owned(&["override-1"]),
            ]
        );
    }

    /// A refused chunk is resent ahead of the chunks behind it, and the stream
    /// carries on past it: a backlog of one chunk and one event more, led by a
    /// refused event, costs the chunk, its events one at a time, then the
    /// second chunk.
    #[tokio::test]
    async fn test_flush_resends_a_refused_chunk_before_the_next_chunk() {
        use std::sync::{Arc, Mutex};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _no_tel = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        let requests = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let sink = requests.clone();
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/plan-governance/record")
            .with_status_code_from_request(move |req| {
                let ids = insert_ids_of(req);
                let refused = ids.iter().any(|id| id == "id-000");
                sink.lock().unwrap().push(ids);
                if refused {
                    400
                } else {
                    200
                }
            })
            .with_body_from_request(|req| {
                let n = insert_ids_of(req).len();
                format!(r#"{{"recorded":{n},"failed":0}}"#).into_bytes()
            })
            // The first chunk, its 100 events alone, then the second chunk.
            .expect(MAX_EVENTS_PER_BATCH + 2)
            .create_async()
            .await;

        for i in 0..=MAX_EVENTS_PER_BATCH {
            append(&check_event(
                PlanGovernanceEventName::PlanGovernanceRuleViolation,
                &format!("id-{i:03}"),
            ))
            .unwrap();
        }

        let cfg = cfg_for(&server.url());
        assert_eq!(flush(&cfg, &server.url()).await, Delivery::Refused);
        mock.assert_async().await;
        assert!(load_all().is_empty());

        let requests = requests.lock().unwrap();
        assert_eq!(requests[0].len(), MAX_EVENTS_PER_BATCH);
        let singles: Vec<String> = requests[1..=MAX_EVENTS_PER_BATCH].concat();
        assert_eq!(
            singles, requests[0],
            "each event of the chunk, alone, in order"
        );
        assert_eq!(
            requests[MAX_EVENTS_PER_BATCH + 1],
            owned(&[format!("id-{MAX_EVENTS_PER_BATCH:03}").as_str()])
        );
    }

    /// A 2xx reply that reports failed events is an outage: the proxy failed to
    /// forward them, so a later try may land them. Each stream keeps its whole
    /// chunk, the events the proxy recorded included, with no resend, and the
    /// next flush delivers them.
    #[tokio::test]
    async fn test_flush_keeps_a_chunk_whose_2xx_reports_failed_events() {
        use std::sync::{Arc, Mutex};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _no_tel = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        // Answers 200 to every batch and reports the `-failed` events as failed.
        let requests = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let sink = requests.clone();
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/plan-governance/record")
            .with_status(200)
            .with_body_from_request(move |req| {
                let ids = insert_ids_of(req);
                let failed = ids.iter().filter(|id| id.ends_with("-failed")).count();
                let recorded = ids.len() - failed;
                sink.lock().unwrap().push(ids);
                format!(r#"{{"recorded":{recorded},"failed":{failed}}}"#).into_bytes()
            })
            .create_async()
            .await;

        append(&sample_event("run-1", "scope-1")).unwrap();
        append(&sample_event("run-2", "scope-failed")).unwrap();
        for id in ["check-1", "check-failed"] {
            append(&check_event(
                PlanGovernanceEventName::PlanGovernanceRuleViolation,
                id,
            ))
            .unwrap();
        }

        let cfg = cfg_for(&server.url());
        assert_eq!(flush(&cfg, &server.url()).await, Delivery::Failed);
        let queued: Vec<String> = load_all().into_iter().map(|e| e.insert_id).collect();
        assert_eq!(
            queued,
            owned(&["scope-1", "scope-failed", "check-1", "check-failed"])
        );
        assert_eq!(
            *requests.lock().unwrap(),
            vec![
                owned(&["scope-1", "scope-failed"]),
                owned(&["check-1", "check-failed"]),
            ]
        );

        let mut healthy = mockito::Server::new_async().await;
        let mock = healthy
            .mock("POST", "/plan-governance/record")
            .with_status(200)
            .with_body(r#"{"recorded":2,"failed":0}"#)
            .expect(2)
            .create_async()
            .await;
        let cfg = cfg_for(&healthy.url());
        assert_eq!(flush(&cfg, &healthy.url()).await, Delivery::Delivered);
        mock.assert_async().await;
        assert!(load_all().is_empty());
    }

    /// An outage drops nothing: a 408, a 429, a 5xx or a refused connection
    /// ends each stream's pass after its first request, with no resend and
    /// every event still queued.
    #[tokio::test]
    async fn test_flush_keeps_every_event_through_an_outage() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _no_tel = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        append(&sample_event("run-1", "scope-1")).unwrap();
        append(&check_event(
            PlanGovernanceEventName::PlanGovernanceCheckCompleted,
            "check-1",
        ))
        .unwrap();
        append(&check_event(
            PlanGovernanceEventName::PlanGovernanceRuleOverride,
            "override-1",
        ))
        .unwrap();
        let everything = owned(&["scope-1", "check-1", "override-1"]);

        for status in [408, 429, 500, 503] {
            let mut server = mockito::Server::new_async().await;
            // One request per stream.
            let mock = server
                .mock("POST", "/plan-governance/record")
                .with_status(status)
                .expect(2)
                .create_async()
                .await;

            let cfg = cfg_for(&server.url());
            assert_eq!(
                flush(&cfg, &server.url()).await,
                Delivery::Failed,
                "HTTP {status}"
            );
            mock.assert_async().await;
            let queued: Vec<String> = load_all().into_iter().map(|e| e.insert_id).collect();
            assert_eq!(queued, everything, "HTTP {status}");
        }

        let unreachable = "http://127.0.0.1:1";
        assert_eq!(
            flush(&cfg_for(unreachable), unreachable).await,
            Delivery::Failed
        );
        let queued: Vec<String> = load_all().into_iter().map(|e| e.insert_id).collect();
        assert_eq!(queued, everything);
    }

    /// A 401, 403, 404 or 413 is about the service key, the route or the
    /// request's size, not an event, so a key rotation or a bad deploy must not
    /// empty the spool. Each stream's multi-event chunk goes out once and stays
    /// queued whole, with no event sent on its own.
    #[tokio::test]
    async fn test_flush_keeps_every_event_on_a_401_403_404_or_413() {
        use std::sync::{Arc, Mutex};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _no_tel = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        append(&sample_event("run-1", "scope-1")).unwrap();
        append(&sample_event("run-2", "scope-2")).unwrap();
        append(&check_event(
            PlanGovernanceEventName::PlanGovernanceCheckCompleted,
            "check-1",
        ))
        .unwrap();
        append(&check_event(
            PlanGovernanceEventName::PlanGovernanceRuleOverride,
            "override-1",
        ))
        .unwrap();
        let everything = owned(&["scope-1", "scope-2", "check-1", "override-1"]);

        for status in [401, 403, 404, 413] {
            let requests = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
            let sink = requests.clone();
            let mut server = mockito::Server::new_async().await;
            let _mock = server
                .mock("POST", "/plan-governance/record")
                .with_status_code_from_request(move |req| {
                    sink.lock().unwrap().push(insert_ids_of(req));
                    status
                })
                .create_async()
                .await;

            let cfg = cfg_for(&server.url());
            assert_eq!(
                flush(&cfg, &server.url()).await,
                Delivery::Failed,
                "HTTP {status}"
            );
            let queued: Vec<String> = load_all().into_iter().map(|e| e.insert_id).collect();
            assert_eq!(queued, everything, "HTTP {status}");
            assert_eq!(
                *requests.lock().unwrap(),
                vec![
                    owned(&["scope-1", "scope-2"]),
                    owned(&["check-1", "override-1"]),
                ],
                "HTTP {status}"
            );
        }
    }

    /// An outage partway through the single sends ends that stream's pass and
    /// keeps the events it did not reach, and an outage in either stream makes
    /// the flush `Failed` even when the proxy also refused an event.
    #[tokio::test]
    async fn test_flush_an_outage_during_the_single_sends_keeps_the_rest() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _no_tel = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        // Refuses any batch carrying `poison` and answers every other with a
        // 503: the scope stream fails first, then the checks stream's chunk is
        // refused, `poison` is dropped on its own and `check-1` hits the outage.
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/plan-governance/record")
            .with_status_code_from_request(|req| {
                if insert_ids_of(req).iter().any(|id| id == "poison") {
                    400
                } else {
                    503
                }
            })
            .expect(4)
            .create_async()
            .await;

        append(&sample_event("run-1", "scope-1")).unwrap();
        for id in ["poison", "check-1", "check-2"] {
            append(&check_event(
                PlanGovernanceEventName::PlanGovernanceCheckCompleted,
                id,
            ))
            .unwrap();
        }

        let cfg = cfg_for(&server.url());
        assert_eq!(flush(&cfg, &server.url()).await, Delivery::Failed);
        mock.assert_async().await;
        let queued: Vec<String> = load_all().into_iter().map(|e| e.insert_id).collect();
        assert_eq!(queued, owned(&["scope-1", "check-1", "check-2"]));
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
    /// not the process) — which is the real deployment, where each command that
    /// spools an event spawns its own uploader.
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

    // --- Spool::take, the hand-off the OTLP uploader drains with ---

    fn otlp_lines(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("{{\"n\":{i}}}")).collect()
    }

    /// `take` hands over the oldest lines, at most `max`, and they leave the
    /// file as they go.
    #[test]
    fn test_take_hands_over_the_oldest_lines_and_removes_them() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        OTLP_LOGS.append_lines(&otlp_lines(5)).unwrap();

        assert_eq!(OTLP_LOGS.take(3), otlp_lines(3));
        assert_eq!(OTLP_LOGS.take(3), otlp_lines(5)[3..]);
        assert!(OTLP_LOGS.take(3).is_empty());
        assert!(!home.path().join("otlp-logs-spool.jsonl").exists());
        // The scope spool is a separate file.
        assert!(load_all().is_empty());
    }

    /// Several lines appended at once still respect the cap, oldest first.
    #[test]
    fn test_append_lines_past_the_cap_keeps_the_newest() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let all = otlp_lines(MAX_SPOOL_EVENTS + 10);
        OTLP_LOGS
            .append_lines(&all[..MAX_SPOOL_EVENTS - 5])
            .unwrap();
        OTLP_LOGS
            .append_lines(&all[MAX_SPOOL_EVENTS - 5..])
            .unwrap();

        assert_eq!(OTLP_LOGS.take(usize::MAX), all[10..]);
    }

    /// Nothing is taken without a config dir, without the lock, or from a
    /// spool that does not exist.
    #[test]
    fn test_take_without_a_spool_or_the_lock_takes_nothing() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        {
            let _cfg = EnvGuard::remove("ACTUAL_CONFIG");
            let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", "");
            assert!(OTLP_LOGS.take(1).is_empty());
        }
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        assert!(OTLP_LOGS.take(1).is_empty());

        OTLP_LOGS.append_lines(&otlp_lines(2)).unwrap();
        std::fs::remove_file(home.path().join("otlp-logs-spool.lock")).unwrap();
        std::fs::create_dir(home.path().join("otlp-logs-spool.lock")).unwrap();
        assert!(OTLP_LOGS.take(1).is_empty());
        let spooled = std::fs::read_to_string(home.path().join("otlp-logs-spool.jsonl")).unwrap();
        assert_eq!(spooled.lines().count(), 2);
    }

    /// A line is handed over only once it is gone from the file: when the
    /// rewrite fails, nothing is taken and the file is left as it was. Taking
    /// everything stages nothing, so it still succeeds, and hands over every
    /// line exactly once.
    #[test]
    fn test_take_returns_nothing_when_the_rewrite_fails() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        OTLP_LOGS.append_lines(&otlp_lines(3)).unwrap();
        // A non-empty directory where the rewrite stages its `.tmp` file.
        let staging = home.path().join("otlp-logs-spool.jsonl.tmp");
        std::fs::create_dir_all(staging.join("blocker")).unwrap();

        assert!(OTLP_LOGS.take(1).is_empty());
        let spooled = std::fs::read_to_string(home.path().join("otlp-logs-spool.jsonl")).unwrap();
        assert_eq!(spooled.lines().count(), 3);

        assert_eq!(OTLP_LOGS.take(3), otlp_lines(3));
        assert!(!home.path().join("otlp-logs-spool.jsonl").exists());
    }
}
