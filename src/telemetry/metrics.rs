use std::collections::HashMap;

use crate::api::types::{TelemetryMetric, TelemetryRequest};

/// Holds the counts from a sync operation for telemetry reporting.
pub struct SyncMetrics {
    pub adrs_fetched: u64,
    pub adrs_tailored: u64,
    pub adrs_rejected: u64,
    pub adrs_written: u64,
    pub repo_hash: String,
    pub version: String,
}

impl SyncMetrics {
    /// Converts the sync metrics into a `TelemetryRequest` containing four counter metrics.
    ///
    /// Each metric includes tags for `repo_hash`, `source` (always `"actual-cli"`),
    /// and `version`.
    pub fn to_counter_request(&self) -> TelemetryRequest {
        let build_tags = |this: &Self| -> HashMap<String, String> {
            let mut tags = HashMap::new();
            tags.insert("repo_hash".to_string(), this.repo_hash.clone());
            tags.insert("source".to_string(), "actual-cli".to_string());
            tags.insert("version".to_string(), this.version.clone());
            tags
        };

        TelemetryRequest {
            metrics: vec![
                TelemetryMetric {
                    name: "cli.sync.adrs_fetched".to_string(),
                    value: self.adrs_fetched,
                    tags: build_tags(self),
                },
                TelemetryMetric {
                    name: "cli.sync.adrs_tailored".to_string(),
                    value: self.adrs_tailored,
                    tags: build_tags(self),
                },
                TelemetryMetric {
                    name: "cli.sync.adrs_rejected".to_string(),
                    value: self.adrs_rejected,
                    tags: build_tags(self),
                },
                TelemetryMetric {
                    name: "cli.sync.adrs_written".to_string(),
                    value: self.adrs_written,
                    tags: build_tags(self),
                },
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_metrics() -> SyncMetrics {
        SyncMetrics {
            adrs_fetched: 32,
            adrs_tailored: 28,
            adrs_rejected: 3,
            adrs_written: 25,
            repo_hash: "testhash".to_string(),
            version: "0.1.0".to_string(),
        }
    }

    #[test]
    fn test_to_counter_request_produces_four_metrics() {
        let metrics = sample_metrics();
        let request = metrics.to_counter_request();

        assert_eq!(request.metrics.len(), 4);

        let names: Vec<&str> = request.metrics.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "cli.sync.adrs_fetched",
                "cli.sync.adrs_tailored",
                "cli.sync.adrs_rejected",
                "cli.sync.adrs_written",
            ]
        );
    }

    #[test]
    fn test_all_tags_include_required_keys() {
        let metrics = SyncMetrics {
            adrs_fetched: 10,
            adrs_tailored: 8,
            adrs_rejected: 1,
            adrs_written: 7,
            repo_hash: "abc123".to_string(),
            version: "2.0.0".to_string(),
        };
        let request = metrics.to_counter_request();

        for metric in &request.metrics {
            assert!(
                metric.tags.contains_key("repo_hash"),
                "metric '{}' missing repo_hash tag",
                metric.name
            );
            assert!(
                metric.tags.contains_key("source"),
                "metric '{}' missing source tag",
                metric.name
            );
            assert!(
                metric.tags.contains_key("version"),
                "metric '{}' missing version tag",
                metric.name
            );

            assert_eq!(
                metric.tags.get("source").unwrap(),
                "actual-cli",
                "metric '{}' has wrong source",
                metric.name
            );
            assert_eq!(
                metric.tags.get("repo_hash").unwrap(),
                "abc123",
                "metric '{}' has wrong repo_hash",
                metric.name
            );
            assert_eq!(
                metric.tags.get("version").unwrap(),
                "2.0.0",
                "metric '{}' has wrong version",
                metric.name
            );
        }
    }

    #[test]
    fn test_json_serialization_matches_expected_format() {
        let metrics = sample_metrics();
        let request = metrics.to_counter_request();
        let json_value = serde_json::to_value(&request).unwrap();

        let metrics_array = json_value["metrics"].as_array().unwrap();
        assert_eq!(metrics_array.len(), 4);

        let expected: &[(&str, u64)] = &[
            ("cli.sync.adrs_fetched", 32),
            ("cli.sync.adrs_tailored", 28),
            ("cli.sync.adrs_rejected", 3),
            ("cli.sync.adrs_written", 25),
        ];

        for (metric_json, &(expected_name, expected_value)) in
            metrics_array.iter().zip(expected.iter())
        {
            assert_eq!(metric_json["name"], expected_name);
            assert_eq!(metric_json["value"], expected_value);

            let tags = metric_json["tags"].as_object().unwrap();
            assert_eq!(tags.len(), 3);
            assert_eq!(tags["repo_hash"], "testhash");
            assert_eq!(tags["source"], "actual-cli");
            assert_eq!(tags["version"], "0.1.0");
        }
    }

    #[test]
    fn test_metric_values_match_input() {
        let metrics = SyncMetrics {
            adrs_fetched: 100,
            adrs_tailored: 75,
            adrs_rejected: 10,
            adrs_written: 65,
            repo_hash: "hash456".to_string(),
            version: "3.0.0".to_string(),
        };
        let request = metrics.to_counter_request();

        let value_map: HashMap<&str, u64> = request
            .metrics
            .iter()
            .map(|m| (m.name.as_str(), m.value))
            .collect();

        assert_eq!(value_map["cli.sync.adrs_fetched"], 100);
        assert_eq!(value_map["cli.sync.adrs_tailored"], 75);
        assert_eq!(value_map["cli.sync.adrs_rejected"], 10);
        assert_eq!(value_map["cli.sync.adrs_written"], 65);
    }
}
