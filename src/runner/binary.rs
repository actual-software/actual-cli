use std::path::PathBuf;

use crate::error::ActualError;

/// The default binary name to search for on PATH.
const CLAUDE_BINARY_NAME: &str = "claude";

/// Environment variable that overrides the default binary lookup.
/// Useful for testing and CI environments.
const CLAUDE_BINARY_ENV: &str = "CLAUDE_BINARY";

/// Locate the Claude Code binary on the system.
///
/// Resolution order:
/// 1. If `CLAUDE_BINARY` env var is set, use that path (must exist and be executable).
/// 2. Otherwise, search PATH for `claude` using `which::which`.
///
/// Returns the absolute path to the binary on success, or
/// `ActualError::ClaudeNotFound` if the binary cannot be located or is invalid.
pub fn find_claude_binary() -> Result<PathBuf, ActualError> {
    if let Ok(path_str) = std::env::var(CLAUDE_BINARY_ENV) {
        let path = PathBuf::from(&path_str);
        if !path.exists() {
            return Err(ActualError::ClaudeNotFound);
        }
        if !path.is_file() {
            return Err(ActualError::ClaudeNotFound);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .map(|m| m.permissions().mode())
                .unwrap_or(0);
            if mode & 0o111 == 0 {
                return Err(ActualError::ClaudeNotFound);
            }
        }
        return Ok(path);
    }
    which::which(CLAUDE_BINARY_NAME).map_err(|_| ActualError::ClaudeNotFound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

    #[test]
    fn test_error_message_contains_not_installed() {
        let err = ActualError::ClaudeNotFound;
        let msg = err.to_string();
        assert!(
            msg.contains("not installed"),
            "Error message should contain 'not installed', got: {msg}"
        );
    }

    #[test]
    fn test_hint_contains_install_instructions() {
        let err = ActualError::ClaudeNotFound;
        assert_eq!(
            err.hint(),
            Some("npm install -g @anthropic-ai/claude-code"),
            "Hint should contain install instructions"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_env_var_override_with_valid_path() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let fake_binary = dir.path().join("fake-claude");
        std::fs::write(&fake_binary, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&fake_binary, std::fs::Permissions::from_mode(0o755)).unwrap();

        let _guard = EnvGuard::set("CLAUDE_BINARY", fake_binary.to_str().unwrap());
        let result = find_claude_binary();

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), fake_binary);
    }

    #[test]
    fn test_env_var_override_with_nonexistent_path() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/path/to/claude");
        let result = find_claude_binary();

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::ClaudeNotFound));
    }

    #[test]
    fn test_not_found_when_binary_missing() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Ensure CLAUDE_BINARY is absent so we fall through to PATH lookup,
        // then blank PATH so `claude` cannot be found.
        let _g1 = EnvGuard::remove("CLAUDE_BINARY");
        let _g2 = EnvGuard::set("PATH", "");
        let result = find_claude_binary();

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::ClaudeNotFound));
    }

    #[test]
    #[cfg(unix)]
    fn test_env_var_override_with_non_executable() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let non_exec = dir.path().join("claude-not-executable");
        // Create the file but do NOT set executable bit
        std::fs::write(&non_exec, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&non_exec, std::fs::Permissions::from_mode(0o644)).unwrap();

        let _guard = EnvGuard::set("CLAUDE_BINARY", non_exec.to_str().unwrap());
        let result = find_claude_binary();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::ClaudeNotFound));
    }

    #[test]
    #[cfg(unix)]
    fn test_env_var_override_with_directory_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();

        let _guard = EnvGuard::set("CLAUDE_BINARY", dir.path().to_str().unwrap());
        let result = find_claude_binary();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::ClaudeNotFound));
    }

    /// Integration test: only runs when `claude` is actually on PATH.
    /// Run with: cargo test --features integration -- --ignored
    #[test]
    #[ignore = "requires Claude Code to be installed on PATH"]
    #[cfg(feature = "integration")]
    fn test_find_claude_binary_on_path() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("CLAUDE_BINARY");
        let result = find_claude_binary();
        assert!(
            result.is_ok(),
            "Claude Code should be installed for integration tests"
        );
        let path = result.unwrap();
        assert!(
            path.exists(),
            "Returned path should exist: {}",
            path.display()
        );
    }
}
