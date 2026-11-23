//! Best-effort session-end telemetry. The state is session-wide across rule
//! sets and agents. Checks share the brief ledger's lock wait and age pruning;
//! ends use a shorter wait to leave time for dispatch before the hook exits.
//!
//! The end also sends, beside `actual.session.ended`, one
//! `actual.gate.resolved` record with the resolution `still_open` for each
//! block still open. A gate's session file is keyed by its rules directory as
//! well as the session, so the state names every rules directory a block
//! opened under.
//!
//! A signed-in end adds the session's token totals per model, read from the
//! transcript the envelope names; see [`super::session_usage`]. A repeated
//! end is suppressed before the scan, so the totals are those at the last end
//! reported, which lags a resumed session that completes no hook check.

use crate::error::ActualError;

pub fn exec() -> Result<(), ActualError> {
    #[cfg(feature = "telemetry")]
    report(&super::rules_brief::read_stdin());
    Ok(())
}

#[cfg(feature = "telemetry")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "telemetry")]
use sha2::{Digest, Sha256};
#[cfg(feature = "telemetry")]
use std::collections::BTreeSet;
#[cfg(feature = "telemetry")]
use std::path::{Path, PathBuf};
#[cfg(feature = "telemetry")]
use std::time::Duration;

#[cfg(feature = "telemetry")]
const END_LOCK_WAIT: Duration = Duration::from_millis(100);

/// How long the end may spend reading the transcript's token totals, inside
/// the plugin's 1 s hook timeout.
#[cfg(feature = "telemetry")]
const USAGE_SCAN_BUDGET: Duration = Duration::from_millis(500);

#[cfg(feature = "telemetry")]
#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct State {
    last_check_blocked: bool,
    ended: bool,
    /// The rules directories a gate opened a block under in this session.
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    rules_dirs: BTreeSet<PathBuf>,
}

#[cfg(feature = "telemetry")]
#[derive(Deserialize)]
struct Envelope {
    session_id: String,
    hook_event_name: String,
    reason: String,
    #[serde(default)]
    transcript_path: Option<PathBuf>,
}

#[cfg(feature = "telemetry")]
pub(super) fn record_check(session_id: &str, blocked: bool) {
    if session_id.is_empty() || session_id.len() > 256 {
        return;
    }
    with_state(session_id, super::brief_ledger::LOCK_WAIT, |state| {
        state.last_check_blocked = blocked;
        state.ended = false;
    });
}

/// Note that a gate opened a block under `rules_dir` in `session_id`, so the
/// session's end can find it.
#[cfg(feature = "telemetry")]
pub(super) fn record_rules_dir(session_id: &str, rules_dir: &Path) {
    if session_id.is_empty() || session_id.len() > 256 {
        return;
    }
    with_state(session_id, super::brief_ledger::LOCK_WAIT, |state| {
        state.rules_dirs.insert(rules_dir.to_path_buf());
    });
}

#[cfg(feature = "telemetry")]
fn with_state(session_id: &str, wait: Duration, change: impl FnOnce(&mut State)) -> Option<State> {
    let dir = crate::config::paths::config_dir()
        .ok()?
        .join("session-telemetry");
    std::fs::create_dir_all(&dir).ok()?;
    let _lock = super::brief_ledger::lock(&dir, wait)?;
    let path = dir.join(format!("{:x}.json", Sha256::digest(session_id.as_bytes())));
    let mut state: State = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    change(&mut state);
    let json = serde_json::to_vec(&state).ok()?;
    crate::config::paths::write_secure(&path, &json).ok()?;
    super::brief_ledger::prune_stale(&dir);
    Some(state)
}

/// The state for `session_id`, read without the lock and written nowhere, for
/// a telemetry debug run that keeps no state of its own.
#[cfg(feature = "telemetry")]
fn read_state(session_id: &str) -> State {
    crate::config::paths::config_dir()
        .ok()
        .map(|dir| {
            dir.join("session-telemetry")
                .join(format!("{:x}.json", Sha256::digest(session_id.as_bytes())))
        })
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

#[cfg(feature = "telemetry")]
pub(super) fn report(raw: &str) {
    use crate::telemetry::otlp;

    let Ok(envelope) = serde_json::from_str::<Envelope>(raw) else {
        return;
    };
    if envelope.hook_event_name != "SessionEnd"
        || envelope.session_id.is_empty()
        || envelope.session_id.len() > 256
    {
        return;
    }
    crate::telemetry::debug::use_session(&envelope.session_id);
    let Some(destination) = super::brief_telemetry::Destination::resolve() else {
        return;
    };
    let state = if destination.sends() {
        let mut fresh = false;
        let Some(state) = with_state(&envelope.session_id, END_LOCK_WAIT, |state| {
            fresh = !state.ended;
            state.ended = true;
        }) else {
            return;
        };
        if !fresh {
            return;
        }
        state
    } else {
        let state = read_state(&envelope.session_id);
        if state.ended {
            return;
        }
        state
    };
    let reason = match envelope.reason.as_str() {
        "clear" | "resume" | "logout" | "prompt_input_exit" => envelope.reason.as_str(),
        _ => "other",
    };
    let ended: Vec<_> = state
        .rules_dirs
        .iter()
        .flat_map(|rules_dir| {
            super::governance_session::end_open_blocks(
                &envelope.session_id,
                rules_dir,
                destination.sends(),
            )
        })
        .collect();
    let usage = envelope
        .transcript_path
        .as_deref()
        .filter(|_| destination.signed_in())
        .and_then(|path| {
            super::session_usage::read(path, std::time::Instant::now() + USAGE_SCAN_BUDGET)
        });
    let mut records = vec![otlp::Origin::resolve().session_ended(
        &envelope.session_id,
        reason,
        state.last_check_blocked,
        usage.as_ref(),
    )];
    records.extend(super::check_telemetry::resolved_records(
        Some(&envelope.session_id),
        None,
        &ended,
    ));
    destination.deliver(records);
}

#[cfg(all(test, feature = "telemetry"))]
mod tests {
    use super::*;
    use crate::cli::commands::brief_telemetry::tests::take_captured;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use tempfile::tempdir;

    fn envelope(id: &str, reason: &str) -> String {
        serde_json::json!({"session_id":id,"hook_event_name":"SessionEnd","reason":reason,
            "cwd":"/private/repo","transcript_path":"/private/transcript"})
        .to_string()
    }

    fn attributes(record: &serde_json::Value) -> serde_json::Value {
        record["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|kv| (kv["key"].as_str().unwrap().to_string(), kv["value"].clone()))
            .collect::<serde_json::Map<_, _>>()
            .into()
    }

    #[test]
    fn end_reports_the_last_completed_check_and_reopens_after_a_hook_check() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _home = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _config = EnvGuard::remove("ACTUAL_CONFIG");
        let _off = EnvGuard::remove("ACTUAL_NO_TELEMETRY");
        let _ci = EnvGuard::remove("CI");
        let _out = EnvGuard::set("ACTUAL_OTLP_FILE", "/tmp/session-end-test.jsonl");
        take_captured();
        let check = |id, hook, blocked| {
            use super::super::check_telemetry::{self, Action, Started};
            use crate::api::types::PlanGovernanceDecision;
            check_telemetry::report(
                &Action {
                    kind: crate::rules::check::ArtifactKind::Diff,
                    hook,
                    session_id: Some(id),
                    agent_id: None,
                    started: Started::now(),
                },
                if blocked {
                    PlanGovernanceDecision::Block
                } else {
                    PlanGovernanceDecision::Allow
                },
                &[],
                vec![],
            );
        };
        check("blocked", true, true);
        check("blocked", false, false);
        check("fixed", true, true);
        check("fixed", true, false);
        take_captured();
        for (id, reason) in [
            ("blocked", "prompt_input_exit"),
            ("fixed", "clear"),
            ("quiet", "private text"),
        ] {
            report(&envelope(id, reason));
            report(&envelope(id, reason));
        }
        let records = take_captured();
        assert_eq!(records.len(), 3);
        for (record, id, reason, blocked) in [
            (&records[0], "blocked", "prompt_input_exit", true),
            (&records[1], "fixed", "clear", false),
            (&records[2], "quiet", "other", false),
        ] {
            assert_eq!(record["eventName"], "actual.session.ended");
            let attrs = attributes(record);
            assert_eq!(attrs["actual.session.id"]["stringValue"], id);
            assert_eq!(attrs["actual.session.end_reason"]["stringValue"], reason);
            assert_eq!(
                attrs["actual.session.last_check_blocked"]["boolValue"],
                blocked
            );
            assert!(!record.to_string().contains("private"));
        }
        check("blocked", false, false);
        take_captured();
        report(&envelope("blocked", "logout"));
        assert!(take_captured().is_empty());
        check("blocked", true, false);
        take_captured();
        report(&envelope("blocked", "logout"));
        report(&envelope("blocked", "logout"));
        let resumed = take_captured();
        assert_eq!(resumed.len(), 1);
        let attrs = attributes(&resumed[0]);
        assert_eq!(attrs["actual.session.end_reason"]["stringValue"], "logout");
        assert_eq!(
            attrs["actual.session.last_check_blocked"]["boolValue"],
            false
        );
    }

    /// Signed in to Actual's API, the end carries the transcript's token
    /// totals and nothing of the transcript itself. Signed out, or with no
    /// transcript to read, it carries none.
    #[test]
    fn a_signed_in_end_carries_the_transcripts_token_totals() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _home = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _config = EnvGuard::remove("ACTUAL_CONFIG");
        let _off = EnvGuard::remove("ACTUAL_NO_TELEMETRY");
        let _ci = EnvGuard::remove("CI");
        let _out = EnvGuard::remove("ACTUAL_OTLP_FILE");
        let _api = EnvGuard::set("ACTUAL_API_URL", "https://api.example.test");
        let _endpoint = EnvGuard::set(
            "ACTUAL_OTLP_LOGS_ENDPOINT",
            "https://api.example.test/v1/logs",
        );
        let transcript = home.path().join("private-transcript.jsonl");
        let line = serde_json::json!({"type": "assistant", "message": {
            "id": "m1", "model": "claude-opus", "content": [{"type": "text", "text": "private words"}],
            "usage": {"input_tokens": 3, "output_tokens": 10, "cache_read_input_tokens": 100,
                      "cache_creation_input_tokens": 20}}});
        std::fs::write(&transcript, format!("{line}\n{line}\n")).unwrap();
        let end = |id: &str, transcript: Option<&Path>| {
            let mut raw: serde_json::Value = serde_json::from_str(&envelope(id, "clear")).unwrap();
            raw["transcript_path"] = serde_json::json!(transcript);
            report(&raw.to_string());
            let records = take_captured();
            assert_eq!(records.len(), 1);
            records.into_iter().next().unwrap()
        };
        let usage_keys = |record: &serde_json::Value| -> Vec<String> {
            attributes(record)
                .as_object()
                .unwrap()
                .keys()
                .filter(|key| key.starts_with("actual.usage."))
                .cloned()
                .collect()
        };
        take_captured();

        let signed_out = end("signed-out", Some(&transcript));
        crate::auth::store::save(&crate::telemetry::sign_in::tests::signed_in(
            "org-a",
            "token-a",
            chrono::Duration::hours(1),
            None,
        ))
        .unwrap();
        let no_transcript = end("no-transcript", None);
        let signed_in = end("signed-in", Some(&transcript));

        assert!(usage_keys(&signed_out).is_empty(), "{signed_out}");
        assert!(usage_keys(&no_transcript).is_empty(), "{no_transcript}");
        let attrs = attributes(&signed_in);
        let list = |key: &str| attrs[key]["arrayValue"]["values"].clone();
        assert_eq!(attrs["actual.usage.source"]["stringValue"], "messages");
        assert_eq!(
            list("actual.usage.models"),
            serde_json::json!([{"stringValue": "claude-opus"}])
        );
        assert_eq!(
            list("actual.usage.input_tokens"),
            serde_json::json!([{"intValue": "3"}])
        );
        assert_eq!(
            list("actual.usage.output_tokens"),
            serde_json::json!([{"intValue": "10"}])
        );
        assert_eq!(
            list("actual.usage.cache_read_tokens"),
            serde_json::json!([{"intValue": "100"}])
        );
        assert_eq!(
            list("actual.usage.cache_write_tokens"),
            serde_json::json!([{"intValue": "20"}])
        );
        for record in [&signed_out, &no_transcript, &signed_in] {
            assert!(!record.to_string().contains("private"), "{record}");
        }
    }

    /// An end repeated with no hook check between is suppressed before the
    /// scan, so tokens added after the first end go unreported until a hook
    /// check reopens the session.
    #[test]
    fn a_repeated_end_keeps_the_totals_of_the_last_reported_end() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _home = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _config = EnvGuard::remove("ACTUAL_CONFIG");
        let _off = EnvGuard::remove("ACTUAL_NO_TELEMETRY");
        let _ci = EnvGuard::remove("CI");
        let _out = EnvGuard::remove("ACTUAL_OTLP_FILE");
        let _api = EnvGuard::set("ACTUAL_API_URL", "https://api.example.test");
        let _endpoint = EnvGuard::set(
            "ACTUAL_OTLP_LOGS_ENDPOINT",
            "https://api.example.test/v1/logs",
        );
        crate::auth::store::save(&crate::telemetry::sign_in::tests::signed_in(
            "org-a",
            "token-a",
            chrono::Duration::hours(1),
            None,
        ))
        .unwrap();
        let transcript = home.path().join("transcript.jsonl");
        let message = |id: &str, input: u64| {
            serde_json::json!({"type": "assistant", "message": {
                "id": id, "model": "claude-opus", "usage": {"input_tokens": input}}})
            .to_string()
                + "\n"
        };
        let end = || {
            let mut raw: serde_json::Value =
                serde_json::from_str(&envelope("resumed", "resume")).unwrap();
            raw["transcript_path"] = serde_json::json!(transcript);
            report(&raw.to_string());
            take_captured()
        };
        let input_tokens = |records: Vec<serde_json::Value>| {
            assert_eq!(records.len(), 1, "{records:?}");
            attributes(&records[0])["actual.usage.input_tokens"]["arrayValue"]["values"].clone()
        };
        take_captured();

        std::fs::write(&transcript, message("m1", 3)).unwrap();
        assert_eq!(input_tokens(end()), serde_json::json!([{"intValue": "3"}]));

        let grown = message("m1", 3) + &message("m2", 5);
        std::fs::write(&transcript, grown).unwrap();
        assert!(end().is_empty());

        record_check("resumed", false);
        assert_eq!(input_tokens(end()), serde_json::json!([{"intValue": "8"}]));
    }

    #[test]
    fn checks_and_ends_wait_for_a_briefly_contended_lock() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _home = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _config = EnvGuard::remove("ACTUAL_CONFIG");
        let _off = EnvGuard::remove("ACTUAL_NO_TELEMETRY");
        let _ci = EnvGuard::remove("CI");
        let _out = EnvGuard::set("ACTUAL_OTLP_FILE", "/tmp/session-end-test.jsonl");
        take_captured();
        record_check("contended", false);
        let dir = home.path().join("session-telemetry");
        for check in [true, false] {
            let held = super::super::brief_ledger::lock(&dir, std::time::Duration::ZERO).unwrap();
            super::super::brief_ledger::tests::release_on_retry(
                move || held.unlock().unwrap(),
                || {
                    if check {
                        record_check("contended", true);
                    } else {
                        report(&envelope("contended", "other"));
                    }
                },
            );
        }
        let records = take_captured();
        assert_eq!(records.len(), 1);
        assert_eq!(
            attributes(&records[0])["actual.session.last_check_blocked"]["boolValue"],
            true
        );
    }

    #[test]
    fn malformed_opted_out_and_unwritable_hooks_are_silent() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _home = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _config = EnvGuard::remove("ACTUAL_CONFIG");
        let _ci = EnvGuard::remove("CI");
        let _out = EnvGuard::set("ACTUAL_OTLP_FILE", "/tmp/session-end-test.jsonl");
        take_captured();
        record_check("", true);
        record_check(&"s".repeat(257), true);
        for raw in [
            "",
            "{}",
            &envelope("", "other"),
            &envelope(&"s".repeat(257), "other"),
            r#"{"session_id":"s","hook_event_name":"Stop","reason":"other"}"#,
        ] {
            report(raw);
        }
        {
            let _off = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
            report(&envelope("off", "other"));
        }
        {
            let _out = EnvGuard::remove("ACTUAL_OTLP_FILE");
            let _endpoint = EnvGuard::remove("ACTUAL_OTLP_LOGS_ENDPOINT");
            report(&envelope("no-output", "other"));
        }
        let dir = home.path().join("session-telemetry");
        std::fs::write(&dir, "a file blocks the directory").unwrap();
        record_check("no-write", true);
        report(&envelope("no-write", "other"));
        std::fs::remove_file(&dir).unwrap();
        std::fs::create_dir(&dir).unwrap();
        let _held = super::super::brief_ledger::lock(&dir, std::time::Duration::ZERO).unwrap();
        let start = std::time::Instant::now();
        record_check("locked", true);
        assert!(start.elapsed() >= super::super::brief_ledger::LOCK_WAIT);
        let start = std::time::Instant::now();
        report(&envelope("locked", "other"));
        assert!(start.elapsed() >= END_LOCK_WAIT);
        assert!(END_LOCK_WAIT < Duration::from_secs(1));
        assert!(take_captured().is_empty());
    }

    /// Telemetry off in debug mode: the end is logged as not sent, under the
    /// session's own id, and no session state is written.
    #[test]
    fn debug_mode_with_telemetry_off_logs_the_end_and_writes_no_state() {
        use crate::telemetry::debug::{
            self,
            tests::{logged, FreshId},
        };
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _fresh = FreshId::new();
        let home = tempdir().unwrap();
        let _home = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _config = EnvGuard::remove("ACTUAL_CONFIG");
        let _off = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
        let _out = EnvGuard::set("ACTUAL_OTLP_FILE", "/tmp/session-end-test.jsonl");
        let _debug = EnvGuard::set(debug::ENV, "1");
        let _id = EnvGuard::remove(debug::ID_ENV);
        take_captured();

        report(&envelope("debug-session-1", "clear"));

        assert!(take_captured().is_empty());
        assert!(!home.path().join("session-telemetry").exists());
        let lines = logged("debug-session-1");
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0]["stage"], "not_sent");
        assert_eq!(
            lines[0]["event"]["eventName"],
            crate::telemetry::otlp::SESSION_ENDED
        );

        // A session already ended with telemetry on has nothing more to send.
        let _on = EnvGuard::remove("ACTUAL_NO_TELEMETRY");
        report(&envelope("debug-session-2", "clear"));
        assert_eq!(take_captured().len(), 1);
        let _off_again = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
        report(&envelope("debug-session-2", "clear"));
        assert!(take_captured().is_empty());
        assert_eq!(logged("debug-session-2").len(), 0);
    }

    /// Open a block on `adr_ref` in `session_id`'s plan loop under
    /// `rules_dir`, as a denied round does, and note the directory.
    fn open_block(session_id: &str, rules_dir: &Path, adr_ref: &str) {
        let mut session = super::super::governance_session::load(session_id, rules_dir);
        session.plan.rounds += 1;
        session
            .plan
            .open_block(1, adr_ref, &[], &format!("{adr_ref}::R-1"), "action-0");
        super::super::governance_session::store(session_id, rules_dir, &session);
        record_rules_dir(session_id, rules_dir);
    }

    /// The end sends `still_open` for each block still open, under every
    /// rules directory a block opened under, beside the session's end, and
    /// only once: the blocks are ended in the session file. A session with no
    /// open block sends the end alone.
    #[test]
    fn the_end_sends_each_block_still_open_once() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _home = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _config = EnvGuard::remove("ACTUAL_CONFIG");
        let _off = EnvGuard::remove("ACTUAL_NO_TELEMETRY");
        let _ci = EnvGuard::remove("CI");
        let _out = EnvGuard::set("ACTUAL_OTLP_FILE", "/tmp/session-end-test.jsonl");
        let (a, b) = (home.path().join("a/rules"), home.path().join("b/rules"));
        open_block("open", &a, "adr-a");
        open_block("open", &b, "adr-b");
        record_rules_dir("", &a);
        record_rules_dir(&"s".repeat(257), &a);
        take_captured();

        report(&envelope("open", "clear"));
        report(&envelope("open", "clear"));
        report(&envelope("quiet", "clear"));
        let records = take_captured();
        let events: Vec<(&str, String, String)> = records
            .iter()
            .map(|record| {
                let attrs = attributes(record);
                let text = |key: &str| attrs[key]["stringValue"].as_str().unwrap_or("").to_string();
                (
                    record["eventName"].as_str().unwrap(),
                    text("actual.session.id"),
                    text("actual.adr.ref") + &text("actual.gate.resolution"),
                )
            })
            .collect();
        assert_eq!(
            events,
            [
                ("actual.session.ended", "open".into(), String::new()),
                (
                    "actual.gate.resolved",
                    "open".into(),
                    "adr-astill_open".into()
                ),
                (
                    "actual.gate.resolved",
                    "open".into(),
                    "adr-bstill_open".into()
                ),
                ("actual.session.ended", "quiet".into(), String::new()),
            ]
        );
        assert_eq!(
            attributes(&records[1])["actual.gate.rounds"]["intValue"],
            "1"
        );
        assert!(super::super::governance_session::load("open", &a)
            .plan
            .episodes
            .is_empty());
    }

    /// Telemetry off in debug mode: a block still open at the end is logged
    /// as not sent, and the session file keeps it.
    #[test]
    fn debug_mode_logs_a_block_still_open_and_keeps_it() {
        use crate::telemetry::debug::{
            self,
            tests::{logged, FreshId},
        };
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _fresh = FreshId::new();
        let home = tempdir().unwrap();
        let _home = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _config = EnvGuard::remove("ACTUAL_CONFIG");
        let _ci = EnvGuard::remove("CI");
        let _out = EnvGuard::set("ACTUAL_OTLP_FILE", "/tmp/session-end-test.jsonl");
        let _debug = EnvGuard::set(debug::ENV, "1");
        let _id = EnvGuard::remove(debug::ID_ENV);
        let rules_dir = home.path().join("rules");
        open_block("debug-open", &rules_dir, "adr-a");
        let _off = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
        take_captured();

        report(&envelope("debug-open", "clear"));

        assert!(take_captured().is_empty());
        let events: Vec<serde_json::Value> = logged("debug-open")
            .iter()
            .map(|line| line["event"]["eventName"].clone())
            .collect();
        assert_eq!(
            events,
            [
                crate::telemetry::otlp::SESSION_ENDED,
                crate::telemetry::otlp::GATE_RESOLVED
            ]
        );
        assert_eq!(
            super::super::governance_session::load("debug-open", &rules_dir)
                .plan
                .episodes
                .len(),
            1
        );
    }
}
