//! The OpenTelemetry events `plan-check` and `impl-check` send once the judge
//! has answered. Every run that reaches a verdict sends an
//! [`otlp::ACTION_STARTED`] record, stamped when the check started, and an
//! [`otlp::ACTION_COMPLETED`] record with its outcome, what made a `warn` one,
//! and its duration. A hook round also sends, between those two, one
//! [`otlp::ADR_DETECTED`] record per logical ADR it judged a rule of, with the
//! gate as how it was detected, and one [`otlp::GATE_EVALUATED`] record per
//! logical ADR it blocked on, with the outcome `breached` and the gate.
//!
//! # Design
//!
//! **Started is stamped at the start and sent at the end.** A run reads the
//! time just before the pipeline that calls the judge, and builds both action
//! records once it has a verdict, so the started record carries the real start
//! and the two share one spool write and one uploader start. The action
//! events go wherever the plan-governance `check_started` and `check_completed`
//! events go, and with the same outcome, so a run that never reaches a verdict
//! sends neither. Every caller reports them before it hands those to the
//! spool, so the completed record times the check alone.
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
//! **No session, no ADR events.** Direct mode, and a hook envelope without a
//! session id, have nothing to count an ADR once against. Their action events
//! still go, with the session id empty.
//!
//! **What the hook pays.** The judge's prompt, output and call count are
//! unchanged. With telemetry opted out or no output set, a round reads the
//! config and builds nothing. With an output set, every round appends its
//! records to the spool and starts the detached uploader. A round with no new
//! ADR reads the ledger once, without its lock; only a new key costs the
//! locked write.

use std::collections::BTreeMap;
use std::time::{Instant, SystemTime};

use super::brief_ledger::{self, LedgerKey};
use super::brief_telemetry::{self, BriefedAdr};
use super::check_engine::HookRun;
use crate::api::types::PlanGovernanceDecision;
use crate::rules::check::{ArtifactKind, CheckedRule, Verdict};
use crate::rules::types::AdrMeta;
use crate::telemetry::otlp;

/// When a check started: the time its started event carries and the instant
/// its duration counts from, read together.
#[derive(Debug, Clone, Copy)]
pub(super) struct Started {
    pub time: SystemTime,
    pub instant: Instant,
}

impl Started {
    pub(super) fn now() -> Self {
        Self {
            time: SystemTime::now(),
            instant: Instant::now(),
        }
    }
}

/// One plan-check or impl-check run, as its action events describe it.
pub(super) struct Action<'a> {
    pub kind: ArtifactKind,
    /// Whether Claude Code ran it as a hook.
    pub hook: bool,
    pub session_id: Option<&'a str>,
    pub agent_id: Option<&'a str>,
    pub started: Started,
}

/// What made a check `warn`, as `actual.action.warn_reasons` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WarnReason {
    /// Every rule a hook round blocked on had been denied more times than
    /// `--max-rounds` allows, so the round went through.
    RoundLimit,
    /// A direct run judged a rule as needing a decision, which only the hooks
    /// deny on.
    RequiresDecision,
    /// The judge saw only some of the rules that apply.
    PartialCoverage,
    /// A hook round passed while the session carries an override.
    ActiveOverride,
}

/// What makes a direct run that blocks on nothing `warn`.
pub(super) fn direct_warn_reasons(verdicts: &[CheckedRule], partial: bool) -> Vec<WarnReason> {
    let requires_decision = verdicts
        .iter()
        .any(|v| v.verdict == Verdict::RequiresDecision);
    [
        requires_decision.then_some(WarnReason::RequiresDecision),
        partial.then_some(WarnReason::PartialCoverage),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Report a run that came to `decision`: its started record, then `records`,
/// then its completed record, in one dispatch. `warn_reasons` go out only on
/// a `warn`. Best-effort and silent, like the hook's other telemetry.
pub(super) fn report(
    action: &Action<'_>,
    decision: PlanGovernanceDecision,
    warn_reasons: &[WarnReason],
    records: Vec<otlp::LogRecord>,
) {
    let Some(config) = crate::telemetry::opt_out::load_if_enabled() else {
        return;
    };
    if otlp::Outputs::resolve(&config).is_none() {
        return;
    }
    let origin = otlp::Origin::resolve();
    let session_id = action.session_id.unwrap_or_default();
    let action_type = action_type(action.kind);
    let mode = if action.hook { "hook" } else { "direct" };
    let warn_reasons: Vec<&str> = match decision {
        PlanGovernanceDecision::Warn => warn_reasons.iter().map(|&r| warn_reason(r)).collect(),
        PlanGovernanceDecision::Allow | PlanGovernanceDecision::Block => Vec::new(),
    };
    let mut all = vec![origin.action_started(
        action.started.time,
        session_id,
        action.agent_id,
        action_type,
        mode,
    )];
    all.extend(records);
    all.push(origin.action_completed(
        session_id,
        action.agent_id,
        action_type,
        mode,
        outcome(decision),
        &warn_reasons,
        action.started.instant.elapsed(),
    ));
    brief_telemetry::dispatch(all);
}

/// [`report`] for a hook round, with the round's ADR records.
pub(super) fn report_round(
    run: &HookRun<'_>,
    decision: PlanGovernanceDecision,
    warn_reasons: &[WarnReason],
    records: Vec<otlp::LogRecord>,
) {
    let action = Action {
        kind: run.kind,
        hook: true,
        session_id: run.session_id,
        agent_id: run.agent_id,
        started: run.started,
    };
    report(&action, decision, warn_reasons, records);
}

/// The records for the logical ADRs a hook round judged, and those it blocked
/// on, that are new to this session at this stage. `headers` names each
/// judged rule's ADR.
pub(super) fn adr_records(
    run: &HookRun<'_>,
    headers: &BTreeMap<String, AdrMeta>,
    verdicts: &[CheckedRule],
    blocking: &[&CheckedRule],
) -> Vec<otlp::LogRecord> {
    let Some(session_id) = run.session_id.filter(|id| !id.is_empty()) else {
        return Vec::new();
    };
    let Some(config) = crate::telemetry::opt_out::load_if_enabled() else {
        return Vec::new();
    };
    if otlp::Outputs::resolve(&config).is_none() {
        return Vec::new();
    }
    let used = adrs_of(headers, verdicts.iter());
    if used.is_empty() {
        return Vec::new();
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
        return Vec::new();
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
    records
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

/// The command that judges `kind`, as the action events name it.
fn action_type(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Plan => "plan-check",
        ArtifactKind::Diff => "impl-check",
    }
}

/// What a check came to, in the plan-governance events' words.
fn outcome(decision: PlanGovernanceDecision) -> &'static str {
    match decision {
        PlanGovernanceDecision::Allow => "allow",
        PlanGovernanceDecision::Warn => "warn",
        PlanGovernanceDecision::Block => "block",
    }
}

fn warn_reason(reason: WarnReason) -> &'static str {
    match reason {
        WarnReason::RoundLimit => "round_limit",
        WarnReason::RequiresDecision => "requires_decision",
        WarnReason::PartialCoverage => "partial_coverage",
        WarnReason::ActiveOverride => "active_override",
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    use std::path::Path;

    use super::super::brief_telemetry::tests::{attribute, take_captured};
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

    /// Dispatch the ADR records of a round that judged `verdicts` at `kind`'s
    /// stage in `session`, blocking on the ones the hooks deny on, as
    /// `finish_verdicts` does.
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
            started: Started::now(),
        };
        let blocking: Vec<&CheckedRule> = verdicts
            .iter()
            .filter(|v| matches!(v.verdict, Verdict::Conflicting | Verdict::RequiresDecision))
            .collect();
        brief_telemetry::dispatch(adr_records(&run, &headers(), verdicts, &blocking));
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

    /// How many records one blocking round builds with `config` in the config
    /// file, and whether it wrote the ledger. `ACTUAL_OTLP_FILE` names an
    /// output, so the config alone decides whether anything is sent.
    fn report_one_blocking_round_under(config: &str) -> (usize, bool) {
        let mut outcome = (0, false);
        with_config(config, |home| {
            let sink = home.join("otlp.jsonl");
            let _file = EnvGuard::set("ACTUAL_OTLP_FILE", sink.to_str().unwrap());
            round(
                home,
                ArtifactKind::Plan,
                Some("s1"),
                &[judged("rotation", Verdict::Conflicting)],
            );
            outcome = (take_captured().len(), home.join("brief-ledger").exists());
        });
        outcome
    }

    /// `telemetry.enabled: false` in a config file that doesn't load: nothing
    /// is built, and the ledger is never written.
    #[test]
    fn test_a_round_sends_nothing_when_the_config_cannot_load() {
        assert_eq!(
            report_one_blocking_round_under(crate::testutil::UNLOADABLE_TELEMETRY_OPT_OUT),
            (0, false)
        );
    }

    /// The positive control: a config that loads sends the round's used and
    /// breached events.
    #[test]
    fn test_a_round_sends_when_the_config_loads() {
        assert_eq!(
            report_one_blocking_round_under(crate::testutil::LOADABLE_TELEMETRY_ON),
            (2, true)
        );
    }

    /// Assert that `completed`, a check's completed record, was stamped no
    /// later than the plan-governance `check_completed` event among `events`,
    /// so the run read its end before it built and sent that event.
    pub(in crate::cli::commands) fn assert_completed_before_governance(
        completed: &serde_json::Value,
        events: &[crate::api::types::PlanGovernanceEvent],
    ) {
        let governance = events
            .iter()
            .find(|e| {
                e.event == crate::api::types::PlanGovernanceEventName::PlanGovernanceCheckCompleted
            })
            .expect("a check_completed event");
        let sent = chrono::DateTime::parse_from_rfc3339(governance.timestamp.as_deref().unwrap())
            .unwrap()
            .timestamp_nanos_opt()
            .unwrap();
        let stamped: i64 = completed["timeUnixNano"].as_str().unwrap().parse().unwrap();
        assert!(
            stamped <= sent,
            "completed at {stamped}, after check_completed at {sent}"
        );
    }

    /// The milliseconds a completed record says its check took.
    pub(in crate::cli::commands) fn duration_ms(record: &serde_json::Value) -> u128 {
        let attributes = record["attributes"].as_array().unwrap();
        let duration = attributes
            .iter()
            .find(|kv| kv["key"] == "actual.action.duration_ms")
            .unwrap();
        duration["value"]["intValue"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap()
    }

    /// A run that started 250 ms ago at `time`, as a plan-check hook round in
    /// session `s1` for `agent-1`.
    fn hook_action(time: SystemTime) -> Action<'static> {
        Action {
            kind: ArtifactKind::Plan,
            hook: true,
            session_id: Some("s1"),
            agent_id: Some("agent-1"),
            started: Started {
                time,
                instant: Instant::now() - std::time::Duration::from_millis(250),
            },
        }
    }

    /// A run reports its started record, stamped when the check started, then
    /// the records it was given, then its completed record with the outcome and
    /// how long the check took. All of them share the run's action id.
    #[test]
    fn test_a_run_reports_started_then_its_records_then_completed() {
        with_config(WITH_FILE, |home| {
            let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_792_446_915);
            let used = otlp::Origin::resolve().used(
                "s1",
                Some("agent-1"),
                "grp-sign",
                &["adr-sign-1".to_string()],
                "planning",
            );

            report(
                &hook_action(time),
                PlanGovernanceDecision::Block,
                &[],
                vec![used],
            );

            let records = take_captured();
            let events: Vec<&str> = records
                .iter()
                .map(|record| record["eventName"].as_str().unwrap())
                .collect();
            assert_eq!(
                events,
                [
                    otlp::ACTION_STARTED,
                    otlp::ADR_DETECTED,
                    otlp::ACTION_COMPLETED
                ]
            );
            let (started, completed) = (&records[0], &records[2]);
            assert_eq!(started["timeUnixNano"], "1792446915000000000");
            for record in [started, completed] {
                assert_eq!(attribute(record, "actual.session.id"), "s1");
                assert_eq!(attribute(record, "actual.agent.id"), "agent-1");
                assert_eq!(attribute(record, "actual.action.type"), "plan-check");
                assert_eq!(attribute(record, "actual.action.mode"), "hook");
                assert_eq!(
                    attribute(record, "actual.action.id"),
                    attribute(&records[1], "actual.action.id")
                );
            }
            assert_eq!(attribute(completed, "actual.action.outcome"), "block");
            let took = duration_ms(completed);
            assert!((250..60_000).contains(&took), "{took}");
            assert!(!home.join("telemetry-id").exists());
        });
    }

    /// Each decision reaches the completed record in the plan-governance
    /// events' words, with the warn reasons on a `warn` only, and a direct run
    /// sends its session and agent empty.
    #[test]
    fn test_a_direct_run_reports_each_outcome_with_no_session() {
        with_config(WITH_FILE, |_| {
            let reasons = [WarnReason::RequiresDecision, WarnReason::PartialCoverage];
            for (decision, outcome, sent) in [
                (PlanGovernanceDecision::Allow, "allow", json!([])),
                (
                    PlanGovernanceDecision::Warn,
                    "warn",
                    json!(["requires_decision", "partial_coverage"]),
                ),
                (PlanGovernanceDecision::Block, "block", json!([])),
            ] {
                let action = Action {
                    kind: ArtifactKind::Diff,
                    hook: false,
                    session_id: None,
                    agent_id: None,
                    started: Started::now(),
                };

                report(&action, decision, &reasons, Vec::new());

                let records = take_captured();
                assert_eq!(records.len(), 2, "{outcome}");
                for record in &records {
                    assert_eq!(attribute(record, "actual.session.id"), "");
                    assert_eq!(attribute(record, "actual.agent.id"), "");
                    assert_eq!(attribute(record, "actual.action.type"), "impl-check");
                    assert_eq!(attribute(record, "actual.action.mode"), "direct");
                }
                assert_eq!(attribute(&records[1], "actual.action.outcome"), outcome);
                assert_eq!(
                    attribute(&records[1], "actual.action.warn_reasons"),
                    sent,
                    "{outcome}"
                );
            }
        });
    }

    /// A direct run warns for a rule that needs a decision and for a judge
    /// that saw only some of the rules, naming both when both hold. A
    /// conflict is a block, never a warn reason.
    #[test]
    fn test_a_direct_run_warns_for_a_decision_and_for_partial_coverage() {
        let decision = judged("signing-a1", Verdict::RequiresDecision);
        let conforming = judged("signing-a2", Verdict::Conforming);
        let conflicting = judged("rotation", Verdict::Conflicting);
        for (verdicts, partial, expected) in [
            (vec![conforming.clone()], false, vec![]),
            (vec![conflicting], false, vec![]),
            (
                vec![conforming.clone(), decision.clone()],
                false,
                vec![WarnReason::RequiresDecision],
            ),
            (
                vec![conforming.clone()],
                true,
                vec![WarnReason::PartialCoverage],
            ),
            (
                vec![decision, conforming],
                true,
                vec![WarnReason::RequiresDecision, WarnReason::PartialCoverage],
            ),
        ] {
            assert_eq!(direct_warn_reasons(&verdicts, partial), expected);
        }
    }

    /// A hook round reports the run its `HookRun` describes: its kind, its
    /// session and agent, and when it started, beside the outcome and warn
    /// reasons it was given.
    #[test]
    fn test_a_round_reports_the_run_it_describes() {
        with_config(WITH_FILE, |home| {
            let rules_dir = home.join("rules");
            let run = HookRun {
                kind: ArtifactKind::Diff,
                command: "impl-check --claude-hook",
                session_id: Some("s2"),
                agent_id: None,
                artifact_digest: "digest-1",
                root: home,
                rules_dir: &rules_dir,
                max_rounds: 3,
                started: Started {
                    time: std::time::UNIX_EPOCH + std::time::Duration::from_secs(7),
                    instant: Instant::now(),
                },
            };

            report_round(
                &run,
                PlanGovernanceDecision::Warn,
                &[WarnReason::RoundLimit],
                Vec::new(),
            );

            let records = take_captured();
            assert_eq!(records[0]["timeUnixNano"], "7000000000");
            assert_eq!(attribute(&records[0], "actual.session.id"), "s2");
            assert_eq!(attribute(&records[1], "actual.agent.id"), "");
            assert_eq!(attribute(&records[1], "actual.action.type"), "impl-check");
            assert_eq!(attribute(&records[1], "actual.action.mode"), "hook");
            assert_eq!(attribute(&records[1], "actual.action.outcome"), "warn");
            assert_eq!(
                attribute(&records[1], "actual.action.warn_reasons"),
                json!(["round_limit"])
            );
        });
    }

    /// No output set, or opted out by config or env: a run builds no action
    /// record either.
    #[test]
    fn test_a_run_unconfigured_or_opted_out_reports_nothing() {
        let opted_out = format!("{WITH_FILE}  enabled: false\n");
        for config in ["", opted_out.as_str()] {
            with_config(config, |home| {
                report(
                    &hook_action(SystemTime::now()),
                    PlanGovernanceDecision::Allow,
                    &[],
                    Vec::new(),
                );

                assert!(take_captured().is_empty());
                assert!(!home.join("telemetry-id").exists());
            });
        }
        with_config(WITH_FILE, |_| {
            let _no = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");

            report(
                &hook_action(SystemTime::now()),
                PlanGovernanceDecision::Allow,
                &[],
                Vec::new(),
            );

            assert!(take_captured().is_empty());
        });
    }

    /// How many action records one run's report builds with `config` in the
    /// config file. `ACTUAL_OTLP_FILE` names an output, so the config alone
    /// decides whether anything is sent.
    fn report_one_run_under(config: &str) -> usize {
        let mut built = 0;
        with_config(config, |home| {
            let sink = home.join("otlp.jsonl");
            let _file = EnvGuard::set("ACTUAL_OTLP_FILE", sink.to_str().unwrap());
            report(
                &hook_action(SystemTime::now()),
                PlanGovernanceDecision::Allow,
                &[],
                Vec::new(),
            );
            built = take_captured().len();
        });
        built
    }

    /// `telemetry.enabled: false` in a config file that doesn't load: no
    /// action record is built.
    #[test]
    fn test_a_run_reports_nothing_when_the_config_cannot_load() {
        assert_eq!(
            report_one_run_under(crate::testutil::UNLOADABLE_TELEMETRY_OPT_OUT),
            0
        );
    }

    /// The positive control: a config that loads sends the started and
    /// completed records.
    #[test]
    fn test_a_run_reports_when_the_config_loads() {
        assert_eq!(
            report_one_run_under(crate::testutil::LOADABLE_TELEMETRY_ON),
            2
        );
    }
}
