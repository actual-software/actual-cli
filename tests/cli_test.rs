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
// `dispatch_governance_events` (in `cli::commands::check_engine`) is `cfg(all(feature = "telemetry",
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
        .stdout(predicate::str::contains(
            "Conforming: no selected rule was violated.",
        ));
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
