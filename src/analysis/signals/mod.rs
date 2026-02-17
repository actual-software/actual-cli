pub mod language_resolver;
pub mod rule_resolver;
pub mod semgrep;
pub mod tree_sitter;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Source of a signal detection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SignalSource {
    #[serde(rename = "tree_sitter")]
    TreeSitter,
    #[serde(rename = "semgrep")]
    Semgrep,
    #[serde(rename = "manifest")]
    Manifest,
}

/// A span of source code that provides evidence for a signal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvidenceSpan {
    pub file_path: String,
    pub start_byte: usize,
    pub end_byte: usize,
    /// 1-indexed line number.
    pub start_line: Option<usize>,
    /// 1-indexed line number.
    pub end_line: Option<usize>,
    pub source: SignalSource,
}

/// A single match from a detection tool (semgrep, tree-sitter, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolMatch {
    pub rule_id: String,
    pub facet_slot: String,
    pub leaf_id: String,
    pub value: serde_json::Value,
    pub confidence: f64,
    pub spans: Vec<EvidenceSpan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<serde_json::Value>,
}

/// Aggregated architecture signals from all detection tools.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArchitectureSignals {
    pub matches: Vec<ToolMatch>,
    pub files_analyzed: usize,
    pub queries_executed: usize,
}

impl ArchitectureSignals {
    /// Group matches by their `leaf_id`.
    pub fn matches_by_leaf_id(&self) -> HashMap<&str, Vec<&ToolMatch>> {
        let mut result: HashMap<&str, Vec<&ToolMatch>> = HashMap::new();
        for m in &self.matches {
            result.entry(m.leaf_id.as_str()).or_default().push(m);
        }
        result
    }

    /// Group matches by their `facet_slot`.
    pub fn matches_by_facet_slot(&self) -> HashMap<&str, Vec<&ToolMatch>> {
        let mut result: HashMap<&str, Vec<&ToolMatch>> = HashMap::new();
        for m in &self.matches {
            result.entry(m.facet_slot.as_str()).or_default().push(m);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_evidence_span() -> EvidenceSpan {
        EvidenceSpan {
            file_path: "src/main.rs".to_string(),
            start_byte: 0,
            end_byte: 100,
            start_line: Some(1),
            end_line: Some(5),
            source: SignalSource::Semgrep,
        }
    }

    fn sample_tool_match(rule_id: &str, facet_slot: &str, leaf_id: &str) -> ToolMatch {
        ToolMatch {
            rule_id: rule_id.to_string(),
            facet_slot: facet_slot.to_string(),
            leaf_id: leaf_id.to_string(),
            value: serde_json::json!({"matched": true}),
            confidence: 0.9,
            spans: vec![sample_evidence_span()],
            raw: None,
        }
    }

    #[test]
    fn test_signal_source_serialization() {
        assert_eq!(
            serde_json::to_string(&SignalSource::TreeSitter).unwrap(),
            "\"tree_sitter\""
        );
        assert_eq!(
            serde_json::to_string(&SignalSource::Semgrep).unwrap(),
            "\"semgrep\""
        );
        assert_eq!(
            serde_json::to_string(&SignalSource::Manifest).unwrap(),
            "\"manifest\""
        );
    }

    #[test]
    fn test_signal_source_deserialization() {
        let ts: SignalSource = serde_json::from_str("\"tree_sitter\"").unwrap();
        assert_eq!(ts, SignalSource::TreeSitter);

        let sg: SignalSource = serde_json::from_str("\"semgrep\"").unwrap();
        assert_eq!(sg, SignalSource::Semgrep);

        let mf: SignalSource = serde_json::from_str("\"manifest\"").unwrap();
        assert_eq!(mf, SignalSource::Manifest);
    }

    #[test]
    fn test_evidence_span_roundtrip() {
        let span = sample_evidence_span();
        let json = serde_json::to_string(&span).unwrap();
        let deserialized: EvidenceSpan = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.file_path, "src/main.rs");
        assert_eq!(deserialized.start_byte, 0);
        assert_eq!(deserialized.end_byte, 100);
        assert_eq!(deserialized.start_line, Some(1));
        assert_eq!(deserialized.end_line, Some(5));
        assert_eq!(deserialized.source, SignalSource::Semgrep);
    }

    #[test]
    fn evidence_span_round_trip() {
        let span = EvidenceSpan {
            file_path: "src/main.rs".to_string(),
            start_byte: 10,
            end_byte: 42,
            start_line: Some(1),
            end_line: Some(3),
            source: SignalSource::TreeSitter,
        };
        let json = serde_json::to_string(&span).unwrap();
        let deserialized: EvidenceSpan = serde_json::from_str(&json).unwrap();
        assert_eq!(span, deserialized);
    }

    #[test]
    fn tool_match_round_trip() {
        let m = ToolMatch {
            rule_id: "ts.rust.pub_function".to_string(),
            facet_slot: "api.public.contracts".to_string(),
            leaf_id: "25".to_string(),
            value: serde_json::Value::String("pub fn hello()".to_string()),
            confidence: 0.9,
            spans: vec![EvidenceSpan {
                file_path: "src/lib.rs".to_string(),
                start_byte: 0,
                end_byte: 14,
                start_line: Some(1),
                end_line: Some(1),
                source: SignalSource::TreeSitter,
            }],
            raw: None,
        };
        let json = serde_json::to_string(&m).unwrap();
        let deserialized: ToolMatch = serde_json::from_str(&json).unwrap();
        assert_eq!(m, deserialized);
    }

    #[test]
    fn signal_source_serializes_to_snake_case() {
        let ts = serde_json::to_string(&SignalSource::TreeSitter).unwrap();
        assert_eq!(ts, "\"tree_sitter\"");

        let sg = serde_json::to_string(&SignalSource::Semgrep).unwrap();
        assert_eq!(sg, "\"semgrep\"");
    }

    #[test]
    fn test_tool_match_roundtrip() {
        let m = sample_tool_match("js.express.route", "boundaries.service_definitions", "23");
        let json = serde_json::to_string(&m).unwrap();
        let deserialized: ToolMatch = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.rule_id, "js.express.route");
        assert_eq!(deserialized.facet_slot, "boundaries.service_definitions");
        assert_eq!(deserialized.leaf_id, "23");
        assert!((deserialized.confidence - 0.9).abs() < f64::EPSILON);
        assert_eq!(deserialized.spans.len(), 1);
        assert!(deserialized.raw.is_none());
    }

    #[test]
    fn test_tool_match_raw_skipped_when_none() {
        let m = sample_tool_match("rule", "slot", "1");
        let json = serde_json::to_string(&m).unwrap();
        assert!(!json.contains("\"raw\""));
    }

    #[test]
    fn test_tool_match_raw_included_when_some() {
        let mut m = sample_tool_match("rule", "slot", "1");
        m.raw = Some(serde_json::json!({"extra": "data"}));
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"raw\""));
        let deserialized: ToolMatch = serde_json::from_str(&json).unwrap();
        assert!(deserialized.raw.is_some());
    }

    #[test]
    fn architecture_signals_default_is_empty() {
        let signals = ArchitectureSignals::default();
        assert!(signals.matches.is_empty());
        assert_eq!(signals.files_analyzed, 0);
        assert_eq!(signals.queries_executed, 0);
    }

    #[test]
    fn test_matches_by_leaf_id() {
        let signals = ArchitectureSignals {
            matches: vec![
                sample_tool_match("rule1", "slot_a", "10"),
                sample_tool_match("rule2", "slot_b", "10"),
                sample_tool_match("rule3", "slot_a", "20"),
            ],
            files_analyzed: 0,
            queries_executed: 0,
        };
        let by_leaf = signals.matches_by_leaf_id();
        assert_eq!(by_leaf.len(), 2);
        assert_eq!(by_leaf["10"].len(), 2);
        assert_eq!(by_leaf["20"].len(), 1);
    }

    #[test]
    fn test_matches_by_facet_slot() {
        let signals = ArchitectureSignals {
            matches: vec![
                sample_tool_match("rule1", "slot_a", "10"),
                sample_tool_match("rule2", "slot_b", "10"),
                sample_tool_match("rule3", "slot_a", "20"),
            ],
            files_analyzed: 0,
            queries_executed: 0,
        };
        let by_facet = signals.matches_by_facet_slot();
        assert_eq!(by_facet.len(), 2);
        assert_eq!(by_facet["slot_a"].len(), 2);
        assert_eq!(by_facet["slot_b"].len(), 1);
    }

    #[test]
    fn test_matches_by_leaf_id_empty() {
        let signals = ArchitectureSignals {
            matches: vec![],
            files_analyzed: 0,
            queries_executed: 0,
        };
        assert!(signals.matches_by_leaf_id().is_empty());
    }

    #[test]
    fn test_matches_by_facet_slot_empty() {
        let signals = ArchitectureSignals {
            matches: vec![],
            files_analyzed: 0,
            queries_executed: 0,
        };
        assert!(signals.matches_by_facet_slot().is_empty());
    }
}
