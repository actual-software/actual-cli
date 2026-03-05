use crate::config::HooksConfig;
use crate::error::{Result, SymphonyError};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tracing::{info, warn};

/// Result of workspace preparation.
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
            info!(
                workspace = %workspace_path.display(),
                "running after_create hook"
            );
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
        info!(
            workspace = %workspace_path.display(),
            "running before_run hook"
        );
        run_hook("before_run", script, workspace_path, hooks.timeout_ms).await?;
    }
    Ok(())
}

/// Run the after_run hook (best-effort, errors logged and ignored).
pub async fn run_after_run_hook(workspace_path: &Path, hooks: &HooksConfig) {
    if let Some(ref script) = hooks.after_run {
        info!(
            workspace = %workspace_path.display(),
            "running after_run hook"
        );
        if let Err(e) = run_hook("after_run", script, workspace_path, hooks.timeout_ms).await {
            warn!(
                workspace = %workspace_path.display(),
                error = %e,
                "after_run hook failed (ignored)"
            );
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
        info!(
            workspace = %workspace_path.display(),
            "running before_remove hook"
        );
        if let Err(e) = run_hook("before_remove", script, workspace_path, hooks.timeout_ms).await {
            warn!(
                workspace = %workspace_path.display(),
                error = %e,
                "before_remove hook failed (ignored)"
            );
        }
    }

    // Remove the workspace directory
    if let Err(e) = std::fs::remove_dir_all(workspace_path) {
        warn!(
            workspace = %workspace_path.display(),
            error = %e,
            "failed to remove workspace directory"
        );
    } else {
        info!(
            workspace = %workspace_path.display(),
            "workspace removed"
        );
    }
}

/// Execute a hook script in a workspace directory with timeout.
async fn run_hook(hook_name: &str, script: &str, cwd: &Path, timeout_ms: u64) -> Result<()> {
    let timeout = Duration::from_millis(timeout_ms);

    let result = tokio::time::timeout(timeout, async {
        let output = Command::new("bash")
            .arg("-lc")
            .arg(script)
            .current_dir(cwd)
            .output()
            .await
            .map_err(|e| SymphonyError::WorkspaceHookFailed {
                hook: hook_name.to_string(),
                reason: format!("failed to execute: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let truncated = if stderr.len() > 500 {
                format!("{}...(truncated)", &stderr[..500])
            } else {
                stderr.to_string()
            };
            return Err(SymphonyError::WorkspaceHookFailed {
                hook: hook_name.to_string(),
                reason: format!("exit code {:?}: {}", output.status.code(), truncated),
            });
        }

        Ok(())
    })
    .await;

    match result {
        Ok(inner) => inner,
        Err(_) => Err(SymphonyError::WorkspaceHookTimedOut {
            hook: hook_name.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
