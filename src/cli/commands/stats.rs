//! `actual stats`: how ADR enforcement went on this machine over a period.
//!
//! Reads two local logs and nothing else: the enforcement log
//! ([`enforcement_log`]), one line per gate round from a hook or a direct
//! run, and the override log ([`governance_session::read_audit`]), one line
//! per override, round-limit pass or partially judged round. No network call
//! is made and nothing is sent anywhere, so it works signed out and with
//! telemetry off.
//!
//! A round counts as passed when it did not block. An ADR's check passed
//! when no rule of it was judged conflicting in that round.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::enforcement_log::{self, Decision, Gate, Mode, Round};
use super::governance_session::{self, AuditEntry, AuditKind, OverrideReasonCode};
use super::rules_brief;
use crate::cli::args::{Since, StatsArgs};
use crate::error::ActualError;
use crate::rules::check::Verdict;

/// Written when an override carried no reason.
const NO_REASON: &str = "none";

#[derive(Debug, Default, PartialEq, Serialize)]
struct Rounds {
    /// Rounds the judge ran in; every count below except `settled` is a
    /// share of these.
    total: usize,
    /// Lines with no ADR judged: a hook call with every applicable rule
    /// already cleared or overridden, or a run where no rule applied.
    settled: usize,
    plan: usize,
    #[serde(rename = "impl")]
    impl_: usize,
    hook: usize,
    direct: usize,
    allowed: usize,
    warned: usize,
    blocked: usize,
    pass_rate: Option<f64>,
}

#[derive(Debug, Default, PartialEq, Serialize)]
struct AdrStats {
    adr: String,
    checks: usize,
    followed: usize,
    not_applicable: usize,
    breaches: usize,
    decisions_needed: usize,
    overrides: usize,
    round_limits: usize,
    pass_rate: Option<f64>,
}

#[derive(Debug, Default, PartialEq, Serialize)]
struct Overrides {
    total: usize,
    by_reason: BTreeMap<&'static str, usize>,
}

#[derive(Debug, PartialEq, Serialize)]
struct Report {
    since: Option<DateTime<Utc>>,
    /// Set when the round log reaches back less far than the overrides in
    /// the period: the oldest round it still holds.
    rounds_since: Option<DateTime<Utc>>,
    repo: Option<String>,
    log_enabled: bool,
    rounds: Rounds,
    adrs: Vec<AdrStats>,
    overrides: Overrides,
    round_limits: usize,
    partial_coverage: usize,
}

pub fn exec(args: &StatsArgs) -> Result<(), ActualError> {
    let since = match args.since {
        Since::All => None,
        Since::Last(delta) => Utc::now().checked_sub_signed(delta),
    };
    let repo = args.repo.as_deref().map(rules_brief::resolve);
    let report = build(
        &enforcement_log::read(None),
        &governance_session::read_audit(since),
        since,
        repo.as_deref(),
        enforcement_log::enabled(),
    );
    println!("{}", render(&report, args.json));
    Ok(())
}

fn in_repo(rules_dir: &str, repo: Option<&Path>) -> bool {
    repo.is_none_or(|repo| rules_brief::resolve(Path::new(rules_dir)).starts_with(repo))
}

fn rate(passed: usize, total: usize) -> Option<f64> {
    (total > 0).then(|| passed as f64 / total as f64)
}

/// `rounds` is the whole round log, unfiltered, so the oldest round in it
/// says how far back the round counts can reach.
fn build(
    rounds: &[Round],
    audit: &[AuditEntry],
    since: Option<DateTime<Utc>>,
    repo: Option<&Path>,
    log_enabled: bool,
) -> Report {
    let held_from = rounds.iter().map(|r| r.at).min();
    let rounds: Vec<&Round> = rounds
        .iter()
        .filter(|r| since.is_none_or(|since| r.at >= since) && in_repo(&r.rules_dir, repo))
        .collect();
    let audit: Vec<&AuditEntry> = audit
        .iter()
        .filter(|e| in_repo(&e.rules_dir, repo))
        .collect();
    let rounds_since = held_from.filter(|from| audit.iter().any(|e| e.at < *from));

    let mut totals = Rounds::default();
    let mut adrs: BTreeMap<String, AdrStats> = BTreeMap::new();
    for round in &rounds {
        if round.adrs.is_empty() {
            totals.settled += 1;
            continue;
        }
        totals.total += 1;
        *match round.gate {
            Gate::Plan => &mut totals.plan,
            Gate::Impl => &mut totals.impl_,
        } += 1;
        *match round.mode {
            Mode::Hook => &mut totals.hook,
            Mode::Direct => &mut totals.direct,
        } += 1;
        *match round.decision {
            Decision::Allow => &mut totals.allowed,
            Decision::Warn => &mut totals.warned,
            Decision::Block => &mut totals.blocked,
        } += 1;
        for judged in &round.adrs {
            let stats = adrs.entry(judged.adr.clone()).or_default();
            stats.checks += 1;
            *match judged.verdict {
                Verdict::Followed => &mut stats.followed,
                Verdict::NotApplicable => &mut stats.not_applicable,
                Verdict::Conflicting => &mut stats.breaches,
                Verdict::RequiresDecision => &mut stats.decisions_needed,
            } += 1;
        }
    }
    totals.pass_rate = rate(totals.total - totals.blocked, totals.total);

    let mut overrides = Overrides::default();
    let mut round_limits = 0;
    let mut partial_coverage = 0;
    for entry in &audit {
        let adr = entry.key.split("::").next().unwrap_or_default();
        match entry.kind {
            AuditKind::Override => {
                overrides.total += 1;
                let code = entry
                    .reason_code
                    .or_else(|| OverrideReasonCode::split_reason(entry.reason.as_deref()).0);
                *overrides
                    .by_reason
                    .entry(code.map_or(NO_REASON, OverrideReasonCode::as_str))
                    .or_default() += 1;
                adrs.entry(adr.to_string()).or_default().overrides += 1;
            }
            AuditKind::RoundLimit => {
                round_limits += 1;
                adrs.entry(adr.to_string()).or_default().round_limits += 1;
            }
            AuditKind::PartialCoverage => partial_coverage += 1,
        }
    }

    let mut adrs: Vec<AdrStats> = adrs
        .into_iter()
        .map(|(adr, stats)| AdrStats {
            pass_rate: rate(stats.checks - stats.breaches, stats.checks),
            adr,
            ..stats
        })
        .collect();
    adrs.sort_by(|a, b| {
        (b.breaches, b.overrides, b.checks)
            .cmp(&(a.breaches, a.overrides, a.checks))
            .then_with(|| a.adr.cmp(&b.adr))
    });

    Report {
        since,
        rounds_since,
        repo: repo.map(|repo| repo.display().to_string()),
        log_enabled,
        rounds: totals,
        adrs,
        overrides,
        round_limits,
        partial_coverage,
    }
}

fn percent(rate: Option<f64>) -> String {
    rate.map_or_else(|| "-".to_string(), |rate| format!("{:.0}%", rate * 100.0))
}

fn render(report: &Report, json: bool) -> String {
    if json {
        return serde_json::to_string_pretty(report)
            .expect("stats report is serializable — this is a programmer error");
    }
    let period = report.since.map_or_else(
        || "ADR enforcement, everything recorded".to_string(),
        |since| {
            format!(
                "ADR enforcement since {}",
                since.format("%Y-%m-%d %H:%M UTC")
            )
        },
    );
    let scope = report
        .repo
        .as_deref()
        .map_or_else(String::new, |repo| format!(" in {repo}"));
    let mut out = vec![format!("{period}{scope}")];
    if let Some(from) = report.rounds_since {
        out.push(format!(
            "Gate rounds go back only to {}, the oldest the round log still holds; overrides cover the whole period.",
            from.format("%Y-%m-%d %H:%M UTC")
        ));
    }
    if !report.log_enabled {
        out.push(format!(
            "The enforcement log is off ({}=off), so new rounds are not recorded.",
            enforcement_log::OFF_SWITCH
        ));
    }
    let r = &report.rounds;
    if r.total == 0 && r.settled == 0 && report.adrs.is_empty() && report.partial_coverage == 0 {
        out.push("No gate rounds or overrides recorded in this period.".to_string());
        return out.join("\n");
    }
    if r.total == 0 {
        out.push("No gate rounds judged in this period.".to_string());
    } else {
        out.push(format!(
            "Gate rounds: {} ({} plan, {} impl · {} from hooks, {} direct)",
            r.total, r.plan, r.impl_, r.hook, r.direct
        ));
        out.push(format!(
            "Blocked {}, warned {}, allowed {} · {} passed",
            r.blocked,
            r.warned,
            r.allowed,
            percent(r.pass_rate)
        ));
    }
    if !report.adrs.is_empty() {
        let width = report
            .adrs
            .iter()
            .map(|a| a.adr.len())
            .max()
            .unwrap_or(0)
            .max(3);
        out.push(String::new());
        out.push(format!(
            "{:<width$}  {:>6}  {:>8}  {:>9}  {:>9}  {:>6}",
            "ADR", "Checks", "Breaches", "Decisions", "Overrides", "Passed"
        ));
        for a in &report.adrs {
            out.push(format!(
                "{:<width$}  {:>6}  {:>8}  {:>9}  {:>9}  {:>6}",
                a.adr,
                a.checks,
                a.breaches,
                a.decisions_needed,
                a.overrides,
                percent(a.pass_rate)
            ));
        }
    }
    let mut notes = Vec::new();
    if r.settled > 0 {
        notes.push(format!(
            "Checks with nothing to judge: {} (no rule applied, or every one was already cleared)",
            r.settled
        ));
    }
    if report.overrides.total > 0 {
        let reasons: Vec<String> = report
            .overrides
            .by_reason
            .iter()
            .map(|(reason, count)| format!("{reason} {count}"))
            .collect();
        notes.push(format!(
            "Overrides: {} ({})",
            report.overrides.total,
            reasons.join(", ")
        ));
    }
    if report.round_limits > 0 {
        notes.push(format!(
            "Rules let through at the round limit: {}",
            report.round_limits
        ));
    }
    if report.partial_coverage > 0 {
        notes.push(format!(
            "Rounds that judged only part of the rules that applied: {}",
            report.partial_coverage
        ));
    }
    if !notes.is_empty() {
        out.push(String::new());
        out.extend(notes);
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::commands::enforcement_log::{record, OFF_SWITCH};
    use crate::rules::check::CheckedRule;
    use crate::rules::types::RuleLevel;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use chrono::TimeDelta;
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// The rules directory `--repo` names, for rounds written under it.
    fn repo_rules_dir(repo: &Path) -> PathBuf {
        crate::rules::rules_dir(&rules_brief::resolve(repo))
    }

    fn checked(doc_slug: &str, verdict: Verdict) -> CheckedRule {
        CheckedRule {
            doc_slug: doc_slug.to_string(),
            rule_id: "R-001".to_string(),
            level: RuleLevel::Must,
            statement: "statement".to_string(),
            verdict,
            span: String::new(),
            reason: String::new(),
        }
    }

    fn round(
        gate: Gate,
        mode: Mode,
        rules_dir: &Path,
        decision: Decision,
        adrs: &[(&str, Verdict)],
    ) -> Round {
        let verdicts: Vec<CheckedRule> = adrs.iter().map(|(adr, v)| checked(adr, *v)).collect();
        Round::new(gate, mode, rules_dir, decision, &verdicts)
    }

    fn audit(kind: &str, rules_dir: &Path, key: &str, extra: serde_json::Value) -> AuditEntry {
        let mut value = serde_json::json!({
            "at": Utc::now(),
            "session_id": "s",
            "rules_dir": rules_dir.display().to_string(),
            "key": key,
            "reason": null,
            "round": 1,
            "kind": kind,
        });
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn test_counts_rounds_per_gate_mode_and_decision() {
        let dir = Path::new("/repo/.actual/rules");
        let rounds = [
            round(
                Gate::Plan,
                Mode::Hook,
                dir,
                Decision::Block,
                &[("adr-a", Verdict::Conflicting)],
            ),
            round(
                Gate::Plan,
                Mode::Hook,
                dir,
                Decision::Allow,
                &[("adr-a", Verdict::Followed)],
            ),
            round(
                Gate::Impl,
                Mode::Direct,
                dir,
                Decision::Warn,
                &[("adr-b", Verdict::RequiresDecision)],
            ),
            round(
                Gate::Impl,
                Mode::Hook,
                dir,
                Decision::Allow,
                &[("adr-b", Verdict::NotApplicable)],
            ),
        ];
        let report = build(&rounds, &[], None, None, true);

        assert_eq!(
            report.rounds,
            Rounds {
                total: 4,
                settled: 0,
                plan: 2,
                impl_: 2,
                hook: 3,
                direct: 1,
                allowed: 2,
                warned: 1,
                blocked: 1,
                pass_rate: Some(0.75),
            }
        );
        let names: Vec<&str> = report.adrs.iter().map(|a| a.adr.as_str()).collect();
        assert_eq!(names, ["adr-a", "adr-b"]);
        assert_eq!(
            report.adrs[0],
            AdrStats {
                adr: "adr-a".to_string(),
                checks: 2,
                followed: 1,
                breaches: 1,
                pass_rate: Some(0.5),
                ..AdrStats::default()
            }
        );
        assert_eq!(
            report.adrs[1],
            AdrStats {
                adr: "adr-b".to_string(),
                checks: 2,
                not_applicable: 1,
                decisions_needed: 1,
                pass_rate: Some(1.0),
                ..AdrStats::default()
            }
        );
    }

    #[test]
    fn test_counts_overrides_by_reason_and_per_adr() {
        let dir = Path::new("/repo/.actual/rules");
        let entries = [
            audit(
                "override",
                dir,
                "adr-a::R-001",
                serde_json::json!({"reason_code": "misread"}),
            ),
            audit(
                "override",
                dir,
                "adr-a::R-002",
                serde_json::json!({"reason": "accepted shipping it"}),
            ),
            audit(
                "override",
                dir,
                "adr-b::R-001",
                serde_json::json!({"reason": "just because"}),
            ),
            audit("override", dir, "adr-b::R-002", serde_json::json!({})),
            audit("round_limit", dir, "adr-c::R-001", serde_json::json!({})),
            audit(
                "partial_coverage",
                dir,
                "",
                serde_json::json!({"judged": 40, "total": 50}),
            ),
        ];
        let report = build(&[], &entries, None, None, true);

        let by_reason: Vec<(&str, usize)> = report
            .overrides
            .by_reason
            .iter()
            .map(|(r, c)| (*r, *c))
            .collect();
        assert_eq!(report.overrides.total, 4);
        assert_eq!(
            by_reason,
            [("accepted", 1), ("misread", 1), ("none", 1), ("other", 1)]
        );
        assert_eq!((report.round_limits, report.partial_coverage), (1, 1));
        let per_adr: Vec<(&str, usize, usize, Option<f64>)> = report
            .adrs
            .iter()
            .map(|a| (a.adr.as_str(), a.overrides, a.round_limits, a.pass_rate))
            .collect();
        assert_eq!(
            per_adr,
            [
                ("adr-a", 2, 0, None),
                ("adr-b", 2, 0, None),
                ("adr-c", 0, 1, None)
            ]
        );
    }

    #[test]
    fn test_repo_keeps_only_its_own_rules_dir() {
        let mine = tempdir().unwrap();
        let theirs = tempdir().unwrap();
        let rounds = [
            round(
                Gate::Plan,
                Mode::Hook,
                &repo_rules_dir(mine.path()),
                Decision::Allow,
                &[("adr-a", Verdict::Followed)],
            ),
            round(
                Gate::Plan,
                Mode::Hook,
                &repo_rules_dir(theirs.path()),
                Decision::Block,
                &[("adr-b", Verdict::Conflicting)],
            ),
        ];
        let entries = [
            audit(
                "override",
                &repo_rules_dir(mine.path()),
                "adr-a::R-001",
                serde_json::json!({}),
            ),
            audit(
                "override",
                &repo_rules_dir(theirs.path()),
                "adr-b::R-001",
                serde_json::json!({}),
            ),
        ];
        let repo = rules_brief::resolve(mine.path());
        let report = build(&rounds, &entries, None, Some(&repo), true);

        assert_eq!((report.rounds.total, report.overrides.total), (1, 1));
        let names: Vec<&str> = report.adrs.iter().map(|a| a.adr.as_str()).collect();
        assert_eq!(names, ["adr-a"]);
        assert_eq!(report.repo, Some(repo.display().to_string()));
    }

    #[test]
    fn test_adrs_sort_by_breaches_then_overrides_then_checks() {
        let dir = Path::new("/r");
        let rounds = [
            round(
                Gate::Plan,
                Mode::Hook,
                dir,
                Decision::Allow,
                &[("quiet", Verdict::Followed), ("busy", Verdict::Followed)],
            ),
            round(
                Gate::Plan,
                Mode::Hook,
                dir,
                Decision::Allow,
                &[
                    ("busy", Verdict::Followed),
                    ("broken", Verdict::Conflicting),
                ],
            ),
        ];
        let entries = [audit(
            "override",
            dir,
            "waved::R-001",
            serde_json::json!({}),
        )];
        let report = build(&rounds, &entries, None, None, true);

        let names: Vec<&str> = report.adrs.iter().map(|a| a.adr.as_str()).collect();
        assert_eq!(names, ["broken", "waved", "busy", "quiet"]);
    }

    #[test]
    fn test_render_plain_report() {
        let dir = Path::new("/r");
        let rounds = [
            round(
                Gate::Plan,
                Mode::Hook,
                dir,
                Decision::Block,
                &[("adr-a", Verdict::Conflicting)],
            ),
            round(
                Gate::Impl,
                Mode::Direct,
                dir,
                Decision::Allow,
                &[("adr-a", Verdict::Followed)],
            ),
        ];
        let entries = [
            audit(
                "override",
                dir,
                "adr-a::R-001",
                serde_json::json!({"reason_code": "accepted"}),
            ),
            audit("round_limit", dir, "adr-a::R-001", serde_json::json!({})),
            audit("partial_coverage", dir, "", serde_json::json!({})),
        ];
        let since = "2026-11-16T09:30:00Z".parse::<DateTime<Utc>>().unwrap();
        let text = render(&build(&rounds, &entries, Some(since), None, true), false);

        let expected = [
            "ADR enforcement since 2026-11-16 09:30 UTC",
            "Gate rounds: 2 (1 plan, 1 impl · 1 from hooks, 1 direct)",
            "Blocked 1, warned 0, allowed 1 · 50% passed",
            "",
            "ADR    Checks  Breaches  Decisions  Overrides  Passed",
            "adr-a       2         1          0          1     50%",
            "",
            "Overrides: 1 (accepted 1)",
            "Rules let through at the round limit: 1",
            "Rounds that judged only part of the rules that applied: 1",
        ];
        assert_eq!(text, expected.join("\n"));
    }

    #[test]
    fn test_render_empty_period_and_log_off() {
        let report = build(&[], &[], None, Some(Path::new("/repo")), false);
        let text = render(&report, false);

        let expected = [
            "ADR enforcement, everything recorded in /repo",
            "The enforcement log is off (ACTUAL_ENFORCEMENT_LOG=off), so new rounds are not recorded.",
            "No gate rounds or overrides recorded in this period.",
        ];
        assert_eq!(text, expected.join("\n"));
    }

    #[test]
    fn test_render_overrides_without_rounds() {
        let entries = [audit(
            "override",
            Path::new("/r"),
            "adr-a::R-001",
            serde_json::json!({}),
        )];
        let text = render(&build(&[], &entries, None, None, true), false);

        let expected = [
            "ADR enforcement, everything recorded",
            "No gate rounds judged in this period.",
            "",
            "ADR    Checks  Breaches  Decisions  Overrides  Passed",
            "adr-a       0         0          0          1       -",
            "",
            "Overrides: 1 (none 1)",
        ];
        assert_eq!(text, expected.join("\n"));
    }

    /// A line with no ADR judged is a settled check: it stays out of the
    /// round counts and the pass rate, and gets its own line.
    #[test]
    fn test_settled_checks_stay_out_of_rounds_and_pass_rate() {
        let dir = Path::new("/r");
        let rounds = [
            round(
                Gate::Plan,
                Mode::Hook,
                dir,
                Decision::Block,
                &[("adr-a", Verdict::Conflicting)],
            ),
            round(Gate::Plan, Mode::Hook, dir, Decision::Allow, &[]),
            round(Gate::Impl, Mode::Direct, dir, Decision::Warn, &[]),
        ];
        let report = build(&rounds, &[], None, None, true);

        assert_eq!(
            (
                report.rounds.total,
                report.rounds.settled,
                report.rounds.hook
            ),
            (1, 2, 1)
        );
        assert_eq!(
            (
                report.rounds.allowed,
                report.rounds.warned,
                report.rounds.direct
            ),
            (0, 0, 0)
        );
        assert_eq!(report.rounds.pass_rate, Some(0.0));
        let text = render(&report, false);
        assert!(text.contains("Gate rounds: 1 (1 plan, 0 impl"), "{text}");
        assert!(
            text.ends_with("Checks with nothing to judge: 2 (no rule applied, or every one was already cleared)"),
            "{text}"
        );
    }

    #[test]
    fn test_render_only_settled_checks() {
        let rounds = [round(
            Gate::Plan,
            Mode::Hook,
            Path::new("/r"),
            Decision::Allow,
            &[],
        )];
        let text = render(&build(&rounds, &[], None, None, true), false);

        let expected = [
            "ADR enforcement, everything recorded",
            "No gate rounds judged in this period.",
            "",
            "Checks with nothing to judge: 1 (no rule applied, or every one was already cleared)",
        ];
        assert_eq!(text, expected.join("\n"));
    }

    /// The round log is pruned and the override log is not, so a report
    /// whose overrides reach back past the oldest round says so.
    #[test]
    fn test_says_when_rounds_reach_back_less_far_than_overrides() {
        let dir = Path::new("/r");
        let at = |days: i64| Utc::now() - TimeDelta::days(days);
        let mut dropped = round(
            Gate::Plan,
            Mode::Hook,
            dir,
            Decision::Block,
            &[("adr-a", Verdict::Conflicting)],
        );
        dropped.at = at(20);
        let mut kept = round(
            Gate::Plan,
            Mode::Hook,
            dir,
            Decision::Allow,
            &[("adr-a", Verdict::Followed)],
        );
        kept.at = at(5);
        let recent = audit(
            "override",
            dir,
            "adr-a::R-001",
            serde_json::json!({"at": at(1)}),
        );
        let old = audit(
            "override",
            dir,
            "adr-a::R-001",
            serde_json::json!({"at": at(30)}),
        );

        let report = build(
            std::slice::from_ref(&kept),
            std::slice::from_ref(&recent),
            None,
            None,
            true,
        );
        assert_eq!(report.rounds_since, None, "no override predates the rounds");

        let report = build(
            std::slice::from_ref(&kept),
            &[recent.clone(), old],
            None,
            None,
            true,
        );
        assert_eq!(report.rounds_since, Some(kept.at));
        let text = render(&report, false);
        let note = format!(
            "Gate rounds go back only to {}, the oldest the round log still holds; overrides cover the whole period.",
            kept.at.format("%Y-%m-%d %H:%M UTC")
        );
        assert_eq!(text.lines().nth(1), Some(note.as_str()), "{text}");

        let report = build(&[dropped, kept], &[recent], Some(at(7)), None, true);
        assert_eq!((report.rounds.total, report.rounds.blocked), (1, 0));
        assert_eq!(
            report.rounds_since, None,
            "the round log reaches back past the period"
        );
    }

    #[test]
    fn test_render_json() {
        let rounds = [round(
            Gate::Impl,
            Mode::Hook,
            Path::new("/r"),
            Decision::Block,
            &[("adr-a", Verdict::Conflicting)],
        )];
        let value: serde_json::Value =
            serde_json::from_str(&render(&build(&rounds, &[], None, None, true), true)).unwrap();

        assert_eq!(value["rounds"]["impl"], 1);
        assert_eq!(value["rounds"]["pass_rate"], 0.0);
        assert_eq!(value["adrs"][0]["breaches"], 1);
        assert_eq!(value["since"], serde_json::Value::Null);
        assert_eq!(value["rounds_since"], serde_json::Value::Null);
        assert_eq!(value["log_enabled"], true);
    }

    #[test]
    fn test_exec_reads_both_logs_and_honours_since() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = [
            EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap()),
            EnvGuard::remove("ACTUAL_CONFIG"),
            EnvGuard::remove(OFF_SWITCH),
        ];
        let dir = Path::new("/repo/.actual/rules");
        let mut old = round(
            Gate::Plan,
            Mode::Hook,
            dir,
            Decision::Block,
            &[("adr-a", Verdict::Conflicting)],
        );
        old.at = Utc::now() - TimeDelta::days(10);
        record(&old);
        record(&round(
            Gate::Plan,
            Mode::Hook,
            dir,
            Decision::Allow,
            &[("adr-a", Verdict::Followed)],
        ));
        governance_session::record_override("s", dir, &["adr-a::R-001".to_string()], "misread");

        let since = Some(Utc::now() - TimeDelta::days(7));
        let report = build(
            &enforcement_log::read(None),
            &governance_session::read_audit(since),
            since,
            None,
            enforcement_log::enabled(),
        );
        assert_eq!((report.rounds.total, report.rounds.blocked), (1, 0));
        assert_eq!(report.overrides.total, 1);
        assert_eq!(
            report.adrs[0].adr, "adr-a",
            "the override and the round land on the same ADR"
        );
        assert_eq!(report.adrs.len(), 1);
        assert_eq!(report.rounds_since, None);

        let args = StatsArgs {
            since: Since::All,
            repo: None,
            json: true,
        };
        assert!(exec(&args).is_ok());
        let args = StatsArgs {
            since: Since::Last(TimeDelta::days(7)),
            repo: Some(PathBuf::from("/repo")),
            json: false,
        };
        assert!(exec(&args).is_ok());
    }

    #[test]
    fn test_unparseable_audit_lines_are_skipped() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = [
            EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap()),
            EnvGuard::remove("ACTUAL_CONFIG"),
        ];
        let dir = Path::new("/r");
        governance_session::record_override("s", dir, &["a::R-1".to_string()], "");
        let path = governance_session::audit_log_path().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"not json\n{\"torn\xff\n");
        std::fs::write(&path, bytes).unwrap();
        governance_session::record_override("s", dir, &["a::R-2".to_string()], "");

        let keys: Vec<String> = governance_session::read_audit(None)
            .into_iter()
            .map(|e| e.key)
            .collect();
        assert_eq!(keys, ["a::R-1", "a::R-2"]);
        assert!(governance_session::read_audit(Some(Utc::now() + TimeDelta::days(1))).is_empty());
    }
}
