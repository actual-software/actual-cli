use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Top-level configuration for the actual CLI.
///
/// Stored as YAML at `~/.actualai/actual/config.yaml`.
/// All fields are optional; missing fields use sensible defaults.
#[derive(Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Config {
    /// API endpoint URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,

    /// Default model for Claude Code invocations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Categories to always include.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_categories: Option<Vec<String>>,

    /// Categories to always exclude.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_categories: Option<Vec<String>>,

    /// Whether to include general (language-agnostic) ADRs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_general: Option<bool>,

    /// Maximum number of ADRs to return per framework.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_per_framework: Option<u32>,

    /// Maximum budget per tailoring invocation (USD).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_budget_usd: Option<f64>,

    /// Batch size for ADR tailoring (default: 15).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_size: Option<usize>,

    /// Max concurrent projects during tailoring (default: 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<usize>,

    /// Telemetry settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<TelemetryConfig>,

    /// Rejected ADR IDs per repo (keyed by SHA-256 of origin URL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_adrs: Option<HashMap<String, Vec<String>>>,

    /// Cached repo analysis (keyed to HEAD commit hash).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_analysis: Option<CachedAnalysis>,
}

/// Telemetry configuration.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct TelemetryConfig {
    /// Whether telemetry is enabled (default: true, opt-out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// Cached repository analysis result.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct CachedAnalysis {
    /// Path to the repository.
    pub repo_path: String,

    /// HEAD commit hash (`None` if not a git repo).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_commit: Option<String>,

    /// The analysis data (stored as opaque YAML value until analysis types are implemented).
    pub analysis: serde_yaml::Value,

    /// When the analysis was performed.
    pub analyzed_at: chrono::DateTime<chrono::Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip test: create Config with values, serialize to YAML string, deserialize back, assert equality.
    #[test]
    fn test_round_trip_with_values() {
        let mut rejected = HashMap::new();
        rejected.insert(
            "abc123def456".to_string(),
            vec!["adr-001".to_string(), "adr-002".to_string()],
        );

        let config = Config {
            api_url: Some("https://api.example.com".to_string()),
            model: Some("sonnet".to_string()),
            include_categories: Some(vec!["testing".to_string(), "security".to_string()]),
            exclude_categories: Some(vec!["deprecated".to_string()]),
            include_general: Some(true),
            max_per_framework: Some(10),
            max_budget_usd: Some(0.50),
            batch_size: Some(20),
            concurrency: Some(5),
            telemetry: Some(TelemetryConfig {
                enabled: Some(false),
            }),
            rejected_adrs: Some(rejected),
            cached_analysis: None,
        };

        let yaml = serde_yaml::to_string(&config).expect("serialize to YAML");
        let deserialized: Config = serde_yaml::from_str(&yaml).expect("deserialize from YAML");
        assert_eq!(config, deserialized);
    }

    /// Default Config serializes to valid YAML.
    #[test]
    fn test_default_config_serializes_to_valid_yaml() {
        let config = Config::default();
        let yaml = serde_yaml::to_string(&config).expect("serialize default to YAML");
        let _parsed: Config = serde_yaml::from_str(&yaml).expect("deserialize default YAML");
    }

    /// Empty YAML `{}` deserializes to default Config.
    #[test]
    fn test_empty_yaml_deserializes_to_default() {
        let config: Config = serde_yaml::from_str("{}").expect("deserialize empty YAML");
        assert_eq!(config, Config::default());
    }

    /// Round-trip test with CachedAnalysis included.
    #[test]
    fn test_round_trip_with_cached_analysis() {
        let config = Config {
            cached_analysis: Some(CachedAnalysis {
                repo_path: "/home/user/project".to_string(),
                head_commit: Some("abc123".to_string()),
                analysis: serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
                analyzed_at: chrono::Utc::now(),
            }),
            ..Config::default()
        };

        let yaml = serde_yaml::to_string(&config).expect("serialize to YAML");
        let deserialized: Config = serde_yaml::from_str(&yaml).expect("deserialize from YAML");
        assert_eq!(config, deserialized);
    }

    /// Partial YAML (only some fields set) deserializes correctly with remaining fields as None.
    #[test]
    fn test_partial_yaml_deserializes_with_defaults() {
        let yaml = "model: opus\nbatch_size: 10\n";
        let config: Config = serde_yaml::from_str(yaml).expect("deserialize partial YAML");
        assert_eq!(config.model, Some("opus".to_string()));
        assert_eq!(config.batch_size, Some(10));
        assert_eq!(config.api_url, None);
        assert_eq!(config.telemetry, None);
        assert_eq!(config.concurrency, None);
    }
}
