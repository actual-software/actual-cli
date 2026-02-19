use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::generation::OutputFormat;

/// Default batch size for ADR tailoring.
pub const DEFAULT_BATCH_SIZE: usize = 15;

/// Default maximum concurrent projects during tailoring.
pub const DEFAULT_CONCURRENCY: usize = 3;

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

    /// Per-project tailoring timeout in seconds (default: 600).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_timeout_secs: Option<u64>,

    /// Output file format (default: `claude-md`).
    ///
    /// Supported values:
    /// - `claude-md`    — write `CLAUDE.md` (Claude Code)
    /// - `agents-md`    — write `AGENTS.md` (Codex CLI)
    /// - `cursor-rules` — write `.cursor/rules/actual-policies.mdc` (Cursor IDE)
    ///
    /// Set via CLI: `actual sync --output-format agents-md`
    /// Set persistently: `actual config set output_format agents-md`
    ///
    /// Only one format is active per sync run.  Teams that need multiple output
    /// files (e.g., both `CLAUDE.md` and `AGENTS.md`) should run `actual sync`
    /// twice with different `--output-format` values, or configure separate CI
    /// jobs.  A future `output_formats: Vec<String>` field may be added to
    /// support this natively; if so, `output_format` (singular) will be read
    /// with a deprecation warning and users will be asked to migrate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<OutputFormat>,

    /// Telemetry settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<TelemetryConfig>,

    /// Rejected ADR IDs per repo (keyed by SHA-256 of origin URL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_adrs: Option<HashMap<String, Vec<String>>>,

    /// Cached repo analysis (keyed to HEAD commit hash).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_analysis: Option<CachedAnalysis>,

    /// Cached tailoring result (keyed to composite input hash).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tailoring: Option<CachedTailoring>,

    /// AI backend runner for tailoring (default: "claude-cli").
    ///
    /// Supported values: "claude-cli", "anthropic-api", "openai-api", "codex-cli"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,

    /// Fallback Anthropic API key (used when runner = "anthropic-api" and ANTHROPIC_API_KEY not set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_api_key: Option<String>,

    /// Fallback OpenAI API key (used when runner = "openai-api" and OPENAI_API_KEY not set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_api_key: Option<String>,
}

/// Telemetry configuration.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct TelemetryConfig {
    /// Whether telemetry is enabled (default: true, opt-out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// Cached tailoring result, keyed by a hash of all tailoring inputs.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct CachedTailoring {
    /// SHA-256 hash of the composite tailoring inputs.
    pub cache_key: String,
    /// Repo path (for display/debugging).
    pub repo_path: String,
    /// The tailoring output (stored as opaque YAML value).
    pub tailoring: serde_yaml::Value,
    /// When the tailoring was performed.
    pub tailored_at: chrono::DateTime<chrono::Utc>,
}

/// Cached repository analysis result.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct CachedAnalysis {
    /// Path to the repository.
    pub repo_path: String,

    /// HEAD commit hash (`None` if not a git repo).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_commit: Option<String>,

    /// SHA-256 hash of the config fields that affect analysis results
    /// (`include_categories`, `exclude_categories`, `include_general`, `max_per_framework`).
    /// A missing value is treated as a cache miss so that caches created before this
    /// field existed are automatically invalidated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_hash: Option<String>,

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
            invocation_timeout_secs: Some(300),
            telemetry: Some(TelemetryConfig {
                enabled: Some(false),
            }),
            rejected_adrs: Some(rejected),
            cached_analysis: None,
            cached_tailoring: None,
            output_format: None,
            runner: None,
            anthropic_api_key: None,
            openai_api_key: None,
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
                config_hash: Some("deadbeef".to_string()),
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

    /// Round-trip test: output_format = claude-md serializes and deserializes correctly.
    #[test]
    fn test_round_trip_output_format_claude_md() {
        let config = Config {
            output_format: Some(OutputFormat::ClaudeMd),
            ..Config::default()
        };
        let yaml = serde_yaml::to_string(&config).expect("serialize to YAML");
        assert!(
            yaml.contains("claude-md"),
            "YAML must contain 'claude-md': {yaml}"
        );
        let deserialized: Config = serde_yaml::from_str(&yaml).expect("deserialize from YAML");
        assert_eq!(config, deserialized);
    }

    /// Round-trip test: output_format = agents-md serializes and deserializes correctly.
    #[test]
    fn test_round_trip_output_format_agents_md() {
        let config = Config {
            output_format: Some(OutputFormat::AgentsMd),
            ..Config::default()
        };
        let yaml = serde_yaml::to_string(&config).expect("serialize to YAML");
        assert!(
            yaml.contains("agents-md"),
            "YAML must contain 'agents-md': {yaml}"
        );
        let deserialized: Config = serde_yaml::from_str(&yaml).expect("deserialize from YAML");
        assert_eq!(config, deserialized);
    }

    /// Round-trip test: output_format = cursor-rules serializes and deserializes correctly.
    #[test]
    fn test_round_trip_output_format_cursor_rules() {
        let config = Config {
            output_format: Some(OutputFormat::CursorRules),
            ..Config::default()
        };
        let yaml = serde_yaml::to_string(&config).expect("serialize to YAML");
        assert!(
            yaml.contains("cursor-rules"),
            "YAML must contain 'cursor-rules': {yaml}"
        );
        let deserialized: Config = serde_yaml::from_str(&yaml).expect("deserialize from YAML");
        assert_eq!(config, deserialized);
    }

    /// Partial YAML with output_format set deserializes correctly.
    #[test]
    fn test_partial_yaml_with_output_format() {
        let yaml = "output_format: agents-md\n";
        let config: Config = serde_yaml::from_str(yaml).expect("deserialize partial YAML");
        assert_eq!(config.output_format, Some(OutputFormat::AgentsMd));
    }

    /// Round-trip test: config with runner field round-trips through YAML.
    #[test]
    fn test_round_trip_runner_field() {
        let config = Config {
            runner: Some("anthropic-api".to_string()),
            ..Config::default()
        };
        let yaml = serde_yaml::to_string(&config).expect("serialize to YAML");
        assert!(
            yaml.contains("anthropic-api"),
            "YAML must contain 'anthropic-api': {yaml}"
        );
        let deserialized: Config = serde_yaml::from_str(&yaml).expect("deserialize from YAML");
        assert_eq!(config, deserialized);
    }

    /// Round-trip test: config with both API key fields round-trips through YAML.
    #[test]
    fn test_round_trip_api_keys() {
        let config = Config {
            anthropic_api_key: Some("sk-ant-test123".to_string()),
            openai_api_key: Some("sk-openai-test456".to_string()),
            ..Config::default()
        };
        let yaml = serde_yaml::to_string(&config).expect("serialize to YAML");
        assert!(
            yaml.contains("sk-ant-test123"),
            "YAML must contain anthropic key: {yaml}"
        );
        assert!(
            yaml.contains("sk-openai-test456"),
            "YAML must contain openai key: {yaml}"
        );
        let deserialized: Config = serde_yaml::from_str(&yaml).expect("deserialize from YAML");
        assert_eq!(config, deserialized);
    }

    /// Round-trip test with CachedTailoring included.
    #[test]
    fn test_round_trip_with_cached_tailoring() {
        let config = Config {
            cached_tailoring: Some(CachedTailoring {
                cache_key: "abc123def456".to_string(),
                repo_path: "/home/user/project".to_string(),
                tailoring: serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
                tailored_at: chrono::Utc::now(),
            }),
            ..Config::default()
        };

        let yaml = serde_yaml::to_string(&config).expect("serialize to YAML");
        let deserialized: Config = serde_yaml::from_str(&yaml).expect("deserialize from YAML");
        assert_eq!(config, deserialized);
    }
}
