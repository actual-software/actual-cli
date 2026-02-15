use actual_cli::*;
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
fn test_run_sync() {
    let cli = Cli::parse_from(["actual", "sync"]);
    assert_eq!(run(cli), 0);
}

#[test]
fn test_run_status() {
    let _lock = ENV_MUTEX.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");
    std::env::set_var("ACTUAL_CONFIG", config_file.to_str().unwrap());
    let cli = Cli::parse_from(["actual", "status"]);
    assert_eq!(run(cli), 0);
    std::env::remove_var("ACTUAL_CONFIG");
}

#[test]
fn test_run_auth() {
    let _lock = ENV_MUTEX.lock().unwrap();
    std::env::set_var("CLAUDE_BINARY", "/nonexistent/path/to/claude");
    let cli = Cli::parse_from(["actual", "auth"]);
    assert_eq!(run(cli), 2);
    std::env::remove_var("CLAUDE_BINARY");
}

#[test]
fn test_run_config_show() {
    let _lock = ENV_MUTEX.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");
    std::env::set_var("ACTUAL_CONFIG", config_file.to_str().unwrap());
    let cli = Cli::parse_from(["actual", "config", "show"]);
    assert_eq!(run(cli), 0);
    std::env::remove_var("ACTUAL_CONFIG");
}

#[test]
fn test_run_config_set() {
    let _lock = ENV_MUTEX.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");
    std::env::set_var("ACTUAL_CONFIG", config_file.to_str().unwrap());
    let cli = Cli::parse_from(["actual", "config", "set", "batch_size", "10"]);
    assert_eq!(run(cli), 0);
    std::env::remove_var("ACTUAL_CONFIG");
}

#[test]
fn test_run_config_path() {
    let _lock = ENV_MUTEX.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");
    std::env::set_var("ACTUAL_CONFIG", config_file.to_str().unwrap());
    let cli = Cli::parse_from(["actual", "config", "path"]);
    assert_eq!(run(cli), 0);
    std::env::remove_var("ACTUAL_CONFIG");
}
