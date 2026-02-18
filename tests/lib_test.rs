use actual_cli::{handle_result, run, Cli, Command, ConfigAction};
use clap::Parser;
use std::sync::Mutex;

/// Mutex to serialize tests that manipulate env vars, since env vars are
/// process-global and tests run in parallel by default.
static ENV_MUTEX: Mutex<()> = Mutex::new(());

#[test]
fn test_cli_parse_sync() {
    let cli = Cli::parse_from(["actual", "sync"]);
    assert!(matches!(cli.command, Command::Sync(_)));
}

#[test]
fn test_cli_parse_sync_with_flags() {
    let cli = Cli::parse_from([
        "actual",
        "sync",
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
    let Command::Sync(args) = cli.command else {
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
    let cli = Cli::parse_from(["actual", "sync"]);
    let Command::Sync(args) = cli.command else {
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
    let result = Cli::try_parse_from(["actual", "sync", "--full"]);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("--dry-run"),
        "error should mention --dry-run: {err}"
    );
}

#[test]
fn test_cli_parse_sync_dry_run_full_accepted() {
    let cli = Cli::parse_from(["actual", "sync", "--dry-run", "--full"]);
    let Command::Sync(args) = cli.command else {
        unreachable!()
    };
    assert!(args.dry_run);
    assert!(args.full);
}

#[test]
fn test_cli_parse_sync_multiple_projects() {
    let cli = Cli::parse_from([
        "actual",
        "sync",
        "--project",
        "apps/web",
        "--project",
        "apps/api",
    ]);
    let Command::Sync(args) = cli.command else {
        unreachable!()
    };
    assert_eq!(args.projects, vec!["apps/web", "apps/api"]);
}

#[test]
fn test_cli_parse_sync_model_and_budget() {
    let cli = Cli::parse_from([
        "actual",
        "sync",
        "--model",
        "opus",
        "--max-budget-usd",
        "0.50",
    ]);
    let Command::Sync(args) = cli.command else {
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
    assert!(matches!(cli.command, Command::Auth));
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
    let _lock = ENV_MUTEX.lock().unwrap();
    std::env::set_var("CLAUDE_BINARY", "/nonexistent/path/to/claude");
    let cli = Cli::parse_from(["actual", "sync", "--dry-run"]);
    assert_eq!(handle_result(run(cli)), 2);
    std::env::remove_var("CLAUDE_BINARY");
}

#[test]
fn test_run_status() {
    let _lock = ENV_MUTEX.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");
    std::env::set_var("ACTUAL_CONFIG", config_file.to_str().unwrap());
    let cli = Cli::parse_from(["actual", "status"]);
    assert_eq!(handle_result(run(cli)), 0);
    std::env::remove_var("ACTUAL_CONFIG");
}

#[test]
fn test_run_auth() {
    let _lock = ENV_MUTEX.lock().unwrap();
    std::env::set_var("CLAUDE_BINARY", "/nonexistent/path/to/claude");
    let cli = Cli::parse_from(["actual", "auth"]);
    assert_eq!(handle_result(run(cli)), 2);
    std::env::remove_var("CLAUDE_BINARY");
}

#[test]
fn test_run_config_show() {
    let _lock = ENV_MUTEX.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");
    std::env::set_var("ACTUAL_CONFIG", config_file.to_str().unwrap());
    let cli = Cli::parse_from(["actual", "config", "show"]);
    assert_eq!(handle_result(run(cli)), 0);
    std::env::remove_var("ACTUAL_CONFIG");
}

#[test]
fn test_run_config_set() {
    let _lock = ENV_MUTEX.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");
    std::env::set_var("ACTUAL_CONFIG", config_file.to_str().unwrap());
    let cli = Cli::parse_from(["actual", "config", "set", "batch_size", "10"]);
    assert_eq!(handle_result(run(cli)), 0);
    std::env::remove_var("ACTUAL_CONFIG");
}

#[test]
fn test_run_config_path() {
    let _lock = ENV_MUTEX.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");
    std::env::set_var("ACTUAL_CONFIG", config_file.to_str().unwrap());
    let cli = Cli::parse_from(["actual", "config", "path"]);
    assert_eq!(handle_result(run(cli)), 0);
    std::env::remove_var("ACTUAL_CONFIG");
}

/// Create a fake Claude binary script that handles both `auth status --json`
/// and `--print ...` (analysis) invocations.
///
/// - `auth status --json` → returns the given `auth_json`
/// - `--print ...` → returns the given `analysis_json`
#[cfg(unix)]
fn create_fake_claude_binary(
    dir: &std::path::Path,
    auth_json: &str,
    analysis_json: &str,
) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("fake-claude");
    let script_content = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"auth\" ]; then\n\
         printf '%s\\n' '{}'\n\
         exit 0\n\
         elif [ \"$1\" = \"--print\" ]; then\n\
         printf '%s\\n' '{}'\n\
         exit 0\n\
         else\n\
         echo \"unexpected args: $@\" >&2\n\
         exit 1\n\
         fi\n",
        auth_json, analysis_json,
    );
    std::fs::write(&script, script_content).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

#[test]
fn test_cli_parse_sync_negative_budget_rejected() {
    let result = Cli::try_parse_from(["actual", "sync", "--max-budget-usd", "-5"]);
    assert!(result.is_err(), "negative budget should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("non-negative"),
        "error should mention non-negative: {err}"
    );
}

#[test]
fn test_cli_parse_sync_zero_budget_accepted() {
    let cli = Cli::parse_from(["actual", "sync", "--max-budget-usd", "0"]);
    let Command::Sync(args) = cli.command else {
        unreachable!()
    };
    assert_eq!(args.max_budget_usd, Some(0.0));
}

#[test]
fn test_cli_parse_sync_non_numeric_budget_rejected() {
    let result = Cli::try_parse_from(["actual", "sync", "--max-budget-usd", "abc"]);
    assert!(result.is_err(), "non-numeric budget should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not a valid number"),
        "error should mention invalid number: {err}"
    );
}

#[test]
fn test_cli_parse_sync_nan_budget_rejected() {
    let result = Cli::try_parse_from(["actual", "sync", "--max-budget-usd", "nan"]);
    assert!(result.is_err(), "NaN budget should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("non-negative finite"),
        "error should mention non-negative finite: {err}"
    );
}

#[test]
fn test_cli_parse_sync_inf_budget_rejected() {
    let result = Cli::try_parse_from(["actual", "sync", "--max-budget-usd", "inf"]);
    assert!(result.is_err(), "inf budget should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("non-negative finite"),
        "error should mention non-negative finite: {err}"
    );
}

#[cfg(unix)]
#[test]
fn test_run_sync_force_with_fake_claude() {
    let _lock = ENV_MUTEX.lock().unwrap();
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

    let auth_json = r#"{"loggedIn": true, "authMethod": "claude.ai", "email": "test@example.com"}"#;
    let analysis_json = r#"{"is_monorepo": false, "projects": [{"path": ".", "name": "test-app", "languages": ["rust"], "frameworks": [], "package_manager": "cargo"}]}"#;

    let script = create_fake_claude_binary(dir.path(), auth_json, analysis_json);

    std::env::set_var("CLAUDE_BINARY", script.to_str().unwrap());
    std::env::set_var("ACTUAL_CONFIG", config_file.to_str().unwrap());

    let cli = Cli::parse_from(["actual", "sync", "--force", "--api-url", &server.url()]);
    let exit_code = handle_result(run(cli));

    std::env::remove_var("CLAUDE_BINARY");
    std::env::remove_var("ACTUAL_CONFIG");

    assert_eq!(exit_code, 0, "sync --force with fake Claude should succeed");
}

#[cfg(unix)]
#[test]
fn test_run_sync_not_authenticated_with_fake_claude() {
    let _lock = ENV_MUTEX.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();

    let auth_json = r#"{"loggedIn": false}"#;
    let analysis_json = r#"{}"#; // Won't be reached

    let script = create_fake_claude_binary(dir.path(), auth_json, analysis_json);

    std::env::set_var("CLAUDE_BINARY", script.to_str().unwrap());

    let cli = Cli::parse_from(["actual", "sync", "--force"]);
    let exit_code = handle_result(run(cli));

    std::env::remove_var("CLAUDE_BINARY");

    assert_eq!(
        exit_code, 2,
        "sync with unauthenticated Claude should exit with code 2"
    );
}
