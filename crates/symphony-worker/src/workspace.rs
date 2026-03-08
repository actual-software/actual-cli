use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("failed to clone repository '{url}' to '{path}': {reason}")]
    CloneFailed {
        url: String,
        path: String,
        reason: String,
    },

    #[error("failed to fetch in '{path}': {reason}")]
    FetchFailed { path: String, reason: String },

    #[error("failed to create worktree at '{path}': {reason}")]
    WorktreeCreationFailed { path: String, reason: String },

    #[error("failed to remove worktree at '{path}': {reason}")]
    WorktreeRemovalFailed { path: String, reason: String },

    #[error("base clone path has no parent directory: {path}")]
    NoParentDirectory { path: String },

    #[error("failed to create directory '{path}': {reason}")]
    DirectoryCreationFailed { path: String, reason: String },
}

pub type Result<T> = std::result::Result<T, WorkspaceError>;

// ---------------------------------------------------------------------------
// GitCommandRunner trait
// ---------------------------------------------------------------------------

/// Abstraction over git command execution for testability.
///
/// Accepts `&[String]` rather than `&[&str]` so that `mockall` can generate
/// a mock without running into nested-lifetime issues.
#[cfg_attr(test, mockall::automock)]
#[async_trait::async_trait]
pub trait GitCommandRunner: Send + Sync {
    async fn run_git(&self, cwd: &Path, args: &[String]) -> std::result::Result<String, String>;
}

// ---------------------------------------------------------------------------
// RealGitCommandRunner
// ---------------------------------------------------------------------------

pub struct RealGitCommandRunner;

#[async_trait::async_trait]
impl GitCommandRunner for RealGitCommandRunner {
    async fn run_git(&self, cwd: &Path, args: &[String]) -> std::result::Result<String, String> {
        let output = tokio::process::Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .await
            .map_err(|e| format!("failed to spawn git: {e}"))?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helper: convert &[&str] to Vec<String> for the trait call
// ---------------------------------------------------------------------------

fn to_strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_owned()).collect()
}

// ---------------------------------------------------------------------------
// Logging helpers (avoid LLVM coverage splits in tracing macros)
// ---------------------------------------------------------------------------

fn log_clone(url: &str, path: &Path) {
    let p = path.display().to_string();
    info!(url = %url, path = %p, "cloning repository");
}

fn log_fetch(path: &Path) {
    let p = path.display().to_string();
    info!(path = %p, "fetching latest from origin");
}

fn log_fetch_main(path: &Path) {
    let p = path.display().to_string();
    debug!(path = %p, "fetching origin/main");
}

fn log_worktree_add(worktree_path: &Path, branch: &str) {
    let p = worktree_path.display().to_string();
    info!(path = %p, branch = %branch, "creating worktree");
}

fn log_worktree_remove(worktree_path: &Path) {
    let p = worktree_path.display().to_string();
    info!(path = %p, "removing worktree");
}

fn log_branch_delete_failed(branch: &str, reason: &str) {
    warn!(branch = %branch, reason = %reason, "best-effort branch delete failed");
}

fn log_prune_failed(reason: &str) {
    warn!(reason = %reason, "best-effort worktree prune failed");
}

fn log_creating_parent(parent: &Path) {
    let p = parent.display().to_string();
    debug!(path = %p, "creating parent directory");
}

fn log_creating_worktree_root(root: &Path) {
    let p = root.display().to_string();
    debug!(path = %p, "creating worktree root directory");
}

/// Helper to ensure a directory exists, creating it (and parents) if needed.
fn ensure_dir_exists(dir: &Path, log_fn: fn(&Path)) -> Result<()> {
    if !dir.exists() {
        log_fn(dir);
        std::fs::create_dir_all(dir).map_err(|e| WorkspaceError::DirectoryCreationFailed {
            path: dir.to_string_lossy().into_owned(),
            reason: e.to_string(),
        })?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Ensure a shared base clone exists at `base_clone_path`.
///
/// If the path already contains a `.git` directory, fetches the latest
/// changes. Otherwise, performs a fresh clone from `repo_url`.
pub async fn ensure_base_clone(
    git: &dyn GitCommandRunner,
    repo_url: &str,
    base_clone_path: &Path,
) -> Result<()> {
    if base_clone_path.join(".git").exists() {
        return fetch_latest(git, base_clone_path).await;
    }

    // Create parent directory if it does not exist.
    let parent = base_clone_path
        .parent()
        .ok_or_else(|| WorkspaceError::NoParentDirectory {
            path: base_clone_path.to_string_lossy().into_owned(),
        })?;

    ensure_dir_exists(parent, log_creating_parent)?;

    log_clone(repo_url, base_clone_path);

    let path_str = base_clone_path.to_string_lossy();
    git.run_git(parent, &to_strings(&["clone", repo_url, &path_str]))
        .await
        .map_err(|reason| WorkspaceError::CloneFailed {
            url: repo_url.to_string(),
            path: path_str.into_owned(),
            reason,
        })?;

    Ok(())
}

/// Fetch the latest changes from `origin` in an existing base clone.
pub async fn fetch_latest(git: &dyn GitCommandRunner, base_clone_path: &Path) -> Result<()> {
    log_fetch(base_clone_path);

    git.run_git(base_clone_path, &to_strings(&["fetch", "origin"]))
        .await
        .map_err(|reason| WorkspaceError::FetchFailed {
            path: base_clone_path.to_string_lossy().into_owned(),
            reason,
        })?;

    Ok(())
}

/// Create a new git worktree for a job.
///
/// The worktree is placed at `worktree_root/<issue_id>` and tracks
/// `origin/main` via a new branch named `branch_name`.
pub async fn create_worktree(
    git: &dyn GitCommandRunner,
    base_clone_path: &Path,
    worktree_root: &Path,
    issue_id: &str,
    branch_name: &str,
) -> Result<PathBuf> {
    // Fetch origin/main first.
    log_fetch_main(base_clone_path);
    git.run_git(base_clone_path, &to_strings(&["fetch", "origin", "main"]))
        .await
        .map_err(|reason| WorkspaceError::FetchFailed {
            path: base_clone_path.to_string_lossy().into_owned(),
            reason,
        })?;

    let worktree_path = worktree_root.join(issue_id);

    // Ensure worktree_root exists.
    ensure_dir_exists(worktree_root, log_creating_worktree_root)?;

    let wt_str = worktree_path.to_string_lossy().into_owned();

    log_worktree_add(&worktree_path, branch_name);
    git.run_git(
        base_clone_path,
        &to_strings(&["worktree", "add", &wt_str, "-b", branch_name, "origin/main"]),
    )
    .await
    .map_err(|reason| WorkspaceError::WorktreeCreationFailed {
        path: wt_str,
        reason,
    })?;

    Ok(worktree_path)
}

/// Remove a worktree and clean up its branch.
///
/// The worktree is force-removed, then the branch is deleted on a
/// best-effort basis (a failure is logged but not propagated).  Finally
/// `git worktree prune` is called, also best-effort.
pub async fn remove_worktree(
    git: &dyn GitCommandRunner,
    base_clone_path: &Path,
    worktree_path: &Path,
    branch_name: &str,
) -> Result<()> {
    let wt_str = worktree_path.to_string_lossy().into_owned();

    log_worktree_remove(worktree_path);
    git.run_git(
        base_clone_path,
        &to_strings(&["worktree", "remove", &wt_str, "--force"]),
    )
    .await
    .map_err(|reason| WorkspaceError::WorktreeRemovalFailed {
        path: wt_str,
        reason,
    })?;

    // Best-effort branch delete.
    if let Err(reason) = git
        .run_git(base_clone_path, &to_strings(&["branch", "-D", branch_name]))
        .await
    {
        log_branch_delete_failed(branch_name, &reason);
    }

    // Best-effort prune.
    if let Err(reason) = git
        .run_git(base_clone_path, &to_strings(&["worktree", "prune"]))
        .await
    {
        log_prune_failed(&reason);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_test::traced_test;

    // Helper to check if a slice of String matches expected &str values.
    fn args_eq(args: &[String], expected: &[&str]) -> bool {
        args.len() == expected.len() && args.iter().zip(expected).all(|(a, e)| a == e)
    }

    // ---- ensure_base_clone ------------------------------------------------

    #[tokio::test]
    async fn test_ensure_base_clone_fresh_clone() {
        let tmp = tempfile::tempdir().unwrap();
        let base_path = tmp.path().join("repo");

        let parent_path = tmp.path().to_path_buf();
        let base_path_clone = base_path.clone();

        let mut mock = MockGitCommandRunner::new();
        mock.expect_run_git()
            .withf(move |cwd, args| {
                cwd == parent_path.as_path()
                    && args.first().map(|s| s.as_str()) == Some("clone")
                    && args.get(1).map(|s| s.as_str()) == Some("https://example.com/repo.git")
                    && args.get(2).map(|s| s.as_str()) == Some(&*base_path_clone.to_string_lossy())
            })
            .times(1)
            .returning(|_, _| Ok(String::new()));

        let result = ensure_base_clone(&mock, "https://example.com/repo.git", &base_path).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_ensure_base_clone_existing_fetches() {
        let tmp = tempfile::tempdir().unwrap();
        let base_path = tmp.path().join("repo");

        // Create the .git directory so it looks like an existing clone.
        std::fs::create_dir_all(base_path.join(".git")).unwrap();

        let base_path_clone = base_path.clone();
        let mut mock = MockGitCommandRunner::new();
        mock.expect_run_git()
            .withf(move |cwd, args| {
                cwd == base_path_clone.as_path() && args_eq(args, &["fetch", "origin"])
            })
            .times(1)
            .returning(|_, _| Ok(String::new()));

        let result = ensure_base_clone(&mock, "https://example.com/repo.git", &base_path).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_ensure_base_clone_clone_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let base_path = tmp.path().join("repo");

        let mut mock = MockGitCommandRunner::new();
        mock.expect_run_git()
            .withf(|_cwd, args| args.first().map(|s| s.as_str()) == Some("clone"))
            .times(1)
            .returning(|_, _| Err("network error".to_string()));

        let result = ensure_base_clone(&mock, "https://example.com/repo.git", &base_path).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("failed to clone repository"));
        assert!(msg.contains("network error"));
    }

    #[tokio::test]
    async fn test_ensure_base_clone_fetch_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let base_path = tmp.path().join("repo");
        std::fs::create_dir_all(base_path.join(".git")).unwrap();

        let mut mock = MockGitCommandRunner::new();
        mock.expect_run_git()
            .withf(|_cwd, args| args_eq(args, &["fetch", "origin"]))
            .times(1)
            .returning(|_, _| Err("fetch timeout".to_string()));

        let result = ensure_base_clone(&mock, "https://example.com/repo.git", &base_path).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("failed to fetch"));
        assert!(msg.contains("fetch timeout"));
    }

    #[tokio::test]
    async fn test_ensure_base_clone_creates_parent_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let base_path = tmp.path().join("deep").join("nested").join("repo");

        let mut mock = MockGitCommandRunner::new();
        mock.expect_run_git()
            .withf(|_cwd, args| args.first().map(|s| s.as_str()) == Some("clone"))
            .times(1)
            .returning(|_, _| Ok(String::new()));

        let result = ensure_base_clone(&mock, "https://example.com/repo.git", &base_path).await;
        assert!(result.is_ok());

        // Parent directories should have been created.
        assert!(tmp.path().join("deep").join("nested").exists());
    }

    #[tokio::test]
    async fn test_ensure_base_clone_no_parent_directory() {
        // A path with no parent: Path::new("").parent() returns None.
        let base_path = Path::new("");

        let mock = MockGitCommandRunner::new();
        let result = ensure_base_clone(&mock, "https://example.com/repo.git", base_path).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("no parent directory"));
    }

    // ---- fetch_latest -----------------------------------------------------

    #[tokio::test]
    async fn test_fetch_latest_success() {
        let tmp = tempfile::tempdir().unwrap();

        let tmp_path = tmp.path().to_path_buf();
        let mut mock = MockGitCommandRunner::new();
        mock.expect_run_git()
            .withf(move |cwd, args| {
                cwd == tmp_path.as_path() && args_eq(args, &["fetch", "origin"])
            })
            .times(1)
            .returning(|_, _| Ok(String::new()));

        let result = fetch_latest(&mock, tmp.path()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_fetch_latest_failure() {
        let tmp = tempfile::tempdir().unwrap();

        let mut mock = MockGitCommandRunner::new();
        mock.expect_run_git()
            .withf(|_cwd, args| args_eq(args, &["fetch", "origin"]))
            .times(1)
            .returning(|_, _| Err("connection refused".to_string()));

        let result = fetch_latest(&mock, tmp.path()).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("failed to fetch"));
        assert!(msg.contains("connection refused"));
    }

    // ---- create_worktree --------------------------------------------------

    #[tokio::test]
    async fn test_create_worktree_success() {
        let tmp = tempfile::tempdir().unwrap();
        let base_path = tmp.path().join("base");
        let wt_root = tmp.path().join("worktrees");

        std::fs::create_dir_all(&base_path).unwrap();

        let base_path_clone = base_path.clone();
        let wt_root_clone = wt_root.clone();

        let mut mock = MockGitCommandRunner::new();

        // First call: fetch origin main
        let bp1 = base_path.clone();
        mock.expect_run_git()
            .withf(move |cwd, args| {
                cwd == bp1.as_path() && args_eq(args, &["fetch", "origin", "main"])
            })
            .times(1)
            .returning(|_, _| Ok(String::new()));

        // Second call: worktree add
        let expected_wt = wt_root_clone.join("ACTCLI-42");
        let expected_wt_str = expected_wt.to_string_lossy().into_owned();
        mock.expect_run_git()
            .withf(move |cwd, args| {
                cwd == base_path_clone.as_path()
                    && args.first().map(|s| s.as_str()) == Some("worktree")
                    && args.get(1).map(|s| s.as_str()) == Some("add")
                    && args.get(2).map(|s| s.as_str()) == Some(expected_wt_str.as_str())
                    && args.get(3).map(|s| s.as_str()) == Some("-b")
                    && args.get(4).map(|s| s.as_str()) == Some("feature/actcli-42")
                    && args.get(5).map(|s| s.as_str()) == Some("origin/main")
            })
            .times(1)
            .returning(|_, _| Ok(String::new()));

        let result = create_worktree(
            &mock,
            &base_path,
            &wt_root,
            "ACTCLI-42",
            "feature/actcli-42",
        )
        .await;

        assert!(result.is_ok());
        let path = result.unwrap();
        assert_eq!(path, wt_root.join("ACTCLI-42"));
    }

    #[tokio::test]
    async fn test_create_worktree_fetch_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let base_path = tmp.path().join("base");
        let wt_root = tmp.path().join("worktrees");

        std::fs::create_dir_all(&base_path).unwrap();

        let mut mock = MockGitCommandRunner::new();
        mock.expect_run_git()
            .withf(|_cwd, args| args_eq(args, &["fetch", "origin", "main"]))
            .times(1)
            .returning(|_, _| Err("fetch failed".to_string()));

        let result = create_worktree(
            &mock,
            &base_path,
            &wt_root,
            "ACTCLI-42",
            "feature/actcli-42",
        )
        .await;

        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("failed to fetch"));
    }

    #[tokio::test]
    async fn test_create_worktree_add_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let base_path = tmp.path().join("base");
        let wt_root = tmp.path().join("worktrees");

        std::fs::create_dir_all(&base_path).unwrap();

        let mut mock = MockGitCommandRunner::new();

        // Fetch succeeds
        mock.expect_run_git()
            .withf(|_cwd, args| args_eq(args, &["fetch", "origin", "main"]))
            .times(1)
            .returning(|_, _| Ok(String::new()));

        // Worktree add fails
        mock.expect_run_git()
            .withf(|_cwd, args| {
                args.first().map(|s| s.as_str()) == Some("worktree")
                    && args.get(1).map(|s| s.as_str()) == Some("add")
            })
            .times(1)
            .returning(|_, _| Err("worktree error".to_string()));

        let result = create_worktree(
            &mock,
            &base_path,
            &wt_root,
            "ACTCLI-42",
            "feature/actcli-42",
        )
        .await;

        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("failed to create worktree"));
        assert!(msg.contains("worktree error"));
    }

    #[tokio::test]
    async fn test_create_worktree_creates_root_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let base_path = tmp.path().join("base");
        let wt_root = tmp.path().join("deep").join("worktrees");

        std::fs::create_dir_all(&base_path).unwrap();

        let mut mock = MockGitCommandRunner::new();

        mock.expect_run_git()
            .withf(|_cwd, args| args_eq(args, &["fetch", "origin", "main"]))
            .times(1)
            .returning(|_, _| Ok(String::new()));

        mock.expect_run_git()
            .withf(|_cwd, args| {
                args.first().map(|s| s.as_str()) == Some("worktree")
                    && args.get(1).map(|s| s.as_str()) == Some("add")
            })
            .times(1)
            .returning(|_, _| Ok(String::new()));

        let result = create_worktree(
            &mock,
            &base_path,
            &wt_root,
            "ACTCLI-42",
            "feature/actcli-42",
        )
        .await;

        assert!(result.is_ok());
        // The worktree root directory should have been created.
        assert!(wt_root.exists());
    }

    // ---- remove_worktree --------------------------------------------------

    #[tokio::test]
    async fn test_remove_worktree_success() {
        let tmp = tempfile::tempdir().unwrap();
        let base_path = tmp.path().join("base");
        let wt_path = tmp.path().join("worktrees").join("ACTCLI-42");

        let base_clone = base_path.clone();
        let wt_str = wt_path.to_string_lossy().into_owned();

        let mut mock = MockGitCommandRunner::new();

        // worktree remove
        let bc1 = base_clone.clone();
        let ws1 = wt_str.clone();
        mock.expect_run_git()
            .withf(move |cwd, args| {
                cwd == bc1.as_path()
                    && args.first().map(|s| s.as_str()) == Some("worktree")
                    && args.get(1).map(|s| s.as_str()) == Some("remove")
                    && args.get(2).map(|s| s.as_str()) == Some(ws1.as_str())
                    && args.get(3).map(|s| s.as_str()) == Some("--force")
            })
            .times(1)
            .returning(|_, _| Ok(String::new()));

        // branch -D
        let bc2 = base_clone.clone();
        mock.expect_run_git()
            .withf(move |cwd, args| {
                cwd == bc2.as_path() && args_eq(args, &["branch", "-D", "feature/actcli-42"])
            })
            .times(1)
            .returning(|_, _| Ok(String::new()));

        // worktree prune
        let bc3 = base_clone.clone();
        mock.expect_run_git()
            .withf(move |cwd, args| cwd == bc3.as_path() && args_eq(args, &["worktree", "prune"]))
            .times(1)
            .returning(|_, _| Ok(String::new()));

        let result = remove_worktree(&mock, &base_path, &wt_path, "feature/actcli-42").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_remove_worktree_remove_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let base_path = tmp.path().join("base");
        let wt_path = tmp.path().join("worktrees").join("ACTCLI-42");

        let mut mock = MockGitCommandRunner::new();

        mock.expect_run_git()
            .withf(|_cwd, args| {
                args.first().map(|s| s.as_str()) == Some("worktree")
                    && args.get(1).map(|s| s.as_str()) == Some("remove")
            })
            .times(1)
            .returning(|_, _| Err("not a worktree".to_string()));

        let result = remove_worktree(&mock, &base_path, &wt_path, "feature/actcli-42").await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("failed to remove worktree"));
        assert!(msg.contains("not a worktree"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_remove_worktree_branch_delete_fails_best_effort() {
        let tmp = tempfile::tempdir().unwrap();
        let base_path = tmp.path().join("base");
        let wt_path = tmp.path().join("worktrees").join("ACTCLI-42");

        let mut mock = MockGitCommandRunner::new();

        // worktree remove succeeds
        mock.expect_run_git()
            .withf(|_cwd, args| {
                args.first().map(|s| s.as_str()) == Some("worktree")
                    && args.get(1).map(|s| s.as_str()) == Some("remove")
            })
            .times(1)
            .returning(|_, _| Ok(String::new()));

        // branch -D fails (best-effort)
        mock.expect_run_git()
            .withf(|_cwd, args| {
                args.first().map(|s| s.as_str()) == Some("branch")
                    && args.get(1).map(|s| s.as_str()) == Some("-D")
            })
            .times(1)
            .returning(|_, _| Err("branch not found".to_string()));

        // prune succeeds
        mock.expect_run_git()
            .withf(|_cwd, args| {
                args.first().map(|s| s.as_str()) == Some("worktree")
                    && args.get(1).map(|s| s.as_str()) == Some("prune")
            })
            .times(1)
            .returning(|_, _| Ok(String::new()));

        let result = remove_worktree(&mock, &base_path, &wt_path, "feature/actcli-42").await;
        assert!(result.is_ok());
        assert!(logs_contain("best-effort branch delete failed"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_remove_worktree_prune_fails_best_effort() {
        let tmp = tempfile::tempdir().unwrap();
        let base_path = tmp.path().join("base");
        let wt_path = tmp.path().join("worktrees").join("ACTCLI-42");

        let mut mock = MockGitCommandRunner::new();

        // worktree remove succeeds
        mock.expect_run_git()
            .withf(|_cwd, args| {
                args.first().map(|s| s.as_str()) == Some("worktree")
                    && args.get(1).map(|s| s.as_str()) == Some("remove")
            })
            .times(1)
            .returning(|_, _| Ok(String::new()));

        // branch -D succeeds
        mock.expect_run_git()
            .withf(|_cwd, args| {
                args.first().map(|s| s.as_str()) == Some("branch")
                    && args.get(1).map(|s| s.as_str()) == Some("-D")
            })
            .times(1)
            .returning(|_, _| Ok(String::new()));

        // prune fails (best-effort)
        mock.expect_run_git()
            .withf(|_cwd, args| {
                args.first().map(|s| s.as_str()) == Some("worktree")
                    && args.get(1).map(|s| s.as_str()) == Some("prune")
            })
            .times(1)
            .returning(|_, _| Err("prune error".to_string()));

        let result = remove_worktree(&mock, &base_path, &wt_path, "feature/actcli-42").await;
        assert!(result.is_ok());
        assert!(logs_contain("best-effort worktree prune failed"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_remove_worktree_branch_and_prune_both_fail() {
        let tmp = tempfile::tempdir().unwrap();
        let base_path = tmp.path().join("base");
        let wt_path = tmp.path().join("worktrees").join("ACTCLI-42");

        let mut mock = MockGitCommandRunner::new();

        // worktree remove succeeds
        mock.expect_run_git()
            .withf(|_cwd, args| {
                args.first().map(|s| s.as_str()) == Some("worktree")
                    && args.get(1).map(|s| s.as_str()) == Some("remove")
            })
            .times(1)
            .returning(|_, _| Ok(String::new()));

        // branch -D fails
        mock.expect_run_git()
            .withf(|_cwd, args| {
                args.first().map(|s| s.as_str()) == Some("branch")
                    && args.get(1).map(|s| s.as_str()) == Some("-D")
            })
            .times(1)
            .returning(|_, _| Err("branch not found".to_string()));

        // prune fails
        mock.expect_run_git()
            .withf(|_cwd, args| {
                args.first().map(|s| s.as_str()) == Some("worktree")
                    && args.get(1).map(|s| s.as_str()) == Some("prune")
            })
            .times(1)
            .returning(|_, _| Err("prune error".to_string()));

        let result = remove_worktree(&mock, &base_path, &wt_path, "feature/actcli-42").await;
        assert!(result.is_ok());
        assert!(logs_contain("best-effort branch delete failed"));
        assert!(logs_contain("best-effort worktree prune failed"));
    }

    // ---- RealGitCommandRunner ---------------------------------------------

    #[tokio::test]
    async fn test_real_git_command_runner_success() {
        let tmp = tempfile::tempdir().unwrap();
        // Initialize a real git repo in the temp dir.
        let status = std::process::Command::new("git")
            .args(["init"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        assert!(status.status.success());

        let runner = RealGitCommandRunner;
        let result = runner.run_git(tmp.path(), &to_strings(&["status"])).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_real_git_command_runner_failure() {
        let tmp = tempfile::tempdir().unwrap();
        // Run git log in a non-git directory - should fail.
        let runner = RealGitCommandRunner;
        let result = runner.run_git(tmp.path(), &to_strings(&["log"])).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_real_git_command_runner_spawn_error() {
        // Use a non-existent directory as cwd so the spawn itself fails.
        let bad_dir = Path::new("/nonexistent_dir_for_test_12345");
        let runner = RealGitCommandRunner;
        let result = runner.run_git(bad_dir, &to_strings(&["status"])).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("failed to spawn git"));
    }

    // ---- Directory creation failure tests ---------------------------------

    #[tokio::test]
    async fn test_ensure_base_clone_parent_dir_creation_fails() {
        let tmp = tempfile::tempdir().unwrap();
        // Create a file where we need a directory, so create_dir_all fails.
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, "I am a file").unwrap();
        // base_clone_path has the file as its parent-to-be-created
        let base_path = blocker.join("subdir").join("repo");

        let mock = MockGitCommandRunner::new();
        let result = ensure_base_clone(&mock, "https://example.com/repo.git", &base_path).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("failed to create directory"));
    }

    #[tokio::test]
    async fn test_create_worktree_root_dir_creation_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let base_path = tmp.path().join("base");
        std::fs::create_dir_all(&base_path).unwrap();

        // Create a file where we need a directory, so create_dir_all fails.
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, "I am a file").unwrap();
        let wt_root = blocker.join("worktrees");

        let mut mock = MockGitCommandRunner::new();
        // Fetch succeeds
        mock.expect_run_git()
            .withf(|_cwd, args| args_eq(args, &["fetch", "origin", "main"]))
            .times(1)
            .returning(|_, _| Ok(String::new()));

        let result = create_worktree(
            &mock,
            &base_path,
            &wt_root,
            "ACTCLI-42",
            "feature/actcli-42",
        )
        .await;

        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("failed to create directory"));
    }

    // ---- Error Display tests ----------------------------------------------

    #[test]
    fn test_error_display_clone_failed() {
        let err = WorkspaceError::CloneFailed {
            url: "https://example.com/repo.git".to_string(),
            path: "/tmp/repo".to_string(),
            reason: "network error".to_string(),
        };
        let msg = err.to_string();
        assert_eq!(
            msg,
            "failed to clone repository 'https://example.com/repo.git' to '/tmp/repo': network error"
        );
    }

    #[test]
    fn test_error_display_fetch_failed() {
        let err = WorkspaceError::FetchFailed {
            path: "/tmp/repo".to_string(),
            reason: "timeout".to_string(),
        };
        let msg = err.to_string();
        assert_eq!(msg, "failed to fetch in '/tmp/repo': timeout");
    }

    #[test]
    fn test_error_display_worktree_creation_failed() {
        let err = WorkspaceError::WorktreeCreationFailed {
            path: "/tmp/wt".to_string(),
            reason: "already exists".to_string(),
        };
        let msg = err.to_string();
        assert_eq!(
            msg,
            "failed to create worktree at '/tmp/wt': already exists"
        );
    }

    #[test]
    fn test_error_display_worktree_removal_failed() {
        let err = WorkspaceError::WorktreeRemovalFailed {
            path: "/tmp/wt".to_string(),
            reason: "not found".to_string(),
        };
        let msg = err.to_string();
        assert_eq!(msg, "failed to remove worktree at '/tmp/wt': not found");
    }

    #[test]
    fn test_error_display_no_parent_directory() {
        let err = WorkspaceError::NoParentDirectory {
            path: "/".to_string(),
        };
        let msg = err.to_string();
        assert_eq!(msg, "base clone path has no parent directory: /");
    }

    #[test]
    fn test_error_display_directory_creation_failed() {
        let err = WorkspaceError::DirectoryCreationFailed {
            path: "/tmp/dir".to_string(),
            reason: "permission denied".to_string(),
        };
        let msg = err.to_string();
        assert_eq!(
            msg,
            "failed to create directory '/tmp/dir': permission denied"
        );
    }

    #[test]
    fn test_error_is_debug() {
        let err = WorkspaceError::CloneFailed {
            url: "u".to_string(),
            path: "p".to_string(),
            reason: "r".to_string(),
        };
        let debug = format!("{:?}", err);
        assert!(debug.contains("CloneFailed"));
    }
}
