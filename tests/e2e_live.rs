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

fn create_realistic_rust_project(dir: &std::path::Path) {
    fs::write(
        dir.join("Cargo.toml"),
        r#"[package]
name = "sample-cli-app"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
clap = { version = "4", features = ["derive"] }
anyhow = "1"
reqwest = { version = "0.12", features = ["json", "rustls-tls"] }
tracing = "0.1"
tracing-subscriber = "0.3"
"#,
    )
    .unwrap();

    fs::create_dir_all(dir.join("src/api")).unwrap();

    fs::write(
        dir.join("src/main.rs"),
        r#"mod api;
mod config;
mod error;

use clap::Parser;

#[derive(Parser)]
#[command(name = "sample-cli")]
struct Cli {
    #[arg(short, long)]
    config: Option<String>,
    #[arg(short, long, default_value = "false")]
    verbose: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt::init();
    let cfg = config::AppConfig::load(cli.config.as_deref())?;
    let client = api::ApiClient::new(&cfg.api_url);
    let result = client.fetch_data().await?;
    println!("{result}");
    Ok(())
}
"#,
    )
    .unwrap();

    fs::write(
        dir.join("src/config.rs"),
        r#"use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub api_url: String,
    pub timeout_secs: u64,
    pub retry_count: u32,
}

impl AppConfig {
    pub fn load(path: Option<&str>) -> anyhow::Result<Self> {
        let path = path.unwrap_or("config.toml");
        let content = std::fs::read_to_string(path)?;
        let config: AppConfig = serde_json::from_str(&content)?;
        Ok(config)
    }
}
"#,
    )
    .unwrap();

    fs::write(
        dir.join("src/api/mod.rs"),
        r#"mod client;
pub use client::ApiClient;
"#,
    )
    .unwrap();

    fs::write(
        dir.join("src/api/client.rs"),
        r#"use reqwest::Client;

pub struct ApiClient {
    client: Client,
    base_url: String,
}

impl ApiClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.to_string(),
        }
    }

    pub async fn fetch_data(&self) -> anyhow::Result<String> {
        let resp = self.client
            .get(&format!("{}/data", self.base_url))
            .send()
            .await?
            .text()
            .await?;
        Ok(resp)
    }
}
"#,
    )
    .unwrap();

    fs::write(
        dir.join("src/error.rs"),
        r#"use std::fmt;

#[derive(Debug)]
pub enum AppError {
    Config(String),
    Api(String),
    Io(std::io::Error),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::Config(msg) => write!(f, "config error: {msg}"),
            AppError::Api(msg) => write!(f, "API error: {msg}"),
            AppError::Io(e) => write!(f, "IO error: {e}"),
        }
    }
}

impl std::error::Error for AppError {}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::Io(e)
    }
}
"#,
    )
    .unwrap();

    fs::create_dir_all(dir.join("tests")).unwrap();
    fs::write(
        dir.join("tests/integration_test.rs"),
        r#"#[test]
fn it_works() {
    assert_eq!(2 + 2, 4);
}
"#,
    )
    .unwrap();

    fs::write(dir.join(".gitignore"), "/target\n").unwrap();
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
        assert!(status.success(), "git {} failed", args.join(" "));
    };
    run(&["init"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test User"]);
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
fn live_sync_realistic_project() {
    require_claude_auth();

    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    create_realistic_rust_project(dir);
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

    // A realistic project with tokio/serde/clap/reqwest should match significantly
    // more ADRs than the minimal hello-world project
    let heading_count = content.matches("## ").count();
    assert!(
        heading_count >= 3,
        "expected at least 3 ADR headings for a realistic project, got {heading_count}"
    );

    // Validate that adr-ids contains multiple entries
    let adr_ids_line = content
        .lines()
        .find(|l| l.contains("<!-- adr-ids:"))
        .expect("should have adr-ids line");
    let comma_count = adr_ids_line.matches(',').count();
    assert!(
        comma_count >= 2,
        "expected at least 3 ADR IDs (2+ commas) for realistic project, got {} commas in: {}",
        comma_count,
        adr_ids_line
    );

    // Managed content should be substantial for a realistic project
    let managed_start = content.find("<!-- managed:actual-start -->").unwrap();
    let managed_end = content.find("<!-- managed:actual-end -->").unwrap();
    let managed_len = managed_end - managed_start;
    assert!(
        managed_len > 500,
        "expected substantial managed content (>500 chars) for realistic project, got {managed_len}"
    );
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
#[ignore = "requires LIVE_E2E_TAILOR=1; run with --ignored and set LIVE_E2E_TAILOR=1"]
fn live_sync_with_tailoring() {
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
