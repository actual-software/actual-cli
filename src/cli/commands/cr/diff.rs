use std::process::Command;

use crate::cli::commands::cr::types::DiffContext;
use crate::error::ActualError;

/// Gather the git diff context for code review.
///
/// Runs git commands to collect the diff, stat, commit messages, and HEAD SHA
/// relative to the given base branch.
pub fn gather_diff(base: &str) -> Result<DiffContext, ActualError> {
    // 1. Verify the base branch exists
    let merge_base = run_git(&["merge-base", base, "HEAD"]).map_err(|_| {
        ActualError::ConfigError(format!(
            "Base branch '{base}' does not exist or has no common ancestor with HEAD"
        ))
    })?;
    let _ = merge_base; // just verifying it exists

    // 2. Get the full diff
    let diff_text = run_git(&["diff", &format!("{base}...HEAD")])?;

    if diff_text.trim().is_empty() {
        return Err(ActualError::ConfigError(format!(
            "No changes found between '{base}' and HEAD"
        )));
    }

    // 3. Get the diff stat
    let diff_stat = run_git(&["diff", &format!("{base}...HEAD"), "--stat"])?;

    // 4. Get commit messages
    let commit_messages = run_git(&["log", &format!("{base}..HEAD"), "--oneline"])?;

    // 5. Get current commit SHA
    let head_sha = run_git(&["rev-parse", "HEAD"])?;
    let head_sha = head_sha.trim().to_string();

    // 6. Extract changed file paths
    let name_only = run_git(&["diff", &format!("{base}...HEAD"), "--name-only"])?;
    let changed_files: Vec<String> = name_only
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    Ok(DiffContext {
        base_branch: base.to_string(),
        head_sha,
        diff_text,
        diff_stat,
        commit_messages,
        changed_files,
    })
}

/// Run a git command and return its stdout as a string.
fn run_git(args: &[&str]) -> Result<String, ActualError> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(ActualError::IoError)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(ActualError::InternalError(format!(
            "git {} failed: {}",
            args.join(" "),
            stderr.trim()
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_git_rev_parse() {
        // This test runs in the actual repo, so rev-parse HEAD should work
        let sha = run_git(&["rev-parse", "HEAD"]).unwrap();
        assert!(!sha.trim().is_empty());
        assert_eq!(sha.trim().len(), 40); // full SHA is 40 hex chars
    }

    #[test]
    fn test_run_git_invalid_command() {
        let result = run_git(&["not-a-real-command"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_gather_diff_nonexistent_base() {
        let result = gather_diff("nonexistent-branch-that-does-not-exist-abc123");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("does not exist") || err.contains("no common ancestor"),
            "unexpected error: {err}"
        );
    }
}
