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
fn test_status_not_implemented() {
    cmd()
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::contains("not implemented yet"));
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
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("not implemented yet"));
}
