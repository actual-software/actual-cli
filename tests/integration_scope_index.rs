//! End-to-end checks for the scope index, through the public library API.
//!
//! # Design
//!
//! These are the acceptance criteria for local scope resolution, written so a
//! regression fails the build rather than being noticed later:
//!
//! * the index beats the status-quo filename scan on a fixed golden set, by a
//!   measured margin;
//! * retrieval is deterministic and offline — no model, no network, no clock;
//! * building an index over a corpus the size of the reference one stays inside
//!   an interactive latency budget.
//!
//! Everything here goes through `actual_cli::rules::scope`, never through a
//! private helper, so the tests exercise the same surface a caller has.
//!
//! The corpus under `tests/fixtures/scope_corpus/` is synthetic. It reproduces
//! the shapes of the reference corpus — the same dead `cross-cutting-` prefix
//! on every filename, `<ADR title>: <Aspect>` titles, prose scope sentences,
//! and `### Verify` blocks naming real paths — across twelve topic clusters
//! with deliberate near-neighbours (OAuth beside Next.js auth, Terraform
//! providers beside Lambda sizing, metrics beside logging), because those
//! neighbours are where a filename scan fails.

use std::path::{Path, PathBuf};
use std::time::Instant;

use actual_cli::rules::scope::{
    baseline,
    eval::{CaseResult, EvaluationReport, GoldenCase, Scores},
    index::{Query, ScopeIndex, Weights},
};
use actual_cli::rules::{load_rule_set, rules_dir};

/// The fixture repository root.
fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scope_corpus")
}

/// Build the index directly, with no cache in the loop. The cache is exercised
/// by its own unit tests; here it would only make the result depend on whatever
/// a previous test left behind.
fn build_index(root: &Path) -> ScopeIndex {
    let report = load_rule_set(root).expect("fixture corpus loads");
    assert!(
        report.errors.is_empty(),
        "fixture corpus must parse cleanly"
    );
    ScopeIndex::build(&report, root, "test".to_string())
}

fn golden_cases() -> Vec<GoldenCase> {
    let path = corpus_root().join("golden.json");
    let text = std::fs::read_to_string(&path).expect("golden set is readable");
    serde_json::from_str(&text).expect("golden set parses")
}

/// Score one selector over the golden set at `limit`.
fn evaluate(
    name: &str,
    cases: &[GoldenCase],
    limit: usize,
    select: impl Fn(&GoldenCase) -> Vec<String>,
) -> EvaluationReport {
    let results = cases
        .iter()
        .map(|case| {
            let selected = select(case);
            assert!(
                selected.len() <= limit,
                "{name} returned more than the cap on case {}",
                case.name
            );
            CaseResult {
                name: case.name.clone(),
                scores: Scores::measure(&selected, &case.expected),
                selected,
                expected: case.expected.clone(),
            }
        })
        .collect();
    EvaluationReport::new(name, results)
}

fn index_report(index: &ScopeIndex, cases: &[GoldenCase], limit: usize) -> EvaluationReport {
    evaluate("scope-index", cases, limit, |case| {
        let query = Query::new(case.plan.clone()).with_paths(case.paths.clone());
        index
            .search(&query, limit)
            .into_iter()
            .map(|m| m.slug)
            .collect()
    })
}

fn scan_report(index: &ScopeIndex, cases: &[GoldenCase], limit: usize) -> EvaluationReport {
    evaluate("filename-scan", cases, limit, |case| {
        baseline::select(index, &case.plan, limit)
            .into_iter()
            .map(|hit| hit.slug)
            .collect()
    })
}

// ── the golden set ───────────────────────────────────────────────────────

#[test]
fn test_golden_set_refers_only_to_documents_that_exist() {
    let index = build_index(&corpus_root());
    let known: Vec<&str> = index.documents.iter().map(|d| d.slug.as_str()).collect();
    for case in golden_cases() {
        assert!(
            !case.expected.is_empty(),
            "case {} expects nothing",
            case.name
        );
        for slug in &case.expected {
            assert!(
                known.contains(&slug.as_str()),
                "case {} expects unknown rule {slug}",
                case.name
            );
        }
    }
}

/// The load-bearing claim of this work, at the cap the status quo works under.
#[test]
fn test_scope_index_beats_the_filename_scan_at_the_status_quo_cap() {
    let index = build_index(&corpus_root());
    let cases = golden_cases();
    let limit = baseline::DEFAULT_LIMIT;

    let indexed = index_report(&index, &cases, limit);
    let scanned = scan_report(&index, &cases, limit);

    // Asserted with headroom below the measured margin, so an ordinary
    // fluctuation does not fail the build but a regression in kind does.
    assert!(
        indexed.micro.f1 > scanned.micro.f1 + 0.05,
        "index must beat the filename scan by a clear margin\n  {}\n  {}",
        indexed.summary_line(),
        scanned.summary_line()
    );
    assert!(
        indexed.micro.recall > scanned.micro.recall + 0.10,
        "recall is the failure that matters — a governing rule missed\n  {}\n  {}",
        indexed.summary_line(),
        scanned.summary_line()
    );
    assert!(indexed.micro.precision >= scanned.micro.precision);
}

/// Absolute floors, so the index cannot quietly rot to just-above-baseline.
#[test]
fn test_scope_index_meets_its_absolute_floor() {
    let index = build_index(&corpus_root());
    let report = index_report(&index, &golden_cases(), baseline::DEFAULT_LIMIT);
    assert!(report.micro.recall >= 0.80, "{}", report.summary_line());
    assert!(report.micro.f1 >= 0.70, "{}", report.summary_line());
}

/// Every plan must retrieve at least one governing rule. A plan that retrieves
/// none is the failure the whole exercise exists to prevent, and it is not
/// visible in an aggregate.
#[test]
fn test_every_golden_plan_retrieves_at_least_one_governing_rule() {
    let index = build_index(&corpus_root());
    let report = index_report(&index, &golden_cases(), baseline::DEFAULT_LIMIT);
    let missed: Vec<&str> = report
        .total_misses()
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(missed.is_empty(), "plans retrieved nothing: {missed:?}");
}

/// The index must win on its own merits, not on the one signal the ticket did
/// not ask for. With the title switched off it still has to beat the scan.
#[test]
fn test_index_beats_the_scan_without_the_title_signal() {
    let index = build_index(&corpus_root());
    let cases = golden_cases();
    let limit = baseline::DEFAULT_LIMIT;
    let weights = Weights::default().without(actual_cli::rules::scope::Field::Title);

    let indexed = evaluate("scope-index-no-title", &cases, limit, |case| {
        let query = Query::new(case.plan.clone()).with_paths(case.paths.clone());
        index
            .search_weighted(&query, limit, &weights)
            .into_iter()
            .map(|m| m.slug)
            .collect()
    });
    let scanned = scan_report(&index, &cases, limit);
    assert!(
        indexed.micro.f1 > scanned.micro.f1,
        "{}\n{}",
        indexed.summary_line(),
        scanned.summary_line()
    );
}

// ── the mixed v1/v2 corpus ───────────────────────────────────────────────
//
// Twenty of the corpus's files carry a v2 header (six whole decisions, with
// their roots, statuses, languages and an exclude varied), eighteen stay v1,
// and four more are monorepo-rooted v2 files added for root scoping, excludes
// and status. A decision is converted whole, so no decision is split across
// the two formats.

const WEB_COMPONENTS: &str = "cross-cutting-web-components-are-presentational-8a1c";
const WEB_TESTS: &str = "cross-cutting-web-test-files-use-factories-5d3e";
const API_HANDLERS: &str = "cross-cutting-api-handlers-validate-input-2f9d";
const LEGACY_SUPERSEDED: &str = "cross-cutting-legacy-web-widgets-are-frozen-7b4a";

/// The golden cases added for v2 roots and excludes. Everything else in the
/// golden set predates v2 and is held to its pre-v2 numbers.
fn is_v2_case(name: &str) -> bool {
    matches!(
        name,
        "web-component-edit" | "api-handler-edit" | "web-test-edit"
    )
}

fn slugs_for_path(index: &ScopeIndex, path: &str) -> Vec<String> {
    index
        .search(&Query::new("").with_paths([path.to_string()]), 10)
        .into_iter()
        .map(|hit| hit.slug)
        .collect()
}

#[test]
fn test_the_corpus_mixes_v1_and_v2_documents() {
    let index = build_index(&corpus_root());
    let v2 = index
        .documents
        .iter()
        .filter(|d| !d.root.is_empty())
        .count();
    let v1 = index.documents.len() - v2;
    assert!(v2 >= 20 && v1 >= 18, "v2={v2} v1={v1}");

    let roots: std::collections::BTreeSet<&str> = index
        .documents
        .iter()
        .filter(|d| !d.root.is_empty())
        .map(|d| d.root.as_str())
        .collect();
    for root in [".", "web", "backend/db", "packages/web", "packages/api"] {
        assert!(
            roots.contains(root),
            "no v2 document rooted at {root}: {roots:?}"
        );
    }
    assert!(index.documents.iter().any(|d| !d.exclude_globs.is_empty()));
    assert!(index.documents.iter().any(|d| !d.languages.is_empty()));
}

/// The original golden cases, on the mixed corpus, are held to what they scored
/// on the all-v1 corpus before v2 existed (micro recall 0.87, F1 0.76).
#[test]
fn test_the_original_golden_cases_are_not_worse_on_the_mixed_corpus() {
    let index = build_index(&corpus_root());
    let cases: Vec<GoldenCase> = golden_cases()
        .into_iter()
        .filter(|c| !is_v2_case(&c.name))
        .collect();
    assert_eq!(cases.len(), 16);
    let report = index_report(&index, &cases, baseline::DEFAULT_LIMIT);
    assert!(report.micro.recall >= 0.865, "{}", report.summary_line());
    assert!(report.micro.f1 >= 0.755, "{}", report.summary_line());
}

#[test]
fn test_a_rooted_decision_governs_its_own_package_and_not_a_sibling() {
    let index = build_index(&corpus_root());
    let web = slugs_for_path(&index, "packages/web/src/components/Button.tsx");
    let api = slugs_for_path(&index, "packages/api/src/handlers/orders.ts");
    assert_eq!(
        web.first().map(String::as_str),
        Some(WEB_COMPONENTS),
        "{web:?}"
    );
    assert_eq!(
        api.first().map(String::as_str),
        Some(API_HANDLERS),
        "{api:?}"
    );
    assert!(!web.contains(&API_HANDLERS.to_string()), "{web:?}");
    assert!(!api.contains(&WEB_COMPONENTS.to_string()), "{api:?}");
    // A package nobody rooted a decision at gets neither.
    let other = slugs_for_path(&index, "packages/other/src/components/x.tsx");
    assert!(!other.contains(&WEB_COMPONENTS.to_string()), "{other:?}");
}

#[test]
fn test_an_excluded_test_file_gets_no_brief_for_the_decision_that_excludes_it() {
    let index = build_index(&corpus_root());
    let test_file = slugs_for_path(&index, "packages/web/src/components/Button.test.tsx");
    assert!(
        !test_file.contains(&WEB_COMPONENTS.to_string()),
        "{test_file:?}"
    );
    assert_eq!(
        test_file.first().map(String::as_str),
        Some(WEB_TESTS),
        "{test_file:?}"
    );
    // Even with words that match it, the excluded decision stays out.
    let words = index.search(
        &Query::new("web components are presentational")
            .with_paths(["packages/web/src/components/Button.test.tsx".to_string()]),
        10,
    );
    assert!(words.iter().all(|hit| hit.slug != WEB_COMPONENTS));
}

#[test]
fn test_a_superseded_decision_is_not_indexed_and_never_selected() {
    let index = build_index(&corpus_root());
    assert!(index.documents.iter().all(|d| d.slug != LEGACY_SUPERSEDED));
    assert_eq!(index.inactive_documents, 1);
    let hits = slugs_for_path(&index, "packages/web/src/components/Button.tsx");
    assert!(!hits.contains(&LEGACY_SUPERSEDED.to_string()), "{hits:?}");
}

/// The api document's Verify block names `services/legacy-api/`. Its header
/// declares paths, so that decoy must not become a glob.
#[test]
fn test_declared_paths_win_over_a_decoy_verify_path() {
    let index = build_index(&corpus_root());
    let api = index
        .documents
        .iter()
        .find(|d| d.slug == API_HANDLERS)
        .expect("api document is indexed");
    assert_eq!(api.globs, vec!["packages/api/src/handlers/**"]);
    // Other (v1) decisions may govern `services/**`; this one must not.
    let decoy = slugs_for_path(&index, "services/legacy-api/x.ts");
    assert!(!decoy.contains(&API_HANDLERS.to_string()), "{decoy:?}");
}

#[test]
fn test_a_decision_converted_to_v2_keeps_its_documents_together() {
    let index = build_index(&corpus_root());
    const OAUTH_ADR_ID: &str = "1243b051-95a3-50a8-be4f-9f652693c20c";
    const OAUTH_GROUP_ID: &str = "990bb253-6cba-5551-a3ea-4b58289b03f8";
    // Four OAuth documents, one decision: selected by adr_id, not by slug.
    let oauth: Vec<_> = index
        .documents
        .iter()
        .filter(|d| d.adr_id.as_deref() == Some(OAUTH_ADR_ID))
        .collect();
    assert_eq!(
        oauth.len(),
        4,
        "token-signing, -verification, -expiration, access-tokens-carry-standard-claims"
    );
    let keys: std::collections::BTreeSet<String> = oauth.iter().map(|d| d.decision_key()).collect();
    assert_eq!(keys.len(), 1, "{keys:?}");
    // The group id takes precedence over the adr_id; the key format is not pinned.
    assert!(keys.into_iter().next().unwrap().ends_with(OAUTH_GROUP_ID));
}

#[test]
fn test_the_v2_fields_survive_a_serialization_round_trip() {
    let index = build_index(&corpus_root());
    let restored: ScopeIndex =
        serde_json::from_str(&serde_json::to_string(&index).unwrap()).unwrap();
    assert_eq!(restored, index);
    let web = restored
        .documents
        .iter()
        .find(|d| d.slug == WEB_COMPONENTS)
        .unwrap();
    assert_eq!(web.root, "packages/web");
    assert_eq!(web.include_globs, vec!["packages/web/src/components/**"]);
    assert_eq!(web.exclude_globs, vec!["packages/web/src/**/*.test.tsx"]);
    assert_eq!(web.languages, vec!["typescript"]);
    assert!(web.adr_id.is_some());
    assert_eq!(restored.inactive_documents, 1);
}

// ── determinism and offline operation ────────────────────────────────────

/// Retrieval takes no model and no network, so the same index and query must
/// give the same answer every time, and two builds of one rule set must be
/// identical.
#[test]
fn test_retrieval_is_deterministic_across_builds() {
    let root = corpus_root();
    let first = build_index(&root);
    let second = build_index(&root);
    assert_eq!(first, second, "two builds of one rule set differ");

    for case in golden_cases() {
        let query = Query::new(case.plan.clone()).with_paths(case.paths.clone());
        let expected = first.search(&query, 10);
        for _ in 0..3 {
            assert_eq!(first.search(&query, 10), expected, "case {}", case.name);
        }
        assert_eq!(second.search(&query, 10), expected, "case {}", case.name);
    }
}

#[test]
fn test_index_survives_a_serialization_round_trip() {
    let index = build_index(&corpus_root());
    let restored: ScopeIndex =
        serde_json::from_str(&serde_json::to_string(&index).unwrap()).unwrap();
    assert_eq!(restored, index);

    let query = Query::new("rotate the OAuth signing key");
    assert_eq!(restored.search(&query, 5), index.search(&query, 5));
}

/// The dead topic prefix is neutralized by the corpus itself, with nothing
/// naming it.
#[test]
fn test_the_shared_filename_prefix_carries_no_signal() {
    let index = build_index(&corpus_root());
    let ubiquitous = index.ubiquitous_terms();
    assert!(ubiquitous.contains(&"cross"));
    assert!(ubiquitous.contains(&"cutting"));
    assert_eq!(index.idf("cross"), 0.0);
    // A plan made only of the prefix selects nothing, rather than everything.
    assert!(index.search(&Query::new("cross cutting"), 10).is_empty());
}

// ── latency ──────────────────────────────────────────────────────────────

/// Build an index over a corpus the size of the reference one and hold it to an
/// interactive budget.
///
/// The corpus is synthesized here rather than committed: 425 files exist to
/// measure throughput, and committing them would add a megabyte of fixtures
/// that assert nothing the smaller corpus does not already assert.
///
/// The budget is generous on purpose. This runs in CI on shared hardware and in
/// a debug build, so a tight bound would fail for reasons that have nothing to
/// do with the index. It is set to catch a change in *kind* — an accidental
/// quadratic, a per-document re-scan of the corpus — not a change in constant.
#[test]
fn test_index_build_over_a_reference_sized_corpus_stays_interactive() {
    const DOCUMENTS: usize = 425;
    const BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

    let root = tempfile::tempdir().expect("temp dir");
    let dir = rules_dir(root.path());
    std::fs::create_dir_all(&dir).expect("create rules dir");
    for n in 0..DOCUMENTS {
        let body = format!(
            "# Adopt Something {n}: Aspect {n}\n\n\
             These rules are ALWAYS ACTIVE for module {n} in `services/module{n}/`, \
             covering its handlers, its configuration and its tests.\n\n\
             ### Rules\n\n\
             - **R-M{n}-001** MUST: do the thing.\n\
             - **R-M{n}-002** SHOULD: do the other thing.\n\n\
             ### Verify\n\n\
             ```bash\n\
             grep -r \"thing\" services/module{n}/ --include=\"*.ts\"\n\
             test -d services/module{n}/\n\
             ```\n"
        );
        std::fs::write(
            dir.join(format!("cross-cutting-module-{n}-a{n:03x}.md")),
            body,
        )
        .expect("write rule file");
    }

    let started = Instant::now();
    let report = load_rule_set(root.path()).expect("corpus loads");
    let index = ScopeIndex::build(&report, root.path(), "bench".to_string());
    let elapsed = started.elapsed();

    assert_eq!(index.len(), DOCUMENTS);
    assert!(
        elapsed < BUDGET,
        "index build over {DOCUMENTS} files took {elapsed:?}, over the {BUDGET:?} budget"
    );

    // A query against the built index must stay well inside a keystroke.
    //
    // It is anchored on a path: every document in this synthetic corpus shares
    // the same vocabulary, so every word in it is ubiquitous and scores zero —
    // which is the index behaving correctly, and why the timing query names a
    // directory instead.
    let started = Instant::now();
    let hits = index.search(
        &Query::new("change the handlers")
            .with_paths(vec!["services/module7/handler.ts".to_string()]),
        10,
    );
    let query_elapsed = started.elapsed();
    assert!(!hits.is_empty());
    assert!(
        query_elapsed < std::time::Duration::from_millis(250),
        "query took {query_elapsed:?}"
    );
}
