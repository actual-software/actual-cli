//! The OpenTelemetry events `plan-check --claude-hook` and `impl-check
//! --claude-hook` send once the judge has answered: one [`otlp::ADR_DETECTED`]
//! record per logical ADR the round judged a rule of, with the gate as how it
//! was detected, and one [`otlp::GATE_EVALUATED`] record per logical ADR it
//! blocked on, with the outcome `breached` and the gate.
//!
//! # Design
//!
//! **Used means checked against.** A round uses an ADR when the judge returns
//! a verdict for any of its rules, whatever the verdict, so a blocking verdict
//! sends both events. A rule the session already settled never reaches the
//! judge, so it uses nothing.
//!
//! **Breached means blocked on.** The two verdicts the hooks deny on,
//! `conflicting` and `requires_decision`, breach their ADR. A round the round
//! limit lets through still breaches, though the denials that reached the
//! limit have normally sent the event already.
//!
//! **Once per ADR per session and stage.** impl-check judges again at every
//! Stop, so each event's keys go into the session's brief ledger beside the
//! context event's, one set per stage, and a record is built only for a key
//! that write added. A missed lock or a failed write therefore undercounts,
//! and nothing is sent twice.
//!
//! **No session, nothing sent.** Direct mode, and a hook envelope without a
//! session id, have nothing to count once against or to join on.
//!
//! **What the hook pays.** The judge's prompt, output and call count are
//! unchanged. With telemetry opted out or no output set, [`report`] returns
//! once it has read the config. A round with nothing new reads the ledger
//! once, without its lock; only a new key costs the locked write, the spool
//! append and the detached uploader's start.

use std::collections::BTreeMap;

use super::brief_ledger::{self, LedgerKey};
use super::brief_telemetry::{self, BriefedAdr};
use super::check_engine::HookRun;
use crate::rules::check::{ArtifactKind, CheckedRule};
use crate::rules::types::AdrMeta;
use crate::telemetry::otlp;

/// Report the logical ADRs a hook round judged, and those it blocked on, that
/// are new to this session at this stage. `headers` names each judged rule's
/// ADR. Best-effort and silent, like the hook's other telemetry.
pub(super) fn report(
    run: &HookRun<'_>,
    headers: &BTreeMap<String, AdrMeta>,
    verdicts: &[CheckedRule],
    blocking: &[&CheckedRule],
) {
    let Some(session_id) = run.session_id.filter(|id| !id.is_empty()) else {
        return;
    };
    let config = crate::config::paths::load().unwrap_or_default();
    if otlp::Outputs::resolve(&config).is_none() {
        return;
    }
    let used = adrs_of(headers, verdicts.iter());
    if used.is_empty() {
        return;
    }
    let breached = adrs_of(headers, blocking.iter().copied());
    let rules_dir = super::rules_brief::resolve(run.rules_dir);
    let key = LedgerKey {
        session_id,
        rules_dir: &rules_dir,
    };
    let stage = stage(run.kind);
    let (new_used, new_breached) = brief_ledger::ledger_dir()
        .map(|dir| brief_ledger::report_checks(&dir, &key, stage, &keys(&used), &keys(&breached)))
        .unwrap_or_default();
    if new_used.is_empty() && new_breached.is_empty() {
        return;
    }
    let origin = otlp::Origin::resolve();
    let gate = gate(run.kind);
    let mut records = Vec::new();
    for adr in used.iter().filter(|adr| new_used.contains(&adr.key)) {
        records.push(origin.used(session_id, run.agent_id, &adr.key, &adr.adr_ids, gate));
    }
    for adr in breached
        .iter()
        .filter(|adr| new_breached.contains(&adr.key))
    {
        records.push(origin.breached(session_id, run.agent_id, &adr.key, &adr.adr_ids, gate));
    }
    brief_telemetry::dispatch(records);
}

/// The logical ADRs whose rules `judged` holds verdicts for, in verdict order.
/// A rule from a file without a v2 header names none.
fn adrs_of<'a>(
    headers: &'a BTreeMap<String, AdrMeta>,
    judged: impl Iterator<Item = &'a CheckedRule>,
) -> Vec<BriefedAdr> {
    brief_telemetry::group_headers(judged.filter_map(|verdict| headers.get(&verdict.doc_slug)))
}

fn keys(adrs: &[BriefedAdr]) -> Vec<&str> {
    adrs.iter().map(|adr| adr.key.as_str()).collect()
}

/// The stage a gate judges `kind` at, as the ledger keys its events.
fn stage(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Plan => "plan",
        ArtifactKind::Diff => "implementation",
    }
}

/// The gate that judges `kind`, as the events name it.
fn gate(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Plan => "planning",
        ArtifactKind::Diff => "implementation",
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    use std::path::Path;

    use super::super::brief_telemetry::tests::{attribute, take_captured};
    use crate::rules::check::Verdict;
    use crate::rules::types::RuleLevel;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use serde_json::json;
    use tempfile::tempdir;

    /// `body` behind a v2 header naming `adr_id`, in `group` when one is given.
    pub(in crate::cli::commands) fn with_header(
        adr_id: &str,
        group: Option<&str>,
        body: &str,
    ) -> String {
        let group = group.map_or(String::new(), |g| format!("    candidate_group_id: {g}\n"));
        format!(
            "---\nversion: 2\nname: {adr_id}\ndescription: \"{adr_id}\"\nmetadata:\n  actual:\n    adr_id: {adr_id}\n    status: accepted\n{group}---\n{body}"
        )
    }

    const WITH_FILE: &str = "telemetry:\n  otlp_file: /tmp/actual-otlp-check-test.jsonl\n";

    /// The headers of two files in the group `grp-sign` and one with no group.
    /// `legacy`, a file without a v2 header, has none.
    fn headers() -> BTreeMap<String, AdrMeta> {
        [
            ("signing-a1", "adr-sign-1", Some("grp-sign")),
            ("signing-a2", "adr-sign-2", Some("grp-sign")),
            ("rotation", "adr-rotate", None),
        ]
        .into_iter()
        .map(|(slug, adr_id, group)| {
            let header = AdrMeta {
                adr_id: Some(adr_id.to_string()),
                candidate_group_id: group.map(str::to_string),
                ..Default::default()
            };
            (slug.to_string(), header)
        })
        .collect()
    }

    fn judged(slug: &str, verdict: Verdict) -> CheckedRule {
        CheckedRule {
            doc_slug: slug.to_string(),
            rule_id: "R-1".to_string(),
            level: RuleLevel::Must,
            statement: "follow it.".to_string(),
            verdict,
            span: String::new(),
            reason: String::new(),
        }
    }

    /// Run `body` with a scratch config dir whose config sets `config`.
    fn with_config(config: &str, body: impl FnOnce(&Path)) {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _guards: Vec<EnvGuard> = [
            "ACTUAL_CONFIG",
            "ACTUAL_NO_TELEMETRY",
            "ACTUAL_OTLP_LOGS_ENDPOINT",
            "ACTUAL_OTLP_FILE",
            "TRACEPARENT",
        ]
        .into_iter()
        .map(EnvGuard::remove)
        .collect();
        std::fs::write(home.path().join("config.yaml"), config).unwrap();
        take_captured();
        body(home.path());
    }

    /// Report a round that judged `verdicts` at `kind`'s stage in `session`,
    /// blocking on the ones the hooks deny on, as `finish_verdicts` does.
    fn round(home: &Path, kind: ArtifactKind, session: Option<&str>, verdicts: &[CheckedRule]) {
        let rules_dir = home.join("rules");
        let run = HookRun {
            kind,
            command: "test --claude-hook",
            session_id: session,
            agent_id: Some("agent-1"),
            artifact_digest: "digest-1",
            root: home,
            rules_dir: &rules_dir,
            max_rounds: 3,
            started_at: std::time::Instant::now(),
        };
        let blocking: Vec<&CheckedRule> = verdicts
            .iter()
            .filter(|v| matches!(v.verdict, Verdict::Conflicting | Verdict::RequiresDecision))
            .collect();
        report(&run, &headers(), verdicts, &blocking);
    }

    /// The records reported since the last call as (event, gate, ADR ref, ADR
    /// ids), after checking the session and agent each carries. A detected
    /// record names its gate as how it was detected, and an evaluated one as
    /// its gate type, beside the outcome `breached`.
    fn reported(session: &str) -> Vec<(String, String, String, serde_json::Value)> {
        take_captured()
            .iter()
            .map(|record| {
                assert_eq!(attribute(record, "actual.session.id"), session);
                assert_eq!(attribute(record, "actual.agent.id"), "agent-1");
                let event = record["eventName"].as_str().unwrap().to_string();
                let gate = if event == otlp::GATE_EVALUATED {
                    assert_eq!(attribute(record, "actual.gate.outcome"), "breached");
                    attribute(record, "actual.gate.type")
                } else {
                    attribute(record, "actual.detection.method")
                };
                (
                    event,
                    gate.as_str().unwrap().to_string(),
                    attribute(record, "actual.adr.ref")
                        .as_str()
                        .unwrap()
                        .to_string(),
                    attribute(record, "actual.adr.ids"),
                )
            })
            .collect()
    }

    fn record(
        event: &str,
        gate: &str,
        adr_ref: &str,
        ids: serde_json::Value,
    ) -> (String, String, String, serde_json::Value) {
        (
            event.to_string(),
            gate.to_string(),
            adr_ref.to_string(),
            ids,
        )
    }

    /// Every ADR the round judged a rule of is used, listing the files judged,
    /// and each it blocked on is breached too, listing the files blocked on.
    /// A file without a v2 header names no ADR either way.
    #[test]
    fn test_a_round_uses_each_judged_adr_and_breaches_each_blocking_one() {
        with_config(WITH_FILE, |home| {
            round(
                home,
                ArtifactKind::Plan,
                Some("s1"),
                &[
                    judged("signing-a1", Verdict::Conflicting),
                    judged("rotation", Verdict::Conforming),
                    judged("signing-a2", Verdict::Conforming),
                    judged("legacy", Verdict::Conflicting),
                ],
            );

            assert_eq!(
                reported("s1"),
                [
                    record(
                        otlp::ADR_DETECTED,
                        "planning",
                        "grp-sign",
                        json!(["adr-sign-1", "adr-sign-2"])
                    ),
                    record(
                        otlp::ADR_DETECTED,
                        "planning",
                        "adr-rotate",
                        json!(["adr-rotate"])
                    ),
                    record(
                        otlp::GATE_EVALUATED,
                        "planning",
                        "grp-sign",
                        json!(["adr-sign-1"])
                    ),
                ]
            );
        });
    }

    /// The hooks deny on `requires_decision` as on `conflicting`, so it is a
    /// breach too, and a round with nothing blocking breaches nothing.
    #[test]
    fn test_requires_decision_breaches_and_conforming_only_uses() {
        with_config(WITH_FILE, |home| {
            round(
                home,
                ArtifactKind::Diff,
                Some("s1"),
                &[judged("rotation", Verdict::RequiresDecision)],
            );
            assert_eq!(
                reported("s1"),
                [
                    record(
                        otlp::ADR_DETECTED,
                        "implementation",
                        "adr-rotate",
                        json!(["adr-rotate"])
                    ),
                    record(
                        otlp::GATE_EVALUATED,
                        "implementation",
                        "adr-rotate",
                        json!(["adr-rotate"])
                    ),
                ]
            );

            round(
                home,
                ArtifactKind::Diff,
                Some("s2"),
                &[judged("rotation", Verdict::Conforming)],
            );
            assert_eq!(
                reported("s2"),
                [record(
                    otlp::ADR_DETECTED,
                    "implementation",
                    "adr-rotate",
                    json!(["adr-rotate"])
                )]
            );
        });
    }

    /// Each event goes once per ADR per session and stage: a plan breach and
    /// a later implementation breach of one ADR are two of each, and the next
    /// Stop that blocks on it again sends nothing. Another session, the
    /// control, sends again.
    #[test]
    fn test_each_event_is_sent_once_per_session_and_stage() {
        with_config(WITH_FILE, |home| {
            let blocking = [judged("signing-a1", Verdict::Conflicting)];
            let both = |stage: &str| {
                [
                    record(otlp::ADR_DETECTED, stage, "grp-sign", json!(["adr-sign-1"])),
                    record(
                        otlp::GATE_EVALUATED,
                        stage,
                        "grp-sign",
                        json!(["adr-sign-1"]),
                    ),
                ]
            };

            round(home, ArtifactKind::Plan, Some("s1"), &blocking);
            assert_eq!(reported("s1"), both("planning"));
            round(home, ArtifactKind::Plan, Some("s1"), &blocking);
            assert_eq!(reported("s1"), []);

            round(home, ArtifactKind::Diff, Some("s1"), &blocking);
            assert_eq!(reported("s1"), both("implementation"));
            round(home, ArtifactKind::Diff, Some("s1"), &blocking);
            assert_eq!(reported("s1"), [], "impl-check's next Stop");

            round(home, ArtifactKind::Diff, Some("s2"), &blocking);
            assert_eq!(reported("s2"), both("implementation"));
        });
    }

    /// The two events count apart: an ADR used on a clean round is still
    /// breached when a later round blocks on it, and only the breach is new.
    #[test]
    fn test_a_used_adr_is_still_breached_later() {
        with_config(WITH_FILE, |home| {
            round(
                home,
                ArtifactKind::Diff,
                Some("s1"),
                &[judged("signing-a2", Verdict::Conforming)],
            );
            assert_eq!(
                reported("s1"),
                [record(
                    otlp::ADR_DETECTED,
                    "implementation",
                    "grp-sign",
                    json!(["adr-sign-2"])
                )]
            );

            round(
                home,
                ArtifactKind::Diff,
                Some("s1"),
                &[
                    judged("signing-a1", Verdict::Conflicting),
                    judged("signing-a2", Verdict::Conforming),
                ],
            );
            assert_eq!(
                reported("s1"),
                [record(
                    otlp::GATE_EVALUATED,
                    "implementation",
                    "grp-sign",
                    json!(["adr-sign-1"])
                )]
            );
        });
    }

    /// No session id, or an empty one, has nothing to count once against: no
    /// record, no ledger and no install id.
    #[test]
    fn test_a_round_without_a_session_sends_nothing() {
        with_config(WITH_FILE, |home| {
            for session in [None, Some("")] {
                round(
                    home,
                    ArtifactKind::Plan,
                    session,
                    &[judged("signing-a1", Verdict::Conflicting)],
                );
            }

            assert!(take_captured().is_empty());
            assert!(!home.join("brief-ledger").exists());
            assert!(!home.join("telemetry-id").exists());
        });
    }

    /// Rules from files without a v2 header name no ADR, so a round of only
    /// those, or of none, sends nothing and leaves the ledger alone.
    #[test]
    fn test_a_round_naming_no_adr_sends_nothing() {
        with_config(WITH_FILE, |home| {
            round(
                home,
                ArtifactKind::Plan,
                Some("s1"),
                &[judged("legacy", Verdict::Conflicting)],
            );
            round(home, ArtifactKind::Plan, Some("s1"), &[]);

            assert!(take_captured().is_empty());
            assert!(!home.join("brief-ledger").exists());
            assert!(!home.join("telemetry-id").exists());
        });
    }

    /// No output set, or opted out by config or env: nothing is built, the
    /// ledger is never written and no install id is created.
    #[test]
    fn test_a_round_unconfigured_or_opted_out_does_nothing() {
        let blocking = [judged("signing-a1", Verdict::Conflicting)];
        let opted_out = format!("{WITH_FILE}  enabled: false\n");
        for config in ["", opted_out.as_str()] {
            with_config(config, |home| {
                round(home, ArtifactKind::Plan, Some("s1"), &blocking);

                assert!(take_captured().is_empty());
                assert!(!home.join("brief-ledger").exists());
                assert!(!home.join("telemetry-id").exists());
            });
        }
        with_config(WITH_FILE, |home| {
            let _no = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");

            round(home, ArtifactKind::Plan, Some("s1"), &blocking);

            assert!(take_captured().is_empty());
            assert!(!home.join("brief-ledger").exists());
            assert!(!home.join("telemetry-id").exists());
        });
    }
}
