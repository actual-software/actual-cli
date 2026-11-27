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

use std::collections::BTreeMap;
use std::io::{IsTerminal, Read};
use std::path::Path;

use serde::Serialize;

use crate::api::types::PlanGovernanceDecision;
use crate::cli::args::{
    OverrideGateArg, OverrideVia, PlanCheckArgs, PlanCheckOverrideArgs, RunnerChoice,
};
use crate::cli::commands::enforcement_log::{self, Mode, Round};
use crate::cli::commands::governance_session::{
    self, GovernanceSession, Pending, PickerAsk, PickerStage,
};
use crate::cli::commands::hook_host::HookHost;
use crate::cli::commands::plan_check::repo_root;
use crate::cli::commands::plan_check_hook;
use crate::cli::commands::rules_rank;
use crate::cli::ui::panel::Panel;
use crate::cli::ui::term_size;
use crate::error::ActualError;
use crate::rules::check::{self, CheckedRule, RuleForJudging, Verdict};
use crate::rules::scope::{self, select, FilterTally, KeptFile, Query, Selection, Stage2};
use crate::rules::types::AdrMeta;

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
    /// The runners tried when nothing names one. See
    /// [`rules_rank::resolve_with_defaults`].
    pub(super) default_runners: &'static [RunnerChoice],
}

impl CheckKnobs<'_> {
    /// These knobs for a check running in `host`: inside Codex, the OpenAI
    /// backends are tried first when nothing names a runner.
    pub(super) fn for_host(self, host: HookHost) -> Self {
        match host {
            HookHost::Claude => self,
            HookHost::Codex => Self {
                default_runners: rules_rank::CODEX_DEFAULT_RUNNERS,
                ..self
            },
        }
    }
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
            default_runners: rules_rank::DEFAULT_RUNNERS,
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
        /// The v2 header of each selected document that has one, by slug:
        /// what names the ADR behind a verdict's rule.
        headers: BTreeMap<String, AdrMeta>,
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
    run_pipeline_filtered(plan_text, root, rules_dir, knobs, kind, use_rank, session)
        .map(|(outcome, _)| outcome)
}

/// [`run_pipeline`], with the account of what selection kept and dropped on
/// the way to the judge. The account is `None` only when a rank found no
/// runner, which stops the round before anything is selected.
pub(super) fn run_pipeline_filtered(
    plan_text: &str,
    root: &Path,
    rules_dir: &Path,
    knobs: &CheckKnobs,
    kind: check::ArtifactKind,
    use_rank: bool,
    session: &GovernanceSession,
) -> Result<(Outcome, Option<FilterTally>), ActualError> {
    let plan_digest = governance_session::content_digest(plan_text);
    let resolved = scope::resolve_in(rules_dir, root, knobs.rebuild)?;
    let query = match kind {
        check::ArtifactKind::Plan => Query::new(plan_text),
        check::ArtifactKind::Diff => Query::for_diff(plan_text),
    };
    let prefiltered = select::prefilter(&resolved.index, &query, knobs.limit, knobs.candidates);

    if prefiltered.is_empty() {
        let selection = prefiltered.finish(Stage2::NotRequested);
        let tally = filter_tally(&prefiltered, &selection, &GatheredRules::default());
        return Ok((Outcome::NothingApplies, Some(tally)));
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
        match rules_rank::resolve_with_defaults(
            knobs.runner,
            knobs.model,
            &cfg,
            rules_rank::RANK_TIMEOUT_SECS,
            knobs.default_runners,
        ) {
            Ok(runner) => Some(runner),
            Err(reason) => {
                return Ok((
                    Outcome::NoRunner {
                        documents_selected: prefiltered.len(),
                        reason,
                    },
                    None,
                ))
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
        let tally = filter_tally(&prefiltered, &selection, &GatheredRules::default());
        return Ok((Outcome::NothingApplies, Some(tally)));
    }

    let gathered = gather_rules(&selection, root, session, kind, &plan_digest);
    let tally = filter_tally(&prefiltered, &selection, &gathered);
    if gathered.rules.is_empty() {
        let outcome = if gathered.excluded > 0 {
            // Every applicable rule was already settled this session: there
            // is nothing new to judge, which is the fully-conforming case,
            // not "no rule applies" (this plan does have applicable rules —
            // they were just already cleared or overridden).
            Outcome::Verdicts {
                selection,
                verdicts: Vec::new(),
                runner_label: None,
                partial: None,
                headers: gathered.headers,
            }
        } else {
            Outcome::NothingApplies
        };
        return Ok((outcome, Some(tally)));
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
    let resolved_runner = match rules_rank::resolve_with_defaults(
        knobs.runner,
        knobs.model,
        &cfg,
        crate::rules::check::CHECK_TIMEOUT_SECS,
        knobs.default_runners,
    ) {
        Ok(runner) => runner,
        Err(reason) => {
            return Ok((
                Outcome::NoRunner {
                    documents_selected: selection.selected.len(),
                    reason,
                },
                Some(tally),
            ))
        }
    };
    let label = resolved_runner.label();

    let outcome = match runtime.block_on(check::check(
        &resolved_runner.runner,
        kind,
        plan_text,
        &gathered.rules,
        resolved_runner.model.as_deref(),
        resolved_runner.max_budget_usd,
    )) {
        Ok(verdicts) => Outcome::Verdicts {
            selection,
            verdicts,
            runner_label: Some(label),
            partial: gathered
                .truncated
                .then_some((gathered.rules.len(), gathered.considered)),
            headers: gathered.headers,
        },
        Err(e) => Outcome::CheckFailed {
            documents_selected: selection.selected.len(),
            reason: e.to_string(),
        },
    };
    Ok((outcome, Some(tally)))
}

/// The account of one round's selection: stage 1's and the limit's drops from
/// `prefiltered`, then what became of each selected file in `gathered`.
fn filter_tally(
    prefiltered: &select::Prefiltered,
    selection: &Selection,
    gathered: &GatheredRules,
) -> FilterTally {
    let mut tally = prefiltered.tally(selection);
    tally.inactive += gathered.unusable;
    tally.session = gathered.settled;
    tally.window = gathered.windowed;
    tally.kept = gathered
        .judged
        .iter()
        .map(|&at| {
            let rule = &selection.selected[at];
            KeptFile::new(
                rule.candidate_group_id.as_deref(),
                rule.adr_id.as_deref(),
                rule.score,
            )
        })
        .collect();
    tally
}

/// The rules gathered from every selected document, capped at
/// [`MAX_RULES_JUDGED`], and whether the true count exceeded that cap.
#[derive(Default)]
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
    /// The v2 header of each document read, by slug. A document without one
    /// is absent.
    headers: BTreeMap<String, AdrMeta>,
    /// Selected documents, by position in the selection, with a rule in
    /// `rules`.
    judged: Vec<usize>,
    /// Selected documents that no longer read, parse or are active.
    unusable: usize,
    /// Selected documents whose every rule `session` excludes.
    settled: usize,
    /// Selected documents with a rule still to judge, none of them inside the
    /// window.
    windowed: usize,
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
    let mut headers = BTreeMap::new();
    let mut unusable = 0usize;
    let mut settled = 0usize;
    // Documents with a rule still to judge, by selection position and slug.
    let mut live: Vec<(usize, &str)> = Vec::new();
    for (position, selected) in selection.selected.iter().enumerate() {
        let path = root.join(&selected.relative_path);
        let Some(doc) = read_active(&path) else {
            unusable += 1;
            continue;
        };
        if doc.rules.iter().all(|rule| {
            session.excludes(
                kind,
                &governance_session::key(&selected.slug, &rule.id),
                plan_digest,
            )
        }) {
            settled += 1;
        } else {
            live.push((position, &selected.slug));
        }
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
        if let Some(meta) = doc.meta {
            headers.insert(selected.slug.clone(), meta);
        }
    }
    let considered = candidates.len();
    let truncated = considered > MAX_RULES_JUDGED;
    if truncated {
        candidates = select_judged_window(candidates, session, kind);
    }
    let (judged, windowed): (Vec<_>, Vec<_>) = live
        .into_iter()
        .partition(|(_, slug)| candidates.iter().any(|rule| rule.doc_slug == *slug));
    GatheredRules {
        rules: candidates,
        considered,
        truncated,
        excluded,
        headers,
        judged: judged.into_iter().map(|(position, _)| position).collect(),
        unusable,
        settled,
        windowed: windowed.len(),
    }
}

/// The rule document at `path`, when it reads, parses and is still active.
/// The index already leaves inactive decisions out; this is the same check
/// for a file edited to a retired status after selection.
fn read_active(path: &Path) -> Option<crate::rules::types::RuleDocument> {
    let text = String::from_utf8(std::fs::read(path).ok()?).ok()?;
    let doc = crate::rules::parse_rule_document(path, &text).ok()?;
    doc.is_active().then_some(doc)
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
    panel = panel.kv(copy.artifact_label, &truncate_line(text, 72));
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
            ..
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
    let panel = panel.line(&format!("      {}", truncate_line(&rule.reason, 68)));
    panel.line(&format!("      \"{}\"", truncate_line(&rule.span, 68)))
}

#[derive(Serialize)]
struct PartialCoverage {
    /// How many rules were actually judged.
    judged: usize,
    /// How many rules applied in total, including the untruncated tail.
    total: usize,
}

#[derive(Serialize)]
pub(super) struct CheckJson {
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
    serde_json::to_string_pretty(&check_json(outcome, kind))
        .expect("plan check report is serializable — this is a programmer error")
}

/// [`render_json`]'s report, for a caller that prints fields of its own
/// beside it.
pub(super) fn check_json(outcome: &Outcome, kind: check::ArtifactKind) -> CheckJson {
    let copy = artifact_copy(kind);
    match outcome {
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
            ..
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
    }
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

/// Build the hashed identity + timestamp envelope for a governance event.
///
/// Reads only local sources — stored OAuth credentials and a config sticky-scope
/// pin — so it never adds a network call on the hook path. When logged in, the
/// user/org ids are hashed; otherwise a synthetic fallback org id (from the repo
/// slug) is set. Either way the connected-repo id is hashed if a sticky pin
/// records it. Never carries a raw identifier.
#[cfg(feature = "telemetry")]
fn identity_envelope(
    cfg: &crate::config::types::Config,
    repo_url: &str,
) -> crate::telemetry::identity::IdentityEnvelope {
    // Single local-only identity lookup shared with the scope-select emitter.
    crate::telemetry::identity::IdentityEnvelope::resolve(cfg, repo_url)
}

/// Hand a batch of already-built plan-governance events to the detached
/// uploader, the path `rules select` already takes: spool every event, start
/// `__telemetry-flush`, and return without waiting for it.
///
/// The command itself makes no network call, so a check returns its verdict
/// however slow or unreachable the endpoint is. An event an outage keeps from
/// the proxy stays in the spool for the next run's uploader, one the proxy
/// refuses on its own is dropped, and a failed or disabled send never fails or
/// slows the check.
///
/// The `#[cfg(test)]` twin below captures events into a thread-local instead,
/// so every existing (and future) `exec_hook_with`/`exec_direct`/
/// `exec_override_impl` test stays hermetic, writing no spool and starting no
/// uploader, while still letting tests that care assert on exactly what would
/// have been sent — see `tests::take_captured_plan_governance_events`.
#[cfg(all(feature = "telemetry", not(test)))]
fn dispatch_governance_events(events: Vec<crate::api::types::PlanGovernanceEvent>) {
    spool_governance_events(&events, super::rules_scope::spawn_uploader);
}

#[cfg(all(feature = "telemetry", test))]
fn dispatch_governance_events(events: Vec<crate::api::types::PlanGovernanceEvent>) {
    tests::CAPTURED_PLAN_GOVERNANCE_EVENTS.with(|cell| cell.borrow_mut().extend(events));
}

/// Spool `events`, then call `spawn_uploader`. Split from
/// [`dispatch_governance_events`] so the hand-off runs under test with a
/// stand-in for the uploader. An event the spool turns away is dropped, and
/// the drop is logged at debug.
#[cfg(feature = "telemetry")]
fn spool_governance_events(
    events: &[crate::api::types::PlanGovernanceEvent],
    spawn_uploader: impl FnOnce(),
) {
    for event in events {
        if let Err(e) = crate::telemetry::spool::append(event) {
            tracing::debug!("plan-governance telemetry: event not spooled, dropped: {e}");
        }
    }
    spawn_uploader();
}

/// Build repo-identity hashes, assemble a started+completed(+violation)
/// batch of plan-governance events for one `plan-check`/`--claude-hook` run,
/// and dispatch them. Only rule id/slug and coarse enums ever go into
/// `violations` — never a rule's span, statement, or reason text; see
/// `PRIVACY.md`.
///
/// Checks `plan_governance::gate` first, before any repo-identity hashing or
/// `distinct_id()` — an opted-out user must not have a persistent telemetry
/// id written to disk (or git subprocesses spawned) just because a check
/// ran; the uploader's own opt-out check is too late for that, since by
/// then the id file already exists. See `PRIVACY.md`'s "How to opt out of
/// telemetry." Telemetry debug mode with telemetry off builds the events with
/// no id written and logs them as not sent.
#[cfg(feature = "telemetry")]
pub(super) fn send_governance_events(
    command: &str,
    root: &Path,
    started_at: std::time::Instant,
    decision: crate::api::types::PlanGovernanceDecision,
    exit_code: i32,
    violations: &[(&str, &str, crate::api::types::PlanGovernanceDecision)],
    rounds: Option<(u32, u32)>,
) {
    use crate::telemetry::plan_governance::{self, EventContext};

    let Some((cfg, send)) = plan_governance::gate() else {
        return;
    };

    let repo_url = crate::analysis::cache::get_git_remote_origin_url(root).unwrap_or_default();
    let (repo_hash, repo_url_hash) = repo_identity_hashes(root);
    let identity = identity_envelope(&cfg, &repo_url);
    let mut ctx = EventContext::with_distinct_id(command, plan_governance::distinct_id_for(send))
        .with_repo_hashes(repo_hash, repo_url_hash)
        .with_identity(identity);
    if let Some((round_index, round_total)) = rounds {
        ctx = ctx.with_rounds(round_index, round_total);
    }
    // `duration_ms` uses the precise monotonic elapsed time. Both events'
    // `timestamp` fields are stamped as the run hands them to the spool (the
    // uploader sends them later as they are), not backdated to when the check
    // actually began -- for this stream's aggregate-counting use, ordering/dedup
    // is what `timestamp` is for, and `duration_ms` is already the authoritative
    // latency figure.
    let duration_ms = started_at.elapsed().as_secs_f64() * 1000.0;

    let mut events = vec![
        ctx.started_event(),
        ctx.completed_event(decision, duration_ms, exit_code),
    ];
    for (rule_id, rule_source, v_decision) in violations {
        events.push(ctx.violation_event(rule_id, rule_source, *v_decision));
    }

    if send {
        dispatch_governance_events(events);
    } else {
        plan_governance::hold_back(&events);
    }
}

/// Emit one `plan_governance_rule_override` event per rule an
/// `actual check-override` call clears — a first-class event name (not a
/// `check_completed` with `command="check-override"`) so overrides are cleanly
/// separable from real check completions and override rate has an unambiguous
/// denominator. Each event also carries a privacy-preserving summary of the
/// override `reason` (length bucket and length — never the text; see `reason_summary`
/// and `PRIVACY.md`). `keys` are already `"<doc-slug>::<rule-id>"` strings (see
/// `governance_session::key`), validated against the rule corpus by
/// [`validate_override_rules`] before this is ever called.
///
/// Checks `plan_governance::gate` first, before any repo-identity hashing or
/// `distinct_id()` — see `send_governance_events`'s doc comment for why
/// that ordering matters.
#[cfg(feature = "telemetry")]
fn send_override_events(
    root: &Path,
    keys: &[String],
    reason: &str,
    source: governance_session::OverrideSource,
    gates: &[Option<governance_session::OverrideGate>],
) {
    use crate::api::types::{
        PlanGovernanceDecision, PlanGovernanceEvent, PlanGovernanceEventName,
        PlanGovernanceEventProperties,
    };
    use crate::telemetry::plan_governance::{self, reason_summary};

    let Some((cfg, send)) = plan_governance::gate() else {
        return;
    };

    let repo_url = crate::analysis::cache::get_git_remote_origin_url(root).unwrap_or_default();
    let (repo_hash, repo_url_hash) = repo_identity_hashes(root);
    // Same hashed identity/timestamp envelope every other governance event carries.
    let identity = identity_envelope(&cfg, &repo_url);
    let id = plan_governance::distinct_id_for(send);
    let cli_version = env!("CARGO_PKG_VERSION").to_string();
    let timestamp = chrono::Utc::now().to_rfc3339();
    // The reason is customer free text: events carry only its length bucket
    // and length, never the text or a hash of it (see `PRIVACY.md`).
    let (reason_length_bucket, reason_len) = reason_summary(reason);

    let events: Vec<PlanGovernanceEvent> = keys
        .iter()
        .enumerate()
        .map(|(i, key)| {
            let (rule_source, rule_id) = key.split_once("::").unwrap_or((key.as_str(), ""));
            let mut properties = PlanGovernanceEventProperties {
                cli_version: Some(cli_version.clone()),
                command: Some("check-override".to_string()),
                rule_id: Some(rule_id.to_string()),
                rule_source: Some(rule_source.to_string()),
                decision: Some(PlanGovernanceDecision::Allow),
                exit_code: Some(0),
                repo_hash: Some(repo_hash.clone()),
                repo_url_hash: Some(repo_url_hash.clone()),
                reason_length_bucket: Some(reason_length_bucket),
                reason_len: Some(reason_len),
                override_source: Some(source.as_str().to_string()),
                override_gate: gates
                    .get(i)
                    .copied()
                    .flatten()
                    .map(|gate| gate.as_str().to_string()),
                ..Default::default()
            };
            identity.apply_to(&mut properties);
            PlanGovernanceEvent {
                event: PlanGovernanceEventName::PlanGovernanceRuleOverride,
                distinct_id: id.clone(),
                properties: Some(properties),
                timestamp: Some(timestamp.clone()),
                insert_id: crate::telemetry::plan_governance::new_insert_id(),
            }
        })
        .collect();

    if send {
        dispatch_governance_events(events);
    } else {
        plan_governance::hold_back(&events);
    }
}

/// `--claude-hook`-mode wrapper around [`send_governance_events`]: every
/// non-passing verdict this round gets a violation event with the same
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
    // `(round_index, round_total)` for the `--claude-hook` revision loop.
    rounds: (u32, u32),
) {
    let violations: Vec<(&str, &str, crate::api::types::PlanGovernanceDecision)> = verdicts
        .iter()
        .filter(|v| !v.verdict.passes())
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
    send_governance_events(
        command,
        root,
        started_at,
        decision,
        0,
        &violations,
        Some(rounds),
    );
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
///
/// Codex's own markers count too. Every command Codex's shell tool runs
/// carries `CODEX_THREAD_ID` and `CODEX_SESSION_ID` (observed on Codex CLI
/// 0.167.0), so an agent in Codex is refused the same way.
fn running_under_agent_host() -> bool {
    [
        "CLAUDECODE",
        "CLAUDE_CODE_ENTRYPOINT",
        "CODEX_THREAD_ID",
        "CODEX_SESSION_ID",
    ]
    .iter()
    .any(|name| std::env::var_os(name).is_some())
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
/// or `socat`, could still spoof one), and [`running_under_agent_host`]
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
/// every other `--claude-hook` stdin test there does. [`running_under_agent_host`]
/// carries no such restriction and is exercised directly. Everything else
/// that does not depend on the real terminal lives in [`exec_override_impl`],
/// which is also exercised directly.
pub fn exec_override(args: &PlanCheckOverrideArgs) -> Result<(), ActualError> {
    if args.claude_hook {
        // The envelope is only JSON on stdin, so the event name in it proves
        // nothing. What the caller's environment says is the check: every
        // command an agent runs, and every settings hook, inherits Claude
        // Code's CLAUDECODE marker, while the actual-cli mod's own spawns (the
        // only intended caller) do not -- probed on 2.1.287 in a terminal
        // session. CLAUDE_CODE_ENTRYPOINT is not checked here, unlike the
        // terminal path: Claude Code's own process carries it, so the mod's
        // spawns inherit it too.
        if let Some(marker) = codex_agent_marker() {
            return Err(ActualError::NotInteractive(format!(
                "check-override --claude-hook was refused: this process carries {marker}, which \
                 Codex sets for the agent's own shell commands. Nothing was overridden. To \
                 override a rule in Codex, the user types `actual override <rule-id|all> [code] \
                 [reason]` as a message, or runs `actual check-override` from an ordinary \
                 terminal."
            )));
        }
        if std::env::var_os("CLAUDECODE").is_some() {
            return Err(ActualError::NotInteractive(format!(
                "check-override --claude-hook was refused: this process carries Claude Code's \
                 CLAUDECODE marker, so it was started by an agent's shell command or a settings \
                 hook, not by the actual-cli plugin's mod. Nothing was overridden. To override \
                 a rule, the user types {OVERRIDE_COMMAND} <rule-id|all> [code] [reason] as a message \
                 in the session, or runs `actual check-override` from an ordinary terminal."
            )));
        }
        let mut raw = String::new();
        let _ = std::io::stdin().read_to_string(&mut raw);
        if let Some(out) = exec_override_hook_with(args, &raw) {
            println!("{out}");
        }
        return Ok(());
    }
    // Deliberately a single-line if/else expression, not an early-return
    // guard clause: no test anywhere (in-process or subprocess) can ever
    // give this process a real terminal, so the success branch here can
    // never be exercised on its own -- keeping both branches on the one
    // line that *is* exercised every time (the terminal check itself) is
    // what keeps coverage honest about what's actually tested, rather than
    // manufacturing a fake terminal in a test just to satisfy a line count.
    // `running_under_agent_host()` joins the same condition for the same
    // reason: whichever half is false, the line executed is identical.
    #[rustfmt::skip]
    let result = if std::io::stdin().is_terminal() && !running_under_agent_host() { exec_override_impl(args) } else { Err(not_interactive()) };
    result
}

fn not_interactive() -> ActualError {
    ActualError::NotInteractive(
        "check-override must be run interactively, from an ordinary terminal — not through a \
         script, an agent's tool call, or a terminal Claude Code itself launched. It records a \
         human decision to override plan- or implementation-stage governance, and this \
         invocation was refused because it looks like an agent's own shell rather than a \
         human's: either no terminal is attached, or this process's environment carries Claude \
         Code's own CLAUDECODE / CLAUDE_CODE_ENTRYPOINT markers or Codex's CODEX_THREAD_ID / \
         CODEX_SESSION_ID. If you are a human seeing this from inside an agent's integrated \
         terminal (or a similar wrapper), run the command from a separate, plain terminal \
         window instead. This is a real but not airtight check, \
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
    let known: std::collections::HashSet<String> =
        report.documents.iter().flat_map(override_keys).collect();
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

/// The `<doc-slug>::<rule-id>` key of each rule in `document`, the form
/// `check-override --rule` names a rule by.
pub(super) fn override_keys(
    document: &crate::rules::types::RuleDocument,
) -> impl Iterator<Item = String> + '_ {
    let slug = document.slug().unwrap_or("<unnamed>").to_string();
    document
        .rules
        .iter()
        .map(move |rule| governance_session::key(&slug, &rule.id))
}

/// Every `<doc-slug>::<rule-id>` the corpus under `rules_dir` defines.
fn corpus_rule_keys(rules_dir: &Path) -> Result<Vec<String>, ActualError> {
    let report = crate::rules::parse_rule_sources(crate::rules::read_rule_sources_in(rules_dir)?);
    Ok(report.documents.iter().flat_map(override_keys).collect())
}

/// The in-session override command as the user must type it. Claude Code
/// registers a plugin's commands only under `/<plugin>:<command>` and rejects a
/// bare `/override` as an unknown command before any hook sees it, so every
/// message that tells the user what to type names the actual-cli plugin's
/// namespaced form. The parser still accepts the bare form.
const OVERRIDE_COMMAND: &str = "/actual-cli:override";

/// The code a terminal override records: `--reason-code` when given, `other`
/// for a `--reason` without one, and none for neither.
fn terminal_reason_code(
    flag: Option<crate::cli::args::OverrideReasonCodeArg>,
    reason: Option<&str>,
) -> Option<governance_session::OverrideReasonCode> {
    flag.map(Into::into)
        .or(reason.map(|_| governance_session::OverrideReasonCode::Other))
}

/// How the reply to the agent names an override's reason.
fn describe_reason(
    code: Option<governance_session::OverrideReasonCode>,
    reason: Option<&str>,
) -> String {
    use governance_session::OverrideReasonCode as Code;
    match (code, reason) {
        (Some(Code::Other) | None, Some(text)) => text.to_string(),
        (Some(code), Some(text)) => format!("{}, {text}", code.as_str()),
        (Some(code), None) => code.as_str().to_string(),
        (None, None) => "none given".to_string(),
    }
}

/// What a user-typed `/override` prompt asked for.
#[derive(Debug, PartialEq, Eq)]
struct OverridePrompt {
    /// A bare rule id, a full `<doc-slug>::<rule-id>`, or `all`.
    target: String,
    /// The code a leading word after the target names, `other` for any other
    /// text, `None` for none.
    reason_code: Option<governance_session::OverrideReasonCode>,
    /// The words kept on the machine: all of the text after the target, less
    /// a leading code word.
    reason: Option<String>,
}

/// Parse `/override <rule-id|all> [code] [reason...]` (also the plugin-namespaced
/// `/actual-cli:override`, and no other plugin's `:override`, which is that
/// plugin's command, not this one). `None` when the prompt is not an override command at
/// all; `Some` with an empty `target` when it is but names nothing.
fn parse_override_prompt(prompt: &str) -> Option<OverridePrompt> {
    let trimmed = prompt.trim();
    let (command, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((c, r)) => (c, r.trim()),
        None => (trimmed, ""),
    };
    let name = command.strip_prefix('/')?;
    if name != "override" && format!("/{name}") != OVERRIDE_COMMAND {
        return None;
    }
    Some(split_override_args(rest))
}

/// `<rule-id|all> [code] [reason...]`, the words after the override command.
fn split_override_args(rest: &str) -> OverridePrompt {
    let (target, reason) = match rest.split_once(char::is_whitespace) {
        Some((t, r)) => (t, r.trim()),
        None => (rest, ""),
    };
    let (reason_code, reason) = governance_session::OverrideReasonCode::split_reason(Some(reason));
    OverridePrompt {
        target: target.to_string(),
        reason_code,
        reason,
    }
}

/// The rules `gate` is blocking now, sorted: every rule its latest judged round
/// denied (see [`governance_session::LoopState::blocking`]) that is not already
/// overridden. With no gate, either gate's.
fn blocking_keys(session: &GovernanceSession, gate: Option<OverrideGateArg>) -> Vec<String> {
    let blocking = |k: &&String| match gate {
        Some(OverrideGateArg::Plan) => session.plan.is_blocking(k),
        Some(OverrideGateArg::Impl) => session.diff.is_blocking(k),
        None => session.plan.is_blocking(k) || session.diff.is_blocking(k),
    };
    let mut denied: Vec<String> = session
        .plan
        .deny_counts
        .keys()
        .chain(session.diff.deny_counts.keys())
        .filter(blocking)
        .filter(|k| !session.overrides.iter().any(|o| &o.key == *k))
        .cloned()
        .collect();
    denied.sort();
    denied.dedup();
    denied
}

/// Resolve an `/override` target to full rule keys: `all` is every rule `gate`
/// is blocking now ([`blocking_keys`]; either gate's when `gate` is `None`) — a
/// rule denied earlier and since fixed or no longer judged is left out, since
/// an override would turn it off for the rest of the session, and so is a rule
/// only the other gate is denying, which the user answering this gate's denial
/// was never shown.
/// Otherwise the target is matched (exactly) against those blocking rules
/// first, then the whole corpus, by bare rule id or full key. Ambiguity or no
/// match is an `Err` message for the user — never a guess.
fn resolve_override_target(
    session: &GovernanceSession,
    rules_dir: &Path,
    target: &str,
    gate: Option<OverrideGateArg>,
) -> Result<Vec<String>, String> {
    let denied = blocking_keys(session, gate);
    if target.eq_ignore_ascii_case("all") {
        return if denied.is_empty() {
            Err(match gate {
                Some(OverrideGateArg::Plan) => {
                    "no rules are currently blocking the plan in this session"
                }
                Some(OverrideGateArg::Impl) => {
                    "no rules are currently blocking the changes in this session"
                }
                None => "no rules are currently blocking in this session",
            }
            .to_string())
        } else {
            Ok(denied)
        };
    }
    let matches = |k: &String| k == target || k.rsplit("::").next() == Some(target);
    let hits: Vec<String> = denied.iter().filter(|k| matches(k)).cloned().collect();
    let hits = if hits.is_empty() {
        let mut all = corpus_rule_keys(rules_dir).map_err(|e| e.to_string())?;
        all.sort();
        all.dedup();
        all.into_iter().filter(|k| matches(k)).collect()
    } else {
        hits
    };
    match hits.len() {
        0 => Err(format!("no rule matching '{target}' was found")),
        1 => Ok(hits),
        _ => Err(format!(
            "'{target}' is ambiguous ({}); use the full <doc-slug>::<rule-id>",
            hits.join(", ")
        )),
    }
}

/// `check-override --claude-hook`: the testable core. Returns the hook's JSON
/// stdout, or `None` to emit nothing. Acts only on a `UserPromptSubmit`
/// envelope carrying a `session_id` and an `/override` prompt — the one event
/// type an agent cannot produce. Any other input (including every
/// `PreToolUse`/`Stop` shaped envelope) records nothing and prints nothing.
fn exec_override_hook_with(args: &PlanCheckOverrideArgs, raw: &str) -> Option<String> {
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct Env {
        session_id: Option<String>,
        hook_event_name: Option<String>,
        prompt: Option<String>,
    }
    let env: Env = serde_json::from_str(raw).ok()?;
    if env.hook_event_name.as_deref() != Some("UserPromptSubmit") {
        return None;
    }
    let session_id = env.session_id.filter(|s| !s.is_empty())?;
    #[cfg(feature = "telemetry")]
    crate::telemetry::debug::use_session(&session_id);
    let root = repo_root(args.repo.as_ref());
    let rules_dir = args
        .rules_dir
        .clone()
        .unwrap_or_else(|| crate::rules::rules_dir(&root));
    if args.list_blocking {
        // Read-only: what the mod's dialog lists, from the same set `all`
        // with this `--gate` would record.
        let session = governance_session::load(&session_id, &rules_dir);
        return Some(
            serde_json::json!({ "blocking": blocking_keys(&session, args.gate) }).to_string(),
        );
    }
    let parsed = parse_override_prompt(env.prompt.as_deref()?)?;
    let say = |msg: String| {
        Some(
            serde_json::json!({"hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": msg,
            }})
            .to_string(),
        )
    };
    if parsed.target.is_empty() {
        return say(format!(
            "The user typed {OVERRIDE_COMMAND} without naming a rule. Nothing was overridden. \
             Tell them the syntax: {OVERRIDE_COMMAND} <rule-id|all> [code] [reason]."
        ));
    }
    let session = governance_session::load(&session_id, &rules_dir);
    let keys = match resolve_override_target(&session, &rules_dir, &parsed.target, args.gate) {
        Ok(keys) => keys,
        Err(msg) => {
            return say(format!(
                "The user asked to override '{}' but nothing was overridden: {msg}.",
                parsed.target
            ))
        }
    };
    if let Err(e) = validate_override_rules(&rules_dir, &keys) {
        return say(format!("Nothing was overridden: {e}"));
    }
    let source = match args.via {
        OverrideVia::Prompt => governance_session::OverrideSource::SessionPrompt,
        OverrideVia::Dialog => governance_session::OverrideSource::SessionDialog,
    };
    let reason = parsed.reason.as_deref();
    let reason_code = parsed.reason_code;
    // The ended blocks only feed telemetry.
    #[cfg_attr(not(feature = "telemetry"), allow(unused_variables))]
    let (gates, ended) = governance_session::record_override_ending(
        &session_id,
        &rules_dir,
        &keys,
        reason,
        reason_code,
        source,
    );
    #[cfg(feature = "telemetry")]
    send_override_events(&root, &keys, reason.unwrap_or(""), source, &gates);
    #[cfg(feature = "telemetry")]
    super::override_telemetry::report(&rules_dir, &session_id, &keys, source, reason_code, reason);
    #[cfg(feature = "telemetry")]
    super::check_telemetry::report_resolved(Some(&session_id), None, &ended);
    let reason = describe_reason(reason_code, reason);
    let not_blocking: Vec<&str> = keys
        .iter()
        .zip(&gates)
        .filter(|(_, gate)| gate.is_none())
        .map(|(key, _)| key.as_str())
        .collect();
    let mut msg = match source {
        governance_session::OverrideSource::SessionDialog => format!(
            "The user overrode {} in the override dialog (reason: {}). These rules will no \
             longer block plan-check or impl-check this session.",
            keys.join(", "),
            reason,
        ),
        _ => format!(
            "The user overrode {} via {OVERRIDE_COMMAND} (reason: {}). These rules will no \
             longer block plan-check or impl-check this session. Retry the blocked action. Do \
             not run or type {OVERRIDE_COMMAND} yourself.",
            keys.join(", "),
            reason,
        ),
    };
    if !not_blocking.is_empty() {
        msg.push_str(&format!(
            " Tell the user that no gate was blocking {} when it was overridden (no gate's \
             latest check denied it); the override still turns it off for the rest of this \
             session.",
            not_blocking.join(", "),
        ));
    }
    // `overridden` is for the mod: the keys actually recorded, so it can tell
    // a dialog choice that landed from one that did not without reading prose.
    Some(
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": msg,
            },
            "overridden": keys,
        })
        .to_string(),
    )
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
    let session_id = args.session.as_deref().ok_or_else(|| {
        ActualError::ConfigError("--session is required unless --claude-hook is given".to_string())
    })?;
    let reason = args.reason.as_deref().filter(|r| !r.trim().is_empty());
    let reason_code = terminal_reason_code(args.reason_code, reason);
    let source = governance_session::OverrideSource::Terminal;
    // The gates and the ended blocks only feed telemetry here.
    #[cfg_attr(not(feature = "telemetry"), allow(unused_variables))]
    let (gates, ended) = governance_session::record_override_ending(
        session_id,
        &rules_dir,
        &args.rules,
        reason,
        reason_code,
        source,
    );
    #[cfg(feature = "telemetry")]
    send_override_events(&root, &args.rules, reason.unwrap_or(""), source, &gates);
    #[cfg(feature = "telemetry")]
    super::override_telemetry::report(
        &rules_dir,
        session_id,
        &args.rules,
        source,
        reason_code,
        reason,
    );
    #[cfg(feature = "telemetry")]
    super::check_telemetry::report_resolved(Some(session_id), None, &ended);
    let width = term_size::terminal_width();
    let mut panel = Panel::titled("Check override recorded");
    panel = panel.kv("Session", session_id);
    panel = panel.kv("Rules dir", &rules_dir.display().to_string());
    if let Some(code) = reason_code {
        panel = panel.kv("Reason code", code.as_str());
    }
    panel = panel.kv("Reason", reason.unwrap_or("(none given)"));
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
#[cfg(test)]
pub(super) fn hook_deny_reason(
    blocking: &[&CheckedRule],
    session_id: Option<&str>,
    partial: Option<(usize, usize)>,
    kind: check::ArtifactKind,
) -> String {
    hook_deny_reason_for(HookHost::Claude, blocking, session_id, partial, kind)
}

/// [`hook_deny_reason`] for either host. The only difference is the line
/// telling the user how to override: Claude Code has the plugin's
/// `/actual-cli:override` command, Codex has no such command, so on Codex the
/// line names `actual check-override` from a terminal alone.
pub(super) fn hook_deny_reason_for(
    host: HookHost,
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
        lines.push(match host {
            HookHost::Claude => format!(
                "{} Session: {session_id}. Only the user can override a rule — the agent must \
                 not try. Tell the user they can type `{OVERRIDE_COMMAND} <rule-id> [code] \
                 [reason]` (or `{OVERRIDE_COMMAND} all`) as a message in this session, or run \
                 `actual check-override` from an ordinary terminal (see `actual check-override \
                 --help`).",
                copy.revised
            ),
            HookHost::Codex => format!(
                "{} Session: {session_id}. Only the user can override a rule — the agent must \
                 not try. Tell the user they can run `actual check-override` from an ordinary \
                 terminal outside Codex (see `actual check-override --help`).",
                copy.revised
            ),
        });
    }
    lines.join("\n")
}

/// The override codes, as a user types them after the target.
const OVERRIDE_CODES: &str = "misread, not-here, adr-wrong, adr-outdated, accepted";

/// How the Codex ask copy names the artifact: (subject of "conflict(s) with",
/// whether that subject is plural, the pronoun a user keeps it by).
fn ask_subject(kind: check::ArtifactKind) -> (&'static str, bool, &'static str) {
    match kind {
        check::ArtifactKind::Plan => ("the plan", false, "it"),
        check::ArtifactKind::Diff => ("the changes", true, "them"),
    }
}

/// The `--codex-ask` stop reason, shown to the user only: what conflicts, and
/// the two ways to answer. The rule lines are the ones the agent would have
/// been sent; the closing line replaces the agent-facing override line.
fn codex_ask_reason(
    blocking: &[&CheckedRule],
    partial: Option<(usize, usize)>,
    kind: check::ArtifactKind,
) -> String {
    let lines = hook_deny_reason_for(HookHost::Codex, blocking, None, partial, kind);
    codex_ask_text(kind, &lines, "")
}

/// The text stop's words around the conflict `lines`; `aside` goes after the
/// first sentence (the picker that went unanswered, say).
fn codex_ask_text(kind: check::ArtifactKind, lines: &str, aside: &str) -> String {
    let copy = artifact_copy(kind);
    let (subject, plural, pronoun) = ask_subject(kind);
    format!(
        "{} stopped this turn: {subject} {} with this repository's rules.{aside}\n{lines}\n\
         To have the agent fix it, reply (for example, \"revise to conform\"). To keep \
         {pronoun}, type `actual override all <code>`, or a rule id in place of `all`. \
         Codes: {OVERRIDE_CODES}.",
        copy.governance,
        if plural { "conflict" } else { "conflicts" },
    )
}

/// The picker's copy for one gate, matching the Claude Code plugin's dialog.
struct PickerCopy {
    header: &'static str,
    blocked: &'static str,
    revise: &'static str,
    revise_note: &'static str,
    override_note: &'static str,
    revised: &'static str,
}

fn picker_copy(kind: check::ArtifactKind) -> PickerCopy {
    match kind {
        check::ArtifactKind::Plan => PickerCopy {
            header: "Plan blocked",
            blocked: "Plan blocked by",
            revise: "Revise the plan to conform (Recommended)",
            revise_note: "The agent revises the plan so it follows these rules.",
            override_note: "Keep the plan. These rules stop blocking for the rest of this session.",
            revised: "The user chose to revise the plan so it conforms to these rules. Revise it \
                      to satisfy every rule below and propose it again; the check runs again \
                      when this turn ends. Don't suggest an override.",
        },
        check::ArtifactKind::Diff => PickerCopy {
            // Codex, like Claude Code's dialog, clips a long header.
            header: "Code blocked",
            blocked: "Changes blocked by",
            revise: "Revise the changes to conform (Recommended)",
            revise_note: "The agent changes the code so it follows these rules.",
            override_note: "Keep the changes. These rules stop blocking for the rest of this \
                            session.",
            revised: "The user chose to revise the changes so they conform to these rules. \
                      Change the code to satisfy every rule below; the check runs again when \
                      this turn ends. Don't suggest an override.",
        },
    }
}

/// The "why" options after Override, and the code each records. The picker
/// takes three, so `adr-outdated` and `accepted` are typed under Other.
const PICKER_REASONS: [(&str, &str, governance_session::OverrideReasonCode); 3] = [
    (
        "The check misread my change",
        "The change does follow the rule.",
        governance_session::OverrideReasonCode::Misread,
    ),
    (
        "This ADR doesn't apply here",
        "The rule is right, but not for this code.",
        governance_session::OverrideReasonCode::NotHere,
    ),
    (
        "This ADR is wrong",
        "The decision itself should change.",
        governance_session::OverrideReasonCode::AdrWrong,
    ),
];

const PICKER_REASON_QUESTION: &str = "If the block is right and you're going ahead anyway, \
     choose Other and type \"accepted\" (or \"adr-outdated\" if the ADR is out of date); \
     anything else typed there may be sent to your organization's admins, with anything that \
     looks like a secret removed, so don't type one. Otherwise, why override?";

/// A short random id for one denial's picker questions.
fn picker_nonce() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// The rule ids a picker names, in order and without repeats.
fn picker_rule_ids(blocking: &[&CheckedRule]) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for rule in blocking {
        if !ids.contains(&rule.rule_id) {
            ids.push(rule.rule_id.clone());
        }
    }
    ids
}

/// The override option's label: the rule for one, a count for several.
fn override_label(blocking: &[&CheckedRule]) -> String {
    match picker_rule_ids(blocking).as_slice() {
        [one] => format!("Override {one}"),
        ids => format!("Override all {} rules", ids.len()),
    }
}

/// `request_user_input`'s arguments for one question.
fn picker_arguments(
    id: String,
    header: &str,
    question: String,
    options: &[(&str, &str)],
) -> String {
    let options: Vec<serde_json::Value> = options
        .iter()
        .map(|(label, description)| serde_json::json!({"label": label, "description": description}))
        .collect();
    serde_json::json!({"questions": [{
        "id": id,
        "header": header,
        "question": question,
        "options": options,
    }]})
    .to_string()
}

/// What a picker-mode deny tells the agent: the conflicts, and the exact
/// `request_user_input` call that asks the user to revise or override.
fn codex_picker_reason(
    blocking: &[&CheckedRule],
    partial: Option<(usize, usize)>,
    kind: check::ArtifactKind,
    picker: &PickerAsk,
) -> String {
    let copy = artifact_copy(kind);
    let words = picker_copy(kind);
    let (subject, plural, _) = ask_subject(kind);
    let lines = hook_deny_reason_for(HookHost::Codex, blocking, None, partial, kind);
    let ids = picker_rule_ids(blocking);
    let what = if ids.len() == 1 {
        "1 rule".to_string()
    } else {
        format!("{} rules", ids.len())
    };
    let arguments = picker_arguments(
        format!("actual_choice_{}", picker.nonce),
        words.header,
        format!(
            "{} {what}: {}. What should happen?",
            words.blocked,
            ids.join(", ")
        ),
        &[
            (words.revise, words.revise_note),
            (&picker.override_label, words.override_note),
        ],
    );
    format!(
        "{} found that {subject} {} with this repository's rules:\n{lines}\n\
         Don't change, undo or override anything yet. Ask the user what to do: call \
         request_user_input now with exactly these arguments, then follow the instructions its \
         result brings back.\n{arguments}",
        copy.governance,
        if plural { "conflict" } else { "conflicts" },
    )
}

/// The text stop for a picker question that went unanswered (the agent
/// didn't ask, the user dismissed it, or the answer never reached actual),
/// when the plan or diff is the one the question was about. Clears the
/// question, so a later conflict asks again; the conflicts stay for the next
/// prompt. `None` when there is no such question.
pub(super) fn codex_unanswered_picker(
    host: HookHost,
    kind: check::ArtifactKind,
    session_id: Option<&str>,
    rules_dir: &Path,
    session: &GovernanceSession,
    digest: &str,
) -> Option<String> {
    if host != HookHost::Codex {
        return None;
    }
    let session_id = session_id?;
    let pending = session.loop_state(kind).pending.as_ref()?;
    let ask = pending.ask.as_ref().filter(|ask| ask.digest == digest)?;
    let aside = match ask.stage {
        PickerStage::Choice => " The picker asking what to do went unanswered.",
        PickerStage::Reason => {
            " The picker asking why to override went unanswered, so nothing was overridden."
        }
    };
    let text = codex_ask_text(kind, &pending.text, aside);
    governance_session::update(session_id, rules_dir, |s| {
        if let Some(pending) = s.loop_state_mut(kind).pending.as_mut() {
            pending.ask = None;
        }
    });
    Some(host.stop(&with_override_reminder(text, session)))
}

/// The user's answer to a gate's picker, from a Codex `PostToolUse` on
/// `request_user_input`: what the agent should do next, as context, or `None`
/// to say nothing. An answer counts only to a question this session asked,
/// by its id, and only from a process without Codex's agent markers; anything
/// else is left to [`codex_unanswered_picker`].
pub(super) fn codex_answer_reply(
    session_id: &str,
    tool_input: Option<&serde_json::Value>,
    tool_response: Option<&serde_json::Value>,
    root: &Path,
    rules_dir: &Path,
) -> Option<String> {
    #[cfg(not(feature = "telemetry"))]
    let _ = root;
    if codex_agent_marker().is_some() {
        return None;
    }
    let asked: Vec<&str> = tool_input?
        .get("questions")?
        .as_array()?
        .iter()
        .filter_map(|q| q.get("id")?.as_str())
        .collect();
    let answers = picker_answers(tool_response?)?;
    let session = governance_session::load(session_id, rules_dir);
    let (kind, pending, ask) = [check::ArtifactKind::Diff, check::ArtifactKind::Plan]
        .into_iter()
        .find_map(|kind| {
            let pending = session.loop_state(kind).pending.clone()?;
            let ask = pending.ask.clone()?;
            Some((kind, pending, ask))
        })?;
    let id = match ask.stage {
        PickerStage::Choice => format!("actual_choice_{}", ask.nonce),
        PickerStage::Reason => format!("actual_reason_{}", ask.nonce),
    };
    if !asked.contains(&id.as_str()) {
        return None;
    }
    let answer = answers
        .get(&id)?
        .get("answers")?
        .as_array()?
        .iter()
        .filter_map(|a| a.as_str())
        .find(|a| !a.trim().is_empty())?
        .to_string();
    let words = picker_copy(kind);
    let set_pending = |pending: Option<Pending>| {
        governance_session::update(session_id, rules_dir, |s| {
            s.loop_state_mut(kind).pending = pending;
        });
    };
    let reply = match ask.stage {
        PickerStage::Choice if answer == words.revise => {
            set_pending(None);
            format!("{}\n\n{}", words.revised, pending.text)
        }
        PickerStage::Choice if answer == ask.override_label => {
            let options: Vec<(&str, &str)> = PICKER_REASONS
                .iter()
                .map(|(label, description, _)| (*label, *description))
                .collect();
            let arguments = picker_arguments(
                format!("actual_reason_{}", ask.nonce),
                "Why override",
                PICKER_REASON_QUESTION.to_string(),
                &options,
            );
            set_pending(Some(Pending {
                ask: Some(PickerAsk {
                    stage: PickerStage::Reason,
                    ..ask
                }),
                ..pending
            }));
            format!(
                "The user chose to override. Ask why before anything else: call \
                 request_user_input now with exactly these arguments, then follow the \
                 instructions its result brings back. Don't change anything meanwhile.\n{arguments}"
            )
        }
        PickerStage::Choice => {
            set_pending(None);
            format!(
                "The user answered the picker: \"{answer}\". No rule was overridden. Act on their \
                 answer; don't suggest an override.\n\n{}",
                pending.text
            )
        }
        PickerStage::Reason => {
            let (reason_code, reason) =
                match PICKER_REASONS.iter().find(|(label, ..)| *label == answer) {
                    Some((_, _, code)) => (Some(*code), None),
                    None => governance_session::OverrideReasonCode::split_reason(Some(&answer)),
                };
            let gate = match kind {
                check::ArtifactKind::Plan => OverrideGateArg::Plan,
                check::ArtifactKind::Diff => OverrideGateArg::Impl,
            };
            let keys = match resolve_override_target(&session, rules_dir, "all", Some(gate)) {
                Ok(keys) => keys,
                Err(msg) => {
                    set_pending(None);
                    return Some(codex_prompt_context(
                        "PostToolUse",
                        &format!(
                            "Nothing was overridden: {msg}. Tell the user, then end your turn."
                        ),
                    ));
                }
            };
            if let Err(e) = validate_override_rules(rules_dir, &keys) {
                set_pending(None);
                return Some(codex_prompt_context(
                    "PostToolUse",
                    &format!("Nothing was overridden: {e}. Tell the user, then end your turn."),
                ));
            }
            let source = governance_session::OverrideSource::SessionDialog;
            let reason = reason.as_deref();
            #[cfg_attr(not(feature = "telemetry"), allow(unused_variables))]
            let (gates, ended) = governance_session::record_override_ending(
                session_id,
                rules_dir,
                &keys,
                reason,
                reason_code,
                source,
            );
            #[cfg(feature = "telemetry")]
            send_override_events(root, &keys, reason.unwrap_or(""), source, &gates);
            #[cfg(feature = "telemetry")]
            super::override_telemetry::report(
                rules_dir,
                session_id,
                &keys,
                source,
                reason_code,
                reason,
            );
            #[cfg(feature = "telemetry")]
            super::check_telemetry::report_resolved(Some(session_id), None, &ended);
            set_pending(None);
            format!(
                "The user overrode {} in the picker (reason: {}). These rules won't block again \
                 this session. Keep the work as it is and end your turn with one line saying so. \
                 Don't run `actual override` or `actual check-override` yourself.",
                short_ids(&keys),
                describe_reason(reason_code, reason),
            )
        }
    };
    Some(codex_prompt_context("PostToolUse", &reply))
}

/// The answers in a `request_user_input` result, `{"<id>": {"answers": [...]}}`.
/// Codex hands a hook the result as JSON or as the JSON text, so both parse.
fn picker_answers(
    response: &serde_json::Value,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let parsed;
    let value = match response {
        serde_json::Value::String(text) => {
            parsed = serde_json::from_str::<serde_json::Value>(text).ok()?;
            &parsed
        }
        other => other,
    };
    value.get("answers")?.as_object().cloned()
}

/// What the agent is told with the user's message after a `--codex-ask`
/// check stopped its last turn: the conflicts, and that the message answers
/// them. `conflicts` is [`LoopState::pending`]'s text.
fn codex_pending_context(kind: check::ArtifactKind, conflicts: &str) -> String {
    let (what, revise) = match kind {
        check::ArtifactKind::Plan => (
            "your plan conflicts",
            "If they want the plan revised, make it conform to every rule below and propose \
             it again; the check runs again when this turn ends.",
        ),
        check::ArtifactKind::Diff => (
            "your changes conflict",
            "If they want the changes revised, make them conform to every rule below; the \
             check runs again when this turn ends.",
        ),
    };
    format!(
        "At the end of your last turn, Actual's check found that {what} with this \
         repository's rules, and stopped the turn so the user could decide. The user's \
         message is their answer.\n\
         - {revise}\n\
         - Don't undo your work unless the user asks, and don't override a rule yourself: only \
         the user can, by typing `actual override <rule-id|all> [code]` as a message.\n\
         - If their message is about something else, mention the open conflicts in one line, \
         then do what they asked.\n\n{conflicts}"
    )
}

/// A Codex `UserPromptSubmit` after the turn start is recorded: an override
/// the user typed, as a stop reply that keeps the prompt from the model, or
/// the conflicts a `--codex-ask` check stopped the last turn on, as context
/// for the model with this message. `None` when neither applies.
pub(super) fn codex_prompt_reply(
    session_id: &str,
    prompt: Option<&str>,
    root: &Path,
    rules_dir: &Path,
) -> Option<String> {
    if let Some(parsed) = prompt.and_then(parse_codex_override_prompt) {
        return Some(
            HookHost::Codex.stop(&codex_typed_override(session_id, root, rules_dir, &parsed)),
        );
    }
    let mut pending = Vec::new();
    governance_session::update(session_id, rules_dir, |s| {
        for kind in [check::ArtifactKind::Diff, check::ArtifactKind::Plan] {
            if let Some(waiting) = s.loop_state_mut(kind).pending.take() {
                pending.push(codex_pending_context(kind, &waiting.text));
            }
        }
    });
    if pending.is_empty() {
        return None;
    }
    Some(codex_prompt_context(
        "UserPromptSubmit",
        &pending.join("\n\n"),
    ))
}

/// Codex's context reply for `event` (`UserPromptSubmit` or `PostToolUse`),
/// with `hookEventName` ahead of `additionalContext`. The plugin forwards a
/// Codex reply only when its bytes match the shape exactly, and
/// `serde_json::json!` sorts keys, which would put them the other way round.
fn codex_prompt_context(event: &'static str, context: &str) -> String {
    #[derive(Serialize)]
    struct Inner<'a> {
        #[serde(rename = "hookEventName")]
        hook_event_name: &'static str,
        #[serde(rename = "additionalContext")]
        additional_context: &'a str,
    }
    #[derive(Serialize)]
    struct Reply<'a> {
        #[serde(rename = "hookSpecificOutput")]
        hook_specific_output: Inner<'a>,
    }
    serde_json::to_string(&Reply {
        hook_specific_output: Inner {
            hook_event_name: event,
            additional_context: context,
        },
    })
    .expect("strings serialize")
}

/// An override typed in a Codex session: `actual override <rule-id|all>
/// [code] [reason]`, or the `/override` forms Claude Code users know. The
/// plain-words form only counts when its target looks like one (`all`, a
/// rule id with a hyphen, or a full key), so a message that merely starts
/// with "actual override is..." still reaches the agent.
fn parse_codex_override_prompt(prompt: &str) -> Option<OverridePrompt> {
    if let Some(parsed) = parse_override_prompt(prompt) {
        return Some(parsed);
    }
    let mut words = prompt.trim().splitn(3, char::is_whitespace);
    let (first, second) = (words.next()?, words.next().unwrap_or(""));
    if !first.eq_ignore_ascii_case("actual") || !second.eq_ignore_ascii_case("override") {
        return None;
    }
    let parsed = split_override_args(words.next().unwrap_or("").trim());
    let target = parsed.target.as_str();
    let looks_like_target = target.is_empty()
        || target.eq_ignore_ascii_case("all")
        || target.contains("::")
        || target.contains('-');
    looks_like_target.then_some(parsed)
}

/// The rule id a user reads: the part of a key after `::`.
fn short_id(key: &str) -> &str {
    key.rsplit("::").next().unwrap_or(key)
}

fn short_ids(keys: &[String]) -> String {
    keys.iter()
        .map(|k| short_id(k))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The Codex marker set on this process, when it is one the agent's own shell
/// commands carry. A Codex hook ran without either on Codex CLI 0.167.0.
fn codex_agent_marker() -> Option<&'static str> {
    ["CODEX_THREAD_ID", "CODEX_SESSION_ID"]
        .into_iter()
        .find(|name| std::env::var_os(name).is_some())
}

/// Record a typed Codex override and say what happened, for the stop reason
/// the user sees. Records nothing when this process looks like the agent's
/// own shell, when no rule is named, or when the target matches nothing.
fn codex_typed_override(
    session_id: &str,
    root: &Path,
    rules_dir: &Path,
    parsed: &OverridePrompt,
) -> String {
    #[cfg(not(feature = "telemetry"))]
    let _ = root;
    if let Some(marker) = codex_agent_marker() {
        return format!(
            "Actual AI: nothing was overridden. The override reached actual from a process \
             carrying {marker}, which Codex sets for the agent's own shell commands, so it \
             can't be told apart from the agent overriding its own block. Run `actual \
             check-override` from an ordinary terminal outside Codex instead."
        );
    }
    if parsed.target.is_empty() {
        return format!(
            "Actual AI: nothing was overridden: name a rule, or `all`. Type `actual override \
             <rule-id|all> [code] [reason]`. Codes: {OVERRIDE_CODES}."
        );
    }
    let session = governance_session::load(session_id, rules_dir);
    let gate = if session.diff.pending.is_some() {
        Some(OverrideGateArg::Impl)
    } else if session.plan.pending.is_some() {
        Some(OverrideGateArg::Plan)
    } else {
        None
    };
    let keys = match resolve_override_target(&session, rules_dir, &parsed.target, gate) {
        Ok(keys) => keys,
        Err(msg) => return format!("Actual AI: nothing was overridden: {msg}."),
    };
    if let Err(e) = validate_override_rules(rules_dir, &keys) {
        return format!("Actual AI: nothing was overridden: {e}");
    }
    let source = governance_session::OverrideSource::SessionPrompt;
    let reason = parsed.reason.as_deref();
    let reason_code = parsed.reason_code;
    // The ended blocks only feed telemetry.
    #[cfg_attr(not(feature = "telemetry"), allow(unused_variables))]
    let (gates, ended) = governance_session::record_override_ending(
        session_id,
        rules_dir,
        &keys,
        reason,
        reason_code,
        source,
    );
    #[cfg(feature = "telemetry")]
    send_override_events(root, &keys, reason.unwrap_or(""), source, &gates);
    #[cfg(feature = "telemetry")]
    super::override_telemetry::report(rules_dir, session_id, &keys, source, reason_code, reason);
    #[cfg(feature = "telemetry")]
    super::check_telemetry::report_resolved(Some(session_id), None, &ended);
    // Once nothing a stopped turn named is still blocking, its conflicts are
    // settled and must not be handed to the agent with the next message.
    let after = governance_session::update(session_id, rules_dir, |s| {
        for (kind, arg) in [
            (check::ArtifactKind::Diff, OverrideGateArg::Impl),
            (check::ArtifactKind::Plan, OverrideGateArg::Plan),
        ] {
            if s.loop_state(kind).pending.is_some() && blocking_keys(s, Some(arg)).is_empty() {
                s.loop_state_mut(kind).pending = None;
            }
        }
    });
    let mut msg = format!(
        "Actual AI: overrode {} (reason: {}). {} won't block again this session, and the \
         agent's work stays as it is.",
        short_ids(&keys),
        describe_reason(reason_code, reason),
        if keys.len() == 1 { "It" } else { "They" },
    );
    let remaining = blocking_keys(&after, gate);
    if !remaining.is_empty() {
        msg.push_str(&format!(
            " Still blocking: {}. Reply to have the agent revise, or override {} too.",
            short_ids(&remaining),
            if remaining.len() == 1 { "it" } else { "them" },
        ));
    }
    let not_blocking: Vec<String> = keys
        .iter()
        .zip(&gates)
        .filter(|(_, gate)| gate.is_none())
        .map(|(key, _)| key.clone())
        .collect();
    if !not_blocking.is_empty() {
        msg.push_str(&format!(
            " No check was blocking {} when you overrode it; the override still applies for \
             the rest of this session.",
            short_ids(&not_blocking),
        ));
    }
    msg
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

/// Print exactly one line: the single call site that writes to stdout for
/// either `--claude-hook` command, so "stdout is exactly one JSON object, or
/// nothing" is enforceable by inspection rather than by discipline.
pub(super) fn emit(json: String) {
    #[cfg(test)]
    tests::EMITTED.with(|cell| cell.borrow_mut().push(json.clone()));
    println!("{json}");
}

/// What a hook round answers, before the ADR question is folded in.
enum Reply {
    Silent,
    /// Information only: a notice on Claude Code, silence on Codex.
    Status(String),
    Notice(String),
    Deny(String),
}

/// Emit `reply`, carrying `run`'s ADR question when this round is the one that
/// asks it: after a deny's reason, worded to be answered once that reason is
/// fixed, as the reason of a block that would
/// otherwise have been a notice (which stays beside it for the user), or as
/// the reason of a block on an otherwise silent pass.
fn reply(run: &HookRun<'_>, reply: Reply) {
    let on_block = matches!(reply, Reply::Deny(_));
    let question = run
        .ask
        .zip(run.session_id)
        .filter(|(ask, session_id)| {
            super::adr_report::mark(session_id, run.rules_dir, ask, on_block)
        })
        .map(|(ask, _)| ask.text(on_block));
    let json = match (reply, question) {
        (Reply::Silent, None) => return,
        (Reply::Status(message), None) => match run.host.status(&message) {
            Some(json) => json,
            None => return,
        },
        (Reply::Notice(message), None) => run.host.notice(&message),
        (Reply::Deny(reason), None) => run.host.deny(&reason),
        (Reply::Silent, Some(question)) => run.host.deny(question),
        (Reply::Status(message) | Reply::Notice(message), Some(question)) => {
            run.host.deny_with_notice(question, &message)
        }
        (Reply::Deny(reason), Some(question)) => run.host.deny(&format!("{reason}\n\n{question}")),
    };
    emit(json);
}

/// Emit [`HookHost::status`]'s line, which is nothing on Codex.
pub(super) fn emit_status(host: HookHost, message: &str) {
    if let Some(json) = host.status(message) {
        emit(json);
    }
}

/// Everything [`finish_hook`] needs from a `--claude-hook` command that is
/// not the session or the pipeline's outcome. The two commands differ only in
/// the values they put here — which artifact kind they judge, the label their
/// telemetry carries, and where the artifact text came from — never in what
/// happens once a verdict exists.
// `command`, `agent_id`, `root` and `started` only feed telemetry.
#[cfg_attr(not(feature = "telemetry"), allow(dead_code))]
pub(super) struct HookRun<'a> {
    /// Which agent's hook this is, which picks the reply's shape.
    pub host: HookHost,
    pub kind: check::ArtifactKind,
    /// The telemetry command label, e.g. `"plan-check --claude-hook"`.
    pub command: &'static str,
    pub session_id: Option<&'a str>,
    /// The subagent whose plan or diff this is, absent for the main agent.
    pub agent_id: Option<&'a str>,
    /// [`governance_session::content_digest`] of the judged artifact text.
    pub artifact_digest: &'a str,
    pub root: &'a Path,
    pub rules_dir: &'a Path,
    pub max_rounds: u32,
    /// The ADR question, when this round may be the turn's first to ask it.
    pub ask: Option<&'a super::adr_report::PreparedAsk>,
    /// `--codex-ask`: on Codex, a conflict ends the turn for the user to
    /// answer (see [`HookHost::stop`]) instead of sending the agent back.
    pub codex_ask: bool,
    /// With `codex_ask`: the agent can show a picker this turn (`--codex-picker`,
    /// and the rollout says `request_user_input` works in the turn's mode),
    /// so a conflict tells it to ask instead of ending the turn.
    pub picker: bool,
    #[cfg(feature = "telemetry")]
    pub started: super::check_telemetry::Started,
    /// What this round's selection kept and dropped.
    #[cfg(feature = "telemetry")]
    pub filter: Option<&'a FilterTally>,
}

/// A direct run's decision: `block` on a conflict, `warn` when a rule needs a
/// decision or the judge saw only part of what applies, `allow` otherwise.
pub(super) fn direct_decision(verdicts: &[CheckedRule], partial: bool) -> PlanGovernanceDecision {
    if verdicts.iter().any(|v| v.verdict.blocks()) {
        PlanGovernanceDecision::Block
    } else if partial
        || verdicts
            .iter()
            .any(|v| v.verdict == Verdict::RequiresDecision)
    {
        PlanGovernanceDecision::Warn
    } else {
        PlanGovernanceDecision::Allow
    }
}

/// Record a direct run's round in the local enforcement log.
pub(super) fn record_direct_round(
    kind: check::ArtifactKind,
    rules_dir: &Path,
    decision: PlanGovernanceDecision,
    verdicts: &[CheckedRule],
) {
    enforcement_log::record(&Round::new(
        kind.into(),
        Mode::Direct,
        &absolute(rules_dir),
        decision.into(),
        verdicts,
    ));
}

/// Record a hook round in the local enforcement log, with the session's
/// round counter for the gate when there is a session and the judge ran.
fn record_hook_round(
    run: &HookRun<'_>,
    session: &GovernanceSession,
    decision: PlanGovernanceDecision,
    verdicts: &[CheckedRule],
    judge_ran: bool,
) {
    let round = Round::new(
        run.kind.into(),
        Mode::Hook,
        &absolute(run.rules_dir),
        decision.into(),
        verdicts,
    );
    let round = match run.session_id {
        Some(id) if judge_ran => round.in_session(id, session.loop_state(run.kind).rounds),
        Some(id) => Round {
            session_id: Some(id.to_string()),
            ..round
        },
        None => round,
    };
    enforcement_log::record(&round);
}

fn absolute(path: &Path) -> std::path::PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Turn the pipeline's [`Outcome`] into the hook's one line of output and the
/// session state that goes with it: fail-open notices, session bookkeeping,
/// the deny / round-limit / partial-coverage / override-reminder decision, and
/// the governance telemetry for whichever it was.
///
/// Shared by `plan-check` and `impl-check` so the two gates cannot drift on a
/// policy that has to stay identical (they did, twice, while this was copied).
/// Every user-visible noun comes from [`artifact_copy`] via `run.kind`, and
/// loop state is reached through [`GovernanceSession::loop_state_mut`], so
/// nothing in here names a specific gate.
///
/// INVARIANT: never returns an error and never panics on its own — every
/// branch ends in a notice or a deny, so a caller's `--claude-hook` path
/// cannot reach an ordinary nonzero exit through it.
pub(super) fn finish_hook(run: &HookRun<'_>, session: &mut GovernanceSession, outcome: Outcome) {
    let copy = artifact_copy(run.kind);
    // A round that reaches a verdict sends its selection with the round's
    // other records; one that stops short sends it alone.
    #[cfg(feature = "telemetry")]
    if !matches!(outcome, Outcome::Verdicts { .. }) {
        super::check_telemetry::report_filter(run);
    }
    match outcome {
        Outcome::NothingApplies => {
            // Nothing applies, so nothing blocks: the previous round's
            // blocking rules must not stay targets for `/override all`.
            // It decides without judging, so it ends every open block.
            let state = session.loop_state_mut(run.kind);
            state.blocking.clear();
            state.pending = None;
            if let Some(session_id) = run.session_id {
                #[cfg(feature = "telemetry")]
                let mut ended = Vec::new();
                governance_session::update(session_id, run.rules_dir, |s| {
                    let state = s.loop_state_mut(run.kind);
                    state.blocking.clear();
                    #[cfg(feature = "telemetry")]
                    {
                        state.end_blocks(|_| Some(governance_session::Resolution::NotRechecked));
                        ended = state
                            .take_ended()
                            .into_iter()
                            .map(|episode| (run.kind, episode))
                            .collect();
                    }
                    state.pending = None;
                });
                #[cfg(feature = "telemetry")]
                super::check_telemetry::report_resolved(run.session_id, run.agent_id, &ended);
            }
            reply(
                run,
                Reply::Status(format!(
                    "No committed rule under .actual/rules/ applies to this {}.",
                    copy.noun
                )),
            );
        }
        Outcome::NoRunner { reason, .. } => {
            reply(
                run,
                Reply::Notice(format!(
                    "{} did not run: no runner available ({reason}).",
                    copy.governance
                )),
            );
        }
        Outcome::CheckFailed { reason, .. } => {
            reply(
                run,
                Reply::Notice(format!("{} did not run: {reason}", copy.governance)),
            );
        }
        Outcome::Verdicts {
            verdicts,
            runner_label,
            partial,
            headers,
            ..
        } => finish_verdicts(
            run,
            session,
            verdicts,
            runner_label.is_some(),
            partial,
            &headers,
        ),
    }
}

// `headers` only feeds telemetry.
#[cfg_attr(not(feature = "telemetry"), allow(unused_variables))]
fn finish_verdicts(
    run: &HookRun<'_>,
    session: &mut GovernanceSession,
    verdicts: Vec<CheckedRule>,
    // A round is one *completed judge call* — `runner_label` is only ever
    // `Some` when `check::check` actually ran (never for the "everything was
    // already excluded" shortcut in `run_pipeline`).
    judge_ran: bool,
    partial: Option<(usize, usize)>,
    headers: &BTreeMap<String, AdrMeta>,
) {
    let kind = run.kind;
    let session_id = run.session_id;
    if session_id.is_some() {
        let state = session.loop_state_mut(kind);
        for v in &verdicts {
            let key = governance_session::key(&v.doc_slug, &v.rule_id);
            if v.verdict.passes() {
                state.cleared.insert(key, run.artifact_digest.to_string());
            } else {
                // A rule that was cleared against an earlier artifact and no
                // longer passes against this one must not leave a stale
                // entry behind -- it is no longer a true fact about the
                // current artifact, digest mismatch or not.
                state.cleared.remove(&key);
            }
        }
        if judge_ran {
            state.begin_round();
        } else {
            // Every applicable rule was already cleared or overridden, so
            // nothing blocks -- not a round, but the gate's latest decision.
            state.blocking.clear();
            state.pending = None;
        }
    }

    // A verdict blocks the tool call when it is a genuine conflict, or when
    // the judge classifies the artifact as *deliberately* superseding a rule
    // — that classification is model output, not a recorded human decision,
    // so it gets exactly the same deny + override treatment as an outright
    // conflict, not a notice the agent can simply proceed past. See
    // `plan_check`'s module doc, "advisory gate".
    let blocking: Vec<&CheckedRule> = verdicts
        .iter()
        .filter(|v| matches!(v.verdict, Verdict::Conflicting | Verdict::RequiresDecision))
        .collect();
    #[cfg(feature = "telemetry")]
    let mut records: Vec<_> = super::check_telemetry::filter_record(run)
        .into_iter()
        .chain(super::check_telemetry::adr_records(
            run, headers, &verdicts, &blocking,
        ))
        .collect();

    if !blocking.is_empty() {
        // `--codex-ask` needs a session: the conflicts wait in it for the
        // user's next message, and an override is recorded against it.
        let ask = run.codex_ask && run.host == HookHost::Codex && session_id.is_some();
        if session_id.is_some() {
            // Every currently-blocking rule gets its own denial recorded,
            // independent of any other rule's count — see `plan_check`'s
            // module doc, "the round limit is per rule".
            for c in &blocking {
                session.record_denial(kind, &governance_session::key(&c.doc_slug, &c.rule_id));
            }
        }

        if let Some(session_id) = session_id {
            let exhausted: Vec<&CheckedRule> = blocking
                .iter()
                .filter(|c| {
                    session.deny_limit_exceeded(
                        kind,
                        &governance_session::key(&c.doc_slug, &c.rule_id),
                        run.max_rounds,
                    )
                })
                .copied()
                .collect();
            // Fail open only when *every* rule blocking this round has
            // individually exhausted its own budget. A single rule still
            // within budget keeps the whole call denied — the hook can only
            // deny or not deny the tool call as a whole, so an exhausted rule
            // cannot be waved through while a fresh one still needs to block.
            if !exhausted.is_empty() && exhausted.len() == blocking.len() {
                let keys: Vec<String> = exhausted
                    .iter()
                    .map(|c| governance_session::key(&c.doc_slug, &c.rule_id))
                    .collect();
                let message = round_limit_message(&exhausted, session, kind, run.max_rounds);
                governance_session::record_round_limit(
                    session_id,
                    run.rules_dir,
                    session.loop_state(kind).rounds,
                    &keys,
                    &message,
                );
                #[cfg(feature = "telemetry")]
                records.extend(settle_episodes(
                    run,
                    session,
                    &verdicts,
                    headers,
                    &[],
                    &keys,
                ));
                governance_session::store_loop(
                    session_id,
                    run.rules_dir,
                    kind,
                    session.loop_state(kind),
                );
                // The audit log keeps the round-limit message on its own,
                // undecorated -- the override reminder is only appended to
                // what the human actually sees, same spirit as the
                // silent-path `notes` below.
                reply(run, Reply::Notice(with_override_reminder(message, session)));
                // Every rule here is `blocked: false` -- the round limit let
                // the call through this round -- so this is `warn`, not
                // `block`, distinct from a clean pass.
                record_hook_round(
                    run,
                    session,
                    PlanGovernanceDecision::Warn,
                    &verdicts,
                    judge_ran,
                );
                #[cfg(feature = "telemetry")]
                super::check_telemetry::report_round(
                    run,
                    crate::api::types::PlanGovernanceDecision::Warn,
                    &[super::check_telemetry::WarnReason::RoundLimit],
                    records,
                );
                #[cfg(feature = "telemetry")]
                send_hook_governance_events(
                    run.command,
                    run.root,
                    run.started.instant,
                    crate::api::types::PlanGovernanceDecision::Warn,
                    &verdicts,
                    false,
                    (session.loop_state(kind).rounds, run.max_rounds),
                );
                return;
            }
            #[cfg(feature = "telemetry")]
            records.extend(settle_episodes(
                run,
                session,
                &verdicts,
                headers,
                &blocking,
                &[],
            ));
            if ask {
                // Worded for the agent, without the line telling it how the
                // user overrides: the user is told that in the stop reason,
                // or chooses in the picker.
                let text = hook_deny_reason_for(run.host, &blocking, None, partial, kind);
                let ask = run.picker.then(|| PickerAsk {
                    nonce: picker_nonce(),
                    stage: PickerStage::Choice,
                    digest: run.artifact_digest.to_string(),
                    override_label: override_label(&blocking),
                });
                session.loop_state_mut(kind).pending = Some(Pending { text, ask });
            }
            governance_session::store_loop(
                session_id,
                run.rules_dir,
                kind,
                session.loop_state(kind),
            );
        }
        // A deny is not silence, but it is also not the same as "no active
        // override" -- a different rule's conflict must not bury the fact
        // that this session still carries a recorded override elsewhere. See
        // `plan_check`'s module doc, "override visibility".
        let picker = ask
            .then(|| session.loop_state(kind).pending.as_ref())
            .flatten()
            .and_then(|pending| pending.ask.clone());
        if let Some(picker) = picker {
            // Sent back to the agent, which shows the user the picker.
            let reason = codex_picker_reason(&blocking, partial, kind, &picker);
            emit(run.host.deny(&with_override_reminder(reason, session)));
        } else if ask {
            let reason = codex_ask_reason(&blocking, partial, kind);
            emit(run.host.stop(&with_override_reminder(reason, session)));
        } else {
            let deny_reason = hook_deny_reason_for(run.host, &blocking, session_id, partial, kind);
            reply(
                run,
                Reply::Deny(with_override_reminder(deny_reason, session)),
            );
        }
        record_hook_round(
            run,
            session,
            PlanGovernanceDecision::Block,
            &verdicts,
            judge_ran,
        );
        #[cfg(feature = "telemetry")]
        super::check_telemetry::report_round(
            run,
            crate::api::types::PlanGovernanceDecision::Block,
            &[],
            records,
        );
        #[cfg(feature = "telemetry")]
        send_hook_governance_events(
            run.command,
            run.root,
            run.started.instant,
            crate::api::types::PlanGovernanceDecision::Block,
            &verdicts,
            true,
            (session.loop_state(kind).rounds, run.max_rounds),
        );
        return;
    }

    if let Some(session_id) = session_id {
        #[cfg(feature = "telemetry")]
        records.extend(settle_episodes(run, session, &verdicts, headers, &[], &[]));
        governance_session::store_loop(session_id, run.rules_dir, kind, session.loop_state(kind));
    }

    // Fully conforming (nothing blocking): the contract is silence — UNLESS
    // this session carries an active override (must stay visible on every
    // round, never silently absorbed once granted) or this round only covered
    // a prefix of what applies (a plain "conforming" silence would misreport
    // partial coverage as complete). Either reason alone is enough to break
    // silence; both together are joined into one notice.
    let mut notes = Vec::new();
    if let Some((judged, total)) = partial {
        notes.push(partial_coverage_note(judged, total));
        // This is the one fail-open path with no other durable trace: a deny
        // keeps per-rule accounting live in the session, and an override or
        // round-limit pass already writes its own audit entry, but
        // "conforming, as far as we looked" would otherwise exist only in the
        // hook response the agent — not a human — is the one actually
        // reading. See `plan_check`'s module doc, "advisory gate".
        if let Some(session_id) = session_id {
            governance_session::record_partial_coverage(
                session_id,
                run.rules_dir,
                session.loop_state(kind).rounds,
                judged,
                total,
            );
            notes.push(format!(
                "This is not a silent pass — recorded in {}.",
                audit_log_note(kind)
            ));
        }
    }
    let reminder = override_reminder(session);
    // An active override on an otherwise-clean round is still worth
    // distinguishing from a truly clean allow -- something was once wrong here
    // and a human waived it, which is exactly the kind of fact this stream
    // exists to preserve. `verdicts` this round holds only passing entries
    // ([`Verdict::passes`]) by construction (any such rule would already be in `blocking`,
    // handled above), so no violation events are emitted here regardless.
    let has_override_reminder = reminder.is_some();
    if let Some(reminder) = reminder {
        notes.push(reminder);
    }
    if notes.is_empty() {
        reply(run, Reply::Silent);
    } else {
        reply(run, Reply::Notice(notes.join("\n")));
    }
    let decision = if partial.is_some() || has_override_reminder {
        PlanGovernanceDecision::Warn
    } else {
        PlanGovernanceDecision::Allow
    };
    record_hook_round(run, session, decision, &verdicts, judge_ran);
    #[cfg(feature = "telemetry")]
    {
        use super::check_telemetry::WarnReason;
        let warn_reasons: Vec<WarnReason> = [
            partial.is_some().then_some(WarnReason::PartialCoverage),
            has_override_reminder.then_some(WarnReason::ActiveOverride),
        ]
        .into_iter()
        .flatten()
        .collect();
        super::check_telemetry::report_round(run, decision, &warn_reasons, records);
        send_hook_governance_events(
            run.command,
            run.root,
            run.started.instant,
            decision,
            &verdicts,
            false,
            (session.loop_state(kind).rounds, run.max_rounds),
        );
    }
}

/// Bring the block episodes of `run`'s loop up to date with a judged round, and
/// return the resolution records for those that ended. A rule still blocking
/// in an episode ends `fixed` when `verdicts` judges it followed, `round_limit`
/// when it is among `round_limited`, and `not_rechecked` when `verdicts`
/// leaves it out or judges it not applicable, which for a diff includes a rule
/// whose answer turns on code the diff does not show.
/// Each rule in `denied` then joins its ADR's episode, or opens one at this
/// round. A rule from a file without a v2 header has no ADR, so opens nothing.
/// Every round that leaves an episode open notes the rules directory for the
/// session's end, since that note is best-effort and one write can be lost.
/// With nowhere to send or log the records, no episode is kept, and any an
/// earlier round kept are dropped, so a block can't carry its rounds and
/// denials across a stretch with telemetry off.
///
/// The episodes are settled under the sessions lock on a fresh copy, not on
/// `session`, which was loaded before the judge call: two overlapping rounds of
/// one gate would otherwise each end, and each report, the same episode.
/// [`governance_session::store_loop`] leaves them as this stores them.
#[cfg(feature = "telemetry")]
fn settle_episodes(
    run: &HookRun<'_>,
    session: &mut GovernanceSession,
    verdicts: &[CheckedRule],
    headers: &BTreeMap<String, AdrMeta>,
    denied: &[&CheckedRule],
    round_limited: &[String],
) -> Vec<crate::telemetry::otlp::LogRecord> {
    use governance_session::Resolution;

    let Some(session_id) = run.session_id else {
        return Vec::new();
    };
    if super::brief_telemetry::Destination::resolve_quietly().is_none() {
        governance_session::update(session_id, run.rules_dir, |s| {
            s.loop_state_mut(run.kind).episodes.clear();
        });
        session.loop_state_mut(run.kind).episodes.clear();
        return Vec::new();
    }
    let judged: BTreeMap<String, Verdict> = verdicts
        .iter()
        .map(|v| (governance_session::key(&v.doc_slug, &v.rule_id), v.verdict))
        .collect();
    let denied: Vec<(String, Vec<super::brief_telemetry::BriefedAdr>)> = denied
        .iter()
        .map(|rule| {
            (
                governance_session::key(&rule.doc_slug, &rule.rule_id),
                super::brief_telemetry::group_headers(headers.get(&rule.doc_slug)),
            )
        })
        .collect();
    let round = session.loop_state(run.kind).rounds;
    let action_id = crate::telemetry::otlp::current_action_id();
    let mut ended = Vec::new();
    let stored = governance_session::update(session_id, run.rules_dir, |s| {
        let state = s.loop_state_mut(run.kind);
        state.end_blocks(|key| match judged.get(key) {
            Some(Verdict::Followed) => Some(Resolution::Fixed),
            Some(Verdict::NotApplicable) | None => Some(Resolution::NotRechecked),
            Some(_) if round_limited.iter().any(|k| k == key) => Some(Resolution::RoundLimit),
            Some(_) => None,
        });
        for (key, adrs) in &denied {
            for adr in adrs {
                state.open_block(round, &adr.key, &adr.adr_ids, key, action_id);
            }
        }
        ended = state
            .take_ended_at(round)
            .into_iter()
            .map(|episode| (run.kind, episode))
            .collect();
    });
    session.loop_state_mut(run.kind).episodes = stored.loop_state(run.kind).episodes.clone();
    if !stored.loop_state(run.kind).episodes.is_empty() {
        super::session_end::record_rules_dir(session_id, run.rules_dir);
    }
    super::check_telemetry::resolved_records(run.session_id, run.agent_id, &ended)
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
            // The code alone is a reason too: the dialog and a typed
            // `accepted` record one with no text.
            let reason = match (o.reason_code, o.reason.as_deref()) {
                (None, None) => "no reason given".to_string(),
                (code, text) => describe_reason(code, text),
            };
            format!(
                "{}: manually overridden ({reason}) at {}; not re-checked.",
                o.key, o.at
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

/// [`truncate`] for a single panel row: a panel row is one terminal line, so
/// a raw newline (a diff, a multi-line plan, a span quoted across lines) would
/// break the box borders. Runs of whitespace, newlines and tabs included,
/// collapse to one space before shortening.
fn truncate_line(text: &str, width: usize) -> String {
    truncate(
        &text.split_whitespace().collect::<Vec<_>>().join(" "),
        width,
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use crate::cli::args::DEFAULT_MAX_ROUNDS;
    use std::path::PathBuf;
    use tempfile::{tempdir, TempDir};

    use crate::rules::types::RuleLevel;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

    thread_local! {
        pub(in crate::cli::commands) static EMITTED: std::cell::RefCell<Vec<String>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    /// Every hook line [`emit`] printed on this thread since the last call.
    #[cfg(feature = "telemetry")]
    pub(in crate::cli::commands) fn take_emitted() -> Vec<String> {
        EMITTED.with(|cell| std::mem::take(&mut *cell.borrow_mut()))
    }

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

    /// Assert the enforcement log holds exactly one round, a direct run of
    /// `gate` over `rules_dir` that came to `decision` with the token-signing
    /// ADR's strongest verdict `verdict`.
    pub(crate) fn assert_one_direct_round(
        gate: enforcement_log::Gate,
        rules_dir: &Path,
        decision: enforcement_log::Decision,
        verdict: Verdict,
    ) {
        let rounds = enforcement_log::read(None);
        assert_eq!(rounds.len(), 1, "{rounds:?}");
        let round = &rounds[0];
        assert_eq!(
            (round.gate, round.mode, round.decision),
            (gate, Mode::Direct, decision)
        );
        assert_eq!(round.rules_dir, rules_dir.display().to_string());
        assert_eq!((round.session_id.as_deref(), round.round), (None, None));
        assert_eq!(
            round.adrs,
            vec![enforcement_log::AdrVerdict {
                adr: "cross-cutting-token-signing-1c57".to_string(),
                verdict,
            }]
        );
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
            codex_hook: false,
            codex_ask: false,
            codex_picker: false,
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

    /// Each selected document's v2 header rides along by slug, so a verdict's
    /// rule can name its ADR. A document without one adds nothing.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_gather_rules_keeps_each_v2_header_by_slug() {
        use super::super::check_telemetry::tests::with_header;

        let root = seed(&[
            (
                "token-signing.md",
                &with_header("adr-sign-1", Some("grp-sign"), OAUTH_DOC),
            ),
            ("token-signing-legacy.md", OAUTH_DOC),
            ("cross-cutting-terraform-c340.md", TERRAFORM_DOC),
        ]);
        let report = crate::rules::load_rule_set(root.path()).unwrap();
        let index = crate::rules::scope::ScopeIndex::build(&report, root.path(), "fp".to_string());
        let query = Query::new("Sign access tokens for OAuth".to_string());
        let selection = select::prefilter(&index, &query, 10, 30).finish(Stage2::NotRequested);
        assert_eq!(selection.selected.len(), 2);

        let gathered = gather_rules(
            &selection,
            root.path(),
            &GovernanceSession::default(),
            check::ArtifactKind::Plan,
            "test-digest",
        );

        assert_eq!(gathered.rules.len(), 4);
        assert_eq!(gathered.headers.len(), 1);
        let header = &gathered.headers["token-signing"];
        assert_eq!(header.adr_id.as_deref(), Some("adr-sign-1"));
        assert_eq!(header.candidate_group_id.as_deref(), Some("grp-sign"));
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

    /// Codex has no `/actual-cli:override` command, so its deny reason names
    /// only the terminal route; the rest of the reason is the same.
    #[test]
    fn test_hook_deny_reason_for_codex_names_only_the_terminal_override() {
        let a = checked("R-A-002", Verdict::Conflicting, "span", "reason");
        let claude = hook_deny_reason_for(
            HookHost::Claude,
            &[&a],
            Some("sess-1"),
            None,
            check::ArtifactKind::Plan,
        );
        let codex = hook_deny_reason_for(
            HookHost::Codex,
            &[&a],
            Some("sess-1"),
            None,
            check::ArtifactKind::Plan,
        );
        assert!(claude.contains(OVERRIDE_COMMAND), "{claude}");
        assert!(!codex.contains(OVERRIDE_COMMAND), "{codex}");
        assert!(codex.contains("actual check-override"), "{codex}");
        assert!(codex.contains("outside Codex"), "{codex}");
        assert!(codex.contains("Session: sess-1"), "{codex}");
        let first = |text: &str| text.lines().next().unwrap().to_string();
        assert_eq!(first(&claude), first(&codex));
    }

    /// Inside Codex the OpenAI backends come first; Claude Code keeps its
    /// order, and nothing else about the knobs changes.
    #[test]
    fn test_check_knobs_for_host_changes_only_the_default_runners() {
        let args = PlanCheckArgs {
            plan: Vec::new(),
            plan_file: None,
            repo: None,
            rules_dir: None,
            claude_hook: false,
            codex_hook: true,
            codex_ask: false,
            codex_picker: false,
            limit: 7,
            candidates: 9,
            no_rank: false,
            runner: None,
            model: Some("m".to_string()),
            json: false,
            rebuild: true,
            max_rounds: 3,
        };
        let claude = args.check_knobs().for_host(HookHost::Claude);
        assert_eq!(claude.default_runners, rules_rank::DEFAULT_RUNNERS);
        let codex = args.check_knobs().for_host(HookHost::Codex);
        assert_eq!(codex.default_runners, rules_rank::CODEX_DEFAULT_RUNNERS);
        assert_eq!((codex.limit, codex.candidates, codex.rebuild), (7, 9, true));
        assert_eq!(codex.model, Some("m"));
    }

    #[test]
    fn test_emit_status_is_silent_on_codex() {
        // Printing is not captured here; this pins that both arms run.
        emit_status(HookHost::Codex, "No rule applies.");
        emit_status(HookHost::Claude, "No rule applies.");
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
        // A bare `/override` is an unknown command in Claude Code; only the
        // plugin-namespaced form reaches the UserPromptSubmit hook.
        assert!(
            reason.contains("`/actual-cli:override <rule-id> [code] [reason]`"),
            "{reason}"
        );
        assert!(!reason.contains("`/override"), "{reason}");
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
            verdicts: vec![checked(
                "R-A-001",
                Verdict::Followed,
                "Sign every access token with RS256.",
                "uses RS256",
            )],
            runner_label: Some("claude-cli (sonnet)".to_string()),
            partial: None,
            headers: BTreeMap::new(),
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
            verdicts: vec![checked(
                "R-A-001",
                Verdict::Followed,
                "Sign every access token with RS256.",
                "uses RS256",
            )],
            runner_label: Some("claude-cli (sonnet)".to_string()),
            partial: Some((60, 85)),
            headers: BTreeMap::new(),
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
            headers: BTreeMap::new(),
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
            verdicts: vec![
                checked("R-A-001", Verdict::Followed, "Sign with RS256.", ""),
                checked("R-A-002", Verdict::NotApplicable, "", ""),
            ],
            runner_label: None,
            partial: None,
            headers: BTreeMap::new(),
        };
        let json = render_json(&conforming, check::ArtifactKind::Plan);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "conforming");
        assert!(value.get("partial").is_none());
        assert_eq!(value["verdicts"][0]["verdict"], "followed");
        assert_eq!(value["verdicts"][0]["span"], "Sign with RS256.");
        assert_eq!(value["verdicts"][1]["verdict"], "not_applicable");
        assert_eq!(value["verdicts"][1]["span"], "");

        let conflicting = Outcome::Verdicts {
            selection: selection.clone(),
            verdicts: vec![checked("R-A-002", Verdict::Conflicting, "x", "y")],
            runner_label: None,
            partial: None,
            headers: BTreeMap::new(),
        };
        let json = render_json(&conflicting, check::ArtifactKind::Plan);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "conflicting");

        let requires_decision = Outcome::Verdicts {
            selection: selection.clone(),
            verdicts: vec![checked("R-A-003", Verdict::RequiresDecision, "x", "y")],
            runner_label: None,
            partial: None,
            headers: BTreeMap::new(),
        };
        let json = render_json(&requires_decision, check::ArtifactKind::Plan);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "requires_decision");

        let partial = Outcome::Verdicts {
            selection: selection.clone(),
            verdicts: vec![checked("R-A-001", Verdict::NotApplicable, "", "")],
            runner_label: None,
            partial: Some((60, 85)),
            headers: BTreeMap::new(),
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
    fn test_truncate_line_collapses_newlines_and_tabs() {
        assert_eq!(
            truncate_line("diff --git a/x b/x\nindex 1..2\n\t+line", 72),
            "diff --git a/x b/x index 1..2 +line"
        );
        assert_eq!(truncate_line("a\n\nbcdef", 4), "a b…");
    }

    /// The gap this guards: the direct-mode impl-check panel printed the
    /// diff's raw newlines (and those in a multi-line quoted span), which
    /// broke the box borders. Every rendered line must stay framed.
    #[test]
    fn test_render_panel_keeps_multiline_diff_and_span_inside_the_box() {
        let selection = Selection {
            plan: "p".to_string(),
            paths: Vec::new(),
            indexed_documents: 1,
            limit: 10,
            selected: Vec::new(),
            stage2: Stage2::NotRequested,
        };
        let outcome = Outcome::Verdicts {
            selection,
            verdicts: vec![checked(
                "R-A-001",
                Verdict::Conflicting,
                "+let key =\n+    \"sk-live\";",
                "hardcodes\na secret",
            )],
            runner_label: None,
            partial: None,
            headers: BTreeMap::new(),
        };
        let panel = render_panel(
            &outcome,
            "diff --git a/x.rs b/x.rs\nindex 1..2 100644\n--- a/x.rs",
            Path::new("/x/.actual/rules"),
            80,
            check::ArtifactKind::Diff,
        );
        let plain = console::strip_ansi_codes(&panel);
        let lines: Vec<&str> = plain.lines().collect();
        for line in &lines[1..lines.len() - 1] {
            assert!(
                line.starts_with('│') || line.starts_with('├'),
                "unframed line {line:?} in:\n{plain}"
            );
        }
        assert!(
            plain.contains("diff --git a/x.rs b/x.rs index 1..2"),
            "{plain}"
        );
        assert!(plain.contains("+let key = + \"sk-live\";"), "{plain}");
        assert!(plain.contains("hardcodes a secret"), "{plain}");
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

    // ── v2 ADR status: only active decisions are judged ──────────────────

    /// `OAUTH_DOC` behind a v2 header carrying `status`.
    fn v2_oauth(status: &str) -> String {
        format!(
            "---\nversion: 2\nname: cross-cutting-token-signing-1c57\nmetadata:\n  actual:\n    adr_id: adr-oauth\n    status: {status}\n  scope:\n    root: .\n---\n\n{OAUTH_DOC}"
        )
    }

    /// A judge that calls R-A-001 conflicting, so reaching it means a deny.
    #[cfg(unix)]
    fn conflicting_judge(dir: &Path) -> PathBuf {
        fake_claude(
            dir,
            &check_output(serde_json::json!([
                {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "conflicting", "span": "skips signing", "reason": "unsigned"},
                {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "not_applicable", "span": "", "reason": "fine"},
            ])),
        )
    }

    #[cfg(unix)]
    fn run_plan_gate(root: &Path) -> Outcome {
        let mut args = base_args();
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        run_pipeline(
            "Sign access tokens with RS256",
            root,
            &crate::rules::rules_dir(root),
            &args.check_knobs(),
            check::ArtifactKind::Plan,
            false,
            &GovernanceSession::default(),
        )
        .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn test_an_active_v2_adr_with_a_violated_rule_still_denies_in_the_same_shape() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[("cross-cutting-token-signing-1c57.md", &v2_oauth("accepted"))]);
        let bin = tempdir().unwrap();
        let _binary = EnvGuard::set(
            "CLAUDE_BINARY",
            conflicting_judge(bin.path()).to_str().unwrap(),
        );

        let outcome = run_plan_gate(root.path());
        #[rustfmt::skip]
        let Outcome::Verdicts { verdicts, .. } = outcome else { panic!("an active ADR must reach the judge") };
        let conflicts: Vec<&CheckedRule> = verdicts
            .iter()
            .filter(|v| v.verdict == Verdict::Conflicting)
            .collect();
        assert_eq!(conflicts.len(), 1);
        let reason = hook_deny_reason(&conflicts, None, None, check::ArtifactKind::Plan);
        assert!(reason.contains("CONFLICT R-A-001 (MUST)"), "{reason}");
    }

    #[cfg(unix)]
    #[test]
    fn test_a_retired_v2_adr_with_a_violated_rule_produces_no_deny() {
        for status in [
            "superseded",
            "deprecated",
            "rejected",
            "deferred",
            "draft",
            "deleted",
            "archived-draft",
            "suggested",
        ] {
            let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
            let home = tempdir().unwrap();
            let _guards = isolated_config(&home);
            let root = seed(&[("cross-cutting-token-signing-1c57.md", &v2_oauth(status))]);
            let bin = tempdir().unwrap();
            // The judge would deny if it were ever asked.
            let _binary = EnvGuard::set(
                "CLAUDE_BINARY",
                conflicting_judge(bin.path()).to_str().unwrap(),
            );
            assert!(
                matches!(run_plan_gate(root.path()), Outcome::NothingApplies),
                "{status}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_a_v2_adr_with_an_unrecognised_or_absent_status_is_judged() {
        for header_status in ["status: something-new\n", ""] {
            let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
            let home = tempdir().unwrap();
            let _guards = isolated_config(&home);
            let text = format!(
                "---\nversion: 2\nmetadata:\n  actual:\n    adr_id: adr-oauth\n    {header_status}---\n\n{OAUTH_DOC}"
            );
            let root = seed(&[("cross-cutting-token-signing-1c57.md", &text)]);
            let bin = tempdir().unwrap();
            let _binary = EnvGuard::set(
                "CLAUDE_BINARY",
                conflicting_judge(bin.path()).to_str().unwrap(),
            );
            assert!(
                matches!(run_plan_gate(root.path()), Outcome::Verdicts { .. }),
                "{header_status:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_the_impl_gate_honours_status_the_same_way() {
        for (status, judged) in [("accepted", true), ("superseded", false)] {
            let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
            let home = tempdir().unwrap();
            let _guards = isolated_config(&home);
            let root = seed(&[("cross-cutting-token-signing-1c57.md", &v2_oauth(status))]);
            let bin = tempdir().unwrap();
            let _binary = EnvGuard::set(
                "CLAUDE_BINARY",
                conflicting_judge(bin.path()).to_str().unwrap(),
            );
            let mut args = base_args();
            args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
            let outcome = run_pipeline(
                "Sign access tokens with RS256",
                root.path(),
                &crate::rules::rules_dir(root.path()),
                &args.check_knobs(),
                check::ArtifactKind::Diff,
                false,
                &GovernanceSession::default(),
            )
            .unwrap();
            assert_eq!(
                matches!(outcome, Outcome::Verdicts { .. }),
                judged,
                "{status}"
            );
            assert_eq!(
                matches!(outcome, Outcome::NothingApplies),
                !judged,
                "{status}"
            );
        }
    }

    #[test]
    fn test_gather_rules_skips_a_document_edited_to_a_retired_status_after_selection() {
        let root = seed(&[("cross-cutting-token-signing-1c57.md", &v2_oauth("accepted"))]);
        let report = crate::rules::load_rule_set(root.path()).unwrap();
        let index = crate::rules::scope::ScopeIndex::build(&report, root.path(), "fp".to_string());
        let query = Query::new("Sign tokens".to_string());
        let selection = select::prefilter(&index, &query, 10, 30).finish(Stage2::NotRequested);
        let gather = || {
            gather_rules(
                &selection,
                root.path(),
                &GovernanceSession::default(),
                check::ArtifactKind::Plan,
                "test-digest",
            )
        };
        assert_eq!(gather().rules.len(), 2);

        std::fs::write(
            crate::rules::rules_dir(root.path()).join("cross-cutting-token-signing-1c57.md"),
            v2_oauth("superseded"),
        )
        .unwrap();
        assert!(gather().rules.is_empty());
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

    /// A diff of a file inside a rule's root reaches the judge (no runner is
    /// available, so `NoRunner` with a selection is as far as it gets), and a
    /// diff entirely outside that root never selects the rule.
    #[test]
    fn test_run_pipeline_judges_a_rooted_rule_only_on_a_diff_inside_its_root() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _no_claude = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/path/to/claude");

        let rooted = v2_oauth("accepted").replace(
            "root: .",
            "root: services/auth\n    paths:\n      - \"oauth/**\"",
        );
        let root = seed(&[("cross-cutting-token-signing-1c57.md", &rooted)]);
        let mut args = base_args();
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let rules_dir = crate::rules::rules_dir(root.path());
        let diff_of = |path: &str| {
            format!(
                "diff --git a/{path} b/{path}\nnew file mode 100644\nindex 0000000..1111111\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1 @@\n+sign access tokens with RS256\n"
            )
        };
        let run = |diff: &str| {
            run_pipeline(
                diff,
                root.path(),
                &rules_dir,
                &args.check_knobs(),
                check::ArtifactKind::Diff,
                false,
                &GovernanceSession::default(),
            )
            .unwrap()
        };

        assert!(matches!(
            run(&diff_of("services/auth/oauth/token.rs")),
            Outcome::NoRunner { documents_selected, .. } if documents_selected > 0
        ));
        assert!(matches!(
            run(&diff_of("web/app/token.rs")),
            Outcome::NothingApplies
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
                    "verdict": "not_applicable",
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
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-001", "verdict": "followed", "span": "Sign every access token with RS256.", "reason": "uses RS256"},
            {"doc_slug": "cross-cutting-token-signing-1c57", "rule_id": "R-A-002", "verdict": "not_applicable", "span": "", "reason": "no logging"},
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
            reason: Some("reviewed".to_string()),
            at: chrono::Utc::now(),
            round: 1,
            source: Default::default(),
            gate: None,
            reason_code: None,
        });
        session.overrides.push(governance_session::Override {
            key: governance_session::key("cross-cutting-token-signing-1c57", "R-A-002"),
            reason: Some("reviewed".to_string()),
            at: chrono::Utc::now(),
            round: 1,
            source: Default::default(),
            gate: None,
            reason_code: None,
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
            reason: Some("reviewed and accepted".to_string()),
            at: chrono::Utc::now(),
            round: 1,
            source: Default::default(),
            gate: None,
            reason_code: None,
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
            reason: Some("reviewed and accepted".to_string()),
            at: chrono::Utc::now(),
            round: 1,
            source: Default::default(),
            gate: None,
            reason_code: None,
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

    // ── finish_hook: the policy shared by plan-check and impl-check ─────────

    fn verdicts_outcome(verdicts: Vec<CheckedRule>) -> Outcome {
        Outcome::Verdicts {
            selection: Selection {
                plan: "p".to_string(),
                paths: Vec::new(),
                indexed_documents: 1,
                limit: 10,
                selected: vec![],
                stage2: Stage2::NotRequested,
            },
            verdicts,
            runner_label: Some("claude-cli (sonnet)".to_string()),
            partial: None,
            headers: BTreeMap::new(),
        }
    }

    fn hook_run<'a>(
        kind: check::ArtifactKind,
        session_id: Option<&'a str>,
        rules_dir: &'a Path,
        max_rounds: u32,
    ) -> HookRun<'a> {
        HookRun {
            host: crate::cli::commands::hook_host::HookHost::Claude,
            kind,
            command: "test --claude-hook",
            session_id,
            agent_id: None,
            artifact_digest: "digest-1",
            root: rules_dir,
            rules_dir,
            max_rounds,
            ask: None,
            codex_ask: false,
            picker: false,
            #[cfg(feature = "telemetry")]
            started: super::super::check_telemetry::Started::now(),
            #[cfg(feature = "telemetry")]
            filter: None,
        }
    }

    #[test]
    fn test_finish_hook_records_clearances_and_rounds_on_the_kinds_own_loop() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let rules = tempdir().unwrap();
        let key = governance_session::key("cross-cutting-token-signing-1c57", "R-A-001");

        for (kind, other) in [
            (check::ArtifactKind::Diff, check::ArtifactKind::Plan),
            (check::ArtifactKind::Plan, check::ArtifactKind::Diff),
        ] {
            let mut session = GovernanceSession::default();
            let run = hook_run(kind, Some("s-clear"), rules.path(), 3);
            finish_hook(
                &run,
                &mut session,
                verdicts_outcome(vec![checked("R-A-001", Verdict::NotApplicable, "", "ok")]),
            );

            let own = session.loop_state(kind);
            assert_eq!(own.cleared.get(&key).map(String::as_str), Some("digest-1"));
            assert_eq!(own.rounds, 1);
            let untouched = session.loop_state(other);
            assert!(
                untouched.cleared.is_empty(),
                "{kind:?} leaked into {other:?}"
            );
            assert_eq!(untouched.rounds, 0);
        }
    }

    #[cfg(feature = "telemetry")]
    #[test]
    fn test_finish_hook_completed_event_carries_round_counts() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let rules = tempdir().unwrap();
        let mut session = GovernanceSession::default();
        let run = hook_run(check::ArtifactKind::Plan, Some("s-rounds"), rules.path(), 3);

        let events = with_captured_plan_governance_events(|| {
            finish_hook(
                &run,
                &mut session,
                verdicts_outcome(vec![checked("R-A-001", Verdict::NotApplicable, "", "ok")]),
            );
        });

        let completed = events
            .iter()
            .find(|e| {
                e.event == crate::api::types::PlanGovernanceEventName::PlanGovernanceCheckCompleted
            })
            .expect("a completed event");
        let props = completed.properties.as_ref().unwrap();
        // One judged round completed, out of a 3-round budget.
        assert_eq!(props.round_index, Some(1));
        assert_eq!(props.round_total, Some(3));
    }

    #[test]
    fn test_finish_hook_does_not_count_a_round_when_no_judge_ran() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let rules = tempdir().unwrap();
        let mut session = GovernanceSession::default();
        let mut outcome = verdicts_outcome(Vec::new());
        if let Outcome::Verdicts { runner_label, .. } = &mut outcome {
            *runner_label = None;
        }
        finish_hook(
            &hook_run(
                check::ArtifactKind::Diff,
                Some("s-nojudge"),
                rules.path(),
                3,
            ),
            &mut session,
            outcome,
        );
        assert_eq!(session.diff.rounds, 0);
    }

    #[test]
    fn test_finish_hook_counts_a_denial_on_the_kinds_own_loop_only() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let rules = tempdir().unwrap();
        let key = governance_session::key("cross-cutting-token-signing-1c57", "R-A-002");

        for (kind, other) in [
            (check::ArtifactKind::Diff, check::ArtifactKind::Plan),
            (check::ArtifactKind::Plan, check::ArtifactKind::Diff),
        ] {
            let mut session = GovernanceSession::default();
            finish_hook(
                &hook_run(kind, Some("s-deny"), rules.path(), 3),
                &mut session,
                verdicts_outcome(vec![checked("R-A-002", Verdict::Conflicting, "span", "no")]),
            );
            assert_eq!(session.loop_state(kind).deny_counts.get(&key), Some(&1));
            assert!(session.loop_state(other).deny_counts.is_empty());
            // Persisted, so the next hook call sees it.
            let reloaded = governance_session::load("s-deny", rules.path());
            assert_eq!(reloaded.loop_state(kind).deny_counts.get(&key), Some(&1));
        }
    }

    /// The blocking set is each round's denials, replaced every judged round:
    /// a rule denied once and absent from the next round's verdicts (dropped
    /// from selection or past the cap) stops blocking even though it was never
    /// cleared, and a round where the judge did not run changes nothing.
    #[test]
    fn test_finish_hook_replaces_the_blocking_set_each_judged_round() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let rules = tempdir().unwrap();
        let a = governance_session::key("cross-cutting-token-signing-1c57", "R-A-001");
        let b = governance_session::key("cross-cutting-token-signing-1c57", "R-A-002");
        let run = hook_run(check::ArtifactKind::Plan, Some("s-blk"), rules.path(), 3);
        let mut session = GovernanceSession::default();

        finish_hook(
            &run,
            &mut session,
            verdicts_outcome(vec![checked("R-A-001", Verdict::Conflicting, "span", "no")]),
        );
        assert!(session.plan.is_blocking(&a));

        // Not judged at all this round: no verdict for R-A-001, so no clearance.
        finish_hook(
            &run,
            &mut session,
            verdicts_outcome(vec![checked("R-A-002", Verdict::Conflicting, "span", "no")]),
        );
        assert!(!session.plan.is_blocking(&a));
        assert!(!session.plan.cleared.contains_key(&a));
        assert!(session.plan.is_blocking(&b));

        // Nothing was decided: the previous round's blocking set stands.
        for failed in [
            Outcome::NoRunner {
                documents_selected: 1,
                reason: "none".into(),
            },
            Outcome::CheckFailed {
                documents_selected: 1,
                reason: "boom".into(),
            },
        ] {
            finish_hook(&run, &mut session, failed);
            assert!(session.plan.is_blocking(&b));
        }

        let reloaded = governance_session::load("s-blk", rules.path());
        assert_eq!(reloaded.plan.blocking.iter().collect::<Vec<_>>(), [&b]);
    }

    /// When the gate decides nothing blocks without judging -- no rule
    /// applies, or every applicable one is already settled -- the previous
    /// round's blocking rules stop being targets for `/override all`, on
    /// disk as well as in memory.
    #[test]
    fn test_finish_hook_empties_the_blocking_set_when_nothing_blocks_unjudged() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let rules = tempdir().unwrap();
        let a = governance_session::key("cross-cutting-token-signing-1c57", "R-A-001");
        let run = hook_run(check::ArtifactKind::Plan, Some("s-empty"), rules.path(), 3);
        let all_settled = || {
            let mut outcome = verdicts_outcome(Vec::new());
            if let Outcome::Verdicts { runner_label, .. } = &mut outcome {
                *runner_label = None;
            }
            outcome
        };

        for nothing_blocks in [Outcome::NothingApplies, all_settled()] {
            let mut session = GovernanceSession::default();
            finish_hook(
                &run,
                &mut session,
                verdicts_outcome(vec![checked("R-A-001", Verdict::Conflicting, "span", "no")]),
            );
            assert!(governance_session::load("s-empty", rules.path())
                .plan
                .is_blocking(&a));

            finish_hook(&run, &mut session, nothing_blocks);
            assert!(!session.plan.is_blocking(&a));
            let reloaded = governance_session::load("s-empty", rules.path());
            assert!(!reloaded.plan.is_blocking(&a));
            assert_eq!(reloaded.plan.rounds, 1, "not a judged round");
        }
    }

    /// An override recorded while a check round was judging survives the
    /// round's store: the hook's copy predates it, and storing that copy
    /// whole would silently drop it while the user was told "overridden".
    #[test]
    fn test_finish_hook_keeps_an_override_recorded_during_the_round() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let rules = tempdir().unwrap();
        let other = governance_session::key("doc", "R-Z-001");
        let mut session = governance_session::load("s-race", rules.path());

        governance_session::record_override_ending(
            "s-race",
            rules.path(),
            std::slice::from_ref(&other),
            None,
            None,
            governance_session::OverrideSource::SessionPrompt,
        );
        finish_hook(
            &hook_run(check::ArtifactKind::Diff, Some("s-race"), rules.path(), 3),
            &mut session,
            verdicts_outcome(vec![checked("R-A-002", Verdict::Conflicting, "span", "no")]),
        );

        let reloaded = governance_session::load("s-race", rules.path());
        assert_eq!(reloaded.overrides.len(), 1);
        assert_eq!(reloaded.overrides[0].key, other);
        assert_eq!(reloaded.diff.rounds, 1);
        assert!(reloaded.diff.is_blocking(&governance_session::key(
            "cross-cutting-token-signing-1c57",
            "R-A-002"
        )));
    }

    /// Impl-check's round limit: the same fail-open as plan-check's, driven
    /// by the diff loop's own count, and named for the diff in the audit log.
    #[test]
    fn test_finish_hook_diff_round_limit_fails_open_and_writes_the_audit_log() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let rules = tempdir().unwrap();
        let key = governance_session::key("cross-cutting-token-signing-1c57", "R-A-002");

        let mut session = GovernanceSession::default();
        // Already denied `max_rounds` times: this round's denial tips it over.
        session.diff.deny_counts.insert(key.clone(), 2);
        finish_hook(
            &hook_run(check::ArtifactKind::Diff, Some("s-limit"), rules.path(), 2),
            &mut session,
            verdicts_outcome(vec![checked("R-A-002", Verdict::Conflicting, "span", "no")]),
        );

        assert_eq!(session.diff.deny_counts.get(&key), Some(&3));
        assert!(session.plan.deny_counts.is_empty());
        let log = std::fs::read_to_string(home.path().join(governance_session::AUDIT_LOG_NAME))
            .expect("the round-limit pass must be written to the audit log");
        assert!(
            log.contains("Actual implementation governance hit its round limit"),
            "{log}"
        );
    }

    /// Each of a hook round's three exits writes one line to the enforcement
    /// log, with the decision telemetry reports for it and the session's round.
    #[test]
    fn test_finish_hook_records_each_exit_in_the_enforcement_log() {
        use enforcement_log::{AdrVerdict, Decision, Gate};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _on = EnvGuard::remove(enforcement_log::OFF_SWITCH);
        let rules = tempdir().unwrap();
        let key = governance_session::key("cross-cutting-token-signing-1c57", "R-A-002");
        let conflict = || vec![checked("R-A-002", Verdict::Conflicting, "span", "no")];
        let conforming = || vec![checked("R-A-001", Verdict::Followed, "span", "ok")];

        let cases = [
            (check::ArtifactKind::Diff, 2, 2, conflict(), Decision::Warn),
            (check::ArtifactKind::Plan, 3, 0, conflict(), Decision::Block),
            (
                check::ArtifactKind::Plan,
                3,
                0,
                conforming(),
                Decision::Allow,
            ),
        ];
        for (kind, max_rounds, earlier_denials, verdicts, _) in cases.clone() {
            let mut session = GovernanceSession::default();
            session
                .loop_state_mut(kind)
                .deny_counts
                .insert(key.clone(), earlier_denials);
            finish_hook(
                &hook_run(kind, Some("s-log"), rules.path(), max_rounds),
                &mut session,
                verdicts_outcome(verdicts),
            );
        }

        let rounds = enforcement_log::read(None);
        assert_eq!(rounds.len(), cases.len(), "{rounds:?}");
        for (round, (kind, _, _, verdicts, decision)) in rounds.iter().zip(cases) {
            assert_eq!(
                (round.gate, round.mode, round.decision),
                (Gate::from(kind), Mode::Hook, decision)
            );
            assert_eq!(round.session_id.as_deref(), Some("s-log"));
            assert_eq!(round.round, Some(1));
            assert_eq!(round.rules_dir, rules.path().display().to_string());
            assert_eq!(
                round.adrs,
                vec![AdrVerdict {
                    adr: "cross-cutting-token-signing-1c57".to_string(),
                    verdict: verdicts[0].verdict,
                }]
            );
        }
    }

    /// A clean round that judged only part of what applies is `warn`, and a
    /// round outside a session records neither a session nor a round.
    #[test]
    fn test_finish_hook_logs_partial_coverage_as_warn_without_a_session() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _on = EnvGuard::remove(enforcement_log::OFF_SWITCH);
        let rules = tempdir().unwrap();
        let mut outcome = verdicts_outcome(vec![checked("R-A-001", Verdict::Followed, "", "ok")]);
        if let Outcome::Verdicts { partial, .. } = &mut outcome {
            *partial = Some((1, 2));
        }

        finish_hook(
            &hook_run(check::ArtifactKind::Plan, None, rules.path(), 3),
            &mut GovernanceSession::default(),
            outcome,
        );

        let rounds = enforcement_log::read(None);
        assert_eq!(rounds.len(), 1, "{rounds:?}");
        assert_eq!(rounds[0].decision, enforcement_log::Decision::Warn);
        assert_eq!(
            (rounds[0].session_id.as_deref(), rounds[0].round),
            (None, None)
        );
    }

    /// A call where every rule was already settled writes its line with no
    /// ADRs and no round number, so it doesn't repeat the last judged round's.
    #[test]
    fn test_finish_hook_logs_a_skipped_judge_without_a_round() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _on = EnvGuard::remove(enforcement_log::OFF_SWITCH);
        let rules = tempdir().unwrap();
        let run = hook_run(check::ArtifactKind::Plan, Some("s-skip"), rules.path(), 3);
        let mut session = GovernanceSession::default();
        session.plan.begin_round();
        let mut outcome = verdicts_outcome(Vec::new());
        if let Outcome::Verdicts { runner_label, .. } = &mut outcome {
            *runner_label = None;
        }

        finish_hook(&run, &mut session, outcome);

        let rounds = enforcement_log::read(None);
        assert_eq!(rounds.len(), 1, "{rounds:?}");
        assert_eq!(rounds[0].decision, enforcement_log::Decision::Allow);
        assert_eq!(
            (rounds[0].session_id.as_deref(), rounds[0].round),
            (Some("s-skip"), None)
        );
        assert!(rounds[0].adrs.is_empty());
    }

    /// Outcomes that reach no verdict are not rounds, so they write nothing.
    #[test]
    fn test_finish_hook_logs_nothing_without_a_verdict() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _on = EnvGuard::remove(enforcement_log::OFF_SWITCH);
        let rules = tempdir().unwrap();
        let run = hook_run(check::ArtifactKind::Plan, Some("s-none"), rules.path(), 3);
        for outcome in [
            Outcome::NothingApplies,
            Outcome::NoRunner {
                documents_selected: 1,
                reason: "none".into(),
            },
            Outcome::CheckFailed {
                documents_selected: 1,
                reason: "boom".into(),
            },
        ] {
            finish_hook(&run, &mut GovernanceSession::default(), outcome);
        }
        assert!(enforcement_log::read(None).is_empty());
    }

    /// A relative rules directory is recorded absolute, so `actual stats
    /// --repo` can match it whatever directory the gate ran from.
    #[test]
    fn test_record_direct_round_makes_the_rules_dir_absolute() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _on = EnvGuard::remove(enforcement_log::OFF_SWITCH);
        let relative = Path::new("repo").join(".actual").join("rules");

        record_direct_round(
            check::ArtifactKind::Diff,
            &relative,
            PlanGovernanceDecision::Allow,
            &[],
        );

        let rounds = enforcement_log::read(None);
        let recorded = Path::new(&rounds[0].rules_dir);
        assert!(recorded.is_absolute(), "{recorded:?}");
        assert!(recorded.ends_with(&relative), "{recorded:?}");
    }

    #[test]
    fn test_direct_decision_blocks_then_warns_then_allows() {
        let conflict = checked("R-A-002", Verdict::Conflicting, "span", "no");
        let decide = checked("R-A-002", Verdict::RequiresDecision, "span", "maybe");
        let followed = checked("R-A-001", Verdict::Followed, "span", "ok");
        for (verdicts, partial, expected) in [
            (
                vec![decide.clone(), conflict],
                false,
                PlanGovernanceDecision::Block,
            ),
            (
                vec![followed.clone(), decide],
                false,
                PlanGovernanceDecision::Warn,
            ),
            (vec![followed.clone()], true, PlanGovernanceDecision::Warn),
            (vec![followed], false, PlanGovernanceDecision::Allow),
        ] {
            assert_eq!(direct_decision(&verdicts, partial), expected);
        }
    }

    /// Each of a hook round's three exits, the round limit's pass, a deny and
    /// a clean pass, sends its action events before the plan-governance
    /// events, so the completed record never times their hand-off to the
    /// spool.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_finish_hook_reports_the_action_before_the_governance_events() {
        use super::super::brief_telemetry::tests::{attribute, take_captured};
        use super::super::check_telemetry::tests::assert_completed_before_governance;

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _env: Vec<EnvGuard> = [
            "ACTUAL_NO_TELEMETRY",
            "ACTUAL_OTLP_LOGS_ENDPOINT",
            "ACTUAL_OTLP_FILE",
            "TRACEPARENT",
        ]
        .into_iter()
        .map(EnvGuard::remove)
        .collect();
        std::fs::write(
            home.path().join("config.yaml"),
            "telemetry:\n  otlp_file: /tmp/actual-otlp-finish-hook-test.jsonl\n",
        )
        .unwrap();
        let rules = tempdir().unwrap();
        let key = governance_session::key("cross-cutting-token-signing-1c57", "R-A-002");
        let conflict = || vec![checked("R-A-002", Verdict::Conflicting, "span", "no")];
        let conforming = vec![checked("R-A-001", Verdict::NotApplicable, "", "ok")];

        for (exit, max_rounds, earlier_denials, verdicts, outcome) in [
            ("the round limit's pass", 2, 2, conflict(), "warn"),
            ("a deny", 3, 0, conflict(), "block"),
            ("a clean pass", 3, 0, conforming, "allow"),
        ] {
            let mut session = GovernanceSession::default();
            if earlier_denials > 0 {
                session
                    .plan
                    .deny_counts
                    .insert(key.clone(), earlier_denials);
            }
            let run = hook_run(
                check::ArtifactKind::Plan,
                Some("s-order"),
                rules.path(),
                max_rounds,
            );
            take_captured();

            let events = with_captured_plan_governance_events(|| {
                finish_hook(&run, &mut session, verdicts_outcome(verdicts));
            });

            let records = take_captured();
            assert_eq!(records.len(), 2, "{exit}");
            assert_eq!(
                attribute(&records[1], "actual.action.outcome"),
                outcome,
                "{exit}"
            );
            assert_completed_before_governance(&records[1], &events);
        }
    }

    /// A hook round that warns names what made it one on the completed
    /// record: the round limit's pass, which names nothing else even over
    /// some of the rules and under an override, and a clean pass over some of
    /// the rules, under an override, or both. A deny and a fully clean pass
    /// name nothing.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_finish_hook_names_what_made_a_round_warn() {
        use super::super::brief_telemetry::tests::{attribute, take_captured};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _env: Vec<EnvGuard> = [
            "ACTUAL_NO_TELEMETRY",
            "ACTUAL_OTLP_LOGS_ENDPOINT",
            "ACTUAL_OTLP_FILE",
            "TRACEPARENT",
        ]
        .into_iter()
        .map(EnvGuard::remove)
        .collect();
        std::fs::write(
            home.path().join("config.yaml"),
            "telemetry:\n  otlp_file: /tmp/actual-otlp-warn-reasons-test.jsonl\n",
        )
        .unwrap();
        let rules = tempdir().unwrap();
        let conflict = || vec![checked("R-A-002", Verdict::Conflicting, "span", "no")];
        let conforming = || vec![checked("R-A-001", Verdict::NotApplicable, "", "ok")];
        let over_some_rules = |verdicts| {
            let mut outcome = verdicts_outcome(verdicts);
            if let Outcome::Verdicts { partial, .. } = &mut outcome {
                *partial = Some((40, 55));
            }
            outcome
        };
        let session = |exhausted: bool, overridden: bool| {
            let mut session = GovernanceSession::default();
            if exhausted {
                let key = governance_session::key("cross-cutting-token-signing-1c57", "R-A-002");
                session.plan.deny_counts.insert(key, 2);
            }
            if overridden {
                session.overrides.push(governance_session::Override {
                    key: governance_session::key("cross-cutting-token-signing-1c57", "R-A-009"),
                    reason: Some("reviewed".to_string()),
                    at: chrono::Utc::now(),
                    round: 1,
                    source: governance_session::OverrideSource::Terminal,
                    gate: None,
                    reason_code: None,
                });
            }
            session
        };

        for (exit, mut session, outcome, max_rounds, decision, reasons) in [
            (
                "the round limit's pass",
                session(true, false),
                verdicts_outcome(conflict()),
                2,
                "warn",
                serde_json::json!(["round_limit"]),
            ),
            (
                "the round limit's pass over some rules, under an override",
                session(true, true),
                over_some_rules(conflict()),
                2,
                "warn",
                serde_json::json!(["round_limit"]),
            ),
            (
                "a deny under an override",
                session(false, true),
                verdicts_outcome(conflict()),
                3,
                "block",
                serde_json::json!([]),
            ),
            (
                "a clean pass",
                session(false, false),
                verdicts_outcome(conforming()),
                3,
                "allow",
                serde_json::json!([]),
            ),
            (
                "a clean pass over some rules",
                session(false, false),
                over_some_rules(conforming()),
                3,
                "warn",
                serde_json::json!(["partial_coverage"]),
            ),
            (
                "a clean pass under an override",
                session(false, true),
                verdicts_outcome(conforming()),
                3,
                "warn",
                serde_json::json!(["active_override"]),
            ),
            (
                "a clean pass over some rules, under an override",
                session(false, true),
                over_some_rules(conforming()),
                3,
                "warn",
                serde_json::json!(["partial_coverage", "active_override"]),
            ),
        ] {
            let run = hook_run(
                check::ArtifactKind::Plan,
                Some("s-warn"),
                rules.path(),
                max_rounds,
            );
            take_captured();

            finish_hook(&run, &mut session, outcome);

            let records = take_captured();
            assert_eq!(records.len(), 2, "{exit}");
            let completed = &records[1];
            assert_eq!(
                attribute(completed, "actual.action.outcome"),
                decision,
                "{exit}"
            );
            assert_eq!(
                attribute(completed, "actual.action.warn_reasons"),
                reasons,
                "{exit}"
            );
        }
    }

    #[test]
    fn test_finish_hook_without_a_session_id_touches_no_state() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let rules = tempdir().unwrap();
        let mut session = GovernanceSession::default();
        finish_hook(
            &hook_run(check::ArtifactKind::Diff, None, rules.path(), 3),
            &mut session,
            verdicts_outcome(vec![checked("R-A-002", Verdict::Conflicting, "span", "no")]),
        );
        assert_eq!(session, GovernanceSession::default());
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
    fn test_running_under_agent_host_false_when_neither_marker_is_set() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("CLAUDECODE");
        let _g2 = EnvGuard::remove("CLAUDE_CODE_ENTRYPOINT");
        let _g3 = EnvGuard::remove("CODEX_THREAD_ID");
        let _g4 = EnvGuard::remove("CODEX_SESSION_ID");
        assert!(!running_under_agent_host());
    }

    /// An agent's shell in Codex is refused too.
    #[test]
    fn test_running_under_agent_host_true_under_codex() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("CLAUDECODE");
        let _g2 = EnvGuard::remove("CLAUDE_CODE_ENTRYPOINT");
        for name in ["CODEX_THREAD_ID", "CODEX_SESSION_ID"] {
            let _other = EnvGuard::remove(if name == "CODEX_THREAD_ID" {
                "CODEX_SESSION_ID"
            } else {
                "CODEX_THREAD_ID"
            });
            let _g = EnvGuard::set(name, "01a1c2fe");
            assert!(running_under_agent_host(), "{name}");
        }
    }

    #[test]
    fn test_running_under_agent_host_true_when_claudecode_is_set() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("CLAUDECODE", "1");
        let _g2 = EnvGuard::remove("CLAUDE_CODE_ENTRYPOINT");
        assert!(running_under_agent_host());
    }

    #[test]
    fn test_running_under_agent_host_true_when_entrypoint_is_set() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("CLAUDECODE");
        let _g2 = EnvGuard::set("CLAUDE_CODE_ENTRYPOINT", "cli");
        assert!(running_under_agent_host());
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
            session: Some("sess-cli-1".to_string()),
            rules: vec![governance_session::key("doc", "R-A-001")],
            reason: Some("reviewed and accepted".to_string()),
            reason_code: None,
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
            claude_hook: false,
            via: OverrideVia::Prompt,
            gate: None,
            list_blocking: false,
        };
        assert!(exec_override_impl(&args).is_ok());

        let rules_dir = crate::rules::rules_dir(repo.path());
        let session = governance_session::load("sess-cli-1", &rules_dir);
        assert_eq!(session.overrides.len(), 1);
        assert_eq!(
            session.overrides[0].reason.as_deref(),
            Some("reviewed and accepted")
        );
    }

    /// `--reason-code` is stored with the override, beside the local text.
    #[test]
    fn test_exec_override_impl_records_the_reason_code_flag() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = seed(&[("doc.md", OAUTH_DOC)]);

        let args = PlanCheckOverrideArgs {
            session: Some("sess-cli-code".to_string()),
            rules: vec![governance_session::key("doc", "R-A-001")],
            reason: Some("in a hurry".to_string()),
            reason_code: Some(crate::cli::args::OverrideReasonCodeArg::Accepted),
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
            claude_hook: false,
            via: OverrideVia::Prompt,
            gate: None,
            list_blocking: false,
        };
        assert!(exec_override_impl(&args).is_ok());

        let rules_dir = crate::rules::rules_dir(repo.path());
        let session = governance_session::load("sess-cli-code", &rules_dir);
        assert_eq!(
            session.overrides[0].reason_code,
            Some(governance_session::OverrideReasonCode::Accepted)
        );
        assert_eq!(session.overrides[0].reason.as_deref(), Some("in a hurry"));
    }

    fn hook_args(repo: &Path) -> PlanCheckOverrideArgs {
        PlanCheckOverrideArgs {
            session: None,
            rules: vec![],
            reason: None,
            reason_code: None,
            repo: Some(repo.to_path_buf()),
            rules_dir: None,
            claude_hook: true,
            via: OverrideVia::Prompt,
            gate: None,
            list_blocking: false,
        }
    }

    fn prompt_env(event: &str, session: &str, prompt: &str) -> String {
        serde_json::json!({"hook_event_name": event, "session_id": session, "prompt": prompt})
            .to_string()
    }

    /// An agent's shell command carries CLAUDECODE; piping a forged envelope
    /// into --claude-hook from there must be refused before stdin is read.
    #[test]
    fn test_claude_hook_refuses_under_an_agents_shell() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard::set("CLAUDECODE", "1");
        let err = exec_override(&hook_args(Path::new("."))).unwrap_err();
        assert!(matches!(err, ActualError::NotInteractive(_)), "{err:?}");
        assert!(err.to_string().contains("CLAUDECODE"), "{err}");
        assert!(err.to_string().contains("Nothing was overridden"), "{err}");
    }

    /// The mod's dialog path: `--via dialog` records `session_dialog`, and the
    /// output names the keys it recorded for the mod to read.
    #[test]
    fn test_override_hook_via_dialog_records_session_dialog() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        let k = governance_session::key("doc", "R-A-001");
        let mut session = governance_session::load("sess-d1", &rules_dir);
        session.plan.record_denial(&k);
        governance_session::store("sess-d1", &rules_dir, &session);

        let args = PlanCheckOverrideArgs {
            via: OverrideVia::Dialog,
            gate: None,
            list_blocking: false,
            ..hook_args(repo.path())
        };
        let out = exec_override_hook_with(
            &args,
            &prompt_env("UserPromptSubmit", "sess-d1", "/actual-cli:override all"),
        )
        .expect("hook output");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["overridden"], serde_json::json!([k]));
        let msg = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(msg.contains("in the override dialog"), "{msg}");
        assert!(!msg.contains("Retry the blocked action"), "{msg}");
        let session = governance_session::load("sess-d1", &rules_dir);
        assert_eq!(
            session.overrides[0].source,
            governance_session::OverrideSource::SessionDialog
        );
        let log = std::fs::read_to_string(governance_session::audit_log_path().unwrap()).unwrap();
        assert!(log.contains("\"source\":\"session_dialog\""), "{log}");
    }

    /// An override typed in a telemetry debug session is logged in that
    /// session's file, beside the gates' events.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_override_hook_in_debug_mode_logs_under_the_session_id() {
        use crate::telemetry::debug::{
            self,
            tests::{logged, FreshId},
        };
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _fresh = FreshId::new();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _no_telemetry = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
        let _debug = EnvGuard::set(debug::ENV, "1");
        let _id = EnvGuard::remove(debug::ID_ENV);
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        let k = governance_session::key("doc", "R-A-001");
        let mut session = governance_session::load("sess-debug-o1", &rules_dir);
        session.plan.record_denial(&k);
        governance_session::store("sess-debug-o1", &rules_dir, &session);

        exec_override_hook_with(
            &hook_args(repo.path()),
            &prompt_env(
                "UserPromptSubmit",
                "sess-debug-o1",
                "/actual-cli:override all",
            ),
        )
        .expect("hook output");

        let names: Vec<String> = logged("sess-debug-o1")
            .iter()
            .filter(|line| line["stream"] == "governance")
            .map(|line| line["event"]["event"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["plan_governance_rule_override"]);
    }

    /// The dialog's pick rides as the first word after the target: it is
    /// stored as the code, with no local text, and the reply names it.
    #[test]
    fn test_override_hook_stores_a_leading_reason_code() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        let k = governance_session::key("doc", "R-A-001");
        let mut session = governance_session::load("sess-c1", &rules_dir);
        session.plan.record_denial(&k);
        governance_session::store("sess-c1", &rules_dir, &session);

        let args = PlanCheckOverrideArgs {
            via: OverrideVia::Dialog,
            ..hook_args(repo.path())
        };
        let out = exec_override_hook_with(
            &args,
            &prompt_env(
                "UserPromptSubmit",
                "sess-c1",
                "/actual-cli:override all misread",
            ),
        )
        .expect("hook output");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["overridden"], serde_json::json!([k]));
        let msg = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(msg.contains("(reason: misread)"), "{msg}");
        let stored = &governance_session::load("sess-c1", &rules_dir).overrides[0];
        assert_eq!(
            stored.reason_code,
            Some(governance_session::OverrideReasonCode::Misread)
        );
        assert_eq!(stored.reason, None);
    }

    /// Typed text that names no code is stored as `other`, with every word
    /// kept on the machine.
    #[test]
    fn test_override_hook_stores_typed_text_as_other() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        let k = governance_session::key("doc", "R-A-001");
        let mut session = governance_session::load("sess-c2", &rules_dir);
        session.plan.record_denial(&k);
        governance_session::store("sess-c2", &rules_dir, &session);

        exec_override_hook_with(
            &hook_args(repo.path()),
            &prompt_env(
                "UserPromptSubmit",
                "sess-c2",
                "/actual-cli:override R-A-001 throwaway demo code",
            ),
        )
        .expect("hook output");
        let stored = &governance_session::load("sess-c2", &rules_dir).overrides[0];
        assert_eq!(
            stored.reason_code,
            Some(governance_session::OverrideReasonCode::Other)
        );
        assert_eq!(stored.reason.as_deref(), Some("throwaway demo code"));
    }

    /// A terminal override takes its code from `--reason-code`, falls back to
    /// `other` for a bare `--reason`, and records none for neither.
    #[test]
    fn test_terminal_reason_code() {
        use crate::cli::args::OverrideReasonCodeArg as Arg;
        use governance_session::OverrideReasonCode as Code;
        assert_eq!(
            terminal_reason_code(Some(Arg::Accepted), Some("in a hurry")),
            Some(Code::Accepted)
        );
        assert_eq!(
            terminal_reason_code(Some(Arg::NotHere), None),
            Some(Code::NotHere)
        );
        assert_eq!(terminal_reason_code(None, Some("why")), Some(Code::Other));
        assert_eq!(terminal_reason_code(None, None), None);
    }

    #[test]
    fn test_describe_reason() {
        use governance_session::OverrideReasonCode as Code;
        assert_eq!(describe_reason(None, None), "none given");
        assert_eq!(describe_reason(Some(Code::Misread), None), "misread");
        assert_eq!(
            describe_reason(Some(Code::Accepted), Some("in a hurry")),
            "accepted, in a hurry"
        );
        assert_eq!(describe_reason(Some(Code::Other), Some("typed")), "typed");
    }

    #[test]
    fn test_parse_override_prompt() {
        assert_eq!(parse_override_prompt("hello"), None);
        assert_eq!(parse_override_prompt("/overrides R-1"), None);
        assert_eq!(
            parse_override_prompt("/override R-A-001 it is fine, really"),
            Some(OverridePrompt {
                target: "R-A-001".into(),
                reason_code: Some(governance_session::OverrideReasonCode::Other),
                reason: Some("it is fine, really".into())
            })
        );
        assert_eq!(
            parse_override_prompt("  /actual-cli:override all "),
            Some(OverridePrompt {
                target: "all".into(),
                reason_code: None,
                reason: None
            })
        );
        assert_eq!(
            parse_override_prompt("/actual-cli:override all adr-wrong"),
            Some(OverridePrompt {
                target: "all".into(),
                reason_code: Some(governance_session::OverrideReasonCode::AdrWrong),
                reason: None
            })
        );
        assert_eq!(
            parse_override_prompt("/actual-cli:override R-A-001 accepted in a hurry"),
            Some(OverridePrompt {
                target: "R-A-001".into(),
                reason_code: Some(governance_session::OverrideReasonCode::Accepted),
                reason: Some("in a hurry".into())
            })
        );
        // Another plugin's override command is not ours to capture.
        assert_eq!(parse_override_prompt("/actual:override all"), None);
        assert_eq!(
            parse_override_prompt("/other-plugin:override R-A-001"),
            None
        );
        assert_eq!(
            parse_override_prompt("/override").unwrap().target,
            String::new()
        );
    }

    /// The in-session path: a `UserPromptSubmit` `/override` for a rule the
    /// gate denied is recorded without a terminal or a reason, tagged
    /// `session_prompt` with the denying gate, in both session and audit log.
    #[test]
    fn test_override_hook_records_denied_rule_without_reason() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        let k = governance_session::key("doc", "R-A-001");
        let mut session = governance_session::load("sess-h1", &rules_dir);
        session.plan.record_denial(&k);
        governance_session::store("sess-h1", &rules_dir, &session);

        let out = exec_override_hook_with(
            &hook_args(repo.path()),
            &prompt_env("UserPromptSubmit", "sess-h1", "/override R-A-001"),
        )
        .expect("hook output");
        assert!(
            out.contains("UserPromptSubmit") && out.contains(&k),
            "{out}"
        );

        let session = governance_session::load("sess-h1", &rules_dir);
        assert_eq!(session.overrides.len(), 1);
        assert_eq!(session.overrides[0].reason, None);
        assert_eq!(
            session.overrides[0].source,
            governance_session::OverrideSource::SessionPrompt
        );
        assert_eq!(
            session.overrides[0].gate,
            Some(governance_session::OverrideGate::Plan)
        );
        let log = std::fs::read_to_string(governance_session::audit_log_path().unwrap()).unwrap();
        assert!(log.contains("\"source\":\"session_prompt\""), "{log}");
        assert!(log.contains("\"gate\":\"plan\""), "{log}");
        assert!(log.contains("\"reason\":null"), "{log}");
    }

    #[test]
    fn test_override_hook_all_and_reason() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        let mut session = governance_session::load("sess-h2", &rules_dir);
        session
            .plan
            .record_denial(&governance_session::key("doc", "R-A-001"));
        session
            .diff
            .record_denial(&governance_session::key("doc", "R-A-002"));
        governance_session::store("sess-h2", &rules_dir, &session);

        let out = exec_override_hook_with(
            &hook_args(repo.path()),
            &prompt_env("UserPromptSubmit", "sess-h2", "/override all team approved"),
        )
        .unwrap();
        assert!(out.contains("via /actual-cli:override"), "{out}");
        let session = governance_session::load("sess-h2", &rules_dir);
        assert_eq!(session.overrides.len(), 2);
        assert!(session
            .overrides
            .iter()
            .all(|o| o.reason.as_deref() == Some("team approved")));
    }

    /// `/override all` covers only what is blocking now: a rule denied once
    /// and cleared in a later round is left on, since an override would turn
    /// it off for both gates for the rest of the session.
    #[test]
    fn test_override_hook_all_skips_a_rule_fixed_since_its_denial() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        let fixed = governance_session::key("doc", "R-A-001");
        let open = governance_session::key("doc", "R-A-002");
        let mut session = governance_session::load("sess-h3", &rules_dir);
        session.plan.record_denial(&fixed);
        // A later round denies only `open`: `fixed` was cleared or not judged.
        session.plan.begin_round();
        session.plan.record_denial(&open);
        governance_session::store("sess-h3", &rules_dir, &session);

        exec_override_hook_with(
            &hook_args(repo.path()),
            &prompt_env("UserPromptSubmit", "sess-h3", "/override all"),
        )
        .unwrap();
        let session = governance_session::load("sess-h3", &rules_dir);
        let keys: Vec<&str> = session.overrides.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(keys, [open.as_str()]);

        // With nothing left blocking, `all` refuses rather than overriding
        // the fixed rule.
        let out = exec_override_hook_with(
            &hook_args(repo.path()),
            &prompt_env("UserPromptSubmit", "sess-h3", "/override all"),
        )
        .unwrap();
        assert!(out.contains("no rules are currently blocking"), "{out}");
        assert_eq!(
            governance_session::load("sess-h3", &rules_dir)
                .overrides
                .len(),
            1
        );
    }

    /// With `--gate`, `all` covers only that gate's blocking rules: the
    /// mod's dialog answers one gate's denial, and a rule only the other gate
    /// is denying was never shown to the user who chose "Override all".
    #[test]
    fn test_override_hook_all_with_a_gate_covers_only_that_gate() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        let plan_only = governance_session::key("doc", "R-A-001");
        let impl_only = governance_session::key("doc", "R-A-002");
        let mut session = governance_session::load("sess-g1", &rules_dir);
        session.plan.record_denial(&plan_only);
        session.diff.record_denial(&impl_only);
        governance_session::store("sess-g1", &rules_dir, &session);

        let plan = PlanCheckOverrideArgs {
            via: OverrideVia::Dialog,
            gate: Some(OverrideGateArg::Plan),
            ..hook_args(repo.path())
        };
        let out = exec_override_hook_with(
            &plan,
            &prompt_env("UserPromptSubmit", "sess-g1", "/actual-cli:override all"),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["overridden"], serde_json::json!([plan_only]));

        // Nothing left blocking the plan: `all` refuses rather than reach
        // into the other gate.
        let out = exec_override_hook_with(
            &plan,
            &prompt_env("UserPromptSubmit", "sess-g1", "/actual-cli:override all"),
        )
        .unwrap();
        assert!(
            out.contains("no rules are currently blocking the plan"),
            "{out}"
        );

        let implementation = PlanCheckOverrideArgs {
            gate: Some(OverrideGateArg::Impl),
            ..plan
        };
        let out = exec_override_hook_with(
            &implementation,
            &prompt_env("UserPromptSubmit", "sess-g1", "/actual-cli:override all"),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["overridden"], serde_json::json!([impl_only]));

        // And the same refusal, worded for the changes, once nothing is left
        // blocking them.
        let out = exec_override_hook_with(
            &implementation,
            &prompt_env("UserPromptSubmit", "sess-g1", "/actual-cli:override all"),
        )
        .unwrap();
        assert!(
            out.contains("no rules are currently blocking the changes"),
            "{out}"
        );
    }

    /// `--list-blocking` prints the gate's blocking keys, the same set `all`
    /// with that gate records, and records nothing. No prompt is needed.
    #[test]
    fn test_override_hook_list_blocking_reads_one_gate_and_records_nothing() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        let plan_only = governance_session::key("doc", "R-A-001");
        let impl_only = governance_session::key("doc", "R-A-002");
        let mut session = governance_session::load("sess-l1", &rules_dir);
        session.plan.record_denial(&plan_only);
        session.diff.record_denial(&impl_only);
        governance_session::store("sess-l1", &rules_dir, &session);

        let list = |gate| {
            let args = PlanCheckOverrideArgs {
                gate: Some(gate),
                list_blocking: true,
                ..hook_args(repo.path())
            };
            let env =
                serde_json::json!({"hook_event_name": "UserPromptSubmit", "session_id": "sess-l1"});
            let out = exec_override_hook_with(&args, &env.to_string()).expect("hook output");
            serde_json::from_str::<serde_json::Value>(&out).unwrap()
        };
        assert_eq!(
            list(OverrideGateArg::Plan),
            serde_json::json!({"blocking": [plan_only]})
        );
        assert_eq!(
            list(OverrideGateArg::Impl),
            serde_json::json!({"blocking": [impl_only]})
        );
        assert!(governance_session::load("sess-l1", &rules_dir)
            .overrides
            .is_empty());

        // An overridden rule is no longer listed.
        exec_override_hook_with(
            &hook_args(repo.path()),
            &prompt_env("UserPromptSubmit", "sess-l1", "/override R-A-001"),
        )
        .unwrap();
        assert_eq!(
            list(OverrideGateArg::Plan),
            serde_json::json!({"blocking": []})
        );
    }

    /// Naming a rule no gate is blocking still overrides it -- a pre-emptive
    /// override is legitimate -- but the reply says it was not blocking, so
    /// the user knows they turned off a rule that was passing.
    #[test]
    fn test_override_hook_says_when_a_named_rule_was_not_blocking() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        let denied = governance_session::key("doc", "R-A-001");
        let passing = governance_session::key("doc", "R-A-002");
        let mut session = governance_session::load("sess-nb", &rules_dir);
        session.plan.record_denial(&denied);
        governance_session::store("sess-nb", &rules_dir, &session);

        let out = exec_override_hook_with(
            &hook_args(repo.path()),
            &prompt_env("UserPromptSubmit", "sess-nb", "/override R-A-001"),
        )
        .unwrap();
        assert!(!out.contains("no gate was blocking"), "{out}");

        let out = exec_override_hook_with(
            &hook_args(repo.path()),
            &prompt_env("UserPromptSubmit", "sess-nb", "/override R-A-002"),
        )
        .unwrap();
        assert!(
            out.contains(&format!(
                "no gate was blocking {passing} when it was overridden"
            )),
            "{out}"
        );
        assert_eq!(
            governance_session::load("sess-nb", &rules_dir)
                .overrides
                .len(),
            2
        );
    }

    /// A bare rule id that two documents define is ambiguous: nothing is
    /// overridden, and the reply names both full keys to choose from.
    #[test]
    fn test_override_hook_refuses_an_ambiguous_bare_rule_id() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = seed(&[("doc.md", OAUTH_DOC), ("other.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());

        let out = exec_override_hook_with(
            &hook_args(repo.path()),
            &prompt_env("UserPromptSubmit", "sess-amb", "/override R-A-001"),
        )
        .unwrap();
        assert!(out.contains("'R-A-001' is ambiguous"), "{out}");
        assert!(
            out.contains("doc::R-A-001") && out.contains("other::R-A-001"),
            "{out}"
        );
        assert!(!out.contains("\"overridden\""), "{out}");
        assert!(governance_session::load("sess-amb", &rules_dir)
            .overrides
            .is_empty());
    }

    /// A rule a gate denied whose document has since been removed resolves
    /// from the session but not the corpus: nothing is overridden, rather than
    /// recording a key no gate will ever match again.
    #[test]
    fn test_override_hook_refuses_a_denied_rule_gone_from_the_corpus() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        let gone = governance_session::key("removed", "R-GONE-001");
        let mut session = governance_session::load("sess-gone", &rules_dir);
        session.plan.record_denial(&gone);
        governance_session::store("sess-gone", &rules_dir, &session);

        let out = exec_override_hook_with(
            &hook_args(repo.path()),
            &prompt_env("UserPromptSubmit", "sess-gone", "/override R-GONE-001"),
        )
        .unwrap();
        assert!(out.contains("Nothing was overridden"), "{out}");
        assert!(out.contains("R-GONE-001"), "{out}");
        assert!(!out.contains("\"overridden\""), "{out}");
        assert!(governance_session::load("sess-gone", &rules_dir)
            .overrides
            .is_empty());
    }

    /// Anything but a `UserPromptSubmit` `/override` records nothing --
    /// specifically the tool-event shapes an agent's own actions produce.
    #[test]
    fn test_override_hook_ignores_non_prompt_events_and_unknown_rules() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        let args = hook_args(repo.path());
        for event in ["PreToolUse", "Stop", "PostToolUse"] {
            assert_eq!(
                exec_override_hook_with(&args, &prompt_env(event, "s", "/override R-A-001")),
                None
            );
        }
        assert_eq!(exec_override_hook_with(&args, "not json"), None);
        assert_eq!(
            exec_override_hook_with(&args, &prompt_env("UserPromptSubmit", "s", "just chatting")),
            None
        );
        let out = exec_override_hook_with(
            &args,
            &prompt_env("UserPromptSubmit", "s", "/override NOPE-1"),
        )
        .unwrap();
        assert!(out.contains("nothing was overridden"), "{out}");
        let out =
            exec_override_hook_with(&args, &prompt_env("UserPromptSubmit", "s", "/override all"))
                .unwrap();
        assert!(out.contains("no rules are currently blocking"), "{out}");
        let out = exec_override_hook_with(
            &args,
            &prompt_env("UserPromptSubmit", "s", "/actual-cli:override"),
        )
        .unwrap();
        assert!(
            out.contains("syntax: /actual-cli:override <rule-id|all> [code] [reason]"),
            "{out}"
        );
        assert!(governance_session::load("s", &rules_dir)
            .overrides
            .is_empty());
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
            session: Some("sess-cli-telemetry-1".to_string()),
            rules: vec![
                governance_session::key("doc", "R-A-001"),
                governance_session::key("doc", "R-A-002"),
            ],
            reason: Some("reviewed and accepted".to_string()),
            reason_code: None,
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
            claude_hook: false,
            via: OverrideVia::Prompt,
            gate: None,
            list_blocking: false,
        };

        let events =
            with_captured_plan_governance_events(|| assert!(exec_override_impl(&args).is_ok()));

        assert_eq!(events.len(), 2);
        for event in &events {
            // Overrides are a first-class event, distinct from check completions,
            // so override rate has a clean denominator.
            assert_eq!(
                event.event,
                crate::api::types::PlanGovernanceEventName::PlanGovernanceRuleOverride
            );
            let props = event.properties.as_ref().unwrap();
            assert_eq!(props.command.as_deref(), Some("check-override"));
            assert_eq!(
                props.decision,
                Some(crate::api::types::PlanGovernanceDecision::Allow)
            );
            assert_eq!(props.rule_source.as_deref(), Some("doc"));
            // The reason rides only as privacy-preserving summaries.
            assert_eq!(props.reason_len, Some("reviewed and accepted".len() as u32));
            assert_eq!(
                props.reason_length_bucket,
                Some(crate::api::types::PlanGovernanceReasonCategory::Medium)
            );
            // The hash of the typed text is no longer sent.
            let raw = serde_json::to_value(props).unwrap();
            assert!(raw.get("reason_hash").is_none());
            assert!(raw.get("reason_category").is_none());
            // The identity envelope is applied to override events too: logged-out
            // run (no creds) -> datetime_utc set, no user/org identity hashes.
            assert!(
                props.datetime_utc.as_deref().is_some_and(|s| !s.is_empty()),
                "override event must carry datetime_utc"
            );
            assert!(props.user_id_hash.is_none());
            assert!(props.org_id_hash.is_none());
            // The raw reason text must never appear anywhere in the payload.
            let serialized = serde_json::to_string(props).unwrap();
            assert!(
                !serialized.contains("reviewed and accepted"),
                "raw override reason must never be serialized"
            );
        }
        let rule_ids: std::collections::HashSet<_> = events
            .iter()
            .map(|e| e.properties.as_ref().unwrap().rule_id.clone().unwrap())
            .collect();
        assert_eq!(
            rule_ids,
            std::collections::HashSet::from(["R-A-001".to_string(), "R-A-002".to_string()])
        );
        // Same event name, install and timestamp -- only a per-event
        // insert_id keeps PostHog from deduplicating one override away.
        let insert_ids: std::collections::HashSet<_> =
            events.iter().map(|e| e.insert_id.clone()).collect();
        assert_eq!(insert_ids.len(), 2);
    }

    /// Authenticated override: stored credentials make the override events carry
    /// the hashed user/org identity (never the raw ids), same envelope the check
    /// events use — the fix for APR-001 P2.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_exec_override_impl_authenticated_carries_identity_envelope() {
        use crate::telemetry::identity::hash_identifier;
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        // A logged-in session: resolve() reads this and hashes the ids.
        std::fs::write(
            home.path().join("credentials.yaml"),
            "access_token: t\nrefresh_token: r\ntoken_type: Bearer\norganization_id: org-abc\nmember_id: m-1\nsubject: user-sub-xyz\n",
        )
        .unwrap();
        let repo = seed(&[("doc.md", OAUTH_DOC)]);

        let args = PlanCheckOverrideArgs {
            session: Some("sess-auth-override-1".to_string()),
            rules: vec![governance_session::key("doc", "R-A-001")],
            reason: Some("false positive: validated in middleware".to_string()),
            reason_code: None,
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
            claude_hook: false,
            via: OverrideVia::Prompt,
            gate: None,
            list_blocking: false,
        };

        let events =
            with_captured_plan_governance_events(|| assert!(exec_override_impl(&args).is_ok()));

        assert_eq!(events.len(), 1);
        let props = events[0].properties.as_ref().unwrap();
        // Authenticated -> hashed user/org identity present, matching the CLI hash.
        assert_eq!(
            props.user_id_hash.as_deref(),
            Some(hash_identifier("user", "user-sub-xyz").as_str())
        );
        assert_eq!(
            props.org_id_hash.as_deref(),
            Some(hash_identifier("org", "org-abc").as_str())
        );
        assert!(props.datetime_utc.as_deref().is_some_and(|s| !s.is_empty()));
        // No fallback org id on an authenticated run.
        assert_eq!(props.org_id, None);
        // Raw ids and raw reason must never be serialized.
        let serialized = serde_json::to_string(props).unwrap();
        assert!(!serialized.contains("user-sub-xyz"));
        assert!(!serialized.contains("org-abc"));
        assert!(!serialized.contains("false positive"));
    }

    /// Every plan/impl-check governance event carries the identity + timestamp
    /// envelope. In an isolated config there are no stored credentials, so the
    /// run is logged out: `datetime_utc` is always stamped, and no raw-id hashes
    /// (and certainly no raw ids) appear.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_send_governance_events_stamps_identity_envelope() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = seed(&[("doc.md", OAUTH_DOC)]);

        let events = with_captured_plan_governance_events(|| {
            send_governance_events(
                "plan-check",
                repo.path(),
                std::time::Instant::now(),
                crate::api::types::PlanGovernanceDecision::Allow,
                0,
                &[],
                None,
            );
        });

        // started + completed.
        assert_eq!(events.len(), 2);
        for event in &events {
            let props = event.properties.as_ref().unwrap();
            assert!(
                props.datetime_utc.as_deref().is_some_and(|s| !s.is_empty()),
                "every governance event must carry datetime_utc"
            );
            // Logged-out run (no creds in the isolated config dir).
            assert!(props.user_id_hash.is_none());
            assert!(props.org_id_hash.is_none());
        }
    }

    /// The hand-off spools every event before it starts the uploader, and sends
    /// nothing itself: the endpoint the config names gets no request.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_spool_governance_events_spools_every_event_before_the_uploader_starts() {
        use crate::api::types::{PlanGovernanceDecision, PlanGovernanceEvent};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let mut server = mockito::Server::new();
        let endpoint = server
            .mock("POST", "/plan-governance/record")
            .expect(0)
            .create();
        std::fs::write(
            home.path().join("config.yaml"),
            format!("api_url: \"{}\"\n", server.url()),
        )
        .unwrap();

        let ctx = crate::telemetry::plan_governance::EventContext::new("plan-check --claude-hook");
        let events = vec![
            ctx.started_event(),
            ctx.completed_event(PlanGovernanceDecision::Allow, 12.0, 0),
        ];
        let mut spooled_at_spawn = None;
        spool_governance_events(&events, || {
            spooled_at_spawn = Some(crate::telemetry::spool::load_all());
        });

        let ids = |list: &[PlanGovernanceEvent]| -> Vec<String> {
            list.iter().map(|e| e.insert_id.clone()).collect()
        };
        let spooled = spooled_at_spawn.expect("the uploader should have been started");
        assert_eq!(ids(&spooled), ids(&events));
        endpoint.assert();
    }

    /// A spool that can't take its lock drops the events without failing the
    /// check, and the uploader still starts for whatever earlier runs queued.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_spool_governance_events_drops_what_the_spool_turns_away() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        // A directory where the lock file belongs, so the lock can never be opened.
        std::fs::create_dir(home.path().join("scope-telemetry-spool.lock")).unwrap();

        let ctx = crate::telemetry::plan_governance::EventContext::new("check-override");
        let mut started = 0;
        spool_governance_events(&[ctx.started_event()], || started += 1);

        assert_eq!(started, 1);
        assert!(crate::telemetry::spool::load_all().is_empty());
    }

    /// The proxy rejects a batch over 100 events whole (see
    /// `plan_governance::MAX_EVENTS_PER_BATCH`), and `check-override`
    /// emits one event per `--rule` flag with no cap of its own -- so an
    /// override naming more than 100 rules at once must hand off every
    /// event, which the uploader then sends in chunks of at most 100.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_exec_override_impl_hands_off_every_event_past_the_batch_cap() {
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
            session: Some("sess-cli-chunking-1".to_string()),
            rules,
            reason: Some("bulk reviewed".to_string()),
            reason_code: None,
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
            claude_hook: false,
            via: OverrideVia::Prompt,
            gate: None,
            list_blocking: false,
        };

        let events =
            with_captured_plan_governance_events(|| assert!(exec_override_impl(&args).is_ok()));

        assert_eq!(
            events.len(),
            RULE_COUNT,
            "every override event must be handed off, not just the first 100"
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
            session: Some("sess-cli-opt-out-1".to_string()),
            rules: vec![governance_session::key("doc", "R-A-001")],
            reason: Some("reviewed and accepted".to_string()),
            reason_code: None,
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
            claude_hook: false,
            via: OverrideVia::Prompt,
            gate: None,
            list_blocking: false,
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

    /// With an OTLP output configured, an override also sends one
    /// `actual.gate.evaluated` record per ADR it names, with the outcome
    /// `overridden` and the gate that had denied it. A file without a v2
    /// header sends nothing, and no record carries the reason.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_exec_override_impl_sends_an_otlp_event_per_overridden_adr() {
        use super::super::brief_telemetry::tests::{attribute, take_captured};
        use super::super::override_telemetry::tests::v2_rule;

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _env: Vec<EnvGuard> = [
            "ACTUAL_NO_TELEMETRY",
            "ACTUAL_OTLP_LOGS_ENDPOINT",
            "ACTUAL_OTLP_FILE",
            "TRACEPARENT",
        ]
        .into_iter()
        .map(EnvGuard::remove)
        .collect();
        std::fs::write(
            home.path().join("config.yaml"),
            "telemetry:\n  otlp_file: /tmp/actual-otlp-override-test.jsonl\n",
        )
        .unwrap();
        let repo = seed(&[
            ("signing-a1.md", &v2_rule("adr-sign-1", Some("grp-sign"))),
            ("signing-a2.md", &v2_rule("adr-sign-2", Some("grp-sign"))),
            ("doc.md", OAUTH_DOC),
        ]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        let denied = governance_session::key("signing-a1", "R-adr-sign-1-1");
        let mut session = governance_session::load("sess-otlp-1", &rules_dir);
        session.record_denial(check::ArtifactKind::Plan, &denied);
        governance_session::store("sess-otlp-1", &rules_dir, &session);
        take_captured();

        let args = PlanCheckOverrideArgs {
            session: Some("sess-otlp-1".to_string()),
            rules: vec![
                denied,
                governance_session::key("signing-a2", "R-adr-sign-2-2"),
                governance_session::key("doc", "R-A-001"),
            ],
            reason: Some("reviewed and accepted".to_string()),
            reason_code: None,
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
            claude_hook: false,
            via: OverrideVia::Prompt,
            gate: None,
            list_blocking: false,
        };
        assert!(exec_override_impl(&args).is_ok());

        let records = take_captured();
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record["eventName"], crate::telemetry::otlp::GATE_EVALUATED);
        assert_eq!(attribute(record, "actual.gate.outcome"), "overridden");
        assert_eq!(attribute(record, "actual.session.id"), "sess-otlp-1");
        assert_eq!(attribute(record, "actual.agent.id"), "");
        assert_eq!(attribute(record, "actual.adr.ref"), "grp-sign");
        assert_eq!(
            attribute(record, "actual.adr.ids"),
            serde_json::json!(["adr-sign-1", "adr-sign-2"])
        );
        assert_eq!(
            attribute(record, "actual.gate.denied_types"),
            serde_json::json!(["planning"])
        );
        assert_eq!(
            attribute(record, "actual.override.blocking_types"),
            serde_json::json!(["planning"])
        );
        assert_eq!(attribute(record, "actual.override.source"), "terminal");
        // A --reason without a code is `other`, so its text is sent.
        assert_eq!(attribute(record, "actual.override.reason"), "other");
        assert_eq!(
            attribute(record, "actual.override.reason_text"),
            "reviewed and accepted"
        );

        // With a named code, the same words stay on the machine.
        let args = PlanCheckOverrideArgs {
            reason_code: Some(crate::cli::args::OverrideReasonCodeArg::Accepted),
            ..args
        };
        assert!(exec_override_impl(&args).is_ok());
        let records = take_captured();
        assert_eq!(records.len(), 1);
        assert_eq!(attribute(&records[0], "actual.override.reason"), "accepted");
        assert!(!records[0].to_string().contains("reviewed and accepted"));
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
            session: Some("sess-cli-2".to_string()),
            rules: vec![governance_session::key("doc", "R-A-001")],
            reason: Some("reviewed".to_string()),
            reason_code: None,
            repo: None,
            rules_dir: Some(rules_dir.path().to_path_buf()),
            claude_hook: false,
            via: OverrideVia::Prompt,
            gate: None,
            list_blocking: false,
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
            session: Some("sess-cli-unknown-rule".to_string()),
            rules: vec![governance_session::key("doc", "R-A-999")],
            reason: Some("reviewed".to_string()),
            reason_code: None,
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
            claude_hook: false,
            via: OverrideVia::Prompt,
            gate: None,
            list_blocking: false,
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

    /// Clap requires `--session` unless `--claude-hook` is given, but the
    /// terminal path still refuses without one rather than recording an
    /// override against no session.
    #[test]
    fn test_exec_override_impl_requires_a_session() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let repo = seed(&[("doc.md", OAUTH_DOC)]);

        let args = PlanCheckOverrideArgs {
            session: None,
            rules: vec![governance_session::key("doc", "R-A-001")],
            reason: None,
            reason_code: None,
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
            claude_hook: false,
            via: OverrideVia::Prompt,
            gate: None,
            list_blocking: false,
        };
        let err = exec_override_impl(&args).unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(_)), "{err:?}");
        assert!(err.to_string().contains("--session is required"), "{err}");
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
            session: Some("sess-cli-unknown-doc".to_string()),
            rules: vec![governance_session::key("no-such-doc", "R-A-001")],
            reason: Some("reviewed".to_string()),
            reason_code: None,
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
            claude_hook: false,
            via: OverrideVia::Prompt,
            gate: None,
            list_blocking: false,
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
            session: Some("sess-cli-missing-dir".to_string()),
            rules: vec![governance_session::key("doc", "R-A-001")],
            reason: Some("reviewed".to_string()),
            reason_code: None,
            repo: None,
            rules_dir: Some(missing),
            claude_hook: false,
            via: OverrideVia::Prompt,
            gate: None,
            list_blocking: false,
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
            session: Some("sess-cli-partial".to_string()),
            rules: vec![
                governance_session::key("doc", "R-A-001"),
                governance_session::key("doc", "R-A-999"),
            ],
            reason: Some("reviewed".to_string()),
            reason_code: None,
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
            claude_hook: false,
            via: OverrideVia::Prompt,
            gate: None,
            list_blocking: false,
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
            session: Some("sess-cli-multi-unknown".to_string()),
            rules: vec![
                governance_session::key("doc", "R-A-998"),
                governance_session::key("doc", "R-A-999"),
            ],
            reason: Some("reviewed".to_string()),
            reason_code: None,
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
            claude_hook: false,
            via: OverrideVia::Prompt,
            gate: None,
            list_blocking: false,
        };
        let err = exec_override_impl(&args).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("rules"), "message was: {message}");
        assert!(message.contains("were not found"), "message was: {message}");
        assert!(message.contains("doc::R-A-998"), "message was: {message}");
        assert!(message.contains("doc::R-A-999"), "message was: {message}");
    }

    /// How many events one plan-check run hands off with `config` in the
    /// config file, and whether it wrote a telemetry id.
    #[cfg(feature = "telemetry")]
    fn governance_events_under(config: &str) -> (usize, bool) {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _tel_on = EnvGuard::remove("ACTUAL_NO_TELEMETRY");
        std::fs::write(home.path().join("config.yaml"), config).unwrap();
        let repo = seed(&[("doc.md", OAUTH_DOC)]);

        let events = with_captured_plan_governance_events(|| {
            send_governance_events(
                "plan-check",
                repo.path(),
                std::time::Instant::now(),
                crate::api::types::PlanGovernanceDecision::Allow,
                0,
                &[],
                None,
            );
        });
        (events.len(), home.path().join("telemetry-id").exists())
    }

    /// `telemetry.enabled: false` in a config file that doesn't load: no
    /// event is built, and no telemetry id is written.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_send_governance_events_sends_nothing_when_the_config_cannot_load() {
        assert_eq!(
            governance_events_under(crate::testutil::UNLOADABLE_TELEMETRY_OPT_OUT),
            (0, false)
        );
    }

    /// The positive control: a config that loads sends started and completed.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_send_governance_events_sends_when_the_config_loads() {
        assert_eq!(
            governance_events_under(crate::testutil::LOADABLE_TELEMETRY_ON),
            (2, true)
        );
    }

    /// How many events one `check-override` call hands off with `config` in
    /// the config file, and whether it wrote a telemetry id.
    #[cfg(feature = "telemetry")]
    fn override_events_under(config: &str) -> (usize, bool) {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _tel_on = EnvGuard::remove("ACTUAL_NO_TELEMETRY");
        std::fs::write(home.path().join("config.yaml"), config).unwrap();
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let args = PlanCheckOverrideArgs {
            session: Some("sess-cli-config-1".to_string()),
            rules: vec![governance_session::key("doc", "R-A-001")],
            reason: Some("reviewed and accepted".to_string()),
            reason_code: None,
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
            claude_hook: false,
            via: OverrideVia::Prompt,
            gate: None,
            list_blocking: false,
        };

        let events =
            with_captured_plan_governance_events(|| assert!(exec_override_impl(&args).is_ok()));
        (events.len(), home.path().join("telemetry-id").exists())
    }

    /// `telemetry.enabled: false` in a config file that doesn't load: the
    /// command still succeeds, but no event is built and no telemetry id is
    /// written.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_exec_override_impl_sends_nothing_when_the_config_cannot_load() {
        assert_eq!(
            override_events_under(crate::testutil::UNLOADABLE_TELEMETRY_OPT_OUT),
            (0, false)
        );
    }

    /// The positive control: a config that loads sends one event per rule.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_exec_override_impl_sends_when_the_config_loads() {
        assert_eq!(
            override_events_under(crate::testutil::LOADABLE_TELEMETRY_ON),
            (1, true)
        );
    }

    /// Telemetry off in debug mode: the check's and the override's events
    /// are built and logged as not sent, none is handed to the spool, and no
    /// telemetry id is written.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_debug_mode_with_telemetry_off_logs_governance_events_and_writes_no_id() {
        use crate::telemetry::debug::{self, tests::logged};
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _no_telemetry = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
        let _debug = EnvGuard::set(debug::ENV, "1");
        let _id = EnvGuard::set(debug::ID_ENV, "governance-off");
        let repo = seed(&[("doc.md", OAUTH_DOC)]);

        let events = with_captured_plan_governance_events(|| {
            send_governance_events(
                "plan-check",
                repo.path(),
                std::time::Instant::now(),
                crate::api::types::PlanGovernanceDecision::Allow,
                0,
                &[],
                None,
            );
        });
        let args = PlanCheckOverrideArgs {
            session: Some("sess-debug-1".to_string()),
            rules: vec![governance_session::key("doc", "R-A-001")],
            reason: Some("reviewed and accepted".to_string()),
            reason_code: None,
            repo: Some(repo.path().to_path_buf()),
            rules_dir: None,
            claude_hook: false,
            via: OverrideVia::Prompt,
            gate: None,
            list_blocking: false,
        };
        let override_events =
            with_captured_plan_governance_events(|| assert!(exec_override_impl(&args).is_ok()));

        assert!(events.is_empty());
        assert!(override_events.is_empty());
        assert!(!home.path().join("telemetry-id").exists());
        let lines: Vec<_> = logged("governance-off")
            .into_iter()
            .filter(|line| line["stream"] == "governance")
            .collect();
        let names: Vec<&str> = lines
            .iter()
            .map(|line| line["event"]["event"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "plan_governance_check_started",
                "plan_governance_check_completed",
                "plan_governance_rule_override"
            ]
        );
        for line in &lines {
            assert_eq!(line["stage"], "not_sent");
            assert_eq!(line["reason"], "telemetry_off");
            assert!(uuid::Uuid::parse_str(line["event"]["distinct_id"].as_str().unwrap()).is_ok());
        }
    }

    // ── the selection's account ─────────────────────────────────────────

    /// [`run_pipeline_filtered`] for a plan, with no runner, so a round gets
    /// as far as choosing what the judge would see.
    fn filtered_without_a_runner(
        root: &Path,
        plan: &str,
        args: &PlanCheckArgs,
        kind: check::ArtifactKind,
    ) -> (Outcome, FilterTally) {
        let _no_claude = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/path/to/claude");
        let (outcome, tally) = run_pipeline_filtered(
            plan,
            root,
            &crate::rules::rules_dir(root),
            &args.check_knobs(),
            kind,
            false,
            &GovernanceSession::default(),
        )
        .unwrap();
        let tally = tally.expect("a round without a rank always has an account");
        assert_eq!(tally.accounted(), tally.index_size, "{tally:?}");
        (outcome, tally)
    }

    fn no_runner_args() -> PlanCheckArgs {
        let mut args = base_args();
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        args
    }

    /// Every rule file lands in one bucket: retired, matching nothing, or
    /// kept, with the kept file named by its ADR. The
    /// gates apply no floor yet, and say so.
    #[test]
    fn test_the_gate_account_counts_every_file_once() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let retired = v2_oauth("draft")
            .replace("token-signing-1c57", "token-signing-old")
            .replace("adr-oauth", "adr-old");
        let root = seed(&[
            ("cross-cutting-token-signing-1c57.md", &v2_oauth("accepted")),
            ("cross-cutting-token-signing-old.md", &retired),
            ("cross-cutting-terraform-2b2b.md", TERRAFORM_DOC),
        ]);

        let (outcome, tally) = filtered_without_a_runner(
            root.path(),
            "Sign access tokens with RS256",
            &no_runner_args(),
            check::ArtifactKind::Plan,
        );

        assert!(matches!(outcome, Outcome::NoRunner { .. }));
        assert_eq!(tally.index_size, 3);
        assert_eq!(tally.inactive, 1);
        assert_eq!(tally.no_match, 1);
        assert_eq!(tally.min_score, 0.0);
        assert_eq!(tally.kept_adrs(), vec![("adr-oauth", tally.kept[0].score)]);
    }

    /// A round that keeps nothing still has an account: a plan nothing
    /// matches, and a limit of zero, which caps everything that matched.
    #[test]
    fn test_a_round_that_keeps_nothing_is_accounted_for() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[
            ("cross-cutting-token-signing-1c57.md", OAUTH_DOC),
            ("cross-cutting-terraform-2b2b.md", TERRAFORM_DOC),
        ]);

        let (outcome, tally) = filtered_without_a_runner(
            root.path(),
            "Paint the garden fence",
            &no_runner_args(),
            check::ArtifactKind::Plan,
        );
        assert!(matches!(outcome, Outcome::NothingApplies));
        assert_eq!((tally.no_match, tally.kept.len()), (2, 0));

        let mut none = no_runner_args();
        none.limit = 0;
        let (outcome, tally) =
            filtered_without_a_runner(root.path(), "Sign tokens", &none, check::ArtifactKind::Plan);
        assert!(matches!(outcome, Outcome::NothingApplies));
        assert_eq!((tally.capped, tally.kept.len()), (1, 0));
    }

    /// A diff wholly outside a rule's root drops the rule by path.
    #[test]
    fn test_a_diff_outside_a_rules_root_counts_as_an_excluded_path() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let rooted = v2_oauth("accepted").replace(
            "root: .",
            "root: services/auth\n    paths:\n      - \"oauth/**\"",
        );
        let root = seed(&[("cross-cutting-token-signing-1c57.md", &rooted)]);
        let diff = "diff --git a/web/token.rs b/web/token.rs\nnew file mode 100644\nindex 0000000..1111111\n--- /dev/null\n+++ b/web/token.rs\n@@ -0,0 +1 @@\n+sign access tokens with RS256\n";

        let (outcome, tally) = filtered_without_a_runner(
            root.path(),
            diff,
            &no_runner_args(),
            check::ArtifactKind::Diff,
        );

        assert!(matches!(outcome, Outcome::NothingApplies));
        assert_eq!(tally.excluded_path, 1);
    }

    /// The candidate cap and stage 2's rank are counted from the selection:
    /// a candidate judged unrelated, and one the limit left out.
    #[test]
    fn test_a_ranked_selection_counts_the_unrelated_and_the_capped() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = seed(&[
            ("cross-cutting-token-signing-1c57.md", OAUTH_DOC),
            (
                "cross-cutting-key-rotation-3c3c.md",
                "# Rotate Signing Keys: Rotation\n\nThese rules are ALWAYS ACTIVE for signing key rotation.\n\n### Rules\n\n- **R-C-001** MUST: rotate signing keys.\n",
            ),
            (
                "cross-cutting-key-storage-4d4d.md",
                "# Store Signing Keys: Storage\n\nThese rules are ALWAYS ACTIVE for signing key storage.\n\n### Rules\n\n- **R-D-001** MUST: store signing keys in the vault.\n",
            ),
            ("cross-cutting-terraform-2b2b.md", TERRAFORM_DOC),
        ]);
        let index = scope::resolve_in(&crate::rules::rules_dir(root.path()), root.path(), true)
            .unwrap()
            .index;
        let prefiltered =
            select::prefilter(&index, &Query::new("Rotate and store signing keys"), 1, 30);
        let candidates = prefiltered.candidates();
        assert!(candidates.len() >= 3, "{}", candidates.len());
        let verdict = |at: usize, verdict: &str| scope::RankedVerdict {
            slug: candidates[at].slug.clone(),
            verdict: serde_json::from_value(serde_json::json!(verdict)).unwrap(),
            reason: String::new(),
        };
        let selection = prefiltered.apply(&[verdict(0, "unrelated"), verdict(1, "governs")]);

        let mut tally = prefiltered.tally(&selection);
        assert_eq!(tally.unrelated, 1);
        assert_eq!(tally.capped, candidates.len() - 2);
        tally.kept = vec![KeptFile::new(None, None, 1.0)];
        assert_eq!(tally.accounted(), tally.index_size, "{tally:?}");
    }

    /// Gathering sorts each selected file: one whose every rule the session
    /// already settled, one the judge's window has no room for, and the one
    /// it judges.
    #[test]
    fn test_gathering_counts_settled_and_windowed_files() {
        let mut many = "# Many Rules: Widget Handling\n\nThese rules are ALWAYS ACTIVE for widget handling.\n\n### Rules\n\n".to_string();
        for i in 0..MAX_RULES_JUDGED {
            many.push_str(&format!("- **R-X-{i:04}** MUST: rule number {i}.\n"));
        }
        let root = seed(&[
            ("many.md", &many),
            ("oauth.md", OAUTH_DOC),
            ("terraform.md", TERRAFORM_DOC),
        ]);
        let selected = |slug: &str| crate::rules::scope::SelectedRule {
            slug: slug.to_string(),
            relative_path: format!(".actual/rules/{slug}.md"),
            title: None,
            adr_id: None,
            candidate_group_id: None,
            root: None,
            score: 1.0,
            stage: scope::Stage::Prefilter,
            verdict: None,
            reason: String::new(),
        };
        let selection = Selection {
            plan: String::new(),
            paths: Vec::new(),
            indexed_documents: 3,
            limit: 3,
            selected: vec![selected("terraform"), selected("many"), selected("oauth")],
            stage2: Stage2::NotRequested,
        };
        let mut session = GovernanceSession::default();
        session.plan.cleared.insert(
            governance_session::key("terraform", "R-B-001"),
            "digest-1".to_string(),
        );

        let gathered = gather_rules(
            &selection,
            root.path(),
            &session,
            check::ArtifactKind::Plan,
            "digest-1",
        );

        assert_eq!(gathered.judged, vec![1]);
        assert_eq!((gathered.settled, gathered.windowed), (1, 1));
        assert_eq!(gathered.unusable, 0);
    }

    // ── gate resolution: one event per ADR when its block ends ─────────────

    /// One `actual.gate.resolved` record as (resolution, ADR ref, rounds,
    /// denials).
    #[cfg(feature = "telemetry")]
    type Resolved = (String, String, i64, i64);

    /// Run `body` with a scratch config whose telemetry writes to a file, so
    /// the gates build records, and nothing captured from before.
    #[cfg(feature = "telemetry")]
    fn with_telemetry_output(body: impl FnOnce(&Path)) {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _guards: Vec<EnvGuard> = [
            "ACTUAL_CONFIG",
            "ACTUAL_NO_TELEMETRY",
            "ACTUAL_OTLP_LOGS_ENDPOINT",
            "ACTUAL_OTLP_FILE",
            "TRACEPARENT",
            "CI",
            crate::telemetry::debug::ENV,
            crate::telemetry::debug::ID_ENV,
        ]
        .into_iter()
        .map(EnvGuard::remove)
        .collect();
        std::fs::write(
            home.path().join("config.yaml"),
            "telemetry:\n  otlp_file: /tmp/actual-otlp-resolved-test.jsonl\n",
        )
        .unwrap();
        super::super::brief_telemetry::tests::take_captured();
        body(home.path());
    }

    /// The resolution records captured since the last call, after checking
    /// the attributes every one of them shares for a plan-check round.
    #[cfg(feature = "telemetry")]
    fn resolved(session_id: &str, gate: &str, mechanism: &str) -> Vec<Resolved> {
        resolved_in(
            &super::super::brief_telemetry::tests::take_captured(),
            session_id,
            gate,
            mechanism,
        )
    }

    /// [`resolved`] over `records`.
    #[cfg(feature = "telemetry")]
    fn resolved_in(
        records: &[serde_json::Value],
        session_id: &str,
        gate: &str,
        mechanism: &str,
    ) -> Vec<Resolved> {
        let text = |record: &serde_json::Value, key: &str| {
            super::super::brief_telemetry::tests::attribute(record, key)
                .as_str()
                .unwrap()
                .to_string()
        };
        let int = |record: &serde_json::Value, key: &str| text(record, key).parse::<i64>().unwrap();
        records
            .iter()
            .filter(|record| record["eventName"] == crate::telemetry::otlp::GATE_RESOLVED)
            .map(|record| {
                assert_eq!(text(record, "actual.session.id"), session_id);
                assert_eq!(text(record, "actual.gate.type"), gate);
                assert_eq!(text(record, "actual.gate.mechanism"), mechanism);
                // In a test every round is one process, so one action id.
                assert_eq!(
                    text(record, "actual.gate.block_action_id"),
                    crate::telemetry::otlp::current_action_id()
                );
                (
                    text(record, "actual.gate.resolution"),
                    text(record, "actual.adr.ref"),
                    int(record, "actual.gate.rounds"),
                    int(record, "actual.gate.denials"),
                )
            })
            .collect()
    }

    #[cfg(feature = "telemetry")]
    fn ended(resolution: &str, adr_ref: &str, rounds: i64, denials: i64) -> Resolved {
        (resolution.to_string(), adr_ref.to_string(), rounds, denials)
    }

    /// A rule of `doc_slug` judged `verdict`.
    #[cfg(feature = "telemetry")]
    fn rule_of(doc_slug: &str, rule_id: &str, verdict: Verdict) -> CheckedRule {
        CheckedRule {
            doc_slug: doc_slug.to_string(),
            ..checked(rule_id, verdict, "span", "reason")
        }
    }

    /// A judged round's outcome for `verdicts`, where `signing-1` and
    /// `signing-2` are two files of the ADR group `grp-sign`, `rotation` is the
    /// ADR `adr-rotate`, and `legacy` has no v2 header.
    #[cfg(feature = "telemetry")]
    fn round_of(verdicts: Vec<CheckedRule>) -> Outcome {
        let header = |adr_id: &str, group: Option<&str>| AdrMeta {
            adr_id: Some(adr_id.to_string()),
            candidate_group_id: group.map(str::to_string),
            ..Default::default()
        };
        Outcome::Verdicts {
            selection: Selection {
                plan: "p".to_string(),
                paths: Vec::new(),
                indexed_documents: 1,
                limit: 10,
                selected: vec![],
                stage2: Stage2::NotRequested,
            },
            verdicts,
            runner_label: Some("claude-cli (sonnet)".to_string()),
            partial: None,
            headers: [
                ("signing-1", header("adr-sign-1", Some("grp-sign"))),
                ("signing-2", header("adr-sign-2", Some("grp-sign"))),
                ("rotation", header("adr-rotate", None)),
                ("doc", header("adr-oauth", None)),
            ]
            .into_iter()
            .map(|(slug, meta)| (slug.to_string(), meta))
            .collect(),
        }
    }

    /// Blocked, fixed and blocked again is two episodes, each `fixed` after
    /// two rounds and one denial, and each sent once, in the round that
    /// learned it. A session that never blocks sends none, and a round
    /// without a session keeps no episodes at all.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_each_block_episode_sends_one_resolution_when_it_is_fixed() {
        with_telemetry_output(|home| {
            let rules_dir = home.join("rules");
            let run = hook_run(check::ArtifactKind::Plan, Some("s-fix"), &rules_dir, 3);
            let mut session = GovernanceSession::default();
            let conflict = || round_of(vec![rule_of("rotation", "R-1", Verdict::Conflicting)]);
            let pass = || round_of(vec![rule_of("rotation", "R-1", Verdict::Followed)]);

            for (outcome, expected) in [
                (conflict(), vec![]),
                (conflict(), vec![]),
                (pass(), vec![ended("fixed", "adr-rotate", 3, 2)]),
                (pass(), vec![]),
                (conflict(), vec![]),
                (pass(), vec![ended("fixed", "adr-rotate", 2, 1)]),
            ] {
                finish_hook(&run, &mut session, outcome);
                assert_eq!(resolved("s-fix", "planning", "plan_deny"), expected);
            }
            assert!(governance_session::load("s-fix", &rules_dir)
                .plan
                .episodes
                .is_empty());

            let quiet = hook_run(check::ArtifactKind::Plan, Some("s-quiet"), &rules_dir, 3);
            let mut session = GovernanceSession::default();
            finish_hook(&quiet, &mut session, pass());
            finish_hook(&quiet, &mut session, pass());
            assert_eq!(resolved("s-quiet", "planning", "plan_deny"), vec![]);

            let direct = hook_run(check::ArtifactKind::Plan, None, &rules_dir, 3);
            let mut session = GovernanceSession::default();
            finish_hook(&direct, &mut session, conflict());
            assert!(session.plan.episodes.is_empty());
        });
    }

    /// A block the next decision doesn't judge followed ends `not_rechecked`,
    /// never `fixed`: a judged round that leaves its rule out, one that judges
    /// it not applicable, a round where every applicable rule was already
    /// settled, and a round where nothing applies.
    /// The Stop gate names itself and its mechanism.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_a_block_the_next_round_leaves_out_is_not_rechecked() {
        with_telemetry_output(|home| {
            let rules_dir = home.join("rules");
            let run = hook_run(check::ArtifactKind::Diff, Some("s-left"), &rules_dir, 3);
            let all_settled = || {
                let mut outcome = verdicts_outcome(Vec::new());
                if let Outcome::Verdicts { runner_label, .. } = &mut outcome {
                    *runner_label = None;
                }
                outcome
            };
            let left_out = || round_of(vec![rule_of("signing-1", "R-1", Verdict::Followed)]);
            let not_applicable =
                || round_of(vec![rule_of("rotation", "R-1", Verdict::NotApplicable)]);

            for next in [
                left_out(),
                not_applicable(),
                all_settled(),
                Outcome::NothingApplies,
            ] {
                let mut session = GovernanceSession::default();
                finish_hook(
                    &run,
                    &mut session,
                    round_of(vec![rule_of("rotation", "R-1", Verdict::Conflicting)]),
                );
                assert_eq!(resolved("s-left", "implementation", "stop_block"), vec![]);
                finish_hook(&run, &mut session, next);
                let rounds = governance_session::load("s-left", &rules_dir).diff.rounds;
                assert_eq!(
                    resolved("s-left", "implementation", "stop_block"),
                    vec![ended("not_rechecked", "adr-rotate", i64::from(rounds), 1)]
                );
                assert!(governance_session::load("s-left", &rules_dir)
                    .diff
                    .episodes
                    .is_empty());
                governance_session::store("s-left", &rules_dir, &GovernanceSession::default());
            }

            // A round that never decided ends nothing.
            let mut session = GovernanceSession::default();
            finish_hook(
                &run,
                &mut session,
                round_of(vec![rule_of("rotation", "R-1", Verdict::Conflicting)]),
            );
            finish_hook(
                &run,
                &mut session,
                Outcome::CheckFailed {
                    documents_selected: 1,
                    reason: "boom".into(),
                },
            );
            assert_eq!(resolved("s-left", "implementation", "stop_block"), vec![]);
        });
    }

    /// An ADR's episode spans its rules: it stays open while any of them
    /// blocks, counts every denial, and when they end differently reports the
    /// least resolved. A rule from a file without a v2 header has no ADR and
    /// opens nothing.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_an_adr_whose_rules_end_differently_reports_the_least_resolved() {
        with_telemetry_output(|home| {
            let rules_dir = home.join("rules");
            let run = hook_run(check::ArtifactKind::Plan, Some("s-roll"), &rules_dir, 3);
            let mut session = GovernanceSession::default();
            let both = || {
                round_of(vec![
                    rule_of("signing-1", "R-1", Verdict::Conflicting),
                    rule_of("signing-2", "R-1", Verdict::RequiresDecision),
                    rule_of("legacy", "R-1", Verdict::Conflicting),
                ])
            };

            finish_hook(&run, &mut session, both());
            assert_eq!(resolved("s-roll", "planning", "plan_deny"), vec![]);
            assert_eq!(
                session.plan.episodes.keys().collect::<Vec<_>>(),
                ["grp-sign"]
            );
            // One rule fixed, the other still blocking: still open.
            finish_hook(
                &run,
                &mut session,
                round_of(vec![
                    rule_of("signing-1", "R-1", Verdict::Followed),
                    rule_of("signing-2", "R-1", Verdict::Conflicting),
                ]),
            );
            assert_eq!(resolved("s-roll", "planning", "plan_deny"), vec![]);
            // The other left out: fixed and not rechecked reads not rechecked.
            finish_hook(
                &run,
                &mut session,
                round_of(vec![rule_of("signing-1", "R-1", Verdict::Followed)]),
            );
            let records = super::super::brief_telemetry::tests::take_captured();
            assert_eq!(
                resolved_in(&records, "s-roll", "planning", "plan_deny"),
                vec![ended("not_rechecked", "grp-sign", 3, 3)]
            );
            // Both files' ids, as the `breached` record for the ADR carries.
            let ids: Vec<serde_json::Value> = records
                .iter()
                .filter(|record| record["eventName"] == crate::telemetry::otlp::GATE_RESOLVED)
                .map(|record| {
                    super::super::brief_telemetry::tests::attribute(record, "actual.adr.ids")
                })
                .collect();
            assert_eq!(ids, [serde_json::json!(["adr-sign-1", "adr-sign-2"])]);
        });
    }

    /// A file of an ADR that first blocks in a later round of its episode
    /// adds its id to the episode.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_a_file_that_joins_an_open_block_adds_its_adr_id() {
        with_telemetry_output(|home| {
            let rules_dir = home.join("rules");
            let run = hook_run(check::ArtifactKind::Plan, Some("s-join"), &rules_dir, 3);
            let mut session = GovernanceSession::default();
            finish_hook(
                &run,
                &mut session,
                round_of(vec![rule_of("signing-1", "R-1", Verdict::Conflicting)]),
            );
            finish_hook(
                &run,
                &mut session,
                round_of(vec![
                    rule_of("signing-1", "R-1", Verdict::Conflicting),
                    rule_of("signing-2", "R-1", Verdict::Conflicting),
                ]),
            );
            assert_eq!(
                governance_session::load("s-join", &rules_dir).plan.episodes["grp-sign"].adr_ids,
                ["adr-sign-1", "adr-sign-2"]
            );
        });
    }

    /// A round without a session has no session file to keep an episode in,
    /// so settling it opens, ends and sends nothing.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_a_round_without_a_session_settles_no_episode() {
        with_telemetry_output(|home| {
            let rules_dir = home.join("rules");
            let run = hook_run(check::ArtifactKind::Plan, None, &rules_dir, 3);
            let verdicts = vec![rule_of("rotation", "R-1", Verdict::Conflicting)];
            let headers: BTreeMap<String, AdrMeta> = [(
                "rotation".to_string(),
                AdrMeta {
                    adr_id: Some("adr-rotate".to_string()),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect();
            let mut session = GovernanceSession::default();
            let denied: Vec<&CheckedRule> = verdicts.iter().collect();
            assert!(
                settle_episodes(&run, &mut session, &verdicts, &headers, &denied, &[]).is_empty()
            );
            assert!(session.plan.episodes.is_empty());
        });
    }

    /// A round with telemetry off drops the open block it finds and sends
    /// nothing, so once telemetry is back the block that opens again counts
    /// its rounds and denials from there, not across the gap.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_a_round_with_telemetry_off_drops_the_open_blocks() {
        with_telemetry_output(|home| {
            let rules_dir = home.join("rules");
            let run = hook_run(check::ArtifactKind::Plan, Some("s-off"), &rules_dir, 9);
            let mut session = GovernanceSession::default();
            let conflict = || round_of(vec![rule_of("rotation", "R-1", Verdict::Conflicting)]);
            let pass = || round_of(vec![rule_of("rotation", "R-1", Verdict::Followed)]);

            finish_hook(&run, &mut session, conflict());
            assert!(!governance_session::load("s-off", &rules_dir)
                .plan
                .episodes
                .is_empty());
            {
                let _off = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
                finish_hook(&run, &mut session, conflict());
                assert!(session.plan.episodes.is_empty());
                assert!(governance_session::load("s-off", &rules_dir)
                    .plan
                    .episodes
                    .is_empty());
            }
            assert_eq!(resolved("s-off", "planning", "plan_deny"), vec![]);

            finish_hook(&run, &mut session, conflict());
            finish_hook(&run, &mut session, pass());
            assert_eq!(
                resolved("s-off", "planning", "plan_deny"),
                vec![ended("fixed", "adr-rotate", 2, 1)]
            );
        });
    }

    /// Two overlapping rounds of one gate that both loaded the session while
    /// its block was open, and both judge it fixed, send its resolution once.
    /// A round that loaded the session before another round opened a block
    /// ends that block when it stores rather than dropping it.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_overlapping_rounds_of_one_gate_end_a_block_once() {
        with_telemetry_output(|home| {
            let rules_dir = home.join("rules");
            let run = hook_run(check::ArtifactKind::Plan, Some("s-race"), &rules_dir, 3);
            let conflict = |slug: &str| round_of(vec![rule_of(slug, "R-1", Verdict::Conflicting)]);
            let pass = || round_of(vec![rule_of("rotation", "R-1", Verdict::Followed)]);
            finish_hook(
                &run,
                &mut GovernanceSession::default(),
                conflict("rotation"),
            );
            assert_eq!(resolved("s-race", "planning", "plan_deny"), vec![]);

            let mut first = governance_session::load("s-race", &rules_dir);
            let mut second = first.clone();
            finish_hook(&run, &mut first, pass());
            finish_hook(&run, &mut second, pass());
            assert_eq!(
                resolved("s-race", "planning", "plan_deny"),
                vec![ended("fixed", "adr-rotate", 2, 1)]
            );

            let mut stale = governance_session::load("s-race", &rules_dir);
            finish_hook(
                &run,
                &mut governance_session::load("s-race", &rules_dir),
                conflict("signing-1"),
            );
            assert_eq!(resolved("s-race", "planning", "plan_deny"), vec![]);
            finish_hook(&run, &mut stale, pass());
            assert_eq!(
                resolved("s-race", "planning", "plan_deny"),
                vec![ended("not_rechecked", "grp-sign", 1, 1)]
            );
            assert!(governance_session::load("s-race", &rules_dir)
                .plan
                .episodes
                .is_empty());
        });
    }

    /// A block a later round leaves open is still found at the session's end
    /// when the note the opening round made of its rules directory was lost.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_a_round_that_leaves_a_block_open_notes_its_rules_dir() {
        with_telemetry_output(|home| {
            let rules_dir = home.join("rules");
            let run = hook_run(check::ArtifactKind::Plan, Some("s-note"), &rules_dir, 9);
            let mut session = GovernanceSession::default();
            let conflict = || round_of(vec![rule_of("rotation", "R-1", Verdict::Conflicting)]);
            finish_hook(&run, &mut session, conflict());
            std::fs::remove_dir_all(home.join("session-telemetry")).unwrap();
            finish_hook(&run, &mut session, conflict());
            super::super::brief_telemetry::tests::take_captured();

            super::super::session_end::report(
                &serde_json::json!({"session_id": "s-note", "hook_event_name": "SessionEnd", "reason": "clear"})
                    .to_string(),
            );

            assert_eq!(
                resolved("s-note", "planning", "plan_deny"),
                vec![ended("still_open", "adr-rotate", 2, 2)]
            );
        });
    }

    /// With telemetry off in debug mode, a block a gate opens is still found
    /// at the session's end, which logs it as `still_open` and sends nothing.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_debug_mode_logs_a_block_a_gate_left_open_at_the_end() {
        use crate::telemetry::debug::{
            self,
            tests::{logged, FreshId},
        };
        with_telemetry_output(|home| {
            let _fresh = FreshId::new();
            let _off = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
            let _debug = EnvGuard::set(debug::ENV, "1");
            debug::use_session("s-debug");
            let rules_dir = home.join("rules");
            let run = hook_run(check::ArtifactKind::Plan, Some("s-debug"), &rules_dir, 3);
            finish_hook(
                &run,
                &mut GovernanceSession::default(),
                round_of(vec![rule_of("rotation", "R-1", Verdict::Conflicting)]),
            );

            super::super::session_end::report(
                &serde_json::json!({"session_id": "s-debug", "hook_event_name": "SessionEnd", "reason": "clear"})
                    .to_string(),
            );

            assert!(super::super::brief_telemetry::tests::take_captured().is_empty());
            let ended: Vec<serde_json::Value> = logged("s-debug")
                .iter()
                .filter(|line| {
                    line["event"]["eventName"] == crate::telemetry::otlp::SESSION_ENDED
                        || line["event"]["eventName"] == crate::telemetry::otlp::GATE_RESOLVED
                })
                .map(|line| line["event"]["eventName"].clone())
                .collect();
            assert_eq!(
                ended,
                [
                    crate::telemetry::otlp::SESSION_ENDED,
                    crate::telemetry::otlp::GATE_RESOLVED
                ]
            );
        });
    }

    /// The round the limit lets through ends a block `round_limit`. A later
    /// round the limit lets through again blocked no call, so opens and ends
    /// nothing.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_the_round_limit_ends_a_block_once() {
        with_telemetry_output(|home| {
            let rules_dir = home.join("rules");
            let run = hook_run(check::ArtifactKind::Plan, Some("s-limit"), &rules_dir, 1);
            let mut session = GovernanceSession::default();
            let conflict = || round_of(vec![rule_of("rotation", "R-1", Verdict::Conflicting)]);

            for expected in [
                vec![],
                vec![ended("round_limit", "adr-rotate", 2, 1)],
                vec![],
            ] {
                finish_hook(&run, &mut session, conflict());
                assert_eq!(resolved("s-limit", "planning", "plan_deny"), expected);
            }
            assert!(session.plan.episodes.is_empty());
        });
    }

    /// An override ends the blocks it names `overridden`, in both gates, with
    /// no agent. An override of a rule with no open block sends no resolution.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_an_override_ends_the_blocks_it_names() {
        with_telemetry_output(|_| {
            let repo = seed(&[("doc.md", OAUTH_DOC)]);
            let rules_dir = crate::rules::rules_dir(repo.path());
            let conflict = || round_of(vec![rule_of("doc", "R-A-001", Verdict::Conflicting)]);
            let mut session = GovernanceSession::default();
            for kind in [check::ArtifactKind::Plan, check::ArtifactKind::Diff] {
                finish_hook(
                    &hook_run(kind, Some("sess-over"), &rules_dir, 3),
                    &mut session,
                    conflict(),
                );
            }
            super::super::brief_telemetry::tests::take_captured();

            let args = |rule: &str| PlanCheckOverrideArgs {
                session: Some("sess-over".to_string()),
                rules: vec![governance_session::key("doc", rule)],
                reason: None,
                reason_code: None,
                repo: Some(repo.path().to_path_buf()),
                rules_dir: None,
                claude_hook: false,
                via: OverrideVia::Prompt,
                gate: None,
                list_blocking: false,
            };
            assert!(exec_override_impl(&args("R-A-001")).is_ok());
            let records = super::super::brief_telemetry::tests::take_captured();
            let resolved: Vec<_> = records
                .iter()
                .filter(|record| record["eventName"] == crate::telemetry::otlp::GATE_RESOLVED)
                .map(|record| {
                    let attribute = |key| {
                        super::super::brief_telemetry::tests::attribute(record, key)
                            .as_str()
                            .unwrap()
                            .to_string()
                    };
                    assert_eq!(attribute("actual.agent.id"), "");
                    (
                        attribute("actual.gate.type"),
                        attribute("actual.gate.resolution"),
                        attribute("actual.adr.ref"),
                    )
                })
                .collect();
            let overridden = |gate: &str| {
                (
                    gate.to_string(),
                    "overridden".to_string(),
                    "adr-oauth".to_string(),
                )
            };
            assert_eq!(
                resolved,
                [overridden("planning"), overridden("implementation")]
            );

            assert!(exec_override_impl(&args("R-A-002")).is_ok());
            let records = super::super::brief_telemetry::tests::take_captured();
            assert!(records
                .iter()
                .all(|record| record["eventName"] != crate::telemetry::otlp::GATE_RESOLVED));
        });
    }

    /// `/override` in the session ends a block the same way.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_a_session_override_ends_the_block_it_names() {
        with_telemetry_output(|_| {
            let repo = seed(&[("doc.md", OAUTH_DOC)]);
            let rules_dir = crate::rules::rules_dir(repo.path());
            let mut session = GovernanceSession::default();
            finish_hook(
                &hook_run(check::ArtifactKind::Plan, Some("sess-slash"), &rules_dir, 3),
                &mut session,
                round_of(vec![rule_of("doc", "R-A-001", Verdict::Conflicting)]),
            );
            super::super::brief_telemetry::tests::take_captured();

            let env = serde_json::json!({
                "hook_event_name": "UserPromptSubmit",
                "session_id": "sess-slash",
                "prompt": "/override all",
            });
            exec_override_hook_with(&hook_args(repo.path()), &env.to_string())
                .expect("hook output");
            let resolutions: Vec<serde_json::Value> =
                super::super::brief_telemetry::tests::take_captured()
                    .iter()
                    .filter(|record| record["eventName"] == crate::telemetry::otlp::GATE_RESOLVED)
                    .map(|record| {
                        super::super::brief_telemetry::tests::attribute(
                            record,
                            "actual.gate.resolution",
                        )
                    })
                    .collect();
            assert_eq!(resolutions, [serde_json::json!("overridden")]);
        });
    }

    /// The `actual.gate.resolution` of each resolution record captured since
    /// the last call.
    #[cfg(feature = "telemetry")]
    fn resolutions() -> Vec<serde_json::Value> {
        super::super::brief_telemetry::tests::take_captured()
            .iter()
            .filter(|record| record["eventName"] == crate::telemetry::otlp::GATE_RESOLVED)
            .map(|record| {
                super::super::brief_telemetry::tests::attribute(record, "actual.gate.resolution")
            })
            .collect()
    }

    /// A Codex `--codex-ask` block that opens a resolution record for `doc`'s
    /// R-A-001, with the picker when `picker` is set.
    #[cfg(feature = "telemetry")]
    fn codex_block(session_id: &str, rules_dir: &Path, picker: bool) {
        let run = HookRun {
            picker,
            ..codex_ask_run(check::ArtifactKind::Diff, session_id, rules_dir)
        };
        finish_hook(
            &run,
            &mut GovernanceSession::default(),
            round_of(vec![rule_of("doc", "R-A-001", Verdict::Conflicting)]),
        );
        super::super::brief_telemetry::tests::take_captured();
    }

    /// `actual override` typed into Codex ends the block as an override does.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_a_codex_typed_override_ends_the_block() {
        with_telemetry_output(|_| {
            let _codex = no_codex_markers();
            let repo = seed(&[("doc.md", OAUTH_DOC)]);
            let rules_dir = crate::rules::rules_dir(repo.path());
            codex_block("sess-typed", &rules_dir, false);

            codex_prompt_reply(
                "sess-typed",
                Some("actual override all accepted"),
                repo.path(),
                &rules_dir,
            )
            .expect("a reply");
            assert_eq!(resolutions(), [serde_json::json!("overridden")]);
        });
    }

    /// An override chosen in the Codex picker ends the block once its reason
    /// is given, and not before.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_a_codex_picker_override_ends_the_block() {
        with_telemetry_output(|_| {
            let _codex = no_codex_markers();
            let repo = seed(&[("doc.md", OAUTH_DOC)]);
            let rules_dir = crate::rules::rules_dir(repo.path());
            codex_block("sess-picker", &rules_dir, true);
            let ask = governance_session::load("sess-picker", &rules_dir)
                .diff
                .pending
                .and_then(|pending| pending.ask)
                .expect("a picker question");
            let answer = |id: &str, label: &str| {
                codex_answer_reply(
                    "sess-picker",
                    Some(&answer_input(id)),
                    Some(&answer_output(id, label)),
                    repo.path(),
                    &rules_dir,
                )
                .expect("a reply")
            };

            answer(&format!("actual_choice_{}", ask.nonce), &ask.override_label);
            assert!(resolutions().is_empty());
            answer(
                &format!("actual_reason_{}", ask.nonce),
                "This ADR doesn't apply here",
            );
            assert_eq!(resolutions(), [serde_json::json!("overridden")]);
        });
    }

    // ── --codex-ask and the Codex prompt ──────────────────────────────────

    fn codex_ask_run<'a>(
        kind: check::ArtifactKind,
        session_id: &'a str,
        rules_dir: &'a Path,
    ) -> HookRun<'a> {
        HookRun {
            host: HookHost::Codex,
            command: "test --codex-hook",
            codex_ask: true,
            ..hook_run(kind, Some(session_id), rules_dir, 3)
        }
    }

    fn no_codex_markers() -> Vec<EnvGuard> {
        ["CODEX_THREAD_ID", "CODEX_SESSION_ID"]
            .into_iter()
            .map(EnvGuard::remove)
            .collect()
    }

    /// A `--codex-ask` deny keeps the conflicts, worded for the agent, for the
    /// next prompt; without the flag nothing is kept; a clean round drops it.
    #[test]
    fn test_codex_ask_deny_keeps_the_conflicts_until_a_clean_round() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let rules = tempdir().unwrap();
        let conflict = || {
            vec![checked(
                "R-A-002",
                Verdict::Conflicting,
                "log(key)",
                "logs it",
            )]
        };

        let run = codex_ask_run(check::ArtifactKind::Diff, "s-ask", rules.path());
        let mut session = GovernanceSession::default();
        finish_hook(&run, &mut session, verdicts_outcome(conflict()));
        let pending = governance_session::load("s-ask", rules.path()).diff.pending;
        let pending = pending.expect("conflicts kept for the next prompt").text;
        assert!(pending.contains("CONFLICT R-A-002"), "{pending}");
        assert!(!pending.contains("check-override"), "{pending}");

        let plain = HookRun {
            codex_ask: false,
            ..codex_ask_run(check::ArtifactKind::Diff, "s-block", rules.path())
        };
        finish_hook(
            &plain,
            &mut GovernanceSession::default(),
            verdicts_outcome(conflict()),
        );
        assert_eq!(
            governance_session::load("s-block", rules.path())
                .diff
                .pending,
            None
        );

        let mut session = governance_session::load("s-ask", rules.path());
        let clean = vec![checked("R-A-002", Verdict::NotApplicable, "", "ok")];
        finish_hook(&run, &mut session, verdicts_outcome(clean));
        assert_eq!(
            governance_session::load("s-ask", rules.path()).diff.pending,
            None
        );
    }

    /// The stop reason names the conflicts and both ways to answer.
    #[test]
    fn test_codex_ask_reason_offers_revise_or_override() {
        let rule = checked("R-A-002", Verdict::Conflicting, "log(key)", "logs it");
        let diff = codex_ask_reason(&[&rule], None, check::ArtifactKind::Diff);
        assert!(
            diff.starts_with(
                "Actual implementation governance stopped this turn: the changes conflict"
            ),
            "{diff}"
        );
        assert!(diff.contains("CONFLICT R-A-002"), "{diff}");
        assert!(diff.contains("`actual override all <code>`"), "{diff}");
        assert!(diff.contains(OVERRIDE_CODES), "{diff}");
        assert!(!diff.contains("Session:"), "{diff}");
        let plan = codex_ask_reason(&[&rule], None, check::ArtifactKind::Plan);
        assert!(plan.contains("the plan conflicts with"), "{plan}");
        assert!(plan.contains("To keep it,"), "{plan}");
    }

    /// The next prompt hands the agent the conflicts once, as Codex's
    /// `UserPromptSubmit` context.
    #[test]
    fn test_codex_prompt_hands_over_the_conflicts_once() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _codex = no_codex_markers();
        let rules = tempdir().unwrap();
        governance_session::update("s-p", rules.path(), |s| {
            s.diff.pending = Some(Pending::new("CONFLICT R-A-002 (MUST): logs it".to_string()));
        });

        let reply = codex_prompt_reply("s-p", Some("revise it"), rules.path(), rules.path())
            .expect("the conflicts");
        // Byte for byte: the plugin pins this prefix.
        assert!(
            reply.starts_with(
                r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":""#
            ),
            "{reply}"
        );
        let v: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(
            v.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["hookSpecificOutput"]
        );
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
        let context = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(context.contains("your changes conflict"), "{context}");
        assert!(context.contains("message is their answer"), "{context}");
        assert!(
            context.ends_with("CONFLICT R-A-002 (MUST): logs it"),
            "{context}"
        );
        assert_eq!(
            codex_prompt_reply("s-p", Some("again"), rules.path(), rules.path()),
            None
        );
    }

    /// `actual override all accepted` records the override, drops the waiting
    /// conflicts and ends the prompt with a confirmation for the user.
    #[test]
    fn test_codex_typed_override_records_and_stops_the_prompt() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _codex = no_codex_markers();
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        let k1 = governance_session::key("doc", "R-A-001");
        let k2 = governance_session::key("doc", "R-A-002");
        governance_session::update("s-o", &rules_dir, |s| {
            s.diff.record_denial(&k1);
            s.diff.record_denial(&k2);
            s.diff.pending = Some(Pending::new("conflicts".to_string()));
        });

        // One of two: the other still blocks, so the conflicts stay.
        let reply = codex_prompt_reply(
            "s-o",
            Some("actual override R-A-001 not-here"),
            repo.path(),
            &rules_dir,
        )
        .expect("a reply");
        let v: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(
            v.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["continue", "stopReason"]
        );
        assert_eq!(v["continue"], false);
        let text = v["stopReason"].as_str().unwrap();
        assert!(
            text.contains("overrode R-A-001 (reason: not-here)"),
            "{text}"
        );
        assert!(text.contains("Still blocking: R-A-002"), "{text}");
        assert!(governance_session::load("s-o", &rules_dir)
            .diff
            .pending
            .is_some());

        let reply = codex_prompt_reply(
            "s-o",
            Some("Actual Override all accepted"),
            repo.path(),
            &rules_dir,
        )
        .expect("a reply");
        assert!(
            reply.contains("overrode R-A-002 (reason: accepted)"),
            "{reply}"
        );
        assert!(!reply.contains("Still blocking"), "{reply}");
        let session = governance_session::load("s-o", &rules_dir);
        assert_eq!(session.diff.pending, None);
        assert_eq!(session.overrides.len(), 2);
        assert!(session
            .overrides
            .iter()
            .all(|o| o.source == governance_session::OverrideSource::SessionPrompt));
        assert_eq!(
            session.overrides[1].reason_code,
            Some(governance_session::OverrideReasonCode::Accepted)
        );
        // The reminder later rounds carry names the code, not "no reason".
        let reminder = override_reminder(&session).unwrap();
        assert!(
            reminder.contains("R-A-001: manually overridden (not-here)"),
            "{reminder}"
        );
        assert!(
            reminder.contains("R-A-002: manually overridden (accepted)"),
            "{reminder}"
        );
    }

    /// A message that only starts with the words is not an override, and an
    /// override naming nothing, or nothing blocking, records nothing.
    #[test]
    fn test_codex_typed_override_needs_a_target() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _codex = no_codex_markers();
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());

        assert_eq!(
            parse_codex_override_prompt("actual override is confusing"),
            None
        );
        assert_eq!(
            parse_codex_override_prompt("tell me about actual override all"),
            None
        );
        assert!(parse_codex_override_prompt("/override all").is_some());
        assert_eq!(
            codex_prompt_reply(
                "s-n",
                Some("actual override is confusing"),
                repo.path(),
                &rules_dir
            ),
            None
        );
        let bare =
            codex_prompt_reply("s-n", Some("actual override"), repo.path(), &rules_dir).unwrap();
        assert!(
            bare.contains("nothing was overridden: name a rule"),
            "{bare}"
        );
        let none = codex_prompt_reply("s-n", Some("actual override all"), repo.path(), &rules_dir)
            .unwrap();
        assert!(
            none.contains("nothing was overridden: no rules are currently blocking"),
            "{none}"
        );
        assert!(governance_session::load("s-n", &rules_dir)
            .overrides
            .is_empty());
    }

    /// The agent's own shell carries Codex's markers, so an override that
    /// arrives from it is refused, by the prompt path and `--claude-hook`.
    #[test]
    fn test_codex_markers_refuse_an_override() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _codex = no_codex_markers();
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        governance_session::update("s-f", &rules_dir, |s| {
            s.diff
                .record_denial(&governance_session::key("doc", "R-A-001"));
        });

        let _thread = EnvGuard::set("CODEX_THREAD_ID", "t-1");
        let reply = codex_prompt_reply("s-f", Some("actual override all"), repo.path(), &rules_dir)
            .unwrap();
        assert!(reply.contains("nothing was overridden"), "{reply}");
        assert!(reply.contains("CODEX_THREAD_ID"), "{reply}");
        assert!(governance_session::load("s-f", &rules_dir)
            .overrides
            .is_empty());

        let err = exec_override(&hook_args(repo.path())).unwrap_err();
        assert!(matches!(err, ActualError::NotInteractive(_)), "{err:?}");
        assert!(err.to_string().contains("CODEX_THREAD_ID"), "{err}");
    }

    // ── --codex-picker ────────────────────────────────────────────────────

    fn picker_run<'a>(session_id: &'a str, rules_dir: &'a Path) -> HookRun<'a> {
        HookRun {
            picker: true,
            ..codex_ask_run(check::ArtifactKind::Diff, session_id, rules_dir)
        }
    }

    /// The `request_user_input` arguments a picker deny or reply carries: the
    /// JSON after its last line break.
    fn picker_call(text: &str) -> serde_json::Value {
        let start = text.rfind("\n{").expect("arguments") + 1;
        serde_json::from_str(&text[start..]).unwrap()
    }

    fn answer_input(id: &str) -> serde_json::Value {
        serde_json::json!({"questions": [{"id": id, "header": "h", "question": "q", "options": []}]})
    }

    /// How Codex hands a hook the tool's result: the JSON text.
    fn answer_output(id: &str, answer: &str) -> serde_json::Value {
        serde_json::Value::String(
            serde_json::json!({"answers": {id: {"answers": [answer]}}}).to_string(),
        )
    }

    /// Two conflicts seeded as a picker deny would leave them, returning
    /// the nonce.
    fn seed_picker_deny(session: &str, rules_dir: &Path) -> String {
        let k1 = governance_session::key("doc", "R-A-001");
        let k2 = governance_session::key("doc", "R-A-002");
        governance_session::update(session, rules_dir, |s| {
            s.diff.record_denial(&k1);
            s.diff.record_denial(&k2);
            s.diff.pending = Some(Pending {
                text: "CONFLICT R-A-001\nCONFLICT R-A-002".to_string(),
                ask: Some(PickerAsk {
                    nonce: "abcd1234".to_string(),
                    stage: PickerStage::Choice,
                    digest: "digest-1".to_string(),
                    override_label: "Override all 2 rules".to_string(),
                }),
            });
        });
        "abcd1234".to_string()
    }

    fn context(reply: &str) -> String {
        assert!(
            reply.starts_with(
                r#"{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":""#
            ),
            "{reply}"
        );
        let v: serde_json::Value = serde_json::from_str(reply).unwrap();
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// With the picker, a conflict sends the agent back with the exact
    /// question to ask, and the question is remembered with its nonce.
    #[test]
    fn test_picker_deny_hands_the_agent_the_question() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let rules = tempdir().unwrap();
        let conflict = vec![
            checked("R-A-001", Verdict::Conflicting, "HS256", "signs with HS256"),
            checked("R-A-002", Verdict::Conflicting, "log(key)", "logs it"),
        ];
        let run = picker_run("s-pk", rules.path());
        finish_hook(
            &run,
            &mut GovernanceSession::default(),
            verdicts_outcome(conflict.clone()),
        );
        let pending = governance_session::load("s-pk", rules.path())
            .diff
            .pending
            .unwrap();
        let ask = pending.ask.expect("a picker question");
        assert_eq!(ask.stage, PickerStage::Choice);
        assert_eq!(ask.override_label, "Override all 2 rules");
        assert_eq!(ask.digest, "digest-1");
        assert_eq!(ask.nonce.len(), 8);

        let blocking: Vec<&CheckedRule> = conflict.iter().collect();
        let reason = codex_picker_reason(&blocking, None, check::ArtifactKind::Diff, &ask);
        assert!(reason.contains("CONFLICT R-A-001"), "{reason}");
        let call = picker_call(&reason);
        let q = &call["questions"][0];
        assert_eq!(q["id"], format!("actual_choice_{}", ask.nonce));
        assert_eq!(q["header"], "Code blocked");
        assert_eq!(
            q["question"],
            "Changes blocked by 2 rules: R-A-001, R-A-002. What should happen?"
        );
        assert_eq!(
            q["options"][0]["label"],
            "Revise the changes to conform (Recommended)"
        );
        assert_eq!(q["options"][1]["label"], "Override all 2 rules");
    }

    /// Revise drops the question and hands the agent the conflicts.
    #[test]
    fn test_picker_revise_answer() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _codex = no_codex_markers();
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        let nonce = seed_picker_deny("s-r", &rules_dir);
        let id = format!("actual_choice_{nonce}");
        let reply = codex_answer_reply(
            "s-r",
            Some(&answer_input(&id)),
            Some(&answer_output(
                &id,
                "Revise the changes to conform (Recommended)",
            )),
            repo.path(),
            &rules_dir,
        )
        .expect("a reply");
        let text = context(&reply);
        assert!(
            text.starts_with("The user chose to revise the changes"),
            "{text}"
        );
        assert!(text.ends_with("CONFLICT R-A-002"), "{text}");
        assert_eq!(
            governance_session::load("s-r", &rules_dir).diff.pending,
            None
        );
    }

    /// Override asks why with a second picker; the why answer records the
    /// override with its code and the picker as its source.
    #[test]
    fn test_picker_override_then_why() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _codex = no_codex_markers();
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        let nonce = seed_picker_deny("s-o2", &rules_dir);
        let choice = format!("actual_choice_{nonce}");
        let reply = codex_answer_reply(
            "s-o2",
            Some(&answer_input(&choice)),
            Some(&answer_output(&choice, "Override all 2 rules")),
            repo.path(),
            &rules_dir,
        )
        .unwrap();
        let call = picker_call(&context(&reply));
        let q = &call["questions"][0];
        let why = format!("actual_reason_{nonce}");
        assert_eq!(q["id"], why);
        assert_eq!(q["header"], "Why override");
        assert_eq!(q["options"][1]["label"], "This ADR doesn't apply here");
        assert!(governance_session::load("s-o2", &rules_dir)
            .overrides
            .is_empty());
        let stage = governance_session::load("s-o2", &rules_dir)
            .diff
            .pending
            .unwrap()
            .ask
            .unwrap()
            .stage;
        assert_eq!(stage, PickerStage::Reason);

        let reply = codex_answer_reply(
            "s-o2",
            Some(&answer_input(&why)),
            Some(&answer_output(&why, "This ADR doesn't apply here")),
            repo.path(),
            &rules_dir,
        )
        .unwrap();
        let text = context(&reply);
        assert!(
            text.starts_with("The user overrode R-A-001, R-A-002 in the picker (reason: not-here)"),
            "{text}"
        );
        let session = governance_session::load("s-o2", &rules_dir);
        assert_eq!(session.diff.pending, None);
        assert_eq!(session.overrides.len(), 2);
        assert!(session.overrides.iter().all(|o| o.source
            == governance_session::OverrideSource::SessionDialog
            && o.reason_code == Some(governance_session::OverrideReasonCode::NotHere)));
    }

    /// "accepted" typed under Other is the code; other words in the first
    /// question override nothing.
    #[test]
    fn test_picker_other_answers() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _codex = no_codex_markers();
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());

        let nonce = seed_picker_deny("s-ot", &rules_dir);
        let choice = format!("actual_choice_{nonce}");
        let reply = codex_answer_reply(
            "s-ot",
            Some(&answer_input(&choice)),
            Some(&answer_output(&choice, "use the existing helper instead")),
            repo.path(),
            &rules_dir,
        )
        .unwrap();
        let text = context(&reply);
        assert!(
            text.contains("\"use the existing helper instead\". No rule was overridden"),
            "{text}"
        );
        assert!(governance_session::load("s-ot", &rules_dir)
            .overrides
            .is_empty());

        let nonce = seed_picker_deny("s-acc", &rules_dir);
        governance_session::update("s-acc", &rules_dir, |s| {
            s.diff.pending.as_mut().unwrap().ask.as_mut().unwrap().stage = PickerStage::Reason;
        });
        let why = format!("actual_reason_{nonce}");
        codex_answer_reply(
            "s-acc",
            Some(&answer_input(&why)),
            Some(&serde_json::json!({"answers": {why.clone(): {"answers": ["accepted, shipping today"]}}})),
            repo.path(),
            &rules_dir,
        )
        .unwrap();
        let session = governance_session::load("s-acc", &rules_dir);
        assert_eq!(
            session.overrides[0].reason_code,
            Some(governance_session::OverrideReasonCode::Accepted)
        );
        assert_eq!(
            session.overrides[0].reason.as_deref(),
            Some("shipping today")
        );
    }

    /// An answer to some other question, an empty or aborted answer, or one
    /// arriving with the agent's shell markers says nothing and records
    /// nothing.
    #[test]
    fn test_picker_ignores_foreign_answers() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _codex = no_codex_markers();
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        let nonce = seed_picker_deny("s-f2", &rules_dir);
        let choice = format!("actual_choice_{nonce}");
        let other = "actual_choice_ffffffff";
        let ask = |input: &str, output: serde_json::Value| {
            codex_answer_reply(
                "s-f2",
                Some(&answer_input(input)),
                Some(&output),
                repo.path(),
                &rules_dir,
            )
        };
        assert_eq!(
            ask(other, answer_output(other, "Override all 2 rules")),
            None
        );
        assert_eq!(ask(&choice, answer_output(&choice, "  ")), None);
        assert_eq!(
            ask(
                &choice,
                serde_json::Value::String("aborted by user after 9.1s".into())
            ),
            None
        );
        {
            let _thread = EnvGuard::set("CODEX_THREAD_ID", "t-1");
            assert_eq!(
                ask(&choice, answer_output(&choice, "Override all 2 rules")),
                None
            );
        }
        let session = governance_session::load("s-f2", &rules_dir);
        assert!(session.overrides.is_empty());
        assert_eq!(
            session.diff.pending.unwrap().ask.unwrap().stage,
            PickerStage::Choice
        );
    }

    /// An unanswered question over the same work ends the turn with the text
    /// choices and is forgotten; different work, or no question, judges as
    /// usual.
    #[test]
    fn test_unanswered_picker_falls_back_to_the_text_stop() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let rules = tempdir().unwrap();
        seed_picker_deny("s-u", rules.path());
        let session = governance_session::load("s-u", rules.path());
        let diff = check::ArtifactKind::Diff;

        assert_eq!(
            codex_unanswered_picker(
                HookHost::Codex,
                diff,
                Some("s-u"),
                rules.path(),
                &session,
                "other-digest"
            ),
            None
        );
        assert_eq!(
            codex_unanswered_picker(
                HookHost::Claude,
                diff,
                Some("s-u"),
                rules.path(),
                &session,
                "digest-1"
            ),
            None
        );
        let reply = codex_unanswered_picker(
            HookHost::Codex,
            diff,
            Some("s-u"),
            rules.path(),
            &session,
            "digest-1",
        )
        .expect("the text stop");
        let v: serde_json::Value = serde_json::from_str(&reply).unwrap();
        let text = v["stopReason"].as_str().unwrap();
        assert!(text.contains("went unanswered"), "{text}");
        assert!(text.contains("CONFLICT R-A-002"), "{text}");
        assert!(text.contains("actual override all <code>"), "{text}");
        let after = governance_session::load("s-u", rules.path())
            .diff
            .pending
            .unwrap();
        assert_eq!(after.ask, None);
        assert!(
            after.text.contains("R-A-001"),
            "conflicts kept for the next prompt"
        );
    }

    /// A session file written before picker questions holds the conflicts as
    /// a bare string, and still loads.
    #[test]
    fn test_pending_loads_from_the_older_string_form() {
        let state: governance_session::LoopState =
            serde_json::from_str(r#"{"pending":"CONFLICT R-A-001"}"#).unwrap();
        assert_eq!(
            state.pending,
            Some(Pending::new("CONFLICT R-A-001".to_string()))
        );
    }

    // ── coverage for the picker's other branches ──────────────────────────

    /// The plan gate's picker words, its pending context and its gate.
    #[test]
    fn test_picker_and_pending_words_for_the_plan_gate() {
        let rule = checked("R-A-001", Verdict::Conflicting, "HS256", "signs with HS256");
        let ask = PickerAsk {
            nonce: "abcd1234".to_string(),
            stage: PickerStage::Choice,
            digest: "d".to_string(),
            override_label: "Override R-A-001".to_string(),
        };
        let reason = codex_picker_reason(&[&rule], None, check::ArtifactKind::Plan, &ask);
        assert!(reason.contains("the plan conflicts with"), "{reason}");
        let q = &picker_call(&reason)["questions"][0];
        assert_eq!(q["header"], "Plan blocked");
        assert_eq!(
            q["question"],
            "Plan blocked by 1 rule: R-A-001. What should happen?"
        );
        assert_eq!(
            q["options"][0]["label"],
            "Revise the plan to conform (Recommended)"
        );
        let context = codex_pending_context(check::ArtifactKind::Plan, "CONFLICT R-A-001");
        assert!(context.contains("your plan conflicts"), "{context}");
        assert!(context.contains("propose it again"), "{context}");
    }

    /// The why answer for a plan overrides what the plan gate is blocking;
    /// an unanswered why question says nothing was overridden.
    #[test]
    fn test_picker_why_for_a_plan_and_unanswered_why() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _codex = no_codex_markers();
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        let k = governance_session::key("doc", "R-A-001");
        let reason_ask = |digest: &str| PickerAsk {
            nonce: "abcd1234".to_string(),
            stage: PickerStage::Reason,
            digest: digest.to_string(),
            override_label: "Override R-A-001".to_string(),
        };
        governance_session::update("s-pw", &rules_dir, |s| {
            s.plan.record_denial(&k);
            s.plan.pending = Some(Pending {
                text: "CONFLICT R-A-001".to_string(),
                ask: Some(reason_ask("d")),
            });
        });
        let why = "actual_reason_abcd1234";
        let reply = codex_answer_reply(
            "s-pw",
            Some(&answer_input(why)),
            Some(&answer_output(why, "This ADR is wrong")),
            repo.path(),
            &rules_dir,
        )
        .unwrap();
        assert!(context(&reply).contains("overrode R-A-001 in the picker (reason: adr-wrong)"));
        let session = governance_session::load("s-pw", &rules_dir);
        assert_eq!(
            session.overrides[0].gate,
            Some(governance_session::OverrideGate::Plan)
        );

        governance_session::update("s-pw2", &rules_dir, |s| {
            s.plan.pending = Some(Pending {
                text: "CONFLICT R-A-001".to_string(),
                ask: Some(reason_ask("d")),
            });
        });
        let session = governance_session::load("s-pw2", &rules_dir);
        let reply = codex_unanswered_picker(
            HookHost::Codex,
            check::ArtifactKind::Plan,
            Some("s-pw2"),
            &rules_dir,
            &session,
            "d",
        )
        .unwrap();
        assert!(reply.contains("so nothing was overridden"), "{reply}");
    }

    /// A why answer with nothing left blocking, or with a blocking rule gone
    /// from the rule set, overrides nothing and says so.
    #[test]
    fn test_picker_why_that_cannot_be_recorded() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _codex = no_codex_markers();
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        let why = "actual_reason_abcd1234";
        let seed_why = |session: &str, denied: Option<&str>| {
            governance_session::update(session, &rules_dir, |s| {
                if let Some(key) = denied {
                    s.diff.record_denial(key);
                }
                s.diff.pending = Some(Pending {
                    text: "conflicts".to_string(),
                    ask: Some(PickerAsk {
                        nonce: "abcd1234".to_string(),
                        stage: PickerStage::Reason,
                        digest: "d".to_string(),
                        override_label: "Override R-A-001".to_string(),
                    }),
                });
            });
        };
        let answer = |session: &str| {
            codex_answer_reply(
                session,
                Some(&answer_input(why)),
                Some(&answer_output(why, "This ADR is wrong")),
                repo.path(),
                &rules_dir,
            )
            .unwrap()
        };

        seed_why("s-none", None);
        assert!(context(&answer("s-none"))
            .starts_with("Nothing was overridden: no rules are currently blocking"));

        seed_why("s-gone", Some("doc::R-Z-999"));
        let text = context(&answer("s-gone"));
        assert!(text.starts_with("Nothing was overridden:"), "{text}");
        assert!(text.contains("R-Z-999"), "{text}");
        assert!(governance_session::load("s-gone", &rules_dir)
            .overrides
            .is_empty());
    }

    /// A typed override while the plan gate is asking narrows `all` to the
    /// plan's rules; a rule that isn't blocking is overridden and noted; a
    /// blocking rule gone from the rule set is refused.
    #[test]
    fn test_codex_typed_override_other_paths() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let _codex = no_codex_markers();
        let repo = seed(&[("doc.md", OAUTH_DOC)]);
        let rules_dir = crate::rules::rules_dir(repo.path());
        let k1 = governance_session::key("doc", "R-A-001");

        governance_session::update("s-tp", &rules_dir, |s| {
            s.plan.record_denial(&k1);
            s.plan.pending = Some(Pending::new("CONFLICT R-A-001".to_string()));
        });
        let reply = codex_prompt_reply(
            "s-tp",
            Some("actual override all accepted"),
            repo.path(),
            &rules_dir,
        )
        .unwrap();
        assert!(reply.contains("overrode R-A-001"), "{reply}");
        assert_eq!(
            governance_session::load("s-tp", &rules_dir).plan.pending,
            None
        );

        let reply = codex_prompt_reply(
            "s-nb",
            Some("actual override R-A-002 not-here"),
            repo.path(),
            &rules_dir,
        )
        .unwrap();
        assert!(reply.contains("No check was blocking R-A-002"), "{reply}");

        governance_session::update("s-gone2", &rules_dir, |s| {
            s.diff.record_denial("doc::R-Z-999");
        });
        let reply = codex_prompt_reply(
            "s-gone2",
            Some("actual override all"),
            repo.path(),
            &rules_dir,
        )
        .unwrap();
        assert!(reply.contains("nothing was overridden"), "{reply}");
        assert!(reply.contains("R-Z-999"), "{reply}");
    }

    /// An override with neither a code nor words is reminded as one with no
    /// reason.
    #[test]
    fn test_override_reminder_without_any_reason() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let rules = tempdir().unwrap();
        governance_session::record_override(
            "s-nr",
            rules.path(),
            &["doc::R-A-001".to_string()],
            "",
        );
        let session = governance_session::load("s-nr", rules.path());
        let reminder = override_reminder(&session).unwrap();
        assert!(
            reminder.contains("doc::R-A-001: manually overridden (no reason given)"),
            "{reminder}"
        );
    }
}
