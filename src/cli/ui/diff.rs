use crate::generation::markers::ChangeDetection;
use std::fmt::Write;

/// Per-file change summary for display formatting.
#[derive(Debug, Clone, PartialEq)]
pub struct FileDiff {
    pub path: String,
    pub change: FileChange,
}

/// Categorized change type for a file.
#[derive(Debug, Clone, PartialEq)]
pub enum FileChange {
    /// Brand new file (old_content was None).
    NewFile { rule_count: usize },
    /// Existing file with changes.
    Update {
        added: usize,
        updated: usize,
        removed: usize,
    },
    /// Existing file with zero changes.
    NoChanges,
}

impl FileDiff {
    /// Construct a [`FileDiff`] from a [`ChangeDetection`].
    ///
    /// * `is_new_file` – set when the old content was `None` (first sync).
    /// * For a new file the rule count equals `detection.new_ids.len()`.
    /// * For an existing file, `NoChanges` is produced when there are no
    ///   additions (`new_ids`) and no removals (`removed_ids`).  The
    ///   `updated_ids` represent *retained* rules and are not considered
    ///   changes on their own.
    pub fn from_change_detection(
        path: &str,
        detection: &ChangeDetection,
        is_new_file: bool,
    ) -> Self {
        let change = if is_new_file {
            FileChange::NewFile {
                rule_count: detection.new_ids.len(),
            }
        } else if detection.new_ids.is_empty() && detection.removed_ids.is_empty() {
            FileChange::NoChanges
        } else {
            FileChange::Update {
                added: detection.new_ids.len(),
                updated: detection.updated_ids.len(),
                removed: detection.removed_ids.len(),
            }
        };

        Self {
            path: path.to_string(),
            change,
        }
    }
}

/// Format a single file's change summary.
///
/// Returns a styled, human-readable string suitable for terminal output.
pub fn format_file_diff(diff: &FileDiff) -> String {
    let mut output = String::new();

    match &diff.change {
        FileChange::NewFile { rule_count } => {
            let _ = writeln!(output, "  {}: (new file)", diff.path);
            let noun = if *rule_count == 1 { "rule" } else { "rules" };
            let _ = writeln!(output, "    + {rule_count} {noun}");
        }
        FileChange::Update {
            added,
            updated,
            removed,
        } => {
            let _ = writeln!(output, "  {}:", diff.path);
            let _ = writeln!(
                output,
                "    + {added} new, ~ {updated} updated, - {removed} removed"
            );
        }
        FileChange::NoChanges => {
            let _ = writeln!(output, "  {}: no changes", diff.path);
        }
    }

    output
}

/// Format a multi-file diff summary.
///
/// Iterates over each [`FileDiff`] and concatenates their formatted output.
/// Returns an empty string when `diffs` is empty.
pub fn format_diff_summary(diffs: &[FileDiff]) -> String {
    let mut output = String::new();

    for diff in diffs {
        output.push_str(&format_file_diff(diff));
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::markers::ChangeDetection;

    // ── format_file_diff tests ──

    #[test]
    fn format_new_file_output() {
        let diff = FileDiff {
            path: "CLAUDE.md".to_string(),
            change: FileChange::NewFile { rule_count: 8 },
        };
        let output = format_file_diff(&diff);
        assert!(
            output.contains("(new file)"),
            "expected '(new file)' in: {output}"
        );
        assert!(
            output.contains("+ 8 rules"),
            "expected '+ 8 rules' in: {output}"
        );
    }

    #[test]
    fn format_update_output() {
        let diff = FileDiff {
            path: "apps/web/CLAUDE.md".to_string(),
            change: FileChange::Update {
                added: 3,
                updated: 1,
                removed: 2,
            },
        };
        let output = format_file_diff(&diff);
        assert!(output.contains('+'), "expected '+' in: {output}");
        assert!(output.contains('~'), "expected '~' in: {output}");
        assert!(output.contains('-'), "expected '-' in: {output}");
        assert!(output.contains('3'), "expected added count in: {output}");
        assert!(output.contains('1'), "expected updated count in: {output}");
        assert!(output.contains('2'), "expected removed count in: {output}");
    }

    #[test]
    fn format_zero_changes_shows_no_changes() {
        let diff = FileDiff {
            path: "CLAUDE.md".to_string(),
            change: FileChange::NoChanges,
        };
        let output = format_file_diff(&diff);
        assert!(
            output.contains("no changes"),
            "expected 'no changes' in: {output}"
        );
    }

    #[test]
    fn format_new_file_single_rule() {
        let diff = FileDiff {
            path: "CLAUDE.md".to_string(),
            change: FileChange::NewFile { rule_count: 1 },
        };
        let output = format_file_diff(&diff);
        assert!(
            output.contains("+ 1 rule"),
            "expected '+ 1 rule' in: {output}"
        );
        assert!(
            !output.contains("rules"),
            "expected singular 'rule' not 'rules' in: {output}"
        );
    }

    // ── from_change_detection tests ──

    #[test]
    fn from_change_detection_new_file() {
        let detection = ChangeDetection {
            new_ids: vec!["a".into(), "b".into(), "c".into()],
            updated_ids: vec![],
            removed_ids: vec![],
        };
        let diff = FileDiff::from_change_detection("CLAUDE.md", &detection, true);
        assert_eq!(diff.path, "CLAUDE.md");
        assert_eq!(diff.change, FileChange::NewFile { rule_count: 3 });
    }

    #[test]
    fn from_change_detection_update_with_changes() {
        let detection = ChangeDetection {
            new_ids: vec!["new1".into()],
            updated_ids: vec!["kept1".into(), "kept2".into()],
            removed_ids: vec!["gone1".into(), "gone2".into(), "gone3".into()],
        };
        let diff = FileDiff::from_change_detection("apps/web/CLAUDE.md", &detection, false);
        assert_eq!(diff.path, "apps/web/CLAUDE.md");
        assert_eq!(
            diff.change,
            FileChange::Update {
                added: 1,
                updated: 2,
                removed: 3,
            }
        );
    }

    #[test]
    fn from_change_detection_no_changes() {
        let detection = ChangeDetection {
            new_ids: vec![],
            updated_ids: vec!["kept1".into(), "kept2".into()],
            removed_ids: vec![],
        };
        let diff = FileDiff::from_change_detection("CLAUDE.md", &detection, false);
        assert_eq!(diff.change, FileChange::NoChanges);
    }

    // ── format_diff_summary tests ──

    #[test]
    fn format_diff_summary_multiple_files() {
        let diffs = vec![
            FileDiff {
                path: "CLAUDE.md".to_string(),
                change: FileChange::Update {
                    added: 2,
                    updated: 1,
                    removed: 0,
                },
            },
            FileDiff {
                path: "apps/web/CLAUDE.md".to_string(),
                change: FileChange::Update {
                    added: 3,
                    updated: 0,
                    removed: 1,
                },
            },
            FileDiff {
                path: "apps/api/CLAUDE.md".to_string(),
                change: FileChange::NewFile { rule_count: 8 },
            },
        ];
        let output = format_diff_summary(&diffs);
        assert!(
            output.contains("CLAUDE.md"),
            "expected root path in: {output}"
        );
        assert!(
            output.contains("apps/web/CLAUDE.md"),
            "expected web path in: {output}"
        );
        assert!(
            output.contains("apps/api/CLAUDE.md"),
            "expected api path in: {output}"
        );
    }

    #[test]
    fn format_diff_summary_empty() {
        let output = format_diff_summary(&[]);
        assert!(output.is_empty(), "expected empty output, got: {output}");
    }
}
