//! The two-stage selector: which rule documents govern a plan, and why.
//!
//! # Design
//!
//! Stage 1 ([`super::index`]) is the whole answer whenever it can be. It is
//! deterministic, offline and sub-millisecond, and when it hands back no more
//! candidates than the caller may keep there is nothing left to decide. Stage 2
//! ([`super::rank`]) is asked only when the prefiltered set is still larger than
//! the cap — the one case where something has to be discarded and the lexical
//! score is not a good enough reason to discard it.
//!
//! That trigger is also the latency contract. A selector that always called a
//! model would be unusable inside a synchronous hook; one that never did would
//! keep failing on plans that paraphrase their domain. Calling only over the
//! surplus makes the model cost proportional to the ambiguity actually present.
//!
//! Every path out of here produces a usable answer with a reason attached to
//! each selection. No runner configured, the runner down, the runner returning
//! nonsense, the runner keeping nothing: each degrades to the stage-1 answer
//! with the reason recorded in [`Stage2`], and none of them is an error. Only
//! a caller who cannot read the rule set at all gets an `Err`.
//!
//! Reproducibility is a property of the parts that are ours. The candidate set,
//! the order it is presented in, the prompt bytes, and the ordering rule
//! applied to whatever comes back are all deterministic functions of the plan
//! and the rule set. The model is allowed to partition the candidates; it is
//! not allowed to order them, invent them, or empty them.

use serde::Serialize;

use super::index::{Match, Query, ScopeIndex};
use super::rank::{self, Candidate, RankedVerdict, Verdict};
use crate::runner::structured::StructuredRunner;

/// How many candidates stage 1 hands to stage 2, by default.
///
/// Large enough that a rule stage 1 ranked poorly can still be promoted, small
/// enough that the prompt stays inside an interactive budget. Beyond about
/// thirty the ranker is reading more than it can weigh, and the tail is where
/// stage 1's own precision has already collapsed.
pub const DEFAULT_CANDIDATES: usize = 30;

/// Which stage put a rule in the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    /// Chosen by the deterministic index alone.
    Prefilter,
    /// Kept, and possibly promoted, by the runner-backed rank.
    Ranked,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Prefilter => "prefilter",
            Stage::Ranked => "ranked",
        }
    }
}

/// What became of stage 2 on this run.
///
/// Carried in the result rather than logged, because the difference between
/// "the ranker agreed with the prefilter" and "the ranker never ran" changes
/// how much the caller should trust the answer.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum Stage2 {
    /// The prefilter already fit inside the cap, so there was nothing to rank.
    NotNeeded { candidates: usize },
    /// The caller asked for stage 1 only.
    NotRequested,
    /// No runner is configured or available. The degraded path.
    Unavailable { reason: String },
    /// A runner was available and did not produce a usable rank.
    Failed { reason: String },
    /// The rank was applied. `unjudged` candidates were returned at their
    /// prefilter rank, below everything the ranker affirmed — so a selection
    /// under this status is not necessarily a fully judged one, and the counts
    /// are what say which.
    Applied {
        candidates: usize,
        governs: usize,
        related: usize,
        unrelated: usize,
        /// Candidates the ranker returned no usable verdict for. They keep
        /// their stage-1 standing, below everything the ranker affirmed.
        unjudged: usize,
    },
}

impl Stage2 {
    /// A one-line summary for a panel or a log.
    pub fn summary(&self) -> String {
        match self {
            Stage2::NotNeeded { candidates } => format!(
                "not needed — the prefilter returned {candidates} candidate(s), inside the cap"
            ),
            Stage2::NotRequested => "not requested".to_string(),
            Stage2::Unavailable { reason } => format!("unavailable — {reason}"),
            Stage2::Failed { reason } => format!("failed — {reason}"),
            Stage2::Applied {
                candidates,
                governs,
                related,
                unrelated,
                unjudged,
            } => {
                // A rank that judged fewer candidates than it skipped is
                // mostly prefilter padding wearing a `ranked` label, and the
                // count alone is easy to read past. Say which it was.
                let judged = governs + related + unrelated;
                let verb = if *unjudged > judged {
                    "partly ranked"
                } else {
                    "ranked"
                };
                format!(
                    "{verb} {candidates} candidate(s): {governs} governs, {related} related, \
                     {unrelated} unrelated, {unjudged} unjudged (kept at prefilter rank)"
                )
            }
        }
    }

    /// True when a model actually shaped this answer.
    pub fn is_applied(&self) -> bool {
        matches!(self, Stage2::Applied { .. })
    }
}

/// One rule the selector returned, and the reason it did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SelectedRule {
    pub slug: String,
    pub relative_path: String,
    pub title: Option<String>,
    /// The stage-1 score. Kept even on a ranked result so the two orderings can
    /// be compared.
    pub score: f64,
    pub stage: Stage,
    /// The ranker's verdict, absent when stage 2 did not run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    /// Why this rule was selected. Never empty.
    pub reason: String,
}

/// A complete selection, and an account of how it was reached.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Selection {
    pub plan: String,
    pub paths: Vec<String>,
    pub indexed_documents: usize,
    pub limit: usize,
    pub selected: Vec<SelectedRule>,
    pub stage2: Stage2,
}

/// The deterministic prefilter's output, before anything is discarded.
///
/// Held as its own type because the caller needs to know how many candidates
/// there were before deciding whether a model call is worth making.
#[derive(Debug, Clone)]
pub struct Prefiltered {
    plan: String,
    paths: Vec<String>,
    indexed_documents: usize,
    limit: usize,
    candidates: Vec<Match>,
    /// Per candidate, aligned with `candidates`: the document's verify globs
    /// and prose scope sentence, which stage 2 shows the ranker.
    evidence: Vec<(Vec<String>, Option<String>)>,
}

/// Run stage 1.
///
/// `limit` is how many rules the caller may keep; `candidate_cap` is how many
/// the prefilter retrieves before anything is discarded. The cap is raised to
/// at least `limit`, because retrieving fewer candidates than the caller is
/// allowed to keep would throw away results for no reason.
pub fn prefilter(
    index: &ScopeIndex,
    query: &Query,
    limit: usize,
    candidate_cap: usize,
) -> Prefiltered {
    let candidates = index.search(query, candidate_cap.max(limit));
    let evidence = candidates
        .iter()
        .map(|hit| {
            index
                .documents
                .iter()
                .find(|doc| doc.slug == hit.slug)
                .map(|doc| (doc.globs.clone(), doc.scope.clone()))
                .unwrap_or_default()
        })
        .collect();
    Prefiltered {
        plan: query.text.clone(),
        paths: query.all_paths(),
        indexed_documents: index.len(),
        limit,
        candidates,
        evidence,
    }
}

impl Prefiltered {
    /// How many candidates stage 1 found.
    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    /// True when there is more here than the caller may keep, which is the only
    /// condition under which stage 2 earns its latency.
    pub fn needs_rank(&self) -> bool {
        self.candidates.len() > self.limit
    }

    /// The candidates as the ranker sees them.
    pub fn candidates(&self) -> Vec<Candidate> {
        self.candidates
            .iter()
            .zip(&self.evidence)
            .map(|(hit, (globs, scope))| rank::candidate_from(hit, globs, scope.as_deref()))
            .collect()
    }

    pub fn plan(&self) -> &str {
        &self.plan
    }

    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    /// The stage-1 answer, with `stage2` recording why no rank shaped it.
    ///
    /// When the prefilter already fits inside the cap the caller's `stage2` is
    /// overridden with [`Stage2::NotNeeded`]: a run where no model was needed
    /// should not be reported as one where a model was missing.
    pub fn finish(&self, stage2: Stage2) -> Selection {
        let stage2 = if self.needs_rank() {
            stage2
        } else {
            Stage2::NotNeeded {
                candidates: self.candidates.len(),
            }
        };
        let selected = self
            .candidates
            .iter()
            .take(self.limit)
            .map(|hit| SelectedRule {
                slug: hit.slug.clone(),
                relative_path: hit.relative_path.clone(),
                title: hit.title.clone(),
                score: hit.score,
                stage: Stage::Prefilter,
                verdict: None,
                reason: prefilter_reason(hit),
            })
            .collect();
        self.selection(selected, stage2)
    }

    /// Stage 2 over this candidate set, when it is warranted.
    ///
    /// **This is the only place the stage-2 trigger is decided.** Every caller
    /// that has a runner goes through here — the `rules select` command,
    /// `rules eval --rank`, and [`select`] — so a measurement cannot report a
    /// number for a path the shipped command does not take. An earlier version
    /// had the gate in two of the three and the tests pointed at the third.
    ///
    /// Never fails: a runner that is down, slow or wrong produces the stage-1
    /// answer with the reason recorded, because a degraded selection is the
    /// acceptance criterion and an error is not.
    pub async fn rank_with<R: StructuredRunner>(
        &self,
        runner: &R,
        model_override: Option<&str>,
        max_budget_usd: Option<f64>,
    ) -> Selection {
        if !self.needs_rank() {
            // `finish` rewrites this to `NotNeeded`, which is the honest
            // status: no model was wanted, rather than none being available.
            return self.finish(Stage2::NotRequested);
        }
        match rank::rank(
            runner,
            self.plan(),
            self.paths(),
            &self.candidates(),
            model_override,
            max_budget_usd,
        )
        .await
        {
            Ok(verdicts) => self.apply(&verdicts),
            Err(e) => {
                tracing::warn!("stage-2 rank failed, falling back to the prefilter: {e}");
                self.finish(Stage2::Failed {
                    reason: e.to_string(),
                })
            }
        }
    }

    /// Apply `verdicts` to the candidate set.
    ///
    /// The ranker partitions; this function orders. A candidate judged
    /// `unrelated` is dropped, one judged `governs` outranks one judged
    /// `related`, and one the ranker did not judge falls below both — but
    /// within every group the deterministic stage-1 order is preserved
    /// unchanged, so the same verdicts always produce the same list.
    pub fn apply(&self, verdicts: &[RankedVerdict]) -> Selection {
        let verdict_for = |slug: &str| verdicts.iter().find(|v| v.slug == slug);

        let mut kept: Vec<(u8, usize, SelectedRule)> = Vec::new();
        let (mut governs, mut related, mut unrelated, mut unjudged) = (0, 0, 0, 0);

        for (position, hit) in self.candidates.iter().enumerate() {
            let judged = verdict_for(&hit.slug);
            let (tier, verdict) = match judged.map(|v| v.verdict) {
                Some(Verdict::Governs) => {
                    governs += 1;
                    (0u8, Some(Verdict::Governs))
                }
                Some(Verdict::Related) => {
                    related += 1;
                    (1, Some(Verdict::Related))
                }
                Some(Verdict::Unrelated) => {
                    unrelated += 1;
                    continue;
                }
                None => {
                    unjudged += 1;
                    (2, None)
                }
            };
            // A ranker that returns a blank reason still owes the caller one,
            // so the stage-1 evidence stands in rather than an empty line.
            let reason = judged
                .map(|v| v.reason.trim())
                .filter(|reason| !reason.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| prefilter_reason(hit));
            kept.push((
                tier,
                position,
                SelectedRule {
                    slug: hit.slug.clone(),
                    relative_path: hit.relative_path.clone(),
                    title: hit.title.clone(),
                    score: hit.score,
                    stage: if verdict.is_some() {
                        Stage::Ranked
                    } else {
                        Stage::Prefilter
                    },
                    verdict,
                    reason,
                },
            ));
        }

        kept.sort_by_key(|(tier, position, _)| (*tier, *position));
        let selected: Vec<SelectedRule> = kept
            .into_iter()
            .take(self.limit)
            .map(|(_, _, rule)| rule)
            .collect();

        self.selection(
            selected,
            Stage2::Applied {
                candidates: self.candidates.len(),
                governs,
                related,
                unrelated,
                unjudged,
            },
        )
    }

    fn selection(&self, selected: Vec<SelectedRule>, stage2: Stage2) -> Selection {
        Selection {
            plan: self.plan.clone(),
            paths: self.paths.clone(),
            indexed_documents: self.indexed_documents,
            limit: self.limit,
            selected,
            stage2,
        }
    }
}

/// Both stages, for a caller that has no reason to hold the prefilter.
///
/// A convenience wrapper over [`prefilter`] and [`Prefiltered::rank_with`],
/// which is where the behaviour lives. A caller that needs the candidate count
/// before deciding anything — every CLI path does, to resolve a runner only
/// when one will be used — should call those two directly.
pub async fn select<R: StructuredRunner>(
    index: &ScopeIndex,
    query: &Query,
    limit: usize,
    candidate_cap: usize,
    runner: &R,
    model_override: Option<&str>,
    max_budget_usd: Option<f64>,
) -> Selection {
    prefilter(index, query, limit, candidate_cap)
        .rank_with(runner, model_override, max_budget_usd)
        .await
}

/// Terms named in a reason line. Past a handful they stop being evidence and
/// start being the query echoed back.
const MAX_REASON_TERMS: usize = 4;

/// The deterministic reason a rule was prefiltered, in words.
///
/// Built from the index's own attribution rather than restated, so the reason
/// printed without `--explain` and the evidence printed with it can never
/// disagree. It is what makes the degraded path an answer rather than a bare
/// list: even with no runner in sight, every selection says why.
pub fn prefilter_reason(hit: &Match) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(glob) = hit.matched_globs.first() {
        parts.push(format!(
            "verify path `{}` {} `{}`",
            glob.glob,
            if glob.exact { "matches" } else { "contains" },
            glob.query_path
        ));
    }

    let fields: Vec<&str> = hit
        .contributions
        .iter()
        .filter(|c| !c.matched.is_empty())
        .map(|c| c.field.as_str())
        .take(2)
        .collect();
    let mut terms: Vec<&str> = Vec::new();
    for contribution in &hit.contributions {
        for term in &contribution.matched {
            if terms.len() < MAX_REASON_TERMS && !terms.contains(&term.as_str()) {
                terms.push(term);
            }
        }
    }
    if !terms.is_empty() {
        parts.push(format!(
            "{} {} on {}",
            fields.join(" and "),
            if fields.len() == 1 {
                "matches"
            } else {
                "match"
            },
            terms.join(", ")
        ));
    }

    if parts.is_empty() {
        return "ranked by the scope index; no single signal carried it".to_string();
    }
    parts.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::rules::scope::index::{Field, FieldContribution, GlobMatch};
    use crate::rules::{load_rule_set, rules_dir};

    use tempfile::{tempdir, TempDir};

    const OAUTH: &str = "# Sign With Asymmetric Keys: Token Signing\n\nThese rules are ALWAYS ACTIVE for OAuth token signing in `services/auth/oauth/`.\n\n### Rules\n\n- **R-A-001** MUST: sign with RS256.\n\n### Verify\n\n```bash\ngrep -r \"jwt.sign\" services/auth/oauth/ --include=\"*.ts\"\n```\n";
    const REVOCATION: &str = "# Check Revocation: Token Verification\n\nThese rules are ALWAYS ACTIVE for OAuth token verification.\n\n### Rules\n\n- **R-B-001** MUST: check revocation.\n";
    const EXPIRY: &str = "# Bound Expiration: Token Lifetime\n\nThese rules are ALWAYS ACTIVE for OAuth token expiration.\n\n### Rules\n\n- **R-C-001** MUST: bound expiry.\n";
    const TERRAFORM: &str = "# Pin Providers: Terraform\n\nThese rules are ALWAYS ACTIVE for Terraform configuration in `infra/terraform/`.\n\n### Rules\n\n- **R-D-001** MUST: pin providers.\n";

    fn corpus() -> (TempDir, ScopeIndex) {
        let root = tempdir().unwrap();
        let dir = rules_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        for (name, body) in [
            ("cross-cutting-token-signing-1c57.md", OAUTH),
            ("cross-cutting-token-revocation-963a.md", REVOCATION),
            ("cross-cutting-token-expiry-b555.md", EXPIRY),
            ("cross-cutting-terraform-c340.md", TERRAFORM),
        ] {
            std::fs::write(dir.join(name), body).unwrap();
        }
        let report = load_rule_set(root.path()).unwrap();
        let index = ScopeIndex::build(&report, root.path(), "test".to_string());
        (root, index)
    }

    fn token_query() -> Query {
        Query::new("Rotate OAuth tokens and check revocation on verification")
    }

    /// A runner that answers with a fixed value, or fails.
    struct FakeRunner {
        answer: Result<serde_json::Value, &'static str>,
        /// The prompt it was handed, for asserting stage 2 was reached at all.
        seen: std::sync::Mutex<Option<String>>,
    }

    impl FakeRunner {
        fn ok(answer: serde_json::Value) -> Self {
            Self {
                answer: Ok(answer),
                seen: std::sync::Mutex::new(None),
            }
        }

        fn failing() -> Self {
            Self {
                answer: Err("runner is down"),
                seen: std::sync::Mutex::new(None),
            }
        }

        fn was_called(&self) -> bool {
            self.seen.lock().unwrap().is_some()
        }
    }

    impl StructuredRunner for FakeRunner {
        async fn run_structured_json(
            &self,
            prompt: &str,
            _schema: &str,
            _model_override: Option<&str>,
            _max_budget_usd: Option<f64>,
        ) -> Result<serde_json::Value, crate::error::ActualError> {
            *self.seen.lock().unwrap() = Some(prompt.to_string());
            self.answer
                .clone()
                .map_err(|message| crate::error::ActualError::RunnerFailed {
                    message: message.to_string(),
                    stderr: String::new(),
                })
        }
    }

    fn verdicts(entries: &[(&str, &str, &str)]) -> serde_json::Value {
        serde_json::json!({
            "verdicts": entries
                .iter()
                .map(|(slug, verdict, reason)| serde_json::json!({
                    "slug": slug, "verdict": verdict, "reason": reason,
                }))
                .collect::<Vec<_>>()
        })
    }

    // ── stage 1 ─────────────────────────────────────────────────────────

    #[test]
    fn test_prefilter_retrieves_at_least_the_cap() {
        let (_root, index) = corpus();
        // A candidate cap below the limit would discard results the caller is
        // allowed to keep, so it is raised to the limit.
        let prefiltered = prefilter(&index, &token_query(), 3, 1);
        assert!(prefiltered.len() >= 3);
    }

    #[test]
    fn test_prefilter_answer_carries_a_reason_for_every_selection() {
        let (_root, index) = corpus();
        let selection = prefilter(&index, &token_query(), 2, 10).finish(Stage2::NotRequested);
        assert_eq!(selection.selected.len(), 2);
        for rule in &selection.selected {
            assert!(!rule.reason.is_empty(), "{} has no reason", rule.slug);
            assert_eq!(rule.stage, Stage::Prefilter);
            assert!(rule.verdict.is_none());
        }
    }

    #[test]
    fn test_prefilter_is_reproducible_for_the_same_plan_and_rule_set() {
        let (_root, index) = corpus();
        let first = prefilter(&index, &token_query(), 3, 10).finish(Stage2::NotRequested);
        let second = prefilter(&index, &token_query(), 3, 10).finish(Stage2::NotRequested);
        assert_eq!(first, second);
    }

    /// A prefilter that already fits inside the cap has nothing for stage 2 to
    /// do, and must not be reported as one where a runner was missing.
    #[test]
    fn test_finish_reports_not_needed_when_the_prefilter_fits_the_cap() {
        let (_root, index) = corpus();
        let prefiltered = prefilter(&index, &token_query(), 50, 50);
        assert!(!prefiltered.needs_rank());
        let selection = prefiltered.finish(Stage2::Unavailable {
            reason: "no runner".to_string(),
        });
        assert!(matches!(selection.stage2, Stage2::NotNeeded { .. }));
        assert!(selection.stage2.summary().contains("not needed"));
        assert!(!selection.stage2.is_applied());
    }

    #[test]
    fn test_finish_keeps_the_caller_status_when_a_rank_was_warranted() {
        let (_root, index) = corpus();
        let selection = prefilter(&index, &token_query(), 1, 10).finish(Stage2::Unavailable {
            reason: "no runner configured".to_string(),
        });
        assert_eq!(
            selection.stage2,
            Stage2::Unavailable {
                reason: "no runner configured".to_string()
            }
        );
        assert!(selection.stage2.summary().contains("no runner configured"));
    }

    #[test]
    fn test_prefiltered_is_empty_for_a_plan_matching_nothing() {
        let (_root, index) = corpus();
        let prefiltered = prefilter(&index, &Query::new("zzzz"), 5, 10);
        assert!(prefiltered.is_empty());
        assert!(!prefiltered.needs_rank());
        assert!(prefiltered.candidates().is_empty());
    }

    // ── stage 2 ─────────────────────────────────────────────────────────

    /// The point of the rank: a candidate below the cap is promoted past one
    /// above it that the ranker rejected.
    #[test]
    fn test_apply_promotes_a_governing_rule_past_a_rejected_one() {
        let (_root, index) = corpus();
        let prefiltered = prefilter(&index, &token_query(), 1, 10);
        let first = prefiltered.candidates()[0].slug.clone();
        let second = prefiltered.candidates()[1].slug.clone();

        let parsed = rank::parse_verdicts(
            &verdicts(&[
                (&first, "unrelated", "not about this change"),
                (&second, "governs", "constrains the revocation check"),
            ]),
            &prefiltered.candidates(),
        )
        .unwrap();

        let selection = prefiltered.apply(&parsed);
        assert_eq!(selection.selected.len(), 1);
        assert_eq!(selection.selected[0].slug, second);
        assert_eq!(selection.selected[0].verdict, Some(Verdict::Governs));
        assert_eq!(selection.selected[0].stage, Stage::Ranked);
        assert_eq!(
            selection.selected[0].reason,
            "constrains the revocation check"
        );
    }

    #[test]
    fn test_apply_orders_governs_then_related_then_unjudged() {
        let (_root, index) = corpus();
        let prefiltered = prefilter(&index, &token_query(), 10, 10);
        let slugs: Vec<String> = prefiltered
            .candidates()
            .iter()
            .map(|c| c.slug.clone())
            .collect();
        assert!(slugs.len() >= 3, "corpus should offer three candidates");

        // The first candidate is demoted to `related`, the second promoted to
        // `governs`, and the third left unjudged.
        let parsed = rank::parse_verdicts(
            &verdicts(&[
                (&slugs[0], "related", "same area"),
                (&slugs[1], "governs", "constrains it"),
            ]),
            &prefiltered.candidates(),
        )
        .unwrap();

        let selection = prefiltered.apply(&parsed);
        assert_eq!(selection.selected[0].slug, slugs[1]);
        assert_eq!(selection.selected[1].slug, slugs[0]);
        assert_eq!(selection.selected[2].verdict, None);
        assert_eq!(selection.selected[2].stage, Stage::Prefilter);
        // An unjudged candidate still carries the stage-1 reason.
        assert!(!selection.selected[2].reason.is_empty());
    }

    /// Within one verdict the deterministic stage-1 order is preserved, which
    /// is what keeps the answer stable when the ranker judges everything alike.
    #[test]
    fn test_apply_preserves_stage_one_order_inside_a_verdict() {
        let (_root, index) = corpus();
        let prefiltered = prefilter(&index, &token_query(), 10, 10);
        let slugs: Vec<String> = prefiltered
            .candidates()
            .iter()
            .map(|c| c.slug.clone())
            .collect();
        let entries: Vec<(&str, &str, &str)> = slugs
            .iter()
            .map(|slug| (slug.as_str(), "governs", "all equal"))
            .collect();
        let parsed = rank::parse_verdicts(&verdicts(&entries), &prefiltered.candidates()).unwrap();

        let selection = prefiltered.apply(&parsed);
        let got: Vec<String> = selection.selected.iter().map(|r| r.slug.clone()).collect();
        assert_eq!(got, slugs);
    }

    #[test]
    fn test_apply_counts_every_verdict_and_the_unjudged() {
        let (_root, index) = corpus();
        let prefiltered = prefilter(&index, &token_query(), 10, 10);
        let slugs: Vec<String> = prefiltered
            .candidates()
            .iter()
            .map(|c| c.slug.clone())
            .collect();
        let parsed = rank::parse_verdicts(
            &verdicts(&[
                (&slugs[0], "governs", "a"),
                (&slugs[1], "related", "b"),
                (&slugs[2], "unrelated", "c"),
            ]),
            &prefiltered.candidates(),
        )
        .unwrap();

        let selection = prefiltered.apply(&parsed);
        let Stage2::Applied {
            governs,
            related,
            unrelated,
            unjudged,
            candidates,
        } = selection.stage2
        else {
            panic!("expected an applied rank");
        };
        assert_eq!((governs, related, unrelated), (1, 1, 1));
        assert_eq!(unjudged, slugs.len() - 3);
        assert_eq!(candidates, slugs.len());
        assert!(selection.stage2.is_applied());
        assert!(selection.stage2.summary().contains("1 governs"));
    }

    /// A ranker that answers with a blank reason still owes the caller one.
    #[test]
    fn test_apply_falls_back_to_the_stage_one_reason_when_the_ranker_gives_none() {
        let (_root, index) = corpus();
        let prefiltered = prefilter(&index, &token_query(), 10, 10);
        let slug = prefiltered.candidates()[0].slug.clone();
        let parsed = rank::parse_verdicts(
            &verdicts(&[(&slug, "governs", "   ")]),
            &prefiltered.candidates(),
        )
        .unwrap();
        let selection = prefiltered.apply(&parsed);
        assert!(!selection.selected[0].reason.is_empty());
        assert!(selection.selected[0].reason.contains(" on "));
    }

    // ── the two stages together ─────────────────────────────────────────

    #[tokio::test]
    async fn test_select_skips_the_runner_when_the_prefilter_fits_the_cap() {
        let (_root, index) = corpus();
        let runner = FakeRunner::ok(serde_json::json!({}));
        let selection = select(&index, &token_query(), 50, 50, &runner, None, None).await;
        assert!(
            !runner.was_called(),
            "stage 2 must not run without a surplus"
        );
        assert!(matches!(selection.stage2, Stage2::NotNeeded { .. }));
    }

    /// The gate belongs to `rank_with`, not to `select`. Asserting it here is
    /// what makes it true for every caller: `rules select` and
    /// `rules eval --rank` both reach stage 2 through this method, and an
    /// earlier version that gated in `select` alone left the eval path
    /// ungated while this file's tests still passed.
    #[tokio::test]
    async fn test_rank_with_skips_the_runner_when_the_prefilter_fits_the_cap() {
        let (_root, index) = corpus();
        let runner = FakeRunner::ok(serde_json::json!({}));
        let prefiltered = prefilter(&index, &token_query(), 50, 50);
        assert!(!prefiltered.needs_rank());

        let selection = prefiltered.rank_with(&runner, None, None).await;
        assert!(
            !runner.was_called(),
            "stage 2 must not run without a surplus"
        );
        assert!(matches!(selection.stage2, Stage2::NotNeeded { .. }));
        // And the answer is the whole prefilter, untrimmed — an ungated rank
        // would have dropped whatever it judged `unrelated`.
        assert_eq!(selection.selected.len(), prefiltered.len());
    }

    #[tokio::test]
    async fn test_rank_with_calls_the_runner_when_there_is_a_surplus() {
        let (_root, index) = corpus();
        let prefiltered = prefilter(&index, &token_query(), 1, DEFAULT_CANDIDATES);
        assert!(prefiltered.needs_rank());
        let slug = prefiltered.candidates()[1].slug.clone();
        let runner = FakeRunner::ok(verdicts(&[(&slug, "governs", "it governs the change")]));

        let selection = prefiltered.rank_with(&runner, None, None).await;
        assert!(runner.was_called());
        assert!(selection.stage2.is_applied());
        assert_eq!(selection.selected[0].slug, slug);
    }

    #[tokio::test]
    async fn test_select_calls_the_runner_when_there_is_a_surplus() {
        let (_root, index) = corpus();
        let prefiltered = prefilter(&index, &token_query(), 1, DEFAULT_CANDIDATES);
        let slug = prefiltered.candidates()[1].slug.clone();
        let runner = FakeRunner::ok(verdicts(&[(&slug, "governs", "it governs the change")]));

        let selection = select(
            &index,
            &token_query(),
            1,
            DEFAULT_CANDIDATES,
            &runner,
            None,
            None,
        )
        .await;
        assert!(runner.was_called());
        assert!(selection.stage2.is_applied());
        assert_eq!(selection.selected[0].slug, slug);
        assert_eq!(selection.selected[0].reason, "it governs the change");
    }

    /// The degraded path: the runner is reachable and useless, and the caller
    /// still gets the deterministic answer rather than an error.
    #[tokio::test]
    async fn test_select_falls_back_to_stage_one_when_the_runner_fails() {
        let (_root, index) = corpus();
        let runner = FakeRunner::failing();
        let selection = select(
            &index,
            &token_query(),
            2,
            DEFAULT_CANDIDATES,
            &runner,
            None,
            None,
        )
        .await;
        assert!(runner.was_called());
        let Stage2::Failed { ref reason } = selection.stage2 else {
            panic!("expected a failed rank, got {:?}", selection.stage2);
        };
        assert!(reason.contains("runner is down"));
        assert_eq!(selection.selected.len(), 2);
        for rule in &selection.selected {
            assert_eq!(rule.stage, Stage::Prefilter);
            assert!(!rule.reason.is_empty());
        }
    }

    #[tokio::test]
    async fn test_select_falls_back_when_the_ranker_keeps_nothing() {
        let (_root, index) = corpus();
        // The only slug the ranker names was never a candidate, so nothing
        // survives validation and the rank is treated as a failure.
        let runner = FakeRunner::ok(verdicts(&[("not-a-candidate", "governs", "invented")]));

        let selection = select(
            &index,
            &token_query(),
            2,
            DEFAULT_CANDIDATES,
            &runner,
            None,
            None,
        )
        .await;
        assert!(matches!(selection.stage2, Stage2::Failed { .. }));
        assert_eq!(selection.selected.len(), 2);
    }

    // ── reasons ─────────────────────────────────────────────────────────

    #[test]
    fn test_reason_names_the_matched_glob_and_the_terms() {
        let hit = Match {
            slug: "slug".to_string(),
            relative_path: "p".to_string(),
            title: None,
            score: 1.0,
            contributions: vec![FieldContribution {
                field: Field::Scope,
                coverage: 0.5,
                weighted: 1.0,
                matched: vec!["oauth".to_string(), "token".to_string()],
            }],
            matched_globs: vec![GlobMatch {
                glob: "services/auth/**".to_string(),
                query_path: "services/auth/oauth".to_string(),
                segments: 2,
                exact: true,
            }],
        };
        let reason = prefilter_reason(&hit);
        assert!(reason.contains("verify path `services/auth/**` matches `services/auth/oauth`"));
        assert!(reason.contains("scope matches on oauth, token"));
        assert!(
            !reason.contains("scope match on"),
            "one field takes a singular verb"
        );
    }

    /// Two signals take a plural verb, one takes a singular. A reason line is
    /// read by a person deciding whether the selection is right.
    #[test]
    fn test_reason_agrees_with_the_number_of_signals_it_names() {
        let contribution = |field, matched: &[&str]| FieldContribution {
            field,
            coverage: 0.5,
            weighted: 1.0,
            matched: matched.iter().map(|s| s.to_string()).collect(),
        };
        let hit = Match {
            slug: "slug".to_string(),
            relative_path: "p".to_string(),
            title: None,
            score: 1.0,
            contributions: vec![
                contribution(Field::Scope, &["oauth"]),
                contribution(Field::Title, &["token"]),
            ],
            matched_globs: Vec::new(),
        };
        assert!(prefilter_reason(&hit).contains("scope and title match on oauth, token"));
    }

    #[test]
    fn test_reason_says_contains_for_a_partial_path_agreement() {
        let hit = Match {
            slug: "slug".to_string(),
            relative_path: "p".to_string(),
            title: None,
            score: 1.0,
            contributions: Vec::new(),
            matched_globs: vec![GlobMatch {
                glob: "services/auth/**".to_string(),
                query_path: "services".to_string(),
                segments: 1,
                exact: false,
            }],
        };
        assert!(prefilter_reason(&hit).contains("contains `services`"));
    }

    #[test]
    fn test_reason_caps_the_terms_it_names() {
        let hit = Match {
            slug: "slug".to_string(),
            relative_path: "p".to_string(),
            title: None,
            score: 1.0,
            contributions: vec![FieldContribution {
                field: Field::Scope,
                coverage: 0.5,
                weighted: 1.0,
                matched: (0..10).map(|i| format!("t{i}")).collect(),
            }],
            matched_globs: Vec::new(),
        };
        let reason = prefilter_reason(&hit);
        assert_eq!(reason.matches(", ").count(), MAX_REASON_TERMS - 1);
        assert!(reason.contains("t0"));
        assert!(!reason.contains("t9"));
    }

    #[test]
    fn test_reason_for_a_hit_with_no_attributable_signal() {
        let hit = Match {
            slug: "slug".to_string(),
            relative_path: "p".to_string(),
            title: None,
            score: 1.0,
            contributions: Vec::new(),
            matched_globs: Vec::new(),
        };
        assert_eq!(
            prefilter_reason(&hit),
            "ranked by the scope index; no single signal carried it"
        );
    }

    /// A rank that skipped more candidates than it judged is padding with
    /// prefilter hits, and the summary should not call that "ranked".
    #[test]
    fn test_a_mostly_unjudged_rank_says_so() {
        let mostly_unjudged = Stage2::Applied {
            candidates: 12,
            governs: 1,
            related: 0,
            unrelated: 0,
            unjudged: 11,
        };
        assert!(mostly_unjudged.summary().starts_with("partly ranked"));
        assert!(mostly_unjudged.summary().contains("kept at prefilter rank"));

        let fully_judged = Stage2::Applied {
            candidates: 12,
            governs: 3,
            related: 4,
            unrelated: 5,
            unjudged: 0,
        };
        assert!(fully_judged.summary().starts_with("ranked 12"));
        // Both are still `Applied`: the wording changed, not the status.
        assert!(mostly_unjudged.is_applied() && fully_judged.is_applied());
    }

    #[test]
    fn test_stage_and_status_labels() {
        assert_eq!(Stage::Prefilter.as_str(), "prefilter");
        assert_eq!(Stage::Ranked.as_str(), "ranked");
        assert_eq!(Stage2::NotRequested.summary(), "not requested");
        assert!(Stage2::Failed {
            reason: "boom".to_string()
        }
        .summary()
        .contains("failed — boom"));
    }
}
