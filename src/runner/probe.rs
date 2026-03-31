//! Runner availability probing infrastructure.
//!
//! This module centralises availability checks for all supported runner
//! backends.  It is the single place to ask "is this runner available and
//! authenticated on this machine?", and is used by environment-aware
//! runner auto-detection (b1q.3).

use std::path::Path;
use std::time::Duration;

use crate::error::ActualError;
use crate::runner::auth::ClaudeAuthStatus;

// ── Re-exports ────────────────────────────────────────────────────────────────

/// Re-export for single-import convenience.
pub use crate::runner::binary::find_claude_binary;
/// Re-export for single-import convenience.
pub use crate::runner::codex_cli::find_codex_binary;
/// Re-export for single-import convenience.
pub use crate::runner::cursor_cli::find_cursor_binary;

// ── Claude auth probe ─────────────────────────────────────────────────────────

/// Convert a tokio runtime build error into an [`ActualError`].
fn runtime_build_error(e: std::io::Error) -> ActualError {
    ActualError::RunnerFailed {
        message: format!("failed to build tokio runtime: {e}"),
        stderr: String::new(),
    }
}

/// Convert a `wait_with_output` I/O error into an [`ActualError`].
fn wait_io_error(e: std::io::Error) -> ActualError {
    ActualError::RunnerFailed {
        message: format!("failed to wait for claude: {e}"),
        stderr: String::new(),
    }
}

/// Check Claude Code authentication by spawning `claude auth status --json`.
///
/// Blocks the calling thread using a single-threaded tokio runtime.
/// The subprocess is killed when the timeout expires.
pub fn probe_claude_auth(
    binary_path: &Path,
    timeout: Duration,
) -> Result<ClaudeAuthStatus, ActualError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(runtime_build_error)?;
    rt.block_on(probe_claude_auth_async(binary_path, timeout))
}

/// Async inner for [`probe_claude_auth`].
///
/// Spawns `claude auth status --json`, waits up to `timeout`, and deserialises
/// the JSON response into a [`ClaudeAuthStatus`].  The child process is killed
/// when the timeout fires (`kill_on_drop(true)`).
pub(crate) async fn probe_claude_auth_async(
    binary_path: &Path,
    timeout: Duration,
) -> Result<ClaudeAuthStatus, ActualError> {
    let mut cmd = tokio::process::Command::new(binary_path);
    cmd.args(["auth", "status", "--json"]);
    cmd.stdin(std::process::Stdio::null());
    cmd.kill_on_drop(true);

    let child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| ActualError::RunnerFailed {
            message: format!("failed to spawn claude: {e}"),
            stderr: String::new(),
        })?;

    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| ActualError::RunnerTimeout {
            // Round up so sub-second timeouts display as "1s" rather than "0s".
            seconds: timeout.as_secs_f64().ceil() as u64,
        })?
        .map_err(wait_io_error)?;

    if !output.status.success() {
        // Some claude CLI versions exit non-zero even when the user is
        // authenticated (e.g. exit 1 on certain subscription types or OS
        // environments).  Attempt to parse the JSON stdout first; if it
        // contains `loggedIn: true` we trust that over the exit code.
        if let Ok(status) = serde_json::from_slice::<ClaudeAuthStatus>(&output.stdout) {
            if status.logged_in {
                return Ok(status);
            }
        }

        let stderr = String::from_utf8_lossy(&output.stderr);

        // Older versions (e.g. ≤2.1.25) don't recognise --json at all.
        // Retry without the flag so those users aren't blocked.
        if json_flag_not_recognized(&stderr, &output.stdout) {
            return probe_claude_auth_async_no_json(binary_path, timeout).await;
        }

        return Err(ActualError::RunnerFailed {
            message: format!(
                "claude auth status exited with code {}",
                output.status.code().unwrap_or(-1)
            ),
            stderr: stderr.into_owned(),
        });
    }

    let status: ClaudeAuthStatus = serde_json::from_slice(&output.stdout)?;
    Ok(status)
}

/// Returns `true` when the subprocess output suggests that `--json` is not a
/// recognised flag in this version of the Claude CLI.
fn json_flag_not_recognized(stderr: &str, stdout: &[u8]) -> bool {
    // The exact wording varies ("unknown option '--json'", "unknown flag", …)
    // but the flag name always appears in the error text.
    stderr.contains("--json") || String::from_utf8_lossy(stdout).contains("--json")
}

/// Fallback probe for Claude CLI versions that do not support `--json`.
///
/// Runs `claude auth status` without the flag.  Newer versions already emit
/// JSON without being asked; for older ones that produce plain text we treat
/// exit code 0 as "authenticated".
async fn probe_claude_auth_async_no_json(
    binary_path: &Path,
    timeout: Duration,
) -> Result<ClaudeAuthStatus, ActualError> {
    let mut cmd = tokio::process::Command::new(binary_path);
    cmd.args(["auth", "status"]);
    cmd.stdin(std::process::Stdio::null());
    cmd.kill_on_drop(true);

    let child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| ActualError::RunnerFailed {
            message: format!("failed to spawn claude: {e}"),
            stderr: String::new(),
        })?;

    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| ActualError::RunnerTimeout {
            seconds: timeout.as_secs_f64().ceil() as u64,
        })?
        .map_err(wait_io_error)?;

    // Some versions output valid JSON even without the flag.
    if let Ok(status) = serde_json::from_slice::<ClaudeAuthStatus>(&output.stdout) {
        return Ok(status);
    }

    // Plain-text output: exit code 0 means the auth check passed.
    if output.status.success() {
        return Ok(ClaudeAuthStatus::minimal_authenticated());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Err(ActualError::RunnerFailed {
        message: format!(
            "claude auth status exited with code {}",
            output.status.code().unwrap_or(-1)
        ),
        stderr,
    })
}

// ── Codex auth probe ──────────────────────────────────────────────────────────

/// Check whether Codex CLI authentication is available.
///
/// Returns `Ok(())` if any auth source is detected:
/// 1. `api_key` is `Some` and non-empty
/// 2. `~/.codex/auth.json` exists (indicates `codex login` was run)
///
/// Returns `Err(ActualError::CodexNotAuthenticated)` if none are found.
pub fn probe_codex_auth(api_key: Option<&str>) -> Result<(), ActualError> {
    let auth_path = dirs::home_dir().map(|h| h.join(".codex").join("auth.json"));
    probe_codex_auth_inner(api_key, auth_path.as_deref())
}

/// Inner implementation of [`probe_codex_auth`] with injectable auth path for
/// testability.
pub(crate) fn probe_codex_auth_inner(
    api_key: Option<&str>,
    auth_path: Option<&Path>,
) -> Result<(), ActualError> {
    crate::runner::codex_cli::check_codex_auth_inner(api_key, auth_path)
}

// ── Cursor auth probe ─────────────────────────────────────────────────────────

/// Check whether Cursor CLI authentication is available.
///
/// Returns `Ok(())` if any auth source is detected:
/// 1. `api_key` is `Some` and non-empty
/// 2. `~/.cursor/auth.json` exists (indicates `cursor-agent login` was run)
/// 3. `CURSOR_API_KEY` environment variable is set and non-empty
///
/// Returns `Err(ActualError::CursorNotAuthenticated)` if none are found.
pub fn probe_cursor_auth(api_key: Option<&str>) -> Result<(), ActualError> {
    let auth_path = dirs::home_dir().map(|h| h.join(".cursor").join("auth.json"));
    probe_cursor_auth_inner(api_key, auth_path.as_deref())
}

/// Inner implementation of [`probe_cursor_auth`] with injectable auth path for
/// testability.
pub(crate) fn probe_cursor_auth_inner(
    api_key: Option<&str>,
    auth_path: Option<&Path>,
) -> Result<(), ActualError> {
    // 1. Explicit API key provided
    if api_key.is_some_and(|k| !k.is_empty()) {
        return Ok(());
    }

    // 2. Auth file exists (cursor-agent login)
    if let Some(path) = auth_path {
        if path.is_file() {
            return Ok(());
        }
    }

    // 3. CURSOR_API_KEY env var
    if std::env::var("CURSOR_API_KEY").is_ok_and(|v| !v.is_empty()) {
        return Ok(());
    }

    Err(ActualError::CursorNotAuthenticated)
}

// ── resolve_api_key (moved from sync_wiring.rs) ───────────────────────────────

/// Resolve an API key from an environment variable or a config fallback.
///
/// Returns `Err(ActualError::ApiKeyMissing)` if neither source provides a key.
pub(crate) fn resolve_api_key(
    env_var: &str,
    config_key: Option<&str>,
) -> Result<String, ActualError> {
    std::env::var(env_var)
        .ok()
        .or_else(|| config_key.map(|s| s.to_string()))
        .ok_or_else(|| ActualError::ApiKeyMissing {
            env_var: env_var.to_string(),
        })
}

// ── Runner availability checks ────────────────────────────────────────────────

/// Check whether the Claude CLI runner is available and authenticated.
pub fn is_claude_available() -> Result<(), String> {
    let binary = find_claude_binary().map_err(|_| {
        "claude-cli: binary not found (install: npm install -g @anthropic-ai/claude-code)"
            .to_string()
    })?;
    let status = probe_claude_auth(&binary, Duration::from_secs(10))
        .map_err(|e| format!("claude-cli: auth probe failed: {e}"))?;
    if status.is_usable() {
        Ok(())
    } else {
        Err("claude-cli: not authenticated (run: claude auth login)".to_string())
    }
}

/// Check whether the Anthropic API runner is available (API key present).
pub fn is_anthropic_available(config_key: Option<&str>) -> Result<(), String> {
    resolve_api_key("ANTHROPIC_API_KEY", config_key)
        .map(|_| ())
        .map_err(|_| "anthropic-api: ANTHROPIC_API_KEY not set".to_string())
}

/// Check whether the OpenAI API runner is available (API key present).
pub fn is_openai_available(config_key: Option<&str>) -> Result<(), String> {
    resolve_api_key("OPENAI_API_KEY", config_key)
        .map(|_| ())
        .map_err(|_| "openai-api: OPENAI_API_KEY not set".to_string())
}

/// Check whether the Codex CLI runner is available and authenticated.
pub fn is_codex_available(config_key: Option<&str>) -> Result<(), String> {
    find_codex_binary().map_err(|_| {
        "codex-cli: binary not found (install: npm install -g @openai/codex)".to_string()
    })?;
    let api_key = std::env::var("OPENAI_API_KEY")
        .ok()
        .or_else(|| config_key.map(|s| s.to_string()));
    probe_codex_auth(api_key.as_deref()).map_err(|_| {
        "codex-cli: not authenticated (set OPENAI_API_KEY or run: codex login)".to_string()
    })
}

/// Check whether the Cursor CLI runner is available and authenticated.
pub fn is_cursor_available(config_key: Option<&str>) -> Result<(), String> {
    find_cursor_binary().map_err(|_| {
        "cursor-cli: binary not found (install: curl https://cursor.com/install -fsS | bash)"
            .to_string()
    })?;
    let api_key = std::env::var("CURSOR_API_KEY")
        .ok()
        .or_else(|| config_key.map(|s| s.to_string()));
    probe_cursor_auth(api_key.as_deref()).map_err(|_| {
        "cursor-cli: not authenticated (set CURSOR_API_KEY or run: cursor-agent login)".to_string()
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

    /// Write content to `path`, fsync it, and mark it executable.
    ///
    /// Using `File::create` + `write_all` + `sync_all` + explicit `drop` (rather
    /// than `std::fs::write`) guarantees the kernel sees no open write fds before
    /// the file is exec'd, preventing spurious ETXTBSY errors under CI load.
    #[cfg(unix)]
    fn write_executable_script(path: &std::path::Path, content: &[u8]) {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(content).unwrap();
        file.sync_all().unwrap();
        drop(file);
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // ── error helper function tests ───────────────────────────────────────────

    #[test]
    fn test_runtime_build_error_helper() {
        // Exercises runtime_build_error(), the only reachable path to the
        // tokio-runtime-build error conversion without OS-level trickery.
        let io_err = std::io::Error::new(std::io::ErrorKind::Other, "synthetic error");
        let err = runtime_build_error(io_err);
        assert!(matches!(
            err,
            ActualError::RunnerFailed { ref message, .. }
                if message.contains("failed to build tokio runtime")
        ));
    }

    #[test]
    fn test_wait_io_error_helper() {
        // Exercises wait_io_error(), the only reachable path to the
        // wait_with_output error conversion without OS-level trickery.
        let io_err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe closed");
        let err = wait_io_error(io_err);
        assert!(matches!(
            err,
            ActualError::RunnerFailed { ref message, .. }
                if message.contains("failed to wait for claude")
        ));
    }

    // ── probe_codex_auth tests ────────────────────────────────────────────────

    #[test]
    fn test_probe_codex_auth_with_api_key() {
        assert!(probe_codex_auth_inner(Some("sk-key"), None).is_ok());
    }

    #[test]
    fn test_probe_codex_auth_with_auth_file() {
        let dir = tempfile::tempdir().unwrap();
        let auth_file = dir.path().join("auth.json");
        std::fs::write(&auth_file, "{}").unwrap();
        assert!(probe_codex_auth_inner(None, Some(&auth_file)).is_ok());
    }

    #[test]
    fn test_probe_codex_auth_no_auth() {
        let result = probe_codex_auth_inner(None, None);
        assert!(
            matches!(result, Err(ActualError::CodexNotAuthenticated)),
            "expected CodexNotAuthenticated, got: {result:?}"
        );
    }

    // ── probe_cursor_auth tests ───────────────────────────────────────────────

    #[test]
    fn test_probe_cursor_auth_with_api_key() {
        assert!(probe_cursor_auth_inner(Some("key"), None).is_ok());
    }

    #[test]
    fn test_probe_cursor_auth_with_auth_file() {
        let dir = tempfile::tempdir().unwrap();
        let auth_file = dir.path().join("auth.json");
        std::fs::write(&auth_file, "{}").unwrap();
        assert!(probe_cursor_auth_inner(None, Some(&auth_file)).is_ok());
    }

    #[test]
    fn test_probe_cursor_auth_env_var() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("CURSOR_API_KEY", "key");
        // No api_key arg, no auth file — but env var is set
        assert!(probe_cursor_auth_inner(None, None).is_ok());
    }

    #[test]
    fn test_probe_cursor_auth_no_auth() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("CURSOR_API_KEY");
        let result = probe_cursor_auth_inner(None, None);
        assert!(
            matches!(result, Err(ActualError::CursorNotAuthenticated)),
            "expected CursorNotAuthenticated, got: {result:?}"
        );
    }

    // ── resolve_api_key tests ─────────────────────────────────────────────────

    #[test]
    fn test_resolve_api_key_env_takes_priority() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("PROBE_TEST_API_KEY", "env-value");
        let result = resolve_api_key("PROBE_TEST_API_KEY", Some("config-value"));
        assert_eq!(result.unwrap(), "env-value");
    }

    #[test]
    fn test_resolve_api_key_config_fallback() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("PROBE_TEST_API_KEY");
        let result = resolve_api_key("PROBE_TEST_API_KEY", Some("config-value"));
        assert_eq!(result.unwrap(), "config-value");
    }

    #[test]
    fn test_resolve_api_key_missing() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("PROBE_TEST_API_KEY");
        let result = resolve_api_key("PROBE_TEST_API_KEY", None);
        assert!(
            matches!(result, Err(ActualError::ApiKeyMissing { ref env_var }) if env_var == "PROBE_TEST_API_KEY"),
            "expected ApiKeyMissing, got: {result:?}"
        );
    }

    // ── probe_codex_auth public wrapper ──────────────────────────────────────

    #[test]
    fn test_probe_codex_auth_public_with_api_key() {
        // The public wrapper resolves home_dir and delegates to inner.
        // Passing an API key always succeeds without checking the auth file.
        assert!(probe_codex_auth(Some("sk-test-key")).is_ok());
    }

    #[test]
    fn test_probe_codex_auth_public_without_key() {
        // Public wrapper without a key — result depends on whether
        // ~/.codex/auth.json exists on the test machine.  We just verify
        // the function doesn't panic and returns a valid Result.
        let _ = probe_codex_auth(None);
    }

    // ── probe_cursor_auth public wrapper ─────────────────────────────────────

    #[test]
    fn test_probe_cursor_auth_public_with_api_key() {
        // The public wrapper resolves home_dir and delegates to inner.
        // Passing an API key always succeeds without checking the auth file.
        assert!(probe_cursor_auth(Some("cursor-test-key")).is_ok());
    }

    #[test]
    fn test_probe_cursor_auth_public_without_key() {
        // Public wrapper without a key — result depends on the test machine.
        // We just verify the function doesn't panic and returns a valid Result.
        let _ = probe_cursor_auth(None);
    }

    // ── is_*_available tests ──────────────────────────────────────────────────

    #[test]
    fn test_is_anthropic_available_with_env_key() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant-test");
        assert!(is_anthropic_available(None).is_ok());
    }

    #[test]
    fn test_is_anthropic_available_with_config_key() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("ANTHROPIC_API_KEY");
        assert!(is_anthropic_available(Some("sk-ant-from-config")).is_ok());
    }

    #[test]
    fn test_is_anthropic_available_no_key() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("ANTHROPIC_API_KEY");
        let result = is_anthropic_available(None);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("ANTHROPIC_API_KEY"),
            "error must name the env var"
        );
    }

    #[test]
    fn test_is_openai_available_with_env_key() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("OPENAI_API_KEY", "sk-openai-test");
        assert!(is_openai_available(None).is_ok());
    }

    #[test]
    fn test_is_openai_available_with_config_key() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("OPENAI_API_KEY");
        assert!(is_openai_available(Some("sk-openai-from-config")).is_ok());
    }

    #[test]
    fn test_is_openai_available_no_key() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("OPENAI_API_KEY");
        let result = is_openai_available(None);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("OPENAI_API_KEY"),
            "error must name the env var"
        );
    }

    #[test]
    fn test_is_codex_available_binary_not_found() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("CODEX_BINARY", "/nonexistent/codex");
        let result = is_codex_available(None);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("codex-cli"),
            "error must mention codex-cli"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_is_codex_available_no_auth() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake-codex");
        write_executable_script(&fake, b"#!/bin/sh\nexit 0\n");
        let _guard = EnvGuard::set("CODEX_BINARY", fake.to_str().unwrap());
        let _key_guard = EnvGuard::remove("OPENAI_API_KEY");
        // Point HOME to a temp dir so ~/.codex/auth.json doesn't exist
        let home_dir = tempfile::tempdir().unwrap();
        let _home_guard = EnvGuard::set("HOME", home_dir.path().to_str().unwrap());
        // No config key, no env key, no auth file — probe_codex_auth should fail
        let result = is_codex_available(None);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("codex-cli"),
            "error must mention codex-cli"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_is_codex_available_with_api_key() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake-codex");
        write_executable_script(&fake, b"#!/bin/sh\nexit 0\n");
        let _guard = EnvGuard::set("CODEX_BINARY", fake.to_str().unwrap());
        let _key_guard = EnvGuard::set("OPENAI_API_KEY", "sk-openai-test");
        assert!(is_codex_available(None).is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn test_is_codex_available_config_key() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake-codex");
        write_executable_script(&fake, b"#!/bin/sh\nexit 0\n");
        let _guard = EnvGuard::set("CODEX_BINARY", fake.to_str().unwrap());
        let _key_guard = EnvGuard::remove("OPENAI_API_KEY");
        assert!(is_codex_available(Some("sk-openai-from-config")).is_ok());
    }

    #[test]
    fn test_is_cursor_available_binary_not_found() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("CURSOR_BINARY", "/nonexistent/cursor-agent");
        let result = is_cursor_available(None);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("cursor-cli"),
            "error must mention cursor-cli"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_is_cursor_available_with_api_key() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake-cursor");
        write_executable_script(&fake, b"#!/bin/sh\nexit 0\n");
        let _guard = EnvGuard::set("CURSOR_BINARY", fake.to_str().unwrap());
        let _key_guard = EnvGuard::set("CURSOR_API_KEY", "cursor-test-key");
        assert!(is_cursor_available(None).is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn test_is_cursor_available_config_key() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake-cursor");
        write_executable_script(&fake, b"#!/bin/sh\nexit 0\n");
        let _guard = EnvGuard::set("CURSOR_BINARY", fake.to_str().unwrap());
        let _key_guard = EnvGuard::remove("CURSOR_API_KEY");
        assert!(is_cursor_available(Some("cursor-config-key")).is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn test_is_cursor_available_no_auth() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake-cursor");
        write_executable_script(&fake, b"#!/bin/sh\nexit 0\n");
        let _guard = EnvGuard::set("CURSOR_BINARY", fake.to_str().unwrap());
        let _key_guard = EnvGuard::remove("CURSOR_API_KEY");
        // Point HOME to temp dir so ~/.cursor/auth.json doesn't exist
        let home_dir = tempfile::tempdir().unwrap();
        let _home_guard = EnvGuard::set("HOME", home_dir.path().to_str().unwrap());
        let result = is_cursor_available(None);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("cursor-cli"),
            "error must mention cursor-cli"
        );
    }

    #[test]
    fn test_is_claude_available_binary_not_found() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/claude");
        let result = is_claude_available();
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("claude-cli"),
            "error must mention claude-cli"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_is_claude_available_auth_probe_error() {
        // Binary exists but exits with non-zero + no JSON → probe_claude_auth returns Err
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake-claude-crash.sh");
        write_executable_script(&fake, b"#!/bin/sh\necho 'crash'; exit 1\n");
        let _guard = EnvGuard::set("CLAUDE_BINARY", fake.to_str().unwrap());
        let result = is_claude_available();
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("claude-cli"),
            "error must mention claude-cli"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_is_claude_available_not_logged_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake-claude.sh");
        write_executable_script(&fake, b"#!/bin/sh\nprintf '%s\\n' '{\"loggedIn\":false}'\n");
        let _guard = EnvGuard::set("CLAUDE_BINARY", fake.to_str().unwrap());
        let result = is_claude_available();
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("not authenticated"),
            "error must say not authenticated"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_is_claude_available_logged_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake-claude.sh");
        write_executable_script(
            &fake,
            b"#!/bin/sh\nprintf '%s\\n' '{\"loggedIn\":true,\"authMethod\":\"claude.ai\"}'\n",
        );
        let _guard = EnvGuard::set("CLAUDE_BINARY", fake.to_str().unwrap());
        assert!(is_claude_available().is_ok());
    }

    // ── probe_claude_auth sync wrapper ────────────────────────────────────────

    #[test]
    #[cfg(unix)]
    fn test_probe_claude_auth_sync_success() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        write_executable_script(
            &script,
            b"#!/bin/sh\nprintf '%s\\n' '{\"loggedIn\":true,\"authMethod\":\"claude.ai\"}'\n",
        );

        let result = probe_claude_auth(&script, Duration::from_secs(5));
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
        assert!(result.unwrap().logged_in);
    }

    #[test]
    fn test_probe_claude_auth_sync_spawn_failure() {
        // Non-existent binary → spawn fails
        let result = probe_claude_auth(
            std::path::Path::new("/nonexistent/path/to/claude"),
            Duration::from_secs(5),
        );
        assert!(
            matches!(result, Err(ActualError::RunnerFailed { .. })),
            "expected RunnerFailed, got: {result:?}"
        );
    }

    // ── probe_claude_auth_async tests ─────────────────────────────────────────

    #[tokio::test]
    #[cfg(unix)]
    async fn test_probe_claude_auth_async_success() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        write_executable_script(
            &script,
            b"#!/bin/sh\nprintf '%s\\n' '{\"loggedIn\":true,\"authMethod\":\"claude.ai\"}'\n",
        );

        let result = probe_claude_auth_async(&script, Duration::from_secs(5)).await;
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
        assert!(result.unwrap().logged_in);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_probe_claude_auth_async_not_logged_in() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        write_executable_script(
            &script,
            b"#!/bin/sh\nprintf '%s\\n' '{\"loggedIn\":false}'\n",
        );

        let result = probe_claude_auth_async(&script, Duration::from_secs(5)).await;
        assert!(result.is_ok(), "expected Ok(status), got: {result:?}");
        assert!(!result.unwrap().logged_in);
    }

    #[tokio::test]
    async fn test_probe_claude_auth_async_spawn_failure() {
        let result = probe_claude_auth_async(
            std::path::Path::new("/nonexistent/path/to/claude"),
            Duration::from_secs(5),
        )
        .await;
        assert!(
            matches!(result, Err(ActualError::RunnerFailed { .. })),
            "expected RunnerFailed from spawn failure, got: {result:?}"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_probe_claude_auth_async_subprocess_failure() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        write_executable_script(&script, b"#!/bin/sh\necho 'error output' >&2\nexit 1\n");

        let result = probe_claude_auth_async(&script, Duration::from_secs(5)).await;
        assert!(
            matches!(result, Err(ActualError::RunnerFailed { .. })),
            "expected RunnerFailed, got: {result:?}"
        );
    }

    /// Some claude CLI versions exit non-zero even when authenticated.
    /// The probe must accept valid JSON with `loggedIn: true` regardless of
    /// the exit code.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_probe_claude_auth_async_nonzero_exit_but_logged_in() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        write_executable_script(
            &script,
            b"#!/bin/sh\nprintf '%s\\n' '{\"loggedIn\":true,\"authMethod\":\"claude.ai\"}'\nexit 1\n",
        );

        let result = probe_claude_auth_async(&script, Duration::from_secs(5)).await;
        assert!(
            result.is_ok(),
            "expected Ok(status) when loggedIn:true, got: {result:?}"
        );
        assert!(result.unwrap().logged_in);
    }

    /// Non-zero exit with JSON showing `loggedIn: false` must still fail.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_probe_claude_auth_async_nonzero_exit_not_logged_in() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        write_executable_script(
            &script,
            b"#!/bin/sh\nprintf '%s\\n' '{\"loggedIn\":false}'\nexit 1\n",
        );

        let result = probe_claude_auth_async(&script, Duration::from_secs(5)).await;
        assert!(
            matches!(result, Err(ActualError::RunnerFailed { .. })),
            "expected RunnerFailed when loggedIn:false with non-zero exit, got: {result:?}"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_probe_claude_auth_async_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        write_executable_script(&script, b"#!/bin/sh\nprintf '%s\\n' 'not json'\n");

        let result = probe_claude_auth_async(&script, Duration::from_secs(5)).await;
        assert!(
            matches!(result, Err(ActualError::RunnerOutputParse(_))),
            "expected RunnerOutputParse, got: {result:?}"
        );
    }

    /// Simulates Claude CLI versions (e.g. ≤2.1.25) that reject --json but
    /// succeed without it, producing plain text output.
    /// Probe must fall back and return Ok(logged_in: true) from exit code 0.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_probe_claude_auth_async_no_json_flag_text_output() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        // When --json is passed: print error to stderr and exit 1.
        // When called without --json: print plain text and exit 0.
        write_executable_script(
            &script,
            b"#!/bin/sh\n\
             for arg in \"$@\"; do\n\
               if [ \"$arg\" = \"--json\" ]; then\n\
                 printf \"unknown option '--json'\\n\" >&2\n\
                 exit 1\n\
               fi\n\
             done\n\
             printf 'Logged in as user@example.com\\n'\n\
             exit 0\n",
        );

        let result = probe_claude_auth_async(&script, Duration::from_secs(5)).await;
        assert!(
            result.is_ok(),
            "expected Ok when --json unknown and no-flag exits 0, got: {result:?}"
        );
        assert!(result.unwrap().logged_in);
    }

    /// Same scenario but the no-flag fallback outputs JSON (some intermediate
    /// versions emit JSON by default without needing --json).
    #[tokio::test]
    #[cfg(unix)]
    async fn test_probe_claude_auth_async_no_json_flag_json_output() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        write_executable_script(
            &script,
            b"#!/bin/sh\n\
             for arg in \"$@\"; do\n\
               if [ \"$arg\" = \"--json\" ]; then\n\
                 printf \"unknown option '--json'\\n\" >&2\n\
                 exit 1\n\
               fi\n\
             done\n\
             printf '{\"loggedIn\":true,\"authMethod\":\"claude.ai\"}\\n'\n\
             exit 0\n",
        );

        let result = probe_claude_auth_async(&script, Duration::from_secs(5)).await;
        assert!(
            result.is_ok(),
            "expected Ok with JSON from no-flag fallback, got: {result:?}"
        );
        let status = result.unwrap();
        assert!(status.logged_in);
        assert_eq!(status.auth_method.as_deref(), Some("claude.ai"));
    }

    /// When --json is unknown and the no-flag fallback also exits non-zero,
    /// the probe must propagate RunnerFailed.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_probe_claude_auth_async_no_json_flag_fallback_fails() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        write_executable_script(
            &script,
            b"#!/bin/sh\n\
             for arg in \"$@\"; do\n\
               if [ \"$arg\" = \"--json\" ]; then\n\
                 printf \"unknown option '--json'\\n\" >&2\n\
                 exit 1\n\
               fi\n\
             done\n\
             printf 'Not logged in\\n' >&2\n\
             exit 1\n",
        );

        let result = probe_claude_auth_async(&script, Duration::from_secs(5)).await;
        assert!(
            matches!(result, Err(ActualError::RunnerFailed { .. })),
            "expected RunnerFailed when fallback also fails, got: {result:?}"
        );
    }

    /// Direct test: no-json fallback with plain-text output and non-zero exit
    /// must propagate RunnerFailed (covers lines 165–174 of probe_claude_auth_async_no_json).
    #[tokio::test]
    #[cfg(unix)]
    async fn test_probe_claude_auth_async_no_json_exits_nonzero() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        write_executable_script(
            &script,
            b"#!/bin/sh\nprintf 'Not logged in\\n' >&2\nexit 1\n",
        );

        let result = probe_claude_auth_async_no_json(&script, Duration::from_secs(5)).await;
        assert!(
            matches!(result, Err(ActualError::RunnerFailed { .. })),
            "expected RunnerFailed when no-json probe exits non-zero, got: {result:?}"
        );
    }

    /// When the `--json` error goes to stdout instead of stderr, the stdout
    /// branch of `json_flag_not_recognized` must still trigger the fallback.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_probe_claude_auth_async_json_flag_error_in_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        // --json error goes to stdout (not stderr)
        write_executable_script(
            &script,
            b"#!/bin/sh\n\
             for arg in \"$@\"; do\n\
               if [ \"$arg\" = \"--json\" ]; then\n\
                 printf \"unknown option '--json'\\n\"\n\
                 exit 1\n\
               fi\n\
             done\n\
             printf 'Logged in\\n'\n\
             exit 0\n",
        );

        let result = probe_claude_auth_async(&script, Duration::from_secs(5)).await;
        assert!(
            result.is_ok(),
            "expected Ok when --json error in stdout and no-flag exits 0, got: {result:?}"
        );
        assert!(result.unwrap().logged_in);
    }

    /// Spawn failure inside the no-json fallback must propagate RunnerFailed.
    #[tokio::test]
    async fn test_probe_claude_auth_async_no_json_spawn_failure() {
        let result = probe_claude_auth_async_no_json(
            std::path::Path::new("/nonexistent/binary/claude"),
            Duration::from_secs(5),
        )
        .await;
        assert!(
            matches!(result, Err(ActualError::RunnerFailed { .. })),
            "expected RunnerFailed from spawn failure in no-json path, got: {result:?}"
        );
    }

    /// Timeout inside the no-json fallback must propagate RunnerTimeout.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_probe_claude_auth_async_no_json_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        write_executable_script(&script, b"#!/bin/sh\nsleep 30\n");

        let result = probe_claude_auth_async_no_json(&script, Duration::from_millis(100)).await;
        assert!(
            matches!(result, Err(ActualError::RunnerTimeout { .. })),
            "expected RunnerTimeout in no-json path, got: {result:?}"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_probe_claude_auth_async_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        write_executable_script(&script, b"#!/bin/sh\nsleep 30\n");

        let result = probe_claude_auth_async(&script, Duration::from_millis(100)).await;
        assert!(
            matches!(result, Err(ActualError::RunnerTimeout { .. })),
            "expected RunnerTimeout, got: {result:?}"
        );
    }
}
