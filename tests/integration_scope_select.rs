//! End-to-end checks for the two-stage rule selector, through the public API.
//!
//! # Design
//!
//! These are the acceptance criteria for selecting rules for a proposed plan,
//! written so a regression fails the build:
//!
//! * a plan yields a ranked, capped set of rule files, each with a reason;
//! * stage 1 alone is a usable answer when no runner is configured or the
//!   runner is unavailable — degraded, not broken;
//! * the selection is reproducible for the same plan and rule set;
//! * the ranked selector is measurable on the same golden set at the same cap
//!   as the two offline selectors.
//!
//! Everything goes through `actual_cli::rules::scope`, never a private helper,
//! so the tests exercise the surface a caller has. The runner is a fake
//! implementing the public [`StructuredRunner`] contract — no subprocess, no
//! socket, no key. That is the point of the trait: stage 2 is testable without
//! a model, so what is asserted here is the orchestration rather than any
//! particular model's judgement.
//!
//! The corpus under `tests/fixtures/scope_corpus/` is the same synthetic rule
//! set the index tests use.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use actual_cli::error::ActualError;
use actual_cli::rules::load_rule_set;
use actual_cli::rules::scope::{
    index::{Query, ScopeIndex},
    rank::{self, Verdict},
    select::{self, Selection, Stage, Stage2},
    GoldenCase, Scores,
};
use actual_cli::runner::StructuredRunner;

/// The fixture repository root.
fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scope_corpus")
}

/// Build the index directly, with no cache in the loop — the cache has its own
/// tests, and here it would only make a result depend on what a prior test left.
fn build_index() -> ScopeIndex {
    let root = corpus_root();
    let report = load_rule_set(&root).expect("fixture corpus loads");
    ScopeIndex::build(&report, &root, "test".to_string())
}

fn golden_cases() -> Vec<GoldenCase> {
    let text = std::fs::read_to_string(corpus_root().join("golden.json")).unwrap();
    serde_json::from_str(&text).expect("golden set parses")
}

/// A plan from the golden set, by name.
fn case(name: &str) -> GoldenCase {
    golden_cases()
        .into_iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("golden case `{name}` should exist"))
}

fn query(case: &GoldenCase) -> Query {
    Query::new(case.plan.clone()).with_paths(case.paths.clone())
}

// ── a fake runner ────────────────────────────────────────────────────────

/// How the fake answers a rank request.
enum Answer {
    /// Judge each candidate by whether the golden set expects it. Stands in for
    /// a perfectly calibrated ranker, so what is measured is the plumbing
    /// around the model rather than the model.
    Oracle(Vec<String>),
    /// Return this JSON verbatim, whatever shape it has.
    Verbatim(serde_json::Value),
    /// Fail the way an unreachable backend does.
    Down,
}

struct FakeRunner {
    answer: Answer,
    prompts: Mutex<Vec<String>>,
}

impl FakeRunner {
    fn oracle(expected: &[String]) -> Self {
        Self {
            answer: Answer::Oracle(expected.to_vec()),
            prompts: Mutex::new(Vec::new()),
        }
    }

    fn verbatim(value: serde_json::Value) -> Self {
        Self {
            answer: Answer::Verbatim(value),
            prompts: Mutex::new(Vec::new()),
        }
    }

    fn down() -> Self {
        Self {
            answer: Answer::Down,
            prompts: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> usize {
        self.prompts.lock().unwrap().len()
    }

    fn last_prompt(&self) -> String {
        self.prompts.lock().unwrap().last().cloned().unwrap()
    }

    /// Every slug the prompt listed as a candidate, in the order shown.
    ///
    /// Read back out of the prompt text rather than taken from the caller, so
    /// the oracle judges exactly what the model would have seen.
    fn candidate_slugs(&self) -> Vec<String> {
        self.last_prompt()
            .lines()
            .filter_map(|line| line.trim().strip_prefix("- slug: ").map(str::to_string))
            .collect()
    }
}

impl StructuredRunner for FakeRunner {
    async fn run_structured_json(
        &self,
        prompt: &str,
        _schema: &str,
        _model_override: Option<&str>,
        _max_budget_usd: Option<f64>,
    ) -> Result<serde_json::Value, ActualError> {
        self.prompts.lock().unwrap().push(prompt.to_string());
        match &self.answer {
            Answer::Verbatim(value) => Ok(value.clone()),
            Answer::Down => Err(ActualError::RunnerFailed {
                message: "connection refused".to_string(),
                stderr: String::new(),
            }),
            Answer::Oracle(expected) => {
                let verdicts: Vec<serde_json::Value> = self
                    .candidate_slugs()
                    .into_iter()
                    .map(|slug| {
                        let governs = expected.contains(&slug);
                        serde_json::json!({
                            "slug": slug,
                            "verdict": if governs { "governs" } else { "unrelated" },
                            "reason": if governs {
                                "constrains this change"
                            } else {
                                "does not apply here"
                            },
                        })
                    })
                    .collect();
                Ok(serde_json::json!({ "verdicts": verdicts }))
            }
        }
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn run(index: &ScopeIndex, case: &GoldenCase, limit: usize, runner: &FakeRunner) -> Selection {
    block_on(select::select(
        index,
        &query(case),
        limit,
        select::DEFAULT_CANDIDATES,
        runner,
        None,
        None,
    ))
}

// ── acceptance ───────────────────────────────────────────────────────────

/// Given a plan, the selector returns a ranked, capped set of rule files with a
/// reason for each. The first acceptance criterion, stated as a test.
#[test]
fn a_plan_yields_a_ranked_capped_set_with_a_reason_for_each() {
    let index = build_index();
    let case = case("jwks-key-rollover");
    let runner = FakeRunner::oracle(&case.expected);

    let selection = run(&index, &case, 5, &runner);

    assert!(
        !selection.selected.is_empty(),
        "a plan must select something"
    );
    assert!(selection.selected.len() <= 5, "the cap is a cap");
    for rule in &selection.selected {
        assert!(
            !rule.reason.trim().is_empty(),
            "{} was selected without a reason",
            rule.slug
        );
        assert!(
            rule.relative_path.starts_with(".actual/rules/"),
            "{} should name the file it came from",
            rule.slug
        );
    }
    // Ranked, not merely a set: scores descend within a verdict.
    let governing: Vec<f64> = selection
        .selected
        .iter()
        .filter(|r| r.verdict == Some(Verdict::Governs))
        .map(|r| r.score)
        .collect();
    assert!(
        governing.windows(2).all(|pair| pair[0] >= pair[1]),
        "governing rules should stay in descending index order: {governing:?}"
    );
}

/// Stage 1 alone is a usable result when no runner is configured. Nothing here
/// touches a runner at all.
#[test]
fn stage_one_alone_answers_with_no_runner_in_the_loop() {
    let index = build_index();
    let case = case("refresh-token-rotation");

    let selection = select::prefilter(&index, &query(&case), 5, select::DEFAULT_CANDIDATES).finish(
        Stage2::Unavailable {
            reason: "no runner configured".to_string(),
        },
    );

    assert_eq!(selection.selected.len(), 5);
    for rule in &selection.selected {
        assert_eq!(rule.stage, Stage::Prefilter);
        assert!(!rule.reason.trim().is_empty());
    }
    // The degradation is reported rather than hidden.
    assert!(selection.stage2.summary().contains("no runner configured"));
    assert!(!selection.stage2.is_applied());
    // And it still finds real rules: degraded, not broken.
    let found = selection
        .selected
        .iter()
        .filter(|r| case.expected.contains(&r.slug))
        .count();
    assert!(
        found > 0,
        "the prefilter alone must retrieve something real"
    );
}

/// A runner that is reachable and useless is the same as no runner: the
/// deterministic answer comes back, with the failure recorded.
#[test]
fn a_failing_runner_degrades_to_the_prefilter_rather_than_to_an_error() {
    let index = build_index();
    let case = case("jwks-key-rollover");

    for runner in [
        FakeRunner::down(),
        // Reachable, and answering with the wrong shape.
        FakeRunner::verbatim(serde_json::json!({"answer": "yes"})),
        // Reachable, and naming only rules that were never candidates.
        FakeRunner::verbatim(serde_json::json!({"verdicts": [
            {"slug": "invented", "verdict": "governs", "reason": "made up"}
        ]})),
        // Reachable, and rejecting everything.
        FakeRunner::verbatim(serde_json::json!({"verdicts": []})),
    ] {
        let selection = run(&index, &case, 5, &runner);
        assert_eq!(runner.calls(), 1, "the runner should be asked exactly once");
        assert!(
            matches!(selection.stage2, Stage2::Failed { .. }),
            "expected a recorded failure, got {:?}",
            selection.stage2
        );
        assert_eq!(selection.selected.len(), 5);
        for rule in &selection.selected {
            assert_eq!(rule.stage, Stage::Prefilter);
            assert!(!rule.reason.trim().is_empty());
        }
    }
}

/// Selection is reproducible for the same plan and rule set. Both stages are
/// run twice and compared whole, prompt bytes included.
#[test]
fn the_same_plan_and_rule_set_produce_the_same_selection() {
    let index = build_index();
    let case = case("new-temporal-activity");

    let first_runner = FakeRunner::oracle(&case.expected);
    let first = run(&index, &case, 5, &first_runner);
    let second_runner = FakeRunner::oracle(&case.expected);
    let second = run(&index, &case, 5, &second_runner);

    assert_eq!(first, second, "the selection must not vary between runs");
    assert_eq!(
        first_runner.last_prompt(),
        second_runner.last_prompt(),
        "the prompt must not vary between runs either"
    );
    // The index is rebuilt from the same files, and still selects the same set.
    let rebuilt = run(
        &build_index(),
        &case,
        5,
        &FakeRunner::oracle(&case.expected),
    );
    assert_eq!(first, rebuilt);
}

/// The model may partition the candidates. It may not invent them, reorder
/// inside a partition, or empty the result — the three ways a model could make
/// the answer unreproducible.
#[test]
fn the_ranker_may_only_partition_the_candidates_it_was_given() {
    let index = build_index();
    let case = case("jwks-key-rollover");
    let prefiltered = select::prefilter(&index, &query(&case), 5, select::DEFAULT_CANDIDATES);
    let candidates = prefiltered.candidates();
    assert!(candidates.len() > 5, "this plan should leave a surplus");

    // Everything judged alike: the deterministic stage-1 order survives intact.
    let all_governs: Vec<serde_json::Value> = candidates
        .iter()
        .map(|c| serde_json::json!({"slug": c.slug, "verdict": "governs", "reason": "yes"}))
        .collect();
    let verdicts =
        rank::parse_verdicts(&serde_json::json!({ "verdicts": all_governs }), &candidates).unwrap();
    let selection = prefiltered.apply(&verdicts);
    let selected: Vec<&String> = selection.selected.iter().map(|r| &r.slug).collect();
    let expected: Vec<&String> = candidates.iter().take(5).map(|c| &c.slug).collect();
    assert_eq!(selected, expected);

    // An invented slug never reaches the result.
    let mixed = rank::parse_verdicts(
        &serde_json::json!({"verdicts": [
            {"slug": "cross-cutting-not-a-real-rule-0000", "verdict": "governs", "reason": "no"},
            {"slug": candidates[0].slug, "verdict": "governs", "reason": "yes"},
        ]}),
        &candidates,
    )
    .unwrap();
    assert_eq!(mixed.len(), 1);
    assert_eq!(mixed[0].slug, candidates[0].slug);
}

/// Stage 2 costs a model call, so it is only paid when the prefilter leaves
/// more candidates than the caller may keep.
#[test]
fn stage_two_is_skipped_when_the_prefilter_already_fits_the_cap() {
    let index = build_index();
    let case = case("jwks-key-rollover");
    let runner = FakeRunner::oracle(&case.expected);

    // A cap larger than the corpus cannot leave a surplus.
    let selection = run(&index, &case, index.len() + 1, &runner);
    assert_eq!(runner.calls(), 0, "no surplus means no model call");
    assert!(matches!(selection.stage2, Stage2::NotNeeded { .. }));
}

/// The prompt carries the candidates in the prefilter's order and nothing from
/// outside it, which is what makes "the model may only judge" enforceable.
#[test]
fn the_prompt_lists_exactly_the_prefiltered_candidates_in_order() {
    let index = build_index();
    let case = case("new-temporal-activity");
    let runner = FakeRunner::oracle(&case.expected);
    run(&index, &case, 3, &runner);

    let prefiltered = select::prefilter(&index, &query(&case), 3, select::DEFAULT_CANDIDATES);
    let expected: Vec<String> = prefiltered
        .candidates()
        .iter()
        .map(|c| c.slug.clone())
        .collect();
    assert_eq!(runner.candidate_slugs(), expected);
    // The path the plan names reaches the prompt too.
    assert!(runner
        .last_prompt()
        .contains("backend/workers/activities/reconcile.py"));
}

/// The ranked selector is measurable on the golden set, at the same cap the
/// offline selectors are measured at. With a perfectly calibrated ranker it
/// cannot do worse than the prefilter, which is the property the two-stage
/// design is supposed to guarantee: stage 2 only ever removes false positives
/// and promotes what stage 1 already retrieved.
#[test]
fn the_ranked_selector_scores_at_least_as_well_as_the_prefilter() {
    let index = build_index();
    const LIMIT: usize = 5;

    let (mut prefilter_hits, mut ranked_hits) = (0usize, 0usize);
    let (mut prefilter_selected, mut ranked_selected) = (0usize, 0usize);
    let mut expected_total = 0usize;

    for case in golden_cases() {
        let offline = select::prefilter(&index, &query(&case), LIMIT, select::DEFAULT_CANDIDATES)
            .finish(Stage2::NotRequested);
        let offline_slugs: Vec<String> = offline.selected.iter().map(|r| r.slug.clone()).collect();
        let offline_scores = Scores::measure(&offline_slugs, &case.expected);

        let runner = FakeRunner::oracle(&case.expected);
        let ranked = run(&index, &case, LIMIT, &runner);
        let ranked_slugs: Vec<String> = ranked.selected.iter().map(|r| r.slug.clone()).collect();
        let ranked_scores = Scores::measure(&ranked_slugs, &case.expected);

        assert!(
            ranked_scores.true_positives >= offline_scores.true_positives,
            "case `{}`: the rank lost a true positive ({} -> {})",
            case.name,
            offline_scores.true_positives,
            ranked_scores.true_positives
        );

        prefilter_hits += offline_scores.true_positives;
        ranked_hits += ranked_scores.true_positives;
        prefilter_selected += offline_slugs.len();
        ranked_selected += ranked_slugs.len();
        expected_total += case.expected.len();
    }

    assert!(expected_total > 0);
    assert!(
        ranked_hits >= prefilter_hits,
        "pooled hits fell: {ranked_hits} against {prefilter_hits}"
    );
    // Precision is where the rank pays: it drops candidates rather than adding
    // them, so it can only return fewer wrong answers.
    let prefilter_precision = prefilter_hits as f64 / prefilter_selected as f64;
    let ranked_precision = ranked_hits as f64 / ranked_selected.max(1) as f64;
    assert!(
        ranked_precision >= prefilter_precision,
        "pooled precision fell: {ranked_precision:.2} against {prefilter_precision:.2}"
    );
}
