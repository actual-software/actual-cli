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
        .stdout(predicate::str::contains(
            "ADR-powered AI context file generator",
        ))
        .stdout(predicate::str::contains("adr-bot"))
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
fn test_sync_without_claude_binary() {
    // When Claude binary is not found, sync should exit with code 2.
    // Explicitly set --runner to avoid the user's config auto-selecting a
    // different runner (e.g. codex-cli) which would bypass find_claude_binary().
    cmd()
        .args(["adr-bot", "--dry-run", "--runner", "claude-cli"])
        .env("CLAUDE_BINARY", "/nonexistent/path/to/claude")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Claude Code is not installed"));
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
        .stdout(predicate::str::contains("CLAUDE.md Files"));
}

#[test]
fn test_auth_binary_not_found() {
    cmd()
        .arg("auth")
        .env("CLAUDE_BINARY", "/nonexistent/path/to/claude")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Claude Code is not installed"));
}

#[cfg(unix)]
#[test]
fn test_auth_success_with_fake_binary() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("fake-claude");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '%s\\n' '{\"loggedIn\": true, \"authMethod\": \"claude.ai\", \"email\": \"user@example.com\"}'\nexit 0\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    cmd()
        .arg("auth")
        .env("CLAUDE_BINARY", script.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("authenticated"));
}

#[cfg(unix)]
#[test]
fn test_auth_not_authenticated_with_fake_binary() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("fake-claude");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '%s\\n' '{\"loggedIn\": false}'\nexit 0\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    cmd()
        .arg("auth")
        .env("CLAUDE_BINARY", script.to_str().unwrap())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("not authenticated"));
}

#[test]
fn test_config_show() {
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");
    cmd()
        .args(["config", "show"])
        .env("ACTUAL_CONFIG", config_file.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn test_config_set() {
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");

    // Set a config value
    cmd()
        .args(["config", "set", "batch_size", "20"])
        .env("ACTUAL_CONFIG", config_file.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("Set batch_size = 20"));

    // Verify via config show
    cmd()
        .args(["config", "show"])
        .env("ACTUAL_CONFIG", config_file.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("batch_size: 20"));
}

#[test]
fn test_config_path() {
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");
    let config_str = config_file.to_str().unwrap();
    cmd()
        .args(["config", "path"])
        .env("ACTUAL_CONFIG", config_str)
        .assert()
        .success()
        .stdout(predicate::str::contains(config_str));
}

#[test]
fn test_no_args_shows_error() {
    cmd()
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage"));
}

#[test]
fn test_sync_with_flags_without_claude() {
    // Sync with flags but without Claude binary — exits with code 2.
    // Use an empty temp config so the user's global config (which may set
    // `runner: claude-cli`) doesn't bypass auto_detect_runner.
    let config_file = tempfile::NamedTempFile::new().unwrap();
    cmd()
        .args([
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
            "--max-budget-usd",
            "1.50",
        ])
        .env("CLAUDE_BINARY", "/nonexistent/path/to/claude")
        .env("ACTUAL_CONFIG", config_file.path())
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("CURSOR_API_KEY")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("No runner available for model"));
}

#[test]
fn test_sync_dry_run_without_claude() {
    let dir = tempfile::tempdir().unwrap();
    cmd()
        .args(["adr-bot", "--dry-run", "--runner", "claude-cli"])
        .current_dir(dir.path())
        .env("CLAUDE_BINARY", "/nonexistent/path/to/claude")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Claude Code is not installed"));

    // No CLAUDE.md files should be written
    assert!(!dir.path().join("CLAUDE.md").exists());
}

#[test]
fn test_sync_force_without_claude() {
    let dir = tempfile::tempdir().unwrap();
    cmd()
        .args(["adr-bot", "--force", "--runner", "claude-cli"])
        .current_dir(dir.path())
        .env("CLAUDE_BINARY", "/nonexistent/path/to/claude")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Claude Code is not installed"));
}

#[test]
fn test_sync_no_tailor_without_claude() {
    let dir = tempfile::tempdir().unwrap();
    cmd()
        .args([
            "adr-bot",
            "--no-tailor",
            "--force",
            "--runner",
            "claude-cli",
        ])
        .current_dir(dir.path())
        .env("CLAUDE_BINARY", "/nonexistent/path/to/claude")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Claude Code is not installed"));
}

#[test]
fn test_sync_force_dry_run_without_claude() {
    let dir = tempfile::tempdir().unwrap();
    cmd()
        .args(["adr-bot", "--force", "--dry-run", "--runner", "claude-cli"])
        .current_dir(dir.path())
        .env("CLAUDE_BINARY", "/nonexistent/path/to/claude")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Claude Code is not installed"));

    // No CLAUDE.md files should be written
    assert!(!dir.path().join("CLAUDE.md").exists());
}

#[test]
fn test_sync_help_shows_all_flags() {
    cmd()
        .args(["adr-bot", "--help"])
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
        .args(["adr-bot", "--full"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--dry-run"));
}

#[test]
fn test_config_set_anthropic_api_key_redacts_in_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");
    cmd()
        .args(["config", "set", "anthropic_api_key", "sk-ant-test-12345"])
        .env("ACTUAL_CONFIG", &config_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("anthropic_api_key"))
        .stdout(predicate::str::contains("[redacted]"))
        .stdout(predicate::str::contains("sk-ant-test-12345").not());
}

#[test]
fn test_config_set_openai_api_key_redacts_in_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");
    cmd()
        .args(["config", "set", "openai_api_key", "sk-openai-test-67890"])
        .env("ACTUAL_CONFIG", &config_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("openai_api_key"))
        .stdout(predicate::str::contains("[redacted]"))
        .stdout(predicate::str::contains("sk-openai-test-67890").not());
}

#[test]
fn test_config_show_after_anthropic_api_key_set_redacts() {
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");

    // First: set the key
    cmd()
        .args(["config", "set", "anthropic_api_key", "sk-ant-test-12345"])
        .env("ACTUAL_CONFIG", &config_file)
        .assert()
        .success();

    // Then: show should redact
    cmd()
        .args(["config", "show"])
        .env("ACTUAL_CONFIG", &config_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("anthropic_api_key"))
        .stdout(predicate::str::contains("[redacted]"))
        .stdout(predicate::str::contains("sk-ant-test-12345").not());
}

#[test]
fn test_config_show_after_openai_api_key_set_redacts() {
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");

    // First: set the key
    cmd()
        .args(["config", "set", "openai_api_key", "sk-openai-test-67890"])
        .env("ACTUAL_CONFIG", &config_file)
        .assert()
        .success();

    // Then: show should redact
    cmd()
        .args(["config", "show"])
        .env("ACTUAL_CONFIG", &config_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("openai_api_key"))
        .stdout(predicate::str::contains("[redacted]"))
        .stdout(predicate::str::contains("sk-openai-test-67890").not());
}

#[test]
fn test_config_set_cursor_api_key_redacts_in_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");
    cmd()
        .args(["config", "set", "cursor_api_key", "cursor-test-key-99999"])
        .env("ACTUAL_CONFIG", &config_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("cursor_api_key"))
        .stdout(predicate::str::contains("[redacted]"))
        .stdout(predicate::str::contains("cursor-test-key-99999").not());
}

#[test]
fn test_config_show_after_cursor_api_key_set_redacts() {
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");

    // First: set the key
    cmd()
        .args(["config", "set", "cursor_api_key", "cursor-test-key-99999"])
        .env("ACTUAL_CONFIG", &config_file)
        .assert()
        .success();

    // Then: show should redact
    cmd()
        .args(["config", "show"])
        .env("ACTUAL_CONFIG", &config_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("cursor_api_key"))
        .stdout(predicate::str::contains("[redacted]"))
        .stdout(predicate::str::contains("cursor-test-key-99999").not());
}

#[test]
fn test_config_show_redacts_both_keys_simultaneously() {
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.yaml");

    // Set both keys
    cmd()
        .args(["config", "set", "anthropic_api_key", "sk-ant-test-12345"])
        .env("ACTUAL_CONFIG", &config_file)
        .assert()
        .success();

    cmd()
        .args(["config", "set", "openai_api_key", "sk-openai-test-67890"])
        .env("ACTUAL_CONFIG", &config_file)
        .assert()
        .success();

    // Show once — both keys should be redacted, neither raw value should appear
    cmd()
        .args(["config", "show"])
        .env("ACTUAL_CONFIG", &config_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("anthropic_api_key"))
        .stdout(predicate::str::contains("openai_api_key"))
        .stdout(predicate::str::contains("[redacted]"))
        .stdout(predicate::str::contains("sk-ant-test-12345").not())
        .stdout(predicate::str::contains("sk-openai-test-67890").not());
}
