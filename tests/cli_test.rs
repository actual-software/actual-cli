use assert_cmd::Command;
use predicates::prelude::*;

fn cmd() -> Command {
    Command::from(assert_cmd::cargo::cargo_bin_cmd!("actual"))
}

#[test]
fn test_help() {
    cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("ADR-powered CLAUDE.md generator"))
        .stdout(predicate::str::contains("sync"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("auth"))
        .stdout(predicate::str::contains("config"));
}

#[test]
fn test_version() {
    cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn test_sync_not_implemented() {
    cmd()
        .arg("sync")
        .assert()
        .success()
        .stderr(predicate::str::contains("not implemented yet"));
}

#[test]
fn test_status_shows_config() {
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");
    cmd()
        .arg("status")
        .env("ACTUAL_CONFIG", config_file.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("Config"))
        .stdout(predicate::str::contains("CLAUDE.md files"));
}

#[test]
fn test_auth_not_implemented() {
    cmd()
        .arg("auth")
        .assert()
        .success()
        .stderr(predicate::str::contains("not implemented yet"));
}

#[test]
fn test_config_show_not_implemented() {
    cmd()
        .args(["config", "show"])
        .assert()
        .success()
        .stderr(predicate::str::contains("not implemented yet"));
}

#[test]
fn test_config_set_not_implemented() {
    cmd()
        .args(["config", "set", "foo", "bar"])
        .assert()
        .success()
        .stderr(predicate::str::contains("not implemented yet"));
}

#[test]
fn test_config_path_not_implemented() {
    cmd()
        .args(["config", "path"])
        .assert()
        .success()
        .stderr(predicate::str::contains("not implemented yet"));
}

#[test]
fn test_no_args_shows_error() {
    cmd()
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage"));
}

#[test]
fn test_sync_with_flags() {
    cmd()
        .args([
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
            "--max-budget-usd",
            "1.50",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("not implemented yet"));
}

#[test]
fn test_sync_help_shows_all_flags() {
    cmd()
        .args(["sync", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--dry-run"))
        .stdout(predicate::str::contains("--full"))
        .stdout(predicate::str::contains("--force"))
        .stdout(predicate::str::contains("--reset-rejections"))
        .stdout(predicate::str::contains("--project"))
        .stdout(predicate::str::contains("--model"))
        .stdout(predicate::str::contains("--api-url"))
        .stdout(predicate::str::contains("--verbose"))
        .stdout(predicate::str::contains("--no-tailor"))
        .stdout(predicate::str::contains("--max-budget-usd"))
        .stdout(predicate::str::contains(
            "Show summary of what would change",
        ))
        .stdout(predicate::str::contains(
            "Maximum budget per tailoring invocation",
        ));
}

#[test]
fn test_sync_full_without_dry_run_fails() {
    cmd()
        .args(["sync", "--full"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--dry-run"));
}
