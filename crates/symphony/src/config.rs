use crate::error::{Result, SymphonyError};
use crate::model::WorkflowDefinition;
use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use tracing::warn;

/// Typed runtime configuration derived from WorkflowDefinition.config.
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub tracker: TrackerConfig,
    pub polling: PollingConfig,
    pub workspace: WorkspaceConfig,
    pub hooks: HooksConfig,
    pub agent: AgentConfig,
    pub coding_agent: CodingAgentConfig,
    pub server: ServerConfig,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub port: Option<u16>,
}

#[derive(Clone)]
pub struct TrackerConfig {
    pub kind: String,
    pub endpoint: String,
    pub api_key: String,
    pub team_key: String,
    pub active_states: Vec<String>,
    pub terminal_states: Vec<String>,
}

impl std::fmt::Debug for TrackerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrackerConfig")
            .field("kind", &self.kind)
            .field("endpoint", &self.endpoint)
            .field("api_key", &"[REDACTED]")
            .field("team_key", &self.team_key)
            .field("active_states", &self.active_states)
            .field("terminal_states", &self.terminal_states)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct PollingConfig {
    pub interval_ms: u64,
}

#[derive(Debug, Clone)]
pub struct WorkspaceConfig {
    pub root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct HooksConfig {
    pub after_create: Option<String>,
    pub before_run: Option<String>,
    pub after_run: Option<String>,
    pub before_remove: Option<String>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub max_concurrent_agents: u32,
    pub max_turns: u32,
    pub max_retries: Option<u32>,
    pub max_retry_backoff_ms: u64,
    pub max_concurrent_agents_by_state: HashMap<String, u32>,
}

#[derive(Debug, Clone)]
pub struct CodingAgentConfig {
    pub command: String,
    pub permission_mode: String,
    pub turn_timeout_ms: u64,
    pub stall_timeout_ms: u64,
}

impl ServiceConfig {
    /// Build typed config from a parsed workflow definition.
    pub fn from_workflow(workflow: &WorkflowDefinition) -> Result<Self> {
        let root = &workflow.config;

        Ok(Self {
            tracker: parse_tracker_config(root)?,
            polling: parse_polling_config(root),
            workspace: parse_workspace_config(root),
            hooks: parse_hooks_config(root),
            agent: parse_agent_config(root),
            coding_agent: parse_coding_agent_config(root),
            server: parse_server_config(root),
        })
    }

    /// Validate config for dispatch readiness.
    pub fn validate_for_dispatch(&self) -> Result<()> {
        if self.tracker.kind.is_empty() {
            return Err(SymphonyError::MissingRequiredConfig {
                field: "tracker.kind".to_string(),
            });
        }

        if self.tracker.kind != "linear" {
            return Err(SymphonyError::UnsupportedTrackerKind {
                kind: self.tracker.kind.clone(),
            });
        }

        if self.tracker.api_key.is_empty() {
            return Err(SymphonyError::MissingTrackerApiKey);
        }

        if self.tracker.team_key.is_empty() {
            return Err(SymphonyError::MissingTrackerTeamKey);
        }

        if self.coding_agent.command.is_empty() {
            return Err(SymphonyError::MissingRequiredConfig {
                field: "coding_agent.command".to_string(),
            });
        }

        Ok(())
    }
}

// --- Internal parsing helpers ---

fn get_yaml_str(root: &serde_yml::Value, keys: &[&str]) -> Option<String> {
    let mut current = root;
    for key in keys {
        current = current
            .as_mapping()?
            .get(serde_yml::Value::String((*key).to_string()))?;
    }
    match current {
        serde_yml::Value::String(s) => Some(s.clone()),
        serde_yml::Value::Number(n) => Some(n.to_string()),
        serde_yml::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn get_yaml_int(root: &serde_yml::Value, keys: &[&str]) -> Option<i64> {
    let mut current = root;
    for key in keys {
        current = current
            .as_mapping()?
            .get(serde_yml::Value::String((*key).to_string()))?;
    }
    match current {
        serde_yml::Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        serde_yml::Value::String(s) => s.parse::<i64>().ok(),
        _ => None,
    }
}

/// Resolve a value that may be `$VAR_NAME` to an environment variable.
fn resolve_env_var(value: &str) -> String {
    if let Some(var_name) = value.strip_prefix('$') {
        env::var(var_name).unwrap_or_default()
    } else {
        value.to_string()
    }
}

/// Expand ~ to home directory and resolve $VAR in path strings.
fn expand_path(value: &str) -> PathBuf {
    let resolved = resolve_env_var(value);
    resolved
        .strip_prefix('~')
        .and_then(|stripped| {
            dirs::home_dir().map(|home| {
                let rest = stripped.strip_prefix('/').unwrap_or(stripped);
                home.join(rest)
            })
        })
        .unwrap_or_else(|| PathBuf::from(resolved))
}

/// Parse a list that can be either a YAML list of strings or a comma-separated string.
fn parse_string_list(root: &serde_yml::Value, keys: &[&str]) -> Option<Vec<String>> {
    let mut current = root;
    for key in keys {
        current = current
            .as_mapping()?
            .get(serde_yml::Value::String((*key).to_string()))?;
    }

    match current {
        serde_yml::Value::Sequence(seq) => {
            let items: Vec<String> = seq
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            Some(items)
        }
        serde_yml::Value::String(s) => {
            let items: Vec<String> = s.split(',').map(|s| s.trim().to_string()).collect();
            Some(items)
        }
        _ => None,
    }
}

fn parse_tracker_config(root: &serde_yml::Value) -> Result<TrackerConfig> {
    let kind = get_yaml_str(root, &["tracker", "kind"]).unwrap_or_default();

    let default_endpoint = if kind == "linear" {
        "https://api.linear.app/graphql".to_string()
    } else {
        String::new()
    };

    let endpoint = get_yaml_str(root, &["tracker", "endpoint"]).unwrap_or(default_endpoint);

    // API key: resolve $VAR_NAME, fallback to LINEAR_API_KEY env var for linear
    let raw_api_key = get_yaml_str(root, &["tracker", "api_key"]).unwrap_or_default();
    let api_key = if raw_api_key.is_empty() && kind == "linear" {
        env::var("LINEAR_API_KEY").unwrap_or_default()
    } else {
        resolve_env_var(&raw_api_key)
    };

    let team_key = get_yaml_str(root, &["tracker", "team_key"]).unwrap_or_default();

    let active_states = parse_string_list(root, &["tracker", "active_states"])
        .unwrap_or_else(|| vec!["Todo".to_string(), "In Progress".to_string()]);

    let terminal_states =
        parse_string_list(root, &["tracker", "terminal_states"]).unwrap_or_else(|| {
            vec![
                "Closed".to_string(),
                "Cancelled".to_string(),
                "Canceled".to_string(),
                "Duplicate".to_string(),
                "Done".to_string(),
            ]
        });

    Ok(TrackerConfig {
        kind,
        endpoint,
        api_key,
        team_key,
        active_states,
        terminal_states,
    })
}

fn parse_polling_config(root: &serde_yml::Value) -> PollingConfig {
    let interval_ms = get_yaml_int(root, &["polling", "interval_ms"])
        .map(|v| v.max(0) as u64)
        .unwrap_or(30_000);

    PollingConfig { interval_ms }
}

fn parse_workspace_config(root: &serde_yml::Value) -> WorkspaceConfig {
    let root_path = get_yaml_str(root, &["workspace", "root"])
        .map(|s| expand_path(&s))
        .unwrap_or_else(|| {
            let tmp = env::temp_dir();
            tmp.join("symphony_workspaces")
        });

    WorkspaceConfig { root: root_path }
}

fn parse_hooks_config(root: &serde_yml::Value) -> HooksConfig {
    let after_create = get_yaml_str(root, &["hooks", "after_create"]);
    let before_run = get_yaml_str(root, &["hooks", "before_run"]);
    let after_run = get_yaml_str(root, &["hooks", "after_run"]);
    let before_remove = get_yaml_str(root, &["hooks", "before_remove"]);

    let timeout_ms = get_yaml_int(root, &["hooks", "timeout_ms"])
        .map(|v| if v > 0 { v as u64 } else { 60_000 })
        .unwrap_or(60_000);

    HooksConfig {
        after_create,
        before_run,
        after_run,
        before_remove,
        timeout_ms,
    }
}

fn parse_agent_config(root: &serde_yml::Value) -> AgentConfig {
    let max_concurrent_agents = get_yaml_int(root, &["agent", "max_concurrent_agents"])
        .map(|v| v.max(1) as u32)
        .unwrap_or(10);

    let max_turns = get_yaml_int(root, &["agent", "max_turns"])
        .map(|v| v.max(1) as u32)
        .unwrap_or(20);

    let max_retries = get_yaml_int(root, &["agent", "max_retries"]).map(|v| v.max(0) as u32);

    let max_retry_backoff_ms = get_yaml_int(root, &["agent", "max_retry_backoff_ms"])
        .map(|v| v.max(0) as u64)
        .unwrap_or(300_000);

    // Parse per-state concurrency map
    let mut by_state = HashMap::new();
    if let Some(map_val) = get_yaml_mapping(root, &["agent", "max_concurrent_agents_by_state"]) {
        if let Some(mapping) = map_val.as_mapping() {
            for (key, val) in mapping {
                if let Some(state_name) = key.as_str() {
                    let normalized = state_name.trim().to_lowercase();
                    if let Some(limit) = val
                        .as_i64()
                        .or_else(|| val.as_str().and_then(|s| s.parse().ok()))
                    {
                        if limit > 0 {
                            by_state.insert(normalized, limit as u32);
                        }
                    }
                }
            }
        }
    }

    AgentConfig {
        max_concurrent_agents,
        max_turns,
        max_retries,
        max_retry_backoff_ms,
        max_concurrent_agents_by_state: by_state,
    }
}

fn get_yaml_mapping<'a>(root: &'a serde_yml::Value, keys: &[&str]) -> Option<&'a serde_yml::Value> {
    let mut current = root;
    for key in keys {
        current = current
            .as_mapping()?
            .get(serde_yml::Value::String((*key).to_string()))?;
    }
    Some(current)
}

/// Determine which YAML key to use for the coding agent config.
/// Prefers `coding_agent`, falls back to deprecated `codex` with a warning.
/// Returns the chosen key name.
fn resolve_coding_agent_key(root: &serde_yml::Value) -> &'static str {
    let has_coding_agent = root
        .as_mapping()
        .and_then(|m| m.get(serde_yml::Value::String("coding_agent".to_string())))
        .is_some();

    if has_coding_agent {
        return "coding_agent";
    }

    let has_codex = root
        .as_mapping()
        .and_then(|m| m.get(serde_yml::Value::String("codex".to_string())))
        .is_some();

    if has_codex {
        warn!(
            "config key 'codex' is deprecated and will be removed in a future release; \
             rename it to 'coding_agent'"
        );
        return "codex";
    }

    // Neither key present — use "coding_agent" (defaults will apply)
    "coding_agent"
}

fn parse_coding_agent_config(root: &serde_yml::Value) -> CodingAgentConfig {
    let key = resolve_coding_agent_key(root);

    let command = get_yaml_str(root, &[key, "command"])
        .unwrap_or_else(|| "claude -p --output-format stream-json --verbose --dangerously-skip-permissions --max-turns 20".to_string());

    let permission_mode = get_yaml_str(root, &[key, "permission_mode"])
        .unwrap_or_else(|| "bypassPermissions".to_string());

    let turn_timeout_ms = get_yaml_int(root, &[key, "turn_timeout_ms"])
        .map(|v| v.max(0) as u64)
        .unwrap_or(3_600_000);

    // §10.2: read_timeout_ms removed — it was parsed but never used.
    // The field is silently ignored if present in YAML.

    // §8.3: Clamp negative values to 0 (disables stall detection)
    // instead of wrapping via `v as u64`.
    let stall_timeout_ms = get_yaml_int(root, &[key, "stall_timeout_ms"])
        .map(|v| v.max(0) as u64)
        .unwrap_or(300_000);

    CodingAgentConfig {
        command,
        permission_mode,
        turn_timeout_ms,
        stall_timeout_ms,
    }
}

fn parse_server_config(root: &serde_yml::Value) -> ServerConfig {
    let port = get_yaml_int(root, &["server", "port"]).and_then(|v| {
        let clamped = v.clamp(0, u16::MAX as i64);
        if clamped == 0 {
            None
        } else {
            Some(clamped as u16)
        }
    });

    ServerConfig { port }
}

/// Check if a state is in a list (case-insensitive, trimmed).
pub fn state_matches(state: &str, list: &[String]) -> bool {
    let normalized = state.trim().to_lowercase();
    list.iter().any(|s| s.trim().to_lowercase() == normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::parse_workflow;

    #[test]
    fn test_defaults() {
        let wf = parse_workflow("").unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        assert_eq!(config.polling.interval_ms, 30_000);
        assert_eq!(config.agent.max_concurrent_agents, 10);
        assert_eq!(config.agent.max_turns, 20);
        assert_eq!(config.agent.max_retries, None);
        assert_eq!(config.agent.max_retry_backoff_ms, 300_000);
        assert_eq!(config.coding_agent.turn_timeout_ms, 3_600_000);
        assert_eq!(config.coding_agent.stall_timeout_ms, 300_000);
        assert_eq!(config.hooks.timeout_ms, 60_000);
        assert!(config.tracker.active_states.contains(&"Todo".to_string()));
        assert!(config.tracker.terminal_states.contains(&"Done".to_string()));
        assert!(config.server.port.is_none());
    }

    #[test]
    fn test_custom_values() {
        let content = r#"---
tracker:
  kind: linear
  team_key: test-proj
  api_key: test-key
  active_states: "Ready, Working"
  terminal_states:
    - Closed
    - Done
polling:
  interval_ms: 5000
agent:
  max_concurrent_agents: 5
  max_turns: 10
  max_retries: 5
  max_retry_backoff_ms: 60000
  max_concurrent_agents_by_state:
    todo: 2
    in progress: 3
coding_agent:
  turn_timeout_ms: 1800000
  stall_timeout_ms: 120000
---
Do the work."#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        assert_eq!(config.tracker.kind, "linear");
        assert_eq!(config.tracker.team_key, "test-proj");
        assert_eq!(config.tracker.api_key, "test-key");
        assert_eq!(config.tracker.active_states, vec!["Ready", "Working"]);
        assert_eq!(config.polling.interval_ms, 5_000);
        assert_eq!(config.agent.max_concurrent_agents, 5);
        assert_eq!(config.agent.max_turns, 10);
        assert_eq!(config.agent.max_retries, Some(5));
        assert_eq!(config.agent.max_retry_backoff_ms, 60_000);
        assert_eq!(
            config.agent.max_concurrent_agents_by_state.get("todo"),
            Some(&2)
        );
        assert_eq!(
            config
                .agent
                .max_concurrent_agents_by_state
                .get("in progress"),
            Some(&3)
        );
        assert_eq!(config.coding_agent.turn_timeout_ms, 1_800_000);
        assert_eq!(config.coding_agent.stall_timeout_ms, 120_000);
    }

    #[test]
    fn test_env_var_resolution() {
        // If $VAR resolves to empty, treat as missing
        let resolved = resolve_env_var("$NONEXISTENT_SYMPHONY_VAR_12345");
        assert!(resolved.is_empty());
    }

    #[test]
    fn test_state_matches() {
        let states = vec!["Todo".to_string(), "In Progress".to_string()];
        assert!(state_matches("todo", &states));
        assert!(state_matches("TODO", &states));
        assert!(state_matches("  Todo  ", &states));
        assert!(state_matches("in progress", &states));
        assert!(!state_matches("Done", &states));
    }

    #[test]
    fn test_validate_for_dispatch_missing_kind() {
        let wf = parse_workflow("").unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        let result = config.validate_for_dispatch();
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_for_dispatch_valid() {
        let content = r#"---
tracker:
  kind: linear
  team_key: test
  api_key: test-key-123
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        let result = config.validate_for_dispatch();
        assert!(result.is_ok());
    }

    #[test]
    fn test_path_expansion() {
        let path = expand_path("/tmp/test");
        assert_eq!(path, PathBuf::from("/tmp/test"));
    }

    // --- validate_for_dispatch: uncovered error paths ---

    #[test]
    fn test_validate_unsupported_tracker_kind() {
        let content = r#"---
tracker:
  kind: jira
  team_key: test
  api_key: key123
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        let err = config.validate_for_dispatch().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("jira"), "expected 'jira' in error: {msg}");
    }

    #[test]
    fn test_validate_missing_api_key() {
        let content = r#"---
tracker:
  kind: linear
  team_key: test
  api_key: ""
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        let err = config.validate_for_dispatch().unwrap_err();
        assert!(matches!(&err, SymphonyError::MissingTrackerApiKey));
    }

    #[test]
    fn test_validate_missing_team_key() {
        let content = r#"---
tracker:
  kind: linear
  api_key: some-key
  team_key: ""
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        let err = config.validate_for_dispatch().unwrap_err();
        assert!(matches!(&err, SymphonyError::MissingTrackerTeamKey));
    }

    #[test]
    fn test_validate_missing_coding_agent_command() {
        let content = r#"---
tracker:
  kind: linear
  api_key: some-key
  team_key: my-proj
coding_agent:
  command: ""
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        let err = config.validate_for_dispatch().unwrap_err();
        assert!(
            matches!(&err, SymphonyError::MissingRequiredConfig { field } if field == "coding_agent.command")
        );
    }

    // --- expand_path: tilde and env var ---

    #[test]
    fn test_expand_path_tilde() {
        let path = expand_path("~/foo");
        let home = dirs::home_dir().unwrap();
        assert_eq!(path, home.join("foo"));
    }

    #[test]
    fn test_expand_path_env_var() {
        unsafe {
            env::set_var("SYMPHONY_TEST_CONFIG_PATH_1", "/custom/workspace");
        }
        let path = expand_path("$SYMPHONY_TEST_CONFIG_PATH_1");
        assert_eq!(path, PathBuf::from("/custom/workspace"));
        unsafe {
            env::remove_var("SYMPHONY_TEST_CONFIG_PATH_1");
        }
    }

    // --- resolve_env_var: literal and existing var ---

    #[test]
    fn test_resolve_env_var_literal() {
        let result = resolve_env_var("plain-value");
        assert_eq!(result, "plain-value");
    }

    #[test]
    fn test_resolve_env_var_existing() {
        unsafe {
            env::set_var("SYMPHONY_TEST_CONFIG_RESOLVE_1", "resolved-value");
        }
        let result = resolve_env_var("$SYMPHONY_TEST_CONFIG_RESOLVE_1");
        assert_eq!(result, "resolved-value");
        unsafe {
            env::remove_var("SYMPHONY_TEST_CONFIG_RESOLVE_1");
        }
    }

    // --- get_yaml_str: bool and number values ---

    #[test]
    fn test_get_yaml_str_bool_value() {
        let yaml: serde_yml::Value = serde_yml::from_str("flag: true").unwrap();
        let result = get_yaml_str(&yaml, &["flag"]);
        assert_eq!(result, Some("true".to_string()));
    }

    #[test]
    fn test_get_yaml_str_number_value() {
        let yaml: serde_yml::Value = serde_yml::from_str("count: 42").unwrap();
        let result = get_yaml_str(&yaml, &["count"]);
        assert_eq!(result, Some("42".to_string()));
    }

    // --- get_yaml_int: string-encoded int and float ---

    #[test]
    fn test_get_yaml_int_string_encoded() {
        let yaml: serde_yml::Value = serde_yml::from_str("val: \"100\"").unwrap();
        let result = get_yaml_int(&yaml, &["val"]);
        assert_eq!(result, Some(100));
    }

    #[test]
    fn test_get_yaml_int_float_value() {
        let yaml: serde_yml::Value = serde_yml::from_str("val: 3.7").unwrap();
        let result = get_yaml_int(&yaml, &["val"]);
        assert_eq!(result, Some(3));
    }

    // --- parse_string_list: non-string/non-sequence returns None ---

    #[test]
    fn test_parse_string_list_non_string_non_sequence() {
        let yaml: serde_yml::Value = serde_yml::from_str("items: 42").unwrap();
        let result = parse_string_list(&yaml, &["items"]);
        assert!(result.is_none());
    }

    // --- parse_agent_config: by_state with string-encoded limit, zero/negative limit ---

    #[test]
    fn test_agent_by_state_string_encoded_limit() {
        let content = r#"---
agent:
  max_concurrent_agents_by_state:
    review: "5"
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        assert_eq!(
            config.agent.max_concurrent_agents_by_state.get("review"),
            Some(&5)
        );
    }

    #[test]
    fn test_agent_by_state_zero_limit_excluded() {
        let content = r#"---
agent:
  max_concurrent_agents_by_state:
    blocked: 0
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        assert!(config
            .agent
            .max_concurrent_agents_by_state
            .get("blocked")
            .is_none());
    }

    #[test]
    fn test_agent_by_state_negative_limit_excluded() {
        let content = r#"---
agent:
  max_concurrent_agents_by_state:
    wontfix: -1
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        assert!(config
            .agent
            .max_concurrent_agents_by_state
            .get("wontfix")
            .is_none());
    }

    // --- parse_hooks_config: negative timeout falls back to 60000 ---

    #[test]
    fn test_hooks_negative_timeout_fallback() {
        let content = r#"---
hooks:
  timeout_ms: -500
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        assert_eq!(config.hooks.timeout_ms, 60_000);
    }

    // --- parse_coding_agent_config: custom command overrides default ---

    #[test]
    fn test_coding_agent_custom_command() {
        let content = r#"---
coding_agent:
  command: "my-custom-agent --flag"
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        assert_eq!(config.coding_agent.command, "my-custom-agent --flag");
    }

    // --- parse_tracker_config: LINEAR_API_KEY env var fallback, non-linear kind ---

    #[test]
    fn test_tracker_linear_api_key_env_fallback() {
        unsafe {
            env::set_var("LINEAR_API_KEY", "env-fallback-key");
        }
        let content = r#"---
tracker:
  kind: linear
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        assert_eq!(config.tracker.api_key, "env-fallback-key");
        unsafe {
            env::remove_var("LINEAR_API_KEY");
        }
    }

    #[test]
    fn test_tracker_non_linear_kind_empty_endpoint() {
        let content = r#"---
tracker:
  kind: github
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        assert_eq!(config.tracker.kind, "github");
        assert_eq!(config.tracker.endpoint, "");
    }

    // --- get_yaml_str: sequence value returns None ---

    #[test]
    fn test_get_yaml_str_sequence_value() {
        let yaml: serde_yml::Value = serde_yml::from_str("items:\n  - a\n  - b").unwrap();
        let result = get_yaml_str(&yaml, &["items"]);
        assert!(result.is_none());
    }

    // --- get_yaml_int: bool value returns None ---

    #[test]
    fn test_get_yaml_int_bool_value() {
        let yaml: serde_yml::Value = serde_yml::from_str("flag: true").unwrap();
        let result = get_yaml_int(&yaml, &["flag"]);
        assert!(result.is_none());
    }

    // --- expand_path: bare tilde ---

    #[test]
    fn test_expand_path_bare_tilde() {
        let path = expand_path("~");
        let home = dirs::home_dir().unwrap();
        assert_eq!(path, home);
    }

    // --- parse_agent_config: non-string key in by_state mapping ---

    #[test]
    fn test_agent_by_state_non_string_key_skipped() {
        let content = r#"---
agent:
  max_concurrent_agents_by_state:
    123: 5
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        // Numeric key 123 is parsed as integer by YAML, not string — should be skipped
        assert!(config.agent.max_concurrent_agents_by_state.is_empty());
    }

    #[test]
    fn test_agent_by_state_unparseable_value_skipped() {
        // A YAML sequence value can't be parsed as i64 or string — should be skipped
        let content = r#"---
agent:
  max_concurrent_agents_by_state:
    todo:
      - 1
      - 2
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        assert!(config.agent.max_concurrent_agents_by_state.is_empty());
    }

    // --- TrackerConfig Debug redacts API key ---

    #[test]
    fn test_tracker_config_debug_redacts_api_key() {
        let config = TrackerConfig {
            kind: "linear".to_string(),
            endpoint: "https://api.linear.app/graphql".to_string(),
            api_key: "lin_api_supersecretkey123".to_string(),
            team_key: "my-project".to_string(),
            active_states: vec!["Todo".to_string()],
            terminal_states: vec!["Done".to_string()],
        };
        let debug_output = format!("{:?}", config);
        assert_eq!(debug_output.contains("supersecretkey"), false);
        assert!(debug_output.contains("[REDACTED]"));
    }

    // --- stall_timeout_ms: negative value clamps to 0 (§8.3) ---

    #[test]
    fn test_stall_timeout_negative_clamps_to_zero() {
        let content = r#"---
coding_agent:
  stall_timeout_ms: -100
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        assert_eq!(config.coding_agent.stall_timeout_ms, 0);
    }

    #[test]
    fn test_agent_by_state_not_a_mapping() {
        // max_concurrent_agents_by_state set to a scalar instead of a mapping
        let content = r#"---
agent:
  max_concurrent_agents_by_state: "not a map"
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        assert!(config.agent.max_concurrent_agents_by_state.is_empty());
    }

    // --- deprecated `codex` key falls back correctly ---

    #[test]
    fn test_deprecated_codex_key_still_works() {
        let content = r#"---
codex:
  command: "my-legacy-agent --flag"
  turn_timeout_ms: 999000
  stall_timeout_ms: 42000
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        assert_eq!(config.coding_agent.command, "my-legacy-agent --flag");
        assert_eq!(config.coding_agent.turn_timeout_ms, 999_000);
        assert_eq!(config.coding_agent.stall_timeout_ms, 42_000);
    }

    #[test]
    fn test_coding_agent_key_preferred_over_codex() {
        // When both `coding_agent` and `codex` are present, `coding_agent` wins
        let content = r#"---
coding_agent:
  command: "new-agent"
  turn_timeout_ms: 111000
codex:
  command: "old-agent"
  turn_timeout_ms: 222000
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        assert_eq!(config.coding_agent.command, "new-agent");
        assert_eq!(config.coding_agent.turn_timeout_ms, 111_000);
    }

    #[test]
    fn test_validate_missing_coding_agent_command_via_deprecated_codex() {
        let content = r#"---
tracker:
  kind: linear
  api_key: some-key
  team_key: my-proj
codex:
  command: ""
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        let err = config.validate_for_dispatch().unwrap_err();
        assert!(
            matches!(&err, SymphonyError::MissingRequiredConfig { field } if field == "coding_agent.command")
        );
    }

    // --- server config tests ---

    #[test]
    fn test_server_port_configured() {
        let content = r#"---
server:
  port: 8080
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        assert_eq!(config.server.port, Some(8080));
    }

    #[test]
    fn test_server_port_zero_treated_as_none() {
        let content = r#"---
server:
  port: 0
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        assert!(config.server.port.is_none());
    }

    #[test]
    fn test_server_port_negative_treated_as_none() {
        let content = r#"---
server:
  port: -1
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        assert!(config.server.port.is_none());
    }

    #[test]
    fn test_server_port_not_configured() {
        let content = r#"---
server: {}
---
"#;
        let wf = parse_workflow(content).unwrap();
        let config = ServiceConfig::from_workflow(&wf).unwrap();
        assert!(config.server.port.is_none());
    }
}
