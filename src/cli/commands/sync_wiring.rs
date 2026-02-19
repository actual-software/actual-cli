//! Production wiring for `actual sync`.
//!
//! This module is excluded from coverage measurement (see coverage.yml)
//! because it instantiates real subprocess/API runners that cannot be used
//! in unit tests. The generic monomorphization of `run_sync<R>` only exists
//! here, keeping `sync.rs` at 100% line coverage.

use std::time::Duration;

use crate::cli::args::SyncArgs;
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
    let cfg = load_from(&cfg_path).unwrap_or_default();

    // Resolve the runner name: CLI flag > config > default.
    let runner_name = args
        .runner
        .as_deref()
        .or(cfg.runner.as_deref())
        .unwrap_or("claude-cli");

    match runner_name {
        "claude-cli" => {
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
            let runner = CliClaudeRunner::new(binary_path, Duration::from_secs(300));
            run_sync(args, &root_dir, &cfg_path, &term, &runner)
        }
        "anthropic-api" => {
            let api_key = resolve_api_key("ANTHROPIC_API_KEY", cfg.anthropic_api_key.as_deref())?;
            print_header_bar(&AuthDisplay {
                authenticated: true,
                email: Some("Anthropic API".to_string()),
            });
            let model = args
                .model
                .as_deref()
                .or(cfg.model.as_deref())
                .unwrap_or("claude-sonnet-4-5")
                .to_string();
            let runner = AnthropicApiRunner::new(api_key, model, Duration::from_secs(300));
            run_sync(args, &root_dir, &cfg_path, &term, &runner)
        }
        "openai-api" => {
            let api_key = resolve_api_key("OPENAI_API_KEY", cfg.openai_api_key.as_deref())?;
            print_header_bar(&AuthDisplay {
                authenticated: true,
                email: Some("OpenAI API".to_string()),
            });
            let model = args
                .model
                .as_deref()
                .or(cfg.model.as_deref())
                .unwrap_or("gpt-4o")
                .to_string();
            let runner = OpenAiApiRunner::new(api_key, model, Duration::from_secs(300));
            run_sync(args, &root_dir, &cfg_path, &term, &runner)
        }
        "codex-cli" => {
            let binary_path = find_codex_binary()?;
            print_header_bar(&AuthDisplay {
                authenticated: true,
                email: None,
            });
            let model = args
                .model
                .as_deref()
                .or(cfg.model.as_deref())
                .unwrap_or("codex-mini-latest")
                .to_string();
            let runner = CodexCliRunner::new(binary_path, model, Duration::from_secs(300));
            run_sync(args, &root_dir, &cfg_path, &term, &runner)
        }
        other => Err(ActualError::ConfigError(format!(
            "Unknown runner '{other}'. Valid values: claude-cli, anthropic-api, openai-api, codex-cli"
        ))),
    }
}

/// Resolve an API key from an environment variable or a config fallback.
///
/// Returns `Err(ActualError::ClaudeNotAuthenticated)` if neither source provides a key.
fn resolve_api_key(env_var: &str, config_key: Option<&str>) -> Result<String, ActualError> {
    std::env::var(env_var)
        .ok()
        .or_else(|| config_key.map(|s| s.to_string()))
        .ok_or(ActualError::ClaudeNotAuthenticated)
}
