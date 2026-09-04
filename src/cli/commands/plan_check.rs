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

/// Read the hook payload from real stdin and hand it to [`exec_hook_with`].
///
/// Kept to this one fallible line on purpose: `std::io::stdin()` is real
/// process I/O, and calling it from an in-process unit test risks blocking on
/// whatever the test harness's own stdin happens to be (a real terminal,
/// notably — CI's closed/redirected stdin is not a given everywhere this runs).
/// Everything that does not touch the real world lives in [`exec_hook_with`],
/// which takes the bytes as a plain `&str` and is exercised directly; this
/// wrapper itself is covered by a subprocess test in `tests/cli_test.rs`,
/// which controls stdin safely because it drives a separate process.
fn exec_hook(args: &PlanCheckArgs) {
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        emit(plan_check_hook::render_notice(
            "plan-check could not read the hook payload on stdin",
        ));
        return;
    }
    exec_hook_with(args, &raw);
}

/// Run the `--claude-hook` path against an already-read payload.
///
/// INVARIANT: [`exec`] always sees this path complete normally — every
/// fallible step below is matched explicitly and turned into a fail-open
/// notice (or, for a real violation, a deny) rather than propagating an
/// error, so this function has no path that reaches an ordinary nonzero
/// exit. The only way [`exec`] could surface one from this path is an actual
/// Rust panic, which this function's own logic never triggers.
fn exec_hook_with(args: &PlanCheckArgs, raw: &str) {
    let envelope: HookEnvelope = match serde_json::from_str(raw) {
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

/// The deny reason: every conflicting rule id, the rule's own statement
/// verbatim, the judge's reason, and the quoted plan span, one per line — so
/// a reader (or the agent revising the plan) sees every violation at once
/// rather than only the first, and can revise against the rule's actual text
/// rather than the judge's paraphrase of it.
fn hook_deny_reason(conflicts: &[&CheckedRule]) -> String {
    conflicts
        .iter()
        .map(|c| {
            format!(
                "{} ({}): {} — rule: \"{}\" — plan: \"{}\"",
                c.rule_id,
                c.level.as_str(),
                non_empty_or(&c.reason, "conflicts with the plan"),
                truncate(&c.statement, 240),
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
    use crate::testutil::{EnvGuard, ENV_MUTEX};

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

    /// The gap this guards: the deny reason must carry the rule's own
    /// statement verbatim, not just the judge's paraphrase in `reason` — an
    /// agent revising the plan needs the actual rule text to revise against.
    #[test]
    fn test_hook_deny_reason_includes_the_rule_statement_verbatim() {
        let a = checked(
            "R-A-002",
            Verdict::Conflicting,
            "log the signing key for debugging",
            "R-A-002 forbids logging the key",
        );
        let reason = hook_deny_reason(&[&a]);
        assert!(reason.contains(&a.statement));
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

        let requires_decision = Outcome::Verdicts {
            selection: selection.clone(),
            verdicts: vec![checked("R-A-003", Verdict::RequiresDecision, "x", "y")],
            runner_label: None,
        };
        let json = render_json(&requires_decision);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "requires_decision");

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

    /// Helper: an isolated config directory, so a runner-resolving test never
    /// touches the real `~/.actualai` cache or config.
    fn isolated_config(home: &TempDir) -> (EnvGuard, EnvGuard) {
        (
            EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap()),
            EnvGuard::remove("ACTUAL_CONFIG"),
        )
    }

    /// A fake Claude Code binary that answers the auth probe as logged in and
    /// every other invocation with `structured_output`, whatever it is.
    ///
    /// This is what lets a runner-dependent pipeline path run end to end —
    /// runner resolution, the runtime bridge, and response parsing — without
    /// a model, a key or a network, the same technique `rules_scope.rs` uses
    /// for its own stage-2 tests.
    #[cfg(unix)]
    fn fake_claude(dir: &Path, structured_output: &serde_json::Value) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let envelope = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "structured_output": structured_output,
        })
        .to_string();
        let script = dir.join("fake-claude.sh");
        let body = format!(
            "#!/bin/sh\nif [ \"$1\" = \"auth\" ]; then printf '%s' '{{\"loggedIn\":true}}'; exit 0; fi\nprintf '%s' '{envelope}'\n"
        );
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    /// A fake binary that only ever answers the auth probe. Used when a test
    /// needs a runner to resolve successfully but the pipeline should never
    /// actually reach a completion call (e.g. it fails, or refuses, first).
    #[cfg(unix)]
    fn fake_claude_auth_only(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let script = dir.join("fake-claude-auth-only.sh");
        let body = "#!/bin/sh\nif [ \"$1\" = \"auth\" ]; then printf '%s' '{\"loggedIn\":true}'; exit 0; fi\nexit 1\n";
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    /// `structured_output` shaped for the check schema: an array of
    /// `{doc_slug, rule_id, verdict, span, reason}` entries.
    fn check_output(entries: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "verdicts": entries })
    }

    /// `structured_output` shaped for the *rank* schema (`slug`/`verdict`/
    /// `reason`) — valid input for stage 2's rank, and deliberately the wrong
    /// shape for the judge, so reusing it for a check call exercises the
    /// "judge output was malformed" path.
    fn rank_output(slug: &str) -> serde_json::Value {
        serde_json::json!({
            "verdicts": [{"slug": slug, "verdict": "governs", "reason": "it governs the change"}]
        })
    }

    // ── repo_root ────────────────────────────────────────────────────────

    #[test]
    fn test_repo_root_uses_the_explicit_path_when_given() {
        let explicit = PathBuf::from("/some/explicit/repo");
        assert_eq!(repo_root(Some(&explicit)), explicit);
    }

    #[test]
    fn test_repo_root_falls_back_to_the_working_directory() {
        assert_eq!(repo_root(None), crate::cli::commands::sync::resolve_cwd());
    }

    // ── resolve_direct_plan: the branches that never touch real stdin ──────

    #[test]
    fn test_resolve_direct_plan_errors_when_the_plan_file_is_empty() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("empty.md");
        std::fs::write(&file, "   \n\n").unwrap();
        let mut args = base_args();
        args.plan_file = Some(file.clone());
        let err = resolve_direct_plan(&args).unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(_)));
        assert!(err.to_string().contains(&file.display().to_string()));
    }

    #[test]
    fn test_resolve_direct_plan_errors_when_the_plan_file_does_not_exist() {
        let mut args = base_args();
        args.plan_file = Some(PathBuf::from("/no/such/plan-file.md"));
        let err = resolve_direct_plan(&args).unwrap_err();
        assert!(matches!(err, ActualError::IoError(_)));
    }

    // ── gather_rules: the remaining per-file failure branches ───────────────

    #[test]
    fn test_gather_rules_skips_a_document_that_is_not_valid_utf8() {
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let report = crate::rules::load_rule_set(root.path()).unwrap();
        let index = crate::rules::scope::ScopeIndex::build(&report, root.path(), "fp".to_string());
        let query = Query::new("Sign tokens".to_string());
        let selection = select::prefilter(&index, &query, 10, 30).finish(Stage2::NotRequested);

        // Corrupt the file to invalid UTF-8 after selection but before gathering.
        std::fs::write(
            crate::rules::rules_dir(root.path()).join("cross-cutting-token-signing-1c57.md"),
            [0xff, 0xfe, 0x00, 0x41],
        )
        .unwrap();

        let gathered = gather_rules(&selection, root.path());
        assert!(gathered.rules.is_empty());
    }

    #[test]
    fn test_gather_rules_skips_a_document_that_no_longer_parses() {
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let report = crate::rules::load_rule_set(root.path()).unwrap();
        let index = crate::rules::scope::ScopeIndex::build(&report, root.path(), "fp".to_string());
        let query = Query::new("Sign tokens".to_string());
        let selection = select::prefilter(&index, &query, 10, 30).finish(Stage2::NotRequested);

        // Rewrite with content that fails to parse (no rules section) between
        // selection and gathering.
        std::fs::write(
            crate::rules::rules_dir(root.path()).join("cross-cutting-token-signing-1c57.md"),
            "just some prose, no rules section at all\n",
        )
        .unwrap();

        let gathered = gather_rules(&selection, root.path());
        assert!(gathered.rules.is_empty());
    }

    // ── run_pipeline: branches that need a resolved (or unavailable) runner ─

    #[test]
    fn test_run_pipeline_no_runner_available() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _no_claude = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/path/to/claude");

        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let mut args = base_args();
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let rules_dir = crate::rules::rules_dir(root.path());

        let outcome = run_pipeline(
            "Sign access tokens with RS256",
            root.path(),
            &rules_dir,
            &args,
            false,
        )
        .unwrap();
        assert!(matches!(
            outcome,
            Outcome::NoRunner { documents_selected, .. } if documents_selected > 0
        ));
    }

    #[cfg(unix)]
    #[test]
    fn test_run_pipeline_nothing_applies_when_limit_is_zero() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let bin = tempdir().unwrap();
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude_auth_only(bin.path()).to_str().unwrap(),
        );

        let mut args = base_args();
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        args.limit = 0;
        let rules_dir = crate::rules::rules_dir(root.path());

        let outcome = run_pipeline("Sign tokens", root.path(), &rules_dir, &args, false).unwrap();
        assert!(matches!(outcome, Outcome::NothingApplies));
    }

    /// `gathered.rules.is_empty()` with a *non-empty* `selection` — the TOCTOU
    /// case `gather_rules`'s own doc comment names: a document indexed a
    /// moment ago no longer parses by the time it is re-read for gathering.
    ///
    /// Reproducing that race deterministically (rather than timing a real
    /// file mutation mid-call) means decoupling what the index claims from
    /// what is actually on disk: a hand-built `ScopeIndex` naming a document
    /// that was never written is stored directly in the on-disk cache, keyed
    /// under the real, unchanged directory's own content digest. `resolve_in`
    /// then gets a legitimate cache hit — the digest matches, because the
    /// real files never changed — and hands back this index, whose one
    /// document points at a file that does not exist. Selection is therefore
    /// non-empty, but gathering it finds nothing, exactly like a document
    /// that vanished between the two reads would.
    #[cfg(unix)]
    #[test]
    fn test_run_pipeline_nothing_applies_when_a_selected_document_cannot_be_gathered() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let root = tempdir().unwrap();
        let rules_dir = crate::rules::rules_dir(root.path());
        std::fs::create_dir_all(&rules_dir).unwrap();
        // One real, boring file, so the directory's content digest is stable
        // and never changes across this test.
        std::fs::write(
            rules_dir.join("boring.md"),
            "# Boring\n\nThese rules are ALWAYS ACTIVE for nothing in particular.\n\n### Rules\n\n- **R-BORING-001** MAY: exist.\n",
        )
        .unwrap();
        let digest = crate::rules::read_rule_sources_in(&rules_dir)
            .unwrap()
            .digest;

        let mut phantom = crate::rules::RuleDocument::empty(&rules_dir.join("phantom.md"));
        phantom.title = Some("Widget Colors".to_string());
        phantom.scope =
            Some("These rules are ALWAYS ACTIVE for choosing widget colors.".to_string());
        phantom.rules.push(crate::rules::Rule {
            id: "R-PHANTOM-001".to_string(),
            level: RuleLevel::Must,
            statement: "use blue for primary buttons.".to_string(),
            line: 1,
        });
        let report = crate::rules::RuleSetLoadReport {
            rules_dir: rules_dir.clone(),
            documents: vec![phantom],
            errors: Vec::new(),
            digest: digest.clone(),
        };
        let index = crate::rules::scope::ScopeIndex::build(&report, root.path(), digest);
        crate::rules::scope::cache::store(&rules_dir, &index);

        let bin = tempdir().unwrap();
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude_auth_only(bin.path()).to_str().unwrap(),
        );
        let mut args = base_args();
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);

        let outcome = run_pipeline(
            "Choose colors for widget buttons",
            root.path(),
            &rules_dir,
            &args,
            false,
        )
        .unwrap();
        assert!(matches!(outcome, Outcome::NothingApplies));
    }

    #[cfg(unix)]
    #[test]
    fn test_run_pipeline_check_failed_when_rules_exceed_the_judging_cap() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let mut body = "# Many Rules: Widget Handling\n\nThese rules are ALWAYS ACTIVE for widget handling in `services/widgets/`.\n\n### Rules\n\n".to_string();
        for i in 0..(MAX_RULES_JUDGED + 1) {
            body.push_str(&format!("- **R-X-{i:04}** MUST: rule number {i}.\n"));
        }
        let root = seed(&[("cross-cutting-many-abcd.md", &body)]);
        let bin = tempdir().unwrap();
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude_auth_only(bin.path()).to_str().unwrap(),
        );

        let mut args = base_args();
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let rules_dir = crate::rules::rules_dir(root.path());

        let outcome = run_pipeline(
            "Add a new widget in services/widgets",
            root.path(),
            &rules_dir,
            &args,
            false,
        )
        .unwrap();
        assert!(matches!(
            outcome,
            Outcome::CheckFailed { ref reason, .. } if reason.contains("judging cap")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn test_run_pipeline_check_failed_when_the_judge_response_is_malformed() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let bin = tempdir().unwrap();
        // Rank-shaped output is the wrong shape for the judge.
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &rank_output("cross-cutting-token-signing-1c57"))
                .to_str()
                .unwrap(),
        );

        let mut args = base_args();
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let rules_dir = crate::rules::rules_dir(root.path());

        let outcome = run_pipeline(
            "Sign access tokens with RS256",
            root.path(),
            &rules_dir,
            &args,
            false,
        )
        .unwrap();
        assert!(matches!(
            outcome,
            Outcome::CheckFailed { ref reason, .. } if !reason.is_empty()
        ));
    }

    #[cfg(unix)]
    #[test]
    fn test_run_pipeline_produces_verdicts_via_a_resolved_runner() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let bin = tempdir().unwrap();
        let response = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conforming", "span": "", "reason": "uses RS256"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "conforming", "span": "", "reason": "no logging"},
        ]));
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &response).to_str().unwrap(),
        );

        let mut args = base_args();
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let rules_dir = crate::rules::rules_dir(root.path());

        let outcome = run_pipeline(
            "Sign access tokens with RS256",
            root.path(),
            &rules_dir,
            &args,
            false,
        )
        .unwrap();
        assert!(matches!(
            &outcome,
            Outcome::Verdicts { verdicts, runner_label, .. }
                if verdicts.len() == 2
                    && runner_label.as_deref().is_some_and(|l| l.starts_with("claude-cli"))
        ));
    }

    /// `use_rank: true` actually reaches stage 2's rank, not just the
    /// deterministic prefilter — proven by requiring more candidates than
    /// `--limit` (which forces `needs_rank()`) and a runner that only
    /// produces a *rank*-shaped answer. The judge call that necessarily
    /// follows then fails on that same malformed shape, which is itself a
    /// legitimate, separately-asserted outcome.
    #[cfg(unix)]
    #[test]
    fn test_run_pipeline_uses_stage_two_rank_when_use_rank_is_true() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[
            ("cross-cutting-token-signing-1c57.md", OAUTH_DOC),
            ("cross-cutting-terraform-c340.md", TERRAFORM_DOC),
        ]);
        let bin = tempdir().unwrap();
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &rank_output("cross-cutting-token-signing-1c57"))
                .to_str()
                .unwrap(),
        );

        let mut args = base_args();
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        args.limit = 1;
        let rules_dir = crate::rules::rules_dir(root.path());

        let outcome = run_pipeline(
            "Rotate the OAuth signing key and pin providers",
            root.path(),
            &rules_dir,
            &args,
            true,
        )
        .unwrap();
        assert!(matches!(
            outcome,
            Outcome::CheckFailed { .. } | Outcome::Verdicts { .. }
        ));
    }

    // ── exec / exec_direct: dispatch and the conforming/conflict outcomes ───

    #[test]
    fn test_exec_direct_dispatch_with_no_applicable_rules_returns_ok() {
        let repo = tempdir().unwrap();
        let mut args = base_args();
        args.plan = vec!["a plan".to_string()];
        args.repo = Some(repo.path().to_path_buf());
        assert!(exec(&args).is_ok());
    }

    #[test]
    fn test_exec_direct_json_output_with_no_applicable_rules() {
        let repo = tempdir().unwrap();
        let mut args = base_args();
        args.plan = vec!["a plan".to_string()];
        args.repo = Some(repo.path().to_path_buf());
        args.json = true;
        assert!(exec(&args).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn test_exec_direct_returns_ok_on_a_conforming_plan() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let bin = tempdir().unwrap();
        let response = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conforming", "span": "", "reason": "uses RS256"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "conforming", "span": "", "reason": "no logging"},
        ]));
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &response).to_str().unwrap(),
        );

        let mut args = base_args();
        args.plan = vec![
            "Sign".to_string(),
            "access".to_string(),
            "tokens".to_string(),
            "with".to_string(),
            "RS256".to_string(),
        ];
        args.repo = Some(root.path().to_path_buf());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        args.no_rank = true;

        assert!(exec(&args).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn test_exec_direct_returns_plan_not_conforming_on_a_real_conflict() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let bin = tempdir().unwrap();
        let response = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conforming", "span": "", "reason": "uses RS256"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "conflicting", "span": "logs the key", "reason": "forbidden"},
        ]));
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &response).to_str().unwrap(),
        );

        let mut args = base_args();
        args.plan = vec![
            "Sign".to_string(),
            "access".to_string(),
            "tokens".to_string(),
            "with".to_string(),
            "RS256".to_string(),
        ];
        args.repo = Some(root.path().to_path_buf());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        args.no_rank = true;

        let err = exec(&args).unwrap_err();
        assert!(matches!(err, ActualError::PlanNotConforming(_)));
    }

    // ── exec_hook_with: every notice/deny branch, without touching stdin ────

    #[test]
    fn test_exec_hook_with_malformed_json_is_a_silent_fail_open() {
        // No panic, no emitted deny -- covered by not panicking, since stdout
        // capture is not exercised at this layer (see the subprocess tests
        // in tests/cli_test.rs for the observable-stdout contract).
        exec_hook_with(&base_args(), "not json");
    }

    #[test]
    fn test_exec_hook_with_no_plan_resolvable() {
        exec_hook_with(&base_args(), "{}");
    }

    #[test]
    fn test_exec_hook_with_reports_a_rules_directory_load_failure() {
        let root = tempdir().unwrap();
        let not_a_dir = root.path().join("rules-dir-is-a-file");
        std::fs::write(&not_a_dir, "not a directory").unwrap();

        let mut args = base_args();
        args.rules_dir = Some(not_a_dir);
        let raw = serde_json::json!({"tool_input": {"plan": "a plan"}}).to_string();
        exec_hook_with(&args, &raw);
    }

    #[test]
    fn test_exec_hook_with_nothing_applies() {
        let repo = tempdir().unwrap();
        let mut args = base_args();
        args.repo = Some(repo.path().to_path_buf());
        let raw = serde_json::json!({"tool_input": {"plan": "a plan"}}).to_string();
        exec_hook_with(&args, &raw);
    }

    #[test]
    fn test_exec_hook_with_no_runner_available() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _no_claude = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/path/to/claude");

        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let mut args = base_args();
        args.rules_dir = Some(crate::rules::rules_dir(root.path()));
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let raw = serde_json::json!({"tool_input": {"plan": "Sign access tokens with RS256"}})
            .to_string();
        exec_hook_with(&args, &raw);
    }

    #[cfg(unix)]
    #[test]
    fn test_exec_hook_with_check_failed() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let bin = tempdir().unwrap();
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &rank_output("cross-cutting-token-signing-1c57"))
                .to_str()
                .unwrap(),
        );

        let mut args = base_args();
        args.rules_dir = Some(crate::rules::rules_dir(root.path()));
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let raw = serde_json::json!({"tool_input": {"plan": "Sign access tokens with RS256"}})
            .to_string();
        exec_hook_with(&args, &raw);
    }

    #[cfg(unix)]
    #[test]
    fn test_exec_hook_with_denies_a_real_conflict() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let bin = tempdir().unwrap();
        let response = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conforming", "span": "", "reason": "uses RS256"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "conflicting", "span": "logs the key", "reason": "forbidden"},
        ]));
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &response).to_str().unwrap(),
        );

        let mut args = base_args();
        args.rules_dir = Some(crate::rules::rules_dir(root.path()));
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let raw = serde_json::json!({"tool_input": {"plan": "Sign access tokens with RS256"}})
            .to_string();
        exec_hook_with(&args, &raw);
    }

    #[cfg(unix)]
    #[test]
    fn test_exec_hook_with_flags_a_requires_decision_verdict() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let bin = tempdir().unwrap();
        let response = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "requires_decision", "span": "moves to HS256", "reason": "deliberate supersession"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "conforming", "span": "", "reason": "no logging"},
        ]));
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &response).to_str().unwrap(),
        );

        let mut args = base_args();
        args.rules_dir = Some(crate::rules::rules_dir(root.path()));
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let raw = serde_json::json!({"tool_input": {"plan": "Sign access tokens with RS256"}})
            .to_string();
        exec_hook_with(&args, &raw);
    }

    #[cfg(unix)]
    #[test]
    fn test_exec_hook_with_stays_silent_on_a_fully_conforming_plan() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let bin = tempdir().unwrap();
        let response = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conforming", "span": "", "reason": "uses RS256"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "conforming", "span": "", "reason": "no logging"},
        ]));
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &response).to_str().unwrap(),
        );

        let mut args = base_args();
        args.rules_dir = Some(crate::rules::rules_dir(root.path()));
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let raw = serde_json::json!({"tool_input": {"plan": "Sign access tokens with RS256"}})
            .to_string();
        exec_hook_with(&args, &raw);
    }
}
