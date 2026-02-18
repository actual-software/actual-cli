use std::path::{Path, PathBuf};

use crate::cli::ui::panel::Panel;
use crate::cli::ui::theme;
use crate::error::ActualError;
use crate::generation::OutputFormat;

pub mod auth;
pub mod config;
pub mod status;
pub mod sync;
pub(crate) mod sync_wiring;

/// Non-hidden directory names to skip when walking the file tree for
/// CLAUDE.md files.
///
/// All hidden directories (names starting with `.`) are unconditionally
/// skipped by the walker, so they do not need to appear here.
///
/// Shared between the `status` and `sync` walkers so both use the same set
/// of ignored directories.
pub(crate) const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "vendor",
    "__pycache__",
    "dist",
    "build",
];

/// Recursively find all output files matching the given [`OutputFormat`] under
/// the given root directory.
///
/// Skips all hidden directories (names starting with `.`) and the non-hidden
/// directories listed in [`SKIP_DIRS`].
pub(crate) fn find_output_files(root: &Path, format: &OutputFormat) -> Vec<PathBuf> {
    let mut results = Vec::new();
    walk_for_output_files(root, format.filename(), &mut results);
    results.sort();
    results
}

/// Backwards-compatible alias: find all `CLAUDE.md` files (default format).
pub(crate) fn find_claude_md_files(root: &Path) -> Vec<PathBuf> {
    find_output_files(root, &OutputFormat::ClaudeMd)
}

fn walk_for_output_files(dir: &Path, filename: &str, results: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let path = entry.path();
        if path.is_dir() {
            // Skip all hidden directories and known non-project directories
            if name_str.starts_with('.') || SKIP_DIRS.contains(&name_str.as_ref()) {
                continue;
            }
            walk_for_output_files(&path, filename, results);
        } else if name_str == filename {
            results.push(path);
        }
    }
}

/// Convert a command result to an exit code, printing any error to stderr.
///
/// Renders a styled error panel (including an optional hint) and returns the
/// numeric exit code from [`ActualError::exit_code`].  This is the single
/// place where error output is formatted, so callers never need to print
/// errors themselves.
pub fn handle_result(result: Result<(), ActualError>) -> i32 {
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

    // ── find_output_files tests ──

    #[test]
    fn test_find_output_files_agents_md_discovers_agents_md() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        std::fs::write(dir.path().join("AGENTS.md"), "Agent rules").unwrap();
        let files = find_output_files(dir.path(), &OutputFormat::AgentsMd);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_name().unwrap(), "AGENTS.md");
    }

    #[test]
    fn test_find_output_files_agents_md_does_not_discover_claude_md() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        std::fs::write(dir.path().join("CLAUDE.md"), "Claude rules").unwrap();
        let files = find_output_files(dir.path(), &OutputFormat::AgentsMd);
        assert!(
            files.is_empty(),
            "AgentsMd format should not find CLAUDE.md"
        );
    }

    #[test]
    fn test_find_output_files_claude_md_discovers_claude_md() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        std::fs::write(dir.path().join("CLAUDE.md"), "Claude rules").unwrap();
        let files = find_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_name().unwrap(), "CLAUDE.md");
    }

    #[test]
    fn test_find_output_files_nested_agents_md() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        std::fs::create_dir_all(dir.path().join("apps").join("web")).unwrap();
        std::fs::write(
            dir.path().join("apps").join("web").join("AGENTS.md"),
            "Web agent rules",
        )
        .unwrap();
        let files = find_output_files(dir.path(), &OutputFormat::AgentsMd);
        assert_eq!(files.len(), 1);
        assert!(files[0].to_string_lossy().contains("AGENTS.md"));
    }
}
