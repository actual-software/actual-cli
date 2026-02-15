use std::path::Path;

use console::style;

use crate::claude::auth::ClaudeAuthStatus;
use crate::claude::binary::find_claude_binary;
use crate::cli::ui::progress::{ERROR_SYMBOL, SUCCESS_SYMBOL};
use crate::error::ActualError;

pub fn exec() -> i32 {
    match run_auth() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{} {}", style("Error:").red().bold(), e);
            e.exit_code()
        }
    }
}

fn run_auth() -> Result<(), ActualError> {
    let binary_path = find_claude_binary()?;
    run_auth_with_binary(&binary_path)
}

fn run_auth_with_binary(binary_path: &Path) -> Result<(), ActualError> {
    let output = std::process::Command::new(binary_path)
        .args(["auth", "status", "--json"])
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
            style(SUCCESS_SYMBOL).green(),
            style("authenticated").green()
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
            style(ERROR_SYMBOL).red(),
            style("not authenticated").red()
        );
        eprintln!(
            "  Run {} to authenticate.",
            style("`claude auth login`").cyan()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

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
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CLAUDE_BINARY", "/nonexistent/path/to/claude");
        let result = run_auth();
        std::env::remove_var("CLAUDE_BINARY");
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
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CLAUDE_BINARY", "/nonexistent/path/to/claude");
        let code = exec();
        std::env::remove_var("CLAUDE_BINARY");
        assert_eq!(code, 2);
    }

    #[cfg(unix)]
    #[test]
    fn test_exec_success() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(
            dir.path(),
            r#"{"loggedIn": true, "authMethod": "claude.ai", "email": "user@example.com"}"#,
            0,
        );
        std::env::set_var("CLAUDE_BINARY", script.to_str().unwrap());
        let code = exec();
        std::env::remove_var("CLAUDE_BINARY");
        assert_eq!(code, 0);
    }

    #[cfg(unix)]
    #[test]
    fn test_exec_not_authenticated() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(dir.path(), r#"{"loggedIn": false}"#, 0);
        std::env::set_var("CLAUDE_BINARY", script.to_str().unwrap());
        let code = exec();
        std::env::remove_var("CLAUDE_BINARY");
        assert_eq!(code, 2);
    }

    #[cfg(unix)]
    #[test]
    fn test_run_auth_via_env_success() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(
            dir.path(),
            r#"{"loggedIn": true, "authMethod": "claude.ai", "email": "user@example.com"}"#,
            0,
        );
        std::env::set_var("CLAUDE_BINARY", script.to_str().unwrap());
        let result = run_auth();
        std::env::remove_var("CLAUDE_BINARY");
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
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
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let not_executable = dir.path().join("not-executable");
        std::fs::write(&not_executable, "not a script").unwrap();
        // File exists but is NOT executable — goes through exec -> run_auth -> run_auth_with_binary
        std::env::set_var("CLAUDE_BINARY", not_executable.to_str().unwrap());
        let code = exec();
        std::env::remove_var("CLAUDE_BINARY");
        assert_eq!(code, 1);
    }

    #[cfg(unix)]
    #[test]
    fn test_run_auth_subprocess_exit_nonzero() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(dir.path(), "error output", 1);
        std::env::set_var("CLAUDE_BINARY", script.to_str().unwrap());
        let result = run_auth();
        std::env::remove_var("CLAUDE_BINARY");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ActualError::ClaudeSubprocessFailed { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn test_run_auth_invalid_json_via_env() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(dir.path(), "not json", 0);
        std::env::set_var("CLAUDE_BINARY", script.to_str().unwrap());
        let result = run_auth();
        std::env::remove_var("CLAUDE_BINARY");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ActualError::ClaudeOutputParse(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn test_run_auth_not_authenticated_via_env() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let script = create_fake_binary(dir.path(), r#"{"loggedIn": false}"#, 0);
        std::env::set_var("CLAUDE_BINARY", script.to_str().unwrap());
        let result = run_auth();
        std::env::remove_var("CLAUDE_BINARY");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ActualError::ClaudeNotAuthenticated
        ));
    }
}
