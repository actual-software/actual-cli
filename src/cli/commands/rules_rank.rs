//! Resolving a runner for stage-2 rule selection.
//!
//! # Design
//!
//! `sync_wiring` builds a runner and monomorphizes the whole pipeline behind
//! it, one branch per backend, because tailoring either runs or fails. Stage 2
//! cannot work that way: an unavailable runner is a normal outcome there, and
//! the caller has to carry on with the stage-1 answer. So the five runners are
//! wrapped in one enum and the choice becomes a value rather than a branch.
//!
//! The enum exists because [`StructuredRunner`] returns an `impl Future` and is
//! therefore not object-safe — `Box<dyn StructuredRunner>` will not compile.
//! Dispatching by hand across five variants is the cost of keeping the trait's
//! futures unboxed on the tailoring path, which is the hot one.
//!
//! Nothing here returns an error for a missing runner. [`resolve`] hands back
//! the human-readable reason instead, and the caller records it as the reason
//! stage 2 did not run.

use std::time::Duration;

use crate::cli::args::RunnerChoice;
use crate::config::types::{Config, DEFAULT_MODEL, DEFAULT_OPENAI_MODEL, DEFAULT_TIMEOUT_SECS};
use crate::error::ActualError;
use crate::runner::anthropic_api::{AnthropicApiRunner, DEFAULT_MAX_TOKENS};
use crate::runner::binary::find_claude_binary;
use crate::runner::codex_cli::{find_codex_binary, CodexCliRunner};
use crate::runner::cursor_cli::{find_cursor_binary, CursorCliRunner};
use crate::runner::openai_api::OpenAiApiRunner;
use crate::runner::probe::{
    is_anthropic_available, is_claude_available, is_codex_available, is_cursor_available,
    is_openai_available,
};
use crate::runner::structured::StructuredRunner;
use crate::runner::subprocess::CliClaudeRunner;

/// How long stage 2 waits for a verdict.
///
/// Far below the tailoring timeout, which is measured in minutes. Selection is
/// meant to sit inside an interactive turn, and a ranker still thinking after a
/// minute has already cost more than the precision it was going to buy — the
/// caller degrades to the prefilter instead.
pub const RANK_TIMEOUT_SECS: u64 = 60;

/// One of the five backends, chosen at run time.
pub enum SelectionRunner {
    ClaudeCli(CliClaudeRunner),
    AnthropicApi(AnthropicApiRunner),
    OpenAiApi(OpenAiApiRunner),
    CodexCli(CodexCliRunner),
    CursorCli(CursorCliRunner),
}

impl StructuredRunner for SelectionRunner {
    async fn run_structured_json(
        &self,
        prompt: &str,
        schema: &str,
        model_override: Option<&str>,
        max_budget_usd: Option<f64>,
    ) -> Result<serde_json::Value, ActualError> {
        match self {
            SelectionRunner::ClaudeCli(r) => {
                r.run_structured_json(prompt, schema, model_override, max_budget_usd)
                    .await
            }
            SelectionRunner::AnthropicApi(r) => {
                r.run_structured_json(prompt, schema, model_override, max_budget_usd)
                    .await
            }
            SelectionRunner::OpenAiApi(r) => {
                r.run_structured_json(prompt, schema, model_override, max_budget_usd)
                    .await
            }
            SelectionRunner::CodexCli(r) => {
                r.run_structured_json(prompt, schema, model_override, max_budget_usd)
                    .await
            }
            SelectionRunner::CursorCli(r) => {
                r.run_structured_json(prompt, schema, model_override, max_budget_usd)
                    .await
            }
        }
    }
}

impl std::fmt::Debug for SelectionRunner {
    /// Names the backend and nothing else: every variant holds a resolved API
    /// key, and a derived `Debug` would put it in any log line that formats a
    /// resolution failure.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            SelectionRunner::ClaudeCli(_) => "claude-cli",
            SelectionRunner::AnthropicApi(_) => "anthropic-api",
            SelectionRunner::OpenAiApi(_) => "openai-api",
            SelectionRunner::CodexCli(_) => "codex-cli",
            SelectionRunner::CursorCli(_) => "cursor-cli",
        };
        write!(f, "SelectionRunner({name})")
    }
}

/// A resolved runner, and what to call it in the output.
#[derive(Debug)]
pub struct ResolvedRunner {
    pub runner: SelectionRunner,
    pub choice: RunnerChoice,
    /// The model the runner will use, for display. `None` when the backend
    /// picks its own default.
    pub model: Option<String>,
    /// The spending cap for one rank, inherited from `max_budget_usd` in the
    /// config so a selection cannot outspend what tailoring is allowed.
    ///
    /// **Only the Claude CLI enforces this.** The other four backends take the
    /// value and drop it, because their APIs offer no per-request cap. It is
    /// therefore a real bound on one runner and a no-op on the rest, which is
    /// worth knowing before treating it as a spend control.
    pub max_budget_usd: Option<f64>,
}

impl ResolvedRunner {
    /// `claude-cli (sonnet)`, or `codex-cli` when the backend chooses.
    pub fn label(&self) -> String {
        match &self.model {
            Some(model) => format!("{} ({model})", self.choice.display_name()),
            None => self.choice.display_name().to_string(),
        }
    }
}

/// Where a candidate list came from, so a failure can say why nothing else was
/// tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// `--runner` named a backend for this run.
    Flag,
    /// `runner:` in the config pins one. Invisible at the call site, which is
    /// why a failure has to name it.
    Config,
    /// Derived from a model name: an ordered list, not a pin.
    Model,
    /// Nothing was expressed, so the default order.
    Default,
}

/// Which runners to try, in order.
///
/// Fallback happens where the caller expressed a preference loosely, and not
/// where they expressed it precisely. A model name is loose, so it yields an
/// ordered list of backends that can serve it. `--runner` and the config's
/// `runner:` name a backend outright, so each is the whole list: asking for a
/// backend that is not there should say so rather than silently succeeding with
/// another one, which would also mean stage 2 ran on a different model than the
/// caller asked for.
///
/// Treating `runner:` as a pin is what `actual adr-bot` already does — it uses
/// the configured runner as-is and only auto-detects when neither the flag nor
/// the config field is set. One config key has to mean one thing in both
/// commands; a `rules select` that quietly fell back would disagree with an
/// `adr-bot` that failed, from the same line of YAML on the same machine.
fn candidates(
    explicit: Option<&RunnerChoice>,
    model: Option<&str>,
    cfg: &Config,
) -> (Vec<RunnerChoice>, Origin) {
    use clap::ValueEnum as _;

    if let Some(choice) = explicit {
        return (vec![choice.clone()], Origin::Flag);
    }
    if let Some(configured) = cfg
        .runner
        .as_deref()
        .and_then(|name| RunnerChoice::from_str(name, true).ok())
    {
        return (vec![configured], Origin::Config);
    }
    if let Some(model) = model {
        return (
            crate::cli::args::runner_candidates(&model.to_ascii_lowercase()),
            Origin::Model,
        );
    }
    (
        vec![RunnerChoice::ClaudeCli, RunnerChoice::AnthropicApi],
        Origin::Default,
    )
}

/// Is this backend usable right now?
fn probe(choice: &RunnerChoice, cfg: &Config) -> Result<(), String> {
    match choice {
        RunnerChoice::ClaudeCli => is_claude_available(),
        RunnerChoice::AnthropicApi => is_anthropic_available(cfg.anthropic_api_key.as_deref()),
        RunnerChoice::OpenAiApi => is_openai_available(cfg.openai_api_key.as_deref()),
        RunnerChoice::CodexCli => is_codex_available(cfg.openai_api_key.as_deref()),
        RunnerChoice::CursorCli => is_cursor_available(cfg.cursor_api_key.as_deref()),
    }
}

/// Build the runner for `choice`, assuming it has already probed clean.
fn build(
    choice: &RunnerChoice,
    model: Option<&str>,
    cfg: &Config,
    timeout: Duration,
) -> Result<ResolvedRunner, String> {
    let configured = model.or(cfg.model.as_deref());
    let make = |runner, model: Option<String>| ResolvedRunner {
        runner,
        choice: choice.clone(),
        model,
        max_budget_usd: cfg.max_budget_usd,
    };

    match choice {
        RunnerChoice::ClaudeCli => {
            let binary = find_claude_binary().map_err(|e| e.to_string())?;
            let model = configured.unwrap_or(DEFAULT_MODEL).to_string();
            Ok(make(
                SelectionRunner::ClaudeCli(CliClaudeRunner::new(binary, timeout)),
                Some(model),
            ))
        }
        RunnerChoice::AnthropicApi => {
            let key = api_key("ANTHROPIC_API_KEY", cfg.anthropic_api_key.as_deref())?;
            let model = configured.unwrap_or(DEFAULT_MODEL).to_string();
            let runner = AnthropicApiRunner::with_max_tokens(
                key,
                model.clone(),
                timeout,
                cfg.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            )
            .map_err(|e| e.to_string())?;
            Ok(make(SelectionRunner::AnthropicApi(runner), Some(model)))
        }
        RunnerChoice::OpenAiApi => {
            let key = api_key("OPENAI_API_KEY", cfg.openai_api_key.as_deref())?;
            let model = configured.unwrap_or(DEFAULT_OPENAI_MODEL).to_string();
            let runner =
                OpenAiApiRunner::new(key, model.clone(), timeout).map_err(|e| e.to_string())?;
            Ok(make(SelectionRunner::OpenAiApi(runner), Some(model)))
        }
        RunnerChoice::CodexCli => {
            let binary = find_codex_binary().map_err(|e| e.to_string())?;
            let model = configured.map(str::to_string);
            let mut runner = CodexCliRunner::new(binary, model.clone(), timeout);
            if let Some(key) = std::env::var("OPENAI_API_KEY")
                .ok()
                .or_else(|| cfg.openai_api_key.clone())
            {
                runner = runner.with_api_key(key);
            }
            Ok(make(SelectionRunner::CodexCli(runner), model))
        }
        RunnerChoice::CursorCli => {
            let binary = find_cursor_binary().map_err(|e| e.to_string())?;
            let model = configured.map(str::to_string);
            let mut runner = CursorCliRunner::new(binary, model.clone(), timeout);
            if let Some(key) = std::env::var("CURSOR_API_KEY")
                .ok()
                .or_else(|| cfg.cursor_api_key.clone())
            {
                runner = runner.with_api_key(key);
            }
            Ok(make(SelectionRunner::CursorCli(runner), model))
        }
    }
}

/// The environment variable, then the config fallback.
fn api_key(env_var: &str, config_key: Option<&str>) -> Result<String, String> {
    std::env::var(env_var)
        .ok()
        .filter(|k| !k.is_empty())
        .or_else(|| config_key.map(str::to_string).filter(|k| !k.is_empty()))
        .ok_or_else(|| format!("{env_var} is not set"))
}

/// Find a usable runner for stage 2, or say why there is none.
///
/// The `Err` is a sentence meant for a panel, not an error to propagate: no
/// runner is an expected state, and the caller answers from stage 1 alone.
pub fn resolve(
    explicit: Option<&RunnerChoice>,
    model: Option<&str>,
    cfg: &Config,
) -> Result<ResolvedRunner, String> {
    let timeout = Duration::from_secs(
        cfg.invocation_timeout_secs
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .min(RANK_TIMEOUT_SECS),
    );

    let (choices, origin) = candidates(explicit, model.or(cfg.model.as_deref()), cfg);
    let mut tried: Vec<String> = Vec::new();
    for choice in choices {
        match probe(&choice, cfg).and_then(|()| build(&choice, model, cfg, timeout)) {
            Ok(resolved) => return Ok(resolved),
            Err(reason) => tried.push(reason),
        }
    }
    Err(unavailable(origin, &tried, cfg))
}

/// What a config-pinned runner adds to an unavailability message.
///
/// Assembled with `concat!` rather than a `\`-continued literal: rustfmt
/// rewrites those into one line and turns the continuation indent into runs of
/// literal spaces, which then show up verbatim in the panel.
const PIN_HINT: &str = concat!(
    " in the config pins stage 2 to that backend, so no other was tried",
    " — pass --runner to override it for this run, or unset it to let the model choose."
);

/// Why stage 2 has no runner, in a sentence a reader can act on.
///
/// A config-pinned runner gets its own wording. The pin is the reason no second
/// backend was probed, and unlike `--runner` it is nowhere in the command the
/// caller just typed — without naming it, a machine that is missing one binary
/// but holds a perfectly good API key looks like a machine with no runner at
/// all.
fn unavailable(origin: Origin, tried: &[String], cfg: &Config) -> String {
    let tried = tried.join("; ");
    match origin {
        Origin::Config => {
            let name = cfg.runner.as_deref().unwrap_or("?");
            format!("no runner available: {tried}. `runner: {name}`{PIN_HINT}")
        }
        _ => format!("no runner available: {tried}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testutil::{EnvGuard, ENV_MUTEX};

    fn config() -> Config {
        Config::default()
    }

    #[test]
    fn test_an_explicit_runner_is_the_whole_candidate_list() {
        // Asking for a backend that is not installed must report that, rather
        // than quietly succeeding with a different one.
        let got = candidates(Some(&RunnerChoice::CursorCli), Some("sonnet"), &config());
        assert_eq!(got, (vec![RunnerChoice::CursorCli], Origin::Flag));
    }

    /// `runner:` in the config is a pin, not the head of a fallback list —
    /// the same meaning `actual adr-bot` gives it. A model that would have
    /// suggested other backends does not widen it.
    #[test]
    fn test_the_configured_runner_is_a_pin_not_a_fallback_list() {
        let mut cfg = config();
        cfg.runner = Some("openai-api".to_string());
        assert_eq!(
            candidates(None, Some("sonnet"), &cfg),
            (vec![RunnerChoice::OpenAiApi], Origin::Config)
        );
    }

    #[test]
    fn test_an_unparseable_configured_runner_falls_through_to_the_model() {
        let mut cfg = config();
        cfg.runner = Some("not-a-runner".to_string());
        assert_eq!(
            candidates(None, Some("sonnet"), &cfg),
            (
                vec![RunnerChoice::ClaudeCli, RunnerChoice::AnthropicApi],
                Origin::Model
            )
        );
    }

    /// A model is a loose preference, so it yields a list rather than a pin.
    #[test]
    fn test_the_model_picks_the_candidates_when_nothing_else_does() {
        let (got, origin) = candidates(None, Some("gpt-5.2"), &config());
        assert!(got.len() > 1, "a model should widen, not pin: {got:?}");
        assert!(got.contains(&RunnerChoice::OpenAiApi));
        assert!(!got.contains(&RunnerChoice::ClaudeCli));
        assert_eq!(origin, Origin::Model);
    }

    #[test]
    fn test_the_default_candidates_with_no_model_at_all() {
        assert_eq!(
            candidates(None, None, &config()),
            (
                vec![RunnerChoice::ClaudeCli, RunnerChoice::AnthropicApi],
                Origin::Default
            )
        );
    }

    /// A config pin is nowhere in the command the caller typed, so the failure
    /// has to name it. Without this, a machine missing one binary but holding a
    /// usable API key reads as a machine with no runner at all.
    #[test]
    fn test_a_config_pin_explains_itself_when_nothing_resolves() {
        let mut cfg = config();
        cfg.runner = Some("claude-cli".to_string());
        let message = unavailable(
            Origin::Config,
            &["claude-cli: binary not found".to_string()],
            &cfg,
        );
        assert!(message.contains("claude-cli: binary not found"));
        assert!(message.contains("`runner: claude-cli` in the config pins stage 2"));
        assert!(message.contains("--runner"));
        // The message is wrapped into a panel, so a run of literal spaces from
        // a line-continued source literal would show up verbatim on screen.
        assert!(
            !message.contains("  "),
            "message has a double space: {message}"
        );
    }

    /// Every other origin says only what it tried: an explicit `--runner` is in
    /// the caller's own command line, and a model-derived list has no pin to
    /// blame.
    #[test]
    fn test_other_origins_report_only_what_was_tried() {
        let tried = ["claude-cli: binary not found".to_string()];
        for origin in [Origin::Flag, Origin::Model, Origin::Default] {
            let message = unavailable(origin, &tried, &config());
            assert_eq!(message, "no runner available: claude-cli: binary not found");
        }
    }

    /// The pin wording survives a config whose `runner:` cannot be parsed —
    /// that combination never reaches `Origin::Config`, but the formatter must
    /// not panic if it ever does.
    #[test]
    fn test_the_pin_message_tolerates_a_missing_config_value() {
        let message = unavailable(Origin::Config, &["nothing".to_string()], &config());
        assert!(message.contains("`runner: ?`"));
    }

    #[test]
    fn test_api_key_prefers_the_environment_then_the_config() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _set = EnvGuard::set("ACTUAL_TEST_KEY", "from-env");
        assert_eq!(
            api_key("ACTUAL_TEST_KEY", Some("from-config")).unwrap(),
            "from-env"
        );

        let _cleared = EnvGuard::remove("ACTUAL_TEST_KEY");
        assert_eq!(
            api_key("ACTUAL_TEST_KEY", Some("from-config")).unwrap(),
            "from-config"
        );
        assert!(api_key("ACTUAL_TEST_KEY", None)
            .unwrap_err()
            .contains("ACTUAL_TEST_KEY is not set"));
        // An empty value is not a key.
        assert!(api_key("ACTUAL_TEST_KEY", Some("")).is_err());
    }

    #[test]
    fn test_build_reports_a_missing_api_key_rather_than_panicking() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _cleared = EnvGuard::remove("ANTHROPIC_API_KEY");
        let err = build(
            &RunnerChoice::AnthropicApi,
            None,
            &config(),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(err.contains("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn test_build_uses_the_requested_model_over_the_configured_one() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _key = EnvGuard::set("ANTHROPIC_API_KEY", "test-key");
        let mut cfg = config();
        cfg.model = Some("claude-from-config".to_string());
        let resolved = build(
            &RunnerChoice::AnthropicApi,
            Some("claude-from-flag"),
            &cfg,
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(resolved.model.as_deref(), Some("claude-from-flag"));
        assert_eq!(resolved.label(), "anthropic-api (claude-from-flag)");
    }

    #[test]
    fn test_build_falls_back_to_the_configured_model() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _key = EnvGuard::set("OPENAI_API_KEY", "test-key");
        let mut cfg = config();
        cfg.model = Some("gpt-from-config".to_string());
        let resolved = build(&RunnerChoice::OpenAiApi, None, &cfg, Duration::from_secs(1)).unwrap();
        assert_eq!(resolved.model.as_deref(), Some("gpt-from-config"));
    }

    /// A backend that picks its own model has no model to display, and the
    /// label must not invent one.
    #[test]
    fn test_label_omits_a_model_the_backend_chooses_itself() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _key = EnvGuard::set("ANTHROPIC_API_KEY", "test-key");
        let resolved = build(
            &RunnerChoice::AnthropicApi,
            None,
            &config(),
            Duration::from_secs(1),
        )
        .unwrap();
        let unlabelled = ResolvedRunner {
            runner: resolved.runner,
            choice: RunnerChoice::CodexCli,
            model: None,
            max_budget_usd: None,
        };
        assert_eq!(unlabelled.label(), "codex-cli");
    }

    /// The selection timeout is capped well under the tailoring one: a ranker
    /// still thinking after a minute has already cost more than it can buy.
    /// A resolution failure is logged, and a derived `Debug` would put the
    /// resolved API key in that log line.
    #[test]
    fn test_debug_names_the_backend_without_the_api_key() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _key = EnvGuard::set("ANTHROPIC_API_KEY", "sk-secret-value");
        let resolved = build(
            &RunnerChoice::AnthropicApi,
            None,
            &config(),
            Duration::from_secs(1),
        )
        .unwrap();
        let rendered = format!("{resolved:?}");
        assert!(rendered.contains("anthropic-api"));
        assert!(!rendered.contains("sk-secret-value"));
    }

    /// A rank inherits the configured spending cap rather than running
    /// uncapped, so a selection cannot outspend what tailoring is allowed.
    #[test]
    fn test_a_resolved_runner_inherits_the_configured_budget() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _key = EnvGuard::set("ANTHROPIC_API_KEY", "test-key");
        let mut cfg = config();
        cfg.max_budget_usd = Some(0.25);
        let resolved = build(
            &RunnerChoice::AnthropicApi,
            None,
            &cfg,
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(resolved.max_budget_usd, Some(0.25));
    }

    #[test]
    fn test_an_unset_budget_stays_unset() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _key = EnvGuard::set("ANTHROPIC_API_KEY", "test-key");
        let resolved = build(
            &RunnerChoice::AnthropicApi,
            None,
            &config(),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(resolved.max_budget_usd, None);
    }

    #[test]
    fn test_the_rank_timeout_is_far_below_the_tailoring_timeout() {
        const { assert!(RANK_TIMEOUT_SECS < DEFAULT_TIMEOUT_SECS) };
    }

    #[test]
    fn test_resolve_reports_every_backend_it_tried() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _anthropic = EnvGuard::remove("ANTHROPIC_API_KEY");
        let err = resolve(Some(&RunnerChoice::AnthropicApi), None, &config()).unwrap_err();
        assert!(err.starts_with("no runner available:"));
        assert!(err.contains("ANTHROPIC_API_KEY"));
    }
}
