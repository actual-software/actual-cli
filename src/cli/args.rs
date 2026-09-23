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

/// Parse and validate `--max-rounds` / `ACTUAL_PLAN_CHECK_MAX_ROUNDS`, rejecting
/// anything under 1.
///
/// A value of 0 would mean "fail open on the very first denial" — silently
/// disabling the gate through a numeric knob, with no signal that's what
/// happened. There is already an explicit, obviously-named way to fully
/// disable governance (`ACTUAL_PLAN_GATE=off`, read by the shell hook in
/// `actual-skill`); this parser exists so a fat-fingered or templated-into-CI
/// zero is rejected outright instead of quietly reproducing that switch.
fn parse_max_rounds(s: &str) -> Result<u32, String> {
    let val: u32 = s
        .parse()
        .map_err(|_| format!("'{s}' is not a valid number"))?;
    if val < 1 {
        return Err(format!("max-rounds must be at least 1, got {val}"));
    }
    Ok(val)
}

/// Parse and validate a `check-override --rule` value, rejecting
/// anything that is not `<doc-slug>::<rule-id>`.
///
/// Without this, a value missing the `<doc-slug>::` prefix (an easy mistake
/// -- nothing else this command shows a human ever prints that prefix, by
/// design: `hook_deny_reason`'s own doc comment explains why the deny
/// message deliberately never assembles one) would be silently accepted,
/// stored, and reported as a successful override that can never actually
/// match a real rule — `GovernanceSession::excludes` compares against the
/// exact `doc_slug::rule_id` key, so a malformed one is a silent no-op the
/// human has no way to notice short of the same rule denying them again
/// next round.
fn parse_rule_key(s: &str) -> Result<String, String> {
    match s.split_once("::") {
        Some((doc_slug, rule_id)) if !doc_slug.is_empty() && !rule_id.is_empty() => {
            Ok(s.to_string())
        }
        _ => Err(format!(
            "'{s}' is not a valid rule key — expected <doc-slug>::<rule-id>, e.g. \
             cross-cutting-token-signing-1c57::R-A-001"
        )),
    }
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

/// Arguments for the `models` command
#[derive(Parser, Debug)]
pub struct ModelsArgs {
    /// Skip live API fetch; show only the hardcoded model list
    #[arg(long)]
    pub no_fetch: bool,
}

/// ADR-powered AI context file generator
#[derive(Parser, Debug)]
#[command(name = "actual", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Analyze repo, fetch ADRs, tailor and write AI context files
    AdrBot(SyncArgs),
    /// Check output file state
    Status(StatusArgs),
    /// Check the coding-agent runner's auth, or mint a scoped access token
    Auth(AuthArgs),
    /// Sign in to your Actual AI account via browser OAuth
    Login(LoginArgs),
    /// Sign out of your Actual AI account and clear local credentials
    Logout,
    /// Show the signed-in Actual AI account and organization
    Whoami,
    /// Ask the Advisor an org-scoped architecture question
    Advisor(AdvisorArgs),
    /// Mint an access token headlessly via the RFC 7523 jwt-bearer grant
    MintToken(MintTokenArgs),
    /// View or edit configuration
    Config(ConfigArgs),
    /// List all available AI backend runners
    Runners,
    /// List known model names grouped by runner
    Models(ModelsArgs),
    /// Clear local cache (analysis and tailoring results)
    Cache(CacheArgs),
    /// Inspect the rule documents under `.actual/rules/`
    Rules(RulesArgs),
    /// Check an implementation plan against the selected `.actual/rules/`
    PlanCheck(PlanCheckArgs),
    /// Check a `git diff` against the same committed `.actual/rules/` corpus
    /// `plan-check` checks plan text against
    ImplCheck(ImplCheckArgs),
    /// Explicitly override one or more rules the plan-check revision loop
    /// has denied, for a specific session — a human action, never the agent's
    ///
    /// Refused unless run interactively from an ordinary terminal: standard
    /// input must be a real tty, and this process's environment must not
    /// carry Claude Code's own `CLAUDECODE` / `CLAUDE_CODE_ENTRYPOINT`
    /// markers (present in every tool call Claude Code itself runs,
    /// including from its integrated terminal). Neither check is a
    /// cryptographic proof of human origin — a tool that allocates its own
    /// pty could still spoof the first, and an agent that scrubs its own
    /// environment could still clear the second — this raises the cost of
    /// an agent overriding its own denial by default, it does not guarantee
    /// against one that goes out of its way to evade detection. A human
    /// refused from Claude Code's integrated terminal should run the
    /// command from a separate, ordinary terminal instead.
    #[command(name = "check-override", visible_alias = "plan-check-override")]
    PlanCheckOverride(PlanCheckOverrideArgs),
}

/// Arguments for the `advisor` command
#[derive(Parser, Debug)]
pub struct AdvisorArgs {
    /// The architecture question to ask the Advisor. Optional when a scope
    /// command is used on its own: `--show-scope`, or a `--repo` value with no
    /// question (which just changes the remembered scope).
    #[arg(value_name = "QUERY", required_unless_present_any = ["show_scope", "repo"])]
    pub query: Option<String>,

    /// Advisor API base URL (e.g. http://localhost:3099 for the mock).
    /// Defaults to the production api-service.
    #[arg(long, value_name = "URL")]
    pub api_url: Option<String>,

    /// Organization to scope the query to. Defaults to the signed-in org.
    /// Required against the dev advisor mock, which expects a UUID org id.
    /// Passing `--org` without `--repo` runs this query at the organization
    /// level (an explicit opt-out of repo scoping) without changing the
    /// remembered scope.
    #[arg(long, value_name = "ORG_ID")]
    pub org: Option<String>,

    /// Scope the query to a specific connected repository, given either as its
    /// name (e.g. `actual-cli`, or `owner/actual-cli` to disambiguate a name
    /// shared across owners) or as its UUID. A name is resolved to a repo id via
    /// the connected-repos API; an unrecognized name fails with the list of
    /// repositories you can choose from. When omitted, the repository is
    /// auto-detected from the working tree's `origin` remote; if nothing matches
    /// (or the match is ambiguous), the query runs at the organization level.
    ///
    /// The chosen scope is remembered per repository for later `actual advisor`
    /// calls from the same working tree (see `--show-scope`). Two values are
    /// reserved keywords: `none` pins the scope to the organization level (opt
    /// out of repo scoping), and `auto` forgets the pin and reverts to
    /// git-remote auto-detection. To scope to a repository literally named
    /// `none` or `auto`, pass its UUID or `owner/name` form.
    #[arg(long, value_name = "REPO")]
    pub repo: Option<String>,

    /// Print the remembered advisor scope for the current repository and exit,
    /// without asking a question.
    #[arg(long)]
    pub show_scope: bool,
}

/// Arguments for the `mint-token` command — the fully-headless RFC 7523
/// jwt-bearer client. Every input comes from a flag, an environment variable,
/// or a file: no browser, no prompt. The private key is read from a file
/// (`--key` / `ACTUAL_SERVICE_ACCOUNT_KEY_FILE`) or, preferably, from the
/// `ACTUAL_SERVICE_ACCOUNT_KEY` environment variable so it never lands on argv.
#[derive(Parser, Debug)]
pub struct MintTokenArgs {
    /// Service-account id (a UUID). Becomes the assertion `iss`/`sub`.
    #[arg(long, value_name = "UUID", env = "ACTUAL_SERVICE_ACCOUNT_ID")]
    pub service_account_id: String,

    /// Registered key id, placed in the assertion header so the server picks
    /// the matching public key.
    #[arg(long, value_name = "KID", env = "ACTUAL_SERVICE_ACCOUNT_KID")]
    pub kid: String,

    /// Path to the service-account PRIVATE key in PEM (RSA for RS256, EC P-256
    /// for ES256). An EC key must be PKCS#8 ("BEGIN PRIVATE KEY"); SEC1 ("BEGIN
    /// EC PRIVATE KEY", what `openssl ecparam -genkey` writes) is refused, so
    /// convert it first with `openssl pkcs8 -topk8 -nocrypt`. Alternatively set
    /// `ACTUAL_SERVICE_ACCOUNT_KEY` to the PEM contents directly — preferred
    /// with a secret manager, and it keeps the key off argv.
    #[arg(long, value_name = "PATH", env = "ACTUAL_SERVICE_ACCOUNT_KEY_FILE")]
    pub key: Option<std::path::PathBuf>,

    /// Signing algorithm (`rs256` or `es256`). Inferred from the key when
    /// omitted. `HS*`/`none` are refused.
    #[arg(long, value_name = "ALG")]
    pub alg: Option<String>,

    /// OAuth issuer base URL; the token endpoint is `<issuer>/api/oauth/token`.
    /// Defaults to the production server, falling back through
    /// `ACTUAL_AUTH_URL`. Override for staging or a local server.
    #[arg(long, value_name = "URL")]
    pub issuer: Option<String>,

    /// Assertion audience. Defaults to the resolved issuer origin, which the
    /// server accepts alongside `<issuer>/api/oauth/token`.
    #[arg(long, value_name = "URL")]
    pub aud: Option<String>,

    /// Requested scope (repeatable), a subset of the principal's grant (e.g.
    /// `adr:query`, `adr:review`). When omitted the server mints the
    /// principal's full whitelist.
    #[arg(long = "scope", value_name = "SCOPE")]
    pub scopes: Vec<String>,

    /// Assertion lifetime in seconds, clamped to 1..=300. Default 60, which an
    /// explicit `0` also selects.
    #[arg(long, value_name = "SECONDS", default_value_t = 60)]
    pub assertion_ttl_seconds: u64,

    /// Print the full token response as one line of JSON instead of just the
    /// raw access token.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for the `adr-bot` command
#[derive(Parser, Debug, Clone)]
pub struct SyncArgs {
    /// Show summary of what would change without writing files
    #[arg(long)]
    pub dry_run: bool,

    /// With --dry-run, output the full rendered file to stdout
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
    ///   actual adr-bot --output-format agents-md
    ///   actual adr-bot --output-format cursor-rules
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

/// Arguments for the `login` command
#[derive(Parser, Debug)]
pub struct LoginArgs {
    /// Organization to sign in to. Required for accounts that belong to more
    /// than one organization; single-org accounts are auto-selected. If you
    /// belong to multiple orgs and the browser does not prompt within ~3
    /// minutes, pass `--org` explicitly.
    #[arg(long, value_name = "ORG_ID")]
    pub org: Option<String>,

    /// Auth server base URL. Defaults to the production OAuth server
    /// (https://app.actual.ai); falls back through the ACTUAL_AUTH_URL env var.
    /// Override for staging (https://app.staging.actual.ai) or the local mock
    /// (http://localhost:4000).
    #[arg(long, value_name = "URL")]
    pub api_url: Option<String>,

    /// Print the sign-in URL instead of opening a browser (useful over SSH).
    #[arg(long)]
    pub no_browser: bool,

    /// Use the browserless device-authorization flow (RFC 8628): print a short
    /// code + URL for a person to approve on any device, then poll for the
    /// session. For a human signing in from a remote or SSH shell with no local
    /// browser — a person must approve the code, so this is not an unattended
    /// path (CI has no approver; use `auth create-token` there instead). `--org`
    /// is ignored in this mode (the org is selected on the approval page).
    #[arg(long)]
    pub device: bool,
}

/// Arguments for the `auth` command group.
///
/// With no subcommand, `actual auth` keeps its original behavior — a check of
/// the underlying coding-agent runner (e.g. `claude auth status`). The
/// `create-token` subcommand mints a scoped Actual AI platform token.
#[derive(Parser, Debug)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: Option<AuthCommand>,
}

#[derive(Subcommand, Debug)]
pub enum AuthCommand {
    /// Mint a scoped personal access token for non-interactive (CI / agent) use
    CreateToken(CreateTokenArgs),
}

/// Arguments for `auth create-token`.
#[derive(Parser, Debug)]
pub struct CreateTokenArgs {
    /// Label identifying this token and the agent that will use it.
    ///
    /// Mint a DEDICATED token per agent (do not share the human's interactive
    /// login session) so every action is attributable and the credential can be
    /// revoked on its own.
    #[arg(long, value_name = "NAME")]
    pub name: String,

    /// Scopes to grant, comma- or space-separated
    /// (e.g. `--scopes adr:query,adr:review`).
    #[arg(long, value_name = "SCOPE", value_delimiter = ',', required = true, num_args = 1..)]
    pub scopes: Vec<String>,

    /// Issuance API base URL. Defaults to the api-service; falls back through
    /// the `ACTUAL_API_URL` env var. Point at a local mock for testing.
    #[arg(long, value_name = "URL")]
    pub api_url: Option<String>,
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

/// Arguments for the `cache` command
#[derive(Parser, Debug)]
pub struct CacheArgs {
    #[command(subcommand)]
    pub action: CacheAction,
}

#[derive(Subcommand, Debug)]
pub enum CacheAction {
    /// Clear all cached data (analysis and tailoring results)
    Clear,
}

/// Arguments for the `rules` command
#[derive(Parser, Debug)]
pub struct RulesArgs {
    #[command(subcommand)]
    pub action: RulesAction,
}

#[derive(Subcommand, Debug)]
pub enum RulesAction {
    /// List the rule documents under `.actual/rules/`
    Ls(RulesLsArgs),

    /// Build or refresh the scope index over `.actual/rules/`
    Index(RulesIndexArgs),

    /// Select the rule documents that govern a plan, or a path with no plan
    Select(RulesSelectArgs),

    /// Score the scope index against the filename scan on a golden set
    Eval(RulesEvalArgs),
}

/// Arguments for `rules ls`
#[derive(Parser, Debug)]
pub struct RulesLsArgs {
    /// Repository root to scan. Defaults to the current directory.
    #[arg(value_name = "PATH")]
    pub path: Option<std::path::PathBuf>,

    /// Print the parsed rule set as JSON instead of a panel.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `rules index`
#[derive(Parser, Debug)]
pub struct RulesIndexArgs {
    /// Repository root to scan. Defaults to the current directory.
    #[arg(value_name = "PATH")]
    pub path: Option<std::path::PathBuf>,

    /// Rebuild the index even when the cached one is still valid.
    #[arg(long)]
    pub rebuild: bool,

    /// Remove every cached scope index, including those left by other
    /// repositories, then rebuild this one.
    #[arg(long)]
    pub clear: bool,

    /// Print the index summary as JSON instead of a panel.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `rules select`
///
/// The plan text is the positional argument here, rather than the repository
/// root as in `rules ls`, because it is this command's subject. The root moves
/// to `--repo`.
#[derive(Parser, Debug)]
pub struct RulesSelectArgs {
    /// The plan to match against the rule set.
    ///
    /// Optional when at least one `--file` is given: a hook selecting for the
    /// file an agent is about to touch has a path and no plan, and passing
    /// `""` to satisfy a required argument is not an interface. One of the two
    /// is still required, because a query with neither names nothing to match.
    #[arg(value_name = "PLAN", required_unless_present = "files")]
    pub plan: Vec<String>,

    /// Repository root to scan. Defaults to the current directory.
    #[arg(long, value_name = "PATH")]
    pub repo: Option<std::path::PathBuf>,

    /// A file or directory to match against the rule set. Repeatable.
    ///
    /// With a plan, these are the paths the plan touches. Without a plan they
    /// are the query itself: a hook selecting for the file an agent is about
    /// to touch has a path and no plan.
    #[arg(long = "file", value_name = "PATH")]
    pub files: Vec<String>,

    /// Maximum number of rule documents to return.
    #[arg(long, default_value_t = 10)]
    pub limit: usize,

    /// Group the result by the decision each document belongs to, and count
    /// `--limit` in decisions rather than documents.
    ///
    /// A generated rule set splits one decision across many near-identical
    /// documents, so a document-level cap can spend itself on aspects of a
    /// single subject. Stage 1 only: the rank judges documents, and what is
    /// being capped here is decisions.
    #[arg(long = "by-adr")]
    pub by_adr: bool,

    /// How many candidates the deterministic prefilter retrieves before stage 2
    /// judges them. Raised to `--limit` when smaller.
    #[arg(
        long,
        default_value_t = crate::rules::scope::DEFAULT_CANDIDATES,
        conflicts_with = "by_adr"
    )]
    pub candidates: usize,

    /// Skip stage 2 and return the deterministic prefilter alone. Offline, and
    /// exactly reproducible.
    #[arg(long)]
    pub no_rank: bool,

    /// Runner to use for stage 2. Probed automatically when omitted.
    #[arg(long, value_enum, conflicts_with = "by_adr")]
    pub runner: Option<RunnerChoice>,

    /// Model for stage 2, overriding the configured one.
    #[arg(long, conflicts_with = "by_adr")]
    pub model: Option<String>,

    /// Show the signal behind every hit, and what the filename scan would have
    /// chosen instead.
    #[arg(long)]
    pub explain: bool,

    /// Print the selection as JSON instead of a panel.
    #[arg(long)]
    pub json: bool,

    /// Rebuild the index before selecting.
    #[arg(long)]
    pub rebuild: bool,
}

/// Arguments for `rules eval`
#[derive(Parser, Debug)]
pub struct RulesEvalArgs {
    /// Golden set: JSON array of `{name, plan, paths, expected}` cases.
    #[arg(long, value_name = "FILE")]
    pub golden: std::path::PathBuf,

    /// Repository root holding the rule set the golden set refers to.
    #[arg(long, value_name = "PATH")]
    pub repo: Option<std::path::PathBuf>,

    /// Number of documents each selector may return.
    #[arg(long, default_value_t = 5)]
    pub limit: usize,

    /// Score the index with one signal switched off, to measure what that
    /// signal is worth. Repeatable.
    #[arg(long = "ablate", value_name = "FIELD")]
    pub ablate: Vec<String>,

    /// Also score the two-stage selector, which costs one runner call per case.
    #[arg(long)]
    pub rank: bool,

    /// How many candidates the prefilter hands to stage 2 under `--rank`.
    #[arg(long, default_value_t = crate::rules::scope::DEFAULT_CANDIDATES)]
    pub candidates: usize,

    /// Runner to use for `--rank`. Probed automatically when omitted.
    #[arg(long, value_enum)]
    pub runner: Option<RunnerChoice>,

    /// Model for `--rank`, overriding the configured one.
    #[arg(long)]
    pub model: Option<String>,

    /// Print the full comparison as JSON instead of a panel.
    #[arg(long)]
    pub json: bool,

    /// Rebuild the index before evaluating, so the measurement is against
    /// the files on disk rather than a cached index.
    #[arg(long)]
    pub rebuild: bool,
}

/// Arguments for the `plan-check` command.
///
/// Two callers, two shapes. A human runs this directly: the plan is a
/// positional argument, a `--plan-file`, or stdin, and the result is a panel
/// or `--json`, with a nonzero exit reserved specifically for a `conflicting`
/// verdict (a real rule violation) — every other outcome, including
/// `not_checked` (no runner, no applicable rules, or the judge call itself
/// failed) and `requires_decision`, exits 0. A CI job that gates on this
/// command's exit code alone will not see the difference between "checked
/// and clean" and "could not check"; a job that needs that distinction should
/// read `--json`'s `status` field instead. `hooks/plan-gate.sh` runs it with
/// `--claude-hook`: the plan comes from a Claude Code `PreToolUse` hook
/// envelope on stdin instead, and the result is the hook's own JSON contract
/// — see `crate::cli::commands::plan_check` for what that contract requires.
///
/// This doc comment is not what `--help` shows for this subcommand — clap
/// takes a subcommand's "about" text from the `Command` enum variant's own
/// doc comment, not this struct's, so the exit-code explanation above is
/// real documentation that no CLI user would ever see without the
/// `after_help` below repeating the load-bearing part of it.
#[derive(Parser, Debug)]
#[command(
    after_help = "Exit codes: 0 for conforming, requires_decision, or not_checked (no \
runner available, no applicable rules, or the judge call itself failed) -- only a real \
conflict exits nonzero. A CI job that gates on exit code alone cannot distinguish \"checked \
and clean\" from \"could not check\"; read --json's `status` field for that distinction."
)]
pub struct PlanCheckArgs {
    /// The plan to check. Omit to read from `--plan-file` or stdin. Ignored
    /// under `--claude-hook`, which resolves the plan from the hook envelope.
    #[arg(value_name = "PLAN")]
    pub plan: Vec<String>,

    /// Read the plan from this file instead of the positional argument or
    /// stdin. Ignored under `--claude-hook`.
    #[arg(long, value_name = "PATH", conflicts_with = "claude_hook")]
    pub plan_file: Option<std::path::PathBuf>,

    /// Repository root to resolve rules and paths against. Defaults to the
    /// current directory.
    #[arg(long, value_name = "PATH")]
    pub repo: Option<std::path::PathBuf>,

    /// Rules directory to score against, overriding `<repo>/.actual/rules`.
    /// This is what `ACTUAL_RULES_DIR` becomes on the way into the CLI: the
    /// hook resolves the override itself and forwards the resolved path here
    /// rather than leaving this command to rediscover it from `--repo`.
    #[arg(long, value_name = "PATH")]
    pub rules_dir: Option<std::path::PathBuf>,

    /// Parse a Claude Code `PreToolUse` hook envelope from stdin and emit the
    /// hook's JSON contract on stdout instead of a panel. This is the mode
    /// `hooks/plan-gate.sh` drives; every failure degrades to fail-open rather
    /// than an error exit, per that contract.
    #[arg(long)]
    pub claude_hook: bool,

    /// Maximum number of rule documents to judge the plan against.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,

    /// How many candidates the deterministic prefilter retrieves before the
    /// limit is applied. Raised to `--limit` when smaller.
    #[arg(long, default_value_t = crate::rules::scope::DEFAULT_CANDIDATES)]
    pub candidates: usize,

    /// Skip stage 2 (a runner-backed rank refining which documents apply) and
    /// select with the deterministic prefilter alone. Direct-mode use only:
    /// `--claude-hook` always uses the prefilter alone regardless of this
    /// flag, so its one model call stays reserved for the conformance judge
    /// inside Claude Code's 120-second `PreToolUse` timeout — see
    /// `crate::cli::commands::plan_check` for why running both there risks
    /// that budget. A human at a terminal has no such constraint, so direct
    /// mode runs stage 2 by default, the same way `rules select` does.
    #[arg(long)]
    pub no_rank: bool,

    /// Runner to use for stage 2 selection (direct mode only) and the
    /// conformance judge. Probed automatically when omitted.
    #[arg(long, value_enum)]
    pub runner: Option<RunnerChoice>,

    /// Model for stage 2 selection (direct mode only) and the judge,
    /// overriding the configured one.
    #[arg(long)]
    pub model: Option<String>,

    /// Print the result as JSON instead of a panel. Ignored under
    /// `--claude-hook`, which always emits the hook's own JSON contract.
    #[arg(long)]
    pub json: bool,

    /// Rebuild the scope index before selecting.
    #[arg(long)]
    pub rebuild: bool,

    /// How many times a single rule may be denied within one Claude Code
    /// session (`--claude-hook` only) before the gate stops blocking on it
    /// specifically, regardless of verdict. Tracked per rule, not per round:
    /// a brand-new conflict always gets its own fresh count, no matter how
    /// exhausted some other rule's count already is. Direct mode ignores
    /// this — there is no session outside a hook envelope. Must be at least
    /// 1: a value of 0 is rejected outright rather than treated as "always
    /// fail open" (see `parse_max_rounds`).
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_ROUNDS,
        env = "ACTUAL_PLAN_CHECK_MAX_ROUNDS",
        value_parser = parse_max_rounds
    )]
    pub max_rounds: u32,
}

/// Default for [`PlanCheckArgs::max_rounds`].
pub const DEFAULT_MAX_ROUNDS: u32 = 3;

/// Arguments for the `impl-check` command.
///
/// `plan-check`'s implementation-stage counterpart (AK-755): the same
/// committed `.actual/rules/` corpus, the same shared pipeline
/// (`crate::cli::commands::check_engine::run_pipeline`), and the same
/// `--claude-hook` revision loop, judged against a `git diff` instead of plan
/// text. Two callers, two shapes, exactly like `plan-check`: a human runs
/// this directly, with the diff read from `--diff-file`, piped stdin, or (by
/// default) the working-tree diff in the resolved repo (tracked changes vs
/// `HEAD` plus untracked, non-ignored files), and the result is a panel
/// or `--json`, with a nonzero exit reserved specifically for a `conflicting`
/// verdict — every other outcome, including `not_checked` and
/// `requires_decision`, exits 0. A CI job that gates on this command's exit
/// code alone will not see the difference between "checked and clean" and
/// "could not check"; a job that needs that distinction should read
/// `--json`'s `status` field instead. Run with `--claude-hook`, the diff is
/// always resolved from the working tree — there is no envelope field carrying
/// diff text the way `tool_input.plan` carries plan text — and the result is
/// the hook's own JSON contract, shared verbatim with `plan-check
/// --claude-hook`.
///
/// This doc comment is not what `--help` shows for this subcommand — clap
/// takes a subcommand's "about" text from the `Command` enum variant's own
/// doc comment, not this struct's, so the exit-code explanation above is
/// real documentation that no CLI user would ever see without the
/// `after_help` below repeating the load-bearing part of it.
#[derive(Parser, Debug)]
#[command(
    after_help = "Exit codes: 0 for conforming, requires_decision, or not_checked (no \
runner available, no applicable rules, or the judge call itself failed) -- only a real \
conflict exits nonzero. A CI job that gates on exit code alone cannot distinguish \"checked \
and clean\" from \"could not check\"; read --json's `status` field for that distinction."
)]
pub struct ImplCheckArgs {
    /// Read the diff from this file instead of stdin or the working-tree
    /// diff. Mainly for scripting and testing. Ignored under `--claude-hook`,
    /// which always resolves the diff from the working tree (tracked changes
    /// vs `HEAD` plus untracked, non-ignored files).
    #[arg(long, value_name = "PATH", conflicts_with = "claude_hook")]
    pub diff_file: Option<std::path::PathBuf>,

    /// Repository root to resolve rules, paths, and the working-tree diff
    /// against. Defaults to the current directory.
    #[arg(long, value_name = "PATH")]
    pub repo: Option<std::path::PathBuf>,

    /// Rules directory to score against, overriding `<repo>/.actual/rules`.
    /// This is what `ACTUAL_RULES_DIR` becomes on the way into the CLI: the
    /// hook resolves the override itself and forwards the resolved path here
    /// rather than leaving this command to rediscover it from `--repo`.
    #[arg(long, value_name = "PATH")]
    pub rules_dir: Option<std::path::PathBuf>,

    /// Parse a Claude Code `PreToolUse` hook envelope from stdin and emit the
    /// hook's JSON contract on stdout instead of a panel. The diff is always
    /// resolved from the working tree in this mode (tracked changes vs `HEAD`
    /// plus untracked, non-ignored files); every failure degrades to
    /// fail-open rather than an error exit, per that contract.
    #[arg(long)]
    pub claude_hook: bool,

    /// Maximum number of rule documents to judge the diff against.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,

    /// How many candidates the deterministic prefilter retrieves before the
    /// limit is applied. Raised to `--limit` when smaller.
    #[arg(long, default_value_t = crate::rules::scope::DEFAULT_CANDIDATES)]
    pub candidates: usize,

    /// Skip stage 2 (a runner-backed rank refining which documents apply) and
    /// select with the deterministic prefilter alone. Direct-mode use only:
    /// `--claude-hook` always uses the prefilter alone regardless of this
    /// flag, so its one model call stays reserved for the conformance judge —
    /// see `crate::cli::commands::plan_check` for why running both there
    /// risks that budget. A human at a terminal has no such constraint, so
    /// direct mode runs stage 2 by default, the same way `rules select` does.
    #[arg(long)]
    pub no_rank: bool,

    /// Runner to use for stage 2 selection (direct mode only) and the
    /// conformance judge. Probed automatically when omitted.
    #[arg(long, value_enum)]
    pub runner: Option<RunnerChoice>,

    /// Model for stage 2 selection (direct mode only) and the judge,
    /// overriding the configured one.
    #[arg(long)]
    pub model: Option<String>,

    /// Print the result as JSON instead of a panel. Ignored under
    /// `--claude-hook`, which always emits the hook's own JSON contract.
    #[arg(long)]
    pub json: bool,

    /// Rebuild the scope index before selecting.
    #[arg(long)]
    pub rebuild: bool,

    /// How many times a single rule may be denied within one Claude Code
    /// session (`--claude-hook` only) before the gate stops blocking on it
    /// specifically, regardless of verdict. Tracked per rule, not per round:
    /// a brand-new conflict always gets its own fresh count, no matter how
    /// exhausted some other rule's count already is. Direct mode ignores
    /// this — there is no session outside a hook envelope. Must be at least
    /// 1: a value of 0 is rejected outright rather than treated as "always
    /// fail open" (see `parse_max_rounds`).
    ///
    /// Uses its own environment variable, distinct from `plan-check`'s
    /// `ACTUAL_PLAN_CHECK_MAX_ROUNDS`: deny counts, rounds, and content-scoped
    /// clearances are per artifact kind inside the shared session file, so a
    /// plan-stage denial cannot spend this budget. Human overrides on the
    /// same `(session_id, rules_dir)` still apply to both commands.
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_ROUNDS,
        env = "ACTUAL_IMPL_CHECK_MAX_ROUNDS",
        value_parser = parse_max_rounds
    )]
    pub max_rounds: u32,
}

/// Arguments for the `check-override` command (formerly `plan-check-override`,
/// still accepted as a backward-compatible alias): a human, never the
/// agent, explicitly clearing one or more rules that the revision loop
/// (`--claude-hook`) has denied, for a specific session.
///
/// [`crate::cli::commands::check_engine::exec_override`] refuses to run this
/// from an agent's default tool-execution environment (see that function's
/// doc for exactly what the terminal and `CLAUDECODE` checks do and do not
/// close) — the whole point of an override is that it is a human decision,
/// made outside the agent's control, and [`crate::cli::commands::governance_session::record_override`]
/// writes a durable, append-only audit-log entry for every one, so an
/// override is visible, never silent.
#[derive(Parser, Debug)]
pub struct PlanCheckOverrideArgs {
    /// The session to override, as named in the deny message. One Claude
    /// Code conversation can govern more than one repository or monorepo
    /// subproject, so `session_id` alone does not identify a revision loop —
    /// pair it with `--repo`/`--rules-dir` (defaulting to the current
    /// directory, same as `plan-check` itself) naming *which* governed
    /// context to override. Run this from the same repo the denial came
    /// from and the defaults already match.
    #[arg(long, value_name = "SESSION_ID")]
    pub session: String,

    /// A rule to override, as `<doc-slug>::<rule-id>` (also as printed in the
    /// deny message's suggested override command). Repeatable — one override
    /// call can clear several rules at once, each recorded as its own
    /// audit-log entry.
    #[arg(
        long = "rule",
        value_name = "DOC_SLUG::RULE_ID",
        required = true,
        num_args = 1..,
        value_parser = parse_rule_key
    )]
    pub rules: Vec<String>,

    /// Why this rule is being overridden. Required: an override with no
    /// stated reason defeats the point of recording one.
    #[arg(long, value_name = "TEXT")]
    pub reason: String,

    /// Repository root to resolve the rules directory against. Defaults to
    /// the current directory — run this from the repo the denial came from,
    /// same as `plan-check` itself.
    #[arg(long, value_name = "PATH")]
    pub repo: Option<std::path::PathBuf>,

    /// Rules directory this override applies to, overriding
    /// `<repo>/.actual/rules`. Must match what the hook resolved for this
    /// session (`ACTUAL_RULES_DIR`, if the denial came from a monorepo
    /// subproject) — this is what makes the override land on the same
    /// governed context the denial came from, rather than a same-named but
    /// unrelated one.
    #[arg(long, value_name = "PATH")]
    pub rules_dir: Option<std::path::PathBuf>,
}

#[cfg(test)]
mod parse_tests {
    use super::*;
    use clap::Parser;

    /// Extract model from a parsed command; returns None for non-AdrBot commands.
    fn model_from_command(cmd: Command) -> Option<String> {
        match cmd {
            Command::AdrBot(args) => args.model,
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

    /// Helper to extract runner from a parsed AdrBot command.
    fn runner_from_command(cmd: Command) -> Option<RunnerChoice> {
        match cmd {
            Command::AdrBot(args) => args.runner,
            _ => None,
        }
    }

    /// Helper: try to parse `actual adr-bot --runner <value>` and return the result.
    fn parse_runner(value: &str) -> Result<Option<RunnerChoice>, clap::Error> {
        Cli::try_parse_from(["actual", "adr-bot", "--runner", value])
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
        let cli = Cli::try_parse_from(["actual", "adr-bot"])
            .expect("adr-bot without --runner should parse");
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

    // ---- parse_max_rounds unit tests ----

    #[test]
    fn test_parse_max_rounds_valid_positive() {
        assert_eq!(parse_max_rounds("3").unwrap(), 3);
    }

    #[test]
    fn test_parse_max_rounds_rejects_zero() {
        let err = parse_max_rounds("0").unwrap_err();
        assert!(err.contains("at least 1"), "message: {err}");
    }

    #[test]
    fn test_parse_max_rounds_rejects_negative() {
        assert!(parse_max_rounds("-1").is_err());
    }

    #[test]
    fn test_parse_max_rounds_rejects_invalid_string() {
        let err = parse_max_rounds("nope").unwrap_err();
        assert!(err.contains("not a valid number"), "message: {err}");
    }

    // ---- parse_rule_key unit tests ----

    #[test]
    fn test_parse_rule_key_valid() {
        assert_eq!(
            parse_rule_key("cross-cutting-token-signing-1c57::R-A-001").unwrap(),
            "cross-cutting-token-signing-1c57::R-A-001"
        );
    }

    /// The gap this guards: a bare rule id with no doc-slug prefix must be
    /// rejected outright, not silently accepted as an override that can
    /// never match anything.
    #[test]
    fn test_parse_rule_key_rejects_bare_rule_id() {
        let err = parse_rule_key("R-A-001").unwrap_err();
        assert!(err.contains("doc-slug"), "message: {err}");
    }

    #[test]
    fn test_parse_rule_key_rejects_empty_doc_slug() {
        assert!(parse_rule_key("::R-A-001").is_err());
    }

    #[test]
    fn test_parse_rule_key_rejects_empty_rule_id() {
        assert!(parse_rule_key("some-doc::").is_err());
    }

    #[test]
    fn test_parse_rule_key_rejects_empty_string() {
        assert!(parse_rule_key("").is_err());
    }

    #[test]
    fn test_cli_rejects_plan_check_override_without_doc_slug() {
        let result = Cli::try_parse_from([
            "actual",
            "plan-check-override",
            "--session",
            "s1",
            "--rule",
            "R-A-001",
            "--reason",
            "reviewed",
        ]);
        assert!(result.is_err());
    }

    /// The gap this guards: clap_derive resets a wrapped args struct's own
    /// `long_about` to the enum variant's doc comment when that comment is a
    /// single paragraph, so `actual plan-check-override --help` used to
    /// print only the one-line summary and never mention either check a
    /// refusal points a human at. A multi-paragraph variant doc renders in
    /// full instead.
    #[test]
    fn test_plan_check_override_long_help_describes_both_gating_checks() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        let mut sub = cmd
            .find_subcommand("plan-check-override")
            .expect("plan-check-override is a registered subcommand")
            .clone();
        let help = sub.render_long_help().to_string();
        assert!(
            help.contains("CLAUDECODE"),
            "long help does not mention the CLAUDECODE marker check: {help}"
        );
        assert!(
            help.to_lowercase().contains("terminal"),
            "long help does not mention the terminal check: {help}"
        );
    }

    #[test]
    fn test_cli_accepts_plan_check_override_with_a_valid_rule_key() {
        let cli = Cli::try_parse_from([
            "actual",
            "plan-check-override",
            "--session",
            "s1",
            "--rule",
            "some-doc::R-A-001",
            "--reason",
            "reviewed",
        ])
        .unwrap();
        #[rustfmt::skip]
        let Command::PlanCheckOverride(args) = cli.command else { panic!("expected PlanCheckOverride command") };
        assert_eq!(args.rules, vec!["some-doc::R-A-001".to_string()]);
    }

    /// `check-override` is now the primary, canonical name (AK-755 step 3):
    /// `plan-check-override` above still parses only because it is kept as a
    /// `visible_alias`, not because it is still the primary spelling.
    #[test]
    fn test_cli_accepts_check_override_as_the_primary_name() {
        let cli = Cli::try_parse_from([
            "actual",
            "check-override",
            "--session",
            "s1",
            "--rule",
            "some-doc::R-A-001",
            "--reason",
            "reviewed",
        ])
        .unwrap();
        #[rustfmt::skip]
        let Command::PlanCheckOverride(args) = cli.command else { panic!("expected PlanCheckOverride command") };
        assert_eq!(args.rules, vec!["some-doc::R-A-001".to_string()]);
    }

    /// `--help` must document both the new primary name and the old one, so
    /// a user or script that only ever knew `plan-check-override` can
    /// discover the rename instead of being left with a name that silently
    /// stopped being mentioned anywhere.
    #[test]
    fn test_cli_help_mentions_both_check_override_names() {
        use clap::CommandFactory;
        let help = Cli::command().render_long_help().to_string();
        assert!(
            help.contains("check-override"),
            "top-level help does not list check-override: {help}"
        );
        assert!(
            help.contains("plan-check-override"),
            "top-level help does not mention the plan-check-override alias: {help}"
        );
    }

    #[test]
    fn test_cli_rejects_max_rounds_zero() {
        let result = Cli::try_parse_from(["actual", "plan-check", "--max-rounds", "0", "a plan"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cli_accepts_valid_max_rounds() {
        let cli =
            Cli::try_parse_from(["actual", "plan-check", "--max-rounds", "5", "a plan"]).unwrap();
        #[rustfmt::skip]
        let Command::PlanCheck(args) = cli.command else { panic!("expected PlanCheck command") };
        assert_eq!(args.max_rounds, 5);
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
            "adr-bot",
            "--model",
            "--allow-dangerously-skip-permissions",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cli_accepts_valid_model() {
        let cli = Cli::try_parse_from(["actual", "adr-bot", "--model", "claude-sonnet-4-6"])
            .expect("expected Ok");
        assert_eq!(
            model_from_command(cli.command),
            Some("claude-sonnet-4-6".to_string())
        );
    }

    #[test]
    fn test_cli_rejects_model_with_shell_metacharacters() {
        let result = Cli::try_parse_from(["actual", "adr-bot", "--model", "model|cmd"]);
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

    /// Helper to extract no_tui from a parsed AdrBot command.
    fn no_tui_from_command(cmd: Command) -> bool {
        match cmd {
            Command::AdrBot(args) => args.no_tui,
            _ => false,
        }
    }

    #[test]
    fn test_no_tui_flag_absent_is_false() {
        let cli = Cli::try_parse_from(["actual", "adr-bot"])
            .expect("adr-bot without --no-tui should parse");
        assert!(!no_tui_from_command(cli.command));
    }

    #[test]
    fn test_no_tui_flag_present_is_true() {
        let cli = Cli::try_parse_from(["actual", "adr-bot", "--no-tui"])
            .expect("adr-bot with --no-tui should parse");
        assert!(no_tui_from_command(cli.command));
    }

    #[test]
    fn test_no_tui_from_non_adr_bot_command_returns_false() {
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

    // ---- ModelsArgs / --no-fetch flag tests ----

    fn no_fetch_from_command(cmd: Command) -> bool {
        match cmd {
            Command::Models(args) => args.no_fetch,
            _ => false,
        }
    }

    #[test]
    fn test_models_no_fetch_flag_absent_is_false() {
        let cli = Cli::try_parse_from(["actual", "models"])
            .expect("models without --no-fetch should parse");
        assert!(!no_fetch_from_command(cli.command));
    }

    #[test]
    fn test_models_no_fetch_flag_present_is_true() {
        let cli = Cli::try_parse_from(["actual", "models", "--no-fetch"])
            .expect("models with --no-fetch should parse");
        assert!(no_fetch_from_command(cli.command));
    }

    #[test]
    fn test_models_parses() {
        let cli =
            Cli::try_parse_from(["actual", "models"]).expect("models subcommand should parse");
        assert!(matches!(cli.command, Command::Models(_)));
    }

    #[test]
    fn test_no_fetch_from_non_models_command_returns_false() {
        // Exercises the `_ => false` arm of no_fetch_from_command
        let cli = Cli::try_parse_from(["actual", "status"]).unwrap();
        assert!(!no_fetch_from_command(cli.command));
    }

    /// Parse `argv` and return the advisor args, or `None` when parsing fails or
    /// the command is not `advisor`.
    fn advisor_args_from(argv: &[&str]) -> Option<AdvisorArgs> {
        match Cli::try_parse_from(argv).ok()?.command {
            Command::Advisor(a) => Some(a),
            _ => None,
        }
    }

    #[test]
    fn test_advisor_query_and_flags_parse() {
        let a = advisor_args_from(&[
            "actual",
            "advisor",
            "why the app router?",
            "--repo",
            "myrepo",
        ])
        .expect("advisor with a question should parse");
        assert_eq!(a.query.as_deref(), Some("why the app router?"));
        assert_eq!(a.repo.as_deref(), Some("myrepo"));
        assert!(!a.show_scope);
    }

    #[test]
    fn test_advisor_show_scope_makes_query_optional() {
        let a = advisor_args_from(&["actual", "advisor", "--show-scope"])
            .expect("--show-scope needs no question");
        assert!(a.query.is_none());
        assert!(a.show_scope);
    }

    #[test]
    fn test_advisor_repo_change_makes_query_optional() {
        let a = advisor_args_from(&["actual", "advisor", "--repo", "none"])
            .expect("a --repo change needs no question");
        assert!(a.query.is_none());
        assert_eq!(a.repo.as_deref(), Some("none"));
    }

    #[test]
    fn test_advisor_requires_question_or_scope_flag() {
        // Neither a question nor a scope flag → clap rejects the invocation.
        assert!(advisor_args_from(&["actual", "advisor"]).is_none());
    }

    #[test]
    fn test_advisor_args_from_non_advisor_command_is_none() {
        // Covers the helper's non-advisor fallback arm.
        assert!(advisor_args_from(&["actual", "status"]).is_none());
    }

    /// Extract `rules index` arguments from a parsed command.
    ///
    /// A helper returning `Option` rather than a `match` with `panic!` arms,
    /// matching `advisor_args_from` above: the fallback is then an ordinary
    /// value a test can assert on, instead of an unreachable arm that no test
    /// can execute.
    fn rules_index_args_from(argv: &[&str]) -> Option<RulesIndexArgs> {
        match Cli::try_parse_from(argv).ok()?.command {
            Command::Rules(args) => match args.action {
                RulesAction::Index(index) => Some(index),
                _ => None,
            },
            _ => None,
        }
    }

    #[test]
    fn test_rules_index_clear_parses() {
        let index = rules_index_args_from(["actual", "rules", "index", "--clear"].as_slice())
            .expect("expected rules index command");
        assert!(index.clear);
        assert!(!index.rebuild);
    }

    /// Covers the helper's two non-index fallback arms.
    #[test]
    fn test_rules_index_args_from_other_commands_is_none() {
        assert!(rules_index_args_from(["actual", "rules", "ls"].as_slice()).is_none());
        assert!(rules_index_args_from(["actual", "status"].as_slice()).is_none());
    }

    /// Extract `plan-check` arguments from a parsed command.
    fn plan_check_args_from(argv: &[&str]) -> Option<PlanCheckArgs> {
        match Cli::try_parse_from(argv).ok()?.command {
            Command::PlanCheck(args) => Some(args),
            _ => None,
        }
    }

    fn rules_select_args_from(argv: &[&str]) -> Option<RulesSelectArgs> {
        match Cli::try_parse_from(argv).ok()?.command {
            Command::Rules(rules) => match rules.action {
                RulesAction::Select(args) => Some(args),
                _ => None,
            },
            _ => None,
        }
    }

    /// `--by-adr` changes what `--limit` counts, so it has to survive parsing
    /// alongside it rather than being read from a default.
    #[test]
    fn test_rules_select_parses_by_adr() {
        let args = rules_select_args_from(&[
            "actual",
            "rules",
            "select",
            "--file",
            "src/lib.rs",
            "--by-adr",
            "--limit",
            "2",
        ])
        .expect("expected a rules select command");
        assert!(args.by_adr);
        assert_eq!(args.limit, 2);

        let plain = rules_select_args_from(&["actual", "rules", "select", "a plan"])
            .expect("expected a rules select command");
        assert!(!plain.by_adr);
    }

    /// Grouped selection is stage 1 only, so accepting stage-2 tuning flags
    /// would imply they shape an answer that never consults them.
    #[test]
    fn test_rules_select_by_adr_rejects_stage_two_tuning_flags() {
        for (flag, value) in [
            ("--candidates", "12"),
            ("--runner", "anthropic-api"),
            ("--model", "claude-sonnet-4-6"),
        ] {
            let error = Cli::try_parse_from([
                "actual", "rules", "select", "a plan", "--by-adr", flag, value,
            ])
            .expect_err("stage-2 tuning must conflict with --by-adr");
            assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        }
    }

    /// Covers the helper's two non-select fallback arms.
    #[test]
    fn test_rules_select_args_from_other_commands_is_none() {
        assert!(rules_select_args_from(&["actual", "rules", "ls"]).is_none());
        assert!(rules_select_args_from(&["actual", "status"]).is_none());
    }

    /// The hook case: a path and no plan. This is the call `rules select`
    /// refused before, which left `""` as the only way to ask it.
    #[test]
    fn test_rules_select_accepts_a_file_without_a_plan() {
        let args = rules_select_args_from(&[
            "actual",
            "rules",
            "select",
            "--file",
            "src/rules/scope/index.rs",
            "--no-rank",
        ])
        .expect("expected a rules select command");
        assert!(args.plan.is_empty());
        assert_eq!(args.files, vec!["src/rules/scope/index.rs".to_string()]);
        assert!(args.no_rank);
    }

    /// A plan alone still parses: `--file` is what became optional, not the
    /// requirement that a query name something.
    #[test]
    fn test_rules_select_accepts_a_plan_without_a_file() {
        let args = rules_select_args_from(&["actual", "rules", "select", "rotate", "the", "keys"])
            .expect("expected a rules select command");
        assert_eq!(
            args.plan,
            vec!["rotate".to_string(), "the".to_string(), "keys".to_string()]
        );
        assert!(args.files.is_empty());
    }

    /// Neither a plan nor a file names anything to match, so it stays a usage
    /// error rather than selecting against an empty query.
    #[test]
    fn test_rules_select_requires_a_plan_or_a_file() {
        let error = Cli::try_parse_from(["actual", "rules", "select", "--no-rank"])
            .expect_err("expected a usage error");
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
        assert!(error.to_string().contains("PLAN"), "{error}");
    }

    /// `--help` is the interface a hook author reads. The path-only case has
    /// to be on the PLAN and `--file` help, not only in a parse test.
    #[test]
    fn test_rules_select_help_documents_a_file_without_a_plan() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        let rules = cmd.find_subcommand("rules").expect("rules");
        let mut select = rules.find_subcommand("select").expect("select").clone();
        let help = select.render_long_help().to_string();
        assert!(
            help.contains("or a path with no plan"),
            "subcommand about does not mention a path-only query: {help}"
        );
        assert!(
            help.contains("Optional when at least one `--file` is given"),
            "PLAN help does not document the path-only case: {help}"
        );
        assert!(
            help.contains("Without a plan they"),
            "--file help does not document standing alone: {help}"
        );
    }

    #[test]
    fn test_plan_check_parses_the_positional_plan_with_defaults() {
        let args = plan_check_args_from(&["actual", "plan-check", "Add", "caching"])
            .expect("expected a plan-check command");
        assert_eq!(args.plan, vec!["Add".to_string(), "caching".to_string()]);
        assert!(!args.claude_hook);
        assert!(!args.no_rank);
        assert!(!args.json);
        assert!(!args.rebuild);
        assert_eq!(args.limit, 20);
        assert_eq!(args.candidates, crate::rules::scope::DEFAULT_CANDIDATES);
        assert!(args.runner.is_none());
        assert!(args.model.is_none());
        assert!(args.rules_dir.is_none());
    }

    #[test]
    fn test_plan_check_claude_hook_needs_no_positional_plan() {
        let args = plan_check_args_from(&["actual", "plan-check", "--claude-hook"])
            .expect("--claude-hook needs no positional plan");
        assert!(args.claude_hook);
        assert!(args.plan.is_empty());
    }

    #[test]
    fn test_plan_check_rules_dir_and_no_rank_parse() {
        let args = plan_check_args_from(&[
            "actual",
            "plan-check",
            "--no-rank",
            "--rules-dir",
            "/repo/.actual/rules",
            "a plan",
        ])
        .expect("expected a plan-check command");
        assert!(args.no_rank);
        assert_eq!(
            args.rules_dir.as_deref(),
            Some(std::path::Path::new("/repo/.actual/rules"))
        );
    }

    /// `--plan-file` and `--claude-hook` name mutually exclusive plan
    /// sources: the hook always resolves the plan from its own envelope, so
    /// combining them is a usage error rather than a silently ignored flag.
    #[test]
    fn test_plan_check_plan_file_conflicts_with_claude_hook() {
        assert!(Cli::try_parse_from([
            "actual",
            "plan-check",
            "--claude-hook",
            "--plan-file",
            "plan.md",
        ])
        .is_err());
    }

    #[test]
    fn test_plan_check_args_from_other_commands_is_none() {
        assert!(plan_check_args_from(&["actual", "status"]).is_none());
    }

    // ---- ImplCheckArgs / `impl-check` parsing tests ----

    /// Extract `impl-check` arguments from a parsed command.
    fn impl_check_args_from(argv: &[&str]) -> Option<ImplCheckArgs> {
        match Cli::try_parse_from(argv).ok()?.command {
            Command::ImplCheck(args) => Some(args),
            _ => None,
        }
    }

    #[test]
    fn test_impl_check_parses_with_defaults() {
        let args = impl_check_args_from(&["actual", "impl-check"])
            .expect("expected an impl-check command");
        assert!(!args.claude_hook);
        assert!(!args.no_rank);
        assert!(!args.json);
        assert!(!args.rebuild);
        assert_eq!(args.limit, 20);
        assert_eq!(args.candidates, crate::rules::scope::DEFAULT_CANDIDATES);
        assert!(args.runner.is_none());
        assert!(args.model.is_none());
        assert!(args.rules_dir.is_none());
        assert!(args.diff_file.is_none());
        assert_eq!(args.max_rounds, DEFAULT_MAX_ROUNDS);
    }

    #[test]
    fn test_impl_check_claude_hook_parses() {
        let args = impl_check_args_from(&["actual", "impl-check", "--claude-hook"])
            .expect("expected an impl-check command");
        assert!(args.claude_hook);
    }

    #[test]
    fn test_impl_check_diff_file_parses() {
        let args = impl_check_args_from(&["actual", "impl-check", "--diff-file", "the.diff"])
            .expect("expected an impl-check command");
        assert_eq!(
            args.diff_file.as_deref(),
            Some(std::path::Path::new("the.diff"))
        );
    }

    #[test]
    fn test_impl_check_rules_dir_and_no_rank_parse() {
        let args = impl_check_args_from(&[
            "actual",
            "impl-check",
            "--no-rank",
            "--rules-dir",
            "/repo/.actual/rules",
        ])
        .expect("expected an impl-check command");
        assert!(args.no_rank);
        assert_eq!(
            args.rules_dir.as_deref(),
            Some(std::path::Path::new("/repo/.actual/rules"))
        );
    }

    /// `--diff-file` and `--claude-hook` name mutually exclusive diff
    /// sources: the hook always resolves the diff from the working tree, so
    /// combining them is a usage error rather than a silently ignored flag.
    #[test]
    fn test_impl_check_diff_file_conflicts_with_claude_hook() {
        assert!(Cli::try_parse_from([
            "actual",
            "impl-check",
            "--claude-hook",
            "--diff-file",
            "the.diff",
        ])
        .is_err());
    }

    #[test]
    fn test_impl_check_args_from_other_commands_is_none() {
        assert!(impl_check_args_from(&["actual", "status"]).is_none());
    }

    #[test]
    fn test_impl_check_rejects_max_rounds_zero() {
        let result = Cli::try_parse_from(["actual", "impl-check", "--max-rounds", "0"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_impl_check_accepts_valid_max_rounds() {
        let args = impl_check_args_from(&["actual", "impl-check", "--max-rounds", "5"])
            .expect("expected an impl-check command");
        assert_eq!(args.max_rounds, 5);
    }

    /// `impl-check` must be listed at the top level and its own `--help` must
    /// document the exit-code contract, the same way `plan-check`'s does.
    #[test]
    fn test_impl_check_is_listed_and_documents_exit_codes() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        assert!(
            cmd.find_subcommand("impl-check").is_some(),
            "impl-check is not a registered subcommand"
        );
        let mut sub = cmd.find_subcommand("impl-check").unwrap().clone();
        let help = sub.render_long_help().to_string();
        assert!(
            help.contains("Exit codes"),
            "impl-check --help does not document exit codes: {help}"
        );
    }
}
