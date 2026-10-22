use assert_cmd::Command;
use predicates::prelude::*;

fn cmd() -> Command {
    assert_cmd::cargo::cargo_bin_cmd!("actual")
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

// ---- update check and `actual version` ----

/// `actual version --json` is the contract the actual skill reads: these six
/// fields, answered without a network request or a cache write.
#[test]
fn test_version_json_answers_offline() {
    let dir = tempfile::tempdir().unwrap();
    let output = cmd()
        .args(["version", "--json"])
        .env("ACTUAL_CONFIG_DIR", dir.path())
        .env_remove("ACTUAL_CONFIG")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "current_version": env!("CARGO_PKG_VERSION"),
            "latest_version": null,
            "update_available": null,
            "channel": "other",
            "upgrade_command": null,
            "release_url": "https://github.com/actual-software/actual-releases/releases/latest",
        })
    );
    assert!(!dir.path().join("update-check.yaml").exists());
}

#[test]
fn test_version_check_makes_no_request_when_turned_off() {
    let dir = tempfile::tempdir().unwrap();
    let output = cmd()
        .args(["version", "--check", "--json"])
        .env("ACTUAL_CONFIG_DIR", dir.path())
        .env_remove("ACTUAL_CONFIG")
        .env("ACTUAL_NO_UPDATE_CHECK", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert!(value["latest_version"].is_null());
    assert!(!dir.path().join("update-check.yaml").exists());

    cmd()
        .args(["version", "--check"])
        .env("ACTUAL_CONFIG_DIR", dir.path())
        .env_remove("ACTUAL_CONFIG")
        .env("ACTUAL_NO_UPDATE_CHECK", "1")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "not checked (update checks are turned off)",
        ));
}

/// A config file that fails to load may be the one turning update checks off.
/// `version --json` still answers with the local facts beside it.
#[test]
fn test_version_json_keeps_its_local_facts_when_the_config_cannot_load() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.yaml"),
        "update_check:\n  enabled: false\nbatch_size: lots\n",
    )
    .unwrap();
    let output = cmd()
        .args(["version", "--json"])
        .env("ACTUAL_CONFIG_DIR", dir.path())
        .env_remove("ACTUAL_CONFIG")
        .env_remove("ACTUAL_NO_UPDATE_CHECK")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["current_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(value["channel"], "other");
    assert!(value["latest_version"].is_null());
    assert!(!dir.path().join("update-check.yaml").exists());
}

/// With stderr piped, as under an agent's tool call or a script, a command
/// neither prints the notice nor starts a lookup, even with a newer version
/// cached and the last lookup days old.
#[test]
fn test_update_notice_stays_silent_when_stderr_is_not_a_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("update-check.yaml");
    let seeded = "checked_at: 2020-01-01T00:00:00Z\nlatest_version: 999.0.0\n";
    std::fs::write(&cache, seeded).unwrap();
    cmd()
        .arg("runners")
        .env("ACTUAL_CONFIG_DIR", dir.path())
        .env_remove("ACTUAL_CONFIG")
        .env_remove("CI")
        .env_remove("ACTUAL_NO_UPDATE_CHECK")
        .assert()
        .success()
        .stderr(predicate::str::contains("is available").not());
    assert_eq!(std::fs::read_to_string(&cache).unwrap(), seeded);
}

#[test]
fn test_help_lists_version_and_hides_update_check() {
    cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Print the version, how this copy was installed, and how to upgrade it",
        ))
        .stdout(predicate::str::contains("__update-check").not());
}

#[test]
fn test_update_check_command_makes_no_request_when_turned_off() {
    let dir = tempfile::tempdir().unwrap();
    cmd()
        .arg("__update-check")
        .env("ACTUAL_CONFIG_DIR", dir.path())
        .env_remove("ACTUAL_CONFIG")
        .env("ACTUAL_NO_UPDATE_CHECK", "1")
        .assert()
        .success()
        .stdout("")
        .stderr("");
    assert!(!dir.path().join("update-check.yaml").exists());
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

// ── plan-check: the stdin-touching paths, tested as a real subprocess ──────
//
// `resolve_direct_plan`'s stdin fallback and `--claude-hook`'s envelope read
// both call `std::io::stdin()` directly. Driving them in-process from a
// `--lib` unit test would read whatever stdin the test harness itself has —
// a real terminal, notably, which could block waiting for input. Running the
// compiled binary as its own subprocess with piped stdin (the same pattern
// every other `cmd()` test here uses) controls that safely.

#[test]
fn test_plan_check_direct_mode_reads_the_plan_from_stdin() {
    let repo = tempfile::tempdir().unwrap();
    cmd()
        .args(["plan-check", "--repo", repo.path().to_str().unwrap()])
        .write_stdin("Add a caching layer in front of the user repository.")
        .assert()
        .success()
        .stdout(predicate::str::contains("Plan check"));
}

#[test]
fn test_plan_check_direct_mode_errors_when_stdin_is_empty() {
    let repo = tempfile::tempdir().unwrap();
    cmd()
        .args(["plan-check", "--repo", repo.path().to_str().unwrap()])
        .write_stdin("   \n")
        .assert()
        .failure()
        .stderr(predicate::str::contains("no plan given"));
}

#[test]
fn test_plan_check_claude_hook_reads_a_valid_envelope_from_stdin() {
    let repo = tempfile::tempdir().unwrap();
    cmd()
        .args([
            "plan-check",
            "--claude-hook",
            "--rules-dir",
            repo.path().join(".actual/rules").to_str().unwrap(),
        ])
        .write_stdin(r#"{"tool_input":{"plan":"Add a caching layer."}}"#)
        .assert()
        .success();
}

/// `read_to_string` fails on invalid UTF-8, which is what exercises
/// `exec_hook`'s own stdin-read-failure branch specifically (as opposed to
/// `exec_hook_with`'s malformed-JSON branch, reached only once stdin has
/// already been read successfully).
#[test]
fn test_plan_check_claude_hook_fails_open_on_non_utf8_stdin() {
    let repo = tempfile::tempdir().unwrap();
    cmd()
        .args([
            "plan-check",
            "--claude-hook",
            "--rules-dir",
            repo.path().join(".actual/rules").to_str().unwrap(),
        ])
        .write_stdin(vec![0xffu8, 0xfe, 0x00, 0x41])
        .assert()
        .success()
        .stdout(predicate::str::contains("systemMessage"));
}

// Mirrors `plan_check_hook::MAX_READ_BYTES` (1 MiB) — kept as a plain
// constant here rather than importing the lib, since this file drives the
// compiled binary as a black-box subprocess.
const MAX_READ_BYTES: usize = 1024 * 1024;

/// The exact behavior a review flagged: an unbounded stdin read used to let
/// `--claude-hook` buffer an arbitrarily large envelope in full before ever
/// parsing it. This must now fail open the same way a non-UTF8 payload does,
/// rather than exhausting memory on a huge or runaway pipe.
#[test]
fn test_plan_check_claude_hook_fails_open_on_oversized_stdin() {
    let repo = tempfile::tempdir().unwrap();
    cmd()
        .args([
            "plan-check",
            "--claude-hook",
            "--rules-dir",
            repo.path().join(".actual/rules").to_str().unwrap(),
        ])
        .write_stdin(vec![b'x'; MAX_READ_BYTES + 1])
        .assert()
        .success()
        .stdout(predicate::str::contains("could not read the hook payload"));
}

/// Direct mode's stdin fallback must also refuse an oversized plan rather
/// than buffering it in full.
#[test]
fn test_plan_check_direct_mode_errors_when_stdin_exceeds_the_size_limit() {
    let repo = tempfile::tempdir().unwrap();
    cmd()
        .args(["plan-check", "--repo", repo.path().to_str().unwrap()])
        .write_stdin(vec![b'x'; MAX_READ_BYTES + 1])
        .assert()
        .failure()
        .stderr(predicate::str::contains("exceeds"));
}

// ── plan-check: the real telemetry dispatcher, driven to a verdict ────────
//
// `dispatch_governance_events` (in `cli::commands::check_engine`) is `cfg(all(feature = "telemetry",
// not(test)))`: the `--lib` build compiles its capturing twin instead, so no
// unit test can execute the function that actually spools the events and
// starts the uploader. Only the compiled binary carries it, and every other
// `plan-check` case in this file stops at `NothingApplies` before an event is
// built. These seed a rules directory and a fake judge so the pipeline
// reaches a verdict, and route `api_url` to loopback so the uploader runs
// without a network.

/// A repository whose `.actual/rules` holds one document the plan applies
/// to, an isolated config directory whose `config.yaml` points telemetry at
/// `api_url`, and a fake `CLAUDE_BINARY` that answers the auth probe as
/// logged in and every other call with one all-conforming verdict batch,
/// the same shape `plan_check.rs`'s own unit tests use.
#[cfg(unix)]
struct GovernedPlanCheck {
    repo: tempfile::TempDir,
    config_dir: tempfile::TempDir,
    fake_claude: std::path::PathBuf,
}

#[cfg(unix)]
impl GovernedPlanCheck {
    const PLAN: &'static str = "Sign access tokens with RS256";

    fn new(api_url: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;

        let repo = tempfile::tempdir().unwrap();
        let rules = repo.path().join(".actual/rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(
            rules.join("cross-cutting-token-signing-1c57.md"),
            "# Sign With Asymmetric Keys: Token Signing\n\nThese rules are ALWAYS ACTIVE for OAuth token signing in `services/auth/oauth/`.\n\n### Rules\n\n- **R-A-001** MUST: sign with RS256.\n- **R-A-002** MUST NOT: log the raw signing key.\n",
        )
        .unwrap();

        let config_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            config_dir.path().join("config.yaml"),
            format!("api_url: \"{api_url}\"\n"),
        )
        .unwrap();

        let envelope = r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"verdicts":[{"doc_slug":"cross-cutting-token-signing-1c57","rule_id":"R-A-001","verdict":"conforming","span":"","reason":"uses RS256"},{"doc_slug":"cross-cutting-token-signing-1c57","rule_id":"R-A-002","verdict":"conforming","span":"","reason":"no logging"}]}}"#;
        let fake_claude = config_dir.path().join("fake-claude.sh");
        std::fs::write(
            &fake_claude,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"auth\" ]; then printf '%s' '{{\"loggedIn\":true}}'; exit 0; fi\nprintf '%s' '{envelope}'\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake_claude, std::fs::Permissions::from_mode(0o755)).unwrap();

        Self {
            repo,
            config_dir,
            fake_claude,
        }
    }

    /// Direct mode with `--no-rank`, so the fake judge answers exactly one
    /// model call, and an explicit runner, so nothing probes for a real one.
    fn cmd(&self) -> Command {
        let mut command = cmd();
        command
            .args([
                "plan-check",
                "--repo",
                self.repo.path().to_str().unwrap(),
                "--no-rank",
                "--runner",
                "claude-cli",
                Self::PLAN,
            ])
            .env("ACTUAL_CONFIG_DIR", self.config_dir.path())
            .env_remove("ACTUAL_CONFIG")
            .env_remove("ACTUAL_NO_TELEMETRY")
            .env("CLAUDE_BINARY", &self.fake_claude)
            .timeout(std::time::Duration::from_secs(60));
        command
    }

    /// Swap in a judge that finds R-A-001 violated, so the run denies the plan
    /// and `--claude-hook` prints that verdict rather than an empty allow.
    fn judge_finds_r_a_001_violated(&self) {
        let envelope = r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"verdicts":[{"doc_slug":"cross-cutting-token-signing-1c57","rule_id":"R-A-001","verdict":"conflicting","span":"HS256","reason":"signs with HS256"},{"doc_slug":"cross-cutting-token-signing-1c57","rule_id":"R-A-002","verdict":"conforming","span":"","reason":"no logging"}]}}"#;
        std::fs::write(
            &self.fake_claude,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"auth\" ]; then printf '%s' '{{\"loggedIn\":true}}'; exit 0; fi\nprintf '%s' '{envelope}'\n"
            ),
        )
        .unwrap();
    }
}

/// Wait up to ten seconds for the uploader a command started to deliver to
/// `mock`, since it sends after the command has returned.
#[cfg(unix)]
fn wait_for_the_uploader(mock: &mockito::Mock) {
    for _ in 0..100 {
        if mock.matched() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// The dispatcher's whole job: the started and completed events for this
/// run reach `POST /plan-governance/record` at the configured `api_url`,
/// sent by the uploader the run starts.
#[cfg(unix)]
#[test]
fn test_plan_check_verdict_sends_governance_events_to_the_configured_api() {
    let mut server = mockito::Server::new();
    let recorded = server
        .mock("POST", "/plan-governance/record")
        .match_header(
            "authorization",
            mockito::Matcher::Regex("^Bearer .+".to_string()),
        )
        .match_body(mockito::Matcher::AllOf(vec![
            mockito::Matcher::Regex(r#""event":"plan_governance_check_started""#.to_string()),
            mockito::Matcher::Regex(r#""event":"plan_governance_check_completed""#.to_string()),
            mockito::Matcher::Regex(r#""command":"plan-check""#.to_string()),
            mockito::Matcher::Regex(r#""decision":"allow""#.to_string()),
        ]))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"recorded":2,"failed":0}"#)
        .expect(1)
        .create();

    let fixture = GovernedPlanCheck::new(&server.url());
    fixture
        .cmd()
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Conforming: no selected rule was violated.",
        ));

    wait_for_the_uploader(&recorded);
    recorded.assert();
}

/// AK-678's acceptance criterion, at the one layer that runs the real
/// dispatcher: nothing listens on loopback port 1, so the send fails on
/// connect, and the run still prints its verdict and exits on the verdict
/// alone. The telemetry id file proves the send path was entered rather
/// than skipped by an opt-out.
#[cfg(unix)]
#[test]
fn test_plan_check_verdict_survives_a_refused_telemetry_endpoint() {
    let fixture = GovernedPlanCheck::new("http://127.0.0.1:1");
    fixture
        .cmd()
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Conforming: no selected rule was violated.",
        ));

    assert!(fixture.config_dir.path().join("telemetry-id").is_file());
}

/// An endpoint that takes every connection and never answers, keeping the
/// bytes each connection sent. A sender waits on it until its own timeout
/// gives up.
#[cfg(all(unix, feature = "telemetry"))]
struct SilentEndpoint {
    url: String,
    received: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
}

#[cfg(all(unix, feature = "telemetry"))]
impl SilentEndpoint {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = std::sync::Arc::clone(&received);
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let log = std::sync::Arc::clone(&log);
                std::thread::spawn(move || {
                    use std::io::Read;
                    let slot = {
                        let mut log = log.lock().unwrap();
                        log.push(Vec::new());
                        log.len() - 1
                    };
                    let mut chunk = [0u8; 4096];
                    loop {
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => log.lock().unwrap()[slot].extend_from_slice(&chunk[..n]),
                        }
                    }
                });
            }
        });
        Self { url, received }
    }

    /// What each connection has sent so far, in the order they arrived.
    fn requests(&self) -> Vec<String> {
        self.received
            .lock()
            .unwrap()
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .collect()
    }
}

/// The hook against an endpoint that never answers, which used to hold it for
/// the whole send timeout. It still denies the plan, and it sends nothing
/// itself: the endpoint's one request is the uploader's, because it carries the
/// event an earlier run left in the spool, and the hook's own events wait in
/// the spool behind it.
#[cfg(all(unix, feature = "telemetry"))]
#[test]
fn test_plan_check_hook_leaves_its_events_to_the_uploader_when_the_endpoint_never_answers() {
    const QUEUED_ID: &str = "queued-by-an-earlier-run";
    let endpoint = SilentEndpoint::start();
    let fixture = GovernedPlanCheck::new(&endpoint.url);
    fixture.judge_finds_r_a_001_violated();
    let spool = scope_spool(fixture.config_dir.path());
    std::fs::write(
        &spool,
        format!(
            "{{\"event\":\"plan_governance_scope_select\",\"distinct_id\":\"{QUEUED_ID}\",\"insert_id\":\"{QUEUED_ID}\"}}\n"
        ),
    )
    .unwrap();

    fixture
        .cmd()
        .arg("--claude-hook")
        .write_stdin(
            r#"{"session_id":"sess-cli-spool-1","tool_input":{"plan":"Sign access tokens with HS256"}}"#,
        )
        .assert()
        .success()
        .stdout(predicate::str::contains(r#""permissionDecision":"deny""#));

    let mut requests = endpoint.requests();
    for _ in 0..100 {
        if requests
            .iter()
            .any(|r| r.contains("plan_governance_check_completed"))
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        requests = endpoint.requests();
    }
    assert_eq!(requests.len(), 1, "{requests:#?}");
    assert!(
        requests[0].contains(QUEUED_ID),
        "the hook sent its own events rather than leaving them to the uploader: {}",
        requests[0]
    );
    assert!(
        requests[0].contains("plan_governance_check_completed"),
        "{}",
        requests[0]
    );

    let queued = std::fs::read_to_string(&spool).unwrap();
    for event in [
        "plan_governance_check_started",
        "plan_governance_check_completed",
        "plan_governance_rule_violation",
    ] {
        assert!(
            queued.contains(event),
            "{event} is not in the spool: {queued}"
        );
    }
}

/// `rules select` publishes one `plan_governance_scope_select` event to the
/// configured `api_url`. Delivery is owned by the detached uploader process the
/// command spawns (APR-004), not the command itself, so the assertion polls until
/// that process has posted the event. `--no-rank` keeps stage 2 (and any runner
/// probe) out of it, so the run is hermetic.
#[cfg(unix)]
#[test]
fn test_rules_select_sends_a_scope_event_to_the_configured_api() {
    let mut server = mockito::Server::new();
    let recorded = server
        .mock("POST", "/plan-governance/record")
        .match_header(
            "authorization",
            mockito::Matcher::Regex("^Bearer .+".to_string()),
        )
        .match_body(mockito::Matcher::AllOf(vec![
            mockito::Matcher::Regex(r#""event":"plan_governance_scope_select""#.to_string()),
            mockito::Matcher::Regex(r#""command":"rules select""#.to_string()),
            mockito::Matcher::Regex(r#""rules_scanned":"#.to_string()),
        ]))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"recorded":1,"failed":0}"#)
        .expect(1)
        .create();

    let repo = tempfile::tempdir().unwrap();
    let rules = repo.path().join(".actual/rules");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(
        rules.join("cross-cutting-token-signing-1c57.md"),
        "# Sign With Asymmetric Keys: Token Signing\n\nThese rules are ALWAYS ACTIVE for OAuth token signing in `services/auth/oauth/`.\n\n### Rules\n\n- **R-A-001** MUST: sign with RS256.\n",
    )
    .unwrap();

    let config_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        config_dir.path().join("config.yaml"),
        format!("api_url: \"{}\"\n", server.url()),
    )
    .unwrap();

    cmd()
        .args([
            "rules",
            "select",
            "--repo",
            repo.path().to_str().unwrap(),
            "--no-rank",
            "Sign access tokens with RS256",
        ])
        .env("ACTUAL_CONFIG_DIR", config_dir.path())
        .env_remove("ACTUAL_CONFIG")
        .env_remove("ACTUAL_NO_TELEMETRY")
        .timeout(std::time::Duration::from_secs(60))
        .assert()
        .success();

    // The uploader delivers after the command returns; wait for it.
    for _ in 0..100 {
        if recorded.matched() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    recorded.assert();
}

// ── plan-check-override: the interactive-terminal gate ──────────────────
//
// `exec_override` refuses unless stdin is a real terminal *and*
// `running_under_claude_code()` is false (see that function's own doc for
// why those two markers are trustworthy). Only the terminal half needs a
// subprocess test: whether a test *process's* own stdin happens to be a
// terminal depends on how the test binary itself was launched, which a
// `--lib` unit test cannot control. `assert_cmd::Command` gives every
// subprocess here piped (non-terminal) stdin by default, which is exactly
// the condition this gate exists to catch — an agent's own shell tool calls
// never get a pty either. `running_under_claude_code()` carries no such
// restriction and is covered directly by `--lib` unit tests instead.

#[test]
fn test_plan_check_override_refuses_without_a_terminal() {
    cmd()
        .args([
            "plan-check-override",
            "--session",
            "sess-1",
            "--rule",
            "doc::R-001",
            "--reason",
            "reviewed",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("interactively"));
}

/// `check-override` is the new primary name (AK-755 step 3); the test above
/// exercises the same gate through the `plan-check-override` alias, kept for
/// backward compatibility.
#[test]
fn test_check_override_refuses_without_a_terminal() {
    cmd()
        .args([
            "check-override",
            "--session",
            "sess-1",
            "--rule",
            "doc::R-001",
            "--reason",
            "reviewed",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("interactively"));
}

// ── impl-check: a git diff against the same committed rule corpus ──────────
//
// `impl-check` (AK-755) reuses `plan-check`'s entire shared pipeline, judging
// a `git diff` instead of plan text. These tests mirror the `plan-check`
// coverage above, adapted for diff resolution: a real throwaway git repo
// with a committed baseline and a working-tree change, instead of piped plan
// text.

/// Run a git command in `cwd`, asserting it succeeds. Mirrors
/// `impl_check.rs`'s own `run_git` test helper (private to the lib crate, so
/// not reusable directly from this black-box integration test).
fn run_git(cwd: &std::path::Path, git_args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(git_args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git is available in the test environment");
    assert!(status.success(), "git {git_args:?} failed");
}

/// A repository whose `.actual/rules` holds one document a diff applies to,
/// a real git history (a committed baseline plus a working-tree change, so
/// both `git diff HEAD` and an explicit `--diff-file`/stdin capture of the
/// same diff are available), an isolated config directory whose
/// `config.yaml` points telemetry at `api_url`, and a fake `CLAUDE_BINARY`
/// that answers the auth probe as logged in and every other call with one
/// all-conforming verdict batch — the same shape `GovernedPlanCheck` above
/// and `impl_check.rs`'s own unit tests use.
#[cfg(unix)]
struct GovernedImplCheck {
    repo: tempfile::TempDir,
    config_dir: tempfile::TempDir,
    fake_claude: std::path::PathBuf,
}

#[cfg(unix)]
impl GovernedImplCheck {
    /// The diff text `--diff-file`/stdin direct-mode tests pass explicitly —
    /// byte-identical in shape to what `git diff HEAD` would itself produce
    /// for the working-tree change `new` seeds, though the fake judge below
    /// answers the same canned verdict regardless of the diff's actual
    /// content.
    const DIFF: &'static str = "diff --git a/oauth.rs b/oauth.rs\nindex 0000000..1111111 100644\n--- a/oauth.rs\n+++ b/oauth.rs\n@@ -1 +1 @@\n-fn sign() {}\n+fn sign() { sign_with_rs256(); }\n";

    fn new(api_url: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;

        let repo = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init", "-q"]);
        std::fs::write(repo.path().join("oauth.rs"), "fn sign() {}\n").unwrap();
        run_git(repo.path(), &["add", "."]);
        run_git(repo.path(), &["commit", "-q", "-m", "baseline"]);
        // The working-tree change `--claude-hook` will pick up -- `oauth.rs`
        // is already tracked (committed above). Untracked new files are
        // included too (see `working_tree_diff`); this fixture still uses a
        // tracked edit so `--diff-file`/stdin tests can pass a matching
        // unified diff.
        std::fs::write(
            repo.path().join("oauth.rs"),
            "fn sign() { sign_with_rs256(); }\n",
        )
        .unwrap();

        let rules = repo.path().join(".actual/rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(
            rules.join("cross-cutting-token-signing-1c57.md"),
            "# Sign With Asymmetric Keys: Token Signing\n\nThese rules are ALWAYS ACTIVE for OAuth token signing in `services/auth/oauth/`.\n\n### Rules\n\n- **R-A-001** MUST: sign with RS256.\n- **R-A-002** MUST NOT: log the raw signing key.\n",
        )
        .unwrap();

        let config_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            config_dir.path().join("config.yaml"),
            format!("api_url: \"{api_url}\"\n"),
        )
        .unwrap();

        let envelope = r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"verdicts":[{"doc_slug":"cross-cutting-token-signing-1c57","rule_id":"R-A-001","verdict":"conforming","span":"","reason":"uses RS256"},{"doc_slug":"cross-cutting-token-signing-1c57","rule_id":"R-A-002","verdict":"conforming","span":"","reason":"no logging"}]}}"#;
        let fake_claude = config_dir.path().join("fake-claude.sh");
        std::fs::write(
            &fake_claude,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"auth\" ]; then printf '%s' '{{\"loggedIn\":true}}'; exit 0; fi\nprintf '%s' '{envelope}'\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake_claude, std::fs::Permissions::from_mode(0o755)).unwrap();

        Self {
            repo,
            config_dir,
            fake_claude,
        }
    }

    /// Direct mode with `--no-rank`, so the fake judge answers exactly one
    /// model call, and an explicit runner, so nothing probes for a real one.
    /// Callers append `--diff-file`, `--claude-hook`, or `write_stdin` as
    /// their test needs.
    fn cmd(&self) -> Command {
        let mut command = cmd();
        command
            .args([
                "impl-check",
                "--repo",
                self.repo.path().to_str().unwrap(),
                "--no-rank",
                "--runner",
                "claude-cli",
            ])
            .env("ACTUAL_CONFIG_DIR", self.config_dir.path())
            .env_remove("ACTUAL_CONFIG")
            .env_remove("ACTUAL_NO_TELEMETRY")
            .env("CLAUDE_BINARY", &self.fake_claude)
            .timeout(std::time::Duration::from_secs(60));
        command
    }
}

#[cfg(unix)]
#[test]
fn test_impl_check_direct_mode_reads_the_diff_from_diff_file() {
    let fixture = GovernedImplCheck::new("http://127.0.0.1:1");
    let diff_dir = tempfile::tempdir().unwrap();
    let diff_file = diff_dir.path().join("the.diff");
    std::fs::write(&diff_file, GovernedImplCheck::DIFF).unwrap();

    fixture
        .cmd()
        .args(["--diff-file", diff_file.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Implementation check"))
        .stdout(predicate::str::contains(
            "Conforming: no selected rule was violated.",
        ))
        .stdout(predicate::str::contains("Plan check").not());
}

#[cfg(unix)]
#[test]
fn test_impl_check_direct_mode_reads_the_diff_from_stdin() {
    let fixture = GovernedImplCheck::new("http://127.0.0.1:1");

    fixture
        .cmd()
        .write_stdin(GovernedImplCheck::DIFF)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Conforming: no selected rule was violated.",
        ));
}

/// CI, nohup, and most hook runners attach stdin to `/dev/null` — a
/// non-terminal character device, not a pipe. That must fall through to
/// `git diff HEAD` (the documented no-arg default) rather than reading EOF
/// as an empty diff and silently reporting nothing to check on a dirty tree.
/// `GovernedImplCheck` seeds a tracked working-tree change, so the fallback
/// has something to judge.
///
/// `assert_cmd` always rebinds stdin to a pipe (`Command::spawn`), so this
/// drives `std::process::Command` with `Stdio::null()` — the only way to
/// give the child a character-device stdin from a test.
#[cfg(unix)]
#[test]
fn test_impl_check_direct_mode_uses_git_diff_head_when_stdin_is_dev_null() {
    let fixture = GovernedImplCheck::new("http://127.0.0.1:1");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_actual"))
        .args([
            "impl-check",
            "--repo",
            fixture.repo.path().to_str().unwrap(),
            "--no-rank",
            "--runner",
            "claude-cli",
        ])
        .env("ACTUAL_CONFIG_DIR", fixture.config_dir.path())
        .env_remove("ACTUAL_CONFIG")
        .env_remove("ACTUAL_NO_TELEMETRY")
        .env("CLAUDE_BINARY", &fixture.fake_claude)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("impl-check should spawn");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Conforming: no selected rule was violated."),
        "expected git diff HEAD fallback, got: {stdout}"
    );
    assert!(
        !stdout.contains("Nothing to check"),
        "stdin=/dev/null must not be treated as an empty diff: {stdout}"
    );
}

/// An empty *pipe* is still an explicit diff source: the caller supplied
/// stdin, it just happened to be blank. That stays "nothing to check" even
/// when the working tree is dirty — unlike `/dev/null` above. If this fell
/// through to `git diff HEAD`, the fixture's oauth.rs change would be
/// judged and this would print Conforming instead.
#[cfg(unix)]
#[test]
fn test_impl_check_direct_mode_empty_piped_stdin_is_nothing_to_check() {
    let fixture = GovernedImplCheck::new("http://127.0.0.1:1");

    fixture
        .cmd()
        .write_stdin("   \n")
        .assert()
        .success()
        .stdout(predicate::str::contains("Nothing to check"));
}

#[cfg(unix)]
#[test]
fn test_impl_check_direct_mode_prints_nothing_to_check_on_an_empty_diff() {
    let fixture = GovernedImplCheck::new("http://127.0.0.1:1");
    let diff_dir = tempfile::tempdir().unwrap();
    let empty_diff = diff_dir.path().join("empty.diff");
    std::fs::write(&empty_diff, "   \n").unwrap();

    fixture
        .cmd()
        .args(["--diff-file", empty_diff.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Nothing to check"));
}

#[cfg(unix)]
#[test]
fn test_impl_check_claude_hook_reads_a_valid_envelope_from_stdin() {
    let fixture = GovernedImplCheck::new("http://127.0.0.1:1");

    fixture
        .cmd()
        .arg("--claude-hook")
        .write_stdin(r#"{"session_id":"sess-impl-cli-hook-1"}"#)
        .assert()
        .success();
}

/// The turn marker end to end, through the binary: a `UserPromptSubmit`
/// envelope prints nothing (that hook's stdout lands in the prompt), and the
/// check that ends the turn still finds the work after it is all committed.
/// A session that never recorded a turn start sees the same clean tree as
/// nothing to check.
#[cfg(unix)]
#[test]
fn test_impl_check_claude_hook_user_prompt_submit_lets_the_stop_check_see_committed_work() {
    let fixture = GovernedImplCheck::new("http://127.0.0.1:1");
    let prompt = r#"{"session_id":"sess-impl-cli-turn-1","hook_event_name":"UserPromptSubmit","prompt":"sign the tokens"}"#;
    let stop = r#"{"session_id":"sess-impl-cli-turn-1","hook_event_name":"Stop"}"#;
    let unmarked = r#"{"session_id":"sess-impl-cli-turn-2","hook_event_name":"Stop"}"#;

    fixture
        .cmd()
        .arg("--claude-hook")
        .write_stdin(prompt)
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    run_git(fixture.repo.path(), &["add", "."]);
    run_git(
        fixture.repo.path(),
        &["commit", "-q", "-m", "the turn's work"],
    );

    fixture
        .cmd()
        .arg("--claude-hook")
        .write_stdin(stop)
        .assert()
        .success()
        .stdout(predicate::str::contains("no diff to check").not());
    fixture
        .cmd()
        .arg("--claude-hook")
        .write_stdin(unmarked)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "working tree matches HEAD and there are no untracked files",
        ));

    fixture
        .cmd()
        .arg("--claude-hook")
        .write_stdin(prompt)
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
    fixture
        .cmd()
        .arg("--claude-hook")
        .write_stdin(stop)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "nothing has changed since this turn started",
        ));
}

/// `exec_hook`'s own stdin-read-failure branch, `impl-check`'s side: invalid
/// UTF-8 must fail open with a notice, never block or error the hook.
#[cfg(unix)]
#[test]
fn test_impl_check_claude_hook_fails_open_on_non_utf8_stdin() {
    let fixture = GovernedImplCheck::new("http://127.0.0.1:1");

    fixture
        .cmd()
        .arg("--claude-hook")
        .write_stdin(vec![0xffu8, 0xfe, 0x00, 0x41])
        .assert()
        .success()
        .stdout(predicate::str::contains("could not read the hook payload"));
}

/// An oversized envelope is refused rather than buffered in full, same as
/// `plan-check --claude-hook`.
#[cfg(unix)]
#[test]
fn test_impl_check_claude_hook_fails_open_on_oversized_stdin() {
    let fixture = GovernedImplCheck::new("http://127.0.0.1:1");

    fixture
        .cmd()
        .arg("--claude-hook")
        .write_stdin(vec![b'x'; MAX_READ_BYTES + 1])
        .assert()
        .success()
        .stdout(predicate::str::contains("could not read the hook payload"));
}

/// The dispatcher's whole job, `impl-check`'s side: the started and
/// completed events for this run reach `POST /plan-governance/record`,
/// tagged `command":"impl-check"` -- distinct from `plan-check`'s own
/// `"command":"plan-check"`, so the two commands' governance events are
/// never confused for one another in the same analytics stream.
#[cfg(unix)]
#[test]
fn test_impl_check_verdict_sends_governance_events_to_the_configured_api() {
    let mut server = mockito::Server::new();
    let recorded = server
        .mock("POST", "/plan-governance/record")
        .match_header(
            "authorization",
            mockito::Matcher::Regex("^Bearer .+".to_string()),
        )
        .match_body(mockito::Matcher::AllOf(vec![
            mockito::Matcher::Regex(r#""event":"plan_governance_check_started""#.to_string()),
            mockito::Matcher::Regex(r#""event":"plan_governance_check_completed""#.to_string()),
            mockito::Matcher::Regex(r#""command":"impl-check""#.to_string()),
            mockito::Matcher::Regex(r#""decision":"allow""#.to_string()),
        ]))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"recorded":2,"failed":0}"#)
        .expect(1)
        .create();

    let fixture = GovernedImplCheck::new(&server.url());
    fixture
        .cmd()
        .write_stdin(GovernedImplCheck::DIFF)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Conforming: no selected rule was violated.",
        ));

    wait_for_the_uploader(&recorded);
    recorded.assert();
}

/// AK-678's acceptance criterion, `impl-check`'s side: nothing listens on
/// loopback port 1, so the send fails on connect, and the run still prints
/// its verdict and exits on the verdict alone.
#[cfg(unix)]
#[test]
fn test_impl_check_verdict_survives_a_refused_telemetry_endpoint() {
    let fixture = GovernedImplCheck::new("http://127.0.0.1:1");
    fixture
        .cmd()
        .write_stdin(GovernedImplCheck::DIFF)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Conforming: no selected rule was violated.",
        ));

    assert!(fixture.config_dir.path().join("telemetry-id").is_file());
}

/// The ticket's cross-command acceptance criterion: one override mechanism
/// (`check-override`) serves both `plan-check` and `impl-check`, because
/// `GovernanceSession` is keyed generically on `(session_id, rules_dir)`
/// so one override file serves both gates (see `governance_session`'s own module
/// doc). This drives a real `impl-check --claude-hook` denial through the
/// compiled binary, then confirms `check-override`, given the exact session
/// id and rule key `impl-check` just denied, is gated by exactly the same
/// interactive-terminal check `plan-check`-originated sessions get above --
/// proving the two commands share the one override command, not two
/// independent ones.
///
/// The override actually *clearing* the denial cannot be driven through this
/// black-box subprocess harness: `check-override` refuses to run at all
/// without a real terminal attached (see `exec_override`'s own doc comment),
/// and `assert_cmd::Command` only ever gives a subprocess piped, non-terminal
/// stdin -- there is no way to allocate a pty from here. That end-to-end
/// clearing behavior (deny -> override -> re-check no longer blocks) is
/// exercised instead at the library level, in
/// `impl_check::tests::test_exec_hook_with_honors_an_override_recorded_via_check_override`,
/// which calls the same `governance_session::record_override` function
/// `check-override`'s own implementation calls, bypassing only the
/// TTY gate this harness cannot satisfy either way.
#[cfg(unix)]
#[test]
fn test_check_override_is_gated_the_same_way_for_a_session_impl_check_denied() {
    let fixture = GovernedImplCheck::new("http://127.0.0.1:1");
    let rules = fixture.repo.path().join(".actual/rules");
    // A conflicting verdict this time, so the hook run actually denies
    // rather than passing silently.
    let deny_envelope = r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"verdicts":[{"doc_slug":"cross-cutting-token-signing-1c57","rule_id":"R-A-001","verdict":"conforming","span":"","reason":"uses RS256"},{"doc_slug":"cross-cutting-token-signing-1c57","rule_id":"R-A-002","verdict":"conflicting","span":"logs the key","reason":"forbidden"}]}}"#;
    std::fs::write(
        &fixture.fake_claude,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"auth\" ]; then printf '%s' '{{\"loggedIn\":true}}'; exit 0; fi\nprintf '%s' '{deny_envelope}'\n"
        ),
    )
    .unwrap();

    let session_id = "sess-impl-cli-override-1";
    fixture
        .cmd()
        .arg("--claude-hook")
        .write_stdin(format!(r#"{{"session_id":"{session_id}"}}"#))
        .assert()
        .success()
        .stdout(predicate::str::contains("\"permissionDecision\":\"deny\""));

    // The same session id and rule key impl-check's own deny message names,
    // handed to check-override -- refused purely for lack of a terminal,
    // the identical gate `test_check_override_refuses_without_a_terminal`
    // exercises for a plan-check-originated session.
    cmd()
        .args([
            "check-override",
            "--session",
            session_id,
            "--rule",
            "cross-cutting-token-signing-1c57::R-A-002",
            "--reason",
            "reviewed and accepted",
            "--rules-dir",
            rules.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("interactively"));
}

// ── scope telemetry spool (APR-004) ────────────────────────────────────────
//
// These drive the real `actual` binary through the process boundary and observe
// delivery only through the two supported boundaries — the durable spool file
// and the HTTP proxy — never through internals. The invariant under test: an
// opted-in scope event is either delivered or durably retained, while the
// command exits promptly regardless of the endpoint's health.

/// The scope-telemetry spool file inside a config dir (mirrors
/// `telemetry::spool::SPOOL_FILE`).
#[cfg(feature = "telemetry")]
fn scope_spool(config_dir: &std::path::Path) -> std::path::PathBuf {
    config_dir.join("scope-telemetry-spool.jsonl")
}

/// A throwaway repo with one rule document, so `rules select` has something to
/// scan and emits a scope event.
#[cfg(feature = "telemetry")]
fn repo_with_a_rule() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    let rules = repo.path().join(".actual").join("rules");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(
        rules.join("cross-cutting-signing-a1b2.md"),
        "# Adopt RS256: Token Signing\n\nThese rules are ALWAYS ACTIVE for token signing in \
         `services/auth/`.\n\n### Rules\n\n- **R-A-001** MUST: sign with RS256.\n",
    )
    .unwrap();
    repo
}

/// Run `actual rules select --no-rank` against `api_url`, opted in, with an
/// isolated config dir. Returns how long the command took.
#[cfg(feature = "telemetry")]
fn run_rules_select(
    config_dir: &std::path::Path,
    repo: &std::path::Path,
    api_url: &str,
) -> std::time::Duration {
    std::fs::write(
        config_dir.join("config.yaml"),
        format!("api_url: {api_url}\n"),
    )
    .unwrap();
    let started = std::time::Instant::now();
    cmd()
        .args([
            "rules",
            "select",
            "rotate the signing key",
            "--repo",
            repo.to_str().unwrap(),
            "--no-rank",
        ])
        .env("ACTUAL_CONFIG_DIR", config_dir)
        .env_remove("ACTUAL_NO_TELEMETRY")
        .assert()
        .success();
    started.elapsed()
}

/// A slow/unreachable endpoint: `rules select` still exits promptly, and the
/// event is durably queued in the spool rather than lost when the process exits.
#[cfg(feature = "telemetry")]
#[test]
fn test_rules_select_durably_spools_scope_event_when_endpoint_is_slow() {
    let config_dir = tempfile::tempdir().unwrap();
    let repo = repo_with_a_rule();

    // Bind but never respond: the kernel completes the handshake from the
    // backlog, so the uploader connects and then waits out its own timeout.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let elapsed = run_rules_select(config_dir.path(), repo.path(), &format!("http://{addr}"));

    // Prompt exit: the command never waits on the telemetry endpoint. Generous
    // to stay robust on slow CI while still catching a true blocking hang.
    assert!(
        elapsed < std::time::Duration::from_secs(15),
        "rules select took {elapsed:?}"
    );

    // Durably retained: the event survived process exit on disk. Delivery could
    // not complete against the silent endpoint, so it must still be queued.
    let spool = scope_spool(config_dir.path());
    let contents = std::fs::read_to_string(&spool).expect("spool file must exist");
    assert!(contents.contains("scope_select"), "{contents}");
    assert!(contents.contains("rules_scanned"), "{contents}");
    // Privacy: the plan text never reaches the durable queue.
    assert!(
        !contents.contains("rotate the signing key"),
        "raw plan text must never be spooled: {contents}"
    );

    drop(listener);
}

/// With a healthy endpoint the spooled event is eventually delivered by the
/// uploader that outlives the command, and drained from the spool.
#[cfg(feature = "telemetry")]
#[test]
fn test_rules_select_scope_event_is_delivered_when_endpoint_is_up() {
    let config_dir = tempfile::tempdir().unwrap();
    let repo = repo_with_a_rule();

    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/plan-governance/record")
        .with_status(200)
        .with_body(r#"{"recorded":1,"failed":0}"#)
        .expect_at_least(1)
        .create();

    run_rules_select(config_dir.path(), repo.path(), &server.url());

    // The uploader is a separate process that outlives the command; poll until
    // it has delivered and drained the spool (or a generous timeout).
    let spool = scope_spool(config_dir.path());
    let mut drained = false;
    for _ in 0..100 {
        let queued = std::fs::read_to_string(&spool).unwrap_or_default();
        if queued.trim().is_empty() {
            drained = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    mock.assert();
    assert!(drained, "the spool must drain once the event is delivered");
}

/// The uploader outlives the command that started it. The endpoint holds its
/// reply for a second, so the reply can only reach a process that is still
/// running after `rules select` has exited, and only that process drains the
/// spool.
#[cfg(feature = "telemetry")]
#[test]
fn test_rules_select_uploader_outlives_the_command() {
    let config_dir = tempfile::tempdir().unwrap();
    let repo = repo_with_a_rule();

    let replied_at = std::sync::Arc::new(std::sync::Mutex::new(None));
    let stamp = std::sync::Arc::clone(&replied_at);
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/plan-governance/record")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_chunked_body(move |w| {
            std::thread::sleep(std::time::Duration::from_secs(1));
            *stamp.lock().unwrap() = Some(std::time::Instant::now());
            std::io::Write::write_all(w, br#"{"recorded":1,"failed":0}"#)
        })
        .expect(1)
        .create();

    run_rules_select(config_dir.path(), repo.path(), &server.url());
    let exited_at = std::time::Instant::now();

    let spool = scope_spool(config_dir.path());
    let mut drained = false;
    for _ in 0..100 {
        if std::fs::read_to_string(&spool)
            .unwrap_or_default()
            .trim()
            .is_empty()
        {
            drained = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    mock.assert();
    assert!(drained, "the uploader must deliver and drain the spool");
    let replied_at = replied_at
        .lock()
        .unwrap()
        .expect("the endpoint should have replied");
    assert!(
        replied_at > exited_at,
        "the reply should land after the command exited"
    );
}

/// Opted out: no event is written to the spool, and nothing is sent.
#[cfg(feature = "telemetry")]
#[test]
fn test_rules_select_does_not_spool_when_opted_out() {
    let config_dir = tempfile::tempdir().unwrap();
    let repo = repo_with_a_rule();

    std::fs::write(
        config_dir.path().join("config.yaml"),
        "api_url: http://127.0.0.1:1\n",
    )
    .unwrap();
    cmd()
        .args([
            "rules",
            "select",
            "rotate the signing key",
            "--repo",
            repo.path().to_str().unwrap(),
            "--no-rank",
        ])
        .env("ACTUAL_CONFIG_DIR", config_dir.path())
        .env("ACTUAL_NO_TELEMETRY", "1")
        .assert()
        .success();

    assert!(
        !scope_spool(config_dir.path()).exists(),
        "an opted-out run must not write the spool"
    );
}

/// A refused endpoint: delivery fails, but the event is retained in the spool
/// for a later flush rather than dropped.
#[cfg(feature = "telemetry")]
#[test]
fn test_rules_select_retains_spool_on_failed_delivery() {
    let config_dir = tempfile::tempdir().unwrap();
    let repo = repo_with_a_rule();

    // Port 1 refuses immediately, so the uploader fails fast without delivering.
    run_rules_select(config_dir.path(), repo.path(), "http://127.0.0.1:1");

    // Give the uploader a moment to attempt and fail, then confirm retention.
    let spool = scope_spool(config_dir.path());
    let mut retained = false;
    for _ in 0..30 {
        if std::fs::read_to_string(&spool)
            .unwrap_or_default()
            .contains("scope_select")
        {
            retained = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(retained, "a failed delivery must keep the event queued");
}

// ── session summary ──────────────────────────────────────────────────────

/// A repository whose rule set holds one decision that can be added to
/// context.
fn governed_repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    let rules = repo.path().join(".actual").join("rules");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(
        rules.join("cross-cutting-signing-a1b2.md"),
        "# Adopt RS256: Token Signing\n\nThese rules are ALWAYS ACTIVE for token signing in \
         `services/auth/`.\n\n### Rules\n\n- **R-A-001** MUST: sign with RS256.\n\n\
         ### Verify\n\n```bash\ngrep -r \"jwt.sign\" services/auth/ --include=\"*.ts\"\n```\n",
    )
    .unwrap();
    repo
}

/// `actual <args>` run from `dir`, the way `impl-gate.sh` runs it after its
/// `cd`, with `config_dir` holding the session records.
fn actual_in(dir: &std::path::Path, config_dir: &std::path::Path, args: &[&str]) -> Command {
    let mut command = cmd();
    command
        .args(args)
        .current_dir(dir)
        .env("ACTUAL_CONFIG_DIR", config_dir)
        .env_remove("ACTUAL_CONFIG")
        .env("ACTUAL_NO_TELEMETRY", "1");
    command
}

/// The read hook's stdout when the agent reads `file` of `repo` in session
/// `s1`, after asserting it exited 0.
fn hook_read(repo: &std::path::Path, config_dir: &std::path::Path, file: &str) -> String {
    let read = serde_json::json!({
        "session_id": "s1",
        "hook_event_name": "PostToolUse",
        "tool_name": "Read",
        "tool_input": {"file_path": repo.join(file)},
    })
    .to_string();
    let output = actual_in(
        repo,
        config_dir,
        &[
            "rules",
            "brief",
            "--claude-hook",
            "--repo",
            repo.to_str().unwrap(),
        ],
    )
    .write_stdin(read)
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    String::from_utf8(output).unwrap()
}

/// What the read hook does in session `s1` when the agent reads a governed
/// file of `repo`: the decision is briefed and recorded.
fn brief_a_read(repo: &std::path::Path, config_dir: &std::path::Path) {
    let reply = hook_read(repo, config_dir, "services/auth/token.ts");
    assert!(reply.contains("R-A-001"), "{reply}");
}

/// The summary hook's stdout for `envelope`, run from `dir` with `flags`,
/// after asserting it exited 0.
fn summary_hook(
    dir: &std::path::Path,
    config_dir: &std::path::Path,
    flags: &[&str],
    envelope: &str,
) -> String {
    let args = [&["session", "summary", "--claude-hook"], flags].concat();
    let output = actual_in(dir, config_dir, &args)
        .write_stdin(envelope.to_string())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(output).unwrap()
}

fn stop_envelope(cwd: &std::path::Path) -> String {
    serde_json::json!({
        "session_id": "s1",
        "cwd": cwd,
        "hook_event_name": "Stop",
        "stop_hook_active": false,
    })
    .to_string()
}

/// The path the two hooks drive together, with the `--rules-dir` that
/// `impl-gate.sh` passes: a read of a governed file is briefed and recorded,
/// the end of that response shows the count, and the next response, with
/// nothing new, is silent.
#[test]
fn test_session_summary_hook_reports_a_briefed_decision_once() {
    let repo = governed_repo();
    let config_dir = tempfile::tempdir().unwrap();
    let rules_dir = repo.path().join(".actual").join("rules");
    let flags = ["--rules-dir", rules_dir.to_str().unwrap()];
    brief_a_read(repo.path(), config_dir.path());

    let stop = stop_envelope(repo.path());
    assert_eq!(
        summary_hook(repo.path(), config_dir.path(), &flags, &stop),
        "Actual AI: 1 of 1 ADR added to context\n"
    );
    assert_eq!(
        summary_hook(repo.path(), config_dir.path(), &flags, &stop),
        ""
    );
}

/// The envelope's `cwd` follows the agent's `cd`, so the summary keys on the
/// directory it runs in. Run from a repository with nothing briefed, it is
/// silent even though the envelope names another repository whose record
/// would print; run from that repository, the same envelope prints.
#[test]
fn test_session_summary_hook_keys_on_its_own_directory_not_the_envelopes_cwd() {
    let briefed = governed_repo();
    let other = governed_repo();
    let config_dir = tempfile::tempdir().unwrap();
    brief_a_read(briefed.path(), config_dir.path());
    let stop = stop_envelope(briefed.path());

    assert_eq!(
        summary_hook(other.path(), config_dir.path(), &[], &stop),
        ""
    );
    assert_eq!(
        summary_hook(briefed.path(), config_dir.path(), &[], &stop),
        "Actual AI: 1 of 1 ADR added to context\n"
    );
}

/// Nothing to say is empty stdout and exit 0: a session that briefed
/// nothing, an envelope that is not JSON, and a repository with no rules.
#[test]
fn test_session_summary_hook_is_silent_with_nothing_to_say() {
    let repo = governed_repo();
    let bare = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();

    let stop = stop_envelope(repo.path());
    assert_eq!(summary_hook(repo.path(), config_dir.path(), &[], &stop), "");
    assert_eq!(
        summary_hook(repo.path(), config_dir.path(), &[], "{ not json"),
        ""
    );
    brief_a_read(repo.path(), config_dir.path());
    assert_eq!(summary_hook(bare.path(), config_dir.path(), &[], &stop), "");
}

/// Direct mode always prints the current counts, here with nothing added
/// yet, and `--json` carries the same numbers.
#[test]
fn test_session_summary_direct_mode_prints_the_current_counts() {
    let repo = governed_repo();
    let config_dir = tempfile::tempdir().unwrap();
    actual_in(
        repo.path(),
        config_dir.path(),
        &["session", "summary", "--session", "s1"],
    )
    .assert()
    .success()
    .stdout("Actual AI: 0 of 1 ADR added to context\n");

    let output = actual_in(
        repo.path(),
        config_dir.path(),
        &["session", "summary", "--session", "s1", "--json"],
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "x": 0,
            "y": 1,
            "decisions": [],
            "gaps": 0,
            "areas_without_adr": [],
            "line": "Actual AI: 0 of 1 ADR added to context"
        })
    );
}

/// A read of a file no decision governs leaves its folder a gap, which the
/// summary hook reports with no decision added, then beside the count once
/// one is. `--json` names the folder.
#[test]
fn test_session_summary_hook_reports_the_areas_with_no_adr() {
    let repo = governed_repo();
    let config_dir = tempfile::tempdir().unwrap();
    let stop = stop_envelope(repo.path());

    assert_eq!(
        hook_read(repo.path(), config_dir.path(), "infra/terraform/main.tf"),
        ""
    );
    assert_eq!(
        summary_hook(repo.path(), config_dir.path(), &[], &stop),
        "Actual AI: no ADR covers the area you worked in\n"
    );

    brief_a_read(repo.path(), config_dir.path());
    assert_eq!(
        summary_hook(repo.path(), config_dir.path(), &[], &stop),
        "Actual AI: 1 of 1 ADR added to context · 1 area with no ADR\n"
    );
    assert_eq!(summary_hook(repo.path(), config_dir.path(), &[], &stop), "");

    let output = actual_in(
        repo.path(),
        config_dir.path(),
        &["session", "summary", "--session", "s1", "--json"],
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["gaps"], 1);
    assert_eq!(
        value["areas_without_adr"],
        serde_json::json!(["infra/terraform"])
    );
}

#[test]
fn test_session_summary_needs_a_session_or_the_hook() {
    cmd()
        .args(["session", "summary"])
        .assert()
        .failure()
        .code(2);
}

/// The read hook and the plan hook, run as `plan-gate.sh` runs the second,
/// add to one set of folders. The folder read and then planned is one gap,
/// the planned `scripts/` is a second, and the folder of the plan's governed
/// file is none. No runner is available, so the plan is never judged.
#[test]
fn test_session_summary_counts_the_folders_a_plan_names() {
    let repo = governed_repo();
    let config_dir = tempfile::tempdir().unwrap();
    for file in [
        "infra/terraform/main.tf",
        "scripts/deploy.sh",
        "services/auth/token.ts",
    ] {
        let path = repo.path().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }
    assert_eq!(
        hook_read(repo.path(), config_dir.path(), "infra/terraform/main.tf"),
        ""
    );

    let exit_plan_mode = serde_json::json!({
        "session_id": "s1",
        "hook_event_name": "PreToolUse",
        "tool_name": "ExitPlanMode",
        "tool_input": {"plan": "1. Pin the provider in `infra/terraform/main.tf` and \
            `infra/terraform/versions.tf`.\n2. Run it from `scripts/deploy.sh`.\n3. Sign \
            the deploy token in `services/auth/token.ts`."},
    })
    .to_string();
    let rules_dir = repo.path().join(".actual").join("rules");
    actual_in(
        repo.path(),
        config_dir.path(),
        &[
            "plan-check",
            "--claude-hook",
            "--rules-dir",
            rules_dir.to_str().unwrap(),
            "--runner",
            "claude-cli",
        ],
    )
    .env("CLAUDE_BINARY", "/nonexistent/path/to/claude")
    .write_stdin(exit_plan_mode)
    .assert()
    .success()
    .stdout(predicate::str::contains("no runner available"));

    let output = actual_in(
        repo.path(),
        config_dir.path(),
        &["session", "summary", "--session", "s1", "--json"],
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["gaps"], 2);
    assert_eq!(
        value["areas_without_adr"],
        serde_json::json!(["infra/terraform", "scripts"])
    );
}

/// Direct mode's `--json` reports the folder pass the hook runs before its
/// judge, and notes nothing, since there is no session to note it in.
#[test]
fn test_plan_check_json_reports_the_folder_pass() {
    let repo = governed_repo();
    let config_dir = tempfile::tempdir().unwrap();
    for file in ["scripts/deploy.sh", "services/auth/token.ts"] {
        let path = repo.path().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    let output = actual_in(
        repo.path(),
        config_dir.path(),
        &[
            "plan-check",
            "--json",
            "--no-rank",
            "--runner",
            "claude-cli",
            "--repo",
            repo.path().to_str().unwrap(),
            "Run `scripts/deploy.sh` and sign the token in `services/auth/token.ts`.",
        ],
    )
    .env("CLAUDE_BINARY", "/nonexistent/path/to/claude")
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["status"], "not_checked");
    assert_eq!(
        value["folders"]["covered"],
        serde_json::json!(["services/auth"])
    );
    assert_eq!(value["folders"]["gaps"], serde_json::json!(["scripts"]));
    assert_eq!(value["folders"]["undecided"], serde_json::json!([]));
    assert_eq!(value["folders"]["files_searched"], 2);
    assert!(value["folders"]["elapsed_ms"].as_f64().unwrap() > 0.0);
    assert!(!config_dir.path().join("brief-ledger").exists());
}

/// Run from a subfolder the agent changed into, a hook given the conventional
/// `--rules-dir` and no `--repo` still places files from the repository root:
/// a governed file is briefed under its root-relative path, and a file outside
/// the subfolder is `ungoverned` rather than `outside-root`.
#[test]
fn test_rules_brief_takes_the_root_from_a_conventional_rules_dir() {
    let repo = governed_repo();
    let config_dir = tempfile::tempdir().unwrap();
    let rules_dir = repo.path().join(".actual").join("rules");
    let subfolder = repo.path().join("services");
    std::fs::create_dir_all(&subfolder).unwrap();
    let brief = |file: &str| {
        let read = serde_json::json!({
            "session_id": "s1",
            "cwd": subfolder,
            "hook_event_name": "PostToolUse",
            "tool_name": "Read",
            "tool_input": {"file_path": repo.path().join(file)},
        })
        .to_string();
        actual_in(
            &subfolder,
            config_dir.path(),
            &[
                "rules",
                "brief",
                "--claude-hook",
                "--rules-dir",
                rules_dir.to_str().unwrap(),
            ],
        )
        .write_stdin(read)
        .assert()
        .success()
    };

    brief("services/auth/token.ts").stdout(predicate::str::contains("`services/auth/token.ts`"));
    brief("infra/main.tf")
        .stdout("")
        .stderr(predicate::str::contains(
            "no-brief=ungoverned infra/main.tf",
        ));
}

/// A repository with one v1 rule file and two v2 ones, one of them superseded.
fn mixed_format_repo() -> tempfile::TempDir {
    let repo = governed_repo();
    let rules = repo.path().join(".actual").join("rules");
    let v2 = |status: &str| {
        format!(
            "---\nversion: 2\nname: web-1a2b\ndescription: \"Web Rules: Components\"\nmetadata:\n  actual:\n    adr_id: adr-web\n    status: {status}\n  scope:\n    root: packages/web\n    paths:\n      - \"src/**\"\n      - \"!src/**/*.test.ts\"\n---\n\n# Web Rules: Components\n\nScope: applies to `packages/web/src/**`.\n\n### Rules\n\n- **R-W-001** MUST: keep components presentational.\n"
        )
    };
    std::fs::write(rules.join("web-1a2b.md"), v2("accepted")).unwrap();
    std::fs::write(
        rules.join("old-3c4d.md"),
        v2("superseded")
            .replace("web-1a2b", "old-3c4d")
            .replace("adr-web", "adr-old"),
    )
    .unwrap();
    repo
}

/// `rules ls --json` carries the v2 header's fields for a v2 file and nothing
/// new for a v1 file, and `rules index --json` says how many files were left
/// out as inactive.
#[test]
fn test_rules_ls_and_index_json_show_the_v2_header_across_a_mixed_repo() {
    let repo = mixed_format_repo();
    let config_dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| -> serde_json::Value {
        let out = actual_in(repo.path(), config_dir.path(), args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&out).expect("json")
    };

    let ls = run(&["rules", "ls", "--json"]);
    let docs = ls["documents"].as_array().unwrap();
    let by_slug = |slug: &str| docs.iter().find(|d| d["slug"] == slug).unwrap();
    let v1 = by_slug("cross-cutting-signing-a1b2");
    assert!(v1.get("adr_id").is_none() && v1.get("root").is_none() && v1.get("status").is_none());
    let v2 = by_slug("web-1a2b");
    assert_eq!(v2["adr_id"], "adr-web");
    assert_eq!(v2["root"], "packages/web");
    assert_eq!(v2["status"], "accepted");
    // The frontmatter did not leak into the scope sentence.
    assert!(!v2["scope"]
        .as_str()
        .expect("scope is a string")
        .contains("adr_id"));
    assert_eq!(by_slug("old-3c4d")["status"], "superseded");

    let index = run(&["rules", "index", "--json"]);
    assert_eq!(index["documents"], 2);
    assert_eq!(index["inactive_documents"], 1);
    assert_eq!(index["documents_with_declared_paths"], 1);
}

/// The Actual Skill probes for ADR header v2 support by grepping this literal in
/// `rules brief --help` (`have_adr_v2`, AK-963). It is a contract with that
/// repository: changing it here without changing the probe there silently turns
/// v2 support off for every user, so it is pinned.
#[test]
fn test_rules_brief_help_carries_the_adr_header_v2_capability_marker() {
    cmd()
        .args(["rules", "brief", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("adr-header-v2"));
}

/// The marker is absent from the sibling commands, so the probe is about this
/// one help text and not a string that happens to appear everywhere.
#[test]
fn test_the_adr_header_v2_marker_is_specific_to_rules_brief() {
    let others: [&[&str]; 2] = [&["rules", "ls", "--help"], &["impl-check", "--help"]];
    for args in others {
        cmd()
            .args(args)
            .assert()
            .success()
            .stdout(predicate::str::contains("adr-header-v2").not());
    }
}
