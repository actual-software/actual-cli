use std::path::Path;

use crate::cli::ui::theme;
use crate::error::ActualError;
use crate::runner::auth::ClaudeAuthStatus;
use crate::runner::binary::find_claude_binary;

pub fn exec() -> Result<(), ActualError> {
    run_auth()
}

fn run_auth() -> Result<(), ActualError> {
    let binary_path = find_claude_binary()?;
    run_auth_with_binary(&binary_path)
}

/// Check Claude Code authentication status with a timeout.
///
/// Uses `tokio::process::Command` with `kill_on_drop(true)` so that when the
/// timeout fires, dropping the child handle sends SIGKILL and prevents orphaned
/// processes. A single-threaded tokio runtime is created for this call so the
/// function remains synchronous for callers in the CLI dispatch path.
pub fn check_auth_with_timeout(
    binary_path: &Path,
    timeout: std::time::Duration,
) -> Result<ClaudeAuthStatus, ActualError> {
    let rt = build_tokio_runtime()?;
    rt.block_on(check_auth_async(binary_path, timeout))
}

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

/// Build a single-threaded tokio runtime.
fn build_tokio_runtime() -> Result<tokio::runtime::Runtime, ActualError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(runtime_build_error)
}

/// Inner async logic for [`check_auth_with_timeout`].
///
/// Separated for testability with `#[tokio::test]`.
async fn check_auth_async(
    binary_path: &Path,
    timeout: std::time::Duration,
) -> Result<ClaudeAuthStatus, ActualError> {
    let mut cmd = tokio::process::Command::new(binary_path);
    cmd.args(["auth", "status", "--json"]);
    cmd.stdin(std::process::Stdio::null());
    // When the timeout future is dropped, the Child is dropped. With
    // kill_on_drop(true) tokio sends SIGKILL, preventing orphaned processes.
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
            return check_auth_async_no_json(binary_path, timeout).await;
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
    stderr.contains("--json") || String::from_utf8_lossy(stdout).contains("--json")
}

/// Fallback for Claude CLI versions that do not support `--json`.
///
/// Runs `claude auth status` without the flag.  Newer versions already emit
/// JSON without being asked; for older ones that produce plain text we treat
/// exit code 0 as "authenticated".
async fn check_auth_async_no_json(
    binary_path: &Path,
    timeout: std::time::Duration,
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

fn run_auth_with_binary(binary_path: &Path) -> Result<(), ActualError> {
    let status = check_auth_with_timeout(binary_path, std::time::Duration::from_secs(10))?;

    print_auth_status(&status);

    if !status.is_usable() {
        return Err(ActualError::ClaudeNotAuthenticated);
    }

    Ok(())
}

fn print_auth_status(status: &ClaudeAuthStatus) {
    if status.is_usable() {
        println!(
            "{} Claude Code: {}",
            theme::success(&theme::SUCCESS),
            theme::success("authenticated")
        );
        println!("  {:<14} {}", "Method:", status.display_method());
        if let Some(ref email) = status.email {
            println!("  {:<14} {}", "Email:", email);
        }
        if let Some(ref org) = status.org_name {
            println!("  {:<14} {}", "Organization:", org);
        }
    } else {
        eprintln!(
            "{} Claude Code: {}",
            theme::error(&theme::ERROR),
            theme::error("not authenticated")
        );
        eprintln!(
            "  Run {} to authenticate.",
            theme::hint("`claude auth login`")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::commands::handle_result;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

    #[cfg(unix)]
    fn create_fake_binary(dir: &Path, stdout_content: &str, exit_code: i32) -> std::path::PathBuf {
        let script = dir.join("fake-claude");
        let content = format!(
            "#!/bin/sh\nprintf '%s\\n' '{}'\nexit {}\n",
            stdout_content.replace('\'', "'\\''"),
            exit_code
        );
        std::fs::write(&script, &content).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    #[test]
    fn test_print_auth_status_authenticated() {
        let status = ClaudeAuthStatus {
            logged_in: true,
            auth_method: Some("claude.ai".to_string()),
            email: Some("user@example.com".to_string()),
            api_provider: None,
            api_key_source: None,
            org_id: None,
            org_name: None,
            subscription_type: None,
        };
        print_auth_status(&status);
    }

    #[test]
    fn test_print_auth_status_not_authenticated() {
        let status = ClaudeAuthStatus {
            logged_in: false,
            auth_method: None,
            email: None,
            api_provider: None,
            api_key_source: None,
            org_id: None,
            org_name: None,
            subscription_type: None,
        };
        print_auth_status(&status);
    }

    #[test]
    fn test_print_auth_status_api_key() {
        let status = ClaudeAuthStatus {
            logged_in: true,
            auth_method: None,
            email: None,
            api_provider: None,
            api_key_source: Some("ANTHROPIC_API_KEY".to_string()),
            org_id: None,
            org_name: None,
            subscription_type: None,
        };
        print_auth_status(&status);
    }

    #[test]
    fn test_print_auth_status_all_fields() {
        let status = ClaudeAuthStatus {
            logged_in: true,
            auth_method: Some("claude.ai".to_string()),
            email: Some("user@example.com".to_string()),
            api_provider: Some("anthropic".to_string()),
            api_key_source: Some("ANTHROPIC_API_KEY".to_string()),
            org_id: Some("org-123".to_string()),
            org_name: Some("Acme Corp".to_string()),
            subscription_type: Some("pro".to_string()),
        };
        print_auth_status(&status);
    }

    #[test]
    fn test_run_auth_binary_not_found() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/path/to/claude");
        let result = run_auth();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::ClaudeNotFound));
    }

    // Tests below use run_auth_with_binary() directly to avoid env var races
    // with other modules' tests that also manipulate environment variables.

    #[cfg(unix)]
    #[test]
    fn test_run_auth_success() {
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(
            dir.path(),
            r#"{"loggedIn": true, "authMethod": "claude.ai", "email": "user@example.com"}"#,
            0,
        );
        let result = run_auth_with_binary(&script);
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
    }

    #[cfg(unix)]
    #[test]
    fn test_run_auth_not_authenticated() {
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(dir.path(), r#"{"loggedIn": false}"#, 0);
        let result = run_auth_with_binary(&script);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ActualError::ClaudeNotAuthenticated
        ));
    }

    #[cfg(unix)]
    #[test]
    fn test_run_auth_subprocess_failure() {
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(dir.path(), "error output", 1);
        let result = run_auth_with_binary(&script);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ActualError::RunnerFailed { .. }
        ));
    }

    /// Some claude CLI versions exit non-zero even when authenticated.
    /// check_auth_async must accept valid JSON with `loggedIn: true`
    /// regardless of the exit code.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_check_auth_async_nonzero_exit_but_logged_in() {
        use std::path::Path;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude");
        let content = "#!/bin/sh\nprintf '%s\\n' '{\"loggedIn\":true,\"authMethod\":\"claude.ai\"}'\nexit 1\n";
        std::fs::write(&script, content).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let result = check_auth_async(Path::new(&script), std::time::Duration::from_secs(5)).await;
        assert!(
            result.is_ok(),
            "expected Ok(status) when loggedIn:true, got: {result:?}"
        );
        assert!(result.unwrap().logged_in);
    }

    /// Non-zero exit with JSON showing `loggedIn: false` must still fail.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_check_auth_async_nonzero_exit_not_logged_in() {
        use std::path::Path;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude");
        let content = "#!/bin/sh\nprintf '%s\\n' '{\"loggedIn\":false}'\nexit 1\n";
        std::fs::write(&script, content).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let result = check_auth_async(Path::new(&script), std::time::Duration::from_secs(5)).await;
        assert!(
            matches!(result, Err(ActualError::RunnerFailed { .. })),
            "expected RunnerFailed when loggedIn:false with non-zero exit, got: {result:?}"
        );
    }

    /// Simulates Claude CLI versions (e.g. ≤2.1.25) that reject --json but
    /// succeed without it, producing plain text output.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_check_auth_async_no_json_flag_text_output() {
        use std::os::unix::fs::PermissionsExt;
        use std::path::Path;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             for arg in \"$@\"; do\n\
               if [ \"$arg\" = \"--json\" ]; then\n\
                 printf \"unknown option '--json'\\n\" >&2\n\
                 exit 1\n\
               fi\n\
             done\n\
             printf 'Logged in as user@example.com\\n'\n\
             exit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let result = check_auth_async(Path::new(&script), std::time::Duration::from_secs(5)).await;
        assert!(
            result.is_ok(),
            "expected Ok when --json unknown and no-flag exits 0, got: {result:?}"
        );
        assert!(result.unwrap().logged_in);
    }

    /// Same scenario but the no-flag fallback outputs JSON.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_check_auth_async_no_json_flag_json_output() {
        use std::os::unix::fs::PermissionsExt;
        use std::path::Path;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             for arg in \"$@\"; do\n\
               if [ \"$arg\" = \"--json\" ]; then\n\
                 printf \"unknown option '--json'\\n\" >&2\n\
                 exit 1\n\
               fi\n\
             done\n\
             printf '{\"loggedIn\":true,\"authMethod\":\"claude.ai\"}\\n'\n\
             exit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let result = check_auth_async(Path::new(&script), std::time::Duration::from_secs(5)).await;
        assert!(
            result.is_ok(),
            "expected Ok with JSON from no-flag fallback, got: {result:?}"
        );
        let status = result.unwrap();
        assert!(status.logged_in);
        assert_eq!(status.auth_method.as_deref(), Some("claude.ai"));
    }

    /// When --json is unknown and the no-flag fallback also exits non-zero,
    /// RunnerFailed must be propagated.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_check_auth_async_no_json_flag_fallback_fails() {
        use std::os::unix::fs::PermissionsExt;
        use std::path::Path;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             for arg in \"$@\"; do\n\
               if [ \"$arg\" = \"--json\" ]; then\n\
                 printf \"unknown option '--json'\\n\" >&2\n\
                 exit 1\n\
               fi\n\
             done\n\
             printf 'Not logged in\\n' >&2\n\
             exit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let result = check_auth_async(Path::new(&script), std::time::Duration::from_secs(5)).await;
        assert!(
            matches!(result, Err(ActualError::RunnerFailed { .. })),
            "expected RunnerFailed when fallback also fails, got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_run_auth_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(dir.path(), "not json", 0);
        let result = run_auth_with_binary(&script);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ActualError::RunnerOutputParse(_)
        ));
    }

    #[test]
    fn test_exec_binary_not_found() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/path/to/claude");
        let code = handle_result(exec());
        assert_eq!(code, 2);
    }

    #[cfg(unix)]
    #[test]
    fn test_exec_success() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(
            dir.path(),
            r#"{"loggedIn": true, "authMethod": "claude.ai", "email": "user@example.com"}"#,
            0,
        );
        let _guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
        let code = handle_result(exec());
        assert_eq!(code, 0);
    }

    #[cfg(unix)]
    #[test]
    fn test_exec_not_authenticated() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(dir.path(), r#"{"loggedIn": false}"#, 0);
        let _guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
        let code = handle_result(exec());
        assert_eq!(code, 2);
    }

    #[cfg(unix)]
    #[test]
    fn test_run_auth_via_env_success() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(
            dir.path(),
            r#"{"loggedIn": true, "authMethod": "claude.ai", "email": "user@example.com"}"#,
            0,
        );
        let _guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
        let result = run_auth();
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
    }

    #[cfg(unix)]
    #[test]
    fn test_check_auth_timeout() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude");
        std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let result = check_auth_with_timeout(&script, std::time::Duration::from_millis(100));
        assert!(matches!(result, Err(ActualError::RunnerTimeout { .. })));
    }

    #[cfg(unix)]
    #[test]
    fn test_run_auth_with_binary_not_executable() {
        let dir = tempfile::tempdir().unwrap();
        let not_executable = dir.path().join("not-executable");
        std::fs::write(&not_executable, "not a script").unwrap();
        // File exists but is NOT executable, so Command::new().output() will fail
        let result = run_auth_with_binary(&not_executable);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ActualError::RunnerFailed { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn test_exec_subprocess_failed() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let not_executable = dir.path().join("not-executable");
        std::fs::write(&not_executable, "not a script").unwrap();
        // File exists but is NOT executable — find_claude_binary rejects it
        // with ClaudeNotFound (exit code 2) before reaching the subprocess.
        let _guard = EnvGuard::set("CLAUDE_BINARY", not_executable.to_str().unwrap());
        let code = handle_result(exec());
        assert_eq!(code, 2);
    }

    #[cfg(unix)]
    #[test]
    fn test_run_auth_subprocess_exit_nonzero() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(dir.path(), "error output", 1);
        let _guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
        let result = run_auth();
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ActualError::RunnerFailed { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn test_run_auth_invalid_json_via_env() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(dir.path(), "not json", 0);
        let _guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
        let result = run_auth();
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ActualError::RunnerOutputParse(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn test_run_auth_not_authenticated_via_env() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(dir.path(), r#"{"loggedIn": false}"#, 0);
        let _guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
        let result = run_auth();
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ActualError::ClaudeNotAuthenticated
        ));
    }

    #[test]
    fn test_check_auth_with_timeout_spawn_failure() {
        // Non-existent binary → spawn fails immediately
        let result = check_auth_with_timeout(
            Path::new("/nonexistent/binary/that/does/not/exist"),
            std::time::Duration::from_secs(5),
        );
        assert!(matches!(result, Err(ActualError::RunnerFailed { .. })));
    }

    #[cfg(unix)]
    #[test]
    fn test_check_auth_with_timeout_success() {
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(
            dir.path(),
            r#"{"loggedIn": true, "authMethod": "claude.ai", "email": "user@example.com"}"#,
            0,
        );
        let result = check_auth_with_timeout(&script, std::time::Duration::from_secs(5));
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
        let status = result.unwrap();
        assert!(status.logged_in);
        assert_eq!(status.auth_method.as_deref(), Some("claude.ai"));
    }

    #[cfg(unix)]
    #[test]
    fn test_check_auth_with_timeout_subprocess_failure() {
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(dir.path(), "error output", 1);
        let result = check_auth_with_timeout(&script, std::time::Duration::from_secs(5));
        assert!(matches!(result, Err(ActualError::RunnerFailed { .. })));
    }

    #[cfg(unix)]
    #[test]
    fn test_check_auth_with_timeout_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(dir.path(), "not json", 0);
        let result = check_auth_with_timeout(&script, std::time::Duration::from_secs(5));
        assert!(matches!(result, Err(ActualError::RunnerOutputParse(_))));
    }

    // ── Direct tests of check_auth_async for coverage of all reachable paths ──

    #[tokio::test]
    async fn test_check_auth_async_spawn_failure() {
        let result = check_auth_async(
            Path::new("/nonexistent/binary/that/does/not/exist"),
            std::time::Duration::from_secs(5),
        )
        .await;
        assert!(
            matches!(result, Err(ActualError::RunnerFailed { .. })),
            "expected RunnerFailed from spawn failure, got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_check_auth_async_timeout() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude");
        std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let result = check_auth_async(&script, std::time::Duration::from_millis(100)).await;
        assert!(
            matches!(result, Err(ActualError::RunnerTimeout { .. })),
            "expected RunnerTimeout, got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_check_auth_async_subprocess_failure() {
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(dir.path(), "error output", 1);
        let result = check_auth_async(&script, std::time::Duration::from_secs(5)).await;
        assert!(
            matches!(result, Err(ActualError::RunnerFailed { .. })),
            "expected RunnerFailed, got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_check_auth_async_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(dir.path(), "not json", 0);
        let result = check_auth_async(&script, std::time::Duration::from_secs(5)).await;
        assert!(
            matches!(result, Err(ActualError::RunnerOutputParse(_))),
            "expected RunnerOutputParse, got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_check_auth_async_success() {
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(
            dir.path(),
            r#"{"loggedIn": true, "authMethod": "claude.ai", "email": "user@example.com"}"#,
            0,
        );
        let result = check_auth_async(&script, std::time::Duration::from_secs(5)).await;
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
        let status = result.unwrap();
        assert!(status.logged_in);
        assert_eq!(status.auth_method.as_deref(), Some("claude.ai"));
    }

    #[test]
    fn test_build_tokio_runtime_succeeds() {
        // In any normal environment, building the runtime always succeeds.
        // This exercises the success path and ensures the function is reachable.
        let rt = build_tokio_runtime();
        assert!(rt.is_ok(), "expected runtime to build successfully");
    }

    #[test]
    fn test_runtime_build_error_helper() {
        // Exercises the runtime_build_error() conversion function, which is the
        // only reachable coverage of that code path without OS-level trickery.
        let io_err = std::io::Error::new(std::io::ErrorKind::Other, "synthetic error");
        let err = runtime_build_error(io_err);
        assert!(matches!(
            err,
            ActualError::RunnerFailed { ref message, .. } if message.contains("failed to build tokio runtime")
        ));
    }

    #[test]
    fn test_wait_io_error_helper() {
        // Exercises the wait_io_error() conversion function, which is the
        // only reachable coverage of that code path without OS-level trickery.
        let io_err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe closed");
        let err = wait_io_error(io_err);
        assert!(matches!(
            err,
            ActualError::RunnerFailed { ref message, .. } if message.contains("failed to wait for claude")
        ));
    }
}
