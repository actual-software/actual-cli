//! Precision and recall for a rule selection, and the golden-set case it is
//! measured on.
//!
//! # Design
//!
//! The acceptance bar for this work is a number, not an impression: the index
//! must beat the filename scan on a fixed set of plans. That means the metric
//! has to be library code — the same function scoring both selectors, callable
//! from tests, from a benchmark, and from `rules eval` — rather than arithmetic
//! written inline in whichever test happens to need it.
//!
//! Both aggregates are reported because they answer different questions.
//! **Micro** pools every case's hits and misses, so a case with many expected
//! rules counts for more; it is the "how much of the corpus did we get right"
//! number. **Macro** averages the per-case scores, so every plan counts the
//! same; it is the "how well does a typical plan do" number. Quoting only one
//! of them hides the shape of the failure.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// One golden case: a plan, and the rule files that should be selected for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenCase {
    /// Short identifier, used in failure messages.
    pub name: String,
    /// The plan text, as a developer would write it before touching any code.
    pub plan: String,
    /// Paths the plan already names, when it names any. Usually empty: at plan
    /// time there is no diff.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Slugs of the rule documents that genuinely govern this plan.
    pub expected: Vec<String>,
}

/// Precision, recall and F1 for one selection against one expectation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Scores {
    pub true_positives: usize,
    pub false_positives: usize,
    pub false_negatives: usize,
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
}

impl Scores {
    /// Score `selected` against `expected`. Order and duplicates are ignored:
    /// this measures the set that was chosen, not the ranking within it.
    pub fn measure(selected: &[String], expected: &[String]) -> Self {
        let selected: BTreeSet<&str> = selected.iter().map(String::as_str).collect();
        let expected: BTreeSet<&str> = expected.iter().map(String::as_str).collect();
        let true_positives = selected.intersection(&expected).count();
        let false_positives = selected.len() - true_positives;
        let false_negatives = expected.len() - true_positives;
        Self::from_counts(true_positives, false_positives, false_negatives)
    }

    fn from_counts(true_positives: usize, false_positives: usize, false_negatives: usize) -> Self {
        // Precision of an empty selection is 1.0 by convention: abstention is
        // not punished, because nothing false was returned. That is not a
        // claim of a perfect retrieval — when anything was expected, recall
        // and F1 still fall to 0. The other 0/0, empty against empty, is
        // genuinely perfect: nothing was asked for and nothing was wrongly
        // returned.
        let precision = ratio(true_positives, true_positives + false_positives);
        let recall = ratio(true_positives, true_positives + false_negatives);
        let f1 = if precision + recall > 0.0 {
            2.0 * precision * recall / (precision + recall)
        } else {
            0.0
        };
        Self {
            true_positives,
            false_positives,
            false_negatives,
            precision,
            recall,
            f1,
        }
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    // 0/0 is 1.0 so an empty selection is not reported as precision 0. See
    // `Scores::from_counts` for why that is a convention, not a real 1.0.
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

/// Per-case and aggregate results for one selector over a whole golden set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationReport {
    /// What was measured, e.g. `scope-index` or `filename-scan`.
    pub selector: String,
    pub cases: Vec<CaseResult>,
    /// Hits and misses pooled across cases: weights each case by how many rules
    /// it expects.
    pub micro: Scores,
    /// The per-case scores averaged: weights every plan equally.
    pub macro_precision: f64,
    pub macro_recall: f64,
    pub macro_f1: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseResult {
    pub name: String,
    pub selected: Vec<String>,
    pub expected: Vec<String>,
    pub scores: Scores,
}

impl EvaluationReport {
    /// Aggregate per-case results under a selector name.
    pub fn new(selector: impl Into<String>, cases: Vec<CaseResult>) -> Self {
        let (mut tp, mut fp, mut fneg) = (0usize, 0usize, 0usize);
        let (mut precision, mut recall, mut f1) = (0.0, 0.0, 0.0);
        for case in &cases {
            tp += case.scores.true_positives;
            fp += case.scores.false_positives;
            fneg += case.scores.false_negatives;
            precision += case.scores.precision;
            recall += case.scores.recall;
            f1 += case.scores.f1;
        }
        let n = cases.len().max(1) as f64;
        Self {
            selector: selector.into(),
            micro: Scores::from_counts(tp, fp, fneg),
            macro_precision: precision / n,
            macro_recall: recall / n,
            macro_f1: f1 / n,
            cases,
        }
    }

    /// Cases where nothing expected was found — the failures worth reading
    /// first, because a plan that retrieves no governing rule is the failure
    /// the whole exercise is meant to prevent.
    pub fn total_misses(&self) -> Vec<&CaseResult> {
        self.cases
            .iter()
            .filter(|case| case.scores.true_positives == 0 && !case.expected.is_empty())
            .collect()
    }

    /// A single line: `scope-index  micro P 0.82 R 0.91 F1 0.86  macro F1 0.88`.
    pub fn summary_line(&self) -> String {
        format!(
            "{:<14} micro P {:.2} R {:.2} F1 {:.2}  macro P {:.2} R {:.2} F1 {:.2}",
            self.selector,
            self.micro.precision,
            self.micro.recall,
            self.micro.f1,
            self.macro_precision,
            self.macro_recall,
            self.macro_f1,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: a case result with the given selection and expectation.
    fn case(name: &str, selected: &[&str], expected: &[&str]) -> CaseResult {
        let selected: Vec<String> = selected.iter().map(|s| s.to_string()).collect();
        let expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        CaseResult {
            name: name.to_string(),
            scores: Scores::measure(&selected, &expected),
            selected,
            expected,
        }
    }

    // ── Scores ───────────────────────────────────────────────────────────

    #[test]
    fn test_measure_a_perfect_selection() {
        let scores = Scores::measure(
            &["a".to_string(), "b".to_string()],
            &["a".to_string(), "b".to_string()],
        );
        assert_eq!(scores.true_positives, 2);
        assert_eq!(scores.false_positives, 0);
        assert_eq!(scores.false_negatives, 0);
        assert_eq!(scores.precision, 1.0);
        assert_eq!(scores.recall, 1.0);
        assert_eq!(scores.f1, 1.0);
    }

    #[test]
    fn test_measure_a_partial_selection() {
        let scores = Scores::measure(
            &["a".to_string(), "x".to_string()],
            &["a".to_string(), "b".to_string()],
        );
        assert_eq!(scores.true_positives, 1);
        assert_eq!(scores.false_positives, 1);
        assert_eq!(scores.false_negatives, 1);
        assert_eq!(scores.precision, 0.5);
        assert_eq!(scores.recall, 0.5);
        assert_eq!(scores.f1, 0.5);
    }

    #[test]
    fn test_measure_a_total_miss_scores_zero() {
        let scores = Scores::measure(&["x".to_string()], &["a".to_string()]);
        assert_eq!(scores.true_positives, 0);
        assert_eq!(scores.precision, 0.0);
        assert_eq!(scores.recall, 0.0);
        assert_eq!(scores.f1, 0.0);
    }

    /// Order and duplicates are irrelevant: this measures the chosen set, not
    /// the ranking inside it.
    #[test]
    fn test_measure_ignores_order_and_duplicates() {
        let a = Scores::measure(
            &["b".to_string(), "a".to_string()],
            &["a".to_string(), "b".to_string()],
        );
        let b = Scores::measure(
            &["a".to_string(), "a".to_string(), "b".to_string()],
            &["b".to_string(), "a".to_string()],
        );
        assert_eq!(a.f1, 1.0);
        assert_eq!(b.f1, 1.0);
    }

    /// Nothing asked for and nothing wrongly returned is perfect, not
    /// undefined — otherwise a division by zero silently reports failure.
    #[test]
    fn test_measure_empty_against_empty_is_perfect() {
        let scores = Scores::measure(&[], &[]);
        assert_eq!(scores.precision, 1.0);
        assert_eq!(scores.recall, 1.0);
        assert_eq!(scores.f1, 1.0);
    }

    /// Abstention is not punished: returning nothing scores precision 1.0
    /// because there were no false positives. That is a convention, not a
    /// claim of a perfect retrieval — recall and F1 are still 0.
    #[test]
    fn test_measure_empty_selection_against_an_expectation() {
        let scores = Scores::measure(&[], &["a".to_string()]);
        assert_eq!(scores.precision, 1.0);
        assert_eq!(scores.recall, 0.0);
        assert_eq!(scores.f1, 0.0);
    }

    // ── aggregation ──────────────────────────────────────────────────────

    /// Micro weights a case by how many rules it expects; macro weights every
    /// plan the same. On a lopsided set the two differ, which is why both are
    /// reported.
    #[test]
    fn test_micro_and_macro_differ_on_a_lopsided_set() {
        let report = EvaluationReport::new(
            "test",
            vec![
                // Four expected, all found.
                case("big", &["a", "b", "c", "d"], &["a", "b", "c", "d"]),
                // One expected, missed.
                case("small", &["z"], &["y"]),
            ],
        );
        assert_eq!(report.micro.true_positives, 4);
        assert_eq!(report.micro.false_positives, 1);
        assert_eq!(report.micro.false_negatives, 1);
        assert_eq!(report.micro.precision, 0.8);
        assert_eq!(report.micro.recall, 0.8);
        // Two cases, equal weight: (1.0 + 0.0) / 2. A third case would
        // change this; it is not a general "halve the perfect score" rule.
        assert_eq!(report.macro_precision, 0.5);
        assert_eq!(report.macro_recall, 0.5);
        assert_eq!(report.macro_f1, 0.5);
    }

    #[test]
    fn test_report_of_an_empty_case_list() {
        let report = EvaluationReport::new("test", Vec::new());
        assert_eq!(report.micro.f1, 1.0);
        assert_eq!(report.macro_f1, 0.0);
        assert!(report.total_misses().is_empty());
    }

    #[test]
    fn test_total_misses_lists_only_plans_that_found_nothing() {
        let report = EvaluationReport::new(
            "test",
            vec![
                case("found", &["a"], &["a"]),
                case("missed", &["z"], &["y"]),
                // Nothing expected cannot be a miss.
                case("nothing-expected", &[], &[]),
            ],
        );
        let misses: Vec<&str> = report
            .total_misses()
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(misses, vec!["missed"]);
    }

    #[test]
    fn test_summary_line_names_the_selector_and_both_aggregates() {
        let report = EvaluationReport::new("scope-index", vec![case("a", &["a"], &["a"])]);
        let line = report.summary_line();
        assert!(line.starts_with("scope-index"));
        assert!(line.contains("micro P 1.00 R 1.00 F1 1.00"));
        assert!(line.contains("macro P 1.00 R 1.00 F1 1.00"));
    }

    // ── golden case shape ────────────────────────────────────────────────

    /// `paths` is optional: at plan time there is usually no diff, so most
    /// cases omit it and must still deserialize.
    #[test]
    fn test_golden_case_deserializes_without_paths() {
        let case: GoldenCase = serde_json::from_str(
            r#"{"name": "a", "plan": "do the thing", "expected": ["rule-a"]}"#,
        )
        .unwrap();
        assert!(case.paths.is_empty());
        assert_eq!(case.expected, vec!["rule-a"]);
    }

    #[test]
    fn test_golden_case_round_trips_with_paths() {
        let case = GoldenCase {
            name: "a".to_string(),
            plan: "do the thing".to_string(),
            paths: vec!["src/x.rs".to_string()],
            expected: vec!["rule-a".to_string()],
        };
        let back: GoldenCase =
            serde_json::from_str(&serde_json::to_string(&case).unwrap()).unwrap();
        assert_eq!(back, case);
    }

    #[test]
    fn test_report_is_serializable() {
        let report = EvaluationReport::new("scope-index", vec![case("a", &["a"], &["a", "b"])]);
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&report).unwrap()).unwrap();
        assert_eq!(value["selector"], "scope-index");
        assert_eq!(value["cases"][0]["scores"]["false_negatives"], 1);
        assert!(value["micro"]["f1"].is_number());
    }
}
