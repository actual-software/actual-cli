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

/// Which runners to try, in order.
///
/// An explicit `--runner` is the whole list: asking for a backend that is not
/// there should say so, not silently succeed with another one. Otherwise the
/// configured runner leads, then the model's own candidates, and failing both
/// the same default order `adr-bot` uses.
fn candidates(
    explicit: Option<&RunnerChoice>,
    model: Option<&str>,
    cfg: &Config,
) -> Vec<RunnerChoice> {
    use clap::ValueEnum as _;

    if let Some(choice) = explicit {
        return vec![choice.clone()];
    }
    if let Some(configured) = cfg
        .runner
        .as_deref()
        .and_then(|name| RunnerChoice::from_str(name, true).ok())
    {
        return vec![configured];
    }
    if let Some(model) = model {
        return crate::cli::args::runner_candidates(&model.to_ascii_lowercase());
    }
    vec![RunnerChoice::ClaudeCli, RunnerChoice::AnthropicApi]
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

    let mut tried: Vec<String> = Vec::new();
    for choice in candidates(explicit, model.or(cfg.model.as_deref()), cfg) {
        match probe(&choice, cfg).and_then(|()| build(&choice, model, cfg, timeout)) {
            Ok(resolved) => return Ok(resolved),
            Err(reason) => tried.push(reason),
        }
    }
    Err(format!("no runner available: {}", tried.join("; ")))
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
        assert_eq!(got, vec![RunnerChoice::CursorCli]);
    }

    #[test]
    fn test_the_configured_runner_leads_when_no_flag_is_given() {
        let mut cfg = config();
        cfg.runner = Some("openai-api".to_string());
        assert_eq!(
            candidates(None, Some("sonnet"), &cfg),
            vec![RunnerChoice::OpenAiApi]
        );
    }

    #[test]
    fn test_an_unparseable_configured_runner_falls_through_to_the_model() {
        let mut cfg = config();
        cfg.runner = Some("not-a-runner".to_string());
        assert_eq!(
            candidates(None, Some("sonnet"), &cfg),
            vec![RunnerChoice::ClaudeCli, RunnerChoice::AnthropicApi]
        );
    }

    #[test]
    fn test_the_model_picks_the_candidates_when_nothing_else_does() {
        let got = candidates(None, Some("gpt-5.2"), &config());
        assert!(got.contains(&RunnerChoice::OpenAiApi));
        assert!(!got.contains(&RunnerChoice::ClaudeCli));
    }

    #[test]
    fn test_the_default_candidates_with_no_model_at_all() {
        assert_eq!(
            candidates(None, None, &config()),
            vec![RunnerChoice::ClaudeCli, RunnerChoice::AnthropicApi]
        );
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
