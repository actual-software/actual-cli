//! Production wiring for `actual sync`.
//!
//! This module is excluded from coverage measurement (see coverage.yml)
//! because it instantiates real subprocess/API runners that cannot be used
//! in unit tests. The generic monomorphization of `run_sync<R>` only exists
//! here, keeping `sync.rs` at 100% line coverage.

use std::time::Duration;

const DEFAULT_TIMEOUT_SECS: u64 = 300;
const DEFAULT_ANTHROPIC_MODEL: &str = "claude-sonnet-4-5";
const DEFAULT_OPENAI_MODEL: &str = "gpt-4o";
const DEFAULT_CODEX_MODEL: &str = "codex-mini-latest";

use clap::ValueEnum as _;

use crate::cli::args::{RunnerChoice, SyncArgs};
use crate::cli::commands::auth::check_auth_with_timeout;
use crate::cli::commands::sync::{resolve_cwd, run_sync};
use crate::cli::ui::header::{print_header_bar, AuthDisplay};
use crate::cli::ui::real_terminal::RealTerminal;
use crate::config::paths::{config_path, load_from};
use crate::error::ActualError;
use crate::runner::anthropic_api::AnthropicApiRunner;
use crate::runner::binary::find_claude_binary;
use crate::runner::codex_cli::{find_codex_binary, CodexCliRunner};
use crate::runner::openai_api::OpenAiApiRunner;
use crate::runner::subprocess::CliClaudeRunner;

/// Production wiring for `actual sync`.
///
/// Resolves system dependencies (cwd, terminal, runner) and delegates
/// to [`run_sync`].  Kept minimal so nearly all logic lives in the
/// fully-testable [`run_sync`].
pub(crate) fn sync_run(args: &SyncArgs) -> Result<(), ActualError> {
    let root_dir = resolve_cwd();
    let term = RealTerminal::default();
    let cfg_path = config_path()?;

    // Load config first to get runner/key fallbacks.
    // If the config file exists but is malformed, surface the error so the user
    // can fix it rather than silently falling back to defaults (which would cause
    // confusing downstream failures like ClaudeNotAuthenticated).
    let cfg = match load_from(&cfg_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "Warning: failed to load config from {}: {e}. Using defaults.",
                cfg_path.display()
            );
            Default::default()
        }
    };

    // Resolve the runner: CLI flag > config (parsed from string) > default.
    //
    // The CLI flag is already a typed `RunnerChoice` (clap rejects invalid
    // values before they reach here).  The config value is still a raw string
    // (user-edited YAML), so we parse it through `RunnerChoice::from_str` to
    // validate it and surface a clean error for unknown values.
    let runner = if let Some(r) = &args.runner {
        r.clone()
    } else if let Some(cfg_runner_str) = cfg.runner.as_deref() {
        RunnerChoice::from_str(cfg_runner_str, true).map_err(|_| {
            ActualError::ConfigError(format!(
                "Unknown runner '{}' in config. Valid values: claude-cli, anthropic-api, openai-api, codex-cli",
                cfg_runner_str.chars().take(64).collect::<String>()
            ))
        })?
    } else {
        RunnerChoice::ClaudeCli
    };

    match runner {
        RunnerChoice::ClaudeCli => {
            let binary_path = find_claude_binary()?;
            let auth_status = check_auth_with_timeout(&binary_path, Duration::from_secs(10))?;
            if !auth_status.is_usable() {
                return Err(ActualError::ClaudeNotAuthenticated);
            }
            let auth_display = AuthDisplay {
                authenticated: auth_status.logged_in,
                email: auth_status.email.clone(),
            };
            print_header_bar(&auth_display);
            let runner =
                CliClaudeRunner::new(binary_path, Duration::from_secs(DEFAULT_TIMEOUT_SECS));
            run_sync(args, &root_dir, &cfg_path, &term, &runner)
        }
        RunnerChoice::AnthropicApi => {
            let api_key = resolve_api_key("ANTHROPIC_API_KEY", cfg.anthropic_api_key.as_deref())?;
            print_header_bar(&AuthDisplay {
                authenticated: true,
                email: Some("Anthropic API".to_string()),
            });
            // Full API model name — unlike the Claude CLI alias in options.rs DEFAULT_MODEL,
            // the Anthropic HTTP API requires the complete model identifier.
            let model = args
                .model
                .as_deref()
                .or(cfg.model.as_deref())
                .unwrap_or(DEFAULT_ANTHROPIC_MODEL)
                .to_string();
            let runner =
                AnthropicApiRunner::new(api_key, model, Duration::from_secs(DEFAULT_TIMEOUT_SECS))?;
            run_sync(args, &root_dir, &cfg_path, &term, &runner)
        }
        RunnerChoice::OpenAiApi => {
            let api_key = resolve_api_key("OPENAI_API_KEY", cfg.openai_api_key.as_deref())?;
            print_header_bar(&AuthDisplay {
                authenticated: true,
                email: Some("OpenAI API".to_string()),
            });
            // Full API model name — the OpenAI HTTP API requires the complete model identifier,
            // unlike the Claude CLI alias in options.rs DEFAULT_MODEL.
            let model = args
                .model
                .as_deref()
                .or(cfg.model.as_deref())
                .unwrap_or(DEFAULT_OPENAI_MODEL)
                .to_string();
            let runner =
                OpenAiApiRunner::new(api_key, model, Duration::from_secs(DEFAULT_TIMEOUT_SECS))?;
            run_sync(args, &root_dir, &cfg_path, &term, &runner)
        }
        RunnerChoice::CodexCli => {
            let binary_path = find_codex_binary()?;
            print_header_bar(&AuthDisplay {
                authenticated: true,
                email: None,
            });
            let model = args
                .model
                .as_deref()
                .or(cfg.model.as_deref())
                .unwrap_or(DEFAULT_CODEX_MODEL)
                .to_string();
            let runner = CodexCliRunner::new(
                binary_path,
                model,
                Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            );
            run_sync(args, &root_dir, &cfg_path, &term, &runner)
        }
    }
}

/// Resolve an API key from an environment variable or a config fallback.
///
/// Returns `Err(ActualError::ApiKeyMissing)` if neither source provides a key.
pub(crate) fn resolve_api_key(
    env_var: &str,
    config_key: Option<&str>,
) -> Result<String, ActualError> {
    std::env::var(env_var)
        .ok()
        .or_else(|| config_key.map(|s| s.to_string()))
        .ok_or_else(|| ActualError::ApiKeyMissing {
            env_var: env_var.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::resolve_api_key;
    use crate::error::ActualError;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

    #[test]
    fn test_resolve_api_key_from_env_openai() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("OPENAI_API_KEY", "test-key-from-env");
        let result = resolve_api_key("OPENAI_API_KEY", None);
        assert_eq!(result.unwrap(), "test-key-from-env");
    }

    #[test]
    fn test_resolve_api_key_from_env_anthropic() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("ANTHROPIC_API_KEY", "test-anthropic-key");
        let result = resolve_api_key("ANTHROPIC_API_KEY", None);
        assert_eq!(result.unwrap(), "test-anthropic-key");
    }

    #[test]
    fn test_resolve_api_key_config_fallback_when_env_absent() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("OPENAI_API_KEY");
        let result = resolve_api_key("OPENAI_API_KEY", Some("config-fallback-key"));
        assert_eq!(result.unwrap(), "config-fallback-key");
    }

    #[test]
    fn test_resolve_api_key_env_takes_priority_over_config() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("OPENAI_API_KEY", "env-key-wins");
        let result = resolve_api_key("OPENAI_API_KEY", Some("config-fallback-key"));
        assert_eq!(result.unwrap(), "env-key-wins");
    }

    #[test]
    fn test_resolve_api_key_missing_both_returns_err() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("OPENAI_API_KEY");
        let result = resolve_api_key("OPENAI_API_KEY", None);
        assert!(
            matches!(result, Err(ActualError::ApiKeyMissing { ref env_var }) if env_var == "OPENAI_API_KEY"),
            "expected ApiKeyMissing, got: {:?}",
            result
        );
    }

    #[test]
    fn test_env_guard_restores_on_panic() {
        // Use a synthetic key that is guaranteed absent in any real environment.
        const TEST_KEY: &str = "ACTUAL_CLI_TEST_PANIC_GUARD_KEY_DO_NOT_SET";
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Ensure key is absent at the start.
        let _pre = EnvGuard::remove(TEST_KEY);
        drop(_pre);

        let result = std::panic::catch_unwind(|| {
            let _guard = EnvGuard::set(TEST_KEY, "leak-test-value");
            assert_eq!(std::env::var(TEST_KEY).unwrap(), "leak-test-value");
            panic!("deliberate test panic");
        });

        assert!(result.is_err(), "expected catch_unwind to capture panic");
        assert!(
            std::env::var(TEST_KEY).is_err(),
            "{TEST_KEY} must be cleaned up even after panic"
        );
    }
}
