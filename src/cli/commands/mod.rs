use std::path::{Path, PathBuf};

use crate::cli::ui::panel::Panel;
use crate::cli::ui::theme;
use crate::error::ActualError;

pub mod auth;
pub mod config;
pub mod status;
pub mod sync;

/// Directory names to skip when walking the file tree for CLAUDE.md files.
///
/// Shared between the `status` and `sync` walkers so both use the same set
/// of ignored directories.
pub(crate) const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "vendor",
    ".git",
    ".hg",
    ".svn",
    "__pycache__",
    "dist",
    "build",
    ".worktrees",
];

/// Recursively find all files named `CLAUDE.md` under the given root directory.
///
/// Skips hidden directories (starting with `.`) and directories listed in
/// [`SKIP_DIRS`].
pub(crate) fn find_claude_md_files(root: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    walk_for_claude_md(root, &mut results);
    results.sort();
    results
}

fn walk_for_claude_md(dir: &Path, results: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let path = entry.path();
        if path.is_dir() {
            // Skip hidden directories and common ignore dirs
            if name_str.starts_with('.') || SKIP_DIRS.contains(&name_str.as_ref()) {
                continue;
            }
            walk_for_claude_md(&path, results);
        } else if name_str == "CLAUDE.md" {
            results.push(path);
        }
    }
}

/// Convert a command result to an exit code, printing any error.
pub(crate) fn handle_result(result: Result<(), ActualError>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            let width = console::Term::stderr()
                .size_checked()
                .map(|(_, cols)| cols as usize)
                .unwrap_or(80)
                .min(90);

            let error_line = format!("{} {}", theme::error_prefix().for_stderr(), e);
            let mut panel = Panel::titled("Error").line("").line(&error_line);

            if let Some(hint_text) = e.hint() {
                let hint_styled = theme::hint(hint_text).for_stderr();
                panel = panel.line("").line(&format!("Fix: {hint_styled}"));
            }

            panel = panel.line("");

            eprintln!("{}", panel.render(width));
            e.exit_code()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_result_ok() {
        assert_eq!(handle_result(Ok(())), 0);
    }

    #[test]
    fn test_handle_result_user_cancelled() {
        let code = handle_result(Err(ActualError::UserCancelled));
        assert_eq!(code, 4);
    }

    #[test]
    fn test_handle_result_config_error() {
        let code = handle_result(Err(ActualError::ConfigError("bad".to_string())));
        assert_eq!(code, 1);
    }
}
