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

    let diff_text = truncate_diff(diff_text);

    Ok(DiffContext {
        base_branch: base.to_string(),
        head_sha,
        diff_text,
        diff_stat,
        commit_messages,
        changed_files,
    })
}

/// Maximum diff size in bytes before truncation (100 KB).
const MAX_DIFF_BYTES: usize = 100 * 1024;

/// Truncate the diff to at most [`MAX_DIFF_BYTES`] on a newline boundary.
///
/// When truncation occurs, a notice is appended so the LLM knows the diff
/// was cut short. A `tracing::warn!` and `eprintln!` are emitted so the
/// user sees it both in structured logs and terminal output.
fn truncate_diff(diff: String) -> String {
    if diff.len() <= MAX_DIFF_BYTES {
        return diff;
    }

    let original_len = diff.len();

    // Find the last newline at or before the limit so we don't cut mid-line.
    // Include the newline itself (+1) so the truncated text ends cleanly.
    let cut_point = diff[..MAX_DIFF_BYTES]
        .rfind('\n')
        .map(|pos| pos + 1)
        .unwrap_or(MAX_DIFF_BYTES);

    let truncated = &diff[..cut_point];
    let omitted = original_len - cut_point;

    tracing::warn!(
        original_bytes = original_len,
        truncated_bytes = cut_point,
        omitted_bytes = omitted,
        "Diff truncated to stay within size limit"
    );
    eprintln!(
        "Warning: diff truncated from {} to {} bytes ({} bytes omitted)",
        original_len, cut_point, omitted
    );

    format!(
        "{}\n\n[... truncated: {} of {} bytes omitted ...]",
        truncated, omitted, original_len
    )
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

    #[test]
    fn test_truncate_diff_small_diff_unchanged() {
        let small = "line1\nline2\nline3\n".to_string();
        let result = truncate_diff(small.clone());
        assert_eq!(result, small);
    }

    #[test]
    fn test_truncate_diff_large_diff_truncated() {
        // Create a diff larger than MAX_DIFF_BYTES (100KB)
        let line = "a".repeat(100) + "\n";
        let num_lines = (MAX_DIFF_BYTES / line.len()) + 10;
        let large: String = line.repeat(num_lines);
        assert!(large.len() > MAX_DIFF_BYTES);

        let result = truncate_diff(large.clone());
        assert!(result.len() < large.len());
        assert!(result.contains("[... truncated:"));
        assert!(result.contains("bytes omitted"));
    }

    #[test]
    fn test_truncate_diff_cuts_on_newline_boundary() {
        // Build a string just over the limit with known newline positions
        let line = "x".repeat(99) + "\n";
        let num_lines = (MAX_DIFF_BYTES / line.len()) + 5;
        let large: String = line.repeat(num_lines);
        assert!(large.len() > MAX_DIFF_BYTES);

        let result = truncate_diff(large);
        // The result should contain the truncation notice
        assert!(result.contains("[... truncated:"));
        // The truncated body should not cut mid-line — check that the content
        // before the notice ends with a newline.
        let notice_pos = result.find("\n\n[... truncated:").unwrap();
        let body = &result[..notice_pos];
        assert!(body.ends_with('\n'));
    }

    #[test]
    fn test_truncate_diff_exactly_at_limit() {
        let exact = "a".repeat(MAX_DIFF_BYTES);
        let result = truncate_diff(exact.clone());
        assert_eq!(
            result, exact,
            "diff at exactly the limit should not be truncated"
        );
    }
}
