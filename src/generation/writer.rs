use std::path::Path;

use super::markers;
use super::merge;
use crate::tailoring::types::FileOutput;

/// The action taken for a single file write.
#[derive(Debug, Clone, PartialEq)]
pub enum WriteAction {
    /// A new file was created
    Created,
    /// An existing file was updated (managed section replaced)
    Updated,
    /// The write failed
    Failed,
}

/// Result of writing a single CLAUDE.md file.
#[derive(Debug, Clone)]
pub struct WriteResult {
    /// Relative path that was written (e.g. "CLAUDE.md" or "apps/web/CLAUDE.md")
    pub path: String,
    /// Whether the file was created, updated, or failed
    pub action: WriteAction,
    /// The version number written (0 if failed)
    pub version: u32,
    /// Error message if the write failed
    pub error: Option<String>,
}

/// Write multiple CLAUDE.md files to disk.
///
/// For each file in `files`:
/// 1. Determine if root (`path` has no directory separator — just "CLAUDE.md")
/// 2. Read existing content from `root_dir.join(path)`, if file exists
/// 3. Compute next version via `markers::next_version(existing)`
/// 4. Merge via `merge::merge_content(existing, content, version, is_root, adr_ids)`
/// 5. Create parent directories with `std::fs::create_dir_all`
/// 6. Write the file with `std::fs::write`
/// 7. Collect `WriteResult`
///
/// Individual file failures do NOT abort the batch — errors are collected per file.
pub fn write_files(root_dir: &Path, files: &[FileOutput]) -> Vec<WriteResult> {
    files
        .iter()
        .map(|file| {
            let full_path = root_dir.join(&file.path);

            // Determine if root file (no directory component)
            let is_root = Path::new(&file.path)
                .parent()
                .is_none_or(|p| p == Path::new(""));

            // Read existing content
            let existing_content = std::fs::read_to_string(&full_path).ok();
            let existing_ref = existing_content.as_deref();

            // Determine if this is a create or update
            let action = if existing_content.is_some() {
                WriteAction::Updated
            } else {
                WriteAction::Created
            };

            // Compute version
            let version = markers::next_version(existing_ref);

            // Merge content
            let result =
                merge::merge_content(existing_ref, &file.content, version, is_root, &file.adr_ids);

            // Create parent directories (full_path always has a parent since it's root_dir.join(path))
            let parent = full_path.parent().expect("joined path always has a parent");
            if let Err(e) = std::fs::create_dir_all(parent) {
                return WriteResult {
                    path: file.path.clone(),
                    action: WriteAction::Failed,
                    version: 0,
                    error: Some(format!("Failed to create directory: {e}")),
                };
            }

            // Write file
            if let Err(e) = std::fs::write(&full_path, &result.content) {
                return WriteResult {
                    path: file.path.clone(),
                    action: WriteAction::Failed,
                    version: 0,
                    error: Some(format!("Failed to write file: {e}")),
                };
            }

            WriteResult {
                path: file.path.clone(),
                action,
                version,
                error: None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_root_claude_md() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let files = vec![FileOutput {
            path: "CLAUDE.md".to_string(),
            content: "Use consistent error handling.".to_string(),
            reasoning: "Root-level rules".to_string(),
            adr_ids: vec!["adr-001".to_string()],
        }];

        let results = write_files(dir.path(), &files);

        assert_eq!(results.len(), 1, "expected 1 result");
        assert_eq!(results[0].path, "CLAUDE.md");
        assert_eq!(
            results[0].action,
            WriteAction::Created,
            "expected Created action for new file"
        );
        assert_eq!(results[0].version, 1, "expected version 1 for new file");
        assert!(results[0].error.is_none(), "expected no error");

        let written = std::fs::read_to_string(dir.path().join("CLAUDE.md"))
            .expect("failed to read written file");
        assert!(
            written.contains("# Project Guidelines"),
            "root file should have Project Guidelines header, got: {written}"
        );
        assert!(
            markers::has_managed_section(&written),
            "root file should have managed section"
        );
        assert!(
            written.contains("Use consistent error handling."),
            "root file should contain managed content"
        );
    }

    #[test]
    fn test_write_subdirectory_file() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let files = vec![FileOutput {
            path: "apps/web/CLAUDE.md".to_string(),
            content: "Use React Server Components.".to_string(),
            reasoning: "Web-specific rules".to_string(),
            adr_ids: vec!["adr-002".to_string()],
        }];

        let results = write_files(dir.path(), &files);

        assert_eq!(results.len(), 1, "expected 1 result");
        assert_eq!(results[0].path, "apps/web/CLAUDE.md");
        assert_eq!(
            results[0].action,
            WriteAction::Created,
            "expected Created action for new subdirectory file"
        );
        assert!(results[0].error.is_none(), "expected no error");

        // Verify parent directories were created
        assert!(
            dir.path().join("apps/web").is_dir(),
            "expected apps/web directory to be created"
        );

        let written = std::fs::read_to_string(dir.path().join("apps/web/CLAUDE.md"))
            .expect("failed to read written file");
        assert!(
            markers::has_managed_section(&written),
            "subdirectory file should have managed section"
        );
        assert!(
            !written.contains("# Project Guidelines"),
            "subdirectory file should NOT have Project Guidelines header"
        );
        assert!(
            written.contains("Use React Server Components."),
            "subdirectory file should contain managed content"
        );
    }

    #[test]
    fn test_update_existing_file_with_markers() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");

        // Create an existing file with version 1 markers
        let initial_content = format!(
            "# Project Guidelines\n\n{}\n",
            markers::wrap_in_markers("Old content", 1, &["adr-001".to_string()])
        );
        std::fs::write(dir.path().join("CLAUDE.md"), &initial_content)
            .expect("failed to write initial file");

        let files = vec![FileOutput {
            path: "CLAUDE.md".to_string(),
            content: "New content replaces old.".to_string(),
            reasoning: "Updated rules".to_string(),
            adr_ids: vec!["adr-001".to_string(), "adr-003".to_string()],
        }];

        let results = write_files(dir.path(), &files);

        assert_eq!(results.len(), 1, "expected 1 result");
        assert_eq!(
            results[0].action,
            WriteAction::Updated,
            "expected Updated action for existing file"
        );
        assert_eq!(
            results[0].version, 2,
            "expected version 2 after update (was 1)"
        );
        assert!(results[0].error.is_none(), "expected no error");

        let written = std::fs::read_to_string(dir.path().join("CLAUDE.md"))
            .expect("failed to read updated file");
        assert!(
            written.contains("New content replaces old."),
            "updated file should contain new content, got: {written}"
        );
        assert!(
            !written.contains("Old content"),
            "updated file should not contain old managed content"
        );
        assert_eq!(
            markers::extract_version(&written),
            Some(2),
            "expected version 2 in updated file"
        );
        assert!(
            written.contains("# Project Guidelines"),
            "header should be preserved after update"
        );
    }

    #[test]
    fn test_write_three_files() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let files = vec![
            FileOutput {
                path: "CLAUDE.md".to_string(),
                content: "Root rules".to_string(),
                reasoning: "root".to_string(),
                adr_ids: vec!["adr-001".to_string()],
            },
            FileOutput {
                path: "apps/web/CLAUDE.md".to_string(),
                content: "Web rules".to_string(),
                reasoning: "web".to_string(),
                adr_ids: vec!["adr-002".to_string()],
            },
            FileOutput {
                path: "libs/core/CLAUDE.md".to_string(),
                content: "Core rules".to_string(),
                reasoning: "core".to_string(),
                adr_ids: vec!["adr-003".to_string()],
            },
        ];

        let results = write_files(dir.path(), &files);

        assert_eq!(results.len(), 3);

        // All should succeed
        for result in &results {
            assert_eq!(result.action, WriteAction::Created);
            assert!(result.error.is_none());
            assert_eq!(result.version, 1);
        }

        // Verify paths
        assert_eq!(results[0].path, "CLAUDE.md");
        assert_eq!(results[1].path, "apps/web/CLAUDE.md");
        assert_eq!(results[2].path, "libs/core/CLAUDE.md");

        // Verify all files exist and have managed sections
        for file in &files {
            let written = std::fs::read_to_string(dir.path().join(&file.path)).unwrap();
            assert!(markers::has_managed_section(&written));
        }

        // Root should have header, subdirectories should not
        let root = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(root.contains("# Project Guidelines"));

        let web = std::fs::read_to_string(dir.path().join("apps/web/CLAUDE.md")).unwrap();
        assert!(!web.contains("# Project Guidelines"));

        let core = std::fs::read_to_string(dir.path().join("libs/core/CLAUDE.md")).unwrap();
        assert!(!core.contains("# Project Guidelines"));
    }

    #[test]
    fn test_write_fails_when_parent_dir_cannot_be_created() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        // Create a regular file where the parent directory needs to be
        std::fs::write(dir.path().join("blocker"), "I am a file").unwrap();

        let files = vec![FileOutput {
            path: "blocker/nested/CLAUDE.md".to_string(),
            content: "content".to_string(),
            reasoning: "test".to_string(),
            adr_ids: vec![],
        }];

        let results = write_files(dir.path(), &files);

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].action,
            WriteAction::Failed,
            "expected Failed action when directory creation fails"
        );
        assert_eq!(results[0].version, 0, "expected version 0 on failure");
        assert!(
            results[0].error.is_some(),
            "expected error message on failure"
        );
        let err_msg = results[0].error.as_ref().unwrap();
        assert!(
            err_msg.contains("Failed to create directory"),
            "expected directory creation error message"
        );
    }

    #[test]
    fn test_write_fails_when_file_cannot_be_written() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        // Create a directory at the target file path — writing to a directory always fails
        let target = dir.path().join("subdir").join("CLAUDE.md");
        std::fs::create_dir_all(&target).unwrap();

        let files = vec![FileOutput {
            path: "subdir/CLAUDE.md".to_string(),
            content: "content".to_string(),
            reasoning: "test".to_string(),
            adr_ids: vec![],
        }];

        let results = write_files(dir.path(), &files);

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].action,
            WriteAction::Failed,
            "expected Failed action when file write fails"
        );
        assert_eq!(results[0].version, 0, "expected version 0 on failure");
        assert!(
            results[0].error.is_some(),
            "expected error message on write failure"
        );
        let err_msg = results[0].error.as_ref().unwrap();
        assert!(
            err_msg.contains("Failed to write file"),
            "expected file write error message"
        );
    }

    #[test]
    fn test_write_result_and_action_clone() {
        // Exercise Clone for WriteResult with all WriteAction variants
        let cases = vec![
            WriteResult {
                path: "CLAUDE.md".to_string(),
                action: WriteAction::Created,
                version: 1,
                error: None,
            },
            WriteResult {
                path: "sub/CLAUDE.md".to_string(),
                action: WriteAction::Updated,
                version: 2,
                error: None,
            },
            WriteResult {
                path: "fail.md".to_string(),
                action: WriteAction::Failed,
                version: 0,
                error: Some("test error".to_string()),
            },
        ];

        for result in &cases {
            let cloned = result.clone();
            assert_eq!(cloned.path, result.path);
            assert_eq!(cloned.action, result.action);
            assert_eq!(cloned.version, result.version);
            assert_eq!(cloned.error, result.error);
        }
    }
}
