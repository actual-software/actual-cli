use std::path::Path;
use std::process::Command;

const MAX_DIFF_BYTES: usize = 100_000;

/// Capture the git diff for the working directory.
/// Returns the diff output truncated to MAX_DIFF_BYTES.
/// Fails gracefully: returns empty string if not in a git repo or git fails.
pub fn capture_git_diff(cwd: &Path) -> String {
    let output = Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(cwd)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let diff = String::from_utf8_lossy(&out.stdout);
            if diff.len() > MAX_DIFF_BYTES {
                let mut truncated = diff[..MAX_DIFF_BYTES].to_string();
                truncated.push_str("\n\n[diff truncated at 100KB]");
                truncated
            } else {
                diff.into_owned()
            }
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("not a git repository") {
                String::new()
            } else {
                eprintln!("advisor: git diff failed: {}", stderr.trim());
                String::new()
            }
        }
        Err(e) => {
            eprintln!("advisor: git not available: {e}");
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_capture_git_diff_in_git_repo_with_changes() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path();

        Command::new("git")
            .args(["init"])
            .current_dir(dir_path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir_path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir_path)
            .output()
            .unwrap();

        let file_path = dir_path.join("test.txt");
        fs::write(&file_path, "initial content").unwrap();

        Command::new("git")
            .args(["add", "."])
            .current_dir(dir_path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(dir_path)
            .output()
            .unwrap();

        fs::write(&file_path, "modified content").unwrap();

        let diff = capture_git_diff(dir_path);
        assert!(!diff.is_empty(), "should have a diff");
        assert!(diff.contains("modified content"));
    }

    #[test]
    fn test_capture_git_diff_no_changes() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path();

        Command::new("git")
            .args(["init"])
            .current_dir(dir_path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir_path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir_path)
            .output()
            .unwrap();

        let file_path = dir_path.join("test.txt");
        fs::write(&file_path, "content").unwrap();

        Command::new("git")
            .args(["add", "."])
            .current_dir(dir_path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(dir_path)
            .output()
            .unwrap();

        let diff = capture_git_diff(dir_path);
        assert!(diff.is_empty(), "should have no diff when nothing changed");
    }

    #[test]
    fn test_capture_git_diff_not_git_repo() {
        let dir = tempdir().unwrap();
        let diff = capture_git_diff(dir.path());
        assert!(diff.is_empty(), "should return empty for non-git dir");
    }
}
