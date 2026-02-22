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
        .map_err(|e| ActualError::RunnerFailed {
            message: format!("failed to build tokio runtime: {e}"),
            stderr: String::new(),
        })?;
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
        .map_err(|e| ActualError::RunnerFailed {
            message: format!("failed to wait for claude: {e}"),
            stderr: String::new(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(ActualError::RunnerFailed {
            message: format!(
                "claude auth status exited with code {}",
                output.status.code().unwrap_or(-1)
            ),
            stderr,
        });
    }

    let status: ClaudeAuthStatus = serde_json::from_slice(&output.stdout)?;
    Ok(status)
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

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

    // ── probe_claude_auth_async tests ─────────────────────────────────────────

    #[tokio::test]
    #[cfg(unix)]
    async fn test_probe_claude_auth_async_success() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\n' '{\"loggedIn\":true,\"authMethod\":\"claude.ai\"}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let result = probe_claude_auth_async(&script, Duration::from_secs(5)).await;
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
        assert!(result.unwrap().logged_in);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_probe_claude_auth_async_not_logged_in() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\n' '{\"loggedIn\":false}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let result = probe_claude_auth_async(&script, Duration::from_secs(5)).await;
        assert!(result.is_ok(), "expected Ok(status), got: {result:?}");
        assert!(!result.unwrap().logged_in);
    }
}
