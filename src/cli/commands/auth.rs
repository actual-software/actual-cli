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

/// Check Claude Code authentication status using the given binary path.
///
/// Runs `claude auth status --json`, parses the response, and returns the
/// status. This is a plain blocking call with no timeout — callers that need
/// a bounded wait should use [`check_auth_with_timeout`] directly.
pub(crate) fn check_auth(binary_path: &Path) -> Result<ClaudeAuthStatus, ActualError> {
    let output = std::process::Command::new(binary_path)
        .args(["auth", "status", "--json"])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| ActualError::ClaudeSubprocessFailed {
            message: format!("failed to execute claude: {e}"),
            stderr: String::new(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(ActualError::ClaudeSubprocessFailed {
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
    let binary_path = binary_path.to_path_buf();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ActualError::ClaudeSubprocessFailed {
            message: format!("failed to build tokio runtime: {e}"),
            stderr: String::new(),
        })?;

    rt.block_on(async move {
        let mut cmd = tokio::process::Command::new(&binary_path);
        cmd.args(["auth", "status", "--json"]);
        cmd.stdin(std::process::Stdio::null());
        // When the timeout future is dropped, the Child is dropped. With
        // kill_on_drop(true) tokio sends SIGKILL, preventing orphaned processes.
        cmd.kill_on_drop(true);

        let child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| ActualError::ClaudeSubprocessFailed {
                message: format!("failed to spawn claude: {e}"),
                stderr: String::new(),
            })?;

        let result = tokio::time::timeout(timeout, child.wait_with_output()).await;

        match result {
            Ok(Ok(output)) => {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    return Err(ActualError::ClaudeSubprocessFailed {
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
            Ok(Err(e)) => Err(ActualError::ClaudeSubprocessFailed {
                message: format!("failed to wait for claude: {e}"),
                stderr: String::new(),
            }),
            Err(_) => Err(ActualError::ClaudeTimeout {
                // Round up so sub-second timeouts display as "1s" rather than "0s".
                seconds: timeout.as_secs_f64().ceil() as u64,
            }),
        }
    })
}

fn run_auth_with_binary(binary_path: &Path) -> Result<(), ActualError> {
    let status = check_auth(binary_path)?;

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
            ActualError::ClaudeSubprocessFailed { .. }
        ));
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
            ActualError::ClaudeOutputParse(_)
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
        assert!(
            matches!(result, Err(ActualError::ClaudeTimeout { .. })),
            "expected ClaudeTimeout, got: {result:?}"
        );
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
            ActualError::ClaudeSubprocessFailed { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn test_exec_subprocess_failed() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let not_executable = dir.path().join("not-executable");
        std::fs::write(&not_executable, "not a script").unwrap();
        // File exists but is NOT executable — goes through exec -> run_auth -> run_auth_with_binary
        let _guard = EnvGuard::set("CLAUDE_BINARY", not_executable.to_str().unwrap());
        let code = handle_result(exec());
        assert_eq!(code, 1);
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
            ActualError::ClaudeSubprocessFailed { .. }
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
            ActualError::ClaudeOutputParse(_)
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
}
