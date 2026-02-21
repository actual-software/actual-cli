//! Production wiring for `actual sync`.
//!
//! This module is excluded from coverage measurement (see coverage.yml)
//! because it instantiates real subprocess/API runners that cannot be used
//! in unit tests. The generic monomorphization of `run_sync<R>` only exists
//! here, keeping `sync.rs` at 100% line coverage.

use std::time::Duration;

const DEFAULT_ANTHROPIC_MODEL: &str = "claude-sonnet-4-6";
const DEFAULT_OPENAI_MODEL: &str = "gpt-5.2";

use clap::ValueEnum as _;

use crate::cli::args::{RunnerChoice, SyncArgs};
use crate::cli::commands::auth::check_auth_with_timeout;
use crate::cli::commands::sync::{resolve_cwd, run_sync};
use crate::cli::ui::header::{AuthDisplay, RunnerDisplay};
use crate::cli::ui::real_terminal::RealTerminal;
use crate::config::paths::{config_path, load_from};
use crate::config::types::DEFAULT_TIMEOUT_SECS;
use crate::error::ActualError;
use crate::runner::anthropic_api::AnthropicApiRunner;
use crate::runner::binary::find_claude_binary;
use crate::runner::codex_cli::{check_codex_auth, find_codex_binary, CodexCliRunner};
use crate::runner::cursor_cli::{find_cursor_binary, CursorCliRunner};
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
                "Unknown runner '{}' in config. Valid values: claude-cli, anthropic-api, openai-api, codex-cli, cursor-cli",
                cfg_runner_str.chars().take(64).collect::<String>()
            ))
        })?
    } else {
        RunnerChoice::ClaudeCli
    };

    // Resolve the per-subprocess timeout: config value takes precedence over the
    // compiled-in default.  The same value is used regardless of which runner is
    // active so that `invocation_timeout_secs` has a consistent meaning across
    // all runner backends.
    let subprocess_timeout =
        Duration::from_secs(cfg.invocation_timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));

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
            let effective_model = args
                .model
                .as_deref()
                .or(cfg.model.as_deref())
                .unwrap_or("sonnet")
                .to_string();
            let runner_display = RunnerDisplay {
                runner_name: RunnerChoice::ClaudeCli.display_name().to_string(),
                model: effective_model.clone(),
                warning: RunnerChoice::ClaudeCli.model_compatibility_warning(&effective_model),
            };
            let cli_runner = CliClaudeRunner::new(binary_path, subprocess_timeout);
            let cli_runner = if let Some(t) = cfg.max_turns {
                cli_runner.with_max_turns(t)
            } else {
                cli_runner
            };
            run_sync(
                args,
                &root_dir,
                &cfg_path,
                &term,
                &cli_runner,
                Some(&auth_display),
                Some(&runner_display),
            )
        }
        RunnerChoice::AnthropicApi => {
            let api_key = resolve_api_key("ANTHROPIC_API_KEY", cfg.anthropic_api_key.as_deref())?;
            let auth_display = AuthDisplay {
                authenticated: true,
                email: Some("Anthropic API".to_string()),
            };
            // Full API model name — unlike the Claude CLI alias in options.rs DEFAULT_MODEL,
            // the Anthropic HTTP API requires the complete model identifier.
            let model = args
                .model
                .as_deref()
                .or(cfg.model.as_deref())
                .unwrap_or(DEFAULT_ANTHROPIC_MODEL)
                .to_string();
            let runner_display = RunnerDisplay {
                runner_name: RunnerChoice::AnthropicApi.display_name().to_string(),
                model: model.clone(),
                warning: RunnerChoice::AnthropicApi.model_compatibility_warning(&model),
            };
            let api_runner = AnthropicApiRunner::new(api_key, model, subprocess_timeout)?;
            run_sync(
                args,
                &root_dir,
                &cfg_path,
                &term,
                &api_runner,
                Some(&auth_display),
                Some(&runner_display),
            )
        }
        RunnerChoice::OpenAiApi => {
            let api_key = resolve_api_key("OPENAI_API_KEY", cfg.openai_api_key.as_deref())?;
            let auth_display = AuthDisplay {
                authenticated: true,
                email: Some("OpenAI API".to_string()),
            };
            // Model resolution: --model flag > openai_model config > default.
            // Uses `openai_model` (not `model`) so that `model: haiku` in config
            // doesn't leak Claude aliases into the OpenAI API runner.
            let model = args
                .model
                .as_deref()
                .or(cfg.openai_model.as_deref())
                .unwrap_or(DEFAULT_OPENAI_MODEL)
                .to_string();
            let runner_display = RunnerDisplay {
                runner_name: RunnerChoice::OpenAiApi.display_name().to_string(),
                model: model.clone(),
                warning: RunnerChoice::OpenAiApi.model_compatibility_warning(&model),
            };
            let api_runner = OpenAiApiRunner::new(api_key, model, subprocess_timeout)?;
            run_sync(
                args,
                &root_dir,
                &cfg_path,
                &term,
                &api_runner,
                Some(&auth_display),
                Some(&runner_display),
            )
        }
        RunnerChoice::CodexCli => {
            let binary_path = find_codex_binary()?;
            // Codex CLI supports two auth methods:
            // 1. OPENAI_API_KEY env var (API key from platform.openai.com)
            // 2. `codex login` (OAuth / ChatGPT account)
            //
            // We check the env var and config fallback. If an API key is found,
            // we inject it into the subprocess environment. If not, we let the
            // subprocess inherit whatever auth the user has configured (env var
            // or `codex login`). This mirrors the Claude Code runner pattern
            // where auth is inherited from the parent process.
            let api_key = std::env::var("OPENAI_API_KEY")
                .ok()
                .or_else(|| cfg.openai_api_key.clone());
            check_codex_auth(api_key.as_deref())?;
            let auth_display = AuthDisplay {
                authenticated: true,
                email: api_key.as_ref().map(|_| "OpenAI API".to_string()),
            };
            // Model resolution: --model flag > openai_model config > None (let
            // Codex CLI use its own default, currently gpt-5 for ChatGPT OAuth).
            // Uses `openai_model` (not `model`) so that `model: haiku` in config
            // doesn't leak Claude aliases into the Codex CLI runner.
            // Model resolution: --model flag > openai_model config > None (let
            // Codex CLI use its own default, currently gpt-5 for ChatGPT OAuth).
            // Uses `openai_model` (not `model`) so that `model: haiku` in config
            // doesn't leak Claude aliases into the Codex CLI runner.
            let model = args
                .model
                .as_deref()
                .or(cfg.openai_model.as_deref())
                .map(|s| s.to_string());
            let display_model = model
                .clone()
                .unwrap_or_else(|| "(codex default)".to_string());
            let runner_display = RunnerDisplay {
                runner_name: RunnerChoice::CodexCli.display_name().to_string(),
                model: display_model.clone(),
                warning: RunnerChoice::CodexCli.model_compatibility_warning(&display_model),
            };
            let mut codex_runner = CodexCliRunner::new(binary_path, model, subprocess_timeout);
            if let Some(key) = api_key {
                codex_runner = codex_runner.with_api_key(key);
            }
            run_sync(
                args,
                &root_dir,
                &cfg_path,
                &term,
                &codex_runner,
                Some(&auth_display),
                Some(&runner_display),
            )
        }
        RunnerChoice::CursorCli => {
            let binary_path = find_cursor_binary()?;
            let api_key = std::env::var("CURSOR_API_KEY")
                .ok()
                .or_else(|| cfg.cursor_api_key.clone());
            let auth_display = AuthDisplay {
                authenticated: true,
                email: api_key.as_ref().map(|_| "Cursor API".to_string()),
            };
            let model = args
                .model
                .as_deref()
                .or(cfg.cursor_model.as_deref())
                .map(|s| s.to_string());
            let display_model = model
                .clone()
                .unwrap_or_else(|| "(cursor default)".to_string());
            let runner_display = RunnerDisplay {
                runner_name: RunnerChoice::CursorCli.display_name().to_string(),
                model: display_model.clone(),
                warning: RunnerChoice::CursorCli.model_compatibility_warning(&display_model),
            };
            let mut cursor_runner = CursorCliRunner::new(binary_path, model, subprocess_timeout);
            if let Some(key) = api_key {
                cursor_runner = cursor_runner.with_api_key(key);
            }
            run_sync(
                args,
                &root_dir,
                &cfg_path,
                &term,
                &cursor_runner,
                Some(&auth_display),
                Some(&runner_display),
            )
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
