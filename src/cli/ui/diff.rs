use crate::generation::markers::ChangeDetection;
use console::style;
use similar::{ChangeTag, TextDiff};
use std::fmt::Write;

/// Per-file change summary for display formatting.
#[derive(Debug, Clone, PartialEq)]
pub struct FileDiff {
    pub path: String,
    pub change: FileChange,
    /// Old managed content (stripped of metadata), if updating an existing file.
    pub old_content: Option<String>,
    /// New managed content from the tailoring output.
    pub new_content: String,
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
    /// * `old_content` – old managed content stripped of metadata (for text diffing).
    /// * `new_content` – new managed content from the tailoring output.
    /// * For a new file the rule count equals `detection.new_ids.len()`.
    /// * For an existing file, `NoChanges` is produced when there are no
    ///   additions (`new_ids`) and no removals (`removed_ids`).  The
    ///   `updated_ids` represent *retained* rules and are not considered
    ///   changes on their own.
    pub fn from_change_detection(
        path: &str,
        detection: &ChangeDetection,
        is_new_file: bool,
        old_content: Option<String>,
        new_content: String,
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
            old_content,
            new_content,
        }
    }
}

/// Maximum number of changed lines to display before truncating.
const MAX_DIFF_LINES: usize = 20;

/// Format a colored content diff between old and new managed content.
///
/// Returns `None` if the content is identical (no changes).
/// Uses `similar::TextDiff` for line-level diffing with colored output:
/// green for additions, red for removals.  Large diffs are truncated
/// after [`MAX_DIFF_LINES`] changed lines.
pub fn format_content_diff(old_content: &str, new_content: &str) -> Option<String> {
    let diff = TextDiff::from_lines(old_content, new_content);

    let has_changes = diff
        .ops()
        .iter()
        .any(|op| !matches!(op.tag(), similar::DiffTag::Equal));
    if !has_changes {
        return None;
    }

    // Count total changed lines for the truncation message.
    let total_changes = diff
        .iter_all_changes()
        .filter(|c| !matches!(c.tag(), ChangeTag::Equal))
        .count();

    let mut output = String::new();
    let mut shown = 0;

    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Delete => {
                if shown < MAX_DIFF_LINES {
                    let line = change.value().trim_end_matches('\n');
                    let _ = writeln!(output, "    {}", style(format!("- {line}")).red());
                    shown += 1;
                }
            }
            ChangeTag::Insert => {
                if shown < MAX_DIFF_LINES {
                    let line = change.value().trim_end_matches('\n');
                    let _ = writeln!(output, "    {}", style(format!("+ {line}")).green());
                    shown += 1;
                }
            }
            ChangeTag::Equal => {}
        }
    }

    if total_changes > MAX_DIFF_LINES {
        let _ = writeln!(
            output,
            "    ... and {} more changes",
            total_changes - MAX_DIFF_LINES
        );
    }

    Some(output)
}

/// Format a single file's change summary.
///
/// Returns a styled, human-readable string suitable for terminal output.
/// Includes a content-level diff (when available) below the numeric summary.
pub fn format_file_diff(diff: &FileDiff) -> String {
    let mut output = String::new();

    match &diff.change {
        FileChange::NewFile { rule_count } => {
            let _ = writeln!(output, "  {}: (new file)", diff.path);
            let noun = if *rule_count == 1 { "rule" } else { "rules" };
            let _ = writeln!(output, "    + {rule_count} {noun}");
            if let Some(content_diff) = format_content_diff("", &diff.new_content) {
                output.push_str(&content_diff);
            }
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
            if let Some(old) = &diff.old_content {
                if let Some(content_diff) = format_content_diff(old, &diff.new_content) {
                    output.push_str(&content_diff);
                }
            }
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

    // ── format_content_diff tests ──

    #[test]
    fn content_diff_identical_returns_none() {
        let result = format_content_diff("same\ncontent\n", "same\ncontent\n");
        assert!(result.is_none(), "identical content should return None");
    }

    #[test]
    fn content_diff_additions_only() {
        let result = format_content_diff("", "line one\nline two\n").unwrap();
        let plain = console::strip_ansi_codes(&result);
        assert!(
            plain.contains("+ line one"),
            "expected addition in: {plain}"
        );
        assert!(
            plain.contains("+ line two"),
            "expected addition in: {plain}"
        );
    }

    #[test]
    fn content_diff_deletions_only() {
        let result = format_content_diff("old line\n", "").unwrap();
        let plain = console::strip_ansi_codes(&result);
        assert!(
            plain.contains("- old line"),
            "expected deletion in: {plain}"
        );
    }

    #[test]
    fn content_diff_mixed_changes() {
        let old = "keep\nremove me\n";
        let new = "keep\nadd me\n";
        let result = format_content_diff(old, new).unwrap();
        let plain = console::strip_ansi_codes(&result);
        assert!(
            plain.contains("- remove me"),
            "expected deletion in: {plain}"
        );
        assert!(plain.contains("+ add me"), "expected addition in: {plain}");
    }

    #[test]
    fn content_diff_truncates_large_diffs() {
        // Generate more than MAX_DIFF_LINES (20) changed lines
        let old_lines: String = (0..15).map(|i| format!("old {i}\n")).collect();
        let new_lines: String = (0..15).map(|i| format!("new {i}\n")).collect();
        let result = format_content_diff(&old_lines, &new_lines).unwrap();
        let plain = console::strip_ansi_codes(&result);
        assert!(
            plain.contains("... and "),
            "expected truncation message in: {plain}"
        );
        assert!(
            plain.contains("more changes"),
            "expected 'more changes' in: {plain}"
        );
    }

    #[test]
    fn content_diff_exactly_max_lines_no_truncation() {
        // Generate exactly MAX_DIFF_LINES (20) changed lines: 10 deletions + 10 insertions
        let old_lines: String = (0..10).map(|i| format!("old {i}\n")).collect();
        let new_lines: String = (0..10).map(|i| format!("new {i}\n")).collect();
        let result = format_content_diff(&old_lines, &new_lines).unwrap();
        let plain = console::strip_ansi_codes(&result);
        assert!(
            !plain.contains("... and "),
            "should not truncate at exactly MAX_DIFF_LINES: {plain}"
        );
    }

    // ── format_file_diff tests ──

    #[test]
    fn format_new_file_output() {
        let diff = FileDiff {
            path: "CLAUDE.md".to_string(),
            change: FileChange::NewFile { rule_count: 8 },
            old_content: None,
            new_content: "some rules".to_string(),
        };
        let output = format_file_diff(&diff);
        let plain = console::strip_ansi_codes(&output);
        assert!(
            plain.contains("(new file)"),
            "expected '(new file)' in: {plain}"
        );
        assert!(
            plain.contains("+ 8 rules"),
            "expected '+ 8 rules' in: {plain}"
        );
    }

    #[test]
    fn format_new_file_includes_content_additions() {
        let diff = FileDiff {
            path: "CLAUDE.md".to_string(),
            change: FileChange::NewFile { rule_count: 1 },
            old_content: None,
            new_content: "brand new rule\n".to_string(),
        };
        let output = format_file_diff(&diff);
        let plain = console::strip_ansi_codes(&output);
        assert!(
            plain.contains("+ brand new rule"),
            "expected content addition in: {plain}"
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
            old_content: Some("old content\n".to_string()),
            new_content: "new content\n".to_string(),
        };
        let output = format_file_diff(&diff);
        let plain = console::strip_ansi_codes(&output);
        assert!(plain.contains('+'), "expected '+' in: {plain}");
        assert!(plain.contains('~'), "expected '~' in: {plain}");
        assert!(plain.contains('-'), "expected '-' in: {plain}");
        assert!(plain.contains('3'), "expected added count in: {plain}");
        assert!(plain.contains('1'), "expected updated count in: {plain}");
        assert!(plain.contains('2'), "expected removed count in: {plain}");
    }

    #[test]
    fn format_update_includes_content_diff() {
        let diff = FileDiff {
            path: "CLAUDE.md".to_string(),
            change: FileChange::Update {
                added: 1,
                updated: 0,
                removed: 1,
            },
            old_content: Some("old line\n".to_string()),
            new_content: "new line\n".to_string(),
        };
        let output = format_file_diff(&diff);
        let plain = console::strip_ansi_codes(&output);
        assert!(
            plain.contains("- old line"),
            "expected deletion in: {plain}"
        );
        assert!(
            plain.contains("+ new line"),
            "expected addition in: {plain}"
        );
    }

    #[test]
    fn format_zero_changes_shows_no_changes() {
        let diff = FileDiff {
            path: "CLAUDE.md".to_string(),
            change: FileChange::NoChanges,
            old_content: Some("same".to_string()),
            new_content: "same".to_string(),
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
            old_content: None,
            new_content: String::new(),
        };
        let output = format_file_diff(&diff);
        assert!(
            output.contains("+ 1 rule"),
            "expected '+ 1 rule' in: {output}"
        );
        // The content diff may add "+ " lines, so check the rule count line specifically
        assert!(
            output.contains("+ 1 rule\n"),
            "expected singular 'rule' line in: {output}"
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
        let diff = FileDiff::from_change_detection(
            "CLAUDE.md",
            &detection,
            true,
            None,
            "content".to_string(),
        );
        assert_eq!(diff.path, "CLAUDE.md");
        assert_eq!(diff.change, FileChange::NewFile { rule_count: 3 });
        assert!(diff.old_content.is_none());
        assert_eq!(diff.new_content, "content");
    }

    #[test]
    fn from_change_detection_update_with_changes() {
        let detection = ChangeDetection {
            new_ids: vec!["new1".into()],
            updated_ids: vec!["kept1".into(), "kept2".into()],
            removed_ids: vec!["gone1".into(), "gone2".into(), "gone3".into()],
        };
        let diff = FileDiff::from_change_detection(
            "apps/web/CLAUDE.md",
            &detection,
            false,
            Some("old".to_string()),
            "new".to_string(),
        );
        assert_eq!(diff.path, "apps/web/CLAUDE.md");
        assert_eq!(
            diff.change,
            FileChange::Update {
                added: 1,
                updated: 2,
                removed: 3,
            }
        );
        assert_eq!(diff.old_content.as_deref(), Some("old"));
        assert_eq!(diff.new_content, "new");
    }

    #[test]
    fn from_change_detection_no_changes() {
        let detection = ChangeDetection {
            new_ids: vec![],
            updated_ids: vec!["kept1".into(), "kept2".into()],
            removed_ids: vec![],
        };
        let diff = FileDiff::from_change_detection(
            "CLAUDE.md",
            &detection,
            false,
            Some("content".to_string()),
            "content".to_string(),
        );
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
                old_content: Some(String::new()),
                new_content: String::new(),
            },
            FileDiff {
                path: "apps/web/CLAUDE.md".to_string(),
                change: FileChange::Update {
                    added: 3,
                    updated: 0,
                    removed: 1,
                },
                old_content: Some(String::new()),
                new_content: String::new(),
            },
            FileDiff {
                path: "apps/api/CLAUDE.md".to_string(),
                change: FileChange::NewFile { rule_count: 8 },
                old_content: None,
                new_content: String::new(),
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
