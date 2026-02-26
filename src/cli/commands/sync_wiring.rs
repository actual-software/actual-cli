//! Production wiring for `actual adr-bot`.
//!
//! The preamble logic (runner selection, key resolution, timeout) is fully
//! testable via [`sync_run_inner`], which accepts an injectable auth function.
//! Only the thin production shim [`sync_run`] calls the real
//! [`check_auth_with_timeout`] subprocess, and even that path is exercised
//! by the ClaudeCli tests using a fake binary injected via `CLAUDE_BINARY`.

use std::time::Duration;

const DEFAULT_ANTHROPIC_MODEL: &str = DEFAULT_MODEL;

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
use crate::runner::probe::{
    is_anthropic_available, is_claude_available, is_codex_available, is_cursor_available,
    is_openai_available, resolve_api_key,
};
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

/// Detect which runner is available by probing the environment.
///
/// Priority order:
///   1. `args_model` (--model CLI flag overrides everything)
///   2. `model` (the unified model config field)
///   3. Default: probe ClaudeCli first, then AnthropicApi
///
/// For each model, determines the list of candidate runners in priority order,
/// then probes each in turn.  Returns the first available runner, or
/// `Err(ActualError::NoRunnerAvailable)` if none are available.
fn auto_detect_runner(
    args_model: Option<&str>,
    model: Option<&str>,
    cfg: &crate::config::types::Config,
) -> Result<RunnerChoice, ActualError> {
    use crate::cli::args::runner_candidates;

    // Resolve candidates in priority order:
    // 1. --model flag overrides everything
    // 2. model config field
    // 3. No model → default to claude-cli / anthropic-api candidates
    let (effective_model_display, candidates): (String, Vec<RunnerChoice>) =
        if let Some(m) = args_model {
            let lower = m.to_ascii_lowercase();
            let c = runner_candidates(&lower);
            (m.to_string(), c)
        } else if let Some(m) = model {
            let lower = m.to_ascii_lowercase();
            let c = runner_candidates(&lower);
            (m.to_string(), c)
        } else {
            // No model set → default candidates: ClaudeCli first, AnthropicApi fallback
            (
                "(default)".to_string(),
                vec![RunnerChoice::ClaudeCli, RunnerChoice::AnthropicApi],
            )
        };

    let mut tried: Vec<String> = Vec::new();

    for candidate in &candidates {
        let probe_result = match candidate {
            RunnerChoice::ClaudeCli => is_claude_available(),
            RunnerChoice::AnthropicApi => is_anthropic_available(cfg.anthropic_api_key.as_deref()),
            RunnerChoice::OpenAiApi => is_openai_available(cfg.openai_api_key.as_deref()),
            RunnerChoice::CodexCli => is_codex_available(cfg.openai_api_key.as_deref()),
            RunnerChoice::CursorCli => is_cursor_available(cfg.cursor_api_key.as_deref()),
        };

        match probe_result {
            Ok(()) => return Ok(candidate.clone()),
            Err(reason) => tried.push(format!("  - {reason}")),
        }
    }

    Err(ActualError::NoRunnerAvailable {
        model: effective_model_display,
        tried: tried.join("\n"),
    })
}

/// Handle the CodexCli model-error fallback: if the initial run returned a
/// model error and an API key is available, retry using the OpenAI API runner.
///
/// Extracted so the fallback decision can be tested independently.
///
/// Returns `Ok(())` on success (either original run succeeded, or fallback
/// succeeded), or `Err` if the original run failed and no fallback was
/// attempted, or if the fallback run itself failed.
#[allow(clippy::too_many_arguments)]
fn codex_cli_fallback_if_model_error(
    result: Result<(), ActualError>,
    args: &SyncArgs,
    model: Option<String>,
    config_openai_api_key: Option<&str>,
    subprocess_timeout: Duration,
    root_dir: &std::path::Path,
    cfg_path: &std::path::Path,
    term: &crate::cli::ui::real_terminal::RealTerminal,
) -> Result<(), ActualError> {
    match result {
        Err(ref e) if e.is_model_error() => {
            // Only attempt fallback if an API key is available.
            let fallback_key = resolve_api_key("OPENAI_API_KEY", config_openai_api_key);
            match fallback_key {
                Ok(key) => {
                    tracing::warn!(
                        "Codex CLI failed with a model error, falling back to OpenAI API runner"
                    );
                    // keep in sync with RUNNER_FAMILIES default for openai-api (models.rs)
                    let fallback_model = model.unwrap_or_else(|| "gpt-5.2".to_string());
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
                    let api_runner = OpenAiApiRunner::new(key, fallback_model, subprocess_timeout)?;
                    // Skip confirmation on fallback — user already accepted
                    // on the initial CodexCli attempt.
                    let mut fallback_args = args.clone();
                    fallback_args.force = true;
                    run_sync(
                        &fallback_args,
                        root_dir,
                        cfg_path,
                        term,
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

/// Production wiring for `actual adr-bot`.
///
/// Thin shim that calls [`sync_run_inner`] with the real
/// [`check_auth_with_timeout`] function. All logic lives in
/// [`sync_run_inner`], which is fully testable.
pub(crate) fn sync_run(args: &SyncArgs) -> Result<(), ActualError> {
    sync_run_inner(args, check_auth_with_timeout)
}

/// Inner implementation of `actual adr-bot`, parameterised over the auth
/// function so that tests can inject a fake.
///
/// Resolves system dependencies (cwd, terminal, runner) and delegates
/// to [`run_sync`].  Kept minimal so nearly all logic lives in the
/// fully-testable [`run_sync`].
fn sync_run_inner<AuthFn>(args: &SyncArgs, auth_fn: AuthFn) -> Result<(), ActualError>
where
    AuthFn: Fn(
        &std::path::Path,
        std::time::Duration,
    ) -> Result<crate::runner::auth::ClaudeAuthStatus, ActualError>,
{
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
        // config runner is set.  See `auto_detect_runner` doc comment
        // for the full priority order.
        auto_detect_runner(args.model.as_deref(), cfg.model.as_deref(), &cfg)?
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
            let auth_status = auth_fn(&binary_path, Duration::from_secs(10))?;
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
            let api_runner = if let Ok(base_url) = std::env::var("ANTHROPIC_API_BASE_URL") {
                AnthropicApiRunner::with_base_url(api_key, model, subprocess_timeout, base_url)?
            } else {
                AnthropicApiRunner::new(api_key, model, subprocess_timeout)?
            };
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
            // Model resolution: --model flag > model config > OpenAI default.
            // keep in sync with RUNNER_FAMILIES default for openai-api (models.rs)
            let model = args
                .model
                .as_deref()
                .or(cfg.model.as_deref())
                .unwrap_or("gpt-5.2")
                .to_string();
            let runner_display = RunnerDisplay {
                runner_name: RunnerChoice::OpenAiApi.display_name().to_string(),
                model: model.clone(),
                warning: RunnerChoice::OpenAiApi.model_compatibility_warning(&model),
            };
            let api_runner = if let Ok(base_url) = std::env::var("OPENAI_API_BASE_URL") {
                OpenAiApiRunner::new(api_key, model, subprocess_timeout)?.with_base_url(base_url)
            } else {
                OpenAiApiRunner::new(api_key, model, subprocess_timeout)?
            };
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
            // Model resolution: --model flag > model config > None
            // (let Codex CLI use its own default, currently gpt-5 for ChatGPT OAuth).
            let model = args
                .model
                .as_deref()
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
            codex_cli_fallback_if_model_error(
                result,
                args,
                model,
                cfg.openai_api_key.as_deref(),
                subprocess_timeout,
                &root_dir,
                &cfg_path,
                &term,
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

#[cfg(test)]
mod tests {
    use super::{
        auto_detect_runner, codex_cli_fallback_if_model_error, require_api_key_for_model,
        sync_run_inner,
    };
    use crate::cli::args::{RunnerChoice, SyncArgs};
    use crate::error::ActualError;
    use crate::runner::auth::ClaudeAuthStatus;
    use crate::runner::probe::resolve_api_key;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

    // ── Shared test helpers ──────────────────────────────────────────────────

    /// Build a minimal `SyncArgs` suitable for unit-testing the preamble.
    ///
    /// `force = true` skips the user-confirmation prompt; `no_tui = true`
    /// avoids spawning the ratatui TUI.  All other fields are left at their
    /// defaults.
    fn make_sync_args(runner: Option<RunnerChoice>) -> SyncArgs {
        SyncArgs {
            dry_run: false,
            full: false,
            force: true,
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: None,
            verbose: false,
            no_tailor: false,
            max_budget_usd: None,
            no_tui: true,
            output_format: None,
            runner,
            show_errors: false,
        }
    }

    /// Create a fake executable shell script at `dir/name`.
    ///
    /// The script prints `stdout_content` and exits with `exit_code`.
    #[cfg(unix)]
    fn create_fake_executable(
        dir: &std::path::Path,
        name: &str,
        stdout_content: &str,
        exit_code: i32,
    ) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        let content = format!(
            "#!/bin/sh\nprintf '%s\\n' '{}'\nexit {}\n",
            stdout_content.replace('\'', "'\\''"),
            exit_code
        );
        std::fs::write(&path, &content).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Create a minimal config file in `dir` with the given YAML content, set
    /// `ACTUAL_CONFIG` to point to it, and return the `TempDir` (to keep it
    /// alive) and the `EnvGuard` (to restore the env var on drop).
    ///
    /// The caller must hold `ENV_MUTEX` before calling this.
    fn with_temp_config(dir: &tempfile::TempDir, content: &str) -> EnvGuard {
        let cfg_path = dir.path().join("config.yaml");
        std::fs::write(&cfg_path, content).unwrap();
        EnvGuard::set("ACTUAL_CONFIG", cfg_path.to_str().unwrap())
    }

    /// A fake auth function that always returns "not authenticated".
    fn auth_not_authenticated(
        _path: &std::path::Path,
        _timeout: std::time::Duration,
    ) -> Result<ClaudeAuthStatus, ActualError> {
        Ok(ClaudeAuthStatus {
            logged_in: false,
            auth_method: None,
            api_provider: None,
            api_key_source: None,
            email: None,
            org_id: None,
            org_name: None,
            subscription_type: None,
        })
    }

    /// A fake auth function that always returns "authenticated".
    fn auth_authenticated(
        _path: &std::path::Path,
        _timeout: std::time::Duration,
    ) -> Result<ClaudeAuthStatus, ActualError> {
        Ok(ClaudeAuthStatus {
            logged_in: true,
            auth_method: Some("claude.ai".to_string()),
            api_provider: None,
            api_key_source: None,
            email: Some("test@example.com".to_string()),
            org_id: None,
            org_name: None,
            subscription_type: None,
        })
    }

    // ── ClaudeCli arm ────────────────────────────────────────────────────────

    #[test]
    fn test_claude_cli_binary_not_found() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/claude");
        // ACTUAL_CONFIG → use a temp dir so config_path() succeeds with empty config
        let dir = tempfile::tempdir().unwrap();
        let _g2 = with_temp_config(&dir, "{}");
        let args = make_sync_args(Some(RunnerChoice::ClaudeCli));
        let result = sync_run_inner(&args, auth_authenticated);
        assert!(
            matches!(result, Err(ActualError::ClaudeNotFound)),
            "expected ClaudeNotFound, got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_claude_cli_injected_not_authenticated() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let bin_dir = tempfile::tempdir().unwrap();
        // A real executable that outputs valid-but-unauthenticated JSON
        let fake_claude =
            create_fake_executable(bin_dir.path(), "fake-claude", r#"{"loggedIn": false}"#, 0);
        let _g1 = EnvGuard::set("CLAUDE_BINARY", fake_claude.to_str().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let _g2 = with_temp_config(&dir, "{}");
        let args = make_sync_args(Some(RunnerChoice::ClaudeCli));
        let result = sync_run_inner(&args, auth_not_authenticated);
        assert!(
            matches!(result, Err(ActualError::ClaudeNotAuthenticated)),
            "expected ClaudeNotAuthenticated, got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_claude_cli_injected_authenticated_wiring_covered() {
        // With a real authenticated auth_fn and a real executable, the ClaudeCli
        // arm reaches run_sync. Since no ADRs exist in a temp dir, run_sync will
        // error out, but the wiring code (binary lookup, auth check, runner
        // construction, run_sync call) is fully exercised.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let bin_dir = tempfile::tempdir().unwrap();
        let fake_claude = create_fake_executable(
            bin_dir.path(),
            "fake-claude",
            r#"{"loggedIn": true, "authMethod": "claude.ai", "email": "test@example.com"}"#,
            0,
        );
        let _g1 = EnvGuard::set("CLAUDE_BINARY", fake_claude.to_str().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let _g2 = with_temp_config(&dir, "{}");
        let args = make_sync_args(Some(RunnerChoice::ClaudeCli));
        // The preamble (binary lookup, auth, runner construction) must complete
        // without error; run_sync may error due to missing ADR infrastructure.
        let result = sync_run_inner(&args, auth_authenticated);
        // Any error here comes from run_sync internals, not from the wiring.
        // We just verify we got past the preamble without a ClaudeNotFound or
        // ClaudeNotAuthenticated error.
        assert!(
            !matches!(result, Err(ActualError::ClaudeNotFound)),
            "should have found the fake binary: {result:?}"
        );
        assert!(
            !matches!(result, Err(ActualError::ClaudeNotAuthenticated)),
            "auth_authenticated should prevent this: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_claude_cli_max_turns_config_path() {
        // Verify max_turns config is applied: uses a config with max_turns set.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let bin_dir = tempfile::tempdir().unwrap();
        let fake_claude = create_fake_executable(
            bin_dir.path(),
            "fake-claude",
            r#"{"loggedIn": true, "authMethod": "claude.ai"}"#,
            0,
        );
        let _g1 = EnvGuard::set("CLAUDE_BINARY", fake_claude.to_str().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let _g2 = with_temp_config(&dir, "max_turns: 5\n");
        let args = make_sync_args(Some(RunnerChoice::ClaudeCli));
        let result = sync_run_inner(&args, auth_authenticated);
        // Preamble succeeds; run_sync may fail due to missing infrastructure.
        assert!(
            !matches!(result, Err(ActualError::ClaudeNotFound)),
            "should have found fake binary: {result:?}"
        );
        assert!(
            !matches!(result, Err(ActualError::ClaudeNotAuthenticated)),
            "should be authenticated: {result:?}"
        );
    }

    // ── AnthropicApi arm ─────────────────────────────────────────────────────

    #[test]
    fn test_anthropic_api_no_key() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ANTHROPIC_API_KEY");
        let dir = tempfile::tempdir().unwrap();
        let _g2 = with_temp_config(&dir, "{}");
        let args = make_sync_args(Some(RunnerChoice::AnthropicApi));
        let result = sync_run_inner(&args, auth_not_authenticated);
        assert!(
            matches!(result, Err(ActualError::ApiKeyMissing { ref env_var }) if env_var == "ANTHROPIC_API_KEY"),
            "expected ApiKeyMissing(ANTHROPIC_API_KEY), got: {result:?}"
        );
    }

    #[test]
    fn test_anthropic_api_key_from_env() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant-test-key");
        let dir = tempfile::tempdir().unwrap();
        let _g2 = with_temp_config(&dir, "{}");
        let args = make_sync_args(Some(RunnerChoice::AnthropicApi));
        // Key resolves → AnthropicApiRunner::new is called → run_sync may error
        let result = sync_run_inner(&args, auth_not_authenticated);
        // Must NOT be ApiKeyMissing — preamble succeeded
        assert!(
            !matches!(result, Err(ActualError::ApiKeyMissing { .. })),
            "should have resolved key from env, got: {result:?}"
        );
    }

    #[test]
    fn test_anthropic_api_key_from_config_fallback() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ANTHROPIC_API_KEY");
        let dir = tempfile::tempdir().unwrap();
        let _g2 = with_temp_config(&dir, "anthropic_api_key: sk-ant-config-key\n");
        let args = make_sync_args(Some(RunnerChoice::AnthropicApi));
        let result = sync_run_inner(&args, auth_not_authenticated);
        // Config fallback resolves the key — preamble does not return ApiKeyMissing
        assert!(
            !matches!(result, Err(ActualError::ApiKeyMissing { .. })),
            "should have resolved key from config fallback, got: {result:?}"
        );
    }

    #[test]
    fn test_anthropic_api_model_flag_resolution() {
        // Verify the --model flag is honoured in the AnthropicApi arm.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant-test");
        let dir = tempfile::tempdir().unwrap();
        let _g2 = with_temp_config(&dir, "{}");
        let mut args = make_sync_args(Some(RunnerChoice::AnthropicApi));
        args.model = Some("claude-opus-4".to_string());
        let result = sync_run_inner(&args, auth_not_authenticated);
        assert!(
            !matches!(result, Err(ActualError::ApiKeyMissing { .. })),
            "key should be present, preamble should pass: {result:?}"
        );
    }

    // ── OpenAiApi arm ────────────────────────────────────────────────────────

    #[test]
    fn test_openai_api_no_key() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("OPENAI_API_KEY");
        let dir = tempfile::tempdir().unwrap();
        let _g2 = with_temp_config(&dir, "{}");
        let args = make_sync_args(Some(RunnerChoice::OpenAiApi));
        let result = sync_run_inner(&args, auth_not_authenticated);
        assert!(
            matches!(result, Err(ActualError::ApiKeyMissing { ref env_var }) if env_var == "OPENAI_API_KEY"),
            "expected ApiKeyMissing(OPENAI_API_KEY), got: {result:?}"
        );
    }

    #[test]
    fn test_openai_api_key_from_env() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("OPENAI_API_KEY", "sk-openai-test-key");
        let dir = tempfile::tempdir().unwrap();
        let _g2 = with_temp_config(&dir, "{}");
        let args = make_sync_args(Some(RunnerChoice::OpenAiApi));
        let result = sync_run_inner(&args, auth_not_authenticated);
        assert!(
            !matches!(result, Err(ActualError::ApiKeyMissing { .. })),
            "key should resolve from env, preamble should pass: {result:?}"
        );
    }

    #[test]
    fn test_openai_api_model_config() {
        // Verify model config field is resolved in the OpenAiApi arm.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("OPENAI_API_KEY", "sk-openai-test");
        let dir = tempfile::tempdir().unwrap();
        let _g2 = with_temp_config(&dir, "model: gpt-5\n");
        let args = make_sync_args(Some(RunnerChoice::OpenAiApi));
        let result = sync_run_inner(&args, auth_not_authenticated);
        assert!(
            !matches!(result, Err(ActualError::ApiKeyMissing { .. })),
            "key should resolve, preamble should pass: {result:?}"
        );
    }

    // ── CodexCli arm ─────────────────────────────────────────────────────────

    #[test]
    fn test_codex_cli_binary_not_found() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("CODEX_BINARY", "/nonexistent/codex");
        let _g2 = EnvGuard::remove("OPENAI_API_KEY");
        let dir = tempfile::tempdir().unwrap();
        let _g3 = with_temp_config(&dir, "{}");
        let args = make_sync_args(Some(RunnerChoice::CodexCli));
        let result = sync_run_inner(&args, auth_not_authenticated);
        assert!(
            matches!(result, Err(ActualError::CodexNotFound)),
            "expected CodexNotFound, got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_codex_cli_model_requires_api_key() {
        // Model specified + no API key → CodexCliModelRequiresApiKey before binary lookup.
        // We need a valid codex binary so the path doesn't error on CodexNotFound first.
        // Actually, the model check happens AFTER binary lookup, so we need a binary.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let bin_dir = tempfile::tempdir().unwrap();
        let fake_codex = create_fake_executable(bin_dir.path(), "fake-codex", "", 0);
        let _g1 = EnvGuard::set("CODEX_BINARY", fake_codex.to_str().unwrap());
        let _g2 = EnvGuard::remove("OPENAI_API_KEY");
        let dir = tempfile::tempdir().unwrap();
        let _g3 = with_temp_config(&dir, "{}");
        let mut args = make_sync_args(Some(RunnerChoice::CodexCli));
        args.model = Some("gpt-5-mini".to_string());
        let result = sync_run_inner(&args, auth_not_authenticated);
        assert!(
            matches!(result, Err(ActualError::CodexCliModelRequiresApiKey { .. })),
            "expected CodexCliModelRequiresApiKey, got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_codex_cli_no_auth() {
        // No API key + no auth file → CodexNotAuthenticated.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let bin_dir = tempfile::tempdir().unwrap();
        let fake_codex = create_fake_executable(bin_dir.path(), "fake-codex", "", 0);
        let _g1 = EnvGuard::set("CODEX_BINARY", fake_codex.to_str().unwrap());
        let _g2 = EnvGuard::remove("OPENAI_API_KEY");
        // Point HOME to a temp dir so ~/.codex/auth.json doesn't exist
        let home_dir = tempfile::tempdir().unwrap();
        let _g3 = EnvGuard::set("HOME", home_dir.path().to_str().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let _g4 = with_temp_config(&dir, "{}");
        let args = make_sync_args(Some(RunnerChoice::CodexCli));
        let result = sync_run_inner(&args, auth_not_authenticated);
        assert!(
            matches!(result, Err(ActualError::CodexNotAuthenticated)),
            "expected CodexNotAuthenticated, got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_codex_cli_with_api_key() {
        // API key present → auth check passes, reaches run_sync.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let bin_dir = tempfile::tempdir().unwrap();
        let fake_codex = create_fake_executable(bin_dir.path(), "fake-codex", "", 0);
        let _g1 = EnvGuard::set("CODEX_BINARY", fake_codex.to_str().unwrap());
        let _g2 = EnvGuard::set("OPENAI_API_KEY", "sk-openai-test");
        let dir = tempfile::tempdir().unwrap();
        let _g3 = with_temp_config(&dir, "{}");
        let args = make_sync_args(Some(RunnerChoice::CodexCli));
        let result = sync_run_inner(&args, auth_not_authenticated);
        // Preamble succeeds; any error here comes from run_sync internals.
        assert!(
            !matches!(result, Err(ActualError::CodexNotFound)),
            "should have found fake binary: {result:?}"
        );
        assert!(
            !matches!(result, Err(ActualError::CodexNotAuthenticated)),
            "should be authenticated with API key: {result:?}"
        );
        assert!(
            !matches!(result, Err(ActualError::CodexCliModelRequiresApiKey { .. })),
            "no model specified so no model-key error expected: {result:?}"
        );
    }

    // ── CursorCli arm ────────────────────────────────────────────────────────

    #[test]
    fn test_cursor_cli_binary_not_found() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("CURSOR_BINARY", "/nonexistent/cursor-agent");
        let dir = tempfile::tempdir().unwrap();
        let _g2 = with_temp_config(&dir, "{}");
        let args = make_sync_args(Some(RunnerChoice::CursorCli));
        let result = sync_run_inner(&args, auth_not_authenticated);
        assert!(
            matches!(result, Err(ActualError::CursorNotFound)),
            "expected CursorNotFound, got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_cursor_cli_with_api_key() {
        // API key from env → injected into runner, preamble succeeds.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let bin_dir = tempfile::tempdir().unwrap();
        let fake_cursor = create_fake_executable(bin_dir.path(), "fake-cursor", "", 0);
        let _g1 = EnvGuard::set("CURSOR_BINARY", fake_cursor.to_str().unwrap());
        let _g2 = EnvGuard::set("CURSOR_API_KEY", "cursor-api-test-key");
        let dir = tempfile::tempdir().unwrap();
        let _g3 = with_temp_config(&dir, "{}");
        let args = make_sync_args(Some(RunnerChoice::CursorCli));
        let result = sync_run_inner(&args, auth_not_authenticated);
        assert!(
            !matches!(result, Err(ActualError::CursorNotFound)),
            "should have found fake binary: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_cursor_cli_without_api_key() {
        // No API key → runner still created (cursor-cli uses OAuth too), preamble succeeds.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let bin_dir = tempfile::tempdir().unwrap();
        let fake_cursor = create_fake_executable(bin_dir.path(), "fake-cursor", "", 0);
        let _g1 = EnvGuard::set("CURSOR_BINARY", fake_cursor.to_str().unwrap());
        let _g2 = EnvGuard::remove("CURSOR_API_KEY");
        let dir = tempfile::tempdir().unwrap();
        let _g3 = with_temp_config(&dir, "{}");
        let args = make_sync_args(Some(RunnerChoice::CursorCli));
        let result = sync_run_inner(&args, auth_not_authenticated);
        assert!(
            !matches!(result, Err(ActualError::CursorNotFound)),
            "should have found fake binary: {result:?}"
        );
    }

    // ── Preamble tests (runner selection, config loading) ────────────────────

    #[test]
    fn test_preamble_invalid_runner_config_string() {
        // An invalid runner string in config should produce a ConfigError.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let _g1 = with_temp_config(&dir, "runner: definitely-not-a-runner\n");
        let args = make_sync_args(None); // no --runner flag → falls through to config
        let result = sync_run_inner(&args, auth_not_authenticated);
        assert!(
            matches!(result, Err(ActualError::ConfigError(_))),
            "expected ConfigError for unknown runner in config, got: {result:?}"
        );
    }

    #[test]
    fn test_preamble_valid_runner_from_config() {
        // A valid runner in config (without env key) should try that runner.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ANTHROPIC_API_KEY");
        let dir = tempfile::tempdir().unwrap();
        let _g2 = with_temp_config(&dir, "runner: anthropic-api\n");
        let args = make_sync_args(None);
        let result = sync_run_inner(&args, auth_not_authenticated);
        // anthropic-api runner without a key → ApiKeyMissing, not a ConfigError
        assert!(
            matches!(result, Err(ActualError::ApiKeyMissing { ref env_var }) if env_var == "ANTHROPIC_API_KEY"),
            "expected ApiKeyMissing(ANTHROPIC_API_KEY), got: {result:?}"
        );
    }

    #[test]
    fn test_preamble_inferred_from_model() {
        // When model is set in config and no --runner flag, runner is auto-detected.
        // With gpt-5 + no binary + no API key, all candidates fail → NoRunnerAvailable.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("CODEX_BINARY", "/nonexistent/codex");
        let _g2 = EnvGuard::remove("OPENAI_API_KEY");
        let _g3 = EnvGuard::set("CURSOR_BINARY", "/nonexistent/cursor-agent");
        let dir = tempfile::tempdir().unwrap();
        let _g4 = with_temp_config(&dir, "model: gpt-5\n");
        let args = make_sync_args(None);
        let result = sync_run_inner(&args, auth_not_authenticated);
        // auto_detect_runner tries CodexCli (not found), OpenAiApi (no key), CursorCli (not found)
        assert!(
            matches!(result, Err(ActualError::NoRunnerAvailable { .. })),
            "expected NoRunnerAvailable when model set + all runners absent, got: {result:?}"
        );
    }

    #[test]
    fn test_preamble_malformed_config_uses_defaults() {
        // A malformed config file should fall back to defaults (eprintln warning).
        // auto_detect_runner is called with (none, none, none) and default config.
        // It tries ClaudeCli (not found), then AnthropicApi (no key) → NoRunnerAvailable.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/claude");
        let _g2 = EnvGuard::remove("ANTHROPIC_API_KEY");
        let dir = tempfile::tempdir().unwrap();
        let _g3 = with_temp_config(&dir, ": this is not valid yaml [\n");
        let args = make_sync_args(None); // no runner → falls through to auto_detect_runner
        let result = sync_run_inner(&args, auth_not_authenticated);
        // Falls back to defaults → auto_detect_runner → both candidates fail
        assert!(
            matches!(result, Err(ActualError::NoRunnerAvailable { .. })),
            "expected NoRunnerAvailable with defaults after malformed config, got: {result:?}"
        );
    }

    #[test]
    fn test_preamble_invocation_timeout_secs() {
        // invocation_timeout_secs config field is used to set subprocess_timeout.
        // We verify the preamble can read it without panicking.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/claude");
        let dir = tempfile::tempdir().unwrap();
        let _g2 = with_temp_config(&dir, "invocation_timeout_secs: 30\n");
        let args = make_sync_args(Some(RunnerChoice::ClaudeCli));
        let result = sync_run_inner(&args, auth_not_authenticated);
        // ClaudeCli binary not found → stops before auth_fn is called
        assert!(
            matches!(result, Err(ActualError::ClaudeNotFound)),
            "expected ClaudeNotFound, got: {result:?}"
        );
    }

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

    // ---- auto_detect_runner tests ----

    fn default_cfg() -> crate::config::types::Config {
        crate::config::types::Config::default()
    }

    #[cfg(unix)]
    fn make_fake_claude_logged_in(dir: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let fake = dir.join("fake-claude.sh");
        std::fs::write(
            &fake,
            "#!/bin/sh\nprintf '%s\\n' '{\"loggedIn\":true,\"authMethod\":\"claude.ai\"}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        fake
    }

    #[cfg(unix)]
    fn make_fake_binary(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let fake = dir.join(name);
        std::fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        fake
    }

    #[cfg(unix)]
    #[test]
    fn auto_detect_no_model_claude_available_returns_claude_cli() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let bin_dir = tempfile::tempdir().unwrap();
        let fake_claude = make_fake_claude_logged_in(bin_dir.path());
        let _g1 = EnvGuard::set("CLAUDE_BINARY", fake_claude.to_str().unwrap());
        let result = auto_detect_runner(None, None, &default_cfg());
        assert_eq!(result.unwrap(), RunnerChoice::ClaudeCli);
    }

    #[test]
    fn auto_detect_no_model_claude_unavailable_anthropic_key_set_returns_anthropic_api() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/claude");
        let _g2 = EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant-test");
        let result = auto_detect_runner(None, None, &default_cfg());
        assert_eq!(result.unwrap(), RunnerChoice::AnthropicApi);
    }

    #[test]
    fn auto_detect_no_model_no_runner_available_returns_error() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/claude");
        let _g2 = EnvGuard::remove("ANTHROPIC_API_KEY");
        let result = auto_detect_runner(None, None, &default_cfg());
        assert!(
            matches!(result, Err(ActualError::NoRunnerAvailable { .. })),
            "expected NoRunnerAvailable, got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn auto_detect_args_model_gpt_selects_codex_when_binary_available() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let bin_dir = tempfile::tempdir().unwrap();
        let fake_codex = make_fake_binary(bin_dir.path(), "fake-codex");
        let _g1 = EnvGuard::set("CODEX_BINARY", fake_codex.to_str().unwrap());
        let _g2 = EnvGuard::set("OPENAI_API_KEY", "sk-openai");
        let result = auto_detect_runner(Some("gpt-5"), None, &default_cfg());
        assert_eq!(result.unwrap(), RunnerChoice::CodexCli);
    }

    #[test]
    fn auto_detect_args_model_gpt_falls_back_to_openai_api_when_codex_missing() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("CODEX_BINARY", "/nonexistent/codex");
        let _g2 = EnvGuard::set("OPENAI_API_KEY", "sk-openai");
        let result = auto_detect_runner(Some("gpt-5"), None, &default_cfg());
        assert_eq!(result.unwrap(), RunnerChoice::OpenAiApi);
    }

    #[test]
    fn auto_detect_args_model_anthropic_full_returns_anthropic_api() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant-test");
        let result = auto_detect_runner(Some("claude-sonnet-4-6"), None, &default_cfg());
        assert_eq!(result.unwrap(), RunnerChoice::AnthropicApi);
    }

    #[cfg(unix)]
    #[test]
    fn auto_detect_args_model_short_alias_prefers_claude_cli() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let bin_dir = tempfile::tempdir().unwrap();
        let fake_claude = make_fake_claude_logged_in(bin_dir.path());
        let _g1 = EnvGuard::set("CLAUDE_BINARY", fake_claude.to_str().unwrap());
        let result = auto_detect_runner(Some("sonnet"), None, &default_cfg());
        assert_eq!(result.unwrap(), RunnerChoice::ClaudeCli);
    }

    #[cfg(unix)]
    #[test]
    fn auto_detect_cfg_model_gpt_selects_codex_when_available() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let bin_dir = tempfile::tempdir().unwrap();
        let fake_codex = make_fake_binary(bin_dir.path(), "fake-codex");
        let _g1 = EnvGuard::set("CODEX_BINARY", fake_codex.to_str().unwrap());
        let _g2 = EnvGuard::set("OPENAI_API_KEY", "sk-openai");
        let result = auto_detect_runner(None, Some("gpt-5-mini"), &default_cfg());
        assert_eq!(result.unwrap(), RunnerChoice::CodexCli);
    }

    #[test]
    fn auto_detect_cfg_model_anthropic_returns_anthropic_api() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant");
        let result = auto_detect_runner(None, Some("claude-sonnet-4-6"), &default_cfg());
        assert_eq!(result.unwrap(), RunnerChoice::AnthropicApi);
    }

    #[test]
    fn auto_detect_args_model_overrides_cfg_model() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant");
        let result = auto_detect_runner(Some("claude-sonnet-4-6"), None, &default_cfg());
        // args_model wins → AnthropicApi
        assert_eq!(result.unwrap(), RunnerChoice::AnthropicApi);
    }

    #[test]
    fn auto_detect_unrecognized_model_tries_cursor_only_unavailable_returns_error() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("CURSOR_BINARY", "/nonexistent/cursor-agent");
        let _g2 = EnvGuard::remove("CURSOR_API_KEY");
        let result = auto_detect_runner(Some("my-custom-model"), None, &default_cfg());
        assert!(
            matches!(
                result,
                Err(ActualError::NoRunnerAvailable { ref model, .. }) if model == "my-custom-model"
            ),
            "expected NoRunnerAvailable with model name, got: {result:?}"
        );
    }

    #[test]
    fn auto_detect_tried_string_lists_all_failures() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/claude");
        let _g2 = EnvGuard::remove("ANTHROPIC_API_KEY");
        let result = auto_detect_runner(Some("sonnet"), None, &default_cfg());
        let err = result.expect_err("expected NoRunnerAvailable");
        let msg = err.to_string();
        assert!(
            msg.contains("claude-cli"),
            "tried string must mention claude-cli: {msg}"
        );
        assert!(
            msg.contains("anthropic-api"),
            "tried string must mention anthropic-api: {msg}"
        );
    }

    #[test]
    fn auto_detect_config_anthropic_key_used_when_env_absent() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/claude");
        let _g2 = EnvGuard::remove("ANTHROPIC_API_KEY");
        let cfg = crate::config::types::Config {
            anthropic_api_key: Some("sk-ant-from-config".to_string()),
            ..Default::default()
        };
        let result = auto_detect_runner(None, None, &cfg);
        assert_eq!(result.unwrap(), RunnerChoice::AnthropicApi);
    }

    #[cfg(unix)]
    #[test]
    fn auto_detect_config_openai_key_used_for_codex() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let bin_dir = tempfile::tempdir().unwrap();
        let fake_codex = make_fake_binary(bin_dir.path(), "fake-codex");
        let _g1 = EnvGuard::set("CODEX_BINARY", fake_codex.to_str().unwrap());
        let _g2 = EnvGuard::remove("OPENAI_API_KEY");
        let cfg = crate::config::types::Config {
            openai_api_key: Some("sk-openai-from-config".to_string()),
            ..Default::default()
        };
        let result = auto_detect_runner(Some("gpt-5"), None, &cfg);
        assert_eq!(result.unwrap(), RunnerChoice::CodexCli);
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

    // ── codex_cli_fallback_if_model_error tests ──────────────────────────────

    /// Build a `RealTerminal` for tests (used as a dummy terminal in fallback
    /// function calls).
    fn make_term() -> crate::cli::ui::real_terminal::RealTerminal {
        crate::cli::ui::real_terminal::RealTerminal::default()
    }

    /// Build a model-error `ActualError::RunnerFailed`.
    fn model_error() -> ActualError {
        ActualError::RunnerFailed {
            message: "Codex CLI failed: model is not supported".to_string(),
            stderr: String::new(),
        }
    }

    #[test]
    fn test_fallback_non_error_result_passes_through() {
        // Ok(()) should pass through without triggering fallback.
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("cfg.yaml");
        let result = codex_cli_fallback_if_model_error(
            Ok(()),
            &make_sync_args(None),
            None,
            None,
            std::time::Duration::from_secs(60),
            dir.path(),
            &cfg_path,
            &make_term(),
        );
        assert!(
            result.is_ok(),
            "Ok(()) should pass through as Ok: {result:?}"
        );
    }

    #[test]
    fn test_fallback_non_model_error_passes_through() {
        // A non-model-error Err propagates as-is without fallback.
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("cfg.yaml");
        let result = codex_cli_fallback_if_model_error(
            Err(ActualError::CodexNotAuthenticated),
            &make_sync_args(None),
            None,
            None,
            std::time::Duration::from_secs(60),
            dir.path(),
            &cfg_path,
            &make_term(),
        );
        assert!(
            matches!(result, Err(ActualError::CodexNotAuthenticated)),
            "non-model error should propagate unchanged: {result:?}"
        );
    }

    #[test]
    fn test_fallback_model_error_no_api_key_returns_original() {
        // Model error + no API key → original model error returned, no fallback attempted.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard::remove("OPENAI_API_KEY");
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("cfg.yaml");
        let result = codex_cli_fallback_if_model_error(
            Err(model_error()),
            &make_sync_args(None),
            None,
            None, // no config key either
            std::time::Duration::from_secs(60),
            dir.path(),
            &cfg_path,
            &make_term(),
        );
        // Should return Err with the original model error message.
        let err = result.expect_err("expected the original model error to be returned");
        assert!(
            matches!(&err, ActualError::RunnerFailed { message, .. } if message.contains("model is not supported")),
            "expected the original model error, got: {err:?}"
        );
    }

    #[test]
    fn test_fallback_model_error_with_config_api_key_attempts_fallback() {
        // Model error + config API key → fallback is attempted (reaches run_sync).
        // run_sync will fail (no real repo), but we verify it got past key resolution.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard::remove("OPENAI_API_KEY");
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("cfg.yaml");
        let result = codex_cli_fallback_if_model_error(
            Err(model_error()),
            &make_sync_args(None),
            None,
            Some("sk-config-fallback-key"),
            std::time::Duration::from_secs(60),
            dir.path(),
            &cfg_path,
            &make_term(),
        );
        // Fallback was attempted (key resolved, runner created, run_sync called).
        // Result may be Err from run_sync internals (no real repo), but must NOT be
        // ApiKeyMissing — that would mean the key was not resolved.
        assert!(
            !matches!(&result, Err(ActualError::ApiKeyMissing { .. })),
            "API key was provided — should not get ApiKeyMissing: {result:?}"
        );
    }

    #[test]
    fn test_fallback_model_error_with_env_api_key_attempts_fallback() {
        // Model error + env API key → fallback is attempted.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard::set("OPENAI_API_KEY", "sk-env-test-key");
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("cfg.yaml");
        let result = codex_cli_fallback_if_model_error(
            Err(model_error()),
            &make_sync_args(None),
            Some("gpt-5".to_string()),
            None,
            std::time::Duration::from_secs(60),
            dir.path(),
            &cfg_path,
            &make_term(),
        );
        // Fallback was attempted; should not be ApiKeyMissing.
        assert!(
            !matches!(&result, Err(ActualError::ApiKeyMissing { .. })),
            "API key from env should resolve: {result:?}"
        );
    }

    #[test]
    fn test_preamble_cursor_model_yaml_field_folds_into_model() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::set("CURSOR_BINARY", "/nonexistent/cursor-agent");
        let _g2 = EnvGuard::remove("CURSOR_API_KEY");
        let dir = tempfile::tempdir().unwrap();
        // YAML uses old cursor_model key — must load into cfg.model via alias
        let _g3 = with_temp_config(&dir, "cursor_model: cursor-fast\n");
        let args = make_sync_args(None);
        let result = sync_run_inner(&args, auth_not_authenticated);
        // cursor-fast is unrecognized → runner_candidates returns [CursorCli]
        // → all fail (binary missing) → NoRunnerAvailable
        assert!(
            matches!(result, Err(ActualError::NoRunnerAvailable { .. })),
            "cursor_model YAML should fold into model and route through auto_detect_runner: {result:?}"
        );
    }
}
