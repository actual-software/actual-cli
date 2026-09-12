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
// `dispatch_plan_governance_events` is `cfg(all(feature = "telemetry",
// not(test)))`: the `--lib` build compiles its capturing twin instead, so no
// unit test can execute the function that actually loads config, builds a
// runtime and calls `send_events`. Only the compiled binary carries it, and
// every other `plan-check` case in this file stops at `NothingApplies`
// before an event is built. These two seed a rules directory and a fake
// judge so the pipeline reaches a verdict, and route `api_url` to loopback
// so the dispatcher runs without a network.

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
}

/// The dispatcher's whole job: the started and completed events for this
/// run reach `POST /plan-governance/record` at the configured `api_url`.
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
