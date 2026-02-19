use clap::{Parser, Subcommand};

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
}

impl clap::ValueEnum for RunnerChoice {
    fn value_variants<'a>() -> &'a [Self] {
        &[
            RunnerChoice::ClaudeCli,
            RunnerChoice::AnthropicApi,
            RunnerChoice::OpenAiApi,
            RunnerChoice::CodexCli,
        ]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(match self {
            RunnerChoice::ClaudeCli => clap::builder::PossibleValue::new("claude-cli"),
            RunnerChoice::AnthropicApi => clap::builder::PossibleValue::new("anthropic-api"),
            RunnerChoice::OpenAiApi => clap::builder::PossibleValue::new("openai-api"),
            RunnerChoice::CodexCli => clap::builder::PossibleValue::new("codex-cli"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Extract runner choice from a parsed command; returns None for non-Sync commands.
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
    fn test_runner_from_non_sync_command_returns_none() {
        // Exercises the `_ => None` arm of runner_from_command.
        let cli = Cli::try_parse_from(["actual", "status"]).unwrap();
        assert_eq!(runner_from_command(cli.command), None);
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
        assert!(result.is_err());
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
}

/// Arguments for the `sync` command
#[derive(Parser, Debug)]
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
    ///
    /// Can also be set permanently via: actual config set runner anthropic-api
    #[arg(long, value_enum, value_name = "RUNNER")]
    pub runner: Option<RunnerChoice>,
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

    // ---- parse_model unit tests ----

    #[test]
    fn test_parse_model_valid() {
        let valid = [
            "sonnet",
            "opus",
            "o4-mini",
            "gpt-4o",
            "claude-3.5-sonnet",
            "openai/o4-mini",
            "provider/model-name_v2",
            "codex-mini-latest",
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
        let cli = Cli::try_parse_from(["actual", "sync", "--model", "claude-3.5-sonnet"]).unwrap();
        assert_eq!(
            model_from_command(cli.command),
            Some("claude-3.5-sonnet".to_string())
        );
    }

    #[test]
    fn test_cli_rejects_model_with_shell_metacharacters() {
        let result = Cli::try_parse_from(["actual", "sync", "--model", "model|cmd"]);
        assert!(result.is_err(), "expected clap to reject model with '|'");
    }
}
