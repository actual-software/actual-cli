//! Shared engine behind `plan-check` and `impl-check`: rule discovery,
//! scoring, and judging over arbitrary text (a plan, a diff — the pipeline
//! itself does not care), plus the `check-override` command (formerly
//! `plan-check-override`, kept as a backward-compatible alias) and the
//! plan/impl-governance telemetry senders.
//!
//! Extracted out of `plan_check.rs` (AK-755 step 1): every item here was
//! already fully generic over "some text" before the move -- nothing was
//! redesigned, only relocated and, in a couple of spots, renamed where the
//! name itself said "plan" but the logic never did. `plan_check.rs` retains
//! everything that is genuinely plan-specific: resolving *where* the plan
//! text comes from (`PLAN` / `--plan-file` / stdin), the `--claude-hook`
//! envelope parsing and revision-loop orchestration, and `repo_root`.

use std::io::{IsTerminal, Read};
use std::path::Path;

use serde::Serialize;

use crate::cli::args::{PlanCheckArgs, PlanCheckOverrideArgs, RunnerChoice};
use crate::cli::commands::governance_session::{self, GovernanceSession};
use crate::cli::commands::plan_check::repo_root;
use crate::cli::commands::plan_check_hook;
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
pub(super) const MAX_RULES_JUDGED: usize = 40;

// ── the shared pipeline ──────────────────────────────────────────────────

/// The scalar knobs [`run_pipeline`] needs, factored out of `PlanCheckArgs`
/// so the pipeline itself does not depend on that concrete, plan-specific arg
/// type — `impl-check`'s `ImplCheckArgs` (AK-755 step 4) drives the same
/// pipeline through this same struct instead of a second, near-identical
/// `run_pipeline` overload. Purely a structural extraction: every field here
/// is read from the same place and means the same thing `run_pipeline` always
/// read it for, only reached through this struct now instead of directly off
/// a `&PlanCheckArgs`.
pub(super) struct CheckKnobs<'a> {
    pub(super) rebuild: bool,
    pub(super) limit: usize,
    pub(super) candidates: usize,
    pub(super) runner: Option<&'a RunnerChoice>,
    pub(super) model: Option<&'a str>,
}

impl PlanCheckArgs {
    /// Project this command's own scalar fields into the shape
    /// [`run_pipeline`] actually consumes.
    pub(super) fn check_knobs(&self) -> CheckKnobs<'_> {
        CheckKnobs {
            rebuild: self.rebuild,
            limit: self.limit,
            candidates: self.candidates,
            runner: self.runner.as_ref(),
            model: self.model.as_deref(),
        }
    }
}

/// What the pipeline produced, before either caller decides what to do with
/// it. Every variant except [`Outcome::Verdicts`] is a "could not check"
/// state rather than a finding — direct mode surfaces these as informational
/// output with a clean exit; `--claude-hook` treats every one of them as
/// fail-open. The non-`Verdicts` variants carry how many documents were under
/// consideration when the pipeline stopped, so neither caller has to report a
/// misleading zero for a failure that happened after selection ran.
pub(super) enum Outcome {
    /// No committed rule document applied to this artifact, or the rules
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
/// *this* artifact — see [`GovernanceSession::excludes`] for exactly what
/// that means (an override applies regardless of wording or kind; a
/// clearance only for the matching [`check::ArtifactKind`] while the artifact
/// digest still matches the one that earned it). Direct mode
/// always passes [`GovernanceSession::default`]: there is no session outside a
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
///
/// `kind` selects how `plan_text` is described to the judge and which
/// revision-loop memory [`GovernanceSession::excludes`] / the judged-window
/// rotation consult — see [`check::ArtifactKind`] — `plan-check` always
/// passes [`check::ArtifactKind::Plan`]; `impl-check` passes
/// [`check::ArtifactKind::Diff`]. Rule selection and the [`MAX_RULES_JUDGED`]
/// cap are otherwise identical either way.
pub(super) fn run_pipeline(
    plan_text: &str,
    root: &Path,
    rules_dir: &Path,
    knobs: &CheckKnobs,
    kind: check::ArtifactKind,
    use_rank: bool,
    session: &GovernanceSession,
) -> Result<Outcome, ActualError> {
    let plan_digest = governance_session::content_digest(plan_text);
    let resolved = scope::resolve_in(rules_dir, root, knobs.rebuild)?;
    let query = Query::new(plan_text.to_string());
    let prefiltered = select::prefilter(&resolved.index, &query, knobs.limit, knobs.candidates);

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
            knobs.runner,
            knobs.model,
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

    let gathered = gather_rules(&selection, root, session, kind, &plan_digest);
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

    // The judge always resolves its own runner, capped at
    // `CHECK_TIMEOUT_SECS`/`CHECK_BUDGET`, regardless of whether stage 2's
    // rank already resolved one above (`early_runner`, capped at the much
    // shorter `RANK_TIMEOUT_SECS`). Reusing that one for the judge too used
    // to be the bug here: an inactivity timeout sized for a different,
    // shorter call silently wins over the judge's own, more generous
    // budget, no matter how generous that budget is (see
    // `rules_rank::resolve`'s own doc). Resolving twice costs nothing real
    // — `resolve` only probes local binary/API-key availability, no
    // network call — so there is no reason to thread two different caps
    // through one cached runner instead.
    let resolved_runner = match rules_rank::resolve(
        knobs.runner,
        knobs.model,
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
    };
    let label = resolved_runner.label();

    match runtime.block_on(check::check(
        &resolved_runner.runner,
        kind,
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
/// document, dropping any rule [`GovernanceSession::excludes`] for
/// `kind` and `plan_digest` before the [`MAX_RULES_JUDGED`] cap is applied — an excluded
/// rule must never consume cap budget that a rule still worth judging needs.
///
/// When the non-excluded candidate count exceeds the cap, a rule the
/// session has already denied at least once *in this kind's loop* is pinned
/// into the judged window every round (see [`select_judged_window`]) — a proven conflict is
/// never left to chance. The rest of the window is not pinned to index zero
/// every round either: it starts at [`rotation_offset`]`(loop.rounds,
/// _, _)`, a window-sized chunk per completed round of *this* loop, wrapping. A rule left in the
/// truncated tail on round 1 therefore has a real chance of landing inside
/// the judged window on a later round instead of being silently skipped for
/// the entire life of the session — loop `rounds` only advances when a
/// real judge call completes for that kind (see [`governance_session::LoopState::rounds`]), so this
/// only changes anything once a session has actually run more than one
/// round against a selection this large. Still fully deterministic — the
/// same session, at the same round, against the same candidates, always
/// produces the same window — and still only a mitigation, not a guarantee
/// for the *unproven* remainder: a plan approved on its first round always
/// sees round 0's window (offset zero, identical to the old fixed-prefix
/// behavior), and a session that revises only a few times before approval
/// may never rotate far enough to reach a very large tail of rules it has
/// not yet judged even once. Judging the entire candidate set in a single
/// round regardless of size needs concurrent, sharded judge calls instead
/// of a bigger one, which is real implementation work tracked separately as
/// AK-743 — this only ensures the gap moves round over round instead of
/// calcifying on the same never-judged rules forever, and never lets a rule
/// already known to conflict drop out of view.
///
/// A document that no longer parses (removed, edited to something invalid,
/// between selection and this read) is skipped rather than failing the whole
/// batch — one bad file never costs the rest, the same invariant
/// `crate::rules::discover` enforces on the original scan.
fn gather_rules(
    selection: &Selection,
    root: &Path,
    session: &GovernanceSession,
    kind: check::ArtifactKind,
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
                kind,
                &governance_session::key(&selected.slug, &rule.id),
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
        candidates = select_judged_window(candidates, session, kind);
    }
    GatheredRules {
        rules: candidates,
        considered,
        truncated,
        excluded,
    }
}

/// Chooses which [`MAX_RULES_JUDGED`] candidates get judged this round when
/// there are more than that many. A rule this kind's loop has already denied
/// at least once (present in that loop's `deny_counts`) is pinned into every
/// round's window instead of being left to rotation: [`rotation_offset`]
/// moves the judged window by whole cap-sized chunks, so a proven conflict
/// can rotate clean out of the window on the very next round, and an artifact
/// revision resets `cleared` (keyed to the digest that earned it)
/// before rotation ever applies — nothing else keeps re-checking a rule
/// already known to conflict. Only the non-pinned remainder rotates; pinned
/// rules keep their original selection-then-declaration order ahead of it.
/// When pinned rules alone meet or exceed the cap, they fill the whole
/// window on their own (still in that same order) and nothing else rotates
/// in this round. Denials recorded against the other artifact kind do not
/// pin or rotate this window.
fn select_judged_window(
    candidates: Vec<RuleForJudging>,
    session: &GovernanceSession,
    kind: check::ArtifactKind,
) -> Vec<RuleForJudging> {
    let active = session.loop_state(kind);
    let (mut pinned, mut rest): (Vec<_>, Vec<_>) = candidates.into_iter().partition(|rule| {
        active
            .deny_counts
            .contains_key(&governance_session::key(&rule.doc_slug, &rule.rule_id))
    });
    if pinned.len() >= MAX_RULES_JUDGED {
        pinned.truncate(MAX_RULES_JUDGED);
        return pinned;
    }
    let remaining = MAX_RULES_JUDGED - pinned.len();
    if !rest.is_empty() {
        let offset = rotation_offset(active.rounds, remaining, rest.len());
        rest.rotate_left(offset);
    }
    rest.truncate(remaining);
    pinned.extend(rest);
    pinned
}

/// The judged window's starting offset for `round`, into `considered`
/// non-excluded candidates: `round` `window`-sized chunks in, wrapping.
/// `window` is the actual number of slots [`select_judged_window`] will keep
/// after truncating — `MAX_RULES_JUDGED` minus however many rules are
/// pinned this round — not the raw cap: striding by the cap while truncating
/// to a narrower window left a `gcd(MAX_RULES_JUDGED, considered)`-sized band
/// permanently unreached whenever `gcd(MAX_RULES_JUDGED, considered) >
/// window`, which a pinned rule makes almost certain once a session has
/// denied anything. Round 0 — a loop's first call, and every direct-mode
/// call, which never tracks rounds at all (`LoopState::default()`
/// always has `rounds: 0`) — always resolves to offset zero, the same window
/// a plain unrotated cap would have judged, so this changes nothing until a
/// loop completes at least one round against a selection larger than the
/// cap. `considered == 0` cannot occur at the only call site (guarded by
/// `!rest.is_empty()`), but returns 0 rather than divide by zero if ever
/// called otherwise.
fn rotation_offset(round: u32, window: usize, considered: usize) -> usize {
    if considered == 0 {
        return 0;
    }
    ((round as u64).saturating_mul(window as u64) % considered as u64) as usize
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
pub(super) fn capped_read<R: Read>(reader: R, source: &str) -> Result<String, ActualError> {
    let mut bytes = Vec::new();
    reader
        .take(plan_check_hook::MAX_READ_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(ActualError::IoError)?;
    if bytes.len() as u64 > plan_check_hook::MAX_READ_BYTES {
        return Err(ActualError::ConfigError(format!(
            "{source} exceeds the {}-byte check limit",
            plan_check_hook::MAX_READ_BYTES
        )));
    }
    String::from_utf8(bytes)
        .map_err(|_| ActualError::ConfigError(format!("{source} is not valid UTF-8")))
}

/// User-facing nouns for one artifact kind. Shared renderers take `kind`
/// rather than a pile of string parameters so plan-check and impl-check
/// cannot drift on a single label.
struct ArtifactCopy {
    panel_title: &'static str,
    artifact_label: &'static str,
    noun: &'static str,
    revised: &'static str,
    governance: &'static str,
}

fn artifact_copy(kind: check::ArtifactKind) -> ArtifactCopy {
    match kind {
        check::ArtifactKind::Plan => ArtifactCopy {
            panel_title: "Plan check",
            artifact_label: "Plan",
            noun: "plan",
            revised: "A revised plan is re-checked automatically.",
            governance: "Actual plan governance",
        },
        check::ArtifactKind::Diff => ArtifactCopy {
            panel_title: "Implementation check",
            artifact_label: "Diff",
            noun: "diff",
            revised: "A revised working tree is re-checked automatically.",
            governance: "Actual implementation governance",
        },
    }
}

/// How a user-facing notice names the override/round-limit audit log. The
/// file keeps its `plan-check-` name for compatibility with existing config
/// directories, so the impl-check wording says it is the shared log rather
/// than leaving a reader to wonder why a diff check writes to a plan file.
pub(super) fn audit_log_note(kind: check::ArtifactKind) -> String {
    match kind {
        check::ArtifactKind::Plan => governance_session::AUDIT_LOG_NAME.to_string(),
        check::ArtifactKind::Diff => format!(
            "{} (the audit log impl-check shares with plan-check)",
            governance_session::AUDIT_LOG_NAME
        ),
    }
}

pub(super) fn render_panel(
    outcome: &Outcome,
    text: &str,
    rules_dir: &Path,
    width: usize,
    kind: check::ArtifactKind,
) -> String {
    let copy = artifact_copy(kind);
    let mut panel = Panel::titled(copy.panel_title);
    panel = panel.kv(copy.artifact_label, &truncate(text, 72));
    panel = panel.kv("Rules dir", &rules_dir.display().to_string());

    match outcome {
        Outcome::NothingApplies => panel
            .separator()
            .line(&format!(
                "No committed rule document applies to this {}.",
                copy.noun
            ))
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
struct CheckJson {
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

pub(super) fn render_json(outcome: &Outcome, kind: check::ArtifactKind) -> String {
    let copy = artifact_copy(kind);
    let payload = match outcome {
        Outcome::NothingApplies => CheckJson {
            status: "not_checked",
            detail: Some(format!(
                "no committed rule document applies to this {}",
                copy.noun
            )),
            runner: None,
            documents_selected: 0,
            verdicts: Vec::new(),
            partial: None,
        },
        Outcome::NoRunner {
            documents_selected,
            reason,
        } => CheckJson {
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
        } => CheckJson {
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
            CheckJson {
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
pub(super) fn deny_summary(conflicts: &[&CheckedRule], kind: check::ArtifactKind) -> String {
    let copy = artifact_copy(kind);
    conflicts
        .iter()
        .map(|c| {
            format!(
                "{}: {}",
                c.rule_id,
                non_empty_or(&c.reason, &format!("conflicts with the {}", copy.noun))
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

// ── plan-governance telemetry (AK-678) ──────────────────────────────────

/// SHA-256 `(repo_hash, repo_url_hash)` for `root`, same construction as the
/// sync pipeline's own telemetry (`crate::telemetry::identity`). Best-effort:
/// an unreadable git repo or missing `origin` remote hashes an empty string
/// rather than failing.
#[cfg(feature = "telemetry")]
fn repo_identity_hashes(root: &Path) -> (String, String) {
    use crate::telemetry::identity::{hash_repo_identity, hash_repo_url};

    let repo_url = crate::analysis::cache::get_git_remote_origin_url(root).unwrap_or_default();
    let commit_hash = crate::analysis::cache::get_git_head(root).unwrap_or_default();
    (
        hash_repo_identity(&repo_url, &commit_hash),
        hash_repo_url(&repo_url),
    )
}

/// Send a batch of already-built plan-governance events, fire-and-forget.
///
/// Never blocks the caller past its own short internal timeout (see
/// `telemetry::plan_governance::SEND_TIMEOUT`) and never propagates an
/// error — a failed or disabled send is silently absorbed, per AK-678's
/// "telemetry failure never fails or slows a plan check" acceptance
/// criterion.
///
/// `PlanCheckArgs` has no `--api-url` flag (there was never a need for one
/// in production — see the module's other API calls, which all use the
/// configured/default URL), which means this module's own unit tests have
/// no way to redirect this call to a mock server the way e.g. `sync`'s
/// tests redirect via `SyncArgs::api_url`. The `#[cfg(test)]` twin below
/// captures events into a thread-local instead of ever touching the network,
/// so every existing (and future) `exec_hook_with`/`exec_direct`/
/// `exec_override_impl` test stays hermetic while still letting tests that
/// care assert on exactly what would have been sent — see
/// `tests::take_captured_plan_governance_events`.
#[cfg(all(feature = "telemetry", not(test)))]
fn dispatch_governance_events(events: Vec<crate::api::types::PlanGovernanceEvent>) {
    let cfg = crate::config::paths::load().unwrap_or_default();
    let api_url = cfg
        .api_url
        .clone()
        .unwrap_or_else(|| crate::api::client::DEFAULT_API_URL.to_string());

    if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        rt.block_on(crate::telemetry::plan_governance::send_events(
            events, &cfg, &api_url,
        ));
    }
}

#[cfg(all(feature = "telemetry", test))]
fn dispatch_governance_events(events: Vec<crate::api::types::PlanGovernanceEvent>) {
    tests::CAPTURED_PLAN_GOVERNANCE_EVENTS.with(|cell| cell.borrow_mut().extend(events));
}

/// Build repo-identity hashes, assemble a started+completed(+violation)
/// batch of plan-governance events for one `plan-check`/`--claude-hook` run,
/// and dispatch them. Only rule id/slug and coarse enums ever go into
/// `violations` — never a rule's span, statement, or reason text; see
/// `PRIVACY.md`.
///
/// Checks `opt_out::is_disabled` first, before any repo-identity hashing or
/// `distinct_id()` — an opted-out user must not have a persistent telemetry
/// id written to disk (or git subprocesses spawned) just because a check
/// ran; `send_events`'s own opt-out check is too late for that, since by
/// then the id file already exists. See `PRIVACY.md`'s "three independent
/// ways to disable telemetry."
#[cfg(feature = "telemetry")]
pub(super) fn send_governance_events(
    command: &str,
    root: &Path,
    started_at: std::time::Instant,
    decision: crate::api::types::PlanGovernanceDecision,
    exit_code: i32,
    violations: &[(&str, &str, crate::api::types::PlanGovernanceDecision)],
) {
    use crate::telemetry::plan_governance::EventContext;

    let cfg = crate::config::paths::load().unwrap_or_default();
    if crate::telemetry::opt_out::is_disabled(&cfg) {
        return;
    }

    let (repo_hash, repo_url_hash) = repo_identity_hashes(root);
    let ctx = EventContext::new(command).with_repo_hashes(repo_hash, repo_url_hash);
    // `duration_ms` uses the precise monotonic elapsed time; both events'
    // `timestamp` fields are stamped at send time rather than backdating the
    // started event to when the check actually began -- for this stream's
    // aggregate-counting use, ordering/dedup is what `timestamp` is for, and
    // `duration_ms` is already the authoritative latency figure.
    let duration_ms = started_at.elapsed().as_secs_f64() * 1000.0;

    let mut events = vec![
        ctx.started_event(),
        ctx.completed_event(decision, duration_ms, exit_code),
    ];
    for (rule_id, rule_source, v_decision) in violations {
        events.push(ctx.violation_event(rule_id, rule_source, *v_decision));
    }

    dispatch_governance_events(events);
}

/// Emit one `plan_governance_check_completed`-shaped event per rule an
/// `actual check-override` call clears, with `command =
/// "check-override"` distinguishing it from a real check run's
/// completion — so "overrides recorded" (one of AK-678's candidate
/// counters) is `count(command == "check-override")` in PostHog,
/// without a new event name or schema field. `keys` are already
/// `"<doc-slug>::<rule-id>"` strings (see `governance_session::key`),
/// validated against the rule corpus by [`validate_override_rules`] before
/// this is ever called.
///
/// Checks `opt_out::is_disabled` first, before any repo-identity hashing or
/// `distinct_id()` — see `send_governance_events`'s doc comment for why
/// that ordering matters.
#[cfg(feature = "telemetry")]
fn send_override_events(root: &Path, keys: &[String]) {
    use crate::api::types::{
        PlanGovernanceDecision, PlanGovernanceEvent, PlanGovernanceEventName,
        PlanGovernanceEventProperties,
    };
    use crate::telemetry::plan_governance::distinct_id;

    let cfg = crate::config::paths::load().unwrap_or_default();
    if crate::telemetry::opt_out::is_disabled(&cfg) {
        return;
    }

    let (repo_hash, repo_url_hash) = repo_identity_hashes(root);
    let id = distinct_id();
    let cli_version = env!("CARGO_PKG_VERSION").to_string();
    let timestamp = chrono::Utc::now().to_rfc3339();

    let events: Vec<PlanGovernanceEvent> = keys
        .iter()
        .map(|key| {
            let (rule_source, rule_id) = key.split_once("::").unwrap_or((key.as_str(), ""));
            PlanGovernanceEvent {
                event: PlanGovernanceEventName::PlanGovernanceCheckCompleted,
                distinct_id: id.clone(),
                properties: Some(PlanGovernanceEventProperties {
                    cli_version: Some(cli_version.clone()),
                    command: Some("check-override".to_string()),
                    rule_id: Some(rule_id.to_string()),
                    rule_source: Some(rule_source.to_string()),
                    decision: Some(PlanGovernanceDecision::Allow),
                    exit_code: Some(0),
                    repo_hash: Some(repo_hash.clone()),
                    repo_url_hash: Some(repo_url_hash.clone()),
                    ..Default::default()
                }),
                timestamp: Some(timestamp.clone()),
                insert_id: None,
            }
        })
        .collect();

    // One `--rule` flag per event, uncapped by the CLI itself, so a large
    // enough override call can exceed the proxy's 100-event batch cap (see
    // `plan_governance::MAX_EVENTS_PER_BATCH`) -- chunk rather than let one
    // oversized batch get rejected whole.
    for chunk in events.chunks(crate::telemetry::plan_governance::MAX_EVENTS_PER_BATCH) {
        dispatch_governance_events(chunk.to_vec());
    }
}

/// `--claude-hook`-mode wrapper around [`send_governance_events`]: every
/// non-conforming verdict this round gets a violation event with the same
/// `blocked` outcome, since (per the module doc's "revision loop" section) a
/// round either denies the tool call over every rule in `blocking` together
/// or lets all of them through together — there is no per-rule split within
/// one round's outcome.
///
/// `command` names the caller for the same reason [`send_governance_events`]'s
/// own `command` parameter does: `plan_check.rs` passes
/// `"plan-check --claude-hook"`, `impl_check.rs` passes
/// `"impl-check --claude-hook"` — both share this one wrapper rather than
/// each hardcoding their own copy of the violation-event assembly above.
#[cfg(feature = "telemetry")]
pub(super) fn send_hook_governance_events(
    command: &str,
    root: &Path,
    started_at: std::time::Instant,
    decision: crate::api::types::PlanGovernanceDecision,
    verdicts: &[CheckedRule],
    blocked: bool,
) {
    let violations: Vec<(&str, &str, crate::api::types::PlanGovernanceDecision)> = verdicts
        .iter()
        .filter(|v| v.verdict != Verdict::Conforming)
        .map(|v| {
            (
                v.rule_id.as_str(),
                v.doc_slug.as_str(),
                crate::telemetry::plan_governance::rule_decision(v.verdict, blocked),
            )
        })
        .collect();
    // The hook's own contract never returns a non-zero exit -- `exec()`
    // always returns `Ok(())` after `exec_hook` -- so `exit_code` is always
    // 0 here regardless of `decision`.
    send_governance_events(command, root, started_at, decision, 0, &violations);
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

/// Run `actual check-override` (also reachable via its backward-compatible
/// alias, `actual plan-check-override`).
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
        "check-override must be run interactively, from an ordinary terminal — not through a \
         script, an agent's tool call, or a terminal Claude Code itself launched. It records a \
         human decision to override plan- or implementation-stage governance, and this \
         invocation was refused because it looks like an agent's own shell rather than a \
         human's: either no terminal is attached, or this process's environment carries Claude \
         Code's own CLAUDECODE / CLAUDE_CODE_ENTRYPOINT markers. If you are a human seeing this \
         from inside Claude Code's integrated terminal (or a similar wrapper), run the command \
         from a separate, plain terminal window instead. This is a real but not airtight check, \
         not a cryptographic guarantee of human origin — see `actual check-override --help`."
            .to_string(),
    )
}

/// Reject an override naming a `<doc-slug>::<rule-id>` pair the rules corpus
/// under `rules_dir` does not define.
///
/// [`crate::cli::args`]'s `parse_rule_key` (run at CLI-parse time, before
/// `rules_dir` is even known) only checks the value's shape — that it
/// splits on `::` with both halves non-empty — so a mistyped document slug
/// or rule id is otherwise accepted, stored, and reported as "override
/// recorded," while [`GovernanceSession::excludes`] compares the exact
/// string and never matches it: the denial persists every round after,
/// silently, while the operator believes it was cleared.
///
/// Checked against every rule the corpus defines, not against the current
/// session's judged selection: an override for a rule that exists but fell
/// outside this round's cap or selection is legitimate (see the module
/// doc's "explicit, recorded override" note), so the check has to be
/// existence in the corpus, not membership in one round's prefix. A
/// `rules_dir` that does not exist at all reads as an empty corpus (see
/// [`crate::rules::read_rule_sources_in`]'s own contract), so every key is
/// rejected rather than silently accepted — the same mistyped-input
/// protection now also covers a wrong `--rules-dir`, not just a wrong
/// `--rule`.
fn validate_override_rules(rules_dir: &Path, keys: &[String]) -> Result<(), ActualError> {
    let report = crate::rules::parse_rule_sources(crate::rules::read_rule_sources_in(rules_dir)?);
    let known: std::collections::HashSet<String> = report
        .documents
        .iter()
        .flat_map(|doc| {
            let slug = doc.slug().unwrap_or("<unnamed>").to_string();
            doc.rules
                .iter()
                .map(move |rule| governance_session::key(&slug, &rule.id))
        })
        .collect();
    let unknown: Vec<&str> = keys
        .iter()
        .map(String::as_str)
        .filter(|key| !known.contains(*key))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    let (noun, verb) = if unknown.len() == 1 {
        ("rule", "was")
    } else {
        ("rules", "were")
    };
    let named: Vec<String> = unknown.iter().map(|k| format!("'{k}'")).collect();
    Err(ActualError::ConfigError(format!(
        "{noun} {} {verb} not found under {} — check the exact <doc-slug>::<rule-id> named in \
         the deny message. No override was recorded.",
        named.join(", "),
        rules_dir.display()
    )))
}

/// The testable core of `actual check-override`: a human explicitly
/// clearing one or more rules for a specific session. Validates every named
/// rule against the corpus first (see [`validate_override_rules`]) and
/// records nothing at all if any is unknown — a multi-rule call is
/// all-or-nothing, since a human asking to clear several rules together
/// would not expect one typo to silently drop only that one while the rest
/// went through. Otherwise always succeeds — there is no other invalid
/// state this can observe (an unknown `session_id` just starts a fresh
/// session). [`exec_override`] is the real entry point; this exists
/// separately so the recording logic is testable without a real terminal.
fn exec_override_impl(args: &PlanCheckOverrideArgs) -> Result<(), ActualError> {
    let root = repo_root(args.repo.as_ref());
    let rules_dir = args
        .rules_dir
        .clone()
        .unwrap_or_else(|| crate::rules::rules_dir(&root));
    validate_override_rules(&rules_dir, &args.rules)?;
    governance_session::record_override(&args.session, &rules_dir, &args.rules, &args.reason);
    #[cfg(feature = "telemetry")]
    send_override_events(&root, &args.rules);
    let width = term_size::terminal_width();
    let mut panel = Panel::titled("Check override recorded");
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

/// The deny reason: every conflicting rule id, the rule's own statement
/// verbatim, the judge's reason, and the quoted artifact span, one per line —
/// so a reader (or the agent revising the plan or working tree) sees every
/// violation at once rather than only the first, and can revise against the
/// rule's actual text rather than the judge's paraphrase of it. `blocking`
/// names both real conflicts and `requires_decision` verdicts — a claim it
/// deliberately supersedes a rule is model output, not a recorded human
/// decision, so it is denied exactly like an outright conflict (see the
/// module doc's "advisory gate" section) — labeled `CONFLICT` or `DECISION`
/// per rule, the same labels direct mode's panel already uses, so a reader
/// switching between the two callers sees consistent vocabulary.
///
/// When `session_id` is present (a `--claude-hook` call whose envelope named
/// one), a final line names the session and points a human at
/// `check-override --help` rather than handing back a ready-to-paste
/// invocation.
///
/// This is deliberate, not an oversight: this text reaches the agent's own
/// tool result, and the agent already runs a shell. Earlier versions
/// pre-filled `--session <id> --rule <doc-slug>::<rule-id>` here, which meant
/// the *only* control standing between "the agent read its own denial" and
/// "the agent cleared its own denial" was `exec_override`'s TTY check (see
/// its own doc comment) — a second, independent layer, not a substitute for
/// this one. `<doc-slug>::<rule-id>` (see [`governance_session::key`]) is
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
pub(super) fn hook_deny_reason(
    blocking: &[&CheckedRule],
    session_id: Option<&str>,
    partial: Option<(usize, usize)>,
    kind: check::ArtifactKind,
) -> String {
    let copy = artifact_copy(kind);
    let fallback = format!("conflicts with the {}", copy.noun);
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
                "{label} {} ({}): {} — rule: \"{}\" — {}: \"{}\"",
                c.rule_id,
                c.level.as_str(),
                non_empty_or(&c.reason, &fallback),
                truncate(&c.statement, 240),
                copy.noun,
                truncate(&c.span, 240)
            )
        })
        .collect();
    if let Some((judged, total)) = partial {
        lines.push(partial_coverage_note(judged, total));
    }
    if let Some(session_id) = session_id {
        lines.push(format!(
            "{} Session: {session_id}. A human \
             reviewing this — not the agent — can override a specific rule explicitly by \
             running `actual check-override` from an interactive terminal (see `actual \
             check-override --help` for the exact flags); that command refuses to run \
             non-interactively.",
            copy.revised
        ));
    }
    lines.join("\n")
}

/// The disclosure line for a partially-judged round: plain enough that
/// "conforming" or "no conflicts among these" is never mistaken for "the
/// whole plan was checked." Shared between the deny path (appended to
/// [`hook_deny_reason`]) and the silent-otherwise path (emitted as its own
/// notice in `exec_hook_with`), so the wording is identical either way.
pub(super) fn partial_coverage_note(judged: usize, total: usize) -> String {
    format!(
        "Only {judged} of {total} rules in scope were checked this round — the rest exceeded \
         the {MAX_RULES_JUDGED}-rule judging cap and were not evaluated at all."
    )
}

/// The non-blocking notice emitted when every rule still blocking this
/// round (a conflict or an unconfirmed `requires_decision` claim) has
/// *individually* exhausted its own denial budget (see
/// [`GovernanceSession::deny_limit_exceeded`]): the gate stops denying, but
/// says exactly why, names every exhausted rule and how many times each was
/// actually denied, so this is a loud pass, not a silent one. Paired with
/// [`governance_session::record_round_limit`], which writes the durable side
/// of the same event.
pub(super) fn round_limit_message(
    exhausted: &[&CheckedRule],
    session: &GovernanceSession,
    kind: check::ArtifactKind,
    max_rounds: u32,
) -> String {
    let counts = &session.loop_state(kind).deny_counts;
    let parts: Vec<String> = exhausted
        .iter()
        .map(|c| {
            let key = governance_session::key(&c.doc_slug, &c.rule_id);
            let count = counts.get(&key).copied().unwrap_or(0);
            format!("{} (denied {count} times)", c.rule_id)
        })
        .collect();
    let copy = artifact_copy(kind);
    format!(
        "{} hit its round limit ({max_rounds} denials) for {}: proceeding \
         without blocking further on {} specifically. This is not a silent pass — recorded in \
         {}.",
        copy.governance,
        parts.join(", "),
        if exhausted.len() == 1 { "it" } else { "them" },
        audit_log_note(kind)
    )
}

/// A non-blocking reminder naming every active override on `session`. An
/// override must stay visible on every round it applies to — never silently
/// absorbed once granted, whether this round is otherwise fully silent, a
/// deny on some *other* rule, or a round-limit notice. `None` when the
/// session has no overrides at all.
pub(super) fn override_reminder(session: &GovernanceSession) -> Option<String> {
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
pub(super) fn with_override_reminder(mut message: String, session: &GovernanceSession) -> String {
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
pub(crate) mod tests {
    use super::*;

    use crate::cli::args::DEFAULT_MAX_ROUNDS;
    use std::path::PathBuf;
    use tempfile::{tempdir, TempDir};

    use crate::rules::types::RuleLevel;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

    // ── plan-governance telemetry capture (AK-678) ──
    //
    // `cargo test`'s default runner reuses OS threads across many `#[test]`
    // functions, so a plain thread-local would leak an earlier test's events
    // into a later one on the same thread. `take_captured_plan_governance_events`
    // is a drain (`mem::take`), so a test that wants to assert on exactly its
    // own call must drain once *before* exercising the code under test (to
    // discard anything a prior test on this thread left behind) and again
    // *after* (to collect only what it just produced) — see
    // `with_captured_plan_governance_events`, which does both around a closure.
    //
    // `pub(crate)`, not merely private: `plan_check.rs`'s own test module
    // exercises this code through `exec_hook_with`/`exec_direct` and needs to
    // assert on exactly what would have been sent, so it imports these from
    // here rather than keeping a second, disconnected copy.
    #[cfg(feature = "telemetry")]
    thread_local! {
        pub(crate) static CAPTURED_PLAN_GOVERNANCE_EVENTS: std::cell::RefCell<Vec<crate::api::types::PlanGovernanceEvent>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    #[cfg(feature = "telemetry")]
    pub(crate) fn take_captured_plan_governance_events(
    ) -> Vec<crate::api::types::PlanGovernanceEvent> {
        CAPTURED_PLAN_GOVERNANCE_EVENTS.with(|cell| std::mem::take(&mut *cell.borrow_mut()))
    }

    #[cfg(feature = "telemetry")]
    pub(crate) fn with_captured_plan_governance_events(
        f: impl FnOnce(),
    ) -> Vec<crate::api::types::PlanGovernanceEvent> {
        take_captured_plan_governance_events();
        f();
        take_captured_plan_governance_events()
    }

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
            &GovernanceSession::default(),
            check::ArtifactKind::Plan,
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
            &GovernanceSession::default(),
            check::ArtifactKind::Plan,
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
            &GovernanceSession::default(),
            check::ArtifactKind::Plan,
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
            &GovernanceSession::default(),
            check::ArtifactKind::Plan,
            "test-digest",
        );
        assert_eq!(gathered.rules.len(), MAX_RULES_JUDGED);
        assert_eq!(gathered.considered, MAX_RULES_JUDGED);
        assert!(!gathered.truncated);
    }

    /// A session's first round (round 0, the same value direct mode's
    /// `GovernanceSession::default()` always has) must judge exactly the old
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
            &GovernanceSession::default(),
            check::ArtifactKind::Plan,
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

        let mut session = GovernanceSession::default();
        session.plan.rounds = 1;
        let gathered = gather_rules(
            &selection,
            root.path(),
            &session,
            check::ArtifactKind::Plan,
            "test-digest",
        );
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

    /// A rule already denied at least once must stay in the judged window on
    /// a later round even though rotation alone would carry it clean out of
    /// scope — the gap the reviewer flagged in APR-001's rotation mitigation:
    /// without pinning, a rule proven to conflict on round 0 could go
    /// unjudged on round 1 while a plan revision resets `cleared` and
    /// restores the full candidate list.
    #[test]
    fn test_gather_rules_pins_a_previously_denied_rule_across_rotation() {
        let mut body = "# Many Rules: Widget Handling\n\nThese rules are ALWAYS ACTIVE for widget handling in `services/widgets/`.\n\n### Rules\n\n".to_string();
        for i in 0..(MAX_RULES_JUDGED + 5) {
            body.push_str(&format!("- **R-X-{i:04}** MUST: rule number {i}.\n"));
        }
        let root = seed(&[("cross-cutting-many-abcd.md", &body)]);
        let report = crate::rules::load_rule_set(root.path()).unwrap();
        let index = crate::rules::scope::ScopeIndex::build(&report, root.path(), "fp".to_string());
        let query = Query::new("Add a new widget in services/widgets".to_string());
        let selection = select::prefilter(&index, &query, 10, 30).finish(Stage2::NotRequested);
        let doc_slug = selection.selected[0].slug.clone();

        // R-X-0000 was denied on round 0 (inside that round's unrotated
        // window) and would rotate out of round 1's window on its own —
        // round 1's plain rotated window is R-X-0040..R-X-0044 (see the
        // rotation test above), which does not include it.
        let mut session = GovernanceSession::default();
        session.plan.rounds = 1;
        session
            .plan
            .deny_counts
            .insert(governance_session::key(&doc_slug, "R-X-0000"), 1);

        let gathered = gather_rules(
            &selection,
            root.path(),
            &session,
            check::ArtifactKind::Plan,
            "test-digest",
        );
        assert_eq!(gathered.rules.len(), MAX_RULES_JUDGED);
        assert!(gathered.truncated);

        let ids: Vec<&str> = gathered.rules.iter().map(|r| r.rule_id.as_str()).collect();
        assert!(
            ids.contains(&"R-X-0000"),
            "previously-denied rule must stay pinned in the window: {ids:?}"
        );
        // The rotated remainder still reaches into the tail round 0 never
        // saw (R-X-0044, the very last rule), proving rotation still runs
        // over the non-pinned candidates alongside the pin.
        assert!(ids.contains(&"R-X-0044"));
    }

    /// Plan-check rounds must not rotate impl-check's judged window. A
    /// session that has completed one plan-check round still judges the
    /// unrotated prefix on the first impl-check round.
    #[test]
    fn test_gather_rules_does_not_rotate_diff_from_plan_rounds() {
        let mut body = "# Many Rules: Widget Handling\n\nThese rules are ALWAYS ACTIVE for widget handling in `services/widgets/`.\n\n### Rules\n\n".to_string();
        for i in 0..(MAX_RULES_JUDGED + 5) {
            body.push_str(&format!("- **R-X-{i:04}** MUST: rule number {i}.\n"));
        }
        let root = seed(&[("cross-cutting-many-abcd.md", &body)]);
        let report = crate::rules::load_rule_set(root.path()).unwrap();
        let index = crate::rules::scope::ScopeIndex::build(&report, root.path(), "fp".to_string());
        let query = Query::new("Add a new widget in services/widgets".to_string());
        let selection = select::prefilter(&index, &query, 10, 30).finish(Stage2::NotRequested);

        let mut session = GovernanceSession::default();
        session.plan.rounds = 1;
        let gathered = gather_rules(
            &selection,
            root.path(),
            &session,
            check::ArtifactKind::Diff,
            "test-digest",
        );
        let ids: Vec<&str> = gathered.rules.iter().map(|r| r.rule_id.as_str()).collect();
        assert!(ids.contains(&"R-X-0000"));
        assert!(ids.contains(&"R-X-0039"));
        assert!(!ids.contains(&"R-X-0044"));
    }

    /// A rule denied by plan-check must not pin into impl-check's window.
    /// Round 1's unpinned rotation drops `R-X-0039` (the last of round 0's
    /// prefix); if this incorrectly read the plan loop's `deny_counts`, that
    /// rule would be pinned back in.
    #[test]
    fn test_gather_rules_does_not_pin_a_plan_denied_rule_on_a_diff_loop() {
        let mut body = "# Many Rules: Widget Handling\n\nThese rules are ALWAYS ACTIVE for widget handling in `services/widgets/`.\n\n### Rules\n\n".to_string();
        for i in 0..(MAX_RULES_JUDGED + 5) {
            body.push_str(&format!("- **R-X-{i:04}** MUST: rule number {i}.\n"));
        }
        let root = seed(&[("cross-cutting-many-abcd.md", &body)]);
        let report = crate::rules::load_rule_set(root.path()).unwrap();
        let index = crate::rules::scope::ScopeIndex::build(&report, root.path(), "fp".to_string());
        let query = Query::new("Add a new widget in services/widgets".to_string());
        let selection = select::prefilter(&index, &query, 10, 30).finish(Stage2::NotRequested);
        let doc_slug = selection.selected[0].slug.clone();

        let mut session = GovernanceSession::default();
        session.diff.rounds = 1;
        session
            .plan
            .deny_counts
            .insert(governance_session::key(&doc_slug, "R-X-0039"), 1);

        let gathered = gather_rules(
            &selection,
            root.path(),
            &session,
            check::ArtifactKind::Diff,
            "test-digest",
        );
        let ids: Vec<&str> = gathered.rules.iter().map(|r| r.rule_id.as_str()).collect();
        assert!(
            !ids.contains(&"R-X-0039"),
            "a plan-stage denial must not pin the impl-check window: {ids:?}"
        );
        assert!(ids.contains(&"R-X-0044"));
    }

    /// When previously-denied rules alone meet or exceed the cap, they fill
    /// the whole judged window on their own — nothing else rotates in, and
    /// nothing is dropped from the pinned set beyond the cap.
    #[test]
    fn test_gather_rules_pinned_rules_alone_fill_the_window_when_they_meet_the_cap() {
        let mut body = "# Many Rules: Widget Handling\n\nThese rules are ALWAYS ACTIVE for widget handling in `services/widgets/`.\n\n### Rules\n\n".to_string();
        for i in 0..(MAX_RULES_JUDGED + 10) {
            body.push_str(&format!("- **R-X-{i:04}** MUST: rule number {i}.\n"));
        }
        let root = seed(&[("cross-cutting-many-abcd.md", &body)]);
        let report = crate::rules::load_rule_set(root.path()).unwrap();
        let index = crate::rules::scope::ScopeIndex::build(&report, root.path(), "fp".to_string());
        let query = Query::new("Add a new widget in services/widgets".to_string());
        let selection = select::prefilter(&index, &query, 10, 30).finish(Stage2::NotRequested);
        let doc_slug = selection.selected[0].slug.clone();

        // Deny every rule up through the cap plus a couple more, so pinned
        // alone exceeds MAX_RULES_JUDGED.
        let mut session = GovernanceSession::default();
        for i in 0..(MAX_RULES_JUDGED + 2) {
            session.plan.deny_counts.insert(
                governance_session::key(&doc_slug, &format!("R-X-{i:04}")),
                1,
            );
        }

        let gathered = gather_rules(
            &selection,
            root.path(),
            &session,
            check::ArtifactKind::Plan,
            "test-digest",
        );
        assert_eq!(gathered.rules.len(), MAX_RULES_JUDGED);
        assert!(gathered.truncated);

        let ids: Vec<&str> = gathered.rules.iter().map(|r| r.rule_id.as_str()).collect();
        // First MAX_RULES_JUDGED denied rules in original order, truncated —
        // never-denied candidates (R-X-0042 onward) do not crowd them out.
        assert!(ids.contains(&"R-X-0000"));
        assert!(ids.contains(&format!("R-X-{:04}", MAX_RULES_JUDGED - 1).as_str()));
        assert!(!ids.contains(&format!("R-X-{MAX_RULES_JUDGED:04}").as_str()));
    }

    #[test]
    fn test_rotation_offset_is_zero_at_round_zero_and_wraps_thereafter() {
        assert_eq!(rotation_offset(0, MAX_RULES_JUDGED, 45), 0);
        assert_eq!(rotation_offset(1, MAX_RULES_JUDGED, 45), MAX_RULES_JUDGED);
        // Wraps back toward the start once enough rounds have passed to
        // cycle through every candidate at least once.
        assert_eq!(
            rotation_offset(2, MAX_RULES_JUDGED, 45),
            (2 * MAX_RULES_JUDGED) % 45
        );
        assert_eq!(rotation_offset(0, MAX_RULES_JUDGED, 0), 0);
    }

    /// The gap this guards: striding by the raw cap while the window is
    /// narrower (because a rule is pinned) used to leave a permanent gap —
    /// `rotation_offset` must stride by the actual window width instead. 41
    /// candidates with one denied rule pinned means `rest.len() == 40`; the
    /// old `rotation_offset(round, 40)` landed on offset 0 every round
    /// (`round * 40 % 40 == 0`), so the last of the 40 non-pinned candidates
    /// was never selected into the 39-wide remaining window on any round.
    #[test]
    fn test_select_judged_window_rotates_a_pinned_sessions_full_tail_into_view() {
        let candidates: Vec<RuleForJudging> = (0..(MAX_RULES_JUDGED + 1))
            .map(|i| {
                RuleForJudging::new(
                    "docs".to_string(),
                    format!("R-X-{i:04}"),
                    RuleLevel::Must,
                    format!("rule number {i}"),
                )
            })
            .collect();

        let mut session = GovernanceSession::default();
        session
            .plan
            .deny_counts
            .insert(governance_session::key("docs", "R-X-0000"), 1);

        let mut seen = std::collections::BTreeSet::new();
        for round in 0..MAX_RULES_JUDGED as u32 {
            session.plan.rounds = round;
            let window =
                select_judged_window(candidates.clone(), &session, check::ArtifactKind::Plan);
            assert_eq!(window.len(), MAX_RULES_JUDGED);
            seen.extend(window.into_iter().map(|r| r.rule_id));
        }

        assert_eq!(
            seen.len(),
            MAX_RULES_JUDGED + 1,
            "every candidate, including the pinned one, must surface within {MAX_RULES_JUDGED} rounds"
        );
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
        let reason = hook_deny_reason(&[&a], None, None, check::ArtifactKind::Plan);
        assert!(reason.contains("R-A-002"));
        assert!(reason.contains("log the signing key for debugging"));
    }

    #[test]
    fn test_hook_deny_reason_falls_back_when_the_model_reason_is_blank() {
        let a = checked("R-A-002", Verdict::Conflicting, "some span", "");
        let reason = hook_deny_reason(&[&a], None, None, check::ArtifactKind::Plan);
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
        let reason = hook_deny_reason(&[&a], None, None, check::ArtifactKind::Plan);
        assert!(reason.contains(&a.statement));
    }

    #[test]
    fn test_hook_deny_reason_with_no_session_omits_override_instructions() {
        let a = checked("R-A-002", Verdict::Conflicting, "span", "reason");
        let reason = hook_deny_reason(&[&a], None, None, check::ArtifactKind::Plan);
        assert!(!reason.contains("plan-check-override"));
    }

    #[test]
    fn test_hook_deny_reason_with_a_session_points_at_override_help() {
        let a = checked("R-A-002", Verdict::Conflicting, "span", "reason");
        let reason = hook_deny_reason(&[&a], Some("sess-123"), None, check::ArtifactKind::Plan);
        assert!(reason.contains("Session: sess-123"));
        assert!(reason.contains("actual check-override"));
        assert!(reason.contains("--help"));
        assert!(reason.contains("A revised plan is re-checked automatically"));
        assert!(reason.contains("plan:"));
    }

    /// On an implementation gate the deny text is the agent's tool result.
    /// Telling it to revise a plan would send it back to ExitPlanMode
    /// instead of the working tree.
    #[test]
    fn test_hook_deny_reason_impl_check_tells_the_agent_to_revise_the_working_tree() {
        let a = checked("R-A-002", Verdict::Conflicting, "span", "");
        let reason = hook_deny_reason(&[&a], Some("sess-123"), None, check::ArtifactKind::Diff);
        assert!(reason.contains("A revised working tree is re-checked automatically"));
        assert!(reason.contains("diff:"));
        assert!(reason.contains("conflicts with the diff"));
        assert!(!reason.contains("revised plan"));
        assert!(!reason.contains("plan:"));
        assert!(!reason.contains("conflicts with the plan"));
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
        let reason = hook_deny_reason(&[&a], Some("sess-123"), None, check::ArtifactKind::Plan);
        assert!(!reason.contains("--session sess-123 --rule"));
        assert!(!reason.contains(&governance_session::key(&a.doc_slug, &a.rule_id)));
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
        let reason = hook_deny_reason(&[&a], None, None, check::ArtifactKind::Plan);
        assert!(reason.contains("DECISION R-A-001"));
        assert!(!reason.contains("CONFLICT R-A-001"));
    }

    #[test]
    fn test_hook_deny_reason_labels_a_conflicting_verdict_distinctly() {
        let a = checked("R-A-002", Verdict::Conflicting, "span", "reason");
        let reason = hook_deny_reason(&[&a], None, None, check::ArtifactKind::Plan);
        assert!(reason.contains("CONFLICT R-A-002"));
        assert!(!reason.contains("DECISION R-A-002"));
    }

    #[test]
    fn test_hook_deny_reason_discloses_partial_coverage_when_present() {
        let a = checked("R-A-002", Verdict::Conflicting, "span", "reason");
        let reason = hook_deny_reason(&[&a], None, Some((60, 85)), check::ArtifactKind::Plan);
        assert!(reason.contains("Only 60 of 85"));
    }

    #[test]
    fn test_hook_deny_reason_omits_partial_note_when_absent() {
        let a = checked("R-A-002", Verdict::Conflicting, "span", "reason");
        let reason = hook_deny_reason(&[&a], None, None, check::ArtifactKind::Plan);
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
            check::ArtifactKind::Plan,
        );
        assert!(panel.contains("No committed rule document applies"));
        assert!(panel.contains("Plan check"));
        assert!(panel.contains("this plan"));
        assert!(!panel.contains("Implementation check"));
    }

    /// The gap this guards: impl-check used to call the shared renderer and
    /// print a "Plan check" panel with a truncated unified diff in the Plan
    /// row, even though the empty-diff path already knew the title should
    /// be "Implementation check."
    #[test]
    fn test_render_panel_impl_check_titles_implementation_check() {
        let panel = render_panel(
            &Outcome::NothingApplies,
            "diff --git a/oauth.rs b/oauth.rs",
            Path::new("/x/.actual/rules"),
            80,
            check::ArtifactKind::Diff,
        );
        assert!(panel.contains("Implementation check"));
        assert!(panel.contains("Diff"));
        assert!(panel.contains("this diff"));
        assert!(!panel.contains("Plan check"));
        assert!(!panel.contains("this plan"));
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
        let panel = render_panel(
            &outcome,
            "a plan",
            Path::new("/x/.actual/rules"),
            80,
            check::ArtifactKind::Plan,
        );
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
        let panel = render_panel(
            &outcome,
            "a plan",
            Path::new("/x/.actual/rules"),
            80,
            check::ArtifactKind::Plan,
        );
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
        let panel = render_panel(
            &outcome,
            "a plan",
            Path::new("/x/.actual/rules"),
            80,
            check::ArtifactKind::Plan,
        );
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
        let json = render_json(&conforming, check::ArtifactKind::Plan);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "conforming");
        assert!(value.get("partial").is_none());

        let conflicting = Outcome::Verdicts {
            selection: selection.clone(),
            verdicts: vec![checked("R-A-002", Verdict::Conflicting, "x", "y")],
            runner_label: None,
            partial: None,
        };
        let json = render_json(&conflicting, check::ArtifactKind::Plan);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "conflicting");

        let requires_decision = Outcome::Verdicts {
            selection: selection.clone(),
            verdicts: vec![checked("R-A-003", Verdict::RequiresDecision, "x", "y")],
            runner_label: None,
            partial: None,
        };
        let json = render_json(&requires_decision, check::ArtifactKind::Plan);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "requires_decision");

        let partial = Outcome::Verdicts {
            selection: selection.clone(),
            verdicts: vec![checked("R-A-001", Verdict::Conforming, "", "")],
            runner_label: None,
            partial: Some((60, 85)),
        };
        let json = render_json(&partial, check::ArtifactKind::Plan);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "conforming");
        assert_eq!(value["partial"]["judged"], 60);
        assert_eq!(value["partial"]["total"], 85);

        let json = render_json(&Outcome::NothingApplies, check::ArtifactKind::Plan);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "not_checked");
        assert_eq!(
            value["detail"],
            "no committed rule document applies to this plan"
        );

        let json = render_json(&Outcome::NothingApplies, check::ArtifactKind::Diff);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            value["detail"],
            "no committed rule document applies to this diff"
        );
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
        let value: serde_json::Value =
            serde_json::from_str(&render_json(&no_runner, check::ArtifactKind::Plan)).unwrap();
        assert_eq!(value["status"], "not_checked");
        assert_eq!(value["documents_selected"], 7);

        let check_failed = Outcome::CheckFailed {
            documents_selected: 3,
            reason: "runner timed out".to_string(),
        };
        let value: serde_json::Value =
            serde_json::from_str(&render_json(&check_failed, check::ArtifactKind::Plan)).unwrap();
        assert_eq!(value["status"], "not_checked");
        assert_eq!(value["documents_selected"], 3);
    }

    #[test]
    fn test_render_panel_reports_documents_selected_on_no_runner_and_check_failed() {
        let no_runner = Outcome::NoRunner {
            documents_selected: 7,
            reason: "no ANTHROPIC_API_KEY".to_string(),
        };
        let panel = render_panel(
            &no_runner,
            "a plan",
            Path::new("/x/.actual/rules"),
            80,
            check::ArtifactKind::Plan,
        );
        assert!(panel.contains("Documents selected"));
        assert!(panel.contains('7'));

        let check_failed = Outcome::CheckFailed {
            documents_selected: 3,
            reason: "runner timed out".to_string(),
        };
        let panel = render_panel(
            &check_failed,
            "a plan",
            Path::new("/x/.actual/rules"),
            80,
            check::ArtifactKind::Plan,
        );
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
    fn test_audit_log_note_names_the_shared_log_only_for_impl_check() {
        let plan = audit_log_note(check::ArtifactKind::Plan);
        assert_eq!(plan, "plan-check-overrides.log");

        let diff = audit_log_note(check::ArtifactKind::Diff);
        assert!(diff.contains("plan-check-overrides.log"), "{diff}");
        assert!(diff.contains("shares with plan-check"), "{diff}");
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
            &GovernanceSession::default(),
            check::ArtifactKind::Plan,
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
            &GovernanceSession::default(),
            check::ArtifactKind::Plan,
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
            &args.check_knobs(),
            check::ArtifactKind::Plan,
            false,
            &GovernanceSession::default(),
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
            &args.check_knobs(),
            check::ArtifactKind::Plan,
            true,
            &GovernanceSession::default(),
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
            &args.check_knobs(),
            check::ArtifactKind::Plan,
            false,
            &GovernanceSession::default(),
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
            &args.check_knobs(),
            check::ArtifactKind::Plan,
            false,
            &GovernanceSession::default(),
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
            &args.check_knobs(),
            check::ArtifactKind::Plan,
            false,
            &GovernanceSession::default(),
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
            &args.check_knobs(),
            check::ArtifactKind::Plan,
            false,
            &GovernanceSession::default(),
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
            &args.check_knobs(),
            check::ArtifactKind::Plan,
            false,
            &GovernanceSession::default(),
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
            &args.check_knobs(),
            check::ArtifactKind::Plan,
            true,
            &GovernanceSession::default(),
        )
        .unwrap();
        assert!(matches!(
            outcome,
            Outcome::CheckFailed { .. } | Outcome::Verdicts { .. }
        ));
    }

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
        let mut session = GovernanceSession::default();
        session.plan.cleared.insert(
            governance_session::key("cross-cutting-token-signing-1c57", "R-A-001"),
            governance_session::content_digest(plan_text),
        );

        let outcome = run_pipeline(
            plan_text,
            root.path(),
            &rules_dir,
            &args.check_knobs(),
            check::ArtifactKind::Plan,
            false,
            &session,
        )
        .unwrap();

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
        let mut session = GovernanceSession::default();
        session.overrides.push(governance_session::Override {
            key: governance_session::key("cross-cutting-token-signing-1c57", "R-A-001"),
            reason: "reviewed".to_string(),
            at: chrono::Utc::now(),
            round: 1,
        });
        session.overrides.push(governance_session::Override {
            key: governance_session::key("cross-cutting-token-signing-1c57", "R-A-002"),
            reason: "reviewed".to_string(),
            at: chrono::Utc::now(),
            round: 1,
        });

        let outcome = run_pipeline(
            "Sign access tokens with RS256",
            root.path(),
            &rules_dir,
            &args.check_knobs(),
            check::ArtifactKind::Plan,
            false,
            &session,
        )
        .unwrap();

        assert!(matches!(
            outcome,
            Outcome::Verdicts { ref verdicts, runner_label: None, .. } if verdicts.is_empty()
        ));
    }

    #[test]
    fn test_override_reminder_none_without_any_overrides() {
        assert!(override_reminder(&GovernanceSession::default()).is_none());
    }

    #[test]
    fn test_override_reminder_names_the_rule_and_reason() {
        let mut session = GovernanceSession::default();
        session.overrides.push(governance_session::Override {
            key: governance_session::key("doc", "R-001"),
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
            &GovernanceSession::default(),
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
        let mut session = GovernanceSession::default();
        session.overrides.push(governance_session::Override {
            key: governance_session::key("doc", "R-002"),
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
        let mut session = GovernanceSession::default();
        session
            .plan
            .deny_counts
            .insert(governance_session::key(&a.doc_slug, &a.rule_id), 4);
        let message = round_limit_message(&[&a], &session, check::ArtifactKind::Plan, 3);
        assert!(message.contains("R-A-002"));
        assert!(message.contains("denied 4 times"));
        assert!(message.contains("round limit (3"));
        assert!(message.contains("Actual plan governance"));
        assert!(!message.contains("implementation governance"));
    }

    #[test]
    fn test_round_limit_message_impl_check_names_implementation_governance() {
        let a = checked("R-A-002", Verdict::Conflicting, "span", "reason");
        let mut session = GovernanceSession::default();
        session
            .diff
            .deny_counts
            .insert(governance_session::key(&a.doc_slug, &a.rule_id), 4);
        let message = round_limit_message(&[&a], &session, check::ArtifactKind::Diff, 3);
        assert!(message.contains("Actual implementation governance"));
        assert!(!message.contains("plan governance"));
        assert!(
            message.contains(
                "plan-check-overrides.log (the audit log impl-check shares with plan-check)"
            ),
            "{message}"
        );
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
        let repo = seed(&[("doc.md", OAUTH_DOC)]);

        let args = PlanCheckOverrideArgs {
            session: "sess-cli-1".to_string(),
            rules: vec![governance_session::key("doc", "R-A-001")],
            reason: "reviewed and accepted".to_string(),
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
        };
        assert!(exec_override_impl(&args).is_ok());

        let rules_dir = crate::rules::rules_dir(repo.path());
        let session = governance_session::load("sess-cli-1", &rules_dir);
        assert_eq!(session.overrides.len(), 1);
        assert_eq!(session.overrides[0].reason, "reviewed and accepted");
    }

    /// AK-678: an override emits one `check_completed`-shaped event per
    /// cleared rule, tagged `command = "check-override"` and
    /// `decision = allow` -- distinct from a real check run's completion, so
    /// "overrides recorded" is derivable as `count(command ==
    /// "check-override")` without a new event name.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_exec_override_impl_emits_governance_events_per_rule() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = seed(&[("doc.md", OAUTH_DOC)]);

        let args = PlanCheckOverrideArgs {
            session: "sess-cli-telemetry-1".to_string(),
            rules: vec![
                governance_session::key("doc", "R-A-001"),
                governance_session::key("doc", "R-A-002"),
            ],
            reason: "reviewed and accepted".to_string(),
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
        };

        let events =
            with_captured_plan_governance_events(|| assert!(exec_override_impl(&args).is_ok()));

        assert_eq!(events.len(), 2);
        for event in &events {
            assert_eq!(
                event.event,
                crate::api::types::PlanGovernanceEventName::PlanGovernanceCheckCompleted
            );
            let props = event.properties.as_ref().unwrap();
            assert_eq!(props.command.as_deref(), Some("check-override"));
            assert_eq!(
                props.decision,
                Some(crate::api::types::PlanGovernanceDecision::Allow)
            );
            assert_eq!(props.rule_source.as_deref(), Some("doc"));
        }
        let rule_ids: std::collections::HashSet<_> = events
            .iter()
            .map(|e| e.properties.as_ref().unwrap().rule_id.clone().unwrap())
            .collect();
        assert_eq!(
            rule_ids,
            std::collections::HashSet::from(["R-A-001".to_string(), "R-A-002".to_string()])
        );
    }

    /// The proxy rejects a batch over 100 events whole (see
    /// `plan_governance::MAX_EVENTS_PER_BATCH`), and `check-override`
    /// emits one event per `--rule` flag with no cap of its own -- so an
    /// override naming more than 100 rules at once must still get every
    /// event through by chunking, not lose the tail to an oversized batch.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_exec_override_impl_chunks_batches_over_the_event_cap() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        const RULE_COUNT: usize = 130;
        let mut doc = String::from(
            "# Many Rules\n\nThese rules are ALWAYS ACTIVE for everything.\n\n### Rules\n\n",
        );
        for n in 0..RULE_COUNT {
            doc.push_str(&format!("- **R-A-{n:03}** MUST: satisfy condition {n}.\n"));
        }
        let repo = seed(&[("doc.md", &doc)]);

        let rules: Vec<String> = (0..RULE_COUNT)
            .map(|n| governance_session::key("doc", &format!("R-A-{n:03}")))
            .collect();
        let args = PlanCheckOverrideArgs {
            session: "sess-cli-chunking-1".to_string(),
            rules,
            reason: "bulk reviewed".to_string(),
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
        };

        let events =
            with_captured_plan_governance_events(|| assert!(exec_override_impl(&args).is_ok()));

        assert_eq!(
            events.len(),
            RULE_COUNT,
            "every override event must survive chunking, not just the first 100"
        );
        let rule_ids: std::collections::HashSet<_> = events
            .iter()
            .map(|e| e.properties.as_ref().unwrap().rule_id.clone().unwrap())
            .collect();
        assert_eq!(rule_ids.len(), RULE_COUNT);
    }

    /// AK-678 opt-out fix: `ACTUAL_NO_TELEMETRY` must stop `distinct_id()`
    /// from ever running, not just stop the outbound POST -- otherwise an
    /// opted-out user still gets a persistent `telemetry-id` file written to
    /// their config dir the first time they record an override. See
    /// `send_override_events`'s doc comment and `PRIVACY.md`.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_exec_override_impl_opt_out_env_var_skips_events_and_id_file() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _no_telemetry = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
        let repo = seed(&[("doc.md", OAUTH_DOC)]);

        let args = PlanCheckOverrideArgs {
            session: "sess-cli-opt-out-1".to_string(),
            rules: vec![governance_session::key("doc", "R-A-001")],
            reason: "reviewed and accepted".to_string(),
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
        };

        let events =
            with_captured_plan_governance_events(|| assert!(exec_override_impl(&args).is_ok()));

        assert!(
            events.is_empty(),
            "opted-out user must get no governance events"
        );
        assert!(
            !home.path().join("telemetry-id").exists(),
            "opted-out user must get no persistent telemetry id file"
        );
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
        std::fs::write(rules_dir.path().join("doc.md"), OAUTH_DOC).unwrap();

        let args = PlanCheckOverrideArgs {
            session: "sess-cli-2".to_string(),
            rules: vec![governance_session::key("doc", "R-A-001")],
            reason: "reviewed".to_string(),
            repo: None,
            rules_dir: Some(rules_dir.path().to_path_buf()),
        };
        assert!(exec_override_impl(&args).is_ok());

        let session = governance_session::load("sess-cli-2", rules_dir.path());
        assert_eq!(session.overrides.len(), 1);
    }

    /// APR-004: a syntactically valid but nonexistent rule id must be
    /// rejected before it is ever recorded, not silently accepted as a
    /// no-op override.
    #[test]
    fn test_exec_override_impl_rejects_an_unknown_rule_id() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());

        let args = PlanCheckOverrideArgs {
            session: "sess-cli-unknown-rule".to_string(),
            rules: vec![governance_session::key("doc", "R-A-999")],
            reason: "reviewed".to_string(),
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
        };
        let err = exec_override_impl(&args).unwrap_err();
        assert!(err.to_string().contains("R-A-999"));
        assert!(err.to_string().contains("not found"));

        // Nothing was recorded -- the corpus rejected it before
        // `record_override` ever ran.
        assert_eq!(
            governance_session::load("sess-cli-unknown-rule", &rules_dir),
            GovernanceSession::default()
        );
    }

    /// Same as the unknown-rule case, but the document slug itself is wrong
    /// — a typo'd `<doc-slug>` is just as silent a no-op as a typo'd
    /// `<rule-id>` without this check.
    #[test]
    fn test_exec_override_impl_rejects_an_unknown_document_slug() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = seed(&[("doc.md", OAUTH_DOC)]);

        let args = PlanCheckOverrideArgs {
            session: "sess-cli-unknown-doc".to_string(),
            rules: vec![governance_session::key("no-such-doc", "R-A-001")],
            reason: "reviewed".to_string(),
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
        };
        let err = exec_override_impl(&args).unwrap_err();
        assert!(err.to_string().contains("no-such-doc::R-A-001"));
    }

    /// A `--rules-dir` that does not exist at all must not make every key
    /// look plausible by accident: an empty corpus rejects everything,
    /// exactly like a real corpus that simply lacks the named rule.
    #[test]
    fn test_exec_override_impl_rejects_everything_under_a_nonexistent_rules_dir() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let missing = tempdir().unwrap().path().join("does-not-exist");

        let args = PlanCheckOverrideArgs {
            session: "sess-cli-missing-dir".to_string(),
            rules: vec![governance_session::key("doc", "R-A-001")],
            reason: "reviewed".to_string(),
            repo: None,
            rules_dir: Some(missing),
        };
        assert!(exec_override_impl(&args).is_err());
    }

    /// Multi-rule atomicity: one unknown key among several must reject the
    /// whole call, not silently record the valid ones while dropping the
    /// bad one -- a human asking to clear several rules together would not
    /// expect a partial result with no indication which half actually took.
    #[test]
    fn test_exec_override_impl_is_all_or_nothing_across_multiple_rules() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());

        let args = PlanCheckOverrideArgs {
            session: "sess-cli-partial".to_string(),
            rules: vec![
                governance_session::key("doc", "R-A-001"),
                governance_session::key("doc", "R-A-999"),
            ],
            reason: "reviewed".to_string(),
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
        };
        assert!(exec_override_impl(&args).is_err());
        assert_eq!(
            governance_session::load("sess-cli-partial", &rules_dir),
            GovernanceSession::default()
        );
    }

    /// Two or more unknown keys must pluralize the rejection message
    /// ("rules ... were not found", not "rule ... was not found") — the
    /// singular/plural branch in `validate_override_rules` only runs its
    /// plural arm when `unknown.len() > 1`.
    #[test]
    fn test_exec_override_impl_pluralizes_the_message_for_multiple_unknown_rules() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = seed(&[("doc.md", OAUTH_DOC)]);

        let args = PlanCheckOverrideArgs {
            session: "sess-cli-multi-unknown".to_string(),
            rules: vec![
                governance_session::key("doc", "R-A-998"),
                governance_session::key("doc", "R-A-999"),
            ],
            reason: "reviewed".to_string(),
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
        };
        let err = exec_override_impl(&args).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("rules"), "message was: {message}");
        assert!(message.contains("were not found"), "message was: {message}");
        assert!(message.contains("doc::R-A-998"), "message was: {message}");
        assert!(message.contains("doc::R-A-999"), "message was: {message}");
    }
}
