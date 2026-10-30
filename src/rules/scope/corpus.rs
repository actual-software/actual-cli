//! A model judging the whole rule set, with no prefilter: the alternative the
//! two shipped stages are measured against.
//!
//! # Design
//!
//! This is a measurement arm for `rules eval --full-corpus`, not a shipped
//! selection path. The question it answers is whether selection needs a
//! lexical stage at all, or whether a model reading every document's scope
//! would do better. Answering that honestly means removing every advantage
//! and every handicap the two-stage selector has, and keeping everything
//! else the same.
//!
//! What is the same: the change is described in the words stage 2 uses
//! ([`rank::describe_change`]), each rule is shown as stage 2 shows a
//! candidate, and the judgement is the same three verdicts. The two arms
//! differ only in which rules the model sees and in what it is asked to
//! return.
//!
//! What differs, and why:
//!
//! **Every indexed document is listed.** Stage 2 can only rerank what stage 1
//! retrieved, so stage 1's recall is its ceiling. Here there is no ceiling to
//! inherit.
//!
//! **The model names only the rules that apply.** Asked for a verdict on each
//! of several hundred documents, a model spends most of its output on
//! `unrelated`; asked for the ones that apply, the answer stays short. A rule
//! it does not name is not selected. Stage 2 pads an incomplete answer with
//! unjudged candidates at their prefilter rank; padding here would hand the
//! arm stage 1's answer and measure that instead.
//!
//! **Nothing is a valid answer.** Stage 2 treats an empty rank as a failure
//! and falls back to the prefilter, because a hook must always have something
//! to say. A file that no rule governs should get nothing, and the
//! false-injection rate is how `rules eval` measures that, so here an empty
//! answer is the answer.
//!
//! **A path-only query is still judged.** The command skips stage 2 when there
//! is no plan prose. The alternative would have to serve the same hook, which
//! usually knows only the file, so it is asked with the path alone.
//!
//! Order is not the model's to choose, for the reason given in [`rank`]: rules
//! it says govern come before rules it calls related, and within a verdict
//! the stage-1 order holds, with the documents stage 1 scored nothing after
//! the rest, in index order. That only decides what is cut when more rules
//! apply than the cap allows.

use std::collections::HashSet;

use crate::error::ActualError;
use crate::runner::structured::StructuredRunner;

use super::index::{IndexedDocument, Query, ScopeIndex};
use super::rank::{self, Candidate, RankError, RankedVerdict, Verdict};

/// The selector name `rules eval` reports this arm under.
pub const SELECTOR: &str = "full-corpus";

/// Every indexed document as a candidate, in the order a tie inside a verdict
/// is broken: stage 1's ranking first, then whatever it scored nothing.
///
/// A slug appears once. Two documents under different roots can share a stem,
/// and a verdict names a slug, so listing both would let one verdict count
/// for two documents.
pub fn candidates(index: &ScopeIndex, query: &Query) -> Vec<Candidate> {
    let ranked = index.search(query, index.len());
    let rank_of = |doc: &IndexedDocument| {
        ranked
            .iter()
            .position(|hit| hit.relative_path == doc.relative_path)
            .unwrap_or(usize::MAX)
    };
    let mut documents: Vec<&IndexedDocument> = index.documents.iter().collect();
    // Stable, so the documents stage 1 scored nothing keep index order.
    documents.sort_by_key(|doc| rank_of(doc));
    let mut seen = HashSet::new();
    documents
        .into_iter()
        .filter(|doc| seen.insert(doc.slug.as_str()))
        .map(|doc| {
            Candidate::new(
                &doc.slug,
                doc.title.as_deref(),
                doc.scope.as_deref(),
                &doc.globs,
            )
        })
        .collect()
}

/// The full-corpus prompt: stage 2's description of the change, every rule,
/// and a task that asks only for the rules that apply.
pub fn build_prompt(plan: &str, paths: &[String], candidates: &[Candidate]) -> String {
    let mut out = rank::describe_change(plan, paths, candidates);
    out.push_str(
        "\n=== task ===\n\
         The list above is every rule document in the repository. Return a verdict \
         only for the rules that apply to this change, copying each slug exactly, \
         and leave every other rule out.\n\
         - `governs`: following this rule would change how the plan is implemented.\n\
         - `related`: the rule covers the same area but does not constrain this change.\n\
         Most changes are governed by few rules, and many by none: an empty `verdicts` \
         list is the right answer when nothing applies. Do not name any rule that is \
         not above.\n\
         Give a one-sentence reason for each, naming what in the plan the rule applies to.\n",
    );
    out
}

/// The slugs to keep from `verdicts`, at most `limit`: `governs` before
/// `related`, each in candidate order. A candidate with no verdict, or an
/// `unrelated` one, is not kept.
pub fn keep(verdicts: &[RankedVerdict], candidates: &[Candidate], limit: usize) -> Vec<String> {
    let tier = |verdict: Verdict| match verdict {
        Verdict::Governs => Some(0u8),
        Verdict::Related => Some(1),
        Verdict::Unrelated => None,
    };
    let mut kept: Vec<(u8, usize, &str)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(position, candidate)| {
            let verdict = verdicts.iter().find(|v| v.slug == candidate.slug)?;
            Some((tier(verdict.verdict)?, position, candidate.slug.as_str()))
        })
        .collect();
    kept.sort_unstable();
    kept.into_iter()
        .take(limit)
        .map(|(_, _, slug)| slug.to_string())
        .collect()
}

/// Ask `runner` which of every indexed document govern `query`, and keep at
/// most `limit` of them.
///
/// An answer that keeps nothing is `Ok` and empty. A runner that fails, runs
/// past [`rank::RANK_BUDGET`] or returns the wrong shape is an error: unlike
/// stage 2 there is no prefilter to fall back to, and scoring one would
/// measure the wrong selector.
pub async fn select<R: StructuredRunner>(
    index: &ScopeIndex,
    query: &Query,
    limit: usize,
    runner: &R,
    model_override: Option<&str>,
    max_budget_usd: Option<f64>,
) -> Result<Vec<String>, ActualError> {
    let candidates = candidates(index, query);
    let prompt = build_prompt(&query.text, &query.all_paths(), &candidates);
    let value = rank::call_ranker(runner, &prompt, model_override, max_budget_usd).await?;
    match rank::parse_verdicts(&value, &candidates) {
        Ok(verdicts) => Ok(keep(&verdicts, &candidates, limit)),
        Err(RankError::NothingKept) => Ok(Vec::new()),
        Err(e) => Err(ActualError::RuleRankInvalid(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::rules::{load_rule_set, rules_dir};

    use tempfile::{tempdir, TempDir};

    const OAUTH: &str = "# Sign With Asymmetric Keys: Token Signing\n\nThese rules are ALWAYS ACTIVE for OAuth token signing in `services/auth/oauth/`.\n\n### Rules\n\n- **R-A-001** MUST: sign with RS256.\n";
    const REVOCATION: &str = "# Check Revocation: Token Verification\n\nThese rules are ALWAYS ACTIVE for OAuth token verification.\n\n### Rules\n\n- **R-B-001** MUST: check revocation.\n";
    const TERRAFORM: &str = "# Pin Providers: Terraform\n\nThese rules are ALWAYS ACTIVE for Terraform configuration in `infra/terraform/`.\n\n### Rules\n\n- **R-D-001** MUST: pin providers.\n";

    const SIGNING: &str = "cross-cutting-token-signing-1c57";
    const REVOKE: &str = "cross-cutting-token-revocation-963a";
    const PIN: &str = "cross-cutting-terraform-c340";

    fn corpus() -> (TempDir, ScopeIndex) {
        let root = tempdir().unwrap();
        let dir = rules_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        for (slug, body) in [(SIGNING, OAUTH), (REVOKE, REVOCATION), (PIN, TERRAFORM)] {
            std::fs::write(dir.join(format!("{slug}.md")), body).unwrap();
        }
        let report = load_rule_set(root.path()).unwrap();
        let index = ScopeIndex::build(&report, root.path(), "test".to_string());
        (root, index)
    }

    /// A runner that answers with a fixed value, or fails, and keeps the prompt.
    struct FakeRunner {
        answer: Result<serde_json::Value, &'static str>,
        seen: std::sync::Mutex<Option<String>>,
    }

    impl FakeRunner {
        fn new(answer: Result<serde_json::Value, &'static str>) -> Self {
            Self {
                answer,
                seen: std::sync::Mutex::new(None),
            }
        }

        fn prompt(&self) -> String {
            self.seen.lock().unwrap().clone().unwrap()
        }
    }

    impl StructuredRunner for FakeRunner {
        async fn run_structured_json(
            &self,
            prompt: &str,
            _schema: &str,
            _model_override: Option<&str>,
            _max_budget_usd: Option<f64>,
            _effort: Option<&str>,
        ) -> Result<serde_json::Value, ActualError> {
            *self.seen.lock().unwrap() = Some(prompt.to_string());
            self.answer
                .clone()
                .map_err(|message| ActualError::RunnerFailed {
                    message: message.to_string(),
                    stderr: String::new(),
                })
        }
    }

    fn verdicts(entries: &[(&str, &str)]) -> serde_json::Value {
        serde_json::json!({
            "verdicts": entries
                .iter()
                .map(|(slug, verdict)| serde_json::json!({
                    "slug": slug, "verdict": verdict, "reason": "applies",
                }))
                .collect::<Vec<_>>()
        })
    }

    fn verdict(slug: &str, verdict: Verdict) -> RankedVerdict {
        RankedVerdict {
            slug: slug.to_string(),
            verdict,
            reason: String::new(),
        }
    }

    fn slugs(candidates: &[Candidate]) -> Vec<&str> {
        candidates.iter().map(|c| c.slug.as_str()).collect()
    }

    fn run<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    // ── candidates ───────────────────────────────────────────────────────

    /// Every document is listed, not just what stage 1 retrieved: that is
    /// the whole point of the arm.
    #[test]
    fn test_candidates_list_every_document_stage_one_ranking_first() {
        let (_root, index) = corpus();
        let listed = candidates(&index, &Query::new("Pin the terraform providers"));
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].slug, PIN, "stage 1's top hit leads");
        let mut all = slugs(&listed);
        all.sort_unstable();
        assert_eq!(all, vec![PIN, REVOKE, SIGNING]);
    }

    /// A query stage 1 scores nothing for still lists the whole corpus, in
    /// index order.
    #[test]
    fn test_candidates_for_an_unmatched_query_are_the_index_in_order() {
        let (_root, index) = corpus();
        let listed = candidates(&index, &Query::new("zzzz"));
        let expected: Vec<&str> = index.documents.iter().map(|d| d.slug.as_str()).collect();
        assert_eq!(slugs(&listed), expected);
    }

    /// Two documents sharing a stem under different roots are listed once,
    /// because a verdict names a slug and could otherwise count for both.
    #[test]
    fn test_candidates_list_a_shared_slug_once() {
        let (_root, mut index) = corpus();
        let mut twin = index.documents[0].clone();
        twin.relative_path = format!("nested/{}", twin.relative_path);
        index.documents.push(twin);
        let listed = candidates(&index, &Query::new("zzzz"));
        assert_eq!(listed.len(), 3);
    }

    // ── prompt ───────────────────────────────────────────────────────────

    /// Same description of the change as stage 2, a different task.
    #[test]
    fn test_prompt_shares_stage_two_description_and_allows_an_empty_answer() {
        let (_root, index) = corpus();
        let query = Query::new("Rotate the signing keys");
        let listed = candidates(&index, &query);
        let prompt = build_prompt(&query.text, &[], &listed);
        assert!(prompt.starts_with(&rank::describe_change(&query.text, &[], &listed)));
        assert!(prompt.contains("every rule document in the repository"));
        assert!(prompt.contains("an empty `verdicts` list is the right answer"));
        assert!(!prompt.contains("do not omit one"));
    }

    // ── keep ─────────────────────────────────────────────────────────────

    /// `governs` outranks `related`; within a verdict candidate order holds;
    /// unrelated and unnamed candidates are not kept; the cap applies last.
    #[test]
    fn test_keep_orders_by_verdict_then_candidate_order_and_caps() {
        let listed = vec![
            Candidate::new(SIGNING, None, None, &[]),
            Candidate::new(REVOKE, None, None, &[]),
            Candidate::new(PIN, None, None, &[]),
            Candidate::new("unnamed", None, None, &[]),
        ];
        let answer = vec![
            verdict(PIN, Verdict::Governs),
            verdict(SIGNING, Verdict::Related),
            verdict(REVOKE, Verdict::Governs),
        ];
        assert_eq!(keep(&answer, &listed, 5), vec![REVOKE, PIN, SIGNING]);
        assert_eq!(keep(&answer, &listed, 2), vec![REVOKE, PIN]);

        let rejected = vec![verdict(PIN, Verdict::Unrelated)];
        assert!(keep(&rejected, &listed, 5).is_empty());
    }

    // ── select ───────────────────────────────────────────────────────────

    #[test]
    fn test_select_keeps_what_the_model_names_and_sends_every_document() {
        let (_root, index) = corpus();
        let runner = FakeRunner::new(Ok(verdicts(&[(PIN, "governs")])));
        let query = Query::new("zzzz").with_paths(vec!["infra/terraform/main.tf".to_string()]);
        let kept = run(select(&index, &query, 5, &runner, None, None)).unwrap();
        assert_eq!(kept, vec![PIN]);
        let prompt = runner.prompt();
        for slug in [SIGNING, REVOKE, PIN] {
            assert!(
                prompt.contains(&format!("- slug: {slug}")),
                "{slug} not listed"
            );
        }
        assert!(prompt.contains("- infra/terraform/main.tf"));
    }

    /// Rejecting everything is an answer, not a failure: there is no
    /// prefilter to fall back to.
    #[test]
    fn test_select_returns_nothing_when_nothing_applies() {
        let (_root, index) = corpus();
        for answer in [verdicts(&[]), verdicts(&[(PIN, "unrelated")])] {
            let runner = FakeRunner::new(Ok(answer));
            let kept = run(select(&index, &Query::new("x"), 5, &runner, None, None)).unwrap();
            assert!(kept.is_empty());
        }
    }

    #[test]
    fn test_select_fails_on_a_malformed_answer() {
        let (_root, index) = corpus();
        let runner = FakeRunner::new(Ok(serde_json::json!({ "nope": [] })));
        let err = run(select(&index, &Query::new("x"), 5, &runner, None, None)).unwrap_err();
        assert!(matches!(err, ActualError::RuleRankInvalid(_)), "{err:?}");
    }

    #[test]
    fn test_select_fails_when_the_runner_does() {
        let (_root, index) = corpus();
        let runner = FakeRunner::new(Err("runner is down"));
        let err = run(select(&index, &Query::new("x"), 5, &runner, None, None)).unwrap_err();
        assert!(matches!(err, ActualError::RunnerFailed { .. }), "{err:?}");
    }
}
