pub mod aggregation;
pub mod display;
pub mod pipeline;
pub mod schemas;
pub mod types;

use clap::ValueEnum as _;

use crate::cli::args::{CrArgs, RunnerChoice};
use crate::config::paths::{config_path, load_from};
use crate::config::types::{DEFAULT_MODEL, DEFAULT_OPENAI_MODEL, DEFAULT_TIMEOUT_SECS};
use crate::error::ActualError;
use crate::runner::anthropic_api::{AnthropicApiRunner, DEFAULT_MAX_TOKENS};
use crate::runner::binary::find_claude_binary;
use crate::runner::probe::{
    is_anthropic_available, is_claude_available, is_codex_available, is_cursor_available,
    is_openai_available, resolve_api_key,
};
use crate::runner::subprocess::CliClaudeRunner;

use std::time::Duration;

/// Entry point for the `actual cr` command.
///
/// Resolves the runner (same pattern as sync_wiring.rs), builds a tokio runtime,
/// and runs the review pipeline.
pub fn exec(args: &CrArgs) -> Result<(), ActualError> {
    let cfg_path = config_path()?;
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

    // Resolve runner: CLI flag > config > auto-detect
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
        auto_detect_runner(args.model.as_deref(), cfg.model.as_deref(), &cfg)?
    };

    let subprocess_timeout =
        Duration::from_secs(cfg.invocation_timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));

    // Build the tokio runtime
    let rt = tokio::runtime::Runtime::new().map_err(|e| {
        ActualError::CodeReviewError(format!("Failed to create async runtime: {e}"))
    })?;

    match runner {
        RunnerChoice::ClaudeCli => {
            let binary_path = find_claude_binary()?;
            let effective_model = args
                .model
                .as_deref()
                .or(cfg.model.as_deref())
                .unwrap_or("sonnet")
                .to_string();
            let cli_runner = CliClaudeRunner::new(binary_path, subprocess_timeout);
            let cli_runner = if let Some(t) = cfg.max_turns {
                cli_runner.with_max_turns(t)
            } else {
                cli_runner
            };
            let exit_code = rt.block_on(pipeline::run_review(
                args,
                &cli_runner,
                "claude-cli",
                &effective_model,
            ))?;
            if exit_code != 0 {
                std::process::exit(exit_code);
            }
            Ok(())
        }
        RunnerChoice::AnthropicApi => {
            let api_key = resolve_api_key("ANTHROPIC_API_KEY", cfg.anthropic_api_key.as_deref())?;
            let model = args
                .model
                .as_deref()
                .or(cfg.model.as_deref())
                .unwrap_or(DEFAULT_MODEL)
                .to_string();
            let max_tokens = cfg.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
            let api_runner = if let Ok(base_url) = std::env::var("ANTHROPIC_API_BASE_URL") {
                AnthropicApiRunner::with_base_url(
                    api_key,
                    model.clone(),
                    subprocess_timeout,
                    base_url,
                    max_tokens,
                )?
            } else {
                AnthropicApiRunner::with_max_tokens(
                    api_key,
                    model.clone(),
                    subprocess_timeout,
                    max_tokens,
                )?
            };
            let exit_code = rt.block_on(pipeline::run_review(
                args,
                &api_runner,
                "anthropic-api",
                &model,
            ))?;
            if exit_code != 0 {
                std::process::exit(exit_code);
            }
            Ok(())
        }
        RunnerChoice::OpenAiApi => {
            let api_key = resolve_api_key("OPENAI_API_KEY", cfg.openai_api_key.as_deref())?;
            let model = args
                .model
                .as_deref()
                .or(cfg.model.as_deref())
                .unwrap_or(DEFAULT_OPENAI_MODEL)
                .to_string();
            let api_runner = crate::runner::openai_api::OpenAiApiRunner::new(
                api_key,
                model.clone(),
                subprocess_timeout,
            )?;
            let exit_code = rt.block_on(pipeline::run_review(
                args,
                &api_runner,
                "openai-api",
                &model,
            ))?;
            if exit_code != 0 {
                std::process::exit(exit_code);
            }
            Ok(())
        }
        RunnerChoice::CodexCli => Err(ActualError::CodeReviewError(
            "codex-cli runner does not support code review yet".to_string(),
        )),
        RunnerChoice::CursorCli => Err(ActualError::CodeReviewError(
            "cursor-cli runner does not support code review yet".to_string(),
        )),
    }
}

/// Auto-detect runner by probing environment (same logic as sync_wiring.rs).
fn auto_detect_runner(
    args_model: Option<&str>,
    model: Option<&str>,
    cfg: &crate::config::types::Config,
) -> Result<RunnerChoice, ActualError> {
    use crate::cli::args::runner_candidates;

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
