//! Production wiring for `actual sync`.
//!
//! This module is excluded from coverage measurement (see coverage.yml)
//! because it instantiates real subprocess/API runners that cannot be used
//! in unit tests. The generic monomorphization of `run_sync<R>` only exists
//! here, keeping `sync.rs` at 100% line coverage.

use std::time::Duration;

const DEFAULT_ANTHROPIC_MODEL: &str = DEFAULT_MODEL;
const DEFAULT_OPENAI_MODEL: &str = "gpt-5.2";

use clap::ValueEnum as _;

use crate::cli::args::{RunnerChoice, SyncArgs};
use crate::cli::commands::auth::check_auth_with_timeout;
use crate::cli::commands::sync::{resolve_cwd, run_sync};
use crate::cli::ui::header::{AuthDisplay, RunnerDisplay};
use crate::cli::ui::real_terminal::RealTerminal;
use crate::config::paths::{config_path, load_from};
use crate::config::types::{DEFAULT_MODEL, DEFAULT_TIMEOUT_SECS};
use crate::error::ActualError;
use crate::runner::anthropic_api::AnthropicApiRunner;
use crate::runner::binary::find_claude_binary;
use crate::runner::codex_cli::{check_codex_auth, find_codex_binary, CodexCliRunner};
use crate::runner::cursor_cli::{find_cursor_binary, CursorCliRunner};
use crate::runner::openai_api::OpenAiApiRunner;
use crate::runner::subprocess::CliClaudeRunner;

/// Enforce that an explicit Codex CLI model override is only used with an API key.
///
/// ChatGPT OAuth accounts (`codex login`) only support the Codex CLI's own
/// default model.  When the user specifies a model explicitly it signals intent
/// to use API-key access — so we require `OPENAI_API_KEY` and fail early at
/// the Environment step rather than after the full tailoring run.
///
/// Returns `Ok(())` when:
/// - An API key is present (explicit model + API key → full access).
/// - No explicit model was requested (Codex CLI picks its own safe default,
///   which works with ChatGPT OAuth accounts).
///
/// Returns `Err(ActualError::CodexCliModelRequiresApiKey)` when an explicit
/// model is requested but no API key is available, with a message that names
/// the model and explains the ChatGPT OAuth limitation.
fn require_api_key_for_model(
    api_key: Option<&str>,
    model: Option<&str>,
) -> Result<(), ActualError> {
    if let Some(m) = model {
        if api_key.is_none_or(|k| k.is_empty()) {
            return Err(ActualError::CodexCliModelRequiresApiKey {
                model: m.to_string(),
            });
        }
    }
    Ok(())
}

/// Infer the runner from available model configuration when neither `--runner`
/// CLI flag nor `runner` config field is set.
///
/// Priority order:
///   1. `args_model` (--model CLI flag overrides everything)
///   2. `openai_model` (dedicated OpenAI config field signals intent)
///   3. `cursor_model` (dedicated Cursor config field signals intent)
///   4. `model` (legacy/Anthropic-oriented config field)
///   5. Default: `ClaudeCli`
fn infer_runner_from_config(
    args_model: Option<&str>,
    openai_model: Option<&str>,
    cursor_model: Option<&str>,
    model: Option<&str>,
) -> Result<RunnerChoice, String> {
    if let Some(m) = args_model {
        RunnerChoice::infer_from_model(m)
    } else if let Some(m) = openai_model {
        RunnerChoice::infer_from_model(m)
    } else if cursor_model.is_some() {
        Ok(RunnerChoice::CursorCli)
    } else if let Some(m) = model {
        RunnerChoice::infer_from_model(m)
    } else {
        Ok(RunnerChoice::ClaudeCli)
    }
}

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
        // Auto-select runner from model when neither --runner flag nor
        // config runner is set.  See `infer_runner_from_config` doc comment
        // for the full priority order.
        infer_runner_from_config(
            args.model.as_deref(),
            cfg.openai_model.as_deref(),
            cfg.cursor_model.as_deref(),
            cfg.model.as_deref(),
        )
        .map_err(ActualError::ConfigError)?
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
            // Model resolution: --model flag > openai_model config > model config > default.
            // `openai_model` takes priority so runner-specific config wins, but
            // `model` is included because it may have triggered runner auto-selection.
            let model = args
                .model
                .as_deref()
                .or(cfg.openai_model.as_deref())
                .or(cfg.model.as_deref())
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
            // We check the env var and config fallback first. If an API key is
            // found, it is injected into the subprocess environment.
            let api_key = std::env::var("OPENAI_API_KEY")
                .ok()
                .or_else(|| cfg.openai_api_key.clone());
            // Model resolution: --model flag > openai_model config > model config > None
            // (let Codex CLI use its own default, currently gpt-5 for ChatGPT OAuth).
            // `openai_model` takes priority so runner-specific config wins, but
            // `model` is included because it may have triggered runner auto-selection.
            let model = args
                .model
                .as_deref()
                .or(cfg.openai_model.as_deref())
                .or(cfg.model.as_deref())
                .map(|s| s.to_string());
            // An explicit model override signals intent to use API-key access.
            // ChatGPT OAuth accounts only support the Codex CLI default model,
            // so require OPENAI_API_KEY and fail early rather than after the run.
            require_api_key_for_model(api_key.as_deref(), model.as_deref())?;
            check_codex_auth(api_key.as_deref())?;
            let auth_display = AuthDisplay {
                authenticated: true,
                email: api_key.as_ref().map(|_| "OpenAI API".to_string()),
            };
            let display_model = model
                .clone()
                .unwrap_or_else(|| "(codex default)".to_string());
            let runner_display = RunnerDisplay {
                runner_name: RunnerChoice::CodexCli.display_name().to_string(),
                model: display_model.clone(),
                warning: RunnerChoice::CodexCli.model_compatibility_warning(&display_model),
            };
            let mut codex_runner =
                CodexCliRunner::new(binary_path, model.clone(), subprocess_timeout);
            if let Some(ref key) = api_key {
                codex_runner = codex_runner.with_api_key(key.clone());
            }
            let result = run_sync(
                args,
                &root_dir,
                &cfg_path,
                &term,
                &codex_runner,
                Some(&auth_display),
                Some(&runner_display),
            );

            // Fall back to the OpenAI Responses API when Codex CLI reports a
            // model-level error (e.g. "model is not supported").  This lets
            // users who have a valid OPENAI_API_KEY transparently retry via
            // the direct API without manual --runner overrides.
            match result {
                Err(ref e) if e.is_model_error() => {
                    // Only attempt fallback if an API key is available.
                    let fallback_key =
                        resolve_api_key("OPENAI_API_KEY", cfg.openai_api_key.as_deref());
                    match fallback_key {
                        Ok(key) => {
                            tracing::warn!(
                                "Codex CLI failed with a model error, falling back to OpenAI API runner"
                            );
                            let fallback_model =
                                model.unwrap_or_else(|| DEFAULT_OPENAI_MODEL.to_string());
                            let fallback_auth = AuthDisplay {
                                authenticated: true,
                                email: Some("OpenAI API (fallback)".to_string()),
                            };
                            let fallback_display = RunnerDisplay {
                                runner_name: "openai-api (fallback)".to_string(),
                                model: fallback_model.clone(),
                                warning: RunnerChoice::OpenAiApi
                                    .model_compatibility_warning(&fallback_model),
                            };
                            let api_runner =
                                OpenAiApiRunner::new(key, fallback_model, subprocess_timeout)?;
                            // Skip confirmation on fallback — user already accepted
                            // on the initial CodexCli attempt.
                            let mut fallback_args = args.clone();
                            fallback_args.force = true;
                            run_sync(
                                &fallback_args,
                                &root_dir,
                                &cfg_path,
                                &term,
                                &api_runner,
                                Some(&fallback_auth),
                                Some(&fallback_display),
                            )
                        }
                        Err(_) => {
                            // No API key available for fallback — return the
                            // original Codex CLI error.
                            result
                        }
                    }
                }
                other => other,
            }
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
                .or(cfg.model.as_deref())
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
    use super::{infer_runner_from_config, require_api_key_for_model, resolve_api_key};
    use crate::cli::args::RunnerChoice;
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

    // ---- infer_runner_from_config tests ----

    #[test]
    fn infer_runner_nothing_set_defaults_to_claude_cli() {
        assert_eq!(
            infer_runner_from_config(None, None, None, None).unwrap(),
            RunnerChoice::ClaudeCli,
        );
    }

    #[test]
    fn infer_runner_args_model_overrides_all() {
        assert_eq!(
            infer_runner_from_config(Some("gpt-5"), None, Some("cursor-model"), Some("haiku"),)
                .unwrap(),
            RunnerChoice::CodexCli,
        );
    }

    #[test]
    fn infer_runner_args_model_claude_alias() {
        assert_eq!(
            infer_runner_from_config(Some("sonnet"), None, None, None).unwrap(),
            RunnerChoice::ClaudeCli,
        );
    }

    #[test]
    fn infer_runner_args_model_anthropic_full() {
        assert_eq!(
            infer_runner_from_config(Some("claude-sonnet-4-6"), None, None, None).unwrap(),
            RunnerChoice::AnthropicApi,
        );
    }

    #[test]
    fn infer_runner_openai_model_config() {
        assert_eq!(
            infer_runner_from_config(None, Some("gpt-5-mini"), None, None).unwrap(),
            RunnerChoice::CodexCli,
        );
    }

    #[test]
    fn infer_runner_openai_model_overrides_cfg_model() {
        assert_eq!(
            infer_runner_from_config(None, Some("gpt-5"), None, Some("haiku")).unwrap(),
            RunnerChoice::CodexCli,
        );
    }

    #[test]
    fn infer_runner_openai_model_overrides_cursor_model() {
        assert_eq!(
            infer_runner_from_config(None, Some("gpt-5"), Some("auto"), None).unwrap(),
            RunnerChoice::CodexCli,
        );
    }

    #[test]
    fn infer_runner_cursor_model_config() {
        assert_eq!(
            infer_runner_from_config(None, None, Some("auto"), None).unwrap(),
            RunnerChoice::CursorCli,
        );
    }

    #[test]
    fn infer_runner_cursor_model_overrides_cfg_model() {
        assert_eq!(
            infer_runner_from_config(None, None, Some("gpt-4o"), Some("haiku")).unwrap(),
            RunnerChoice::CursorCli,
        );
    }

    #[test]
    fn infer_runner_cursor_model_with_anthropic_name() {
        // Cursor proxies all models, so cursor_model always means CursorCli
        assert_eq!(
            infer_runner_from_config(None, None, Some("claude-sonnet-4-6"), None).unwrap(),
            RunnerChoice::CursorCli,
        );
    }

    #[test]
    fn infer_runner_cfg_model_anthropic() {
        assert_eq!(
            infer_runner_from_config(None, None, None, Some("claude-sonnet-4-6")).unwrap(),
            RunnerChoice::AnthropicApi,
        );
    }

    #[test]
    fn infer_runner_cfg_model_short_alias() {
        assert_eq!(
            infer_runner_from_config(None, None, None, Some("sonnet")).unwrap(),
            RunnerChoice::ClaudeCli,
        );
    }

    #[test]
    fn infer_runner_cfg_model_openai() {
        assert_eq!(
            infer_runner_from_config(None, None, None, Some("gpt-4o")).unwrap(),
            RunnerChoice::CodexCli,
        );
    }

    #[test]
    fn infer_runner_codex_model_via_openai_model() {
        assert_eq!(
            infer_runner_from_config(None, Some("gpt-5.2-codex"), None, None).unwrap(),
            RunnerChoice::CodexCli,
        );
    }

    #[test]
    fn infer_runner_openai_model_gpt5_mini_not_claude() {
        // The exact bug scenario: openai_model: "gpt-5-mini" should NOT default to ClaudeCli
        assert_eq!(
            infer_runner_from_config(None, Some("gpt-5-mini"), None, None).unwrap(),
            RunnerChoice::CodexCli,
            "openai_model should be considered for runner auto-detection"
        );
    }

    #[test]
    fn infer_runner_cursor_model_not_claude() {
        // cursor_model: "gpt-4o" should NOT default to ClaudeCli
        assert_eq!(
            infer_runner_from_config(None, None, Some("gpt-4o"), None).unwrap(),
            RunnerChoice::CursorCli,
            "cursor_model should be considered for runner auto-detection"
        );
    }

    #[test]
    fn infer_runner_unrecognized_model_returns_error() {
        // Unrecognized models in any position should return an error
        assert!(infer_runner_from_config(Some("gemini-pro"), None, None, None).is_err());
        assert!(infer_runner_from_config(None, Some("llama-3"), None, None).is_err());
        assert!(infer_runner_from_config(None, None, None, Some("my-custom-model")).is_err());
        // cursor_model always returns CursorCli regardless of model name
        assert_eq!(
            infer_runner_from_config(None, None, Some("gemini-pro"), None).unwrap(),
            RunnerChoice::CursorCli,
        );
    }

    #[test]
    fn infer_runner_cfg_model_gpt5_mini_selects_codex() {
        // When `model: gpt-5-mini` is set (not openai_model), runner should be CodexCli.
        // The CodexCli runner supports ChatGPT OAuth and all OpenAI-family models.
        assert_eq!(
            infer_runner_from_config(None, None, None, Some("gpt-5-mini")).unwrap(),
            RunnerChoice::CodexCli,
            "model: gpt-5-mini should auto-select CodexCli runner"
        );
    }

    #[test]
    fn infer_runner_cfg_model_codex_selects_codex() {
        assert_eq!(
            infer_runner_from_config(None, None, None, Some("gpt-5.2-codex")).unwrap(),
            RunnerChoice::CodexCli,
            "model: gpt-5.2-codex should auto-select CodexCli runner"
        );
    }

    // ---- require_api_key_for_model tests ----

    #[test]
    fn require_api_key_no_key_with_model_errors() {
        // No API key + explicit model → error (ChatGPT OAuth can't run arbitrary models).
        let err = require_api_key_for_model(None, Some("gpt-5-mini")).unwrap_err();
        assert!(
            matches!(err, ActualError::CodexCliModelRequiresApiKey { ref model } if model == "gpt-5-mini"),
            "expected CodexCliModelRequiresApiKey, got: {err:?}"
        );
        // The error message should name the model and explain what to do.
        let msg = err.to_string();
        assert!(
            msg.contains("gpt-5-mini"),
            "expected model name in error message: {msg}"
        );
        assert!(
            msg.contains("OPENAI_API_KEY"),
            "expected OPENAI_API_KEY in error message: {msg}"
        );
        assert!(
            msg.contains("openai-api"),
            "expected --runner openai-api suggestion in error message: {msg}"
        );
    }

    #[test]
    fn require_api_key_no_key_with_gpt_codex_model_errors() {
        // Test with current gpt-*-codex naming pattern.
        let err = require_api_key_for_model(None, Some("gpt-5.2-codex")).unwrap_err();
        assert!(
            matches!(err, ActualError::CodexCliModelRequiresApiKey { ref model } if model == "gpt-5.2-codex"),
            "expected CodexCliModelRequiresApiKey for gpt-5.2-codex, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("gpt-5.2-codex"),
            "model name must appear in error message: {msg}"
        );
        assert!(
            msg.contains("ChatGPT"),
            "ChatGPT auth explanation must appear in error message: {msg}"
        );
    }

    #[test]
    fn require_api_key_no_key_no_model_ok() {
        // No API key + no model → OK (Codex CLI default works with ChatGPT OAuth).
        assert!(require_api_key_for_model(None, None).is_ok());
    }

    #[test]
    fn require_api_key_with_key_and_model_ok() {
        // API key present + explicit model → OK (full model access).
        assert!(require_api_key_for_model(Some("sk-test"), Some("gpt-5-mini")).is_ok());
    }

    #[test]
    fn require_api_key_empty_key_with_model_errors() {
        // Empty API key is treated as absent → specific error naming the model.
        let err = require_api_key_for_model(Some(""), Some("gpt-5.2")).unwrap_err();
        assert!(
            matches!(err, ActualError::CodexCliModelRequiresApiKey { ref model } if model == "gpt-5.2"),
            "expected CodexCliModelRequiresApiKey, got: {err:?}"
        );
    }

    #[test]
    fn require_api_key_with_key_no_model_ok() {
        // API key present + no model → OK (API key + Codex default is fine too).
        assert!(require_api_key_for_model(Some("sk-test"), None).is_ok());
    }
}
