use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

use super::{EvidenceSpan, SignalSource, ToolMatch};

/// Current taxonomy version for v2 IR.
const TAXONOMY_VERSION_V2: &str = "arch_ir_taxonomy_v2";

/// Maximum number of values to keep per leaf to prevent oversized IR.
const MAX_VALUES_PER_LEAF: usize = 10;

/// Maximum number of spans to keep per leaf to prevent oversized IR payloads.
const MAX_SPANS_PER_LEAF: usize = 50;

/// Summary of an evidence span for the IR output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpanSummary {
    pub file_path: String,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub source: SignalSource,
}

impl SpanSummary {
    /// Construct a `SpanSummary` from an `EvidenceSpan`.
    pub fn from_evidence_span(span: &EvidenceSpan) -> Self {
        Self {
            file_path: span.file_path.clone(),
            start_line: span.start_line,
            end_line: span.end_line,
            source: span.source.clone(),
        }
    }
}

/// Aggregated facet data for a single leaf_id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FacetData {
    pub facet_slot: String,
    pub values: Vec<serde_json::Value>,
    pub confidence: f64,
    pub evidence_count: usize,
    pub rule_ids: Vec<String>,
    pub spans: Vec<SpanSummary>,
}

/// Canonical intermediate representation built from ToolMatch signals.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CanonicalIR {
    pub ir_text: String,
    pub facets_by_leaf_id: HashMap<String, FacetData>,
    pub ir_hash: String,
    pub taxonomy_version: String,
    pub match_count: usize,
    pub leaf_count: usize,
}

/// Compute the SHA-256 hex digest of a string.
fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Intermediate bucket used during aggregation.
struct LeafBucket {
    facet_slot: String,
    values: Vec<serde_json::Value>,
    confidence: f64,
    rule_ids: Vec<String>,
    seen_rule_ids: HashSet<String>,
    evidence_count: usize,
    spans: Vec<SpanSummary>,
}

/// Aggregate matches by leaf_id into buckets.
fn aggregate_matches_by_leaf(matches: &[ToolMatch]) -> HashMap<String, LeafBucket> {
    let mut buckets: HashMap<String, LeafBucket> = HashMap::new();

    for m in matches {
        let bucket = buckets
            .entry(m.leaf_id.clone())
            .or_insert_with(|| LeafBucket {
                facet_slot: m.facet_slot.clone(),
                values: Vec::new(),
                confidence: 0.0,
                rule_ids: Vec::new(),
                seen_rule_ids: HashSet::new(),
                evidence_count: 0,
                spans: Vec::new(),
            });

        // Max confidence across all matches in the bucket.
        if m.confidence > bucket.confidence {
            bucket.confidence = m.confidence;
        }

        // Collect unique rule_ids (HashSet for O(1) dedup, Vec for insertion order).
        if bucket.seen_rule_ids.insert(m.rule_id.clone()) {
            bucket.rule_ids.push(m.rule_id.clone());
        }

        // Collect values, capped at MAX_VALUES_PER_LEAF.
        // Strings are deduped; non-null non-string values (objects, arrays, etc.)
        // are appended without dedup but still subject to the cap.
        if bucket.values.len() < MAX_VALUES_PER_LEAF {
            if m.value.is_string() {
                if !bucket.values.contains(&m.value) {
                    bucket.values.push(m.value.clone());
                }
            } else if !m.value.is_null() {
                bucket.values.push(m.value.clone());
            }
        }

        // Track total evidence count (uncapped) for accurate reporting.
        bucket.evidence_count += m.spans.len();

        // Collect span summaries, capped to prevent oversized IR.
        for span in &m.spans {
            if bucket.spans.len() < MAX_SPANS_PER_LEAF {
                bucket.spans.push(SpanSummary::from_evidence_span(span));
            }
        }
    }

    buckets
}

/// Format a single value for IR text output.
fn format_value(value: &serde_json::Value) -> String {
    if let Some(s) = value.as_str() {
        s.to_string()
    } else {
        value.to_string()
    }
}

/// Build a canonical IR from a set of ToolMatch signals for a single file.
///
/// The algorithm:
/// 1. Aggregate matches by leaf_id
/// 2. Per leaf: collect values (dedup strings), track rule_ids, max confidence, count spans
/// 3. Limit values to 10 per leaf to prevent oversized IR
/// 4. Build deterministic IR text (sorted by leaf_id numeric order)
/// 5. Hash the text with SHA-256
pub fn build_canonical_ir(file_key: &str, language: &str, matches: &[ToolMatch]) -> CanonicalIR {
    let buckets = aggregate_matches_by_leaf(matches);

    // Sort leaf_ids: numeric IDs first (ascending), then non-numeric IDs lexicographically.
    // Uses Option<u64> to distinguish parsed numerics from non-numeric fallbacks,
    // avoiding ambiguity if a leaf_id is literally u64::MAX.
    let mut sorted_leaf_ids: Vec<&String> = buckets.keys().collect();
    sorted_leaf_ids.sort_by(|a, b| {
        let ka = a.parse::<u64>().ok();
        let kb = b.parse::<u64>().ok();
        match (ka, kb) {
            (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.cmp(b)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.cmp(b),
        }
    });

    // Build facets_by_leaf_id map and IR text simultaneously.
    let mut facets_by_leaf_id = HashMap::new();
    let mut ir_lines = Vec::new();

    ir_lines.push("IRv2".to_string());
    ir_lines.push(format!("FILE: {file_key}"));
    ir_lines.push(format!("LANGUAGE: {language}"));
    ir_lines.push(format!("TAXONOMY: {TAXONOMY_VERSION_V2}"));

    for leaf_id in &sorted_leaf_ids {
        let bucket = &buckets[*leaf_id];

        let facet_data = FacetData {
            facet_slot: bucket.facet_slot.clone(),
            values: bucket.values.clone(),
            confidence: bucket.confidence,
            evidence_count: bucket.evidence_count,
            rule_ids: bucket.rule_ids.clone(),
            spans: bucket.spans.clone(),
        };

        // Blank line before each LEAF block.
        ir_lines.push(String::new());
        ir_lines.push(format!(
            "LEAF[{leaf_id}] {} (conf={:.3}):",
            facet_data.facet_slot, facet_data.confidence,
        ));

        if facet_data.values.is_empty() {
            ir_lines.push("  - <detected>".to_string());
        } else {
            for value in &facet_data.values {
                ir_lines.push(format!("  - {}", format_value(value)));
            }
        }

        facets_by_leaf_id.insert((*leaf_id).clone(), facet_data);
    }

    // IR text ends with a single newline.
    let ir_text = ir_lines.join("\n") + "\n";
    let ir_hash = sha256_hex(&ir_text);

    CanonicalIR {
        ir_text,
        facets_by_leaf_id,
        ir_hash,
        taxonomy_version: TAXONOMY_VERSION_V2.to_string(),
        match_count: matches.len(),
        leaf_count: sorted_leaf_ids.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::signals::{EvidenceSpan, SignalSource, ToolMatch};

    /// Helper to create a basic ToolMatch for testing.
    fn make_match(
        rule_id: &str,
        facet_slot: &str,
        leaf_id: &str,
        value: serde_json::Value,
        confidence: f64,
    ) -> ToolMatch {
        ToolMatch {
            rule_id: rule_id.to_string(),
            facet_slot: facet_slot.to_string(),
            leaf_id: leaf_id.to_string(),
            value,
            confidence,
            spans: vec![EvidenceSpan {
                file_path: "src/main.rs".to_string(),
                start_byte: 0,
                end_byte: 100,
                start_line: Some(1),
                end_line: Some(5),
                source: SignalSource::Semgrep,
            }],
            raw: None,
        }
    }

    /// Helper to create a ToolMatch with custom spans.
    fn make_match_with_spans(
        rule_id: &str,
        facet_slot: &str,
        leaf_id: &str,
        value: serde_json::Value,
        confidence: f64,
        spans: Vec<EvidenceSpan>,
    ) -> ToolMatch {
        ToolMatch {
            rule_id: rule_id.to_string(),
            facet_slot: facet_slot.to_string(),
            leaf_id: leaf_id.to_string(),
            value,
            confidence,
            spans,
            raw: None,
        }
    }

    #[test]
    fn test_empty_matches() {
        let ir = build_canonical_ir("src/main.rs", "rust", &[]);
        assert_eq!(ir.match_count, 0);
        assert_eq!(ir.leaf_count, 0);
        assert!(ir.facets_by_leaf_id.is_empty());
        assert!(ir.ir_text.starts_with("IRv2\n"));
        assert!(ir.ir_text.contains("FILE: src/main.rs"));
        assert!(ir.ir_text.contains("LANGUAGE: rust"));
        assert!(ir
            .ir_text
            .contains(&format!("TAXONOMY: {TAXONOMY_VERSION_V2}")));
        assert!(!ir.ir_hash.is_empty());
    }

    #[test]
    fn test_single_match() {
        let matches = vec![make_match(
            "ts.rust.pub_fn",
            "api.public",
            "25",
            serde_json::json!("pub fn hello()"),
            0.95,
        )];
        let ir = build_canonical_ir("src/lib.rs", "rust", &matches);
        assert_eq!(ir.match_count, 1);
        assert_eq!(ir.leaf_count, 1);
        assert!(ir.facets_by_leaf_id.contains_key("25"));

        let facet = &ir.facets_by_leaf_id["25"];
        assert_eq!(facet.facet_slot, "api.public");
        assert_eq!(facet.values.len(), 1);
        assert_eq!(facet.values[0], serde_json::json!("pub fn hello()"));
        assert!((facet.confidence - 0.95).abs() < f64::EPSILON);
        assert_eq!(facet.evidence_count, 1);
        assert_eq!(facet.rule_ids, vec!["ts.rust.pub_fn"]);

        assert!(ir.ir_text.contains("LEAF[25] api.public (conf=0.950):"));
        assert!(ir.ir_text.contains("  - pub fn hello()"));
    }

    #[test]
    fn test_multiple_matches_same_leaf() {
        let matches = vec![
            make_match("rule1", "slot_a", "10", serde_json::json!("value_a"), 0.8),
            make_match("rule2", "slot_b", "10", serde_json::json!("value_b"), 0.95),
        ];
        let ir = build_canonical_ir("test.rs", "rust", &matches);
        assert_eq!(ir.leaf_count, 1);
        assert_eq!(ir.match_count, 2);

        let facet = &ir.facets_by_leaf_id["10"];
        // Max confidence.
        assert!((facet.confidence - 0.95).abs() < f64::EPSILON);
        // Both string values (different, so both kept).
        assert_eq!(facet.values.len(), 2);
        // First facet_slot seen.
        assert_eq!(facet.facet_slot, "slot_a");
        // Both spans accumulated.
        assert_eq!(facet.evidence_count, 2);
        // Both rule_ids.
        assert_eq!(facet.rule_ids.len(), 2);
    }

    #[test]
    fn test_multiple_matches_different_leaves() {
        let matches = vec![
            make_match("rule1", "slot_a", "5", serde_json::json!("val1"), 0.9),
            make_match("rule2", "slot_b", "3", serde_json::json!("val2"), 0.8),
        ];
        let ir = build_canonical_ir("test.rs", "rust", &matches);
        assert_eq!(ir.leaf_count, 2);

        // Verify numeric sort: leaf 3 should appear before leaf 5 in text.
        let pos_3 = ir.ir_text.find("LEAF[3]").unwrap();
        let pos_5 = ir.ir_text.find("LEAF[5]").unwrap();
        assert!(pos_3 < pos_5);
    }

    #[test]
    fn test_deterministic_hash() {
        let matches = vec![
            make_match("rule1", "slot_a", "1", serde_json::json!("v1"), 0.9),
            make_match("rule2", "slot_b", "2", serde_json::json!("v2"), 0.8),
        ];
        let ir1 = build_canonical_ir("f.rs", "rust", &matches);
        let ir2 = build_canonical_ir("f.rs", "rust", &matches);
        assert_eq!(ir1.ir_hash, ir2.ir_hash);
        assert_eq!(ir1.ir_text, ir2.ir_text);
    }

    #[test]
    fn test_different_input_different_hash() {
        let m1 = vec![make_match("r1", "s1", "1", serde_json::json!("a"), 0.9)];
        let m2 = vec![make_match("r1", "s1", "1", serde_json::json!("b"), 0.9)];
        let ir1 = build_canonical_ir("f.rs", "rust", &m1);
        let ir2 = build_canonical_ir("f.rs", "rust", &m2);
        assert_ne!(ir1.ir_hash, ir2.ir_hash);
    }

    #[test]
    fn test_values_limited_to_ten() {
        let matches: Vec<ToolMatch> = (0..15)
            .map(|i| {
                make_match(
                    "rule",
                    "slot",
                    "1",
                    serde_json::json!(format!("value_{i}")),
                    0.9,
                )
            })
            .collect();
        let ir = build_canonical_ir("f.rs", "rust", &matches);
        let facet = &ir.facets_by_leaf_id["1"];
        assert_eq!(facet.values.len(), MAX_VALUES_PER_LEAF);
    }

    #[test]
    fn test_string_value_dedup() {
        let matches = vec![
            make_match("rule1", "slot", "1", serde_json::json!("same"), 0.9),
            make_match("rule2", "slot", "1", serde_json::json!("same"), 0.8),
            make_match("rule3", "slot", "1", serde_json::json!("different"), 0.7),
        ];
        let ir = build_canonical_ir("f.rs", "rust", &matches);
        let facet = &ir.facets_by_leaf_id["1"];
        assert_eq!(facet.values.len(), 2);
        assert_eq!(facet.values[0], serde_json::json!("same"));
        assert_eq!(facet.values[1], serde_json::json!("different"));
    }

    #[test]
    fn test_dict_values_not_deduped() {
        let obj = serde_json::json!({"key": "val"});
        let matches = vec![
            make_match("rule1", "slot", "1", obj.clone(), 0.9),
            make_match("rule2", "slot", "1", obj.clone(), 0.8),
        ];
        let ir = build_canonical_ir("f.rs", "rust", &matches);
        let facet = &ir.facets_by_leaf_id["1"];
        // Object values are not deduped (but still subject to MAX_VALUES_PER_LEAF cap).
        assert_eq!(facet.values.len(), 2);
    }

    #[test]
    fn test_no_values_shows_detected() {
        // Null values are skipped, so leaf ends up with no values.
        let matches = vec![make_match(
            "rule",
            "slot",
            "1",
            serde_json::Value::Null,
            0.9,
        )];
        let ir = build_canonical_ir("f.rs", "rust", &matches);
        let facet = &ir.facets_by_leaf_id["1"];
        assert!(facet.values.is_empty());
        assert!(ir.ir_text.contains("  - <detected>"));
    }

    #[test]
    fn test_confidence_is_max() {
        let matches = vec![
            make_match("r1", "s", "1", serde_json::json!("a"), 0.3),
            make_match("r2", "s", "1", serde_json::json!("b"), 0.99),
            make_match("r3", "s", "1", serde_json::json!("c"), 0.5),
        ];
        let ir = build_canonical_ir("f.rs", "rust", &matches);
        let facet = &ir.facets_by_leaf_id["1"];
        assert!((facet.confidence - 0.99).abs() < f64::EPSILON);
    }

    #[test]
    fn test_confidence_rounded_in_text() {
        let matches = vec![make_match(
            "rule",
            "slot",
            "1",
            serde_json::json!("v"),
            0.123456,
        )];
        let ir = build_canonical_ir("f.rs", "rust", &matches);
        assert!(ir.ir_text.contains("conf=0.123)"));
    }

    #[test]
    fn test_leaf_id_sorted_numerically() {
        let matches = vec![
            make_match("r1", "s", "10", serde_json::json!("a"), 0.9),
            make_match("r2", "s", "2", serde_json::json!("b"), 0.9),
            make_match("r3", "s", "1", serde_json::json!("c"), 0.9),
        ];
        let ir = build_canonical_ir("f.rs", "rust", &matches);

        let pos_1 = ir.ir_text.find("LEAF[1]").unwrap();
        let pos_2 = ir.ir_text.find("LEAF[2]").unwrap();
        let pos_10 = ir.ir_text.find("LEAF[10]").unwrap();
        assert!(pos_1 < pos_2);
        assert!(pos_2 < pos_10);
    }

    #[test]
    fn test_ir_text_format() {
        let matches = vec![
            make_match("r1", "api.public", "1", serde_json::json!("fn foo()"), 0.9),
            make_match("r2", "deps.external", "5", serde_json::json!("serde"), 0.85),
        ];
        let ir = build_canonical_ir("src/lib.rs", "rust", &matches);

        let expected = format!(
            "IRv2\n\
             FILE: src/lib.rs\n\
             LANGUAGE: rust\n\
             TAXONOMY: {TAXONOMY_VERSION_V2}\n\
             \n\
             LEAF[1] api.public (conf=0.900):\n\
             \x20 - fn foo()\n\
             \n\
             LEAF[5] deps.external (conf=0.850):\n\
             \x20 - serde\n"
        );
        assert_eq!(ir.ir_text, expected);
    }

    #[test]
    fn test_span_summary_construction() {
        let span = EvidenceSpan {
            file_path: "src/main.rs".to_string(),
            start_byte: 10,
            end_byte: 50,
            start_line: Some(3),
            end_line: Some(7),
            source: SignalSource::TreeSitter,
        };
        let summary = SpanSummary::from_evidence_span(&span);
        assert_eq!(summary.file_path, "src/main.rs");
        assert_eq!(summary.start_line, Some(3));
        assert_eq!(summary.end_line, Some(7));
        assert_eq!(summary.source, SignalSource::TreeSitter);
    }

    #[test]
    fn test_multiple_spans_per_match() {
        let spans = vec![
            EvidenceSpan {
                file_path: "a.rs".to_string(),
                start_byte: 0,
                end_byte: 10,
                start_line: Some(1),
                end_line: Some(2),
                source: SignalSource::Semgrep,
            },
            EvidenceSpan {
                file_path: "b.rs".to_string(),
                start_byte: 20,
                end_byte: 30,
                start_line: Some(5),
                end_line: Some(6),
                source: SignalSource::TreeSitter,
            },
        ];
        let matches = vec![make_match_with_spans(
            "rule",
            "slot",
            "1",
            serde_json::json!("v"),
            0.9,
            spans,
        )];
        let ir = build_canonical_ir("f.rs", "rust", &matches);
        let facet = &ir.facets_by_leaf_id["1"];
        assert_eq!(facet.evidence_count, 2);
        assert_eq!(facet.spans.len(), 2);
        assert_eq!(facet.spans[0].file_path, "a.rs");
        assert_eq!(facet.spans[1].file_path, "b.rs");
    }

    #[test]
    fn test_spans_limited_to_max() {
        // Create matches that would produce more spans than the cap.
        let matches: Vec<ToolMatch> = (0..60)
            .map(|i| {
                make_match_with_spans(
                    "rule",
                    "slot",
                    "1",
                    serde_json::json!(format!("v{i}")),
                    0.9,
                    vec![EvidenceSpan {
                        file_path: format!("file_{i}.rs"),
                        start_byte: 0,
                        end_byte: 10,
                        start_line: Some(1),
                        end_line: Some(2),
                        source: SignalSource::Semgrep,
                    }],
                )
            })
            .collect();
        let ir = build_canonical_ir("f.rs", "rust", &matches);
        let facet = &ir.facets_by_leaf_id["1"];
        assert_eq!(facet.spans.len(), MAX_SPANS_PER_LEAF);
        // evidence_count tracks the true total, not the capped span count.
        assert_eq!(facet.evidence_count, 60);
    }

    #[test]
    fn test_facet_slot_uses_first_seen() {
        let matches = vec![
            make_match("r1", "first_slot", "1", serde_json::json!("a"), 0.9),
            make_match("r2", "second_slot", "1", serde_json::json!("b"), 0.8),
        ];
        let ir = build_canonical_ir("f.rs", "rust", &matches);
        let facet = &ir.facets_by_leaf_id["1"];
        assert_eq!(facet.facet_slot, "first_slot");
    }

    #[test]
    fn test_rule_ids_deduped() {
        let matches = vec![
            make_match("same_rule", "slot", "1", serde_json::json!("a"), 0.9),
            make_match("same_rule", "slot", "1", serde_json::json!("b"), 0.8),
            make_match("other_rule", "slot", "1", serde_json::json!("c"), 0.7),
        ];
        let ir = build_canonical_ir("f.rs", "rust", &matches);
        let facet = &ir.facets_by_leaf_id["1"];
        assert_eq!(facet.rule_ids.len(), 2);
        assert!(facet.rule_ids.contains(&"same_rule".to_string()));
        assert!(facet.rule_ids.contains(&"other_rule".to_string()));
    }

    #[test]
    fn test_sha256_hex() {
        let hash = sha256_hex("hello");
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn test_canonical_ir_serialization() {
        let matches = vec![make_match(
            "rule",
            "slot",
            "1",
            serde_json::json!("val"),
            0.9,
        )];
        let ir = build_canonical_ir("f.rs", "rust", &matches);
        let json = serde_json::to_string(&ir).unwrap();
        let deserialized: CanonicalIR = serde_json::from_str(&json).unwrap();
        assert_eq!(ir, deserialized);
    }

    #[test]
    fn test_facet_data_serialization_roundtrip() {
        let facet = FacetData {
            facet_slot: "api.public".to_string(),
            values: vec![serde_json::json!("fn foo()"), serde_json::json!(42)],
            confidence: 0.95,
            evidence_count: 3,
            rule_ids: vec!["r1".to_string(), "r2".to_string()],
            spans: vec![SpanSummary {
                file_path: "src/lib.rs".to_string(),
                start_line: Some(1),
                end_line: Some(5),
                source: SignalSource::Semgrep,
            }],
        };
        let json = serde_json::to_string(&facet).unwrap();
        let deserialized: FacetData = serde_json::from_str(&json).unwrap();
        assert_eq!(facet, deserialized);
    }

    #[test]
    fn test_non_numeric_leaf_ids_sorted_deterministically() {
        let matches = vec![
            make_match("r1", "s", "beta", serde_json::json!("b"), 0.9),
            make_match("r2", "s", "alpha", serde_json::json!("a"), 0.9),
            make_match("r3", "s", "3", serde_json::json!("c"), 0.9),
        ];
        let ir1 = build_canonical_ir("f.rs", "rust", &matches);
        let ir2 = build_canonical_ir("f.rs", "rust", &matches);
        // Hash must be deterministic even with non-numeric leaf IDs.
        assert_eq!(ir1.ir_hash, ir2.ir_hash);
        // Numeric leaf "3" should come first, then "alpha" < "beta" lexicographically.
        let pos_3 = ir1.ir_text.find("LEAF[3]").unwrap();
        let pos_alpha = ir1.ir_text.find("LEAF[alpha]").unwrap();
        let pos_beta = ir1.ir_text.find("LEAF[beta]").unwrap();
        assert!(pos_3 < pos_alpha);
        assert!(pos_alpha < pos_beta);
    }

    #[test]
    fn test_u64_max_leaf_id_sorts_correctly() {
        // A leaf_id that is literally u64::MAX should sort as a numeric value,
        // not collide with non-numeric IDs.
        let max_str = u64::MAX.to_string();
        let matches = vec![
            make_match("r1", "s", &max_str, serde_json::json!("max"), 0.9),
            make_match("r2", "s", "alpha", serde_json::json!("a"), 0.9),
            make_match("r3", "s", "5", serde_json::json!("five"), 0.9),
        ];
        let ir = build_canonical_ir("f.rs", "rust", &matches);
        // Numeric 5 first, then numeric u64::MAX, then non-numeric "alpha".
        let pos_5 = ir.ir_text.find("LEAF[5]").unwrap();
        let pos_max = ir.ir_text.find(&format!("LEAF[{max_str}]")).unwrap();
        let pos_alpha = ir.ir_text.find("LEAF[alpha]").unwrap();
        assert!(pos_5 < pos_max, "5 should sort before u64::MAX");
        assert!(
            pos_max < pos_alpha,
            "u64::MAX (numeric) should sort before non-numeric"
        );
    }

    #[test]
    fn test_span_summary_serialization_roundtrip() {
        let summary = SpanSummary {
            file_path: "test.rs".to_string(),
            start_line: Some(10),
            end_line: Some(20),
            source: SignalSource::Manifest,
        };
        let json = serde_json::to_string(&summary).unwrap();
        let deserialized: SpanSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(summary, deserialized);
    }
}
