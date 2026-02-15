use actual_cli::*;
use clap::Parser;

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
    let cli = Cli::parse_from(["actual", "status"]);
    assert_eq!(run(cli), 0);
}

#[test]
fn test_run_auth() {
    let cli = Cli::parse_from(["actual", "auth"]);
    assert_eq!(run(cli), 0);
}

#[test]
fn test_run_config_show() {
    let cli = Cli::parse_from(["actual", "config", "show"]);
    assert_eq!(run(cli), 0);
}

#[test]
fn test_run_config_set() {
    let cli = Cli::parse_from(["actual", "config", "set", "foo", "bar"]);
    assert_eq!(run(cli), 0);
}

#[test]
fn test_run_config_path() {
    let cli = Cli::parse_from(["actual", "config", "path"]);
    assert_eq!(run(cli), 0);
}
