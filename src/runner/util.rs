use std::path::PathBuf;
use std::sync::OnceLock;

use regex::Regex;

use crate::error::ActualError;

/// Compiled regex for validating model names: alphanumeric start, then
/// alphanumeric, dots, underscores, slashes, or hyphens, up to 100 chars total.
static MODEL_NAME_RE: OnceLock<Regex> = OnceLock::new();

pub(crate) fn model_name_regex() -> &'static Regex {
    MODEL_NAME_RE
        .get_or_init(|| Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._/\-]{0,99}$").expect("valid regex"))
}

/// Strip markdown JSON code fences from a string if present.
///
/// Handles both ` ```json\n...\n``` ` and ` ```\n...\n``` ` patterns.
/// Returns the inner content if fences are found, otherwise returns the
/// original string trimmed.
pub(crate) fn strip_markdown_json_fences(s: &str) -> &str {
    let trimmed = s.trim();

    // Try ```json first, then bare ```
    for prefix in &["```json", "```"] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            // Skip optional newline after opening fence
            let rest = rest.strip_prefix('\n').unwrap_or(rest);
            if let Some(inner) = rest.strip_suffix("```") {
                return inner.trim();
            }
        }
    }

    trimmed
}

/// Locate a binary on the system.
///
/// Resolution order:
/// 1. If `env_var` is set, use that path (must exist, be a file, and be executable on unix).
/// 2. Otherwise, search PATH for each name in `binary_names` in order, returning the first found.
///
/// Returns the absolute path to the binary on success, or `not_found()` if the
/// binary cannot be located or the env-var path is invalid.
pub(crate) fn find_binary(
    env_var: &str,
    binary_names: &[&str],
    not_found: fn() -> ActualError,
) -> Result<PathBuf, ActualError> {
    if let Ok(path_str) = std::env::var(env_var) {
        let path = PathBuf::from(&path_str);
        if !path.exists() {
            return Err(not_found());
        }
        if !path.is_file() {
            return Err(not_found());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .map(|m| m.permissions().mode())
                .unwrap_or(0);
            if mode & 0o111 == 0 {
                return Err(not_found());
            }
        }
        return Ok(path);
    }
    for name in binary_names {
        if let Ok(path) = which::which(name) {
            return Ok(path);
        }
    }
    Err(not_found())
}

/// Spawn a child process, retrying briefly on `ETXTBSY` ("text file busy").
///
/// On Linux/macOS a freshly written executable can transiently report `ETXTBSY`
/// when a concurrent `fork` in another thread still holds a writable fd to it
/// across the fork→exec window. This is common in parallel test runs (a fixture
/// script written then immediately spawned) and possible in production when a
/// runner binary was just installed or updated. A short, bounded retry resolves
/// it deterministically. Callers pass the spawn as a closure, e.g.
/// `spawn_with_etxtbsy_retry(|| cmd.spawn()).await`.
pub(crate) async fn spawn_with_etxtbsy_retry<T, F>(mut attempt: F) -> std::io::Result<T>
where
    F: FnMut() -> std::io::Result<T>,
{
    const MAX_ATTEMPTS: u32 = 5;
    let mut tries: u32 = 1;
    loop {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(e) if tries < MAX_ATTEMPTS && is_text_file_busy(&e) => {
                let backoff = std::time::Duration::from_millis(u64::from(20 * tries));
                tokio::time::sleep(backoff).await;
                tries += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// `true` when an I/O error is `ETXTBSY` (text file busy — errno 26 on Linux and macOS).
fn is_text_file_busy(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(26)
}

#[cfg(test)]
mod etxtbsy_retry_tests {
    use super::*;
    use std::cell::Cell;
    use std::io::{Error, Result};

    #[tokio::test]
    async fn returns_value_on_first_success() {
        let out: Result<u8> = spawn_with_etxtbsy_retry(|| Ok(7)).await;
        assert_eq!(out.unwrap(), 7);
    }

    #[tokio::test]
    async fn retries_on_etxtbsy_then_succeeds() {
        let calls = Cell::new(0u32);
        let out: Result<u8> = spawn_with_etxtbsy_retry(|| {
            calls.set(calls.get() + 1);
            if calls.get() < 3 {
                Err(Error::from_raw_os_error(26)) // ETXTBSY
            } else {
                Ok(42)
            }
        })
        .await;
        assert_eq!(out.unwrap(), 42);
        assert_eq!(calls.get(), 3);
    }

    #[tokio::test]
    async fn non_etxtbsy_error_is_not_retried() {
        let calls = Cell::new(0u32);
        let out: Result<u8> = spawn_with_etxtbsy_retry(|| {
            calls.set(calls.get() + 1);
            Err(Error::from_raw_os_error(2)) // ENOENT
        })
        .await;
        assert_eq!(out.unwrap_err().raw_os_error(), Some(2));
        assert_eq!(calls.get(), 1, "non-ETXTBSY must not retry");
    }

    #[tokio::test]
    async fn exhausts_retries_and_returns_last_error() {
        let calls = Cell::new(0u32);
        let out: Result<u8> = spawn_with_etxtbsy_retry(|| {
            calls.set(calls.get() + 1);
            Err(Error::from_raw_os_error(26)) // always ETXTBSY
        })
        .await;
        assert_eq!(out.unwrap_err().raw_os_error(), Some(26));
        assert_eq!(calls.get(), 5);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

    // ---- model_name_regex tests ----

    #[test]
    fn test_model_name_regex_valid_names() {
        let re = model_name_regex();
        for name in &[
            "gpt-5",
            "claude-4.6-sonnet",
            "a",
            "openai/gpt-5",
            "provider/model_v2.1",
            "auto",
            "gpt-5.2",
        ] {
            assert!(re.is_match(name), "expected match for {name:?}");
        }
    }

    #[test]
    fn test_model_name_regex_invalid_names() {
        let re = model_name_regex();
        for name in &[
            "--flag",
            "model name",
            "model;cmd",
            "model|cmd",
            "model$var",
            "",
        ] {
            assert!(!re.is_match(name), "expected no match for {name:?}");
        }
    }

    // ---- strip_markdown_json_fences tests ----

    #[test]
    fn test_strip_fences_json_block() {
        let input = "```json\n{\"key\": \"value\"}\n```";
        assert_eq!(strip_markdown_json_fences(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn test_strip_fences_bare_block() {
        let input = "```\n{\"key\": \"value\"}\n```";
        assert_eq!(strip_markdown_json_fences(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn test_strip_fences_no_fences() {
        let input = r#"{"key": "value"}"#;
        assert_eq!(strip_markdown_json_fences(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn test_strip_fences_with_surrounding_whitespace() {
        let input = "  \n```json\n{\"key\": \"value\"}\n```\n  ";
        assert_eq!(strip_markdown_json_fences(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn test_strip_fences_bare_block_no_newline() {
        // Bare ``` immediately followed by content (no newline after opening fence)
        let input = "```{\"key\": \"value\"}\n```";
        assert_eq!(strip_markdown_json_fences(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn test_strip_fences_opening_fence_no_closing() {
        // Opening ```json found but no closing ``` — returns trimmed input as-is
        let input = "```json\n{\"key\": \"value\"}";
        assert_eq!(
            strip_markdown_json_fences(input),
            "```json\n{\"key\": \"value\"}"
        );
    }

    // ---- find_binary tests ----

    #[test]
    fn test_find_binary_env_nonexistent_path() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("TEST_UTIL_BINARY", "/nonexistent/path/to/binary");
        let result = find_binary("TEST_UTIL_BINARY", &["sh"], || ActualError::ClaudeNotFound);
        assert!(matches!(result.unwrap_err(), ActualError::ClaudeNotFound));
    }

    #[test]
    #[cfg(unix)]
    fn test_find_binary_env_directory_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let _guard = EnvGuard::set("TEST_UTIL_BINARY", dir.path().to_str().unwrap());
        let result = find_binary("TEST_UTIL_BINARY", &["sh"], || ActualError::ClaudeNotFound);
        assert!(matches!(result.unwrap_err(), ActualError::ClaudeNotFound));
    }

    #[test]
    #[cfg(unix)]
    fn test_find_binary_env_non_executable() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let non_exec = dir.path().join("not-executable");
        std::fs::write(&non_exec, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&non_exec, std::fs::Permissions::from_mode(0o644)).unwrap();

        let _guard = EnvGuard::set("TEST_UTIL_BINARY", non_exec.to_str().unwrap());
        let result = find_binary("TEST_UTIL_BINARY", &["sh"], || ActualError::ClaudeNotFound);
        assert!(matches!(result.unwrap_err(), ActualError::ClaudeNotFound));
    }

    #[test]
    #[cfg(unix)]
    fn test_find_binary_env_valid_executable() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("fake-binary");
        std::fs::write(&binary, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();

        let _guard = EnvGuard::set("TEST_UTIL_BINARY", binary.to_str().unwrap());
        let result = find_binary("TEST_UTIL_BINARY", &["sh"], || ActualError::ClaudeNotFound);
        assert_eq!(result.unwrap(), binary);
    }

    #[test]
    fn test_find_binary_path_not_found() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("TEST_UTIL_BINARY");
        let _g2 = EnvGuard::set("PATH", "");
        let result = find_binary(
            "TEST_UTIL_BINARY",
            &["definitely_not_a_real_binary_xyz_12345"],
            || ActualError::ClaudeNotFound,
        );
        assert!(matches!(result.unwrap_err(), ActualError::ClaudeNotFound));
    }

    #[test]
    #[cfg(unix)]
    fn test_find_binary_path_found() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("TEST_UTIL_BINARY");
        // `sh` is always available on unix systems
        let result = find_binary("TEST_UTIL_BINARY", &["sh"], || ActualError::ClaudeNotFound);
        assert!(result.is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn test_find_binary_multiple_names_fallback() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("TEST_UTIL_BINARY");
        // First name doesn't exist, second name (sh) does
        let result = find_binary(
            "TEST_UTIL_BINARY",
            &["definitely_not_a_real_binary_xyz_12345", "sh"],
            || ActualError::ClaudeNotFound,
        );
        assert!(result.is_ok());
    }
}
