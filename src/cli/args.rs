use clap::{Parser, Subcommand};

use crate::cli::commands::models::known_cursor_model_names;
use crate::generation::OutputFormat;

/// AI backend runner selection for the `--runner` flag.
///
/// Using a typed enum prevents log injection: invalid runner names are rejected
/// by clap before they ever reach business logic or error messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerChoice {
    /// Claude Code CLI subprocess (default).
    ClaudeCli,
    /// Anthropic Messages API (requires `ANTHROPIC_API_KEY`).
    AnthropicApi,
    /// OpenAI Responses API (requires `OPENAI_API_KEY`).
    OpenAiApi,
    /// Codex CLI subprocess (requires `codex` binary).
    CodexCli,
    /// Cursor CLI subprocess (requires `agent` binary).
    CursorCli,
}

// ---- Model-detection helpers ----
//
// Shared by `RunnerChoice::infer_from_model()` and
// `RunnerChoice::model_compatibility_warning()`.  All inputs must already
// be lowercased.

/// Returns `true` for short Claude aliases that only work with `claude-cli`.
fn is_claude_short_alias(model: &str) -> bool {
    matches!(model, "sonnet" | "opus" | "haiku")
}

/// Returns `true` for any Anthropic model (full IDs **or** short aliases).
fn is_anthropic_model(model: &str) -> bool {
    model.contains("claude") || is_claude_short_alias(model)
}

/// Returns `true` for OpenAI model IDs (GPT, o-series, ChatGPT).
///
/// Codex models (e.g. `gpt-5.2-codex`, `gpt-5.1-codex-mini`) are excluded
/// because they belong to the `codex-cli` runner, not `openai-api`.
fn is_openai_model(model: &str) -> bool {
    !is_codex_model(model)
        && (model.starts_with("gpt-")
            || model.starts_with("o1")
            || model.starts_with("o3")
            || model.starts_with("o4")
            || model.starts_with("chatgpt-"))
}

/// Returns `true` for Codex-specific model IDs.
///
/// Matches both legacy `codex-*` names and the current `gpt-*-codex*` naming
/// pattern used by the Codex CLI (e.g. `gpt-5.2-codex`, `gpt-5.1-codex-mini`).
fn is_codex_model(model: &str) -> bool {
    model.starts_with("codex-") || model.ends_with("-codex") || model.contains("-codex-")
}

/// Returns `true` if the model name matches any known provider pattern.
fn is_known_model(model: &str) -> bool {
    is_claude_short_alias(model)
        || model.contains("claude")
        || is_openai_model(model)
        || is_codex_model(model)
}

/// Returns all runners capable of handling `model`, in tiebreak priority order.
/// First element is the preferred runner; subsequent elements are fallbacks.
/// `model` must be pre-lowercased by the caller.
pub(crate) fn runner_candidates(model: &str) -> Vec<RunnerChoice> {
    if is_claude_short_alias(model) {
        return vec![RunnerChoice::ClaudeCli, RunnerChoice::AnthropicApi];
    }
    if model.contains("claude") {
        return vec![RunnerChoice::AnthropicApi, RunnerChoice::ClaudeCli];
    }
    // is_openai_model and is_codex_model are mutually exclusive (is_openai_model calls !is_codex_model)
    if is_openai_model(model) {
        return vec![
            RunnerChoice::CodexCli,
            RunnerChoice::OpenAiApi,
            RunnerChoice::CursorCli,
        ];
    }
    if is_codex_model(model) {
        return vec![RunnerChoice::CodexCli, RunnerChoice::CursorCli];
    }
    // Unrecognized: CursorCli is the catch-all (accepts arbitrary model names)
    vec![RunnerChoice::CursorCli]
}

impl RunnerChoice {
    /// Human-readable name for display in the Environment section.
    pub fn display_name(&self) -> &'static str {
        match self {
            RunnerChoice::ClaudeCli => "claude-cli",
            RunnerChoice::AnthropicApi => "anthropic-api",
            RunnerChoice::OpenAiApi => "openai-api",
            RunnerChoice::CodexCli => "codex-cli",
            RunnerChoice::CursorCli => "cursor-cli",
        }
    }

    /// Infer the most appropriate runner from a model name.
    ///
    /// Used when neither `--runner` CLI flag nor `runner` config field is set,
    /// but a model is specified. Returns the runner most likely to support
    /// the given model based on naming conventions.
    ///
    /// Returns an error for unrecognized model strings so the user gets a
    /// clear message at startup instead of a silent fallback.
    pub fn infer_from_model(model: &str) -> Result<Self, String> {
        let m = model.to_ascii_lowercase();
        let candidates = runner_candidates(&m);
        // Only CursorCli in candidates means unrecognized model — preserve existing error
        if candidates == vec![RunnerChoice::CursorCli] {
            return Err(format!(
                "Unrecognized model '{model}'. Known patterns: sonnet, opus, haiku, claude-*, gpt-*, o1*, o3*, o4*, chatgpt-*, codex-*, gpt-*-codex*. \
                 Use --runner to explicitly select a runner for custom models."
            ));
        }
        Ok(candidates.into_iter().next().unwrap())
    }

    /// Check if the given model name is likely incompatible with this runner
    /// and return an actionable warning message if so.
    ///
    /// Returns `None` when the model appears compatible. Returns a warning
    /// for unrecognized model names (defense-in-depth when `--runner` is
    /// explicitly set but the model doesn't match any known pattern).
    pub fn model_compatibility_warning(&self, model: &str) -> Option<String> {
        let m = model.to_ascii_lowercase();

        let short_alias = is_claude_short_alias(&m);
        let anthropic = is_anthropic_model(&m);
        let openai = is_openai_model(&m);
        let codex = is_codex_model(&m);

        match self {
            RunnerChoice::ClaudeCli => {
                if openai {
                    Some(format!(
                        "Model \"{model}\" looks like an OpenAI model; \
                         consider --runner openai-api"
                    ))
                } else if codex {
                    Some(format!(
                        "Model \"{model}\" looks like a Codex model; \
                         consider --runner codex-cli"
                    ))
                } else if !is_known_model(&m) {
                    Some(format!(
                        "Model \"{model}\" is not a recognized model name. \
                         Use --runner to explicitly select a runner for custom models."
                    ))
                } else {
                    None
                }
            }
            RunnerChoice::AnthropicApi => {
                if short_alias {
                    Some(format!(
                        "Model \"{model}\" is a short alias; anthropic-api \
                         requires a full model name (e.g. claude-sonnet-4-5)"
                    ))
                } else if openai {
                    Some(format!(
                        "Model \"{model}\" looks like an OpenAI model; \
                         consider --runner openai-api"
                    ))
                } else if codex {
                    Some(format!(
                        "Model \"{model}\" looks like a Codex model; \
                         consider --runner codex-cli"
                    ))
                } else if !is_known_model(&m) {
                    Some(format!(
                        "Model \"{model}\" is not a recognized model name. \
                         Use --runner to explicitly select a runner for custom models."
                    ))
                } else {
                    None
                }
            }
            RunnerChoice::OpenAiApi => {
                if anthropic {
                    Some(format!(
                        "Model \"{model}\" looks like an Anthropic model; \
                         consider --runner claude-cli or --runner anthropic-api"
                    ))
                } else if codex {
                    Some(format!(
                        "Model \"{model}\" looks like a Codex model; \
                         consider --runner codex-cli"
                    ))
                } else if !is_known_model(&m) {
                    Some(format!(
                        "Model \"{model}\" is not a recognized model name. \
                         Use --runner to explicitly select a runner for custom models."
                    ))
                } else {
                    None
                }
            }
            RunnerChoice::CodexCli => {
                if anthropic {
                    Some(format!(
                        "Model \"{model}\" looks like an Anthropic model; \
                         consider --runner claude-cli or --runner anthropic-api"
                    ))
                } else if !is_known_model(&m) {
                    Some(format!(
                        "Model \"{model}\" is not a recognized model name. \
                         Use --runner to explicitly select a runner for custom models."
                    ))
                } else {
                    // OpenAI models on codex-cli are fine (codex uses OpenAI API)
                    None
                }
            }
            RunnerChoice::CursorCli => {
                // Warn when the model is not in the known cursor model list.
                // This is a soft warning only — Cursor may support additional models
                // not included in the curated list (run cursor-agent models to verify).
                let known = known_cursor_model_names();
                if !known.contains(&m.as_str()) {
                    Some(format!(
                        "cursor model '{model}' not in known list — \
                         run cursor-agent models to verify it's available on your account"
                    ))
                } else {
                    None
                }
            }
        }
    }
}

impl clap::ValueEnum for RunnerChoice {
    fn value_variants<'a>() -> &'a [Self] {
        &[
            RunnerChoice::ClaudeCli,
            RunnerChoice::AnthropicApi,
            RunnerChoice::OpenAiApi,
            RunnerChoice::CodexCli,
            RunnerChoice::CursorCli,
        ]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(match self {
            RunnerChoice::ClaudeCli => clap::builder::PossibleValue::new("claude-cli"),
            RunnerChoice::AnthropicApi => clap::builder::PossibleValue::new("anthropic-api"),
            RunnerChoice::OpenAiApi => clap::builder::PossibleValue::new("openai-api"),
            RunnerChoice::CodexCli => clap::builder::PossibleValue::new("codex-cli"),
            RunnerChoice::CursorCli => clap::builder::PossibleValue::new("cursor-cli"),
        })
    }
}

/// Parse and validate a budget value, rejecting negative and non-finite numbers.
fn parse_budget(s: &str) -> Result<f64, String> {
    let val: f64 = s
        .parse()
        .map_err(|_| format!("'{s}' is not a valid number"))?;
    if val < 0.0 || !val.is_finite() {
        return Err(format!(
            "budget must be a non-negative finite number, got {val}"
        ));
    }
    Ok(val)
}

/// Parse and validate a model name, rejecting flag-like values and shell metacharacters.
///
/// Allowed: alphanumeric start, then alphanumeric, dots, underscores, slashes, or hyphens.
/// Rejects anything starting with `-`, containing whitespace, shell metacharacters, or
/// exceeding 100 characters.
fn parse_model(s: &str) -> Result<String, String> {
    if s.starts_with('-') {
        return Err(format!("model name must not start with '-': {s:?}"));
    }
    if s.contains(char::is_whitespace) {
        return Err(format!("model name must not contain whitespace: {s:?}"));
    }
    for c in s.chars() {
        if matches!(
            c,
            '|' | '&' | ';' | '(' | ')' | '<' | '>' | '`' | '$' | '\\' | '!' | '\'' | '"'
        ) {
            return Err(format!(
                "model name contains invalid character {c:?}: {s:?}"
            ));
        }
    }
    if s.len() > 100 {
        return Err(format!("model name too long (max 100 chars): {s:?}"));
    }
    Ok(s.to_string())
}

/// ADR-powered CLAUDE.md generator
#[derive(Parser, Debug)]
#[command(name = "actual", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Analyze repo, fetch ADRs, tailor and write CLAUDE.md
    Sync(SyncArgs),
    /// Check CLAUDE.md state
    Status(StatusArgs),
    /// Check Claude Code authentication status
    Auth,
    /// View or edit configuration
    Config(ConfigArgs),
    /// List all available AI backend runners
    Runners,
    /// List known model names grouped by runner
    Models,
}

/// Arguments for the `sync` command
#[derive(Parser, Debug, Clone)]
pub struct SyncArgs {
    /// Show summary of what would change without writing files
    #[arg(long)]
    pub dry_run: bool,

    /// With --dry-run, output the full rendered CLAUDE.md to stdout
    #[arg(long, requires = "dry_run")]
    pub full: bool,

    /// Skip user confirmation and force fresh analysis and tailoring (bypass all caches)
    #[arg(long)]
    pub force: bool,

    /// Clear remembered ADR rejections and show all ADRs again
    #[arg(long)]
    pub reset_rejections: bool,

    /// Target a specific sub-project in a monorepo (can be repeated)
    #[arg(long = "project", value_name = "PATH")]
    pub projects: Vec<String>,

    /// Override Claude Code model (e.g., "sonnet", "opus")
    #[arg(long, value_parser = parse_model)]
    pub model: Option<String>,

    /// Override the ADR bank API endpoint
    #[arg(long)]
    pub api_url: Option<String>,

    /// Show detailed progress and Claude Code output
    #[arg(long)]
    pub verbose: bool,

    /// Skip the local tailoring step; use ADRs as-is from the bank
    #[arg(long)]
    pub no_tailor: bool,

    /// Maximum budget per tailoring invocation (USD)
    #[arg(long, value_parser = parse_budget, allow_hyphen_values = true)]
    pub max_budget_usd: Option<f64>,

    /// Disable the ratatui TUI and use plain line output instead
    #[arg(long)]
    pub no_tui: bool,

    /// Output file format to generate.
    ///
    /// Supported values:
    ///   claude-md    — write CLAUDE.md (default, for Claude Code)
    ///   agents-md    — write AGENTS.md (for Codex CLI and compatible tools)
    ///   cursor-rules — write .cursor/rules/actual-policies.mdc (for Cursor IDE)
    ///
    /// Examples:
    ///   actual sync --output-format agents-md
    ///   actual sync --output-format cursor-rules
    ///
    /// Can also be set permanently via: actual config set output_format agents-md
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub output_format: Option<OutputFormat>,

    /// AI backend to use for tailoring.
    ///
    /// Supported values:
    ///   claude-cli     — Claude Code CLI subprocess (default)
    ///   anthropic-api  — Anthropic Messages API (requires ANTHROPIC_API_KEY)
    ///   openai-api     — OpenAI Responses API (requires OPENAI_API_KEY)
    ///   codex-cli      — Codex CLI subprocess (requires codex binary)
    ///   cursor-cli     — Cursor CLI subprocess (requires agent binary)
    ///
    /// Can also be set permanently via: actual config set runner anthropic-api
    #[arg(long, value_enum, value_name = "RUNNER")]
    pub runner: Option<RunnerChoice>,

    /// Stream Claude Code subprocess stderr to the display in real time.
    ///
    /// When the 30-second hang warning fires, stderr output captured so far
    /// is shown. Without this flag only a generic tip is displayed.
    ///
    /// Useful for diagnosing hangs — permission prompts, auth failures, and
    /// other Claude Code diagnostics appear on stderr.
    #[arg(long)]
    pub show_errors: bool,
}

/// Arguments for the `status` command
#[derive(Parser, Debug)]
pub struct StatusArgs {
    /// Show full cached analysis and ADR counts
    #[arg(long)]
    pub verbose: bool,
}

/// Arguments for the `config` command
#[derive(Parser, Debug)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    /// Print current configuration
    Show,
    /// Set a configuration value (supports dotpath, e.g., options.batch_size)
    Set(ConfigSetArgs),
    /// Print config file location
    Path,
}

/// Arguments for `config set`
#[derive(Parser, Debug)]
pub struct ConfigSetArgs {
    /// Configuration key (dotpath notation)
    pub key: String,
    /// Configuration value
    pub value: String,
}

#[cfg(test)]
mod parse_tests {
    use super::*;
    use clap::Parser;

    /// Extract model from a parsed command; returns None for non-Sync commands.
    fn model_from_command(cmd: Command) -> Option<String> {
        match cmd {
            Command::Sync(args) => args.model,
            _ => None,
        }
    }

    #[test]
    fn test_model_from_non_sync_command_returns_none() {
        // Exercises the `_ => None` arm of model_from_command.
        let cli = Cli::try_parse_from(["actual", "status"]).unwrap();
        assert_eq!(model_from_command(cli.command), None);
    }

    // ---- RunnerChoice / --runner flag tests ----

    /// Helper to extract runner from a parsed Sync command.
    fn runner_from_command(cmd: Command) -> Option<RunnerChoice> {
        match cmd {
            Command::Sync(args) => args.runner,
            _ => None,
        }
    }

    /// Helper: try to parse `actual sync --runner <value>` and return the result.
    fn parse_runner(value: &str) -> Result<Option<RunnerChoice>, clap::Error> {
        Cli::try_parse_from(["actual", "sync", "--runner", value])
            .map(|cli| runner_from_command(cli.command))
    }

    #[test]
    fn test_runner_claude_cli_accepted() {
        let result = parse_runner("claude-cli");
        assert!(result.is_ok(), "claude-cli should be accepted");
        assert_eq!(result.unwrap(), Some(RunnerChoice::ClaudeCli));
    }

    #[test]
    fn test_runner_anthropic_api_accepted() {
        let result = parse_runner("anthropic-api");
        assert!(result.is_ok(), "anthropic-api should be accepted");
        assert_eq!(result.unwrap(), Some(RunnerChoice::AnthropicApi));
    }

    #[test]
    fn test_runner_openai_api_accepted() {
        let result = parse_runner("openai-api");
        assert!(result.is_ok(), "openai-api should be accepted");
        assert_eq!(result.unwrap(), Some(RunnerChoice::OpenAiApi));
    }

    #[test]
    fn test_runner_codex_cli_accepted() {
        let result = parse_runner("codex-cli");
        assert!(result.is_ok(), "codex-cli should be accepted");
        assert_eq!(result.unwrap(), Some(RunnerChoice::CodexCli));
    }

    #[test]
    fn test_runner_cursor_cli_accepted() {
        let result = parse_runner("cursor-cli");
        assert!(result.is_ok(), "cursor-cli should be accepted");
        assert_eq!(result.unwrap(), Some(RunnerChoice::CursorCli));
    }

    #[test]
    fn test_runner_invalid_value_rejected() {
        let result = parse_runner("unknown-runner");
        assert!(result.is_err(), "unknown-runner should be rejected by clap");
    }

    #[test]
    fn test_runner_log_injection_rejected() {
        // Ensure clap rejects a runner value that contains characters used for log
        // injection (newline + payload).  The key security property is that clap
        // rejects the value entirely — business logic (sync_wiring.rs) never sees
        // it, so it can never appear in an ActualError message constructed there.
        //
        // Clap's own error message may quote the invalid value for usability, but
        // that is acceptable because clap's error messages are not written to
        // structured logs; they are displayed directly to the user on stderr.
        let injected = "invalid\nlog-injection";
        let result = parse_runner(injected);
        assert!(
            result.is_err(),
            "log-injection runner value must be rejected by clap"
        );
    }

    #[test]
    fn test_runner_empty_value_rejected() {
        let result = parse_runner("");
        assert!(result.is_err(), "empty runner value should be rejected");
    }

    #[test]
    fn test_runner_absent_is_none() {
        let cli =
            Cli::try_parse_from(["actual", "sync"]).expect("sync without --runner should parse");
        assert_eq!(runner_from_command(cli.command), None);
    }

    // ---- parse_budget unit tests ----

    #[test]
    fn test_parse_budget_valid_zero() {
        assert_eq!(parse_budget("0").unwrap(), 0.0);
    }

    #[test]
    fn test_parse_budget_valid_positive() {
        assert_eq!(parse_budget("1.5").unwrap(), 1.5);
    }

    #[test]
    fn test_parse_budget_rejects_negative() {
        let err = parse_budget("-1.0").unwrap_err();
        assert!(err.contains("non-negative"), "message: {err}");
    }

    #[test]
    fn test_parse_budget_rejects_infinite() {
        // f64::INFINITY parses but is not finite
        let err = parse_budget("inf").unwrap_err();
        assert!(err.contains("non-negative"), "message: {err}");
    }

    #[test]
    fn test_parse_budget_rejects_invalid_string() {
        let err = parse_budget("not-a-number").unwrap_err();
        assert!(err.contains("not a valid number"), "message: {err}");
    }

    // ---- parse_model unit tests ----

    #[test]
    fn test_parse_model_valid() {
        let valid = [
            "sonnet",
            "opus",
            "gpt-5",
            "gpt-5.2",
            "claude-sonnet-4-6",
            "openai/gpt-5",
            "provider/model-name_v2",
            "gpt-5.2-codex",
            "gpt-5.1-codex-mini",
        ];
        for m in &valid {
            assert!(parse_model(m).is_ok());
        }
    }

    #[test]
    fn test_parse_model_rejects_flag_like() {
        let err = parse_model("--allow-dangerously-skip-permissions").unwrap_err();
        assert!(err.contains("must not start with '-'"), "message: {err}");
    }

    #[test]
    fn test_parse_model_rejects_leading_dash() {
        assert!(parse_model("-model").is_err());
    }

    #[test]
    fn test_parse_model_rejects_whitespace() {
        let err = parse_model("model name").unwrap_err();
        assert!(err.contains("whitespace"), "message: {err}");
    }

    #[test]
    fn test_parse_model_rejects_pipe() {
        let err = parse_model("model|cmd").unwrap_err();
        assert!(err.contains("invalid character"), "message: {err}");
    }

    #[test]
    fn test_parse_model_rejects_semicolon() {
        assert!(parse_model("model;cmd").is_err());
    }

    #[test]
    fn test_parse_model_rejects_dollar() {
        assert!(parse_model("model$var").is_err());
    }

    #[test]
    fn test_parse_model_rejects_backtick() {
        assert!(parse_model("model`cmd`").is_err());
    }

    #[test]
    fn test_parse_model_rejects_too_long() {
        let long_model = "a".repeat(101);
        let err = parse_model(&long_model).unwrap_err();
        assert!(err.contains("too long"), "message: {err}");
    }

    #[test]
    fn test_parse_model_accepts_exactly_100_chars() {
        // 1 char start + 99 valid chars = 100 total
        let model = format!("a{}", "b".repeat(99));
        assert_eq!(model.len(), 100);
        assert!(parse_model(&model).is_ok());
    }

    // ---- Integration: clap rejects flag-like --model values ----

    #[test]
    fn test_cli_rejects_flag_like_model() {
        let result = Cli::try_parse_from([
            "actual",
            "sync",
            "--model",
            "--allow-dangerously-skip-permissions",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cli_accepts_valid_model() {
        let cli = Cli::try_parse_from(["actual", "sync", "--model", "claude-sonnet-4-6"])
            .expect("expected Ok");
        assert_eq!(
            model_from_command(cli.command),
            Some("claude-sonnet-4-6".to_string())
        );
    }

    #[test]
    fn test_cli_rejects_model_with_shell_metacharacters() {
        let result = Cli::try_parse_from(["actual", "sync", "--model", "model|cmd"]);
        assert!(result.is_err(), "expected clap to reject model with '|'");
    }

    /// Helper to extract ConfigAction from a parsed Config command.
    fn config_action_from_command(cmd: Command) -> Option<ConfigAction> {
        match cmd {
            Command::Config(args) => Some(args.action),
            _ => None,
        }
    }

    #[test]
    fn test_runner_from_non_sync_command_returns_none() {
        // Exercises the `_ => None` arm of runner_from_command.
        let cli = Cli::try_parse_from(["actual", "status"]).unwrap();
        assert_eq!(runner_from_command(cli.command), None);
    }

    #[test]
    fn test_config_action_from_non_config_command_returns_none() {
        // Exercises the `_ => None` arm of config_action_from_command.
        let cli = Cli::try_parse_from(["actual", "status"]).unwrap();
        assert!(config_action_from_command(cli.command).is_none());
    }

    // ---- ConfigAction parsing tests ----

    #[test]
    fn test_config_show_parses() {
        let cli = Cli::try_parse_from(["actual", "config", "show"]).unwrap();
        let action = config_action_from_command(cli.command).expect("expected Config command");
        assert!(matches!(action, ConfigAction::Show));
    }

    #[test]
    fn test_config_path_parses() {
        let cli = Cli::try_parse_from(["actual", "config", "path"]).unwrap();
        let action = config_action_from_command(cli.command).expect("expected Config command");
        assert!(matches!(action, ConfigAction::Path));
    }

    /// Helper to extract ConfigSetArgs from a ConfigAction.
    fn set_args_from_action(action: ConfigAction) -> Option<ConfigSetArgs> {
        match action {
            ConfigAction::Set(args) => Some(args),
            _ => None,
        }
    }

    #[test]
    fn test_config_set_parses() {
        let cli =
            Cli::try_parse_from(["actual", "config", "set", "options.batch_size", "5"]).unwrap();
        let action = config_action_from_command(cli.command).expect("expected Config command");
        let set_args = set_args_from_action(action).expect("expected Set action");
        assert_eq!(set_args.key, "options.batch_size");
        assert_eq!(set_args.value, "5");
    }

    #[test]
    fn test_set_args_from_non_set_action_returns_none() {
        // Exercises the `_ => None` arm of set_args_from_action.
        let cli = Cli::try_parse_from(["actual", "config", "show"]).unwrap();
        let action = config_action_from_command(cli.command).expect("expected Config command");
        assert!(set_args_from_action(action).is_none());
    }

    // ---- parse_budget NaN test ----

    #[test]
    fn test_parse_budget_rejects_nan() {
        let err = parse_budget("nan").unwrap_err();
        assert!(err.contains("non-negative"), "message: {err}");
    }

    // ---- --no-tui flag tests ----

    /// Helper to extract no_tui from a parsed Sync command.
    fn no_tui_from_command(cmd: Command) -> bool {
        match cmd {
            Command::Sync(args) => args.no_tui,
            _ => false,
        }
    }

    #[test]
    fn test_no_tui_flag_absent_is_false() {
        let cli =
            Cli::try_parse_from(["actual", "sync"]).expect("sync without --no-tui should parse");
        assert!(!no_tui_from_command(cli.command));
    }

    #[test]
    fn test_no_tui_flag_present_is_true() {
        let cli = Cli::try_parse_from(["actual", "sync", "--no-tui"])
            .expect("sync with --no-tui should parse");
        assert!(no_tui_from_command(cli.command));
    }

    #[test]
    fn test_no_tui_from_non_sync_command_returns_false() {
        // Exercises the `_ => false` arm of no_tui_from_command.
        let cli = Cli::try_parse_from(["actual", "status"]).unwrap();
        assert!(!no_tui_from_command(cli.command));
    }

    // ---- RunnerChoice::display_name tests ----

    #[test]
    fn test_display_name_claude_cli() {
        assert_eq!(RunnerChoice::ClaudeCli.display_name(), "claude-cli");
    }

    #[test]
    fn test_display_name_anthropic_api() {
        assert_eq!(RunnerChoice::AnthropicApi.display_name(), "anthropic-api");
    }

    #[test]
    fn test_display_name_openai_api() {
        assert_eq!(RunnerChoice::OpenAiApi.display_name(), "openai-api");
    }

    #[test]
    fn test_display_name_codex_cli() {
        assert_eq!(RunnerChoice::CodexCli.display_name(), "codex-cli");
    }

    #[test]
    fn test_display_name_cursor_cli() {
        assert_eq!(RunnerChoice::CursorCli.display_name(), "cursor-cli");
    }

    // ---- model_compatibility_warning tests ----

    #[test]
    fn test_compat_claude_cli_with_anthropic_model_ok() {
        assert!(RunnerChoice::ClaudeCli
            .model_compatibility_warning("claude-sonnet-4-5")
            .is_none());
    }

    #[test]
    fn test_compat_claude_cli_with_short_alias_ok() {
        assert!(RunnerChoice::ClaudeCli
            .model_compatibility_warning("sonnet")
            .is_none());
        assert!(RunnerChoice::ClaudeCli
            .model_compatibility_warning("opus")
            .is_none());
        assert!(RunnerChoice::ClaudeCli
            .model_compatibility_warning("haiku")
            .is_none());
    }

    #[test]
    fn test_compat_claude_cli_with_openai_model_warns() {
        let warn = RunnerChoice::ClaudeCli
            .model_compatibility_warning("gpt-4o")
            .unwrap();
        assert!(warn.contains("OpenAI"), "msg: {warn}");
        assert!(warn.contains("openai-api"), "msg: {warn}");
    }

    #[test]
    fn test_compat_claude_cli_with_codex_model_warns() {
        let warn = RunnerChoice::ClaudeCli
            .model_compatibility_warning("gpt-5.2-codex")
            .unwrap();
        assert!(warn.contains("Codex"), "msg: {warn}");
        assert!(warn.contains("codex-cli"), "msg: {warn}");
    }

    #[test]
    fn test_compat_anthropic_api_with_short_alias_warns() {
        let warn = RunnerChoice::AnthropicApi
            .model_compatibility_warning("sonnet")
            .unwrap();
        assert!(warn.contains("short alias"), "msg: {warn}");
        assert!(warn.contains("full model name"), "msg: {warn}");
    }

    #[test]
    fn test_compat_anthropic_api_with_full_name_ok() {
        assert!(RunnerChoice::AnthropicApi
            .model_compatibility_warning("claude-sonnet-4-5")
            .is_none());
    }

    #[test]
    fn test_compat_anthropic_api_with_openai_model_warns() {
        let warn = RunnerChoice::AnthropicApi
            .model_compatibility_warning("gpt-4o")
            .unwrap();
        assert!(warn.contains("OpenAI"), "msg: {warn}");
    }

    #[test]
    fn test_compat_anthropic_api_with_codex_model_warns() {
        let warn = RunnerChoice::AnthropicApi
            .model_compatibility_warning("gpt-5.2-codex")
            .unwrap();
        assert!(warn.contains("Codex"), "msg: {warn}");
    }

    #[test]
    fn test_compat_openai_api_with_openai_model_ok() {
        assert!(RunnerChoice::OpenAiApi
            .model_compatibility_warning("gpt-4o")
            .is_none());
    }

    #[test]
    fn test_compat_openai_api_with_anthropic_model_warns() {
        let warn = RunnerChoice::OpenAiApi
            .model_compatibility_warning("claude-sonnet-4-6")
            .unwrap();
        assert!(warn.contains("Anthropic"), "msg: {warn}");
    }

    #[test]
    fn test_compat_openai_api_with_short_alias_warns() {
        let warn = RunnerChoice::OpenAiApi
            .model_compatibility_warning("haiku")
            .unwrap();
        assert!(warn.contains("Anthropic"), "msg: {warn}");
    }

    #[test]
    fn test_compat_openai_api_with_codex_model_warns() {
        let warn = RunnerChoice::OpenAiApi
            .model_compatibility_warning("gpt-5.2-codex")
            .unwrap();
        assert!(warn.contains("Codex"), "msg: {warn}");
    }

    #[test]
    fn test_compat_codex_cli_with_codex_model_ok() {
        assert!(RunnerChoice::CodexCli
            .model_compatibility_warning("gpt-5.2-codex")
            .is_none());
    }

    #[test]
    fn test_compat_codex_cli_with_openai_model_ok() {
        // OpenAI models on codex-cli are fine (codex uses OpenAI API)
        assert!(RunnerChoice::CodexCli
            .model_compatibility_warning("gpt-4o")
            .is_none());
    }

    #[test]
    fn test_compat_codex_cli_with_anthropic_model_warns() {
        let warn = RunnerChoice::CodexCli
            .model_compatibility_warning("claude-sonnet-4-5")
            .unwrap();
        assert!(warn.contains("Anthropic"), "msg: {warn}");
    }

    #[test]
    fn test_compat_cursor_cli_known_models_no_warning() {
        // Known cursor models should not produce warnings
        assert!(RunnerChoice::CursorCli
            .model_compatibility_warning("auto")
            .is_none());
        assert!(RunnerChoice::CursorCli
            .model_compatibility_warning("opus-4.6")
            .is_none());
        assert!(RunnerChoice::CursorCli
            .model_compatibility_warning("sonnet-4.6")
            .is_none());
        assert!(RunnerChoice::CursorCli
            .model_compatibility_warning("grok")
            .is_none());
        assert!(RunnerChoice::CursorCli
            .model_compatibility_warning("kimi-k2.5")
            .is_none());
    }

    #[test]
    fn test_compat_cursor_cli_unknown_model_warns() {
        // Unknown models should produce a soft warning with helpful message
        let warn = RunnerChoice::CursorCli
            .model_compatibility_warning("claude-sonnet-4-5")
            .unwrap();
        assert!(warn.contains("not in known list"), "msg: {warn}");
        assert!(warn.contains("cursor-agent models"), "msg: {warn}");

        let warn = RunnerChoice::CursorCli
            .model_compatibility_warning("gpt-4o")
            .unwrap();
        assert!(warn.contains("not in known list"), "msg: {warn}");

        let warn = RunnerChoice::CursorCli
            .model_compatibility_warning("my-custom-model")
            .unwrap();
        assert!(warn.contains("not in known list"), "msg: {warn}");
    }

    #[test]
    fn test_compat_unknown_model_warns() {
        // Unknown/custom models should trigger warnings on all runners
        assert!(RunnerChoice::ClaudeCli
            .model_compatibility_warning("my-custom-model")
            .is_some());
        assert!(RunnerChoice::AnthropicApi
            .model_compatibility_warning("my-custom-model")
            .is_some());
        assert!(RunnerChoice::OpenAiApi
            .model_compatibility_warning("my-custom-model")
            .is_some());
        assert!(RunnerChoice::CodexCli
            .model_compatibility_warning("my-custom-model")
            .is_some());
        // Cursor also warns for unknown models (soft warning — may still work)
        assert!(RunnerChoice::CursorCli
            .model_compatibility_warning("my-custom-model")
            .is_some());
    }

    #[test]
    fn test_compat_case_insensitive() {
        // Model matching should be case-insensitive
        let warn = RunnerChoice::ClaudeCli
            .model_compatibility_warning("GPT-4o")
            .unwrap();
        assert!(warn.contains("OpenAI"), "case-insensitive match: {warn}");
    }

    #[test]
    fn test_compat_o1_o3_o4_prefixes() {
        // OpenAI o-series models
        assert!(RunnerChoice::ClaudeCli
            .model_compatibility_warning("o1-preview")
            .is_some());
        assert!(RunnerChoice::ClaudeCli
            .model_compatibility_warning("o3-mini")
            .is_some());
        assert!(RunnerChoice::ClaudeCli
            .model_compatibility_warning("o4-mini")
            .is_some());
    }

    #[test]
    fn test_compat_chatgpt_prefix() {
        assert!(RunnerChoice::ClaudeCli
            .model_compatibility_warning("chatgpt-4o-latest")
            .is_some());
    }

    // ---- infer_from_model tests ----

    #[test]
    fn test_infer_from_model_claude_short_aliases() {
        assert_eq!(
            RunnerChoice::infer_from_model("sonnet").unwrap(),
            RunnerChoice::ClaudeCli
        );
        assert_eq!(
            RunnerChoice::infer_from_model("opus").unwrap(),
            RunnerChoice::ClaudeCli
        );
        assert_eq!(
            RunnerChoice::infer_from_model("haiku").unwrap(),
            RunnerChoice::ClaudeCli
        );
        // Case insensitive
        assert_eq!(
            RunnerChoice::infer_from_model("Sonnet").unwrap(),
            RunnerChoice::ClaudeCli
        );
        assert_eq!(
            RunnerChoice::infer_from_model("HAIKU").unwrap(),
            RunnerChoice::ClaudeCli
        );
    }

    #[test]
    fn test_infer_from_model_anthropic_full_ids() {
        assert_eq!(
            RunnerChoice::infer_from_model("claude-sonnet-4-6").unwrap(),
            RunnerChoice::AnthropicApi
        );
        assert_eq!(
            RunnerChoice::infer_from_model("claude-opus-4").unwrap(),
            RunnerChoice::AnthropicApi
        );
    }

    #[test]
    fn test_infer_from_model_openai() {
        // All OpenAI-family models now route to CodexCli (which supports
        // ChatGPT OAuth).  OpenAiApi is only used with explicit --runner.
        assert_eq!(
            RunnerChoice::infer_from_model("gpt-5.2").unwrap(),
            RunnerChoice::CodexCli
        );
        assert_eq!(
            RunnerChoice::infer_from_model("gpt-4o").unwrap(),
            RunnerChoice::CodexCli
        );
        assert_eq!(
            RunnerChoice::infer_from_model("o1-preview").unwrap(),
            RunnerChoice::CodexCli
        );
        assert_eq!(
            RunnerChoice::infer_from_model("o3-mini").unwrap(),
            RunnerChoice::CodexCli
        );
        assert_eq!(
            RunnerChoice::infer_from_model("o4-mini").unwrap(),
            RunnerChoice::CodexCli
        );
        assert_eq!(
            RunnerChoice::infer_from_model("chatgpt-4o-latest").unwrap(),
            RunnerChoice::CodexCli
        );
        // Case insensitive
        assert_eq!(
            RunnerChoice::infer_from_model("GPT-5.2").unwrap(),
            RunnerChoice::CodexCli
        );
    }

    #[test]
    fn test_infer_from_model_codex() {
        // Legacy codex-* prefix
        assert_eq!(
            RunnerChoice::infer_from_model("codex-mini").unwrap(),
            RunnerChoice::CodexCli
        );
        // Current gpt-*-codex naming pattern
        assert_eq!(
            RunnerChoice::infer_from_model("gpt-5.2-codex").unwrap(),
            RunnerChoice::CodexCli
        );
        assert_eq!(
            RunnerChoice::infer_from_model("gpt-5.1-codex-mini").unwrap(),
            RunnerChoice::CodexCli
        );
        assert_eq!(
            RunnerChoice::infer_from_model("gpt-5.1-codex-max").unwrap(),
            RunnerChoice::CodexCli
        );
        assert_eq!(
            RunnerChoice::infer_from_model("gpt-5-codex").unwrap(),
            RunnerChoice::CodexCli
        );
    }

    #[test]
    fn test_infer_from_model_unknown_returns_error() {
        assert!(RunnerChoice::infer_from_model("my-custom-model").is_err());
        assert!(RunnerChoice::infer_from_model("llama-3").is_err());
        assert!(RunnerChoice::infer_from_model("gemini-pro").is_err());
        // Verify error message is helpful
        let err = RunnerChoice::infer_from_model("gemini-pro").unwrap_err();
        assert!(err.contains("Unrecognized model"), "msg: {err}");
        assert!(err.contains("--runner"), "msg: {err}");
    }

    // ---- runner_candidates tests ----

    #[test]
    fn test_runner_candidates_short_aliases() {
        assert_eq!(
            runner_candidates("sonnet"),
            vec![RunnerChoice::ClaudeCli, RunnerChoice::AnthropicApi]
        );
        assert_eq!(
            runner_candidates("opus"),
            vec![RunnerChoice::ClaudeCli, RunnerChoice::AnthropicApi]
        );
        assert_eq!(
            runner_candidates("haiku"),
            vec![RunnerChoice::ClaudeCli, RunnerChoice::AnthropicApi]
        );
    }

    #[test]
    fn test_runner_candidates_claude_full_ids() {
        assert_eq!(
            runner_candidates("claude-sonnet-4-6"),
            vec![RunnerChoice::AnthropicApi, RunnerChoice::ClaudeCli]
        );
        assert_eq!(
            runner_candidates("claude-opus-4"),
            vec![RunnerChoice::AnthropicApi, RunnerChoice::ClaudeCli]
        );
    }

    #[test]
    fn test_runner_candidates_openai_models() {
        let expected = vec![
            RunnerChoice::CodexCli,
            RunnerChoice::OpenAiApi,
            RunnerChoice::CursorCli,
        ];
        assert_eq!(runner_candidates("gpt-5.2"), expected);
        assert_eq!(runner_candidates("gpt-4o"), expected);
        assert_eq!(runner_candidates("o1-preview"), expected);
        assert_eq!(runner_candidates("o3-mini"), expected);
        assert_eq!(runner_candidates("chatgpt-4o-latest"), expected);
    }

    #[test]
    fn test_runner_candidates_codex_models() {
        let expected = vec![RunnerChoice::CodexCli, RunnerChoice::CursorCli];
        assert_eq!(runner_candidates("gpt-5.2-codex"), expected);
        assert_eq!(runner_candidates("gpt-5.1-codex-mini"), expected);
        assert_eq!(runner_candidates("codex-mini"), expected);
    }

    #[test]
    fn test_runner_candidates_unrecognized_models() {
        assert_eq!(
            runner_candidates("my-custom-model"),
            vec![RunnerChoice::CursorCli]
        );
        assert_eq!(
            runner_candidates("gemini-pro"),
            vec![RunnerChoice::CursorCli]
        );
    }

    #[test]
    fn test_runner_candidates_first_element_consistency() {
        // Consistent with existing infer_from_model behavior
        assert_eq!(runner_candidates("sonnet")[0], RunnerChoice::ClaudeCli);
        assert_eq!(runner_candidates("gpt-4o")[0], RunnerChoice::CodexCli);
    }

    #[test]
    fn test_runner_candidates_unrecognized_does_not_break_infer_from_model() {
        // runner_candidates must not break the contract that infer_from_model("gemini-pro") returns Err
        assert!(RunnerChoice::infer_from_model("gemini-pro").is_err());
        assert!(RunnerChoice::infer_from_model("my-custom-model").is_err());
    }

    // ---- model-detection helper tests ----

    #[test]
    fn test_is_claude_short_alias() {
        assert!(is_claude_short_alias("sonnet"));
        assert!(is_claude_short_alias("opus"));
        assert!(is_claude_short_alias("haiku"));
        assert!(!is_claude_short_alias("claude-sonnet-4-6"));
        assert!(!is_claude_short_alias("gpt-4o"));
    }

    #[test]
    fn test_is_anthropic_model() {
        assert!(is_anthropic_model("claude-sonnet-4-6"));
        assert!(is_anthropic_model("sonnet"));
        assert!(is_anthropic_model("opus"));
        assert!(is_anthropic_model("haiku"));
        assert!(!is_anthropic_model("gpt-4o"));
        assert!(!is_anthropic_model("codex-mini"));
    }

    #[test]
    fn test_is_openai_model() {
        assert!(is_openai_model("gpt-4o"));
        assert!(is_openai_model("gpt-5.2"));
        assert!(is_openai_model("o1-preview"));
        assert!(is_openai_model("o3-mini"));
        assert!(is_openai_model("o4-mini"));
        assert!(is_openai_model("chatgpt-4o-latest"));
        assert!(!is_openai_model("claude-sonnet-4-6"));
        assert!(!is_openai_model("codex-mini"));
        // Codex models must NOT be classified as openai-api models
        assert!(!is_openai_model("gpt-5.2-codex"));
        assert!(!is_openai_model("gpt-5.1-codex-mini"));
        assert!(!is_openai_model("gpt-5.1-codex-max"));
    }

    #[test]
    fn test_is_codex_model() {
        // Legacy codex-* prefix
        assert!(is_codex_model("codex-mini"));
        // Current gpt-*-codex naming pattern
        assert!(is_codex_model("gpt-5.2-codex"));
        assert!(is_codex_model("gpt-5.1-codex-mini"));
        assert!(is_codex_model("gpt-5.1-codex-max"));
        assert!(is_codex_model("gpt-5-codex"));
        // Non-codex models
        assert!(!is_codex_model("gpt-4o"));
        assert!(!is_codex_model("gpt-5.2"));
        assert!(!is_codex_model("claude-sonnet-4-6"));
    }
}
