use crate::config::HooksConfig;
use crate::error::{Result, SymphonyError};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tracing::{info, warn};

// ── Logging helpers (single-line to avoid LLVM coverage region splits) ────

fn log_hook(hook_name: &str, workspace: &Path) {
    let ws = workspace.display().to_string();
    info!(workspace = %ws, "running {hook_name} hook");
}

fn log_hook_failed(hook_name: &str, workspace: &Path, error: &dyn std::fmt::Display) {
    let ws = workspace.display().to_string();
    warn!(workspace = %ws, error = %error, "{hook_name} hook failed (ignored)");
}

fn log_workspace_removed(workspace: &Path) {
    let ws = workspace.display().to_string();
    info!(workspace = %ws, "workspace removed");
}

fn log_workspace_remove_failed(workspace: &Path, error: &dyn std::fmt::Display) {
    let ws = workspace.display().to_string();
    warn!(workspace = %ws, error = %error, "failed to remove workspace directory");
}

/// Result of workspace preparation.
#[derive(Debug)]
pub struct WorkspaceResult {
    pub path: PathBuf,
    pub workspace_key: String,
    pub created_now: bool,
}

/// Sanitize an issue identifier for use as a workspace directory name.
/// Only [A-Za-z0-9._-] are allowed; everything else becomes `_`.
pub fn sanitize_workspace_key(identifier: &str) -> String {
    identifier
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Validate that a workspace path is under the workspace root.
pub fn validate_workspace_path(workspace_path: &Path, workspace_root: &Path) -> Result<()> {
    let abs_workspace =
        std::fs::canonicalize(workspace_path).unwrap_or_else(|_| workspace_path.to_path_buf());
    let abs_root =
        std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());

    if !abs_workspace.starts_with(&abs_root) {
        return Err(SymphonyError::WorkspacePathOutsideRoot {
            workspace: abs_workspace.display().to_string(),
            root: abs_root.display().to_string(),
        });
    }

    Ok(())
}

/// Create or reuse a workspace for an issue.
pub async fn create_workspace(
    workspace_root: &Path,
    identifier: &str,
    hooks: &HooksConfig,
) -> Result<WorkspaceResult> {
    let workspace_key = sanitize_workspace_key(identifier);
    let workspace_path = workspace_root.join(&workspace_key);

    // Ensure workspace root exists
    std::fs::create_dir_all(workspace_root).map_err(|e| {
        SymphonyError::WorkspaceCreationFailed {
            reason: format!(
                "failed to create workspace root {}: {}",
                workspace_root.display(),
                e
            ),
        }
    })?;

    let created_now = if workspace_path.exists() {
        if !workspace_path.is_dir() {
            return Err(SymphonyError::WorkspaceCreationFailed {
                reason: format!(
                    "path exists but is not a directory: {}",
                    workspace_path.display()
                ),
            });
        }
        false
    } else {
        std::fs::create_dir_all(&workspace_path).map_err(|e| {
            SymphonyError::WorkspaceCreationFailed {
                reason: format!(
                    "failed to create workspace {}: {}",
                    workspace_path.display(),
                    e
                ),
            }
        })?;
        true
    };

    // Validate containment
    validate_workspace_path(&workspace_path, workspace_root)?;

    // Run after_create hook if workspace was newly created
    if created_now {
        if let Some(ref script) = hooks.after_create {
            log_hook("after_create", &workspace_path);
            run_hook("after_create", script, &workspace_path, hooks.timeout_ms).await?;
        }
    }

    Ok(WorkspaceResult {
        path: workspace_path,
        workspace_key,
        created_now,
    })
}

/// Run the before_run hook.
pub async fn run_before_run_hook(workspace_path: &Path, hooks: &HooksConfig) -> Result<()> {
    if let Some(ref script) = hooks.before_run {
        log_hook("before_run", workspace_path);
        run_hook("before_run", script, workspace_path, hooks.timeout_ms).await?;
    }
    Ok(())
}

/// Run the after_run hook (best-effort, errors logged and ignored).
pub async fn run_after_run_hook(workspace_path: &Path, hooks: &HooksConfig) {
    if let Some(ref script) = hooks.after_run {
        log_hook("after_run", workspace_path);
        if let Err(e) = run_hook("after_run", script, workspace_path, hooks.timeout_ms).await {
            log_hook_failed("after_run", workspace_path, &e);
        }
    }
}

/// Clean up a workspace directory.
pub async fn cleanup_workspace(workspace_path: &Path, hooks: &HooksConfig) {
    if !workspace_path.exists() {
        return;
    }

    // Run before_remove hook (best-effort)
    if let Some(ref script) = hooks.before_remove {
        log_hook("before_remove", workspace_path);
        if let Err(e) = run_hook("before_remove", script, workspace_path, hooks.timeout_ms).await {
            log_hook_failed("before_remove", workspace_path, &e);
        }
    }

    // Remove the workspace directory
    if let Err(e) = std::fs::remove_dir_all(workspace_path) {
        log_workspace_remove_failed(workspace_path, &e);
    } else {
        log_workspace_removed(workspace_path);
    }
}

/// Execute a hook script in a workspace directory with timeout.
///
/// On timeout the child process is explicitly killed to prevent orphans.
async fn run_hook(hook_name: &str, script: &str, cwd: &Path, timeout_ms: u64) -> Result<()> {
    let timeout = Duration::from_millis(timeout_ms);

    let mut child = Command::new("bash")
        .arg("-lc")
        .arg(script)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| SymphonyError::WorkspaceHookFailed {
            hook: hook_name.to_string(),
            reason: format!("failed to execute: {e}"),
        })?;

    // Use child.wait() (borrows) instead of child.wait_with_output() (consumes)
    // so we can still call child.kill() on timeout.
    let wait_result = tokio::time::timeout(timeout, child.wait()).await;

    handle_hook_result(hook_name, wait_result, &mut child).await
}

/// Interpret the result of waiting for a hook process.
///
/// Separated from `run_hook` so every branch is directly testable.
async fn handle_hook_result(
    hook_name: &str,
    wait_result: std::result::Result<
        std::io::Result<std::process::ExitStatus>,
        tokio::time::error::Elapsed,
    >,
    child: &mut tokio::process::Child,
) -> Result<()> {
    match wait_result {
        Ok(Ok(status)) => {
            if !status.success() {
                // Collect stderr for diagnostics after the process exits.
                // stderr is always present because we configure Stdio::piped() above.
                let stderr_bytes = match child.stderr.take() {
                    Some(mut handle) => {
                        let mut buf = Vec::new();
                        let _ = tokio::io::AsyncReadExt::read_to_end(&mut handle, &mut buf).await;
                        buf
                    }
                    None => Vec::new(),
                };
                let stderr = String::from_utf8_lossy(&stderr_bytes);
                let truncated = if stderr.len() > 500 {
                    format!("{}...(truncated)", &stderr[..500])
                } else {
                    stderr.to_string()
                };
                return Err(SymphonyError::WorkspaceHookFailed {
                    hook: hook_name.to_string(),
                    reason: format!("exit code {:?}: {}", status.code(), truncated),
                });
            }
            Ok(())
        }
        Ok(Err(e)) => Err(SymphonyError::WorkspaceHookFailed {
            hook: hook_name.to_string(),
            reason: format!("failed to wait: {e}"),
        }),
        Err(_) => {
            // Timeout — kill the child to prevent orphaned processes
            let _ = child.kill().await;
            Err(SymphonyError::WorkspaceHookTimedOut {
                hook: hook_name.to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_test::traced_test;

    #[test]
    fn test_sanitize_workspace_key() {
        assert_eq!(sanitize_workspace_key("ABC-123"), "ABC-123");
        assert_eq!(sanitize_workspace_key("ABC 123"), "ABC_123");
        assert_eq!(sanitize_workspace_key("feat/my-branch"), "feat_my-branch");
        assert_eq!(sanitize_workspace_key("a.b_c-d"), "a.b_c-d");
        assert_eq!(sanitize_workspace_key("hello@world!"), "hello_world_");
    }

    #[test]
    fn test_validate_workspace_path_valid() {
        let tmp = std::env::temp_dir();
        let root = tmp.join("test_ws_root");
        let workspace = root.join("PROJ-1");
        std::fs::create_dir_all(&workspace).ok();

        let result = validate_workspace_path(&workspace, &root);
        assert!(result.is_ok());

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn test_create_workspace_new() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspaces");
        let hooks = HooksConfig {
            after_create: None,
            before_run: None,
            after_run: None,
            before_remove: None,
            timeout_ms: 60_000,
        };

        let result = create_workspace(&root, "PROJ-1", &hooks).await.unwrap();
        assert!(result.created_now);
        assert_eq!(result.workspace_key, "PROJ-1");
        assert!(result.path.exists());
    }

    #[tokio::test]
    async fn test_create_workspace_reuse() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspaces");
        let hooks = HooksConfig {
            after_create: None,
            before_run: None,
            after_run: None,
            before_remove: None,
            timeout_ms: 60_000,
        };

        // Create first time
        let r1 = create_workspace(&root, "PROJ-2", &hooks).await.unwrap();
        assert!(r1.created_now);

        // Reuse
        let r2 = create_workspace(&root, "PROJ-2", &hooks).await.unwrap();
        assert!(!r2.created_now);
        assert_eq!(r1.path, r2.path);
    }

    #[traced_test]
    #[tokio::test]
    async fn test_create_workspace_with_after_create_hook() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspaces");
        let hooks = HooksConfig {
            after_create: Some("touch .initialized".to_string()),
            before_run: None,
            after_run: None,
            before_remove: None,
            timeout_ms: 60_000,
        };

        let result = create_workspace(&root, "PROJ-3", &hooks).await.unwrap();
        assert!(result.created_now);
        assert!(result.path.join(".initialized").exists());
    }

    #[traced_test]
    #[tokio::test]
    async fn test_cleanup_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let ws_path = tmp.path().join("to-remove");
        std::fs::create_dir_all(&ws_path).unwrap();
        std::fs::write(ws_path.join("file.txt"), "data").unwrap();

        let hooks = HooksConfig {
            after_create: None,
            before_run: None,
            after_run: None,
            before_remove: None,
            timeout_ms: 60_000,
        };

        cleanup_workspace(&ws_path, &hooks).await;
        assert!(!ws_path.exists());
    }

    #[test]
    fn test_validate_workspace_path_outside_root() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();

        let result = validate_workspace_path(outside.path(), root.path());
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("outside root"));
    }

    #[tokio::test]
    async fn test_create_workspace_path_is_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspaces");
        std::fs::create_dir_all(&root).unwrap();
        // Place a regular file where the workspace directory would be
        std::fs::write(root.join("PROJ-FILE"), "not a directory").unwrap();

        let hooks = HooksConfig {
            after_create: None,
            before_run: None,
            after_run: None,
            before_remove: None,
            timeout_ms: 60_000,
        };

        let result = create_workspace(&root, "PROJ-FILE", &hooks).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("not a directory"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_before_run_hook_some() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = HooksConfig {
            after_create: None,
            before_run: Some("touch .before_run_executed".to_string()),
            after_run: None,
            before_remove: None,
            timeout_ms: 60_000,
        };

        run_before_run_hook(tmp.path(), &hooks).await.unwrap();
        assert!(tmp.path().join(".before_run_executed").exists());
    }

    #[tokio::test]
    async fn test_run_before_run_hook_none() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = HooksConfig {
            after_create: None,
            before_run: None,
            after_run: None,
            before_remove: None,
            timeout_ms: 60_000,
        };

        // Should return Ok immediately with no hook configured
        run_before_run_hook(tmp.path(), &hooks).await.unwrap();
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_after_run_hook_success() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = HooksConfig {
            after_create: None,
            before_run: None,
            after_run: Some("touch .after_run_ok".to_string()),
            before_remove: None,
            timeout_ms: 60_000,
        };

        run_after_run_hook(tmp.path(), &hooks).await;
        assert!(tmp.path().join(".after_run_ok").exists());
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_after_run_hook_failure_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = HooksConfig {
            after_create: None,
            before_run: None,
            after_run: Some("exit 1".to_string()),
            before_remove: None,
            timeout_ms: 60_000,
        };

        // Should return normally even though the hook fails
        run_after_run_hook(tmp.path(), &hooks).await;
    }

    #[tokio::test]
    async fn test_run_after_run_hook_none() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = HooksConfig {
            after_create: None,
            before_run: None,
            after_run: None,
            before_remove: None,
            timeout_ms: 60_000,
        };

        // Should return immediately with no hook configured
        run_after_run_hook(tmp.path(), &hooks).await;
    }

    #[traced_test]
    #[tokio::test]
    async fn test_cleanup_workspace_with_before_remove_hook() {
        let tmp = tempfile::tempdir().unwrap();
        let ws_path = tmp.path().join("ws-hook-remove");
        std::fs::create_dir_all(&ws_path).unwrap();
        std::fs::write(ws_path.join("data.txt"), "content").unwrap();

        let marker = tmp.path().join("hook_ran");
        let hooks = HooksConfig {
            after_create: None,
            before_run: None,
            after_run: None,
            before_remove: Some(format!("touch {}", marker.display())),
            timeout_ms: 60_000,
        };

        cleanup_workspace(&ws_path, &hooks).await;
        assert!(marker.exists());
        assert!(!ws_path.exists());
    }

    #[tokio::test]
    async fn test_cleanup_workspace_nonexistent_path() {
        let tmp = tempfile::tempdir().unwrap();
        let ws_path = tmp.path().join("does-not-exist");

        let hooks = HooksConfig {
            after_create: None,
            before_run: None,
            after_run: None,
            before_remove: None,
            timeout_ms: 60_000,
        };

        // Should return immediately without error
        cleanup_workspace(&ws_path, &hooks).await;
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_hook_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = HooksConfig {
            after_create: None,
            before_run: Some("exit 1".to_string()),
            after_run: None,
            before_remove: None,
            timeout_ms: 60_000,
        };

        let result = run_before_run_hook(tmp.path(), &hooks).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("hook failed"));
    }

    #[tokio::test]
    async fn test_run_hook_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = HooksConfig {
            after_create: None,
            before_run: Some("sleep 999".to_string()),
            after_run: None,
            before_remove: None,
            timeout_ms: 100,
        };

        let result = run_before_run_hook(tmp.path(), &hooks).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("timed out"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_hook_stderr_truncation() {
        let tmp = tempfile::tempdir().unwrap();
        // Generate >500 chars of stderr then exit 1
        let script = "python3 -c \"import sys; sys.stderr.write('X' * 600)\" && exit 1";
        let hooks = HooksConfig {
            after_create: None,
            before_run: Some(script.to_string()),
            after_run: None,
            before_remove: None,
            timeout_ms: 60_000,
        };

        let result = run_before_run_hook(tmp.path(), &hooks).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("...(truncated)"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_create_workspace_root_creation_failure() {
        // Use /dev/null as parent — can't create subdirectories under a device file
        let root = PathBuf::from("/dev/null/impossible");
        let hooks = HooksConfig {
            after_create: None,
            before_run: None,
            after_run: None,
            before_remove: None,
            timeout_ms: 60_000,
        };

        let result = create_workspace(&root, "PROJ-1", &hooks).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("failed to create workspace root"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_create_workspace_dir_creation_failure() {
        // Create root then make it read-only so workspace dir creation fails
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspaces");
        std::fs::create_dir_all(&root).unwrap();

        // Make root read-only to prevent creating subdirectory
        let mut perms = std::fs::metadata(&root).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        {
            perms.set_readonly(true);
        }
        std::fs::set_permissions(&root, perms.clone()).unwrap();

        let hooks = HooksConfig {
            after_create: None,
            before_run: None,
            after_run: None,
            before_remove: None,
            timeout_ms: 60_000,
        };

        let result = create_workspace(&root, "PROJ-NEW", &hooks).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("failed to create workspace"));

        // Restore permissions for cleanup
        #[allow(clippy::permissions_set_readonly_false)]
        {
            perms.set_readonly(false);
        }
        std::fs::set_permissions(&root, perms).unwrap();
    }

    #[traced_test]
    #[tokio::test]
    async fn test_cleanup_workspace_with_failing_before_remove_hook() {
        let tmp = tempfile::tempdir().unwrap();
        let ws_path = tmp.path().join("ws-fail-hook");
        std::fs::create_dir_all(&ws_path).unwrap();

        let hooks = HooksConfig {
            after_create: None,
            before_run: None,
            after_run: None,
            before_remove: Some("exit 1".to_string()),
            timeout_ms: 60_000,
        };

        // Should still proceed to remove the directory despite hook failure
        cleanup_workspace(&ws_path, &hooks).await;
        assert!(!ws_path.exists());
    }

    #[traced_test]
    #[tokio::test]
    async fn test_cleanup_workspace_remove_dir_all_failure() {
        // Create a workspace but make it impossible to remove by removing permissions
        // On macOS: remove write permission from parent to prevent deletion
        let tmp = tempfile::tempdir().unwrap();
        let ws_path = tmp.path().join("ws-no-delete");
        std::fs::create_dir_all(&ws_path).unwrap();

        // Make the directory immutable by removing write from parent
        let parent = tmp.path();
        let mut perms = std::fs::metadata(parent).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        {
            perms.set_readonly(true);
        }
        std::fs::set_permissions(parent, perms.clone()).unwrap();

        let hooks = HooksConfig {
            after_create: None,
            before_run: None,
            after_run: None,
            before_remove: None,
            timeout_ms: 60_000,
        };

        // cleanup should log warning but not panic
        cleanup_workspace(&ws_path, &hooks).await;

        // Restore permissions for cleanup
        #[allow(clippy::permissions_set_readonly_false)]
        {
            perms.set_readonly(false);
        }
        std::fs::set_permissions(parent, perms).unwrap();
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_hook_spawn_failure() {
        // Use a non-existent directory as cwd to make bash spawn fail
        let nonexistent = PathBuf::from("/tmp/symphony_test_nonexistent_dir_12345");
        // Ensure it really doesn't exist
        let _ = std::fs::remove_dir_all(&nonexistent);

        let result = run_hook("test_hook", "echo hi", &nonexistent, 60_000).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("failed to execute"));
    }

    // ── handle_hook_result unit tests ────────────────────────────────

    #[tokio::test]
    async fn test_handle_hook_result_wait_io_error() {
        // Simulate child.wait() returning an I/O error
        let io_err = std::io::Error::new(std::io::ErrorKind::Other, "wait interrupted");
        let mut child = Command::new("true").spawn().expect("true should spawn");
        let _ = child.wait().await; // let it finish so we don't leak

        let result = handle_hook_result("test_hook", Ok(Err(io_err)), &mut child).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("failed to wait"));
        assert!(msg.contains("wait interrupted"));
    }

    #[tokio::test]
    async fn test_handle_hook_result_failure_no_stderr_handle() {
        // Simulate a failed exit status where stderr has already been taken
        use std::os::unix::process::ExitStatusExt;
        let status = std::process::ExitStatus::from_raw(256); // exit code 1

        let mut child = Command::new("true")
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("true should spawn");
        let _ = child.wait().await;
        // Take stderr so the handle_hook_result hits the None branch
        let _ = child.stderr.take();

        let result = handle_hook_result("test_hook", Ok(Ok(status)), &mut child).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("exit code"));
    }
}
