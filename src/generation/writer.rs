use std::io::ErrorKind;
use std::path::Path;

use super::markers;
use super::merge;
use crate::generation::OutputFormat;
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

/// Write multiple output files to disk.
///
/// For each file in `files`:
/// 1. Determine if root (`path` has no directory separator — just `"CLAUDE.md"`,
///    `"AGENTS.md"`, or `".cursor/rules/actual-policies.mdc"`)
/// 2. Read existing content from `root_dir.join(path)`, if file exists
/// 3. Compute next version via `markers::next_version(existing)`
/// 4. Merge via `merge::merge_content(existing, content, version, is_root, adr_ids, root_header)`
/// 5. Create parent directories with `std::fs::create_dir_all`
/// 6. Write the file with `std::fs::write`
/// 7. Collect `WriteResult`
///
/// The `format` parameter controls what header is prepended to new root-level files.
///
/// Individual file failures do NOT abort the batch — errors are collected per file.
pub fn write_files(
    root_dir: &Path,
    files: &[FileOutput],
    format: &OutputFormat,
) -> Vec<WriteResult> {
    let canonical_root = match root_dir.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            return files
                .iter()
                .map(|file| WriteResult {
                    path: file.path.clone(),
                    action: WriteAction::Failed,
                    version: 0,
                    error: Some(format!("Failed to canonicalize root directory: {e}")),
                })
                .collect();
        }
    };
    files
        .iter()
        .map(|file| {
            let full_path = root_dir.join(&file.path);

            // Layer 1: reject paths with .. components (path traversal guard) or
            // absolute paths (RootDir / Prefix), which would silently ignore root_dir
            // on most operating systems.
            let path_components_invalid = Path::new(&file.path).components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            });
            if path_components_invalid {
                let is_absolute = Path::new(&file.path).is_absolute();
                let error_msg = if is_absolute {
                    "absolute paths are not allowed".to_string()
                } else {
                    "path traversal detected: path escapes root directory".to_string()
                };
                return WriteResult {
                    path: file.path.clone(),
                    action: WriteAction::Failed,
                    version: 0,
                    error: Some(error_msg),
                };
            }

            // Determine if root file.  For ClaudeMd/AgentsMd a root file has no
            // directory component.  For CursorRules the path includes a subdirectory
            // (`.cursor/rules/actual-policies.mdc`) but it is still the canonical
            // "root" output that should receive the format-specific preamble.
            let is_root = file.path == format.filename()
                || Path::new(&file.path)
                    .parent()
                    .is_none_or(|p| p == Path::new(""));

            // Read existing content.
            // Only treat NotFound as "new file"; other errors (permission denied,
            // path is a directory, etc.) must be surfaced to avoid silently
            // overwriting or losing user content.
            let existing_content = match std::fs::read_to_string(&full_path) {
                Ok(c) => Some(c),
                Err(e) if e.kind() == ErrorKind::NotFound => None,
                Err(e) => {
                    return WriteResult {
                        path: file.path.clone(),
                        action: WriteAction::Failed,
                        version: 0,
                        error: Some(format!("failed to read existing file: {e}")),
                    };
                }
            };
            let existing_ref = existing_content.as_deref();

            // Determine if this is a create or update
            let action = if existing_content.is_some() {
                WriteAction::Updated
            } else {
                WriteAction::Created
            };

            // Compute version
            let version = markers::next_version(existing_ref);

            // Merge content — pass the format-specific root header so new root files
            // get the correct preamble (e.g. "# Project Guidelines" vs YAML frontmatter).
            let result = merge::merge_content(
                existing_ref,
                &file.content,
                version,
                is_root,
                &file.adr_ids,
                format.root_header(),
            );

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

            // Layer 2: post-create canonicalization check (defense in depth).
            // If canonicalize fails (e.g. a dangling symlink was somehow created),
            // treat it as a path-escape: an empty PathBuf won't start with canonical_root.
            let canonical_parent = parent
                .canonicalize()
                .unwrap_or_else(|_| std::path::PathBuf::new());
            if !canonical_parent.starts_with(&canonical_root) {
                return WriteResult {
                    path: file.path.clone(),
                    action: WriteAction::Failed,
                    version: 0,
                    error: Some("path escapes root directory".to_string()),
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
    use crate::generation::OutputFormat;

    #[test]
    fn test_write_root_claude_md() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let files = vec![FileOutput {
            path: "CLAUDE.md".to_string(),
            content: "Use consistent error handling.".to_string(),
            reasoning: "Root-level rules".to_string(),
            adr_ids: vec!["adr-001".to_string()],
        }];

        let results = write_files(dir.path(), &files, &OutputFormat::ClaudeMd);

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

        let results = write_files(dir.path(), &files, &OutputFormat::ClaudeMd);

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

        let results = write_files(dir.path(), &files, &OutputFormat::ClaudeMd);

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

        let results = write_files(dir.path(), &files, &OutputFormat::ClaudeMd);

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

        let results = write_files(dir.path(), &files, &OutputFormat::ClaudeMd);

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
        // The error surfaces at either the read stage (NotADirectory for intermediate component)
        // or the directory-creation stage, depending on the OS.
        assert!(
            err_msg.contains("Failed to create directory")
                || err_msg.contains("failed to read existing file"),
            "expected a filesystem error, got: {err_msg}"
        );
    }

    #[test]
    fn test_write_fails_when_file_cannot_be_written() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        // Create a directory at the target file path — reading/writing to a directory fails
        let target = dir.path().join("subdir").join("CLAUDE.md");
        std::fs::create_dir_all(&target).unwrap();

        let files = vec![FileOutput {
            path: "subdir/CLAUDE.md".to_string(),
            content: "content".to_string(),
            reasoning: "test".to_string(),
            adr_ids: vec![],
        }];

        let results = write_files(dir.path(), &files, &OutputFormat::ClaudeMd);

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
        // The error surfaces at either the read stage (is a directory) or the write stage
        assert!(
            err_msg.contains("Failed to write file")
                || err_msg.contains("failed to read existing file"),
            "expected a file operation error, got: {err_msg}"
        );
    }

    #[test]
    fn test_write_path_traversal_fails() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let files = vec![FileOutput {
            path: "../CLAUDE.md".to_string(),
            content: "malicious content".to_string(),
            reasoning: "test".to_string(),
            adr_ids: vec![],
        }];

        let results = write_files(dir.path(), &files, &OutputFormat::ClaudeMd);

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].action,
            WriteAction::Failed,
            "expected Failed action for path traversal"
        );
        assert_eq!(results[0].version, 0, "expected version 0 on failure");
        let err_msg = results[0].error.as_ref().expect("expected error message");
        assert!(
            err_msg.contains("path traversal"),
            "expected 'path traversal' in error, got: {err_msg}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_write_absolute_path_fails() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let files = vec![FileOutput {
            path: "/etc/passwd".to_string(),
            content: "malicious content".to_string(),
            reasoning: "test".to_string(),
            adr_ids: vec![],
        }];

        let results = write_files(dir.path(), &files, &OutputFormat::ClaudeMd);

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].action,
            WriteAction::Failed,
            "expected Failed action for absolute path"
        );
        assert_eq!(results[0].version, 0, "expected version 0 on failure");
        let err_msg = results[0].error.as_ref().expect("expected error message");
        assert!(
            err_msg.contains("absolute paths are not allowed"),
            "expected 'absolute paths are not allowed' in error, got: {err_msg}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_write_absolute_path_arbitrary_fails() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let files = vec![FileOutput {
            path: "/absolute/path/CLAUDE.md".to_string(),
            content: "malicious content".to_string(),
            reasoning: "test".to_string(),
            adr_ids: vec![],
        }];

        let results = write_files(dir.path(), &files, &OutputFormat::ClaudeMd);

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].action,
            WriteAction::Failed,
            "expected Failed action for absolute path"
        );
        assert_eq!(results[0].version, 0, "expected version 0 on failure");
        let err_msg = results[0].error.as_ref().expect("expected error message");
        assert!(
            err_msg.contains("absolute paths are not allowed"),
            "expected 'absolute paths are not allowed' in error, got: {err_msg}"
        );
    }

    #[test]
    fn test_write_fails_when_root_dir_does_not_exist() {
        let nonexistent = Path::new("/tmp/__nonexistent_actual_cli_test_dir__");
        let files = vec![FileOutput {
            path: "CLAUDE.md".to_string(),
            content: "content".to_string(),
            reasoning: "test".to_string(),
            adr_ids: vec![],
        }];

        let results = write_files(nonexistent, &files, &OutputFormat::ClaudeMd);

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].action,
            WriteAction::Failed,
            "expected Failed action when root dir does not exist"
        );
        assert_eq!(results[0].version, 0, "expected version 0 on failure");
        let err_msg = results[0].error.as_ref().expect("expected error message");
        assert!(
            err_msg.contains("Failed to canonicalize root directory"),
            "expected root canonicalize error, got: {err_msg}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_write_layer2_symlink_escape_fails() {
        use std::os::unix::fs::symlink;

        let outer_dir = tempfile::tempdir().expect("failed to create outer temp dir");
        let inner_dir = tempfile::tempdir().expect("failed to create inner temp dir");

        // Create a symlink inside inner_dir that points outside (to outer_dir)
        let symlink_path = inner_dir.path().join("escape");
        symlink(outer_dir.path(), &symlink_path).expect("failed to create symlink");

        let files = vec![FileOutput {
            path: "escape/CLAUDE.md".to_string(),
            content: "malicious content".to_string(),
            reasoning: "test".to_string(),
            adr_ids: vec![],
        }];

        let results = write_files(inner_dir.path(), &files, &OutputFormat::ClaudeMd);

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].action,
            WriteAction::Failed,
            "expected Failed action for symlink escape"
        );
        assert_eq!(results[0].version, 0, "expected version 0 on failure");
        let err_msg = results[0].error.as_ref().expect("expected error message");
        assert!(
            err_msg.contains("path escapes root directory"),
            "expected 'path escapes root directory' in error, got: {err_msg}"
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

    // ── CursorRules format tests ──

    #[test]
    fn test_write_cursor_rules_creates_mdc_with_frontmatter() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let files = vec![FileOutput {
            path: ".cursor/rules/actual-policies.mdc".to_string(),
            content: "Use Tailwind for all styling.".to_string(),
            reasoning: "Cursor rules".to_string(),
            adr_ids: vec!["adr-001".to_string()],
        }];

        let results = write_files(dir.path(), &files, &OutputFormat::CursorRules);

        assert_eq!(results.len(), 1, "expected 1 result");
        assert_eq!(results[0].path, ".cursor/rules/actual-policies.mdc");
        assert_eq!(
            results[0].action,
            WriteAction::Created,
            "expected Created action for new cursor rules file"
        );
        assert_eq!(results[0].version, 1, "expected version 1 for new file");
        assert!(results[0].error.is_none(), "expected no error");

        // Verify .cursor/rules/ directory was created
        assert!(
            dir.path().join(".cursor/rules").is_dir(),
            "expected .cursor/rules directory to be created"
        );

        let written = std::fs::read_to_string(dir.path().join(".cursor/rules/actual-policies.mdc"))
            .expect("failed to read written file");

        // Must start with YAML frontmatter
        assert!(
            written.starts_with("---\n"),
            "cursor rules file must start with YAML front-matter delimiter, got: {written}"
        );
        assert!(
            written.contains("alwaysApply: true"),
            "cursor rules file must contain alwaysApply: true"
        );
        assert!(
            markers::has_managed_section(&written),
            "cursor rules file should have managed section"
        );
        assert!(
            written.contains("Use Tailwind for all styling."),
            "cursor rules file should contain managed content"
        );
    }

    #[test]
    fn test_write_agents_md_root_has_project_guidelines_header() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let files = vec![FileOutput {
            path: "AGENTS.md".to_string(),
            content: "Follow Python conventions.".to_string(),
            reasoning: "Root rules".to_string(),
            adr_ids: vec!["adr-001".to_string()],
        }];

        let results = write_files(dir.path(), &files, &OutputFormat::AgentsMd);

        assert_eq!(results.len(), 1, "expected 1 result");
        assert!(results[0].error.is_none(), "expected no error");

        let written = std::fs::read_to_string(dir.path().join("AGENTS.md"))
            .expect("failed to read written file");
        assert!(
            written.contains("# Project Guidelines"),
            "AGENTS.md root file should have Project Guidelines header, got: {written}"
        );
        assert!(markers::has_managed_section(&written));
    }

    // ── 4eo.4: create_dir_all failure returns Failed ──

    #[test]
    #[cfg(unix)]
    fn test_write_create_dir_fails_returns_failed() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("failed to create temp dir");

        // Make the root dir read-only so that creating a new subdirectory fails.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555))
            .expect("failed to set permissions");

        let files = vec![FileOutput {
            path: "newsubdir/CLAUDE.md".to_string(),
            content: "content".to_string(),
            reasoning: "test".to_string(),
            adr_ids: vec![],
        }];

        let results = write_files(dir.path(), &files, &OutputFormat::ClaudeMd);

        // Restore permissions for cleanup
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
            .expect("failed to restore permissions");

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].action,
            WriteAction::Failed,
            "expected Failed when create_dir_all fails"
        );
        assert_eq!(results[0].version, 0, "expected version 0 on failure");
        let err_msg = results[0].error.as_ref().expect("expected error message");
        assert!(
            err_msg.contains("Failed to create directory"),
            "expected 'Failed to create directory' in error, got: {err_msg}"
        );
    }

    // ── 4eo.4: fs::write failure returns Failed ──

    #[test]
    #[cfg(unix)]
    fn test_write_to_readonly_file_returns_failed() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let target = dir.path().join("CLAUDE.md");

        // Create the file and make it read-only.
        std::fs::write(&target, "existing content").expect("failed to write initial file");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o444))
            .expect("failed to set permissions");

        let files = vec![FileOutput {
            path: "CLAUDE.md".to_string(),
            content: "new content".to_string(),
            reasoning: "test".to_string(),
            adr_ids: vec![],
        }];

        let results = write_files(dir.path(), &files, &OutputFormat::ClaudeMd);

        // Restore permissions for cleanup
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644))
            .expect("failed to restore permissions");

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].action,
            WriteAction::Failed,
            "expected Failed when fs::write fails on read-only file"
        );
        assert_eq!(results[0].version, 0, "expected version 0 on failure");
        let err_msg = results[0].error.as_ref().expect("expected error message");
        assert!(
            err_msg.contains("Failed to write file"),
            "expected 'Failed to write file' in error, got: {err_msg}"
        );
    }

    // ── 4eo.3: Unreadable existing files return Failed, not Created ──

    #[test]
    #[cfg(unix)]
    fn test_write_unreadable_existing_file_returns_failed() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let target = dir.path().join("CLAUDE.md");

        // Create the file and make it unreadable.
        std::fs::write(&target, "existing content").expect("failed to write initial file");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o000))
            .expect("failed to set permissions");

        let files = vec![FileOutput {
            path: "CLAUDE.md".to_string(),
            content: "new content".to_string(),
            reasoning: "test".to_string(),
            adr_ids: vec![],
        }];

        let results = write_files(dir.path(), &files, &OutputFormat::ClaudeMd);

        // Restore permissions for cleanup
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644))
            .expect("failed to restore permissions");

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].action,
            WriteAction::Failed,
            "expected Failed when existing file is unreadable, not Created"
        );
        assert_eq!(results[0].version, 0, "expected version 0 on failure");
        let err_msg = results[0].error.as_ref().expect("expected error message");
        assert!(
            err_msg.contains("failed to read existing file"),
            "expected 'failed to read existing file' in error, got: {err_msg}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_write_path_is_directory_returns_failed() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        // Create a directory at the path where a file should be read.
        let target = dir.path().join("CLAUDE.md");
        std::fs::create_dir(&target).expect("failed to create directory");

        let files = vec![FileOutput {
            path: "CLAUDE.md".to_string(),
            content: "content".to_string(),
            reasoning: "test".to_string(),
            adr_ids: vec![],
        }];

        let results = write_files(dir.path(), &files, &OutputFormat::ClaudeMd);

        assert_eq!(results.len(), 1);
        // Reading a directory as a file fails — should return Failed, not Created
        assert_eq!(
            results[0].action,
            WriteAction::Failed,
            "expected Failed when path is a directory"
        );
        let err_msg = results[0].error.as_ref().expect("expected error message");
        // The read error message should either mention reading or writing failure
        assert!(
            err_msg.contains("failed to read existing file") || err_msg.contains("Failed to write"),
            "expected an error about reading or writing, got: {err_msg}"
        );
    }

    // ── 4eo.4: Distinct error message when canonicalize fails vs path escape ──

    #[test]
    #[cfg(unix)]
    fn test_write_layer2_symlink_escape_has_distinct_error_from_io_failure() {
        use std::os::unix::fs::symlink;

        let outer_dir = tempfile::tempdir().expect("failed to create outer temp dir");
        let inner_dir = tempfile::tempdir().expect("failed to create inner temp dir");

        // Create a symlink inside inner_dir that points outside (to outer_dir).
        // This is a genuine path-escape and should produce "path escapes root directory".
        let symlink_path = inner_dir.path().join("escape");
        symlink(outer_dir.path(), &symlink_path).expect("failed to create symlink");

        let files = vec![FileOutput {
            path: "escape/CLAUDE.md".to_string(),
            content: "content".to_string(),
            reasoning: "test".to_string(),
            adr_ids: vec![],
        }];

        let results = write_files(inner_dir.path(), &files, &OutputFormat::ClaudeMd);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].action, WriteAction::Failed);
        let err_msg = results[0].error.as_ref().expect("expected error message");
        // A real symlink escape must say "path escapes root directory",
        // NOT "failed to verify output path".
        assert!(
            err_msg.contains("path escapes root directory"),
            "real path escape should say 'path escapes root directory', got: {err_msg}"
        );
        assert!(
            !err_msg.contains("failed to verify output path"),
            "real path escape should not say 'failed to verify output path', got: {err_msg}"
        );
    }
}
