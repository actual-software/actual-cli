use clap::{Parser, Subcommand};

use crate::generation::OutputFormat;

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
    #[arg(long)]
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
    #[arg(long, value_name = "RUNNER")]
    pub runner: Option<String>,
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
