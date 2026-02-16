#![cfg(feature = "live-e2e")]

use assert_cmd::cargo::cargo_bin_cmd;
use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::process;
use tempfile::tempdir;

// ── Helpers ─────────────────────────────────────────────────────────

/// Preflight: skip all tests if Claude Code is not installed/authenticated.
fn require_claude_auth() {
    let output = process::Command::new("claude")
        .args(["auth", "status", "--json"])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            if !stdout.contains("\"loggedIn\": true") && !stdout.contains("\"loggedIn\":true") {
                panic!("Claude Code not authenticated. Run: claude auth login");
            }
        }
        _ => panic!(
            "Claude Code not found or not working. Install with: npm install -g @anthropic-ai/claude-code"
        ),
    }
}

fn create_minimal_rust_project(dir: &std::path::Path) {
    fs::write(
        dir.join("Cargo.toml"),
        r#"[package]
name = "test-project"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("src/main.rs"),
        "fn main() { println!(\"hello\"); }\n",
    )
    .unwrap();
}

fn init_git_repo(dir: &std::path::Path) {
    let run = |args: &[&str]| {
        let status = process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .stdout(process::Stdio::null())
            .stderr(process::Stdio::null())
            .status()
            .expect("failed to run git");
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init"]);
    run(&["add", "."]);
    run(&["commit", "-m", "init"]);
}

fn actual_cmd() -> Command {
    Command::from(cargo_bin_cmd!("actual"))
}

// ── Tests ───────────────────────────────────────────────────────────

#[test]
fn live_auth_check() {
    require_claude_auth();

    actual_cmd()
        .arg("auth")
        .assert()
        .success()
        .stdout(predicate::str::contains("uthenticat"));
}

#[test]
fn live_sync_no_tailor() {
    require_claude_auth();

    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    create_minimal_rust_project(dir);
    init_git_repo(dir);

    actual_cmd()
        .args(["sync", "--force", "--no-tailor", "--model", "haiku"])
        .current_dir(dir)
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();

    let claude_md = dir.join("CLAUDE.md");
    assert!(claude_md.exists(), "CLAUDE.md should exist after sync");

    let content = fs::read_to_string(&claude_md).unwrap();
    assert!(
        content.contains("<!-- managed:actual-start -->"),
        "missing managed start marker"
    );
    assert!(
        content.contains("<!-- managed:actual-end -->"),
        "missing managed end marker"
    );
    assert!(
        content.contains("<!-- version: 1 -->"),
        "missing version marker"
    );
    assert!(
        content.contains("<!-- adr-ids:"),
        "missing adr-ids metadata"
    );
    // At least one ADR heading
    assert!(content.contains("## "), "expected at least one ## heading");
}

#[test]
fn live_resync_preserves_user_content() {
    require_claude_auth();

    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    create_minimal_rust_project(dir);
    init_git_repo(dir);

    // First sync
    actual_cmd()
        .args(["sync", "--force", "--no-tailor", "--model", "haiku"])
        .current_dir(dir)
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();

    // Prepend user content before the managed section
    let claude_md = dir.join("CLAUDE.md");
    let existing = fs::read_to_string(&claude_md).unwrap();
    let with_user_content = format!("# My Custom Notes\n\nUser content here.\n\n{}", existing);
    fs::write(&claude_md, &with_user_content).unwrap();

    // Second sync
    actual_cmd()
        .args(["sync", "--force", "--no-tailor", "--model", "haiku"])
        .current_dir(dir)
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();

    let content = fs::read_to_string(&claude_md).unwrap();
    assert!(
        content.contains("# My Custom Notes"),
        "user heading should be preserved"
    );
    assert!(
        content.contains("User content here."),
        "user content should be preserved"
    );
    assert!(
        content.contains("<!-- managed:actual-start -->"),
        "managed start marker should be present"
    );
    assert!(
        content.contains("<!-- managed:actual-end -->"),
        "managed end marker should be present"
    );
    assert!(
        content.contains("<!-- version: 2 -->"),
        "version should be incremented to 2"
    );
}

#[test]
fn live_dry_run_no_files() {
    require_claude_auth();

    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    create_minimal_rust_project(dir);
    init_git_repo(dir);

    actual_cmd()
        .args([
            "sync",
            "--force",
            "--no-tailor",
            "--dry-run",
            "--model",
            "haiku",
        ])
        .current_dir(dir)
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();

    assert!(
        !dir.join("CLAUDE.md").exists(),
        "CLAUDE.md should NOT exist after dry-run"
    );
}

#[test]
fn live_sync_with_tailoring() {
    if std::env::var("LIVE_E2E_TAILOR").as_deref() != Ok("1") {
        eprintln!("Skipping live_sync_with_tailoring: set LIVE_E2E_TAILOR=1 to enable");
        return;
    }

    require_claude_auth();

    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    create_minimal_rust_project(dir);
    init_git_repo(dir);

    actual_cmd()
        .args([
            "sync",
            "--force",
            "--model",
            "haiku",
            "--max-budget-usd",
            "0.25",
        ])
        .current_dir(dir)
        .timeout(std::time::Duration::from_secs(180))
        .assert()
        .success();

    let claude_md = dir.join("CLAUDE.md");
    assert!(
        claude_md.exists(),
        "CLAUDE.md should exist after sync with tailoring"
    );

    let content = fs::read_to_string(&claude_md).unwrap();
    assert!(
        content.contains("<!-- managed:actual-start -->"),
        "missing managed start marker"
    );
    assert!(
        content.contains("<!-- managed:actual-end -->"),
        "missing managed end marker"
    );
}
