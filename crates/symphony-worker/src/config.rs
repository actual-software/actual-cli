use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing required config: {field}")]
    MissingField { field: String },
}

// ---------------------------------------------------------------------------
// Worker configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub orchestrator_url: String,
    pub auth_token: String,
    pub worker_id: String,
    pub repo_url: String,
    pub base_clone_path: PathBuf,
    pub worktree_root: PathBuf,
    pub agent_command: String,
    pub max_turns: u32,
    pub turn_timeout_ms: u64,
    pub poll_backoff_base_ms: u64,
    pub poll_backoff_max_ms: u64,
}

impl WorkerConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.orchestrator_url.is_empty() {
            return Err(ConfigError::MissingField {
                field: "orchestrator_url".to_string(),
            });
        }
        if self.auth_token.is_empty() {
            return Err(ConfigError::MissingField {
                field: "auth_token".to_string(),
            });
        }
        if self.repo_url.is_empty() {
            return Err(ConfigError::MissingField {
                field: "repo_url".to_string(),
            });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_valid_config() -> WorkerConfig {
        WorkerConfig {
            orchestrator_url: "http://localhost:8080".to_string(),
            auth_token: "secret".to_string(),
            worker_id: "w-1".to_string(),
            repo_url: "https://github.com/org/repo.git".to_string(),
            base_clone_path: PathBuf::from("/tmp/base"),
            worktree_root: PathBuf::from("/tmp/worktrees"),
            agent_command: "claude".to_string(),
            max_turns: 30,
            turn_timeout_ms: 1_800_000,
            poll_backoff_base_ms: 1_000,
            poll_backoff_max_ms: 60_000,
        }
    }

    #[test]
    fn test_config_validate_success() {
        let config = make_valid_config();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_validate_missing_orchestrator_url() {
        let mut config = make_valid_config();
        config.orchestrator_url = String::new();
        let err = config.validate().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("orchestrator_url"));
    }

    #[test]
    fn test_config_validate_missing_auth_token() {
        let mut config = make_valid_config();
        config.auth_token = String::new();
        let err = config.validate().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("auth_token"));
    }

    #[test]
    fn test_config_validate_missing_repo_url() {
        let mut config = make_valid_config();
        config.repo_url = String::new();
        let err = config.validate().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("repo_url"));
    }

    #[test]
    fn test_config_error_display() {
        let err = ConfigError::MissingField {
            field: "orchestrator_url".to_string(),
        };
        assert_eq!(err.to_string(), "missing required config: orchestrator_url");
    }

    #[test]
    fn test_config_error_debug() {
        let err = ConfigError::MissingField {
            field: "auth_token".to_string(),
        };
        let debug = format!("{:?}", err);
        assert!(debug.contains("MissingField"));
        assert!(debug.contains("auth_token"));
    }

    #[test]
    fn test_config_clone_and_debug() {
        let config = make_valid_config();
        let cloned = config.clone();
        assert_eq!(cloned.orchestrator_url, config.orchestrator_url);
        assert_eq!(cloned.auth_token, config.auth_token);
        assert_eq!(cloned.repo_url, config.repo_url);
        assert_eq!(cloned.worker_id, config.worker_id);
        assert_eq!(cloned.base_clone_path, config.base_clone_path);
        assert_eq!(cloned.worktree_root, config.worktree_root);
        assert_eq!(cloned.agent_command, config.agent_command);
        assert_eq!(cloned.max_turns, config.max_turns);
        assert_eq!(cloned.turn_timeout_ms, config.turn_timeout_ms);
        assert_eq!(cloned.poll_backoff_base_ms, config.poll_backoff_base_ms);
        assert_eq!(cloned.poll_backoff_max_ms, config.poll_backoff_max_ms);

        let debug = format!("{:?}", config);
        assert!(debug.contains("WorkerConfig"));
        assert!(debug.contains("orchestrator_url"));
    }
}
