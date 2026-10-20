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
//!   notice in the moment — see [`governance_session::record_partial_coverage`].
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
//! conversation, paired with `rules_dir` — see [`governance_session`]'s own
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
//!    `actual check-override` directly, from an interactive terminal —
//!    [`exec_override`]'s own doc comment explains why that check exists and
//!    what it does and does not guarantee. An overridden rule is excluded
//!    from judging from that point on, exactly like a cleared rule, but
//!    every subsequent round says so out loud in a non-blocking notice: an
//!    override is deliberately visible, not a silent bypass. See
//!    [`governance_session::record_override`].
//! 4. **A bounded number of rounds, per rule.** [`DEFAULT_MAX_ROUNDS`] real
//!    denials of the *same rule* (`--max-rounds` / `ACTUAL_PLAN_CHECK_MAX_ROUNDS`
//!    to change it) may block a session before the gate stops blocking on
//!    that rule specifically, regardless of verdict — a hard block with no
//!    exit gets the hook uninstalled, which governs nothing. A single rule
//!    exhausting its budget never exempts a different, still-fresh conflict
//!    in the same round (see [`GovernanceSession::deny_limit_exceeded`]). The
//!    round-limit pass is recorded exactly like an override (see
//!    [`governance_session::record_round_limit`]), never silent, just
//!    triggered by the cap instead of a human action.
//!
//! Direct mode never reads or writes session state (there is no
//! `session_id` outside a hook envelope), so none of this changes its
//! behavior: `requires_decision` still only sets `--json`'s status field
//! there and exits 0, matching its documented, unchanged contract.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::cli::args::PlanCheckArgs;
use crate::cli::commands::brief_ledger::{self, LedgerKey};
use crate::cli::commands::check_engine::{
    capped_read, check_json, deny_summary, emit, finish_hook, render_panel, run_pipeline,
    CheckJson, HookRun, Outcome,
};
use crate::cli::commands::governance_session::{self, GovernanceSession};
use crate::cli::commands::plan_check_hook::{self, HookEnvelope};
use crate::cli::commands::{rules_brief, rules_scope};
use crate::cli::ui::term_size;
use crate::error::ActualError;
#[cfg(feature = "telemetry")]
use crate::rules::check::Verdict;
use crate::rules::check::{ArtifactKind, CheckedRule};
use crate::rules::scope::{self, Query};

#[cfg(feature = "telemetry")]
use crate::cli::commands::check_engine::send_governance_events;

pub(super) fn repo_root(explicit: Option<&PathBuf>) -> PathBuf {
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

// ── direct mode ──────────────────────────────────────────────────────────

fn exec_direct(args: &PlanCheckArgs) -> Result<(), ActualError> {
    let plan_text = resolve_direct_plan(args)?;
    let root = repo_root(args.repo.as_ref());
    let rules_dir = args
        .rules_dir
        .clone()
        .unwrap_or_else(|| crate::rules::rules_dir(&root));

    // The pass the hook runs before its judge, reported by `--json` and noted
    // nowhere, since there is no session outside a hook envelope.
    let folders = args
        .json
        .then(|| folder_pass(&plan_text, &root, &rules_dir, FOLDER_BOUND))
        .flatten();

    #[cfg(feature = "telemetry")]
    let started_at = std::time::Instant::now();

    let outcome = run_pipeline(
        &plan_text,
        &root,
        &rules_dir,
        &args.check_knobs(),
        crate::rules::check::ArtifactKind::Plan,
        !args.no_rank,
        &GovernanceSession::default(),
    )?;

    let width = term_size::terminal_width();
    if args.json {
        println!("{}", plan_json(&outcome, folders.as_ref()));
    } else {
        println!(
            "{}",
            render_panel(
                &outcome,
                &plan_text,
                &rules_dir,
                width,
                crate::rules::check::ArtifactKind::Plan,
            )
        );
    }

    let result = if let Outcome::Verdicts { verdicts, .. } = &outcome {
        let conflicts: Vec<&CheckedRule> = verdicts.iter().filter(|v| v.verdict.blocks()).collect();
        if !conflicts.is_empty() {
            Err(ActualError::PlanNotConforming(deny_summary(
                &conflicts,
                crate::rules::check::ArtifactKind::Plan,
            )))
        } else {
            Ok(())
        }
    } else {
        Ok(())
    };

    // Only emitted when the pipeline actually reached a verdict -- the
    // other `Outcome` variants are "could not check" infra states, not
    // governance decisions, and forcing them into `allow`/`warn`/`block`
    // would misrepresent an infra failure as a verdict. Direct mode's
    // `RequiresDecision` only sets `--json`'s status field and never blocks
    // (see the module doc's "revision loop" section), so it maps to `warn`
    // here via `verdict.blocks()`, matching that documented behavior.
    #[cfg(feature = "telemetry")]
    if let Outcome::Verdicts {
        verdicts, partial, ..
    } = &outcome
    {
        let decision = if verdicts.iter().any(|v| v.verdict.blocks()) {
            crate::api::types::PlanGovernanceDecision::Block
        } else if verdicts
            .iter()
            .any(|v| v.verdict == Verdict::RequiresDecision)
            || partial.is_some()
        {
            crate::api::types::PlanGovernanceDecision::Warn
        } else {
            crate::api::types::PlanGovernanceDecision::Allow
        };
        let exit_code = result.as_ref().err().map(|e| e.exit_code()).unwrap_or(0);
        let violations: Vec<(&str, &str, crate::api::types::PlanGovernanceDecision)> = verdicts
            .iter()
            .filter(|v| v.verdict != Verdict::Conforming)
            .map(|v| {
                (
                    v.rule_id.as_str(),
                    v.doc_slug.as_str(),
                    crate::telemetry::plan_governance::rule_decision(v.verdict, v.verdict.blocks()),
                )
            })
            .collect();
        send_governance_events(
            "plan-check",
            &root,
            started_at,
            decision,
            exit_code,
            &violations,
            None,
        );
    }

    result
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
/// Resolving the plan and running the pipeline live here; everything after
/// the pipeline's outcome (session bookkeeping, deny, round limit, notices,
/// telemetry) is `check_engine::finish_hook`, shared with `impl-check`.
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
        .map(|id| governance_session::load(id, &rules_dir))
        .unwrap_or_default();
    let plan_digest = governance_session::content_digest(&plan_text);

    // Noted before the judge runs, so a judge that outlasts the hook's
    // timeout cannot lose them.
    if let Some(session_id) = session_id.filter(|id| !id.is_empty()) {
        record_plan_folders(&plan_text, session_id, &root, &rules_dir, FOLDER_BOUND);
    }

    #[cfg(feature = "telemetry")]
    let started_at = std::time::Instant::now();

    // `use_rank: false`, unconditionally, regardless of `args.no_rank`: the
    // hook's one model call stays reserved for the judge. See the module doc.
    let outcome = match run_pipeline(
        &plan_text,
        &root,
        &rules_dir,
        &args.check_knobs(),
        crate::rules::check::ArtifactKind::Plan,
        false,
        &session,
    ) {
        Ok(outcome) => outcome,
        Err(e) => {
            emit(plan_check_hook::render_notice(&format!(
                "plan-check could not read {}: {e}",
                rules_dir.display()
            )));
            return;
        }
    };

    finish_hook(
        &HookRun {
            kind: ArtifactKind::Plan,
            command: "plan-check --claude-hook",
            session_id,
            artifact_digest: &plan_digest,
            root: &root,
            rules_dir: &rules_dir,
            max_rounds: args.max_rounds,
            #[cfg(feature = "telemetry")]
            started_at,
        },
        &mut session,
        outcome,
    );
}

/// Direct mode's `--json`: the check, and beside it the folder pass when the
/// plan names a folder it could judge.
#[derive(Serialize)]
struct PlanJson<'a> {
    #[serde(flatten)]
    check: CheckJson,
    #[serde(skip_serializing_if = "Option::is_none")]
    folders: Option<&'a FolderPass>,
}

fn plan_json(outcome: &Outcome, folders: Option<&FolderPass>) -> String {
    let report = PlanJson {
        check: check_json(outcome, ArtifactKind::Plan),
        folders,
    };
    serde_json::to_string_pretty(&report)
        .expect("plan check report is serializable — this is a programmer error")
}

/// How far the folder pass may go before the judge: at most `file_cap` files
/// searched in one folder, and none once `deadline` has passed since the pass
/// began.
#[derive(Clone, Copy)]
struct FolderBound {
    file_cap: usize,
    deadline: Duration,
}

/// 500 files is more than any tracked folder holds across the eleven
/// repositories measured, so the cap stops only generated or fixture output.
/// Three seconds is about a tenth of what the hook's timeout leaves around
/// the judge's [`crate::rules::check::CHECK_BUDGET`].
const FOLDER_BOUND: FolderBound = FolderBound {
    file_cap: 500,
    deadline: Duration::from_secs(3),
};

/// What the folder pass decided about the folders a plan names, and what it
/// cost.
#[derive(Debug, Default, Serialize)]
struct FolderPass {
    elapsed_ms: f64,
    files_searched: usize,
    covered: Vec<String>,
    gaps: Vec<String>,
    undecided: Vec<UndecidedFolder>,
}

/// A folder the bound stopped on before a governed file turned up or every
/// file was searched, so it is neither covered nor a gap.
#[derive(Debug, PartialEq, Serialize)]
struct UndecidedFolder {
    folder: String,
    reason: Stop,
    /// How many files decide the folder.
    files: usize,
    /// How many of them the pass searched.
    searched: usize,
}

/// Which half of [`FolderBound`] stopped a folder.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Stop {
    FileCap,
    Deadline,
}

enum Decision {
    Covered,
    Gap,
    Undecided(Stop),
}

/// Note the folders the plan names in the session record `rules brief` keeps,
/// so a folder both planned and read is one entry. An undecided folder notes
/// nothing.
fn record_plan_folders(
    plan: &str,
    session_id: &str,
    root: &Path,
    rules_dir: &Path,
    bound: FolderBound,
) {
    let (Some(pass), Some(dir)) = (
        folder_pass(plan, root, rules_dir, bound),
        brief_ledger::ledger_dir(),
    ) else {
        return;
    };
    let folders: Vec<(&String, bool)> = pass
        .covered
        .iter()
        .map(|folder| (folder, true))
        .chain(pass.gaps.iter().map(|folder| (folder, false)))
        .collect();
    let key_dir = rules_brief::resolve(rules_dir);
    let key = LedgerKey {
        session_id,
        rules_dir: &key_dir,
    };
    brief_ledger::record_folders(&dir, &key, &folders);
}

/// Decide the folders the plan names, each by the check a read of its files
/// gets, within `bound`. No model call.
///
/// The paths are the ones selection reads from the plan, [`Query::all_paths`].
/// Each folder is decided once, by every file any of them brings to it, and
/// the smallest go first, so a deadline cuts the largest. `None` when the plan
/// names no path, or the rule set holds no documents, which would make every
/// folder a gap.
fn folder_pass(
    plan: &str,
    root: &Path,
    rules_dir: &Path,
    bound: FolderBound,
) -> Option<FolderPass> {
    let started = Instant::now();
    let paths = Query::new(plan).all_paths();
    if paths.is_empty() {
        return None;
    }
    let resolved = scope::resolve_in(rules_dir, root, false).ok()?;
    if resolved.index.is_empty() {
        return None;
    }
    let floor = rules_scope::effective_min_score(None, root).unwrap_or(0.0);
    let mut pass = FolderPass::default();
    for (folder, files) in planned_folders(root, &paths) {
        let (decision, searched) = decide(&files, bound, started, |file| {
            !rules_brief::governing(&resolved.index, file, floor).is_empty()
        });
        pass.files_searched += searched;
        match decision {
            Decision::Covered => pass.covered.push(folder),
            Decision::Gap => pass.gaps.push(folder),
            Decision::Undecided(reason) => pass.undecided.push(UndecidedFolder {
                folder,
                reason,
                files: files.len(),
                searched,
            }),
        }
    }
    pass.covered.sort();
    pass.gaps.sort();
    pass.undecided.sort_by(|a, b| a.folder.cmp(&b.folder));
    pass.elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    Some(pass)
}

/// Search `files` in order until one is governed or `bound` stops the
/// search. Returns the decision and how many files were searched.
fn decide(
    files: &[String],
    bound: FolderBound,
    started: Instant,
    governed: impl Fn(&str) -> bool,
) -> (Decision, usize) {
    for (searched, file) in files.iter().enumerate() {
        if searched == bound.file_cap {
            return (Decision::Undecided(Stop::FileCap), searched);
        }
        if started.elapsed() >= bound.deadline {
            return (Decision::Undecided(Stop::Deadline), searched);
        }
        if governed(file) {
            return (Decision::Covered, searched + 1);
        }
    }
    (Decision::Gap, files.len())
}

/// Each folder the paths count toward, once, with the files that decide it in
/// name order, smallest folder first.
fn planned_folders(root: &Path, paths: &[String]) -> Vec<(String, Vec<String>)> {
    let mut folders: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (folder, files) in paths.iter().filter_map(|path| planned_folder(root, path)) {
        folders.entry(folder).or_default().extend(files);
    }
    let mut folders: Vec<(String, Vec<String>)> = folders
        .into_iter()
        .map(|(folder, files)| (folder, files.into_iter().collect()))
        .collect();
    folders.sort_by_key(|(_, files)| files.len());
    folders
}

/// The folder a planned path counts toward and the repository-relative files
/// that decide whether a decision governs it, or `None` when it counts toward
/// none.
///
/// A path names a folder when it is a directory in the checkout or, when it
/// is not there yet, has no extension, the reading the index gives a query
/// path. That folder counts when it holds a file that would, and those files
/// decide it as reading each of them would: it is covered when a decision
/// governs any of them, and a decision for a subfolder alone leaves it a gap.
/// Any other path counts toward the folder it sits in, as a read file does,
/// once that folder exists. So prose that only looks like a path (`and/or`)
/// notes nothing, a glob counts only toward a folder it spells out, and a path
/// outside the root notes nothing. A folder the plan would create is noted by
/// the brief when a file is first written there.
fn planned_folder(root: &Path, path: &str) -> Option<(String, Vec<String>)> {
    let relative = rules_brief::relative_to_root(root, path)?;
    let on_disk = root.join(&relative);
    let names_a_folder = if on_disk.exists() {
        on_disk.is_dir()
    } else {
        Path::new(&relative).extension().is_none()
    };
    if !names_a_folder {
        let folder =
            brief_ledger::folder_of(&relative).filter(|folder| root.join(folder).is_dir())?;
        return Some((folder, vec![relative]));
    }
    let files: Vec<String> = std::fs::read_dir(&on_disk)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| format!("{relative}/{}", entry.file_name().to_string_lossy()))
        .filter(|file| brief_ledger::folder_of(file).is_some())
        .collect();
    (!files.is_empty()).then_some((relative, files))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::cli::args::DEFAULT_MAX_ROUNDS;
    use crate::cli::commands::check_engine::MAX_RULES_JUDGED;
    use std::path::Path;
    use tempfile::{tempdir, TempDir};

    use crate::testutil::{EnvGuard, ENV_MUTEX};

    #[cfg(feature = "telemetry")]
    use crate::cli::commands::check_engine::tests::with_captured_plan_governance_events;

    const OAUTH_DOC: &str = "# Sign With Asymmetric Keys: Token Signing\n\nThese rules are ALWAYS ACTIVE for OAuth token signing in `services/auth/oauth/`.\n\n### Rules\n\n- **R-A-001** MUST: sign with RS256.\n- **R-A-002** MUST NOT: log the raw signing key.\n";

    fn seed(files: &[(&str, &str)]) -> TempDir {
        let root = tempdir().unwrap();
        let dir = crate::rules::rules_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        for (name, contents) in files {
            std::fs::write(dir.join(name), contents).unwrap();
        }
        root
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

    // ── exec / exec_direct: dispatch and the conforming/conflict outcomes ───

    #[test]
    fn test_exec_direct_dispatch_with_no_applicable_rules_returns_ok() {
        // Reaches `scope::resolve_in`, which caches an index under
        // `config_dir()` regardless of outcome -- isolated so this can never
        // land in the real `$HOME` or race a concurrently-running test that
        // has its own `ACTUAL_CONFIG_DIR` set.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = tempdir().unwrap();
        let mut args = base_args();
        args.plan = vec!["a plan".to_string()];
        args.repo = Some(repo.path().to_path_buf());
        assert!(exec(&args).is_ok());
    }

    #[test]
    fn test_exec_direct_json_output_with_no_applicable_rules() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
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

        #[cfg(feature = "telemetry")]
        let events = with_captured_plan_governance_events(|| assert!(exec(&args).is_ok()));
        #[cfg(not(feature = "telemetry"))]
        assert!(exec(&args).is_ok());

        #[cfg(feature = "telemetry")]
        {
            let completed = events
                .iter()
                .find(|e| {
                    e.event
                        == crate::api::types::PlanGovernanceEventName::PlanGovernanceCheckCompleted
                })
                .expect("a completed event must be emitted");
            let props = completed.properties.as_ref().unwrap();
            assert_eq!(
                props.decision,
                Some(crate::api::types::PlanGovernanceDecision::Allow)
            );
            assert_eq!(props.exit_code, Some(0));
            assert_eq!(props.command.as_deref(), Some("plan-check"));
        }
    }

    /// The bug a review flagged: direct mode's telemetry block used to
    /// destructure `Outcome::Verdicts { verdicts, .. }`, dropping `partial`,
    /// so a run that only judged a prefix of the applicable rules reported
    /// `decision=allow` -- identical to a run that judged everything. The
    /// hook path already treats `partial.is_some()` as `warn` (see
    /// `test_exec_hook_with_round_limit_pass_emits_warn_governance_events`);
    /// direct mode must match it.
    #[cfg(all(unix, feature = "telemetry"))]
    #[test]
    fn test_exec_direct_partial_coverage_emits_warn_not_allow() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let mut body = "# Many Rules: Widget Handling\n\nThese rules are ALWAYS ACTIVE for widget handling in `services/widgets/`.\n\n### Rules\n\n".to_string();
        for i in 0..(MAX_RULES_JUDGED + 1) {
            body.push_str(&format!("- **R-X-{i:04}** MUST: rule number {i}.\n"));
        }
        let root = seed(&[("cross-cutting-many-abcd.md", &body)]);
        let bin = tempdir().unwrap();
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
        args.plan = vec![
            "Add".to_string(),
            "a".to_string(),
            "new".to_string(),
            "widget".to_string(),
            "in".to_string(),
            "services/widgets".to_string(),
        ];
        args.repo = Some(root.path().to_path_buf());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        args.no_rank = true;

        let events = with_captured_plan_governance_events(|| assert!(exec(&args).is_ok()));

        let completed = events
            .iter()
            .find(|e| {
                e.event == crate::api::types::PlanGovernanceEventName::PlanGovernanceCheckCompleted
            })
            .expect("a completed event must be emitted");
        assert_eq!(
            completed.properties.as_ref().unwrap().decision,
            Some(crate::api::types::PlanGovernanceDecision::Warn),
            "a run that only judged a prefix of the applicable rules must warn, not allow"
        );
    }

    /// AK-678 opt-out fix: same as the override-path test above, but for the
    /// direct `plan-check` path -- `ACTUAL_NO_TELEMETRY` must be checked
    /// before `send_governance_events` ever hashes repo identity or
    /// calls `distinct_id()`, so no `telemetry-id` file gets written.
    #[cfg(all(unix, feature = "telemetry"))]
    #[test]
    fn test_exec_direct_opt_out_env_var_skips_events_and_id_file() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _no_telemetry = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
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

        let events = with_captured_plan_governance_events(|| assert!(exec(&args).is_ok()));

        assert!(
            events.is_empty(),
            "opted-out user must get no governance events"
        );
        assert!(
            !home.path().join("telemetry-id").exists(),
            "opted-out user must get no persistent telemetry id file"
        );
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

        #[cfg(feature = "telemetry")]
        let events = with_captured_plan_governance_events(|| {
            let err = exec(&args).unwrap_err();
            assert!(matches!(err, ActualError::PlanNotConforming(_)));
        });
        #[cfg(not(feature = "telemetry"))]
        {
            let err = exec(&args).unwrap_err();
            assert!(matches!(err, ActualError::PlanNotConforming(_)));
        }

        #[cfg(feature = "telemetry")]
        {
            let completed = events
                .iter()
                .find(|e| {
                    e.event
                        == crate::api::types::PlanGovernanceEventName::PlanGovernanceCheckCompleted
                })
                .expect("a completed event must be emitted");
            let props = completed.properties.as_ref().unwrap();
            assert_eq!(
                props.decision,
                Some(crate::api::types::PlanGovernanceDecision::Block)
            );
            assert_eq!(props.exit_code, Some(1));

            let violations: Vec<_> = events
                .iter()
                .filter(|e| {
                    e.event
                        == crate::api::types::PlanGovernanceEventName::PlanGovernanceRuleViolation
                })
                .collect();
            assert_eq!(violations.len(), 1);
            assert_eq!(
                violations[0]
                    .properties
                    .as_ref()
                    .unwrap()
                    .rule_id
                    .as_deref(),
                Some("R-A-002")
            );
        }
    }

    /// Direct mode's counterpart to
    /// `test_exec_hook_with_reports_a_rules_directory_load_failure`: a
    /// caller at a real terminal needs this surfaced as a real `Err`, not
    /// fail-open silence — see `run_pipeline`'s own doc comment on when it
    /// returns `Err` at all.
    #[test]
    fn test_exec_direct_errors_when_the_rules_directory_cannot_be_read() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
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
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        // No panic, no emitted deny -- covered by not panicking, since stdout
        // capture is not exercised at this layer (see the subprocess tests
        // in tests/cli_test.rs for the observable-stdout contract).
        exec_hook_with(&base_args(), "not json");
    }

    #[test]
    fn test_exec_hook_with_no_plan_resolvable() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        exec_hook_with(&base_args(), "{}");
    }

    #[test]
    fn test_exec_hook_with_reports_a_rules_directory_load_failure() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
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
        // Reaches `scope::resolve_in`, which caches an index under
        // `config_dir()` regardless of outcome -- isolated for the same
        // reason as `test_exec_direct_dispatch_with_no_applicable_rules_returns_ok`.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
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

    /// AK-678: a real deny must emit a `block`-decision batch with exactly
    /// one `plan_governance_rule_violation` event for the conflicting rule
    /// (not the conforming one), and no event for `R-A-001`.
    #[cfg(all(unix, feature = "telemetry"))]
    #[test]
    fn test_exec_hook_with_deny_emits_block_governance_events() {
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

        let events = with_captured_plan_governance_events(|| exec_hook_with(&args, &raw));

        let names: Vec<_> = events.iter().map(|e| e.event).collect();
        assert!(
            names.contains(&crate::api::types::PlanGovernanceEventName::PlanGovernanceCheckStarted)
        );
        let completed = events
            .iter()
            .find(|e| {
                e.event == crate::api::types::PlanGovernanceEventName::PlanGovernanceCheckCompleted
            })
            .expect("a completed event must be emitted");
        let completed_props = completed.properties.as_ref().unwrap();
        assert_eq!(
            completed_props.decision,
            Some(crate::api::types::PlanGovernanceDecision::Block)
        );
        assert_eq!(
            completed_props.command.as_deref(),
            Some("plan-check --claude-hook")
        );

        let violations: Vec<_> = events
            .iter()
            .filter(|e| {
                e.event == crate::api::types::PlanGovernanceEventName::PlanGovernanceRuleViolation
            })
            .collect();
        assert_eq!(
            violations.len(),
            1,
            "only the conflicting rule gets a violation event"
        );
        let violation_props = violations[0].properties.as_ref().unwrap();
        assert_eq!(violation_props.rule_id.as_deref(), Some("R-A-002"));
        assert_eq!(
            violation_props.rule_source.as_deref(),
            Some("cross-cutting-token-signing-1c57")
        );
        assert_eq!(
            violation_props.decision,
            Some(crate::api::types::PlanGovernanceDecision::Block)
        );
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
        let session = governance_session::load("sess-decision-1", &rules_dir);
        let key = governance_session::key("cross-cutting-token-signing-1c57", "R-A-001");
        assert_eq!(session.plan.deny_counts.get(&key), Some(&1));
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

        let session = governance_session::load("sess-partial-1", &rules_dir);
        // The one conflicting rule in the prefix was denied...
        assert_eq!(
            session.plan.deny_counts.get(&governance_session::key(
                "cross-cutting-many-abcd",
                "R-X-0000"
            )),
            Some(&1)
        );
        // ...and every other rule in the capped prefix was cleared --
        // proving the judge actually ran on the capped batch rather than the
        // round being refused outright.
        assert_eq!(session.plan.cleared.len(), MAX_RULES_JUDGED - 1);
        // The rules past the cap (R-X-0040..R-X-0044) were never candidates
        // at all, so they can appear in neither bucket.
        assert!(!session.plan.cleared.contains_key(&governance_session::key(
            "cross-cutting-many-abcd",
            "R-X-0044"
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
        let session = governance_session::load("sess-partial-silent-1", &rules_dir);
        assert_eq!(session.plan.cleared.len(), MAX_RULES_JUDGED);

        // A silent-but-partial round is not silent in the durable log: this
        // is the one fail-open path that previously left no trace anywhere
        // but the hook response the agent itself read.
        let log = std::fs::read_to_string(governance_session::audit_log_path().unwrap()).unwrap();
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

    /// AK-678: a fully-conforming round emits `allow`, with no violation
    /// events at all -- distinct from both `warn` and `block`.
    #[cfg(all(unix, feature = "telemetry"))]
    #[test]
    fn test_exec_hook_with_clean_pass_emits_allow_governance_events() {
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

        let events = with_captured_plan_governance_events(|| exec_hook_with(&args, &raw));

        let completed = events
            .iter()
            .find(|e| {
                e.event == crate::api::types::PlanGovernanceEventName::PlanGovernanceCheckCompleted
            })
            .expect("a completed event must be emitted");
        assert_eq!(
            completed.properties.as_ref().unwrap().decision,
            Some(crate::api::types::PlanGovernanceDecision::Allow)
        );
        assert!(
            !events.iter().any(|e| e.event
                == crate::api::types::PlanGovernanceEventName::PlanGovernanceRuleViolation),
            "a fully-conforming round must not emit any violation event"
        );
    }

    // ── the revision loop (AK-677): exclusion, session persistence, ────────
    // ── round limits, and overrides ─────────────────────────────────────

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
        let after_round1 = governance_session::load("sess-rejudge-1", &rules_dir);
        assert!(after_round1
            .plan
            .cleared
            .contains_key(&governance_session::key(
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
        let after_round2 = governance_session::load("sess-rejudge-1", &rules_dir);
        let key_a001 = governance_session::key("cross-cutting-token-signing-1c57", "R-A-001");
        let key_a002 = governance_session::key("cross-cutting-token-signing-1c57", "R-A-002");
        assert!(
            !after_round2.plan.cleared.contains_key(&key_a001),
            "a rule the judge just called conflicting must not remain cleared"
        );
        assert_ne!(
            after_round2.plan.cleared.get(&key_a002),
            after_round1.plan.cleared.get(&key_a002),
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

        let session = governance_session::load("sess-persist-1", &rules_dir);
        assert_eq!(session.plan.rounds, 1);
        assert!(session.plan.cleared.contains_key(&governance_session::key(
            "cross-cutting-token-signing-1c57",
            "R-A-001"
        )));
        assert!(!session.plan.cleared.contains_key(&governance_session::key(
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
            governance_session::load("sess-limit-1", &rules_dir)
                .plan
                .rounds,
            1
        );

        // Round 2: rounds becomes 2, 2 > 1 -> the gate stops denying.
        exec_hook_with(&args, &raw);
        assert_eq!(
            governance_session::load("sess-limit-1", &rules_dir)
                .plan
                .rounds,
            2
        );

        let log = std::fs::read_to_string(governance_session::audit_log_path().unwrap()).unwrap();
        assert!(log.contains("\"kind\":\"round_limit\""));
        assert!(log.contains("sess-limit-1"));
    }

    /// AK-678: the round-limit pass is a `warn`, not a `block` -- distinct
    /// from a genuine deny, since the tool call was let through this round.
    #[cfg(all(unix, feature = "telemetry"))]
    #[test]
    fn test_exec_hook_with_round_limit_pass_emits_warn_governance_events() {
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
        args.max_rounds = 1;
        let raw = serde_json::json!({
            "session_id": "sess-limit-telemetry-1",
            "tool_input": {"plan": "Sign access tokens with RS256"},
        })
        .to_string();

        // Round 1: normal deny, not under test here.
        exec_hook_with(&args, &raw);
        // Round 2: round limit exceeded -> pass, `warn` not `block`.
        let events = with_captured_plan_governance_events(|| exec_hook_with(&args, &raw));

        let completed = events
            .iter()
            .find(|e| {
                e.event == crate::api::types::PlanGovernanceEventName::PlanGovernanceCheckCompleted
            })
            .expect("a completed event must be emitted");
        assert_eq!(
            completed.properties.as_ref().unwrap().decision,
            Some(crate::api::types::PlanGovernanceDecision::Warn)
        );
        let violation = events
            .iter()
            .find(|e| {
                e.event == crate::api::types::PlanGovernanceEventName::PlanGovernanceRuleViolation
            })
            .expect("the exhausted rule still gets a violation event");
        assert_eq!(
            violation.properties.as_ref().unwrap().decision,
            Some(crate::api::types::PlanGovernanceDecision::Warn)
        );
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
            governance_session::load("sess-clean-rounds", &rules_dir)
                .plan
                .rounds,
            3
        );
        assert!(governance_session::load("sess-clean-rounds", &rules_dir)
            .plan
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

        let session = governance_session::load("sess-clean-rounds", &rules_dir);
        let key_a001 = governance_session::key("cross-cutting-token-signing-1c57", "R-A-001");
        assert_eq!(session.plan.deny_counts.get(&key_a001), Some(&1));
        assert!(
            !session.plan.deny_limit_exceeded(&key_a001, args.max_rounds),
            "a rule's first-ever denial must never already be exhausted"
        );
        let log = std::fs::read_to_string(governance_session::audit_log_path().unwrap())
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
        let key_a001 = governance_session::key("cross-cutting-token-signing-1c57", "R-A-001");
        let key_a002 = governance_session::key("cross-cutting-token-signing-1c57", "R-A-002");

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
        let after_round2 = governance_session::load("sess-mixed-exhaustion", &rules_dir);
        assert!(after_round2
            .plan
            .deny_limit_exceeded(&key_a001, args.max_rounds));
        assert!(!after_round2
            .plan
            .deny_limit_exceeded(&key_a002, args.max_rounds));
        let log_after_round2 =
            std::fs::read_to_string(governance_session::audit_log_path().unwrap())
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
            std::fs::read_to_string(governance_session::audit_log_path().unwrap()).unwrap();
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

        governance_session::record_override(
            "sess-override-1",
            &rules_dir,
            &[
                governance_session::key("cross-cutting-token-signing-1c57", "R-A-001"),
                governance_session::key("cross-cutting-token-signing-1c57", "R-A-002"),
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
        let session = governance_session::load("sess-override-1", &rules_dir);
        assert_eq!(session.overrides.len(), 2);
    }

    // ── the plan's folders ───────────────────────────────────────────────

    /// Rules governing `services/auth/oauth/`, beside a source file there and
    /// in `infra/terraform/`, and nothing but docs in `docs/`.
    fn planned_repo() -> TempDir {
        let root = seed(&[("cross-cutting-token-signing-1c57.md", OAUTH_DOC)]);
        for file in [
            "services/auth/oauth/token.ts",
            "infra/terraform/main.tf",
            "docs/guide.md",
        ] {
            let path = root.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        root
    }

    fn plan(root: &Path, text: &str) {
        plan_within(root, text, FOLDER_BOUND);
    }

    fn plan_within(root: &Path, text: &str, bound: FolderBound) {
        record_plan_folders(text, "s1", root, &crate::rules::rules_dir(root), bound);
    }

    fn pass_within(root: &Path, text: &str, bound: FolderBound) -> FolderPass {
        folder_pass(text, root, &crate::rules::rules_dir(root), bound).unwrap()
    }

    /// Empty files at `files` under `root`, with their folders.
    fn touch(root: &Path, files: &[&str]) {
        for file in files {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
    }

    /// The covered folders and the gaps in session `s1`'s record for the rules
    /// under `root`.
    fn noted(root: &Path) -> (Vec<String>, Vec<String>) {
        let rules_dir = rules_brief::resolve(&crate::rules::rules_dir(root));
        let key = LedgerKey {
            session_id: "s1",
            rules_dir: &rules_dir,
        };
        let record = brief_ledger::load(&brief_ledger::ledger_dir().unwrap(), &key);
        let gaps = record.gaps().map(str::to_string).collect();
        (record.covered_folders.into_iter().collect(), gaps)
    }

    fn nothing_noted() -> bool {
        !brief_ledger::ledger_dir().unwrap().exists()
    }

    /// A planned file no decision governs leaves its folder a gap, and one a
    /// decision governs covers its own, both named from the repository root.
    #[test]
    fn test_a_planned_folder_no_decision_governs_is_a_gap() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = planned_repo();

        plan(
            root.path(),
            "Pin `infra/terraform/main.tf`, then sign in `services/auth/oauth/token.ts`.",
        );

        assert_eq!(
            noted(root.path()),
            (
                vec!["services/auth/oauth".to_string()],
                vec!["infra/terraform".to_string()]
            )
        );
    }

    /// Through the hook, with no runner to judge the plan, the folders are
    /// noted all the same. An envelope with no session notes nothing.
    #[test]
    fn test_exec_hook_with_notes_the_plans_folders_without_a_runner() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _no_claude = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/path/to/claude");
        let root = planned_repo();
        let mut args = base_args();
        args.repo = Some(root.path().to_path_buf());
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let plan = "Sign access tokens in `services/auth/oauth/token.ts` and pin `infra/terraform/main.tf`.";

        for session in [None, Some("")] {
            let raw = serde_json::json!({"session_id": session, "tool_input": {"plan": plan}});
            exec_hook_with(&args, &raw.to_string());
        }
        assert!(nothing_noted());

        let raw = serde_json::json!({"session_id": "s1", "tool_input": {"plan": plan}});
        exec_hook_with(&args, &raw.to_string());
        assert_eq!(
            noted(root.path()),
            (
                vec!["services/auth/oauth".to_string()],
                vec!["infra/terraform".to_string()]
            )
        );
    }

    /// A folder a read already noted, then named twice by the plan, is still
    /// one gap.
    #[test]
    fn test_a_folder_both_read_and_planned_counts_once() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = planned_repo();
        let rules_dir = rules_brief::resolve(&crate::rules::rules_dir(root.path()));
        let key = LedgerKey {
            session_id: "s1",
            rules_dir: &rules_dir,
        };
        let dir = brief_ledger::ledger_dir().unwrap();
        brief_ledger::record_folder(&dir, &key, "infra/terraform", false);

        plan(
            root.path(),
            "Pin `infra/terraform/main.tf` and add `infra/terraform/versions.tf`.",
        );

        assert_eq!(
            noted(root.path()),
            (vec![], vec!["infra/terraform".to_string()])
        );
    }

    #[test]
    fn test_a_plan_naming_no_path_notes_nothing() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = planned_repo();

        plan(
            root.path(),
            "Sign access tokens with RS256 and rotate the keys.",
        );

        assert!(nothing_noted());
    }

    /// A glob counts toward a folder it spells out, and one spanning folders
    /// notes nothing.
    #[test]
    fn test_a_glob_counts_only_toward_a_folder_it_spells_out() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = planned_repo();

        plan(
            root.path(),
            "Pin every `infra/terraform/*.tf` and lint `services/**/*.ts`.",
        );

        assert_eq!(
            noted(root.path()),
            (vec![], vec!["infra/terraform".to_string()])
        );
    }

    /// Prose that only looks like a path, a path outside the root, a folder
    /// not in the checkout yet, and docs, config or generated output note
    /// nothing, named as a file or as a folder.
    #[test]
    fn test_a_plan_notes_only_folders_that_would_count() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = planned_repo();
        for file in ["node_modules/react/index.js", ".github/workflows/ci.sh"] {
            let path = root.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        // Not a dot-named directory, which would never count anyway.
        let outside = tempfile::Builder::new()
            .prefix("outside")
            .tempdir()
            .unwrap();
        let elsewhere = outside.path().join("lib/sign.ts");
        std::fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
        std::fs::write(&elsewhere, "").unwrap();

        plan(
            root.path(),
            &format!(
                "Read and/or write through client/server, and touch `{}`. \
                 Create `src/payments/stripe.ts` under `src/payments/`. Update \
                 `docs/guide.md`, `docs/`, `config/app.yaml`, `node_modules/react/` and \
                 `.github/workflows/`.",
                elsewhere.display()
            ),
        );

        assert!(nothing_noted());
    }

    /// A folder named outright counts as itself when it holds a file that
    /// would count, and one holding only folders notes nothing.
    #[test]
    fn test_a_folder_named_outright_counts_as_itself() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = planned_repo();

        plan(
            root.path(),
            "Work in `infra/terraform/`, `services/auth/oauth/` and `services/auth/`.",
        );

        assert_eq!(
            noted(root.path()),
            (
                vec!["services/auth/oauth".to_string()],
                vec!["infra/terraform".to_string()]
            )
        );
    }

    /// A decision for `lib/oauth/` governs no file directly in `lib/`, so a
    /// plan naming `lib/` outright leaves the gap a read of `lib/format.ts`
    /// found, whichever comes first.
    #[test]
    fn test_a_read_gap_survives_a_plan_naming_the_folder_when_a_subfolder_is_governed() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        for read_first in [true, false] {
            let home = tempdir().unwrap();
            let _guards = isolated_config(&home);
            let root = seed(&[(
                "oauth-signing-a1b2.md",
                "# Adopt RS256: Token Signing\n\nThese rules are ALWAYS ACTIVE for token \
                 signing in `lib/oauth/`.\n\n### Rules\n\n- **R-A-001** MUST: sign with RS256.\n",
            )]);
            for file in ["lib/format.ts", "lib/oauth/token.ts"] {
                let path = root.path().join(file);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, "").unwrap();
            }
            let rules_dir = rules_brief::resolve(&crate::rules::rules_dir(root.path()));
            let key = LedgerKey {
                session_id: "s1",
                rules_dir: &rules_dir,
            };
            let dir = brief_ledger::ledger_dir().unwrap();
            let read = || brief_ledger::record_folder(&dir, &key, "lib", false);

            if read_first {
                read();
            }
            plan(root.path(), "Add a date formatter under `lib/`.");
            if !read_first {
                read();
            }

            assert_eq!(
                noted(root.path()),
                (vec![], vec!["lib".to_string()]),
                "read first: {read_first}"
            );
        }
    }

    /// A folder named outright is judged by the files directly in it, so a
    /// decision for every `.tf` file covers `infra/terraform/`, though no glob
    /// names the folder and its shell script is governed by nothing.
    #[test]
    fn test_a_folder_named_outright_is_judged_by_its_files() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[(
            "terraform-pins-7e2a.md",
            "# Pin Providers: Terraform Versions\n\nThese rules are ALWAYS ACTIVE for all \
             code matching: **/*.tf\n\n### Rules\n\n- **R-T-001** MUST: pin every provider \
             version.\n",
        )]);
        for file in [
            "infra/terraform/main.tf",
            "infra/terraform/plan.sh",
            "scripts/deploy.sh",
        ] {
            let path = root.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }

        plan(
            root.path(),
            "Pin the providers in `infra/terraform/` and run them from `scripts/`.",
        );

        assert_eq!(
            noted(root.path()),
            (
                vec!["infra/terraform".to_string()],
                vec!["scripts".to_string()]
            )
        );
    }

    /// A planned path spelled from the filesystem root is named from the
    /// repository root, as a read file is.
    #[test]
    fn test_an_absolute_planned_path_is_named_from_the_repository_root() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = planned_repo();
        let main_tf = root.path().join("infra/terraform/main.tf");

        plan(root.path(), &format!("Pin `{}`.", main_tf.display()));

        assert_eq!(
            noted(root.path()),
            (vec![], vec!["infra/terraform".to_string()])
        );
    }

    /// A rule set with no documents, or one that cannot be read, notes no
    /// folder: against no rules every folder would read as a gap.
    #[test]
    fn test_a_plan_without_a_rule_set_notes_nothing() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = tempdir().unwrap();
        let main_tf = root.path().join("infra/terraform/main.tf");
        std::fs::create_dir_all(main_tf.parent().unwrap()).unwrap();
        std::fs::write(&main_tf, "").unwrap();
        std::fs::create_dir_all(crate::rules::rules_dir(root.path())).unwrap();
        let not_a_dir = root.path().join("rules-dir-is-a-file");
        std::fs::write(&not_a_dir, "not a directory").unwrap();
        let text = "Pin `infra/terraform/main.tf`.";

        plan(root.path(), text);
        record_plan_folders(text, "s1", root.path(), &not_a_dir, FOLDER_BOUND);

        assert!(nothing_noted());
    }

    /// A folder named outright, again from the filesystem root, and again
    /// through a file in it is decided once, searching each of its files one
    /// time.
    #[test]
    fn test_a_folder_the_plan_names_three_ways_is_searched_once() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = planned_repo();
        touch(
            root.path(),
            &["infra/terraform/outputs.tf", "infra/terraform/vars.tf"],
        );
        let text = format!(
            "Pin `infra/terraform/`, check `{}/` and keep `infra/terraform/main.tf`.",
            root.path().join("infra/terraform").display()
        );

        let pass = pass_within(root.path(), &text, FOLDER_BOUND);

        assert_eq!(pass.files_searched, 3);
        assert_eq!(pass.gaps, vec!["infra/terraform".to_string()]);
        assert!(pass.elapsed_ms > 0.0);
        plan(root.path(), &text);
        assert_eq!(
            noted(root.path()),
            (vec![], vec!["infra/terraform".to_string()])
        );
    }

    /// A folder holding more files than the cap, none governed among those
    /// searched, is undecided, and the record notes it as neither covered nor
    /// a gap. A governed file inside the cap still covers its folder, and a
    /// folder holding exactly the cap is decided.
    #[test]
    fn test_a_folder_past_the_file_cap_is_undecided_not_a_gap() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = planned_repo();
        touch(
            root.path(),
            &[
                "infra/terraform/outputs.tf",
                "infra/terraform/vars.tf",
                "services/auth/oauth/keys.ts",
                "services/auth/oauth/rotate.ts",
                "scripts/deploy.sh",
                "scripts/seed.sh",
            ],
        );
        let text = "Pin `infra/terraform/`, sign in `services/auth/oauth/` and run `scripts/`.";
        let bound = FolderBound {
            file_cap: 2,
            ..FOLDER_BOUND
        };

        let pass = pass_within(root.path(), text, bound);

        assert_eq!(pass.covered, vec!["services/auth/oauth".to_string()]);
        assert_eq!(pass.gaps, vec!["scripts".to_string()]);
        assert_eq!(
            pass.undecided,
            vec![UndecidedFolder {
                folder: "infra/terraform".to_string(),
                reason: Stop::FileCap,
                files: 3,
                searched: 2,
            }]
        );
        assert_eq!(pass.files_searched, 5);
        plan_within(root.path(), text, bound);
        assert_eq!(
            noted(root.path()),
            (
                vec!["services/auth/oauth".to_string()],
                vec!["scripts".to_string()]
            )
        );
    }

    /// Once the deadline has passed, the pass searches nothing more: every
    /// folder it had not decided is undecided, listed by name, and nothing is
    /// noted.
    #[test]
    fn test_a_pass_out_of_time_leaves_its_folders_undecided() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = planned_repo();
        touch(
            root.path(),
            &["infra/terraform/outputs.tf", "infra/terraform/vars.tf"],
        );
        let text = "Pin `infra/terraform/`, then sign in `services/auth/oauth/token.ts`.";
        let bound = FolderBound {
            deadline: Duration::ZERO,
            ..FOLDER_BOUND
        };

        let pass = pass_within(root.path(), text, bound);

        assert_eq!(pass.files_searched, 0);
        assert!(pass.covered.is_empty() && pass.gaps.is_empty());
        assert_eq!(
            pass.undecided
                .iter()
                .map(|folder| (
                    folder.folder.as_str(),
                    folder.reason,
                    folder.files,
                    folder.searched
                ))
                .collect::<Vec<_>>(),
            vec![
                ("infra/terraform", Stop::Deadline, 3, 0),
                ("services/auth/oauth", Stop::Deadline, 1, 0)
            ]
        );
        plan_within(root.path(), text, bound);
        assert!(nothing_noted());
    }

    /// Every file a path brings to a folder decides it once, in name order,
    /// and the folders come smallest first, so a deadline cuts the largest.
    #[test]
    fn test_planned_folders_list_each_file_once_smallest_folder_first() {
        let root = planned_repo();
        touch(
            root.path(),
            &["infra/terraform/outputs.tf", "infra/terraform/vars.tf"],
        );
        let paths = [
            "infra/terraform",
            "services/auth/oauth/token.ts",
            "infra/terraform/main.tf",
        ]
        .map(String::from);

        assert_eq!(
            planned_folders(root.path(), &paths),
            vec![
                (
                    "services/auth/oauth".to_string(),
                    vec!["services/auth/oauth/token.ts".to_string()]
                ),
                (
                    "infra/terraform".to_string(),
                    vec![
                        "infra/terraform/main.tf".to_string(),
                        "infra/terraform/outputs.tf".to_string(),
                        "infra/terraform/vars.tf".to_string(),
                    ]
                ),
            ]
        );
    }

    /// `--json` reports the folder pass beside the check, and leaves it out
    /// when the plan named no folder to judge.
    #[test]
    fn test_the_json_report_carries_the_folder_pass() {
        let pass = FolderPass {
            elapsed_ms: 12.5,
            files_searched: 4,
            covered: vec!["src/billing".to_string()],
            gaps: vec!["scripts".to_string()],
            undecided: vec![
                UndecidedFolder {
                    folder: "fixtures".to_string(),
                    reason: Stop::FileCap,
                    files: 3,
                    searched: 2,
                },
                UndecidedFolder {
                    folder: "generated".to_string(),
                    reason: Stop::Deadline,
                    files: 9,
                    searched: 0,
                },
            ],
        };

        let report: serde_json::Value =
            serde_json::from_str(&plan_json(&Outcome::NothingApplies, Some(&pass))).unwrap();
        let without: serde_json::Value =
            serde_json::from_str(&plan_json(&Outcome::NothingApplies, None)).unwrap();

        assert_eq!(report["status"], "not_checked");
        assert_eq!(
            report["folders"],
            serde_json::json!({
                "elapsed_ms": 12.5,
                "files_searched": 4,
                "covered": ["src/billing"],
                "gaps": ["scripts"],
                "undecided": [
                    {"folder": "fixtures", "reason": "file_cap", "files": 3, "searched": 2},
                    {"folder": "generated", "reason": "deadline", "files": 9, "searched": 0},
                ],
            })
        );
        assert_eq!(without["status"], "not_checked");
        assert!(without.get("folders").is_none());
    }
}
