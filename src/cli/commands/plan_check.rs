//! `actual plan-check` — evaluate an implementation plan against the rule
//! documents that govern it.
//!
//! # Design
//!
//! Two callers, one pipeline, two very different contracts for what happens
//! with the answer.
//!
//! A human runs this directly, the way they'd run `rules select`: a plan in,
//! a panel or `--json` out, and a non-zero exit when a rule is actually
//! violated — ordinary linter behavior.
//!
//! `hooks/plan-gate.sh` runs it as `--claude-hook`, piping a Claude Code
//! `PreToolUse` envelope on stdin, and the contract there is fixed by
//! `skills/actual/SKILL.md` in the `actual-skill` plugin repository: a
//! conforming plan prints **nothing** and exits 0 so the user's approval
//! dialog is untouched; a genuine violation prints exactly one JSON object
//! naming the rule id and the conflicting span; and every other outcome —
//! no plan resolvable, no rules, no runner, a crashed judge call — fails
//! open. `--claude-hook` never returns `Err` from [`exec`]: every fallible
//! step is caught and turned into a fail-open notice, the same invariant
//! `crate::rules::discover` documents for its per-file loop. `permission
//! Decision: "allow"` is never emitted anywhere in this module or in
//! [`plan_check_hook`] — there is no function that produces it — because a
//! gate has no business granting the approval it is supposed to be checking.
//!
//! [`run_pipeline`] is the shared core both callers drive: resolve the rules
//! directory, select the documents that apply, gather their individual
//! rules, and hand the whole batch to [`crate::rules::check`] in one call.
//!
//! **Selection's stage 2 is a hook-only restriction, not a blanket one.**
//! `rules select`'s stage 2 spends a model call improving *which documents*
//! are chosen. `--claude-hook` skips it unconditionally and keeps the
//! deterministic prefilter's answer, because that call has exactly one model
//! call to spend inside Claude Code's 120-second `PreToolUse` timeout, and it
//! belongs to the conformance judge — running both risks the judge never
//! getting its turn, which is a worse failure than an imprecise selection
//! (see AK-734). Direct mode has no such deadline, so it runs stage 2 by
//! default, the same way `rules select` does, with `--no-rank` opting out.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::cli::args::PlanCheckArgs;
use crate::cli::commands::plan_check_hook::{self, HookEnvelope};
use crate::cli::commands::rules_rank;
use crate::cli::ui::panel::Panel;
use crate::cli::ui::term_size;
use crate::error::ActualError;
use crate::rules::check::{self, CheckedRule, RuleForJudging, Verdict};
use crate::rules::scope::{self, select, Query, Selection, Stage2};

/// Safety cap on how many individual rules are judged in one call. The
/// prefilter already caps *documents*; a single selected document can still
/// hold far more rules than one structured-output call can weigh usefully, so
/// this bounds the prompt without changing which documents were selected.
///
/// Exceeding it is treated as a failure to check, never as license to judge a
/// partial set: [`gather_rules`] reports how many rules it actually found, and
/// [`run_pipeline`] refuses to call the judge at all when that exceeds the
/// cap, rather than silently sending the first [`MAX_RULES_JUDGED`] and
/// calling whatever came back a complete answer.
const MAX_RULES_JUDGED: usize = 60;

fn repo_root(explicit: Option<&PathBuf>) -> PathBuf {
    explicit
        .cloned()
        .unwrap_or_else(crate::cli::commands::sync::resolve_cwd)
}

pub fn exec(args: &PlanCheckArgs) -> Result<(), ActualError> {
    if args.claude_hook {
        exec_hook(args);
        return Ok(());
    }
    exec_direct(args)
}

// ── the shared pipeline ──────────────────────────────────────────────────

/// What the pipeline produced, before either caller decides what to do with
/// it. Every variant except [`Outcome::Verdicts`] is a "could not check"
/// state rather than a finding — direct mode surfaces these as informational
/// output with a clean exit; `--claude-hook` treats every one of them as
/// fail-open. The non-`Verdicts` variants carry how many documents were under
/// consideration when the pipeline stopped, so neither caller has to report a
/// misleading zero for a failure that happened after selection ran.
enum Outcome {
    /// No committed rule document applied to this plan, or the rules
    /// directory holds none. Not a failure — most plans do not touch every
    /// rule in the corpus.
    NothingApplies,
    /// No backend was available to run the judge.
    NoRunner {
        documents_selected: usize,
        reason: String,
    },
    /// The judge could not be used: the call itself failed (timeout,
    /// malformed or incomplete output), or the selected documents held more
    /// rules than [`MAX_RULES_JUDGED`] and the judge was never called at all
    /// rather than being shown a silently truncated set.
    CheckFailed {
        documents_selected: usize,
        reason: String,
    },
    /// The judge ran and produced verdicts for every selected rule.
    Verdicts {
        selection: Selection,
        verdicts: Vec<CheckedRule>,
        runner_label: Option<String>,
    },
}

/// Run the whole pipeline: resolve the index, select documents, gather their
/// rules, resolve a runner, and judge.
///
/// `use_rank` gates selection's stage 2. `--claude-hook` always passes
/// `false`, unconditionally, regardless of any flag — see the module doc for
/// why. Direct mode passes `!args.no_rank`.
///
/// Returns `Err` only when the rules directory itself could not be read at
/// all — the one condition serious enough that a direct-mode caller should
/// see a real error. `--claude-hook` still catches that `Err` and fails open
/// on it, per the "missing rules directory... must not deny" contract.
fn run_pipeline(
    plan_text: &str,
    root: &Path,
    rules_dir: &Path,
    args: &PlanCheckArgs,
    use_rank: bool,
) -> Result<Outcome, ActualError> {
    let resolved = scope::resolve_in(rules_dir, root, args.rebuild)?;
    let query = Query::new(plan_text.to_string());
    let prefiltered = select::prefilter(&resolved.index, &query, args.limit, args.candidates);

    if prefiltered.is_empty() {
        return Ok(Outcome::NothingApplies);
    }

    // One runner resolution serves both stage 2's rank (when `use_rank`) and
    // the judge call that always follows it — there is never a reason to
    // probe the environment twice for one invocation.
    let cfg = crate::config::paths::load().unwrap_or_default();
    let resolved_runner =
        match rules_rank::resolve(args.runner.as_ref(), args.model.as_deref(), &cfg) {
            Ok(runner) => runner,
            Err(reason) => {
                return Ok(Outcome::NoRunner {
                    documents_selected: prefiltered.len(),
                    reason,
                })
            }
        };
    let label = resolved_runner.label();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ActualError::InternalError(format!("failed to build tokio runtime: {e}")))?;

    let selection = if use_rank {
        runtime.block_on(prefiltered.rank_with(
            &resolved_runner.runner,
            resolved_runner.model.as_deref(),
            resolved_runner.max_budget_usd,
        ))
    } else {
        prefiltered.finish(Stage2::NotRequested)
    };

    if selection.selected.is_empty() {
        return Ok(Outcome::NothingApplies);
    }

    let gathered = gather_rules(&selection, root);
    if gathered.rules.is_empty() {
        return Ok(Outcome::NothingApplies);
    }
    if gathered.truncated {
        return Ok(Outcome::CheckFailed {
            documents_selected: selection.selected.len(),
            reason: format!(
                "the selected documents contain {} rules, over the {MAX_RULES_JUDGED}-rule \
                 judging cap; refusing to judge a partial set",
                gathered.considered
            ),
        });
    }

    match runtime.block_on(check::check(
        &resolved_runner.runner,
        plan_text,
        &gathered.rules,
        resolved_runner.model.as_deref(),
        resolved_runner.max_budget_usd,
    )) {
        Ok(verdicts) => Ok(Outcome::Verdicts {
            selection,
            verdicts,
            runner_label: Some(label),
        }),
        Err(e) => Ok(Outcome::CheckFailed {
            documents_selected: selection.selected.len(),
            reason: e.to_string(),
        }),
    }
}

/// The rules gathered from every selected document, capped at
/// [`MAX_RULES_JUDGED`], and whether the true count exceeded that cap.
struct GatheredRules {
    rules: Vec<RuleForJudging>,
    /// Total individual rules found across every selected document,
    /// including any past the cap. Equal to `rules.len()` unless `truncated`.
    considered: usize,
    /// True when `considered` exceeds [`MAX_RULES_JUDGED`] — some of the
    /// selected documents' rules were never gathered at all. The caller must
    /// treat this as "could not check", not as license to judge `rules` alone
    /// and call the result complete.
    truncated: bool,
}

/// Read the individual rules out of every selected document, in selection
/// order.
///
/// A document that no longer parses (removed, edited to something invalid,
/// between selection and this read) is skipped rather than failing the whole
/// batch — one bad file never costs the rest, the same invariant
/// `crate::rules::discover` enforces on the original scan.
fn gather_rules(selection: &Selection, root: &Path) -> GatheredRules {
    let mut rules = Vec::new();
    let mut considered = 0usize;
    for selected in &selection.selected {
        let path = root.join(&selected.relative_path);
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let Ok(doc) = crate::rules::parse_rule_document(&path, &text) else {
            continue;
        };
        for rule in doc.rules {
            considered += 1;
            if rules.len() < MAX_RULES_JUDGED {
                rules.push(RuleForJudging::new(
                    selected.slug.clone(),
                    rule.id,
                    rule.level,
                    rule.statement,
                ));
            }
        }
    }
    GatheredRules {
        truncated: considered > MAX_RULES_JUDGED,
        rules,
        considered,
    }
}

// ── direct mode ──────────────────────────────────────────────────────────

fn exec_direct(args: &PlanCheckArgs) -> Result<(), ActualError> {
    let plan_text = resolve_direct_plan(args)?;
    let root = repo_root(args.repo.as_ref());
    let rules_dir = args
        .rules_dir
        .clone()
        .unwrap_or_else(|| crate::rules::rules_dir(&root));

    let outcome = run_pipeline(&plan_text, &root, &rules_dir, args, !args.no_rank)?;

    let width = term_size::terminal_width();
    if args.json {
        println!("{}", render_json(&outcome));
    } else {
        println!("{}", render_panel(&outcome, &plan_text, &rules_dir, width));
    }

    if let Outcome::Verdicts { verdicts, .. } = &outcome {
        let conflicts: Vec<&CheckedRule> = verdicts.iter().filter(|v| v.verdict.blocks()).collect();
        if !conflicts.is_empty() {
            return Err(ActualError::PlanNotConforming(deny_summary(&conflicts)));
        }
    }
    Ok(())
}

/// The plan text for direct-mode use: the positional argument, then
/// `--plan-file`, then stdin.
fn resolve_direct_plan(args: &PlanCheckArgs) -> Result<String, ActualError> {
    if !args.plan.is_empty() {
        return Ok(args.plan.join(" "));
    }
    if let Some(path) = &args.plan_file {
        let text = std::fs::read_to_string(path).map_err(ActualError::IoError)?;
        if text.trim().is_empty() {
            return Err(ActualError::ConfigError(format!(
                "{} is empty",
                path.display()
            )));
        }
        return Ok(text);
    }
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(ActualError::IoError)?;
    if text.trim().is_empty() {
        return Err(ActualError::ConfigError(
            "no plan given: pass PLAN, --plan-file, or pipe the plan on stdin".to_string(),
        ));
    }
    Ok(text)
}

fn render_panel(outcome: &Outcome, plan: &str, rules_dir: &Path, width: usize) -> String {
    let mut panel = Panel::titled("Plan check");
    panel = panel.kv("Plan", &truncate(plan, 72));
    panel = panel.kv("Rules dir", &rules_dir.display().to_string());

    match outcome {
        Outcome::NothingApplies => panel
            .separator()
            .line("No committed rule document applies to this plan.")
            .render(width),
        Outcome::NoRunner {
            documents_selected,
            reason,
        } => panel
            .kv("Documents selected", &documents_selected.to_string())
            .separator()
            .line(&format!("Could not check: no runner available ({reason})."))
            .render(width),
        Outcome::CheckFailed {
            documents_selected,
            reason,
        } => panel
            .kv("Documents selected", &documents_selected.to_string())
            .separator()
            .line(&format!("Could not check: {reason}"))
            .render(width),
        Outcome::Verdicts {
            selection,
            verdicts,
            runner_label,
        } => {
            panel = panel.kv("Documents selected", &selection.selected.len().to_string());
            if let Some(label) = runner_label {
                panel = panel.kv("Runner", label);
            }
            panel = panel.kv("Rules checked", &verdicts.len().to_string());
            panel = panel.separator();

            let conflicts: Vec<&CheckedRule> =
                verdicts.iter().filter(|v| v.verdict.blocks()).collect();
            let decisions: Vec<&CheckedRule> = verdicts
                .iter()
                .filter(|v| v.verdict == Verdict::RequiresDecision)
                .collect();

            if conflicts.is_empty() && decisions.is_empty() {
                return panel
                    .line("Conforming: no selected rule was violated.")
                    .render(width);
            }
            for rule in &conflicts {
                panel = render_verdict_line(panel, "CONFLICT", rule);
            }
            for rule in &decisions {
                panel = render_verdict_line(panel, "DECISION", rule);
            }
            panel.render(width)
        }
    }
}

fn render_verdict_line(panel: Panel, label: &str, rule: &CheckedRule) -> Panel {
    let panel = panel.kv(
        label,
        &format!("{} ({})", rule.rule_id, rule.level.as_str()),
    );
    let panel = panel.line(&format!("      {}", truncate(&rule.reason, 68)));
    panel.line(&format!("      \"{}\"", truncate(&rule.span, 68)))
}

#[derive(Serialize)]
struct PlanCheckJson {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runner: Option<String>,
    documents_selected: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    verdicts: Vec<CheckedRule>,
}

fn render_json(outcome: &Outcome) -> String {
    let payload = match outcome {
        Outcome::NothingApplies => PlanCheckJson {
            status: "not_checked",
            detail: Some("no committed rule document applies to this plan".to_string()),
            runner: None,
            documents_selected: 0,
            verdicts: Vec::new(),
        },
        Outcome::NoRunner {
            documents_selected,
            reason,
        } => PlanCheckJson {
            status: "not_checked",
            detail: Some(format!("no runner available: {reason}")),
            runner: None,
            documents_selected: *documents_selected,
            verdicts: Vec::new(),
        },
        Outcome::CheckFailed {
            documents_selected,
            reason,
        } => PlanCheckJson {
            status: "not_checked",
            detail: Some(reason.clone()),
            runner: None,
            documents_selected: *documents_selected,
            verdicts: Vec::new(),
        },
        Outcome::Verdicts {
            selection,
            verdicts,
            runner_label,
        } => {
            let status = if verdicts.iter().any(|v| v.verdict.blocks()) {
                "conflicting"
            } else if verdicts
                .iter()
                .any(|v| v.verdict == Verdict::RequiresDecision)
            {
                "requires_decision"
            } else {
                "conforming"
            };
            PlanCheckJson {
                status,
                detail: None,
                runner: runner_label.clone(),
                documents_selected: selection.selected.len(),
                verdicts: verdicts.clone(),
            }
        }
    };
    serde_json::to_string_pretty(&payload)
        .expect("plan check report is serializable — this is a programmer error")
}

/// The direct-mode error summary: every conflicting rule id, one per line.
fn deny_summary(conflicts: &[&CheckedRule]) -> String {
    conflicts
        .iter()
        .map(|c| {
            format!(
                "{}: {}",
                c.rule_id,
                non_empty_or(&c.reason, "conflicts with the plan")
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

// ── --claude-hook mode ───────────────────────────────────────────────────

/// Run the `--claude-hook` path.
///
/// INVARIANT: [`exec`] always sees this path complete normally — every
/// fallible step below is matched explicitly and turned into a fail-open
/// notice (or, for a real violation, a deny) rather than propagating an
/// error, so this function has no path that reaches an ordinary nonzero
/// exit. The only way [`exec`] could surface one from this path is an actual
/// Rust panic, which this function's own logic never triggers.
fn exec_hook(args: &PlanCheckArgs) {
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        emit(plan_check_hook::render_notice(
            "plan-check could not read the hook payload on stdin",
        ));
        return;
    }

    let envelope: HookEnvelope = match serde_json::from_str(&raw) {
        Ok(envelope) => envelope,
        Err(_) => {
            emit(plan_check_hook::render_notice(
                "plan-check could not parse the hook payload as JSON",
            ));
            return;
        }
    };

    let Some((plan_text, _source)) = plan_check_hook::resolve_plan(&envelope) else {
        emit(plan_check_hook::render_notice(
            "plan-check found no plan text to check (no tool_input.plan, no readable \
             planFilePath, and no plan_mode attachment in the transcript)",
        ));
        return;
    };

    let root = repo_root(args.repo.as_ref());
    let rules_dir = args
        .rules_dir
        .clone()
        .unwrap_or_else(|| crate::rules::rules_dir(&root));

    // `use_rank: false`, unconditionally, regardless of `args.no_rank`: the
    // hook's one model call stays reserved for the judge. See the module doc.
    let outcome = match run_pipeline(&plan_text, &root, &rules_dir, args, false) {
        Ok(outcome) => outcome,
        Err(e) => {
            emit(plan_check_hook::render_notice(&format!(
                "plan-check could not read {}: {e}",
                rules_dir.display()
            )));
            return;
        }
    };

    match outcome {
        Outcome::NothingApplies => {
            emit(plan_check_hook::render_notice(
                "No committed rule under .actual/rules/ applies to this plan.",
            ));
        }
        Outcome::NoRunner { reason, .. } => {
            emit(plan_check_hook::render_notice(&format!(
                "Actual plan governance did not run: no runner available ({reason})."
            )));
        }
        Outcome::CheckFailed { reason, .. } => {
            emit(plan_check_hook::render_notice(&format!(
                "Actual plan governance did not run: {reason}"
            )));
        }
        Outcome::Verdicts { verdicts, .. } => {
            let conflicts: Vec<&CheckedRule> =
                verdicts.iter().filter(|v| v.verdict.blocks()).collect();
            if !conflicts.is_empty() {
                emit(plan_check_hook::render_deny(&hook_deny_reason(&conflicts)));
                return;
            }
            let decisions: Vec<&CheckedRule> = verdicts
                .iter()
                .filter(|v| v.verdict == Verdict::RequiresDecision)
                .collect();
            if !decisions.is_empty() {
                emit(plan_check_hook::render_notice(&requires_decision_message(
                    &decisions,
                )));
            }
            // Fully conforming (no conflicts, no decisions): the contract is
            // silence. No `emit` call.
        }
    }
}

/// Print exactly one line: [`emit`] is the single call site that writes to
/// stdout for `--claude-hook`, so "stdout is exactly one JSON object, or
/// nothing" is enforceable by inspection rather than by discipline.
fn emit(json: String) {
    println!("{json}");
}

/// The deny reason: every conflicting rule id, its reason, and the quoted
/// span, one per line — so a reader (or the agent revising the plan) sees
/// every violation at once rather than only the first.
fn hook_deny_reason(conflicts: &[&CheckedRule]) -> String {
    conflicts
        .iter()
        .map(|c| {
            format!(
                "{} ({}): {} — \"{}\"",
                c.rule_id,
                c.level.as_str(),
                non_empty_or(&c.reason, "conflicts with the plan"),
                truncate(&c.span, 240)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn requires_decision_message(decisions: &[&CheckedRule]) -> String {
    let ids: Vec<&str> = decisions.iter().map(|c| c.rule_id.as_str()).collect();
    format!(
        "This plan appears to deliberately change an established decision ({}), rather than \
         violate it. Not blocked automatically in this MVP — review before proceeding.",
        ids.join(", ")
    )
}

fn non_empty_or<'a>(s: &'a str, fallback: &'a str) -> &'a str {
    if s.trim().is_empty() {
        fallback
    } else {
        s
    }
}

/// Shorten to `width` characters with an ellipsis, counting characters rather
/// than bytes so a multi-byte plan cannot panic.
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::{tempdir, TempDir};

    use crate::rules::types::RuleLevel;

    const OAUTH_DOC: &str = "# Sign With Asymmetric Keys: Token Signing\n\nThese rules are ALWAYS ACTIVE for OAuth token signing in `services/auth/oauth/`.\n\n### Rules\n\n- **R-A-001** MUST: sign with RS256.\n- **R-A-002** MUST NOT: log the raw signing key.\n";
    const TERRAFORM_DOC: &str = "# Pin Providers: Terraform\n\nThese rules are ALWAYS ACTIVE for Terraform configuration in `infra/terraform/`.\n\n### Rules\n\n- **R-B-001** MUST: pin providers.\n";

    fn seed(files: &[(&str, &str)]) -> TempDir {
        let root = tempdir().unwrap();
        let dir = crate::rules::rules_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        for (name, contents) in files {
            std::fs::write(dir.join(name), contents).unwrap();
        }
        root
    }

    fn checked(rule_id: &str, verdict: Verdict, span: &str, reason: &str) -> CheckedRule {
        CheckedRule {
            doc_slug: "cross-cutting-token-signing-1c57".to_string(),
            rule_id: rule_id.to_string(),
            level: RuleLevel::Must,
            statement: "sign with RS256.".to_string(),
            verdict,
            span: span.to_string(),
            reason: reason.to_string(),
        }
    }

    // ── gather_rules ──────────────────────────────────────────────────────

    #[test]
    fn test_gather_rules_reads_every_rule_from_every_selected_document() {
        let root = seed(&[
            ("cross-cutting-token-signing-1c57.md", OAUTH_DOC),
            ("cross-cutting-terraform-c340.md", TERRAFORM_DOC),
        ]);
        let report = crate::rules::load_rule_set(root.path()).unwrap();
        let index = crate::rules::scope::ScopeIndex::build(&report, root.path(), "fp".to_string());
        let query = Query::new("Sign access tokens for OAuth".to_string());
        let prefiltered = select::prefilter(&index, &query, 10, 30);
        let selection = prefiltered.finish(Stage2::NotRequested);
        assert!(!selection.selected.is_empty());

        let gathered = gather_rules(&selection, root.path());
        assert!(!gathered.truncated);
        let ids: Vec<&str> = gathered.rules.iter().map(|r| r.rule_id.as_str()).collect();
        assert!(ids.contains(&"R-A-001"));
        assert!(ids.contains(&"R-A-002"));
    }

    #[test]
    fn test_gather_rules_skips_a_document_that_no_longer_exists() {
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let report = crate::rules::load_rule_set(root.path()).unwrap();
        let index = crate::rules::scope::ScopeIndex::build(&report, root.path(), "fp".to_string());
        let query = Query::new("Sign tokens".to_string());
        let selection = select::prefilter(&index, &query, 10, 30).finish(Stage2::NotRequested);

        // Remove the file after selection but before gathering.
        std::fs::remove_file(
            crate::rules::rules_dir(root.path()).join("cross-cutting-token-signing-1c57.md"),
        )
        .unwrap();

        let gathered = gather_rules(&selection, root.path());
        assert!(gathered.rules.is_empty());
        assert_eq!(gathered.considered, 0);
        assert!(!gathered.truncated);
    }

    /// Exceeding the cap must be visible, not just silently clipped: the
    /// caller decides what to do with a truncated count, but it must be able
    /// to see one occurred.
    #[test]
    fn test_gather_rules_reports_truncation_rather_than_silently_capping() {
        let mut body = "# Many Rules: Widget Handling\n\nThese rules are ALWAYS ACTIVE for widget handling in `services/widgets/`.\n\n### Rules\n\n".to_string();
        for i in 0..(MAX_RULES_JUDGED + 10) {
            body.push_str(&format!("- **R-X-{i:04}** MUST: rule number {i}.\n"));
        }
        let root = seed(&[("cross-cutting-many-abcd.md", &body)]);
        let report = crate::rules::load_rule_set(root.path()).unwrap();
        let index = crate::rules::scope::ScopeIndex::build(&report, root.path(), "fp".to_string());
        let query = Query::new("Add a new widget in services/widgets".to_string());
        let selection = select::prefilter(&index, &query, 10, 30).finish(Stage2::NotRequested);
        assert!(!selection.selected.is_empty());

        let gathered = gather_rules(&selection, root.path());
        assert_eq!(gathered.rules.len(), MAX_RULES_JUDGED);
        assert_eq!(gathered.considered, MAX_RULES_JUDGED + 10);
        assert!(gathered.truncated);
    }

    #[test]
    fn test_gather_rules_not_truncated_exactly_at_the_cap() {
        let mut body = "# Exactly Many Rules: Widget Handling\n\nThese rules are ALWAYS ACTIVE for widget handling in `services/widgets/`.\n\n### Rules\n\n".to_string();
        for i in 0..MAX_RULES_JUDGED {
            body.push_str(&format!("- **R-X-{i:04}** MUST: rule number {i}.\n"));
        }
        let root = seed(&[("cross-cutting-exactly-many-abcd.md", &body)]);
        let report = crate::rules::load_rule_set(root.path()).unwrap();
        let index = crate::rules::scope::ScopeIndex::build(&report, root.path(), "fp".to_string());
        let query = Query::new("Add a new widget in services/widgets".to_string());
        let selection = select::prefilter(&index, &query, 10, 30).finish(Stage2::NotRequested);

        let gathered = gather_rules(&selection, root.path());
        assert_eq!(gathered.rules.len(), MAX_RULES_JUDGED);
        assert_eq!(gathered.considered, MAX_RULES_JUDGED);
        assert!(!gathered.truncated);
    }

    // ── deny / notice text ───────────────────────────────────────────────

    #[test]
    fn test_hook_deny_reason_names_every_rule_id_and_its_span() {
        let a = checked(
            "R-A-002",
            Verdict::Conflicting,
            "log the signing key for debugging",
            "R-A-002 forbids logging the key",
        );
        let reason = hook_deny_reason(&[&a]);
        assert!(reason.contains("R-A-002"));
        assert!(reason.contains("log the signing key for debugging"));
    }

    #[test]
    fn test_hook_deny_reason_falls_back_when_the_model_reason_is_blank() {
        let a = checked("R-A-002", Verdict::Conflicting, "some span", "");
        let reason = hook_deny_reason(&[&a]);
        assert!(reason.contains("conflicts with the plan"));
    }

    #[test]
    fn test_requires_decision_message_names_the_rule_and_does_not_use_deny_language() {
        let a = checked(
            "R-A-001",
            Verdict::RequiresDecision,
            "span",
            "supersedes it",
        );
        let message = requires_decision_message(&[&a]);
        assert!(message.contains("R-A-001"));
        assert!(!message.to_lowercase().contains("deny"));
    }

    // ── rendering ────────────────────────────────────────────────────────

    #[test]
    fn test_render_panel_nothing_applies() {
        let panel = render_panel(
            &Outcome::NothingApplies,
            "a plan",
            Path::new("/x/.actual/rules"),
            80,
        );
        assert!(panel.contains("No committed rule document applies"));
    }

    #[test]
    fn test_render_panel_conforming_says_so_explicitly() {
        let selection = Selection {
            plan: "p".to_string(),
            paths: Vec::new(),
            indexed_documents: 1,
            limit: 10,
            selected: vec![],
            stage2: Stage2::NotRequested,
        };
        let outcome = Outcome::Verdicts {
            selection,
            verdicts: vec![checked("R-A-001", Verdict::Conforming, "", "uses RS256")],
            runner_label: Some("claude-cli (sonnet)".to_string()),
        };
        let panel = render_panel(&outcome, "a plan", Path::new("/x/.actual/rules"), 80);
        assert!(panel.contains("Conforming"));
        assert!(!panel.contains("CONFLICT"));
    }

    #[test]
    fn test_render_panel_shows_conflicts_and_decisions() {
        let selection = Selection {
            plan: "p".to_string(),
            paths: Vec::new(),
            indexed_documents: 1,
            limit: 10,
            selected: vec![],
            stage2: Stage2::NotRequested,
        };
        let outcome = Outcome::Verdicts {
            selection,
            verdicts: vec![
                checked("R-A-002", Verdict::Conflicting, "logs the key", "forbidden"),
                checked(
                    "R-A-003",
                    Verdict::RequiresDecision,
                    "supersedes",
                    "deliberate",
                ),
            ],
            runner_label: None,
        };
        let panel = render_panel(&outcome, "a plan", Path::new("/x/.actual/rules"), 80);
        assert!(panel.contains("CONFLICT"));
        assert!(panel.contains("R-A-002"));
        assert!(panel.contains("DECISION"));
        assert!(panel.contains("R-A-003"));
    }

    #[test]
    fn test_render_json_status_values() {
        let selection = Selection {
            plan: "p".to_string(),
            paths: Vec::new(),
            indexed_documents: 1,
            limit: 10,
            selected: vec![],
            stage2: Stage2::NotRequested,
        };
        let conforming = Outcome::Verdicts {
            selection: selection.clone(),
            verdicts: vec![checked("R-A-001", Verdict::Conforming, "", "")],
            runner_label: None,
        };
        let json = render_json(&conforming);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "conforming");

        let conflicting = Outcome::Verdicts {
            selection: selection.clone(),
            verdicts: vec![checked("R-A-002", Verdict::Conflicting, "x", "y")],
            runner_label: None,
        };
        let json = render_json(&conflicting);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "conflicting");

        let json = render_json(&Outcome::NothingApplies);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "not_checked");
        assert!(value["detail"].is_string());
    }

    /// The review finding this guards: `NoRunner` and `CheckFailed` happen
    /// after selection ran, and must report how many documents were selected
    /// rather than a hardcoded zero that a CI consumer of `--json` cannot
    /// distinguish from "nothing applied at all".
    #[test]
    fn test_render_json_reports_documents_selected_on_no_runner_and_check_failed() {
        let no_runner = Outcome::NoRunner {
            documents_selected: 7,
            reason: "no ANTHROPIC_API_KEY".to_string(),
        };
        let value: serde_json::Value = serde_json::from_str(&render_json(&no_runner)).unwrap();
        assert_eq!(value["status"], "not_checked");
        assert_eq!(value["documents_selected"], 7);

        let check_failed = Outcome::CheckFailed {
            documents_selected: 3,
            reason: "runner timed out".to_string(),
        };
        let value: serde_json::Value = serde_json::from_str(&render_json(&check_failed)).unwrap();
        assert_eq!(value["status"], "not_checked");
        assert_eq!(value["documents_selected"], 3);
    }

    #[test]
    fn test_render_panel_reports_documents_selected_on_no_runner_and_check_failed() {
        let no_runner = Outcome::NoRunner {
            documents_selected: 7,
            reason: "no ANTHROPIC_API_KEY".to_string(),
        };
        let panel = render_panel(&no_runner, "a plan", Path::new("/x/.actual/rules"), 80);
        assert!(panel.contains("Documents selected"));
        assert!(panel.contains('7'));

        let check_failed = Outcome::CheckFailed {
            documents_selected: 3,
            reason: "runner timed out".to_string(),
        };
        let panel = render_panel(&check_failed, "a plan", Path::new("/x/.actual/rules"), 80);
        assert!(panel.contains("Documents selected"));
        assert!(panel.contains('3'));
    }

    #[test]
    fn test_truncate_counts_chars_not_bytes() {
        let s = "é".repeat(200);
        let truncated = truncate(&s, 10);
        assert_eq!(truncated.chars().count(), 10);
    }

    #[test]
    fn test_resolve_direct_plan_prefers_the_positional_argument() {
        let mut args = base_args();
        args.plan = vec!["Add".to_string(), "caching".to_string()];
        assert_eq!(resolve_direct_plan(&args).unwrap(), "Add caching");
    }

    #[test]
    fn test_resolve_direct_plan_reads_plan_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("plan.md");
        std::fs::write(&file, "# The plan").unwrap();
        let mut args = base_args();
        args.plan_file = Some(file);
        assert_eq!(resolve_direct_plan(&args).unwrap(), "# The plan");
    }

    fn base_args() -> PlanCheckArgs {
        PlanCheckArgs {
            plan: Vec::new(),
            plan_file: None,
            repo: None,
            rules_dir: None,
            claude_hook: false,
            limit: 20,
            candidates: crate::rules::scope::DEFAULT_CANDIDATES,
            no_rank: false,
            runner: None,
            model: None,
            json: false,
            rebuild: false,
        }
    }
}
