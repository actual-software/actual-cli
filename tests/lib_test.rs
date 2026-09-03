mod common;

use actual_cli::{handle_result, run, Cli, Command, ConfigAction, RunnerChoice};
use clap::Parser;
use common::{EnvGuard, ENV_MUTEX};

#[test]
fn test_cli_parse_sync() {
    let cli = Cli::parse_from(["actual", "adr-bot"]);
    assert!(matches!(cli.command, Command::AdrBot(_)));
}

#[test]
fn test_cli_parse_sync_with_flags() {
    let cli = Cli::parse_from([
        "actual",
        "adr-bot",
        "--dry-run",
        "--full",
        "--force",
        "--reset-rejections",
        "--verbose",
        "--no-tailor",
        "--model",
        "sonnet",
        "--api-url",
        "https://example.com",
        "--project",
        "apps/web",
        "--project",
        "apps/api",
        "--max-budget-usd",
        "1.50",
    ]);
    let Command::AdrBot(args) = cli.command else {
        unreachable!()
    };
    assert!(args.dry_run);
    assert!(args.full);
    assert!(args.force);
    assert!(args.reset_rejections);
    assert!(args.verbose);
    assert!(args.no_tailor);
    assert_eq!(args.model.as_deref(), Some("sonnet"));
    assert_eq!(args.api_url.as_deref(), Some("https://example.com"));
    assert_eq!(args.projects, vec!["apps/web", "apps/api"]);
    assert_eq!(args.max_budget_usd, Some(1.50));
}

#[test]
fn test_cli_parse_sync_defaults() {
    let cli = Cli::parse_from(["actual", "adr-bot"]);
    let Command::AdrBot(args) = cli.command else {
        unreachable!()
    };
    assert!(!args.dry_run);
    assert!(!args.full);
    assert!(!args.force);
    assert!(!args.reset_rejections);
    assert!(!args.verbose);
    assert!(!args.no_tailor);
    assert!(args.model.is_none());
    assert!(args.api_url.is_none());
    assert!(args.projects.is_empty());
    assert!(args.max_budget_usd.is_none());
}

#[test]
fn test_cli_parse_sync_full_requires_dry_run() {
    let result = Cli::try_parse_from(["actual", "adr-bot", "--full"]);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("--dry-run"),
        "error should mention --dry-run: {err}"
    );
}

#[test]
fn test_cli_parse_sync_dry_run_full_accepted() {
    let cli = Cli::parse_from(["actual", "adr-bot", "--dry-run", "--full"]);
    let Command::AdrBot(args) = cli.command else {
        unreachable!()
    };
    assert!(args.dry_run);
    assert!(args.full);
}

#[test]
fn test_cli_parse_sync_multiple_projects() {
    let cli = Cli::parse_from([
        "actual",
        "adr-bot",
        "--project",
        "apps/web",
        "--project",
        "apps/api",
    ]);
    let Command::AdrBot(args) = cli.command else {
        unreachable!()
    };
    assert_eq!(args.projects, vec!["apps/web", "apps/api"]);
}

#[test]
fn test_cli_parse_sync_model_and_budget() {
    let cli = Cli::parse_from([
        "actual",
        "adr-bot",
        "--model",
        "opus",
        "--max-budget-usd",
        "0.50",
    ]);
    let Command::AdrBot(args) = cli.command else {
        unreachable!()
    };
    assert_eq!(args.model.as_deref(), Some("opus"));
    assert_eq!(args.max_budget_usd, Some(0.50));
}

#[test]
fn test_cli_parse_status() {
    let cli = Cli::parse_from(["actual", "status"]);
    assert!(matches!(cli.command, Command::Status(_)));
}

#[test]
fn test_cli_parse_status_verbose() {
    let cli = Cli::parse_from(["actual", "status", "--verbose"]);
    let Command::Status(args) = cli.command else {
        unreachable!()
    };
    assert!(args.verbose);
}

#[test]
fn test_cli_parse_status_defaults() {
    let cli = Cli::parse_from(["actual", "status"]);
    let Command::Status(args) = cli.command else {
        unreachable!()
    };
    assert!(!args.verbose);
}

#[test]
fn test_cli_parse_auth() {
    let cli = Cli::parse_from(["actual", "auth"]);
    assert!(matches!(cli.command, Command::Auth(_)));
}

#[test]
fn test_cli_parse_auth_create_token() {
    use actual_cli::AuthCommand;
    let cli = Cli::parse_from([
        "actual",
        "auth",
        "create-token",
        "--name",
        "ci-agent",
        "--scopes",
        "adr:query,adr:review",
    ]);
    let Command::Auth(auth) = cli.command else {
        unreachable!()
    };
    let Some(AuthCommand::CreateToken(args)) = auth.command else {
        unreachable!()
    };
    assert_eq!(args.name, "ci-agent");
    assert_eq!(args.scopes, vec!["adr:query", "adr:review"]);
}

#[test]
fn test_cli_parse_config_show() {
    let cli = Cli::parse_from(["actual", "config", "show"]);
    let Command::Config(config) = cli.command else {
        unreachable!()
    };
    assert!(matches!(config.action, ConfigAction::Show));
}

#[test]
fn test_cli_parse_config_set() {
    let cli = Cli::parse_from(["actual", "config", "set", "options.batch_size", "15"]);
    let Command::Config(config) = cli.command else {
        unreachable!()
    };
    let ConfigAction::Set(args) = config.action else {
        unreachable!()
    };
    assert_eq!(args.key, "options.batch_size");
    assert_eq!(args.value, "15");
}

#[test]
fn test_cli_parse_config_path() {
    let cli = Cli::parse_from(["actual", "config", "path"]);
    let Command::Config(config) = cli.command else {
        unreachable!()
    };
    assert!(matches!(config.action, ConfigAction::Path));
}

#[test]
fn test_run_sync_without_claude() {
    // Sync now requires Claude binary for Phase 1 env check.
    // Without it, we expect exit code 2 (ClaudeNotFound).
    // Explicitly set --runner to prevent the user's config from auto-selecting
    // a different runner (e.g. codex-cli) which would bypass find_claude_binary().
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let _guard = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/path/to/claude", &_lock);
    let cli = Cli::parse_from(["actual", "adr-bot", "--dry-run", "--runner", "claude-cli"]);
    assert_eq!(handle_result(run(cli)), 2);
}

#[test]
fn test_run_status() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");
    let _guard = EnvGuard::set("ACTUAL_CONFIG", config_file.to_str().unwrap(), &_lock);
    let cli = Cli::parse_from(["actual", "status"]);
    assert_eq!(handle_result(run(cli)), 0);
}

#[test]
fn test_run_auth() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let _guard = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/path/to/claude", &_lock);
    let cli = Cli::parse_from(["actual", "auth"]);
    assert_eq!(handle_result(run(cli)), 2);
}

#[test]
fn test_run_config_show() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");
    let _guard = EnvGuard::set("ACTUAL_CONFIG", config_file.to_str().unwrap(), &_lock);
    let cli = Cli::parse_from(["actual", "config", "show"]);
    assert_eq!(handle_result(run(cli)), 0);
}

#[test]
fn test_run_config_set() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");
    let _guard = EnvGuard::set("ACTUAL_CONFIG", config_file.to_str().unwrap(), &_lock);
    let cli = Cli::parse_from(["actual", "config", "set", "batch_size", "10"]);
    assert_eq!(handle_result(run(cli)), 0);
}

#[test]
fn test_run_config_path() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");
    let _guard = EnvGuard::set("ACTUAL_CONFIG", config_file.to_str().unwrap(), &_lock);
    let cli = Cli::parse_from(["actual", "config", "path"]);
    assert_eq!(handle_result(run(cli)), 0);
}

#[test]
fn test_cli_parse_sync_negative_budget_rejected() {
    let result = Cli::try_parse_from(["actual", "adr-bot", "--max-budget-usd", "-5"]);
    assert!(result.is_err(), "negative budget should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("non-negative"),
        "error should mention non-negative: {err}"
    );
}

#[test]
fn test_cli_parse_sync_zero_budget_accepted() {
    let cli = Cli::parse_from(["actual", "adr-bot", "--max-budget-usd", "0"]);
    let Command::AdrBot(args) = cli.command else {
        unreachable!()
    };
    assert_eq!(args.max_budget_usd, Some(0.0));
}

#[test]
fn test_cli_parse_sync_non_numeric_budget_rejected() {
    let result = Cli::try_parse_from(["actual", "adr-bot", "--max-budget-usd", "abc"]);
    assert!(result.is_err(), "non-numeric budget should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not a valid number"),
        "error should mention invalid number: {err}"
    );
}

#[test]
fn test_cli_parse_sync_nan_budget_rejected() {
    let result = Cli::try_parse_from(["actual", "adr-bot", "--max-budget-usd", "nan"]);
    assert!(result.is_err(), "NaN budget should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("non-negative finite"),
        "error should mention non-negative finite: {err}"
    );
}

#[test]
fn test_cli_parse_sync_inf_budget_rejected() {
    let result = Cli::try_parse_from(["actual", "adr-bot", "--max-budget-usd", "inf"]);
    assert!(result.is_err(), "inf budget should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("non-negative finite"),
        "error should mention non-negative finite: {err}"
    );
}

#[test]
fn test_cli_parse_sync_runner_claude_cli_accepted() {
    let cli = Cli::parse_from(["actual", "adr-bot", "--runner", "claude-cli"]);
    let Command::AdrBot(args) = cli.command else {
        unreachable!()
    };
    assert_eq!(args.runner, Some(RunnerChoice::ClaudeCli));
}

#[test]
fn test_cli_parse_sync_runner_anthropic_api_accepted() {
    let cli = Cli::parse_from(["actual", "adr-bot", "--runner", "anthropic-api"]);
    let Command::AdrBot(args) = cli.command else {
        unreachable!()
    };
    assert_eq!(args.runner, Some(RunnerChoice::AnthropicApi));
}

#[test]
fn test_cli_parse_sync_runner_openai_api_accepted() {
    let cli = Cli::parse_from(["actual", "adr-bot", "--runner", "openai-api"]);
    let Command::AdrBot(args) = cli.command else {
        unreachable!()
    };
    assert_eq!(args.runner, Some(RunnerChoice::OpenAiApi));
}

#[test]
fn test_cli_parse_sync_runner_codex_cli_accepted() {
    let cli = Cli::parse_from(["actual", "adr-bot", "--runner", "codex-cli"]);
    let Command::AdrBot(args) = cli.command else {
        unreachable!()
    };
    assert_eq!(args.runner, Some(RunnerChoice::CodexCli));
}

#[test]
fn test_cli_parse_sync_runner_cursor_cli_accepted() {
    let cli = Cli::parse_from(["actual", "adr-bot", "--runner", "cursor-cli"]);
    let Command::AdrBot(args) = cli.command else {
        unreachable!()
    };
    assert_eq!(args.runner, Some(RunnerChoice::CursorCli));
}

#[test]
fn test_cli_parse_sync_runner_invalid_value_rejected() {
    let result = Cli::try_parse_from(["actual", "adr-bot", "--runner", "unknown-runner"]);
    assert!(result.is_err(), "invalid runner value should be rejected");
}

#[test]
fn test_cli_parse_sync_runner_log_injection_rejected() {
    let result = Cli::try_parse_from(["actual", "adr-bot", "--runner", "invalid\nlog-injection"]);
    assert!(
        result.is_err(),
        "runner value with newline should be rejected"
    );
}

#[test]
fn test_cli_parse_sync_runner_empty_value_rejected() {
    let result = Cli::try_parse_from(["actual", "adr-bot", "--runner", ""]);
    assert!(result.is_err(), "empty runner value should be rejected");
}

#[test]
fn test_cli_parse_sync_no_runner_flag() {
    let cli = Cli::parse_from(["actual", "adr-bot"]);
    let Command::AdrBot(args) = cli.command else {
        unreachable!()
    };
    assert_eq!(args.runner, None);
}

#[cfg(unix)]
#[test]
fn test_run_sync_force_with_fake_claude() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");

    // Start a mock API server returning an empty match response
    let mut server = mockito::Server::new();
    server
        .mock("POST", "/adrs/match")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{
            "matched_adrs": [],
            "metadata": {"total_matched": 0, "by_framework": {}, "deduplicated_count": 0}
        }"#,
        )
        .create();

    let script = common::create_fake_claude_binary(
        dir.path(),
        common::AUTH_OK,
        common::ANALYSIS_SINGLE_PROJECT,
    );

    let _guard_binary = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap(), &_lock);
    let _guard_config = EnvGuard::set("ACTUAL_CONFIG", config_file.to_str().unwrap(), &_lock);

    let cli = Cli::parse_from([
        "actual",
        "adr-bot",
        "--force",
        "--runner",
        "claude-cli",
        "--api-url",
        &server.url(),
    ]);
    let exit_code = handle_result(run(cli));

    assert_eq!(exit_code, 0, "sync --force with fake Claude should succeed");
}

#[cfg(unix)]
#[test]
fn test_run_sync_not_authenticated_with_fake_claude() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();

    let script = common::create_fake_claude_binary(dir.path(), common::AUTH_FAIL, "{}");

    let _guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap(), &_lock);

    // Explicitly set --runner to prevent the user's config from auto-selecting
    // a different runner (e.g. codex-cli) which would bypass find_claude_binary().
    let cli = Cli::parse_from(["actual", "adr-bot", "--force", "--runner", "claude-cli"]);
    let exit_code = handle_result(run(cli));

    assert_eq!(
        exit_code, 2,
        "sync with unauthenticated Claude should exit with code 2"
    );
}

#[test]
fn test_cli_parse_models() {
    let cli = Cli::parse_from(["actual", "models"]);
    assert!(matches!(cli.command, Command::Models(_)));
}

#[test]
fn test_run_models() {
    let cli = Cli::parse_from(["actual", "models"]);
    assert_eq!(handle_result(run(cli)), 0);
}

#[test]
fn test_cli_parse_rules_ls() {
    let cli = Cli::parse_from(["actual", "rules", "ls"]);
    assert!(matches!(cli.command, Command::Rules(_)));
}

#[test]
fn test_cli_parse_rules_ls_with_path_and_json() {
    let cli = Cli::parse_from(["actual", "rules", "ls", "/some/repo", "--json"]);
    let Command::Rules(args) = cli.command else {
        unreachable!()
    };
    let actual_cli::RulesAction::Ls(ls) = args.action else {
        unreachable!("`rules ls` must parse as the Ls action")
    };
    assert_eq!(ls.path.as_deref(), Some(std::path::Path::new("/some/repo")));
    assert!(ls.json);
}

#[test]
fn test_cli_parse_rules_index() {
    let cli = Cli::parse_from(["actual", "rules", "index", "/some/repo", "--rebuild"]);
    let Command::Rules(args) = cli.command else {
        unreachable!()
    };
    let actual_cli::RulesAction::Index(index) = args.action else {
        unreachable!("`rules index` must parse as the Index action")
    };
    assert_eq!(
        index.path.as_deref(),
        Some(std::path::Path::new("/some/repo"))
    );
    assert!(index.rebuild);
    assert!(!index.json);
}

/// The plan is the positional argument for `rules select`, and it may run to
/// several words without quoting.
#[test]
fn test_cli_parse_rules_select() {
    let cli = Cli::parse_from([
        "actual",
        "rules",
        "select",
        "rotate",
        "the",
        "signing",
        "key",
        "--repo",
        "/some/repo",
        "--file",
        "lib/auth.ts",
        "--limit",
        "3",
        "--explain",
    ]);
    let Command::Rules(args) = cli.command else {
        unreachable!()
    };
    let actual_cli::RulesAction::Select(select) = args.action else {
        unreachable!("`rules select` must parse as the Select action")
    };
    assert_eq!(select.plan.join(" "), "rotate the signing key");
    assert_eq!(
        select.repo.as_deref(),
        Some(std::path::Path::new("/some/repo"))
    );
    assert_eq!(select.files, vec!["lib/auth.ts"]);
    assert_eq!(select.limit, 3);
    assert!(select.explain);
    // Stage 2 is on by default: the default answer should be the best one the
    // environment can give, not the cheapest.
    assert!(!select.no_rank);
    assert_eq!(
        select.candidates,
        actual_cli::rules::scope::DEFAULT_CANDIDATES
    );
    assert!(select.runner.is_none());
    assert!(select.model.is_none());
}

/// The stage-2 switches on `rules select`: skip it, size its candidate set,
/// and name the backend and model it uses.
#[test]
fn test_cli_parse_rules_select_stage_two_flags() {
    let cli = Cli::parse_from([
        "actual",
        "rules",
        "select",
        "add an endpoint",
        "--candidates",
        "12",
        "--runner",
        "anthropic-api",
        "--model",
        "claude-sonnet-4-6",
    ]);
    let Command::Rules(args) = cli.command else {
        unreachable!()
    };
    let actual_cli::RulesAction::Select(select) = args.action else {
        unreachable!("`rules select` must parse as the Select action")
    };
    assert_eq!(select.candidates, 12);
    assert_eq!(select.runner, Some(actual_cli::RunnerChoice::AnthropicApi));
    assert_eq!(select.model.as_deref(), Some("claude-sonnet-4-6"));

    let offline = Cli::parse_from(["actual", "rules", "select", "plan", "--no-rank"]);
    let Command::Rules(args) = offline.command else {
        unreachable!()
    };
    let actual_cli::RulesAction::Select(select) = args.action else {
        unreachable!()
    };
    assert!(select.no_rank);
}

#[test]
fn test_cli_parse_rules_eval() {
    let cli = Cli::parse_from([
        "actual",
        "rules",
        "eval",
        "--golden",
        "golden.json",
        "--limit",
        "10",
        "--ablate",
        "title",
        "--ablate",
        "slug",
        "--json",
        "--rebuild",
    ]);
    let Command::Rules(args) = cli.command else {
        unreachable!()
    };
    let actual_cli::RulesAction::Eval(eval) = args.action else {
        unreachable!("`rules eval` must parse as the Eval action")
    };
    assert_eq!(eval.golden, std::path::PathBuf::from("golden.json"));
    assert_eq!(eval.limit, 10);
    assert_eq!(eval.ablate, vec!["title", "slug"]);
    assert!(eval.json);
    assert!(eval.rebuild);
    // Scoring the two-stage selector costs a runner call per case, so it is
    // opt-in: the default run is the offline comparison CI can afford.
    assert!(!eval.rank);
}

/// `rules eval --rank` adds the two-stage selector to the comparison.
#[test]
fn test_cli_parse_rules_eval_rank() {
    let cli = Cli::parse_from([
        "actual",
        "rules",
        "eval",
        "--golden",
        "golden.json",
        "--rank",
        "--candidates",
        "20",
        "--runner",
        "openai-api",
        "--model",
        "gpt-5.2",
    ]);
    let Command::Rules(args) = cli.command else {
        unreachable!()
    };
    let actual_cli::RulesAction::Eval(eval) = args.action else {
        unreachable!()
    };
    assert!(eval.rank);
    assert_eq!(eval.candidates, 20);
    assert_eq!(eval.runner, Some(actual_cli::RunnerChoice::OpenAiApi));
    assert_eq!(eval.model.as_deref(), Some("gpt-5.2"));
}

/// `rules select` without a plan is a usage error, not an empty selection.
#[test]
fn test_cli_parse_rules_select_requires_a_plan() {
    assert!(Cli::try_parse_from(["actual", "rules", "select"]).is_err());
}

/// `rules eval` without a golden set is a usage error: there is nothing to
/// measure against.
#[test]
fn test_cli_parse_rules_eval_requires_a_golden_set() {
    assert!(Cli::try_parse_from(["actual", "rules", "eval"]).is_err());
}

/// `rules ls` in a directory with no `.actual/rules/` is a successful empty
/// listing, not a failure — an unsynced repository is an ordinary state.
#[test]
fn test_run_rules_ls_without_a_rules_directory() {
    let dir = tempfile::tempdir().unwrap();
    let cli = Cli::parse_from([
        "actual",
        "rules",
        "ls",
        dir.path().to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(handle_result(run(cli)), 0);
}

/// The reader is consumed the way the downstream scope-index task will consume
/// it: through the crate's public path, never a private item.
#[test]
fn test_public_rules_api_loads_rule_documents() {
    let dir = tempfile::tempdir().unwrap();
    let rules_dir = actual_cli::rules::rules_dir(dir.path());
    std::fs::create_dir_all(&rules_dir).unwrap();
    std::fs::write(
        rules_dir.join("alpha.md"),
        "# Alpha\n\nThese rules are ALWAYS ACTIVE.\n\n### Rules\n\n\
         - **R-A-001** MUST: hold.\n- **R-A-002** MUST NOT: break.\n",
    )
    .unwrap();
    std::fs::write(rules_dir.join("beta.md"), "not a rule file\n").unwrap();

    let report = actual_cli::rules::load_rule_set(dir.path()).unwrap();

    assert_eq!(report.documents.len(), 1);
    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.rule_count(), 2);
    assert_eq!(
        report.documents[0]
            .rules
            .iter()
            .map(|rule| rule.level)
            .collect::<Vec<_>>(),
        vec![
            actual_cli::rules::RuleLevel::Must,
            actual_cli::rules::RuleLevel::MustNot,
        ]
    );
}
