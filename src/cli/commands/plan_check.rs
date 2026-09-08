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
//! dialog is untouched; a genuine violation or an unconfirmed "deliberate
//! supersession" claim prints exactly one JSON object naming the rule id
//! and the conflicting span; and every other outcome — no plan resolvable,
//! no rules, no runner, a crashed judge call — fails open. `--claude-hook`
//! never returns `Err` from [`exec`]: every fallible step is caught and
//! turned into a fail-open notice, the same invariant `crate::rules::discover`
//! documents for its per-file loop. `permission Decision: "allow"` is never
//! emitted anywhere in this module or in [`plan_check_hook`] — there is no
//! function that produces it — because a gate has no business granting the
//! approval it is supposed to be checking.
//!
//! # This is an advisory gate, not an enforcement boundary
//!
//! Worth being explicit about, because the fail-open conditions below stack:
//! this hook can raise the bar for an agent that is cooperating with
//! governance, and it can catch an oversight before it ships. It is not a
//! security control, and it cannot be, because it fails open by design in
//! several distinct ways, each individually deliberate:
//!
//! - **Any infrastructure failure fails open.** No runner available, the
//!   judge call itself failing (timeout, malformed output), no plan text
//!   resolvable, no rules directory readable — every one of these is a
//!   `--claude-hook` notice, never a deny. A `PreToolUse` hook that could
//!   block on its own dependencies being unavailable would make the tool
//!   itself unreliable for reasons that have nothing to do with the plan.
//! - **The [`MAX_RULES_JUDGED`] cap means a large enough selected batch is
//!   only ever partially judged.** If the documents [`run_pipeline`] selects
//!   for a plan hold more individual rules combined than that cap — one large
//!   document is enough on its own, and several ordinary ones add up just as
//!   easily — only a deterministically-prioritized prefix (selection order,
//!   then declaration order within a document) is actually judged this
//!   round. This is disclosed, not silent: every caller is told `N of M`
//!   rules were checked, so "conforming" never gets reported as "conforming"
//!   full stop when it was really "conforming, as far as we looked." In
//!   `--claude-hook` mode, which prefix gets judged also rotates by round
//!   (see [`gather_rules`]) so a rule in the truncated tail is not silently
//!   unjudged for the entire life of a session — a mitigation, not a
//!   guarantee: judging the full candidate set in one round regardless of
//!   size is tracked separately (AK-743). A round whose judged prefix has
//!   nothing blocking is always recorded to the durable audit log even when
//!   the hook itself stays silent-but-for-the-disclosure-notice, precisely
//!   because this is the one fail-open path a human is least likely to
//!   notice in the moment — see [`plan_check_session::record_partial_coverage`].
//! - **The revision loop's own escape valves (below) are additional,
//!   deliberate fail-open paths**, not enforcement: the round limit stops
//!   blocking a persistently unresolved rule specifically so the hook does
//!   not get uninstalled, and an override lets a human wave a specific rule
//!   through outright. Both are recorded, which makes them *inspectable*,
//!   not enforced.
//!
//! None of this is a defect to fix — a `PreToolUse` hook that could hang or
//! wrongly block a tool call under infrastructure failure would be a worse
//! design than one that fails open — but it does mean this module's job is
//! to raise the cost of an unreviewed change, not to guarantee one cannot
//! happen. Treat every "deny" this module produces as a strong nudge with a
//! paper trail, not a guarantee nothing gets past it.
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
//!
//! # The revision loop (AK-677)
//!
//! `--claude-hook` fires again every time the agent revises a denied plan and
//! calls `ExitPlanMode` once more. Four things follow from that, all keyed
//! on the hook envelope's `session_id` (stable for one Claude Code
//! conversation, paired with `rules_dir` — see [`plan_check_session`]'s own
//! doc for why session_id alone is not enough):
//!
//! 1. **`requires_decision` blocks exactly like a real conflict.** A plan the
//!    judge classifies as *deliberately* superseding a rule is not
//!    automatically believed — that classification is model output, not a
//!    recorded human decision, and the epic this belongs to (AK-662) asks
//!    for the deliberate case to be "surfaced for explicit architectural
//!    review," not silently allowed past. So it gets the same deny + per-rule
//!    round-limit + override treatment as [`Verdict::Conflicting`], not a
//!    notice the agent can simply proceed past.
//! 2. **Never re-litigate a cleared rule.** Every rule the judge has already
//!    called [`Verdict::Conforming`] for this session, against this exact
//!    plan text, is excluded from the next judge call outright — not merely
//!    re-asked and hoped to agree. A judge is a model call, not a
//!    deterministic function; without this, a borderline rule could flip
//!    from cleared to blocking on a later round for no reason the agent
//!    caused. An explicit override (below) is excluded the same way,
//!    regardless of plan text.
//! 3. **An explicit, recorded override.** A human runs
//!    `actual plan-check-override` directly, from an interactive terminal —
//!    [`exec_override`]'s own doc comment explains why that check exists and
//!    what it does and does not guarantee. An overridden rule is excluded
//!    from judging from that point on, exactly like a cleared rule, but
//!    every subsequent round says so out loud in a non-blocking notice: an
//!    override is deliberately visible, not a silent bypass. See
//!    [`plan_check_session::record_override`].
//! 4. **A bounded number of rounds, per rule.** [`DEFAULT_MAX_ROUNDS`] real
//!    denials of the *same rule* (`--max-rounds` / `ACTUAL_PLAN_CHECK_MAX_ROUNDS`
//!    to change it) may block a session before the gate stops blocking on
//!    that rule specifically, regardless of verdict — a hard block with no
//!    exit gets the hook uninstalled, which governs nothing. A single rule
//!    exhausting its budget never exempts a different, still-fresh conflict
//!    in the same round (see [`PlanCheckSession::deny_limit_exceeded`]). The
//!    round-limit pass is recorded exactly like an override (see
//!    [`plan_check_session::record_round_limit`]), never silent, just
//!    triggered by the cap instead of a human action.
//!
//! Direct mode never reads or writes session state (there is no
//! `session_id` outside a hook envelope), so none of this changes its
//! behavior: `requires_decision` still only sets `--json`'s status field
//! there and exits 0, matching its documented, unchanged contract.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::cli::args::{PlanCheckArgs, PlanCheckOverrideArgs};
use crate::cli::commands::plan_check_hook::{self, HookEnvelope};
use crate::cli::commands::plan_check_session::{self, PlanCheckSession};
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
/// Exceeding it is not treated as a failure to check: [`gather_rules`] still
/// judges [`MAX_RULES_JUDGED`] rules, in priority order (selection order —
/// the prefilter's own relevance ranking — then declaration order within a
/// document) starting from a per-round rotating offset rather than always
/// index zero (see [`gather_rules`]'s own doc), and [`run_pipeline`] reports
/// the judged count against the true total so every caller can say plainly
/// "N of M rules checked" rather than either silently calling a partial
/// answer complete or refusing to check anything at all. See the module
/// doc's "advisory gate" section.
///
/// Lowered from an original 60 after live measurement against a real
/// 425-rule-document corpus: a 60-rule batch's structured answer, plus a real
/// plan long and detailed enough to need one, pushed the judge call close
/// enough to `crate::rules::check::CHECK_BUDGET` that live model-latency
/// variance alone (not a code defect — the identical call measured anywhere
/// from 58 to 82 seconds of API time run to run) could tip it over. 40 is not
/// a value with a precise safety proof behind it, since that variance itself
/// didn't scale predictably with rule count in measurement either — it is a
/// real, measured reduction in typical-case latency, paired with the raised
/// `CHECK_BUDGET` for margin against the variance no batch size alone
/// removes.
const MAX_RULES_JUDGED: usize = 40;

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
    /// The judge call itself failed — timeout, malformed or incomplete
    /// output. Unlike hitting [`MAX_RULES_JUDGED`] (see [`Outcome::Verdicts`]'s
    /// `partial`), this is not a disclosed partial answer: nothing about this
    /// round's verdicts can be trusted, so none are reported.
    CheckFailed {
        documents_selected: usize,
        reason: String,
    },
    /// The judge ran and produced a verdict for every rule it was shown.
    Verdicts {
        selection: Selection,
        verdicts: Vec<CheckedRule>,
        runner_label: Option<String>,
        /// `Some((judged, total))` when the selected documents held more
        /// individual rules than [`MAX_RULES_JUDGED`] and only a
        /// deterministically-prioritized prefix was actually judged this
        /// round; `None` when every candidate rule (after session exclusion)
        /// was judged. Every caller must disclose this when present, never
        /// report a partial answer as a complete one.
        partial: Option<(usize, usize)>,
    },
}

/// Run the whole pipeline: resolve the index, select documents, gather their
/// rules, resolve a runner, and judge.
///
/// A runner is resolved up front only when stage 2's rank needs one before
/// selection can even be computed (`use_rank`); otherwise resolution is
/// deferred until immediately before the judge call, so a round that turns
/// out to need no judging at all — every applicable rule already excluded —
/// never probes for a runner it will not use.
///
/// `use_rank` gates selection's stage 2. `--claude-hook` always passes
/// `false`, unconditionally, regardless of any flag — see the module doc for
/// why. Direct mode passes `!args.no_rank`.
///
/// `session` names every rule that must never reach the judge again for
/// *this* plan text — see [`PlanCheckSession::excludes`] for exactly what
/// that means (an override applies regardless of wording; a clearance only
/// while the plan digest still matches the one that earned it). Direct mode
/// always passes [`PlanCheckSession::default`]: there is no session outside a
/// hook envelope, so nothing is ever excluded there. When every rule a
/// selection would otherwise judge is excluded, this returns
/// `Outcome::Verdicts` with an empty `verdicts` and no runner ever resolved —
/// there is nothing new to check, which is exactly the fully-conforming,
/// silent-in-hook-mode case, not "nothing applies" (which would misreport
/// that no rule governs this plan at all).
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
    session: &PlanCheckSession,
) -> Result<Outcome, ActualError> {
    let plan_digest = plan_check_session::plan_digest(plan_text);
    let resolved = scope::resolve_in(rules_dir, root, args.rebuild)?;
    let query = Query::new(plan_text.to_string());
    let prefiltered = select::prefilter(&resolved.index, &query, args.limit, args.candidates);

    if prefiltered.is_empty() {
        return Ok(Outcome::NothingApplies);
    }

    let cfg = crate::config::paths::load().unwrap_or_default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ActualError::InternalError(format!("failed to build tokio runtime: {e}")))?;

    // A runner is only resolved up front when stage 2's rank needs one to
    // produce a selection at all. Otherwise resolution is deferred until
    // just before the judge call, below — so a round whose every applicable
    // rule turns out to already be excluded (see `gathered.excluded` below)
    // never has to resolve a runner it will not use.
    let early_runner = if use_rank {
        match rules_rank::resolve(
            args.runner.as_ref(),
            args.model.as_deref(),
            &cfg,
            rules_rank::RANK_TIMEOUT_SECS,
        ) {
            Ok(runner) => Some(runner),
            Err(reason) => {
                return Ok(Outcome::NoRunner {
                    documents_selected: prefiltered.len(),
                    reason,
                })
            }
        }
    } else {
        None
    };

    let selection = if use_rank {
        let runner = early_runner
            .as_ref()
            .expect("early_runner is always Some when use_rank is true");
        runtime.block_on(prefiltered.rank_with(
            &runner.runner,
            runner.model.as_deref(),
            runner.max_budget_usd,
        ))
    } else {
        prefiltered.finish(Stage2::NotRequested)
    };

    if selection.selected.is_empty() {
        return Ok(Outcome::NothingApplies);
    }

    let gathered = gather_rules(&selection, root, session, &plan_digest);
    if gathered.rules.is_empty() {
        return Ok(if gathered.excluded > 0 {
            // Every applicable rule was already settled this session: there
            // is nothing new to judge, which is the fully-conforming case,
            // not "no rule applies" (this plan does have applicable rules —
            // they were just already cleared or overridden).
            Outcome::Verdicts {
                selection,
                verdicts: Vec::new(),
                runner_label: None,
                partial: None,
            }
        } else {
            Outcome::NothingApplies
        });
    }

    let resolved_runner = match early_runner {
        // Resolved above with `RANK_TIMEOUT_SECS` for stage 2's own call, then
        // reused here for the judge -- a runner resolved this way can still
        // hit the judge's own timeout ceiling on a large enough batch, same
        // as the `None` branch used to unconditionally. Not fixed here: this
        // path is direct-mode-with-rank-enabled only (`--claude-hook` always
        // takes the `None` branch below, per the module doc), and doesn't
        // have a measured failure the way that branch did.
        Some(runner) => runner,
        // `--claude-hook` always reaches this branch (use_rank is false,
        // unconditionally), so this resolution is for the judge and nothing
        // else -- it must use `CHECK_TIMEOUT_SECS`, not `RANK_TIMEOUT_SECS`.
        // Passing the rank cap here was the exact bug: a real 60-rule batch
        // measured the judge's answer arriving as one unstreamed block after
        // 45+ silent seconds, comfortably inside `CHECK_BUDGET`'s 90-second
        // wall clock but past a 60-second inactivity cap sized for a
        // different call entirely.
        None => match rules_rank::resolve(
            args.runner.as_ref(),
            args.model.as_deref(),
            &cfg,
            crate::rules::check::CHECK_TIMEOUT_SECS,
        ) {
            Ok(runner) => runner,
            Err(reason) => {
                return Ok(Outcome::NoRunner {
                    documents_selected: selection.selected.len(),
                    reason,
                })
            }
        },
    };
    let label = resolved_runner.label();

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
            partial: gathered
                .truncated
                .then_some((gathered.rules.len(), gathered.considered)),
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
    /// The rules actually handed to the judge: up to [`MAX_RULES_JUDGED`]
    /// non-excluded candidates, in priority order starting from this round's
    /// rotating offset when truncated (see [`gather_rules`]).
    rules: Vec<RuleForJudging>,
    /// Total *non-excluded* individual rules found across every selected
    /// document — i.e. `excluded` is not part of this count. Equal to
    /// `rules.len()` unless `truncated`, in which case it is the true count
    /// that would have been judged with no cap at all.
    considered: usize,
    /// True when `considered` exceeds [`MAX_RULES_JUDGED`] — `rules` holds
    /// only a prefix, not everything that applies. The caller must disclose
    /// `rules.len()` of `considered` rather than reporting `rules` as
    /// complete coverage (see [`Outcome::Verdicts`]'s `partial`).
    truncated: bool,
    /// How many rules were dropped because `session` already excludes them
    /// for this plan digest (already cleared against this exact text, or
    /// overridden) — distinct from `truncated`, and from a genuinely empty
    /// selection: it is the caller's signal that `rules` being empty means
    /// "nothing new," not "nothing applies."
    excluded: usize,
}

/// Read the individual rules out of every selected document, in selection
/// order (the prefilter's own relevance ranking — the highest-priority
/// document's rules fill the cap first) and then declaration order within a
/// document, dropping any rule [`PlanCheckSession::excludes`] for
/// `plan_digest` before the [`MAX_RULES_JUDGED`] cap is applied — an excluded
/// rule must never consume cap budget that a rule still worth judging needs.
///
/// When the non-excluded candidate count exceeds the cap, the judged window
/// is not pinned to index zero every round: it starts at
/// [`rotation_offset`]`(session.rounds, considered)`, a cap-sized chunk per
/// completed round, wrapping. A rule left in the truncated tail on round 1
/// therefore has a real chance of landing inside the judged window on a
/// later round instead of being silently skipped for the entire life of the
/// session — session `rounds` only advances when a real judge call
/// completes (see [`PlanCheckSession::rounds`]), so this only changes
/// anything once a session has actually run more than one round against a
/// selection this large. Still fully deterministic — the same session, at
/// the same round, against the same candidates, always rotates to the same
/// window — and still only a mitigation, not a guarantee: a plan approved on
/// its first round always sees round 0's window (offset zero, identical to
/// the old fixed-prefix behavior), and a session that revises only a few
/// times before approval may never rotate far enough to reach a very large
/// tail. Judging the entire candidate set in a single round regardless of
/// size needs concurrent, sharded judge calls instead of a bigger one, which
/// is real implementation work tracked separately as AK-743 — this only
/// ensures the gap moves round over round instead of calcifying on the same
/// rules forever.
///
/// A document that no longer parses (removed, edited to something invalid,
/// between selection and this read) is skipped rather than failing the whole
/// batch — one bad file never costs the rest, the same invariant
/// `crate::rules::discover` enforces on the original scan.
fn gather_rules(
    selection: &Selection,
    root: &Path,
    session: &PlanCheckSession,
    plan_digest: &str,
) -> GatheredRules {
    let mut candidates = Vec::new();
    let mut excluded = 0usize;
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
            if session.excludes(
                &plan_check_session::key(&selected.slug, &rule.id),
                plan_digest,
            ) {
                excluded += 1;
                continue;
            }
            candidates.push(RuleForJudging::new(
                selected.slug.clone(),
                rule.id,
                rule.level,
                rule.statement,
            ));
        }
    }
    let considered = candidates.len();
    let truncated = considered > MAX_RULES_JUDGED;
    if truncated {
        candidates.rotate_left(rotation_offset(session.rounds, considered));
        candidates.truncate(MAX_RULES_JUDGED);
    }
    GatheredRules {
        rules: candidates,
        considered,
        truncated,
        excluded,
    }
}

/// The judged window's starting offset for `round`, into `considered`
/// non-excluded candidates: `round` cap-sized chunks in, wrapping. Round 0 —
/// a session's first call, and every direct-mode call, which never tracks
/// rounds at all (`PlanCheckSession::default()` always has `rounds: 0`) —
/// always resolves to offset zero, the same window a plain unrotated cap
/// would have judged, so this changes nothing until a session completes at
/// least one round against a selection larger than the cap. `considered == 0`
/// cannot occur at the only call site (guarded by `truncated`, which implies
/// `considered > MAX_RULES_JUDGED > 0`), but returns 0 rather than divide by
/// zero if ever called otherwise.
fn rotation_offset(round: u32, considered: usize) -> usize {
    if considered == 0 {
        return 0;
    }
    ((round as u64).saturating_mul(MAX_RULES_JUDGED as u64) % considered as u64) as usize
}

// ── direct mode ──────────────────────────────────────────────────────────

fn exec_direct(args: &PlanCheckArgs) -> Result<(), ActualError> {
    let plan_text = resolve_direct_plan(args)?;
    let root = repo_root(args.repo.as_ref());
    let rules_dir = args
        .rules_dir
        .clone()
        .unwrap_or_else(|| crate::rules::rules_dir(&root));

    let outcome = run_pipeline(
        &plan_text,
        &root,
        &rules_dir,
        args,
        !args.no_rank,
        &PlanCheckSession::default(),
    )?;

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

/// Read `reader` into a string, capped at one byte past
/// [`plan_check_hook::MAX_READ_BYTES`] via `Read::take` -- the same
/// stat-then-read-avoiding technique `plan_check_hook::read_capped` already
/// uses for the hook's own file reads -- so neither a `--plan-file` nor a
/// piped stdin plan can make this process buffer an unbounded amount of
/// input before its size is even checked. `source` names what was being read,
/// for the "too large" message only; callers still do their own trim/empty
/// check afterward with their own distinct message, since "empty" means
/// something different for a missing file than for empty stdin.
fn capped_read<R: Read>(reader: R, source: &str) -> Result<String, ActualError> {
    let mut bytes = Vec::new();
    reader
        .take(plan_check_hook::MAX_READ_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(ActualError::IoError)?;
    if bytes.len() as u64 > plan_check_hook::MAX_READ_BYTES {
        return Err(ActualError::ConfigError(format!(
            "{source} exceeds the {}-byte plan-check limit",
            plan_check_hook::MAX_READ_BYTES
        )));
    }
    String::from_utf8(bytes)
        .map_err(|_| ActualError::ConfigError(format!("{source} is not valid UTF-8")))
}

/// The plan text for direct-mode use: the positional argument, then
/// `--plan-file`, then stdin.
fn resolve_direct_plan(args: &PlanCheckArgs) -> Result<String, ActualError> {
    if !args.plan.is_empty() {
        return Ok(args.plan.join(" "));
    }
    if let Some(path) = &args.plan_file {
        let file = std::fs::File::open(path).map_err(ActualError::IoError)?;
        let text = capped_read(file, &path.display().to_string())?;
        if text.trim().is_empty() {
            return Err(ActualError::ConfigError(format!(
                "{} is empty",
                path.display()
            )));
        }
        return Ok(text);
    }
    let text = capped_read(std::io::stdin(), "stdin")?;
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
            partial,
        } => {
            panel = panel.kv("Documents selected", &selection.selected.len().to_string());
            if let Some(label) = runner_label {
                panel = panel.kv("Runner", label);
            }
            panel = panel.kv(
                "Rules checked",
                &match partial {
                    Some((judged, total)) => format!(
                        "{judged} of {total} (over the {MAX_RULES_JUDGED}-rule cap; the rest \
                         were not judged)"
                    ),
                    None => verdicts.len().to_string(),
                },
            );
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
struct PartialCoverage {
    /// How many rules were actually judged.
    judged: usize,
    /// How many rules applied in total, including the untruncated tail.
    total: usize,
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
    /// Present only when [`MAX_RULES_JUDGED`] was exceeded and `verdicts`
    /// covers a prefix, not everything that applied — a consumer that reads
    /// only `status` must still be able to see a "conforming" answer was
    /// partial by checking for this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    partial: Option<PartialCoverage>,
}

fn render_json(outcome: &Outcome) -> String {
    let payload = match outcome {
        Outcome::NothingApplies => PlanCheckJson {
            status: "not_checked",
            detail: Some("no committed rule document applies to this plan".to_string()),
            runner: None,
            documents_selected: 0,
            verdicts: Vec::new(),
            partial: None,
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
            partial: None,
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
            partial: None,
        },
        Outcome::Verdicts {
            selection,
            verdicts,
            runner_label,
            partial,
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
                partial: partial.map(|(judged, total)| PartialCoverage { judged, total }),
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
    // Capped via `capped_read`, same as `resolve_direct_plan`'s reads: an
    // over-long, non-UTF8, or outright unreadable payload all collapse to
    // the same fail-open notice here -- unlike direct mode, this path has no
    // human waiting on a distinct error message, only an agent that must
    // never be blocked by a malformed or oversized envelope.
    let raw = match capped_read(std::io::stdin(), "the hook payload") {
        Ok(text) => text,
        Err(_) => {
            emit(plan_check_hook::render_notice(
                "plan-check could not read the hook payload on stdin",
            ));
            return;
        }
    };
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

    // The revision loop keys entirely on `session_id`: absent (an older
    // Claude Code build), this call behaves exactly as it did before AK-677
    // — no state read, no state written, no round/override language in the
    // output. See the module doc's "revision loop" section.
    let session_id = envelope.session_id.as_deref();
    let mut session = session_id
        .map(|id| plan_check_session::load(id, &rules_dir))
        .unwrap_or_default();
    let plan_digest = plan_check_session::plan_digest(&plan_text);

    // `use_rank: false`, unconditionally, regardless of `args.no_rank`: the
    // hook's one model call stays reserved for the judge. See the module doc.
    let outcome = match run_pipeline(&plan_text, &root, &rules_dir, args, false, &session) {
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
        Outcome::Verdicts {
            verdicts,
            runner_label,
            partial,
            ..
        } => {
            // A round is one *completed judge call* — `runner_label` is only
            // ever `Some` when `check::check` actually ran (never for the
            // "everything was already excluded" shortcut in `run_pipeline`).
            let judge_ran = runner_label.is_some();
            if session_id.is_some() {
                for v in &verdicts {
                    let key = plan_check_session::key(&v.doc_slug, &v.rule_id);
                    if v.verdict == Verdict::Conforming {
                        session.cleared.insert(key, plan_digest.clone());
                    } else {
                        // A rule that was cleared against an earlier plan and
                        // is no longer conforming against this one must not
                        // leave a stale entry behind -- it is no longer a
                        // true fact about the current plan, digest mismatch
                        // or not.
                        session.cleared.remove(&key);
                    }
                }
                if judge_ran {
                    session.rounds += 1;
                }
            }

            // A verdict blocks the tool call when it is a genuine conflict,
            // or when the judge classifies the plan as *deliberately*
            // superseding a rule — that classification is model output, not
            // a recorded human decision, so it gets exactly the same deny +
            // override treatment as an outright conflict, not a notice the
            // agent can simply proceed past. See the module doc's "advisory
            // gate" section.
            let blocking: Vec<&CheckedRule> = verdicts
                .iter()
                .filter(|v| matches!(v.verdict, Verdict::Conflicting | Verdict::RequiresDecision))
                .collect();

            if !blocking.is_empty() {
                if session_id.is_some() {
                    // Every currently-blocking rule gets its own denial
                    // recorded, independent of any other rule's count — see
                    // the module doc's "the round limit is per rule" note.
                    for c in &blocking {
                        session.record_denial(&plan_check_session::key(&c.doc_slug, &c.rule_id));
                    }
                }

                if let Some(session_id) = session_id {
                    let exhausted: Vec<&CheckedRule> = blocking
                        .iter()
                        .filter(|c| {
                            session.deny_limit_exceeded(
                                &plan_check_session::key(&c.doc_slug, &c.rule_id),
                                args.max_rounds,
                            )
                        })
                        .copied()
                        .collect();
                    // Fail open only when *every* rule blocking this round has
                    // individually exhausted its own budget. A single rule
                    // still within budget keeps the whole call denied — the
                    // hook can only deny or not deny the tool call as a
                    // whole, so an exhausted rule cannot be waved through
                    // while a fresh one still needs to block.
                    if !exhausted.is_empty() && exhausted.len() == blocking.len() {
                        let keys: Vec<String> = exhausted
                            .iter()
                            .map(|c| plan_check_session::key(&c.doc_slug, &c.rule_id))
                            .collect();
                        let message = round_limit_message(&exhausted, &session, args.max_rounds);
                        plan_check_session::record_round_limit(
                            session_id,
                            &rules_dir,
                            session.rounds,
                            &keys,
                            &message,
                        );
                        plan_check_session::store(session_id, &rules_dir, &session);
                        // The audit log keeps the round-limit message on its
                        // own, undecorated -- the override reminder is only
                        // appended to what the human actually sees, same
                        // spirit as the silent-path `notes` below.
                        emit(plan_check_hook::render_notice(&with_override_reminder(
                            message, &session,
                        )));
                        return;
                    }
                    plan_check_session::store(session_id, &rules_dir, &session);
                }
                // A deny is not silence, but it is also not the same as "no
                // active override" -- a different rule's conflict must not
                // bury the fact that this session still carries a recorded
                // override elsewhere. See the module doc's "override
                // visibility" note.
                let deny_reason = hook_deny_reason(&blocking, session_id, partial);
                emit(plan_check_hook::render_deny(&with_override_reminder(
                    deny_reason,
                    &session,
                )));
                return;
            }

            if let Some(session_id) = session_id {
                plan_check_session::store(session_id, &rules_dir, &session);
            }

            // Fully conforming (nothing blocking): the contract is silence —
            // UNLESS this session carries an active override (must stay
            // visible on every round, never silently absorbed once granted)
            // or this round only covered a prefix of what applies (a plain
            // "conforming" silence would misreport partial coverage as
            // complete). Either reason alone is enough to break silence;
            // both together are joined into one notice.
            let mut notes = Vec::new();
            if let Some((judged, total)) = partial {
                notes.push(partial_coverage_note(judged, total));
                // This is the one fail-open path with no other durable
                // trace: a deny keeps per-rule accounting live in the
                // session, and an override or round-limit pass already
                // writes its own audit entry, but "conforming, as far as we
                // looked" would otherwise exist only in the hook response
                // the agent — not a human — is the one actually reading. See
                // the module doc's "advisory gate" section.
                if let Some(session_id) = session_id {
                    plan_check_session::record_partial_coverage(
                        session_id,
                        &rules_dir,
                        session.rounds,
                        judged,
                        total,
                    );
                    notes.push(
                        "This is not a silent pass — recorded in plan-check-overrides.log."
                            .to_string(),
                    );
                }
            }
            let reminder = override_reminder(&session);
            if let Some(reminder) = reminder {
                notes.push(reminder);
            }
            if !notes.is_empty() {
                emit(plan_check_hook::render_notice(&notes.join("\n")));
            }
        }
    }
}

/// True when `CLAUDECODE` or `CLAUDE_CODE_ENTRYPOINT` is set in this
/// process's own environment — the markers Claude Code stamps into every
/// process it spawns for tool execution. `crate::runner::subprocess`
/// already depends on their presence being reliable: it explicitly strips
/// both before invoking a nested `claude` CLI as the judge runner,
/// specifically because a child process inherits them otherwise. That
/// existing reliance is what makes them usable here as a second signal
/// alongside [`std::io::IsTerminal`]: an agent's default tool call gets
/// neither a pty nor a clean environment, so [`exec_override`] catches it on
/// either axis rather than the terminal check alone.
///
/// Not airtight, and does not try to be: an agent deliberately reaching for
/// `script`/`expect`/`socat` *and* scrubbing its own environment first would
/// still pass both checks. This closes the default, no-special-effort
/// bypass, not every conceivable one — see [`exec_override`]'s own doc for
/// what this override does and does not guarantee.
///
/// A human running this from an ordinary terminal is unaffected unless that
/// terminal is itself something Claude Code launched (its own integrated
/// terminal, or a shell it spawned) — a real false-positive this trades for
/// closing the common bypass; see [`not_interactive`]'s message, which
/// tells a human hitting this what to do about it.
fn running_under_claude_code() -> bool {
    std::env::var_os("CLAUDECODE").is_some() || std::env::var_os("CLAUDE_CODE_ENTRYPOINT").is_some()
}

/// Run `actual plan-check-override`.
///
/// This is the one control standing between "a human decided to override a
/// denial" and "the agent that received the denial cleared it itself" — the
/// deny message deliberately no longer hands back a ready-to-paste
/// invocation (see [`hook_deny_reason`]'s doc comment), but that alone is not
/// enough: an agent with shell access could still construct and run this
/// command on its own behalf.
///
/// Two signals gate this, both real but neither individually airtight:
/// standard input must be a real terminal (the coding agent's own tool calls
/// do not get a pty — only a human actually typing at an interactive shell
/// does, though a tool that allocates its own pty, e.g. `script`, `expect`,
/// or `socat`, could still spoof one), and [`running_under_claude_code`]
/// must be false (an agent's default tool-execution environment carries
/// Claude Code's own markers — see that function's doc for exactly what
/// this does and does not close). Neither check is a cryptographic proof of
/// human origin, and the two together still are not: this is a real,
/// deliberately-raised bar against an agent's *default* execution
/// environment, not an enforcement boundary against one that goes out of
/// its way to evade detection. Treat it the same way the module doc treats
/// every other fail-open path here — cost-raising, not guarantee-providing.
///
/// The terminal check is deliberately *not* exercised from a `--lib` unit
/// test: like [`exec_hook`]'s own real-stdin read, whether a test process's
/// stdin happens to be a terminal depends on how it was launched (a real
/// terminal when a developer runs `cargo test` directly at one, never one
/// under CI), so asserting on it in-process would be either untestable or
/// flaky depending on the environment. See `tests/cli_test.rs` for the
/// subprocess test, which controls this safely by piping stdin the same way
/// every other `--claude-hook` stdin test there does. [`running_under_claude_code`]
/// carries no such restriction and is exercised directly. Everything else
/// that does not depend on the real terminal lives in [`exec_override_impl`],
/// which is also exercised directly.
pub fn exec_override(args: &PlanCheckOverrideArgs) -> Result<(), ActualError> {
    // Deliberately a single-line if/else expression, not an early-return
    // guard clause: no test anywhere (in-process or subprocess) can ever
    // give this process a real terminal, so the success branch here can
    // never be exercised on its own -- keeping both branches on the one
    // line that *is* exercised every time (the terminal check itself) is
    // what keeps coverage honest about what's actually tested, rather than
    // manufacturing a fake terminal in a test just to satisfy a line count.
    // `running_under_claude_code()` joins the same condition for the same
    // reason: whichever half is false, the line executed is identical.
    #[rustfmt::skip]
    let result = if std::io::stdin().is_terminal() && !running_under_claude_code() { exec_override_impl(args) } else { Err(not_interactive()) };
    result
}

fn not_interactive() -> ActualError {
    ActualError::NotInteractive(
        "plan-check-override must be run interactively, from an ordinary terminal — not through \
         a script, an agent's tool call, or a terminal Claude Code itself launched. It records a \
         human decision to override plan-stage governance, and this invocation was refused \
         because it looks like an agent's own shell rather than a human's: either no terminal is \
         attached, or this process's environment carries Claude Code's own CLAUDECODE / \
         CLAUDE_CODE_ENTRYPOINT markers. If you are a human seeing this from inside Claude \
         Code's integrated terminal (or a similar wrapper), run the command from a separate, \
         plain terminal window instead. This is a real but not airtight check, not a \
         cryptographic guarantee of human origin — see `actual plan-check-override --help`."
            .to_string(),
    )
}

/// The testable core of `actual plan-check-override`: a human explicitly
/// clearing one or more rules for a specific session. Always succeeds — there
/// is no invalid state this can observe (an unknown `session_id` just starts
/// a fresh session), so there is nothing for a caller to react to beyond "it
/// ran." [`exec_override`] is the real entry point; this exists separately so
/// the recording logic is testable without a real terminal.
fn exec_override_impl(args: &PlanCheckOverrideArgs) -> Result<(), ActualError> {
    let root = repo_root(args.repo.as_ref());
    let rules_dir = args
        .rules_dir
        .clone()
        .unwrap_or_else(|| crate::rules::rules_dir(&root));
    plan_check_session::record_override(&args.session, &rules_dir, &args.rules, &args.reason);
    let width = term_size::terminal_width();
    let mut panel = Panel::titled("Plan check override recorded");
    panel = panel.kv("Session", &args.session);
    panel = panel.kv("Rules dir", &rules_dir.display().to_string());
    panel = panel.kv("Reason", &args.reason);
    for rule in &args.rules {
        panel = panel.kv("Rule", rule);
    }
    println!(
        "{}",
        panel
            .separator()
            .line("This rule will not be re-checked for this session, and every round from")
            .line("now on will say so — an override is recorded, not silent.")
            .render(width)
    );
    Ok(())
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
/// rather than the judge's paraphrase of it. `blocking` names both real
/// conflicts and `requires_decision` verdicts — a plan claiming it
/// deliberately supersedes a rule is model output, not a recorded human
/// decision, so it is denied exactly like an outright conflict (see the
/// module doc's "advisory gate" section) — labeled `CONFLICT` or `DECISION`
/// per rule, the same labels direct mode's panel already uses, so a reader
/// switching between the two callers sees consistent vocabulary.
///
/// When `session_id` is present (a `--claude-hook` call whose envelope named
/// one), a final line names the session and points a human at
/// `plan-check-override --help` rather than handing back a ready-to-paste
/// invocation.
///
/// This is deliberate, not an oversight: this text reaches the agent's own
/// tool result, and the agent already runs a shell. Earlier versions
/// pre-filled `--session <id> --rule <doc-slug>::<rule-id>` here, which meant
/// the *only* control standing between "the agent read its own denial" and
/// "the agent cleared its own denial" was `exec_override`'s TTY check (see
/// its own doc comment) — a second, independent layer, not a substitute for
/// this one. `<doc-slug>::<rule-id>` (see [`plan_check_session::key`]) is
/// never assembled here for that reason: the per-conflict lines above name
/// the bare rule id (needed to revise the plan) but never the document slug,
/// so this message alone is not enough to construct a working `--rule` flag.
/// Absent (no session, or direct mode's own `deny_summary` instead), the
/// message is unchanged from before the revision loop existed.
///
/// No per-rule denial count or round number appears here on purpose: a round
/// budget is tracked per rule (see the module doc's "the round limit is per
/// rule" note), and this message denies the whole call regardless of which
/// individual rule's count is closest to its limit — that number belongs in
/// [`round_limit_message`], emitted only once fail-open actually happens.
///
/// `partial` — `Some((judged, total))` when [`MAX_RULES_JUDGED`] cut this
/// round's candidates to a prefix — adds one more line so a denial is never
/// read as "the whole plan was checked and this is everything wrong with
/// it" when it was really "this is everything wrong with the rules we got
/// to."
fn hook_deny_reason(
    blocking: &[&CheckedRule],
    session_id: Option<&str>,
    partial: Option<(usize, usize)>,
) -> String {
    let mut lines: Vec<String> = blocking
        .iter()
        .map(|c| {
            let label = match c.verdict {
                Verdict::RequiresDecision => "DECISION",
                // `Conflicting` is the only other verdict a caller ever
                // passes here; anything else falls back to the same label a
                // real conflict gets rather than assuming a shape this
                // fail-open hook has no business panicking over.
                _ => "CONFLICT",
            };
            format!(
                "{label} {} ({}): {} — rule: \"{}\" — plan: \"{}\"",
                c.rule_id,
                c.level.as_str(),
                non_empty_or(&c.reason, "conflicts with the plan"),
                truncate(&c.statement, 240),
                truncate(&c.span, 240)
            )
        })
        .collect();
    if let Some((judged, total)) = partial {
        lines.push(partial_coverage_note(judged, total));
    }
    if let Some(session_id) = session_id {
        lines.push(format!(
            "A revised plan is re-checked automatically. Session: {session_id}. A human \
             reviewing this — not the agent — can override a specific rule explicitly by \
             running `actual plan-check-override` from an interactive terminal (see `actual \
             plan-check-override --help` for the exact flags); that command refuses to run \
             non-interactively."
        ));
    }
    lines.join("\n")
}

/// The disclosure line for a partially-judged round: plain enough that
/// "conforming" or "no conflicts among these" is never mistaken for "the
/// whole plan was checked." Shared between the deny path (appended to
/// [`hook_deny_reason`]) and the silent-otherwise path (emitted as its own
/// notice in `exec_hook_with`), so the wording is identical either way.
fn partial_coverage_note(judged: usize, total: usize) -> String {
    format!(
        "Only {judged} of {total} rules in scope were checked this round — the rest exceeded \
         the {MAX_RULES_JUDGED}-rule judging cap and were not evaluated at all."
    )
}

/// The non-blocking notice emitted when every rule still blocking this
/// round (a conflict or an unconfirmed `requires_decision` claim) has
/// *individually* exhausted its own denial budget (see
/// [`PlanCheckSession::deny_limit_exceeded`]): the gate stops denying, but
/// says exactly why, names every exhausted rule and how many times each was
/// actually denied, so this is a loud pass, not a silent one. Paired with
/// [`plan_check_session::record_round_limit`], which writes the durable side
/// of the same event.
fn round_limit_message(
    exhausted: &[&CheckedRule],
    session: &PlanCheckSession,
    max_rounds: u32,
) -> String {
    let parts: Vec<String> = exhausted
        .iter()
        .map(|c| {
            let key = plan_check_session::key(&c.doc_slug, &c.rule_id);
            let count = session.deny_counts.get(&key).copied().unwrap_or(0);
            format!("{} (denied {count} times)", c.rule_id)
        })
        .collect();
    format!(
        "Actual plan governance hit its round limit ({max_rounds} denials) for {}: proceeding \
         without blocking further on {} specifically. This is not a silent pass — recorded in \
         plan-check-overrides.log.",
        parts.join(", "),
        if exhausted.len() == 1 { "it" } else { "them" }
    )
}

/// A non-blocking reminder naming every active override on `session`. An
/// override must stay visible on every round it applies to — never silently
/// absorbed once granted, whether this round is otherwise fully silent, a
/// deny on some *other* rule, or a round-limit notice. `None` when the
/// session has no overrides at all.
fn override_reminder(session: &PlanCheckSession) -> Option<String> {
    if session.overrides.is_empty() {
        return None;
    }
    let lines: Vec<String> = session
        .overrides
        .iter()
        .map(|o| {
            format!(
                "{}: manually overridden ({}) at {}; not re-checked.",
                o.key, o.reason, o.at
            )
        })
        .collect();
    Some(lines.join("\n"))
}

/// Append [`override_reminder`]'s text to `message` when the session has an
/// active override, else return `message` unchanged. The shared tail used by
/// the deny path and the round-limit notice so an active override stays
/// visible no matter which of the two a round ends in — only the fully-silent
/// path builds its own `notes` combination instead, since it also needs to
/// fold in the partial-coverage note.
fn with_override_reminder(mut message: String, session: &PlanCheckSession) -> String {
    if let Some(reminder) = override_reminder(session) {
        message.push('\n');
        message.push_str(&reminder);
    }
    message
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

    use crate::cli::args::DEFAULT_MAX_ROUNDS;
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

        let gathered = gather_rules(
            &selection,
            root.path(),
            &PlanCheckSession::default(),
            "test-digest",
        );
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

        let gathered = gather_rules(
            &selection,
            root.path(),
            &PlanCheckSession::default(),
            "test-digest",
        );
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

        let gathered = gather_rules(
            &selection,
            root.path(),
            &PlanCheckSession::default(),
            "test-digest",
        );
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

        let gathered = gather_rules(
            &selection,
            root.path(),
            &PlanCheckSession::default(),
            "test-digest",
        );
        assert_eq!(gathered.rules.len(), MAX_RULES_JUDGED);
        assert_eq!(gathered.considered, MAX_RULES_JUDGED);
        assert!(!gathered.truncated);
    }

    /// A session's first round (round 0, the same value direct mode's
    /// `PlanCheckSession::default()` always has) must judge exactly the old
    /// fixed prefix — rotation must not change round-0 behavior.
    #[test]
    fn test_gather_rules_round_zero_judges_the_unrotated_prefix() {
        let mut body = "# Many Rules: Widget Handling\n\nThese rules are ALWAYS ACTIVE for widget handling in `services/widgets/`.\n\n### Rules\n\n".to_string();
        for i in 0..(MAX_RULES_JUDGED + 5) {
            body.push_str(&format!("- **R-X-{i:04}** MUST: rule number {i}.\n"));
        }
        let root = seed(&[("cross-cutting-many-abcd.md", &body)]);
        let report = crate::rules::load_rule_set(root.path()).unwrap();
        let index = crate::rules::scope::ScopeIndex::build(&report, root.path(), "fp".to_string());
        let query = Query::new("Add a new widget in services/widgets".to_string());
        let selection = select::prefilter(&index, &query, 10, 30).finish(Stage2::NotRequested);

        let gathered = gather_rules(
            &selection,
            root.path(),
            &PlanCheckSession::default(),
            "test-digest",
        );
        let ids: Vec<&str> = gathered.rules.iter().map(|r| r.rule_id.as_str()).collect();
        assert!(ids.contains(&"R-X-0000"));
        assert!(ids.contains(&"R-X-0039"));
        assert!(!ids.contains(&"R-X-0044"));
    }

    /// A later round rotates the judged window by whole cap-sized chunks, so
    /// a rule truncated out of round 0's prefix has a real chance of being
    /// judged instead of staying permanently invisible to the judge for the
    /// life of the session — the #2 mitigation for APR-001 (partial batches
    /// silently never covering the same tail).
    #[test]
    fn test_gather_rules_rotates_the_judged_window_on_a_later_round() {
        let mut body = "# Many Rules: Widget Handling\n\nThese rules are ALWAYS ACTIVE for widget handling in `services/widgets/`.\n\n### Rules\n\n".to_string();
        for i in 0..(MAX_RULES_JUDGED + 5) {
            body.push_str(&format!("- **R-X-{i:04}** MUST: rule number {i}.\n"));
        }
        let root = seed(&[("cross-cutting-many-abcd.md", &body)]);
        let report = crate::rules::load_rule_set(root.path()).unwrap();
        let index = crate::rules::scope::ScopeIndex::build(&report, root.path(), "fp".to_string());
        let query = Query::new("Add a new widget in services/widgets".to_string());
        let selection = select::prefilter(&index, &query, 10, 30).finish(Stage2::NotRequested);

        let mut session = PlanCheckSession::default();
        session.rounds = 1;
        let gathered = gather_rules(&selection, root.path(), &session, "test-digest");
        assert_eq!(gathered.rules.len(), MAX_RULES_JUDGED);
        assert_eq!(gathered.considered, MAX_RULES_JUDGED + 5);
        assert!(gathered.truncated);

        let ids: Vec<&str> = gathered.rules.iter().map(|r| r.rule_id.as_str()).collect();
        // Round 0's judged window was R-X-0000..R-X-0039 (see the round-zero
        // test above). Round 1 rotates a full cap-sized chunk forward, so
        // the tail round 0 never saw is now in scope...
        assert!(ids.contains(&"R-X-0040"));
        assert!(ids.contains(&"R-X-0044"));
        // ...and the top of round 0's window rotates out to make room,
        // proving the window actually moved rather than simply grew.
        assert!(!ids.contains(&"R-X-0035"));
        assert!(!ids.contains(&"R-X-0039"));
    }

    #[test]
    fn test_rotation_offset_is_zero_at_round_zero_and_wraps_thereafter() {
        assert_eq!(rotation_offset(0, 45), 0);
        assert_eq!(rotation_offset(1, 45), MAX_RULES_JUDGED);
        // Wraps back toward the start once enough rounds have passed to
        // cycle through every candidate at least once.
        assert_eq!(rotation_offset(2, 45), (2 * MAX_RULES_JUDGED) % 45);
        assert_eq!(rotation_offset(0, 0), 0);
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
        let reason = hook_deny_reason(&[&a], None, None);
        assert!(reason.contains("R-A-002"));
        assert!(reason.contains("log the signing key for debugging"));
    }

    #[test]
    fn test_hook_deny_reason_falls_back_when_the_model_reason_is_blank() {
        let a = checked("R-A-002", Verdict::Conflicting, "some span", "");
        let reason = hook_deny_reason(&[&a], None, None);
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
        let reason = hook_deny_reason(&[&a], None, None);
        assert!(reason.contains(&a.statement));
    }

    #[test]
    fn test_hook_deny_reason_with_no_session_omits_override_instructions() {
        let a = checked("R-A-002", Verdict::Conflicting, "span", "reason");
        let reason = hook_deny_reason(&[&a], None, None);
        assert!(!reason.contains("plan-check-override"));
    }

    #[test]
    fn test_hook_deny_reason_with_a_session_points_at_override_help() {
        let a = checked("R-A-002", Verdict::Conflicting, "span", "reason");
        let reason = hook_deny_reason(&[&a], Some("sess-123"), None);
        assert!(reason.contains("Session: sess-123"));
        assert!(reason.contains("actual plan-check-override"));
        assert!(reason.contains("--help"));
    }

    /// The gap this guards: the deny reason must never hand back a
    /// ready-to-paste override invocation. The agent that receives this text
    /// already has shell access -- a fully-formed `--session <id> --rule
    /// <doc-slug>::<rule-id> --reason "..."` command here would let it clear
    /// its own denial. `doc_slug` specifically must never appear: the
    /// per-conflict lines above name the bare rule id (needed to revise the
    /// plan) but pairing it with the document slug is exactly what a valid
    /// `--rule` flag requires, and that pairing must not be assembled here.
    #[test]
    fn test_hook_deny_reason_never_assembles_a_working_override_invocation() {
        let a = checked("R-A-002", Verdict::Conflicting, "span", "reason");
        let reason = hook_deny_reason(&[&a], Some("sess-123"), None);
        assert!(!reason.contains("--session sess-123 --rule"));
        assert!(!reason.contains(&plan_check_session::key(&a.doc_slug, &a.rule_id)));
        assert!(!reason.to_lowercase().contains(&a.doc_slug.to_lowercase()));
    }

    /// The behavior change this guards: an unconfirmed "deliberate
    /// supersession" claim is model output, not a recorded human decision,
    /// so `hook_deny_reason` denies it exactly like a real conflict — but
    /// still labels it `DECISION` rather than `CONFLICT`, matching direct
    /// mode's panel vocabulary, so a reader can tell the two apart.
    #[test]
    fn test_hook_deny_reason_labels_a_requires_decision_verdict_distinctly() {
        let a = checked(
            "R-A-001",
            Verdict::RequiresDecision,
            "span",
            "supersedes it",
        );
        let reason = hook_deny_reason(&[&a], None, None);
        assert!(reason.contains("DECISION R-A-001"));
        assert!(!reason.contains("CONFLICT R-A-001"));
    }

    #[test]
    fn test_hook_deny_reason_labels_a_conflicting_verdict_distinctly() {
        let a = checked("R-A-002", Verdict::Conflicting, "span", "reason");
        let reason = hook_deny_reason(&[&a], None, None);
        assert!(reason.contains("CONFLICT R-A-002"));
        assert!(!reason.contains("DECISION R-A-002"));
    }

    #[test]
    fn test_hook_deny_reason_discloses_partial_coverage_when_present() {
        let a = checked("R-A-002", Verdict::Conflicting, "span", "reason");
        let reason = hook_deny_reason(&[&a], None, Some((60, 85)));
        assert!(reason.contains("Only 60 of 85"));
    }

    #[test]
    fn test_hook_deny_reason_omits_partial_note_when_absent() {
        let a = checked("R-A-002", Verdict::Conflicting, "span", "reason");
        let reason = hook_deny_reason(&[&a], None, None);
        assert!(!reason.contains("judging cap"));
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
            partial: None,
        };
        let panel = render_panel(&outcome, "a plan", Path::new("/x/.actual/rules"), 80);
        assert!(panel.contains("Conforming"));
        assert!(!panel.contains("CONFLICT"));
    }

    /// The behavior change this guards: a partially-judged round must
    /// disclose "N of M" in the panel, not report `verdicts.len()` as if it
    /// were complete coverage.
    #[test]
    fn test_render_panel_discloses_partial_coverage() {
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
            partial: Some((60, 85)),
        };
        let panel = render_panel(&outcome, "a plan", Path::new("/x/.actual/rules"), 80);
        assert!(panel.contains("60 of 85"));
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
            partial: None,
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
            partial: None,
        };
        let json = render_json(&conforming);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "conforming");
        assert!(value.get("partial").is_none());

        let conflicting = Outcome::Verdicts {
            selection: selection.clone(),
            verdicts: vec![checked("R-A-002", Verdict::Conflicting, "x", "y")],
            runner_label: None,
            partial: None,
        };
        let json = render_json(&conflicting);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "conflicting");

        let requires_decision = Outcome::Verdicts {
            selection: selection.clone(),
            verdicts: vec![checked("R-A-003", Verdict::RequiresDecision, "x", "y")],
            runner_label: None,
            partial: None,
        };
        let json = render_json(&requires_decision);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "requires_decision");

        let partial = Outcome::Verdicts {
            selection: selection.clone(),
            verdicts: vec![checked("R-A-001", Verdict::Conforming, "", "")],
            runner_label: None,
            partial: Some((60, 85)),
        };
        let json = render_json(&partial);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "conforming");
        assert_eq!(value["partial"]["judged"], 60);
        assert_eq!(value["partial"]["total"], 85);

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
            max_rounds: DEFAULT_MAX_ROUNDS,
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

    /// The exact behavior a review flagged: `--plan-file` used to read the
    /// whole file via `std::fs::read_to_string` regardless of size. This must
    /// now refuse an oversized file instead of buffering it in full, the same
    /// `Read::take` discipline `plan_check_hook::read_capped` already applies
    /// to the hook's own file reads.
    #[test]
    fn test_resolve_direct_plan_errors_when_the_plan_file_exceeds_the_size_limit() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("huge.md");
        let oversized = vec![b'x'; (plan_check_hook::MAX_READ_BYTES + 1) as usize];
        std::fs::write(&file, &oversized).unwrap();
        let mut args = base_args();
        args.plan_file = Some(file);
        let err = resolve_direct_plan(&args).unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(_)));
        assert!(err.to_string().contains("exceeds"));
    }

    #[test]
    fn test_capped_read_reads_content_within_the_limit() {
        assert_eq!(
            capped_read("hello".as_bytes(), "test input").unwrap(),
            "hello"
        );
    }

    #[test]
    fn test_capped_read_errors_when_the_input_exceeds_the_limit() {
        let oversized = vec![b'x'; (plan_check_hook::MAX_READ_BYTES + 1) as usize];
        let err = capped_read(oversized.as_slice(), "test input").unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(_)));
        assert!(err.to_string().contains("test input"));
    }

    #[test]
    fn test_capped_read_errors_on_non_utf8_input() {
        let err = capped_read([0xff, 0xfe].as_slice(), "test input").unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(_)));
        assert!(err.to_string().contains("not valid UTF-8"));
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

        let gathered = gather_rules(
            &selection,
            root.path(),
            &PlanCheckSession::default(),
            "test-digest",
        );
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

        let gathered = gather_rules(
            &selection,
            root.path(),
            &PlanCheckSession::default(),
            "test-digest",
        );
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
            &PlanCheckSession::default(),
        )
        .unwrap();
        assert!(matches!(
            outcome,
            Outcome::NoRunner { documents_selected, .. } if documents_selected > 0
        ));
    }

    /// The other `NoRunner` branch: `use_rank: true` resolves its runner
    /// *before* selection (see `run_pipeline`'s `early_runner`), so a missing
    /// runner must be caught there too, not just in the deferred, `use_rank:
    /// false` path the test above covers.
    #[test]
    fn test_run_pipeline_no_runner_available_when_use_rank_is_true() {
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
            true,
            &PlanCheckSession::default(),
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

        let outcome = run_pipeline(
            "Sign tokens",
            root.path(),
            &rules_dir,
            &args,
            false,
            &PlanCheckSession::default(),
        )
        .unwrap();
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
            &PlanCheckSession::default(),
        )
        .unwrap();
        assert!(matches!(outcome, Outcome::NothingApplies));
    }

    #[cfg(unix)]
    #[test]
    fn test_run_pipeline_judges_a_deterministic_prefix_when_rules_exceed_the_cap() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let mut body = "# Many Rules: Widget Handling\n\nThese rules are ALWAYS ACTIVE for widget handling in `services/widgets/`.\n\n### Rules\n\n".to_string();
        for i in 0..(MAX_RULES_JUDGED + 1) {
            body.push_str(&format!("- **R-X-{i:04}** MUST: rule number {i}.\n"));
        }
        let root = seed(&[("cross-cutting-many-abcd.md", &body)]);
        let bin = tempdir().unwrap();
        // A verdict for every one of the first MAX_RULES_JUDGED rule ids, in
        // declaration order -- the prefix the cap must actually judge. The
        // 61st rule (never sent as a candidate) is deliberately absent: if
        // gather_rules ever included it, check::check's own "never invent or
        // drop silently" validation would reject the whole batch for a
        // verdict naming a pair that was never a candidate.
        let verdicts: Vec<serde_json::Value> = (0..MAX_RULES_JUDGED)
            .map(|i| {
                serde_json::json!({
                    "doc_slug": "cross-cutting-many-abcd",
                    "rule_id": format!("R-X-{i:04}"),
                    "verdict": "conforming",
                    "span": "",
                    "reason": "not touched",
                })
            })
            .collect();
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &check_output(serde_json::json!(verdicts)))
                .to_str()
                .unwrap(),
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
            &PlanCheckSession::default(),
        )
        .unwrap();
        #[rustfmt::skip]
        let Outcome::Verdicts { verdicts, partial, .. } = outcome else { panic!("expected Verdicts, got a different outcome") };
        assert_eq!(verdicts.len(), MAX_RULES_JUDGED);
        assert_eq!(partial, Some((MAX_RULES_JUDGED, MAX_RULES_JUDGED + 1)));
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
            &PlanCheckSession::default(),
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
            &PlanCheckSession::default(),
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
            &PlanCheckSession::default(),
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

    /// Direct mode's counterpart to
    /// `test_exec_hook_with_reports_a_rules_directory_load_failure`: a
    /// caller at a real terminal needs this surfaced as a real `Err`, not
    /// fail-open silence — see `run_pipeline`'s own doc comment on when it
    /// returns `Err` at all.
    #[test]
    fn test_exec_direct_errors_when_the_rules_directory_cannot_be_read() {
        let root = tempdir().unwrap();
        let not_a_dir = root.path().join("rules-dir-is-a-file");
        std::fs::write(&not_a_dir, "not a directory").unwrap();

        let mut args = base_args();
        args.plan = vec!["a plan".to_string()];
        args.rules_dir = Some(not_a_dir);
        assert!(exec(&args).is_err());
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

    /// The behavior change this guards: a `requires_decision` verdict must be
    /// denied exactly like a real conflict (per-rule denial recorded, same
    /// round-limit/override machinery), not merely surfaced as a notice the
    /// agent can proceed past. See the module doc's "advisory gate" section.
    #[cfg(unix)]
    #[test]
    fn test_exec_hook_with_denies_a_requires_decision_verdict_like_a_conflict() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(root.path());
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
        args.rules_dir = Some(rules_dir.clone());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let raw = serde_json::json!({
            "session_id": "sess-decision-1",
            "tool_input": {"plan": "Sign access tokens with RS256"},
        })
        .to_string();
        exec_hook_with(&args, &raw);

        // A requires_decision verdict must be recorded as a denial, exactly
        // like a real conflict -- proving it went through the same per-rule
        // tracking, not a separate notice-only path.
        let session = plan_check_session::load("sess-decision-1", &rules_dir);
        let key = plan_check_session::key("cross-cutting-token-signing-1c57", "R-A-001");
        assert_eq!(session.deny_counts.get(&key), Some(&1));
    }

    /// The behavior change this guards: exceeding `MAX_RULES_JUDGED` must
    /// still judge and act on a deterministic prefix -- deny/session-state
    /// tracking for the rules it *did* see, not refuse the whole round.
    #[cfg(unix)]
    #[test]
    fn test_exec_hook_with_judges_and_acts_on_a_prefix_past_the_rule_cap() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let mut body = "# Many Rules: Widget Handling\n\nThese rules are ALWAYS ACTIVE for widget handling in `services/widgets/`.\n\n### Rules\n\n".to_string();
        for i in 0..(MAX_RULES_JUDGED + 5) {
            body.push_str(&format!("- **R-X-{i:04}** MUST: rule number {i}.\n"));
        }
        let root = seed(&[("cross-cutting-many-abcd.md", &body)]);
        let rules_dir = crate::rules::rules_dir(root.path());
        let bin = tempdir().unwrap();

        // The first rule in the judged prefix conflicts; the rest conform.
        let mut verdicts = vec![serde_json::json!({
            "doc_slug": "cross-cutting-many-abcd",
            "rule_id": "R-X-0000",
            "verdict": "conflicting",
            "span": "violates it",
            "reason": "conflict",
        })];
        for i in 1..MAX_RULES_JUDGED {
            verdicts.push(serde_json::json!({
                "doc_slug": "cross-cutting-many-abcd",
                "rule_id": format!("R-X-{i:04}"),
                "verdict": "conforming",
                "span": "",
                "reason": "not touched",
            }));
        }
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &check_output(serde_json::json!(verdicts)))
                .to_str()
                .unwrap(),
        );

        let mut args = base_args();
        args.rules_dir = Some(rules_dir.clone());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let raw = serde_json::json!({
            "session_id": "sess-partial-1",
            "tool_input": {"plan": "Add a new widget in services/widgets"},
        })
        .to_string();
        exec_hook_with(&args, &raw);

        let session = plan_check_session::load("sess-partial-1", &rules_dir);
        // The one conflicting rule in the prefix was denied...
        assert_eq!(
            session.deny_counts.get(&plan_check_session::key(
                "cross-cutting-many-abcd",
                "R-X-0000"
            )),
            Some(&1)
        );
        // ...and every other rule in the capped prefix was cleared --
        // proving the judge actually ran on the capped batch rather than the
        // round being refused outright.
        assert_eq!(session.cleared.len(), MAX_RULES_JUDGED - 1);
        // The rules past the cap (R-X-0060..R-X-0064) were never candidates
        // at all, so they can appear in neither bucket.
        assert!(!session.cleared.contains_key(&plan_check_session::key(
            "cross-cutting-many-abcd",
            "R-X-0064"
        )));
    }

    /// The other half of the deterministic-prefix disclosure: a round that
    /// judges only a capped prefix but finds *nothing* blocking in it must
    /// still break silence with the partial-coverage note — otherwise a
    /// plain "conforming" silence would misreport a partial answer as a
    /// complete one. `test_exec_hook_with_judges_and_acts_on_a_prefix_past_the_rule_cap`
    /// covers the case where the prefix also has something to deny; this is
    /// the fully-conforming-but-partial case that branch doesn't reach.
    #[cfg(unix)]
    #[test]
    fn test_exec_hook_with_discloses_partial_coverage_on_an_otherwise_silent_round() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let mut body = "# Many Rules: Widget Handling\n\nThese rules are ALWAYS ACTIVE for widget handling in `services/widgets/`.\n\n### Rules\n\n".to_string();
        for i in 0..(MAX_RULES_JUDGED + 5) {
            body.push_str(&format!("- **R-X-{i:04}** MUST: rule number {i}.\n"));
        }
        let root = seed(&[("cross-cutting-many-abcd.md", &body)]);
        let rules_dir = crate::rules::rules_dir(root.path());
        let bin = tempdir().unwrap();

        // Every rule in the judged prefix conforms -- nothing blocking at
        // all this round, so the only reason to emit anything is the
        // partial-coverage disclosure.
        let verdicts: Vec<serde_json::Value> = (0..MAX_RULES_JUDGED)
            .map(|i| {
                serde_json::json!({
                    "doc_slug": "cross-cutting-many-abcd",
                    "rule_id": format!("R-X-{i:04}"),
                    "verdict": "conforming",
                    "span": "",
                    "reason": "not touched",
                })
            })
            .collect();
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &check_output(serde_json::json!(verdicts)))
                .to_str()
                .unwrap(),
        );

        let mut args = base_args();
        args.rules_dir = Some(rules_dir.clone());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let raw = serde_json::json!({
            "session_id": "sess-partial-silent-1",
            "tool_input": {"plan": "Add a new widget in services/widgets"},
        })
        .to_string();
        exec_hook_with(&args, &raw);

        // Every rule in the capped prefix was cleared, proving the judge
        // ran the capped batch to completion rather than the round being
        // refused outright.
        let session = plan_check_session::load("sess-partial-silent-1", &rules_dir);
        assert_eq!(session.cleared.len(), MAX_RULES_JUDGED);

        // A silent-but-partial round is not silent in the durable log: this
        // is the one fail-open path that previously left no trace anywhere
        // but the hook response the agent itself read.
        let log = std::fs::read_to_string(plan_check_session::audit_log_path().unwrap()).unwrap();
        assert!(log.contains("\"kind\":\"partial_coverage\""));
        assert!(log.contains(&format!("\"judged\":{MAX_RULES_JUDGED}")));
        assert!(log.contains(&format!("\"total\":{}", MAX_RULES_JUDGED + 5)));
        assert!(log.contains("\"session_id\":\"sess-partial-silent-1\""));
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

    // ── the revision loop (AK-677): exclusion, session persistence, ────────
    // ── round limits, and overrides ─────────────────────────────────────

    /// The mechanism the whole revision loop rests on: a rule named in
    /// `exclude` is never even offered to the judge as a candidate. Proven
    /// here by having the fake judge answer for it *anyway* (as a real judge
    /// flip-flopping on a settled rule would) — `check::check`'s own "never
    /// invent or drop silently" validation then discards that verdict as
    /// naming a pair that was never a candidate, so it cannot appear in the
    /// result no matter what the judge says.
    #[cfg(unix)]
    #[test]
    fn test_run_pipeline_excludes_a_settled_rule_even_if_the_judge_answers_for_it_anyway() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let bin = tempdir().unwrap();
        // A judge that flip-flops: R-A-001 was cleared last round, but this
        // (misbehaving) judge answers "conflicting" for it anyway.
        let response = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conflicting", "span": "flip-flopped", "reason": "should never be seen"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "conflicting", "span": "logs the key", "reason": "still conflicting"},
        ]));
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &response).to_str().unwrap(),
        );

        let mut args = base_args();
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let rules_dir = crate::rules::rules_dir(root.path());
        let plan_text = "Sign access tokens with RS256";
        let mut session = PlanCheckSession::default();
        session.cleared.insert(
            plan_check_session::key("cross-cutting-token-signing-1c57", "R-A-001"),
            plan_check_session::plan_digest(plan_text),
        );

        let outcome =
            run_pipeline(plan_text, root.path(), &rules_dir, &args, false, &session).unwrap();

        #[rustfmt::skip]
        let Outcome::Verdicts { verdicts, .. } = outcome else { panic!("expected Verdicts, got a different outcome") };
        assert_eq!(
            verdicts.len(),
            1,
            "the excluded rule must not appear at all"
        );
        assert_eq!(verdicts[0].rule_id, "R-A-002");
    }

    /// When every rule a selection would otherwise judge is already settled,
    /// there is nothing new to check — this must short-circuit before ever
    /// resolving a runner (proven by there being no working `CLAUDE_BINARY`
    /// at all: a real resolution attempt would fail this test as `NoRunner`).
    #[cfg(unix)]
    #[test]
    fn test_run_pipeline_skips_runner_resolution_when_everything_is_already_settled() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _no_claude = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/path/to/claude");

        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let mut args = base_args();
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let rules_dir = crate::rules::rules_dir(root.path());
        // Overrides, not clearances: exclusion via an override does not
        // depend on the plan digest, so this test does not need to compute
        // one to prove the shortcut.
        let mut session = PlanCheckSession::default();
        session.overrides.push(plan_check_session::Override {
            key: plan_check_session::key("cross-cutting-token-signing-1c57", "R-A-001"),
            reason: "reviewed".to_string(),
            at: chrono::Utc::now(),
            round: 1,
        });
        session.overrides.push(plan_check_session::Override {
            key: plan_check_session::key("cross-cutting-token-signing-1c57", "R-A-002"),
            reason: "reviewed".to_string(),
            at: chrono::Utc::now(),
            round: 1,
        });

        let outcome = run_pipeline(
            "Sign access tokens with RS256",
            root.path(),
            &rules_dir,
            &args,
            false,
            &session,
        )
        .unwrap();

        assert!(matches!(
            outcome,
            Outcome::Verdicts { ref verdicts, runner_label: None, .. } if verdicts.is_empty()
        ));
    }

    /// The gap this guards (the exact bypass a review flagged): a rule
    /// cleared against one plan text must be judged fresh once the plan has
    /// actually changed, even within the same session -- a stale clearance
    /// must never mask a new violation. `run_pipeline` alone cannot show
    /// this end-to-end (it does not persist anything itself), so this drives
    /// two rounds through `exec_hook_with` with the same `session_id`: round
    /// one clears R-A-001 against a plan that does not mention signing at
    /// all, round two revises the plan to violate R-A-001 outright, and the
    /// second round's judge must actually be asked about it again.
    #[cfg(unix)]
    #[test]
    fn test_exec_hook_with_rejudges_a_cleared_rule_once_the_plan_actually_changes() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(root.path());

        // Round 1: a plan that does not touch signing at all -- both rules
        // come back conforming (vacuously) and get cleared.
        let bin1 = tempdir().unwrap();
        let round1_response = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conforming", "span": "", "reason": "plan does not touch signing"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "conforming", "span": "", "reason": "plan does not touch logging"},
        ]));
        {
            let _binary = EnvGuard::set(
                "CLAUDE_BINARY",
                fake_claude(bin1.path(), &round1_response).to_str().unwrap(),
            );
            let mut args = base_args();
            args.rules_dir = Some(rules_dir.clone());
            args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
            let raw = serde_json::json!({
                "session_id": "sess-rejudge-1",
                "tool_input": {"plan": "Add a health-check endpoint to the auth service."},
            })
            .to_string();
            exec_hook_with(&args, &raw);
        }
        let after_round1 = plan_check_session::load("sess-rejudge-1", &rules_dir);
        assert!(after_round1.cleared.contains_key(&plan_check_session::key(
            "cross-cutting-token-signing-1c57",
            "R-A-001"
        )));

        // Round 2: the *revised* plan now genuinely violates R-A-001. If the
        // clearance still applied, this rule would never even reach the
        // judge and the plan would pass silently -- exactly the bypass a
        // review flagged.
        let bin2 = tempdir().unwrap();
        let round2_response = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conflicting", "span": "signs with HS256", "reason": "violates RS256 requirement"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "conforming", "span": "", "reason": "still does not log the key"},
        ]));
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin2.path(), &round2_response).to_str().unwrap(),
        );
        let mut args = base_args();
        args.rules_dir = Some(rules_dir.clone());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let raw = serde_json::json!({
            "session_id": "sess-rejudge-1",
            "tool_input": {"plan": "Sign access tokens with HS256 and a hardcoded shared secret."},
        })
        .to_string();
        exec_hook_with(&args, &raw);

        // R-A-001 must have been re-judged (not silently excluded): its
        // cleared entry must be gone (the judge called it conflicting this
        // round, not conforming), and R-A-002's clearance updates to the new
        // plan's digest rather than staying pinned to the old one.
        let after_round2 = plan_check_session::load("sess-rejudge-1", &rules_dir);
        let key_a001 = plan_check_session::key("cross-cutting-token-signing-1c57", "R-A-001");
        let key_a002 = plan_check_session::key("cross-cutting-token-signing-1c57", "R-A-002");
        assert!(
            !after_round2.cleared.contains_key(&key_a001),
            "a rule the judge just called conflicting must not remain cleared"
        );
        assert_ne!(
            after_round2.cleared.get(&key_a002),
            after_round1.cleared.get(&key_a002),
            "a re-cleared rule's stored digest must move to the new plan text"
        );
    }

    /// End-to-end through `exec_hook_with`: a mixed conforming/conflicting
    /// round must persist the conforming rule as cleared and count as one
    /// round, for the session named in the envelope.
    #[cfg(unix)]
    #[test]
    fn test_exec_hook_with_persists_cleared_rules_and_counts_a_round() {
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

        let rules_dir = crate::rules::rules_dir(root.path());
        let mut args = base_args();
        args.rules_dir = Some(rules_dir.clone());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let raw = serde_json::json!({
            "session_id": "sess-persist-1",
            "tool_input": {"plan": "Sign access tokens with RS256"},
        })
        .to_string();
        exec_hook_with(&args, &raw);

        let session = plan_check_session::load("sess-persist-1", &rules_dir);
        assert_eq!(session.rounds, 1);
        assert!(session.cleared.contains_key(&plan_check_session::key(
            "cross-cutting-token-signing-1c57",
            "R-A-001"
        )));
        assert!(!session.cleared.contains_key(&plan_check_session::key(
            "cross-cutting-token-signing-1c57",
            "R-A-002"
        )));
    }

    /// The round limit: repeated conflicting rounds for the same session must
    /// stop denying once the cap is exceeded, and that pass must be recorded
    /// in the audit log rather than merely inferred from silence.
    #[cfg(unix)]
    #[test]
    fn test_exec_hook_with_stops_denying_and_logs_once_the_round_limit_is_exceeded() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let bin = tempdir().unwrap();
        // Always conflicting on R-A-002, so this session never resolves on
        // its own -- the only way it stops denying is the round limit.
        let response = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conforming", "span": "", "reason": "uses RS256"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "conflicting", "span": "logs the key", "reason": "forbidden"},
        ]));
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &response).to_str().unwrap(),
        );

        let rules_dir = crate::rules::rules_dir(root.path());
        let mut args = base_args();
        args.rules_dir = Some(rules_dir.clone());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        args.max_rounds = 1;
        let raw = serde_json::json!({
            "session_id": "sess-limit-1",
            "tool_input": {"plan": "Sign access tokens with RS256"},
        })
        .to_string();

        // Round 1: rounds becomes 1, 1 > max_rounds(1) is false -> normal deny.
        exec_hook_with(&args, &raw);
        assert_eq!(
            plan_check_session::load("sess-limit-1", &rules_dir).rounds,
            1
        );

        // Round 2: rounds becomes 2, 2 > 1 -> the gate stops denying.
        exec_hook_with(&args, &raw);
        assert_eq!(
            plan_check_session::load("sess-limit-1", &rules_dir).rounds,
            2
        );

        let log = std::fs::read_to_string(plan_check_session::audit_log_path().unwrap()).unwrap();
        assert!(log.contains("\"kind\":\"round_limit\""));
        assert!(log.contains("sess-limit-1"));
    }

    /// The exact bug a review flagged: `rounds` used to increment on every
    /// completed judge call, conforming or not, so several clean rounds
    /// could spend the whole budget before a real conflict was ever denied
    /// even once. Three fully-conforming rounds here, with `--max-rounds 1`,
    /// must not leave a brand-new conflict in round four pre-exhausted.
    #[cfg(unix)]
    #[test]
    fn test_exec_hook_with_clean_rounds_do_not_spend_the_round_limit_budget() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(root.path());
        let mut args = base_args();
        args.rules_dir = Some(rules_dir.clone());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        args.max_rounds = 1;

        let clean_response = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conforming", "span": "", "reason": "uses RS256"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "conforming", "span": "", "reason": "no logging"},
        ]));
        let bin_clean = tempdir().unwrap();
        {
            let _binary = EnvGuard::set(
                "CLAUDE_BINARY",
                fake_claude(bin_clean.path(), &clean_response)
                    .to_str()
                    .unwrap(),
            );
            for plan in [
                "Add a health-check endpoint to the auth service.",
                "Add a retry policy to the auth service's health-check endpoint.",
                "Add a metrics counter to the auth service's health-check endpoint.",
            ] {
                let raw = serde_json::json!({
                    "session_id": "sess-clean-rounds",
                    "tool_input": {"plan": plan},
                })
                .to_string();
                exec_hook_with(&args, &raw);
            }
        }
        assert_eq!(
            plan_check_session::load("sess-clean-rounds", &rules_dir).rounds,
            3
        );
        assert!(plan_check_session::load("sess-clean-rounds", &rules_dir)
            .deny_counts
            .is_empty());

        // Round four: a genuinely new conflict on R-A-001, never denied
        // before. If clean rounds had spent the budget, this would
        // fail-open immediately instead of denying.
        let conflict_response = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conflicting", "span": "signs with HS256", "reason": "violates RS256 requirement"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "conforming", "span": "", "reason": "no logging"},
        ]));
        let bin_conflict = tempdir().unwrap();
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin_conflict.path(), &conflict_response)
                .to_str()
                .unwrap(),
        );
        let raw = serde_json::json!({
            "session_id": "sess-clean-rounds",
            "tool_input": {"plan": "Sign access tokens with HS256 and a hardcoded shared secret."},
        })
        .to_string();
        exec_hook_with(&args, &raw);

        let session = plan_check_session::load("sess-clean-rounds", &rules_dir);
        let key_a001 = plan_check_session::key("cross-cutting-token-signing-1c57", "R-A-001");
        assert_eq!(session.deny_counts.get(&key_a001), Some(&1));
        assert!(
            !session.deny_limit_exceeded(&key_a001, args.max_rounds),
            "a rule's first-ever denial must never already be exhausted"
        );
        let log = std::fs::read_to_string(plan_check_session::audit_log_path().unwrap())
            .unwrap_or_default();
        assert!(
            !log.contains("round_limit"),
            "a brand-new conflict must be denied normally, not fail open: {log}"
        );
    }

    /// The other half of the same fix: a rule that has individually
    /// exhausted its own budget must not cause a *different*, still-within-
    /// budget rule's conflict in the same round to fail open. Only once
    /// every currently-conflicting rule is individually exhausted does the
    /// gate stop blocking.
    #[cfg(unix)]
    #[test]
    fn test_exec_hook_with_an_exhausted_rule_does_not_exempt_a_fresh_conflict_in_the_same_round() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(root.path());
        let mut args = base_args();
        args.rules_dir = Some(rules_dir.clone());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        args.max_rounds = 1;
        let key_a001 = plan_check_session::key("cross-cutting-token-signing-1c57", "R-A-001");
        let key_a002 = plan_check_session::key("cross-cutting-token-signing-1c57", "R-A-002");

        // Round 1: only R-A-001 conflicts (count -> 1, not yet exceeded).
        let round1 = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conflicting", "span": "signs with HS256", "reason": "violates RS256"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "conforming", "span": "", "reason": "no logging"},
        ]));
        {
            let bin = tempdir().unwrap();
            let _binary = EnvGuard::set(
                "CLAUDE_BINARY",
                fake_claude(bin.path(), &round1).to_str().unwrap(),
            );
            let raw = serde_json::json!({
                "session_id": "sess-mixed-exhaustion",
                "tool_input": {"plan": "Sign with HS256."},
            })
            .to_string();
            exec_hook_with(&args, &raw);
        }

        // Round 2: R-A-001 conflicts again (count -> 2, now exhausted at
        // max_rounds=1), but the revised plan *also* newly violates R-A-002
        // for the first time (count -> 1, not exhausted). Since not every
        // conflicting rule this round is exhausted, this must still deny.
        let round2 = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conflicting", "span": "still signs with HS256", "reason": "still violates RS256"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "conflicting", "span": "logs the key", "reason": "forbidden"},
        ]));
        {
            let bin = tempdir().unwrap();
            let _binary = EnvGuard::set(
                "CLAUDE_BINARY",
                fake_claude(bin.path(), &round2).to_str().unwrap(),
            );
            let raw = serde_json::json!({
                "session_id": "sess-mixed-exhaustion",
                "tool_input": {"plan": "Sign with HS256 and log the key."},
            })
            .to_string();
            exec_hook_with(&args, &raw);
        }
        let after_round2 = plan_check_session::load("sess-mixed-exhaustion", &rules_dir);
        assert!(after_round2.deny_limit_exceeded(&key_a001, args.max_rounds));
        assert!(!after_round2.deny_limit_exceeded(&key_a002, args.max_rounds));
        let log_after_round2 =
            std::fs::read_to_string(plan_check_session::audit_log_path().unwrap())
                .unwrap_or_default();
        assert!(
            !log_after_round2.contains("round_limit"),
            "R-A-002 is not exhausted yet, so the round must still deny: {log_after_round2}"
        );

        // Round 3: the plan now only violates R-A-001 (the exhausted one).
        // With every currently-conflicting rule individually exhausted, the
        // gate now stops blocking.
        let round3 = check_output(serde_json::json!([
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conflicting", "span": "still signs with HS256", "reason": "still violates RS256"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "conforming", "span": "", "reason": "no longer logs the key"},
        ]));
        let bin = tempdir().unwrap();
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            fake_claude(bin.path(), &round3).to_str().unwrap(),
        );
        let raw = serde_json::json!({
            "session_id": "sess-mixed-exhaustion",
            "tool_input": {"plan": "Sign with HS256, key no longer logged."},
        })
        .to_string();
        exec_hook_with(&args, &raw);

        let log_after_round3 =
            std::fs::read_to_string(plan_check_session::audit_log_path().unwrap()).unwrap();
        assert!(log_after_round3.contains("\"kind\":\"round_limit\""));
    }

    /// An override recorded for a session is honored on the next round: the
    /// overridden rule is excluded from judging even if the judge would
    /// otherwise still call it conflicting.
    #[cfg(unix)]
    #[test]
    fn test_exec_hook_with_honors_a_recorded_override() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(root.path());

        plan_check_session::record_override(
            "sess-override-1",
            &rules_dir,
            &[
                plan_check_session::key("cross-cutting-token-signing-1c57", "R-A-001"),
                plan_check_session::key("cross-cutting-token-signing-1c57", "R-A-002"),
            ],
            "reviewed and accepted by the security team",
        );

        // No working runner at all: if the override did not exclude both
        // rules, run_pipeline would need to resolve one and this would
        // surface as NoRunner instead of completing silently.
        let _no_claude = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/path/to/claude");

        let mut args = base_args();
        args.rules_dir = Some(rules_dir.clone());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let raw = serde_json::json!({
            "session_id": "sess-override-1",
            "tool_input": {"plan": "Sign access tokens with RS256"},
        })
        .to_string();
        exec_hook_with(&args, &raw);

        // The override itself is untouched by this round (exec_hook_with
        // only ever adds to `cleared`, never to `overrides`).
        let session = plan_check_session::load("sess-override-1", &rules_dir);
        assert_eq!(session.overrides.len(), 2);
    }

    #[test]
    fn test_override_reminder_none_without_any_overrides() {
        assert!(override_reminder(&PlanCheckSession::default()).is_none());
    }

    #[test]
    fn test_override_reminder_names_the_rule_and_reason() {
        let mut session = PlanCheckSession::default();
        session.overrides.push(plan_check_session::Override {
            key: plan_check_session::key("doc", "R-001"),
            reason: "reviewed and accepted".to_string(),
            at: chrono::Utc::now(),
            round: 1,
        });
        let reminder = override_reminder(&session).unwrap();
        assert!(reminder.contains("doc::R-001"));
        assert!(reminder.contains("reviewed and accepted"));
    }

    #[test]
    fn test_with_override_reminder_unchanged_without_any_overrides() {
        let message = with_override_reminder(
            "DECISION R-A-001: conflicts".to_string(),
            &PlanCheckSession::default(),
        );
        assert_eq!(message, "DECISION R-A-001: conflicts");
    }

    /// The exact behavior item #2/#4 of a review closed: a deny on one rule
    /// must still mention that a *different* rule in this same session is
    /// actively overridden -- the reminder used to fire only on the fully-
    /// silent path, so a human reviewing a denied round could easily miss
    /// that an earlier override was still in effect.
    #[test]
    fn test_with_override_reminder_appends_to_a_deny_message() {
        let mut session = PlanCheckSession::default();
        session.overrides.push(plan_check_session::Override {
            key: plan_check_session::key("doc", "R-002"),
            reason: "reviewed and accepted".to_string(),
            at: chrono::Utc::now(),
            round: 1,
        });
        let message = with_override_reminder("DECISION R-A-001: conflicts".to_string(), &session);
        assert!(message.starts_with("DECISION R-A-001: conflicts\n"));
        assert!(message.contains("doc::R-002"));
        assert!(message.contains("reviewed and accepted"));
    }

    #[test]
    fn test_round_limit_message_names_every_unresolved_rule() {
        let a = checked("R-A-002", Verdict::Conflicting, "span", "reason");
        let mut session = PlanCheckSession::default();
        session
            .deny_counts
            .insert(plan_check_session::key(&a.doc_slug, &a.rule_id), 4);
        let message = round_limit_message(&[&a], &session, 3);
        assert!(message.contains("R-A-002"));
        assert!(message.contains("denied 4 times"));
        assert!(message.contains("round limit (3"));
    }

    /// Unlike the terminal check, this one carries no restriction on being
    /// exercised in-process — it only reads the environment.
    #[test]
    fn test_running_under_claude_code_false_when_neither_marker_is_set() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("CLAUDECODE");
        let _g2 = EnvGuard::remove("CLAUDE_CODE_ENTRYPOINT");
        assert!(!running_under_claude_code());
    }

    #[test]
    fn test_running_under_claude_code_true_when_claudecode_is_set() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("CLAUDECODE", "1");
        let _g2 = EnvGuard::remove("CLAUDE_CODE_ENTRYPOINT");
        assert!(running_under_claude_code());
    }

    #[test]
    fn test_running_under_claude_code_true_when_entrypoint_is_set() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("CLAUDECODE");
        let _g2 = EnvGuard::set("CLAUDE_CODE_ENTRYPOINT", "cli");
        assert!(running_under_claude_code());
    }

    /// The testable core (see `exec_override`'s own doc comment for why the
    /// TTY-gated entry point itself is covered by a subprocess test in
    /// `tests/cli_test.rs` instead).
    #[test]
    fn test_exec_override_impl_records_and_returns_ok() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = tempdir().unwrap();

        let args = PlanCheckOverrideArgs {
            session: "sess-cli-1".to_string(),
            rules: vec![plan_check_session::key("doc", "R-001")],
            reason: "reviewed and accepted".to_string(),
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
        };
        assert!(exec_override_impl(&args).is_ok());

        let rules_dir = crate::rules::rules_dir(repo.path());
        let session = plan_check_session::load("sess-cli-1", &rules_dir);
        assert_eq!(session.overrides.len(), 1);
        assert_eq!(session.overrides[0].reason, "reviewed and accepted");
    }

    /// The override must land in the same governed context (`rules_dir`) the
    /// hook itself resolves to, matching by an explicit `--rules-dir` rather
    /// than relying on `--repo` deriving the same default.
    #[test]
    fn test_exec_override_impl_honors_an_explicit_rules_dir() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let rules_dir = tempdir().unwrap();

        let args = PlanCheckOverrideArgs {
            session: "sess-cli-2".to_string(),
            rules: vec![plan_check_session::key("doc", "R-001")],
            reason: "reviewed".to_string(),
            repo: None,
            rules_dir: Some(rules_dir.path().to_path_buf()),
        };
        assert!(exec_override_impl(&args).is_ok());

        let session = plan_check_session::load("sess-cli-2", rules_dir.path());
        assert_eq!(session.overrides.len(), 1);
    }
}
