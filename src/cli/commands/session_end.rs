//! Best-effort session-end telemetry. The state is session-wide across rule
//! sets and agents. Checks share the brief ledger's lock wait and age pruning;
//! ends use a shorter wait to leave time for dispatch before the hook exits.

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
use std::time::Duration;

#[cfg(feature = "telemetry")]
const END_LOCK_WAIT: Duration = Duration::from_millis(100);

#[cfg(feature = "telemetry")]
#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct State {
    last_check_blocked: bool,
    ended: bool,
}

#[cfg(feature = "telemetry")]
#[derive(Deserialize)]
struct Envelope {
    session_id: String,
    hook_event_name: String,
    reason: String,
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

#[cfg(feature = "telemetry")]
fn report(raw: &str) {
    use crate::telemetry::{opt_out, otlp};

    let Ok(envelope) = serde_json::from_str::<Envelope>(raw) else {
        return;
    };
    if envelope.hook_event_name != "SessionEnd"
        || envelope.session_id.is_empty()
        || envelope.session_id.len() > 256
    {
        return;
    }
    let Some(config) = opt_out::load_if_enabled() else {
        return;
    };
    let Some(outputs) = otlp::Outputs::resolve(&config) else {
        return;
    };
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
    let reason = match envelope.reason.as_str() {
        "clear" | "resume" | "logout" | "prompt_input_exit" => envelope.reason.as_str(),
        _ => "other",
    };
    super::brief_telemetry::dispatch(
        &outputs,
        vec![otlp::Origin::resolve().session_ended(
            &envelope.session_id,
            reason,
            state.last_check_blocked,
        )],
    );
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
            let release = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(25));
                drop(held);
            });
            if check {
                record_check("contended", true);
            } else {
                report(&envelope("contended", "other"));
            }
            release.join().unwrap();
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
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(take_captured().is_empty());
    }
}
