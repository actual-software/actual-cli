use crate::generation::markers::ChangeDetection;
use similar::{ChangeTag, TextDiff};
use std::fmt::Write;

use super::theme;
use console;

/// Per-ADR change detail within a file.
#[derive(Debug, Clone, PartialEq)]
pub struct AdrDiff {
    pub adr_id: String,
    pub change: AdrChange,
    pub old_content: Option<String>,
    pub new_content: Option<String>,
}

/// Type of change for a single ADR section.
#[derive(Debug, Clone, PartialEq)]
pub enum AdrChange {
    Added,
    Updated,
    Unchanged,
    Removed,
}

/// Per-file change summary for display formatting.
#[derive(Debug, Clone, PartialEq)]
pub struct FileDiff {
    pub path: String,
    pub change: FileChange,
    /// Old managed content (stripped of metadata), if updating an existing file.
    pub old_content: Option<String>,
    /// New managed content from the tailoring output.
    pub new_content: String,
    /// Per-ADR change details (empty for backward compat with `from_change_detection`).
    pub adr_diffs: Vec<AdrDiff>,
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
            adr_diffs: vec![],
        }
    }

    /// Construct a [`FileDiff`] from old/new per-ADR sections, producing
    /// per-ADR change details (`AdrDiff`) alongside the file-level summary.
    ///
    /// * `detection` – the `ChangeDetection` from `markers::detect_changes`.
    /// * `is_new_file` – set when the old content was `None` (first sync).
    /// * `old_sections` – per-ADR sections extracted from the existing managed content.
    /// * `new_sections` – per-ADR sections from the new tailoring output.
    /// * `old_content` – old managed content stripped of metadata (for text diffing).
    /// * `new_content` – new managed content from the tailoring output.
    pub fn from_sections(
        path: &str,
        detection: &ChangeDetection,
        is_new_file: bool,
        old_sections: &[crate::generation::markers::AdrSection],
        new_sections: &[crate::tailoring::types::AdrSection],
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

        // Build a lookup from old sections by ID
        let old_by_id: std::collections::HashMap<&str, &str> = old_sections
            .iter()
            .map(|s| (s.id.as_str(), s.content.as_str()))
            .collect();

        let new_by_id: std::collections::HashSet<&str> =
            new_sections.iter().map(|s| s.adr_id.as_str()).collect();

        let mut adr_diffs = Vec::new();

        // Process new sections: Added, Updated, or Unchanged
        for new_sec in new_sections {
            if let Some(&old_content_str) = old_by_id.get(new_sec.adr_id.as_str()) {
                if old_content_str == new_sec.content {
                    adr_diffs.push(AdrDiff {
                        adr_id: new_sec.adr_id.clone(),
                        change: AdrChange::Unchanged,
                        old_content: Some(old_content_str.to_string()),
                        new_content: Some(new_sec.content.clone()),
                    });
                } else {
                    adr_diffs.push(AdrDiff {
                        adr_id: new_sec.adr_id.clone(),
                        change: AdrChange::Updated,
                        old_content: Some(old_content_str.to_string()),
                        new_content: Some(new_sec.content.clone()),
                    });
                }
            } else {
                adr_diffs.push(AdrDiff {
                    adr_id: new_sec.adr_id.clone(),
                    change: AdrChange::Added,
                    old_content: None,
                    new_content: Some(new_sec.content.clone()),
                });
            }
        }

        // Process removed sections: old sections not in new set
        for old_sec in old_sections {
            if !new_by_id.contains(old_sec.id.as_str()) {
                adr_diffs.push(AdrDiff {
                    adr_id: old_sec.id.clone(),
                    change: AdrChange::Removed,
                    old_content: Some(old_sec.content.clone()),
                    new_content: None,
                });
            }
        }

        Self {
            path: path.to_string(),
            change,
            old_content,
            new_content,
            adr_diffs,
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
///
/// Each returned string is a single styled diff line (e.g. `"+ new rule"`)
/// without any tree-drawing prefix — the caller adds those.
pub fn format_content_diff(old_content: &str, new_content: &str) -> Option<Vec<String>> {
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

    let mut lines = Vec::new();
    let mut shown = 0;

    for change in diff.iter_all_changes() {
        if shown >= MAX_DIFF_LINES {
            break;
        }
        match change.tag() {
            ChangeTag::Delete => {
                let line = console::strip_ansi_codes(change.value().trim_end_matches('\n'));
                lines.push(format!("{}", theme::error(format!("- {line}"))));
                shown += 1;
            }
            ChangeTag::Insert => {
                let line = console::strip_ansi_codes(change.value().trim_end_matches('\n'));
                lines.push(format!("{}", theme::success(format!("+ {line}"))));
                shown += 1;
            }
            ChangeTag::Equal => {}
        }
    }

    if total_changes > MAX_DIFF_LINES {
        lines.push(format!(
            "... and {} more changes",
            total_changes - MAX_DIFF_LINES
        ));
    }

    Some(lines)
}

/// Format per-ADR change details as styled lines.
///
/// - Added: green `+ [adr-id] added` with content additions
/// - Updated: yellow `~ [adr-id] updated` with content diff
/// - Removed: red `- [adr-id] removed`
/// - Unchanged: skipped (no output)
///
/// ADR IDs are abbreviated to the first 8 characters for compact display.
pub fn format_adr_diffs(adr_diffs: &[AdrDiff]) -> Vec<String> {
    let mut lines = Vec::new();

    for adr_diff in adr_diffs {
        let short_id = if adr_diff.adr_id.len() > 8 {
            &adr_diff.adr_id[..8]
        } else {
            &adr_diff.adr_id
        };

        match &adr_diff.change {
            AdrChange::Added => {
                lines.push(format!(
                    "{}",
                    theme::success(format!("+ [{short_id}] added"))
                ));
                // Show content additions if available
                if let Some(content) = &adr_diff.new_content {
                    for line in content.lines().take(3) {
                        let safe = console::strip_ansi_codes(line);
                        lines.push(format!("  {}", theme::success(format!("+ {safe}"))));
                    }
                    let total_lines = content.lines().count();
                    if total_lines > 3 {
                        lines.push(format!("  ... and {} more lines", total_lines - 3));
                    }
                }
            }
            AdrChange::Updated => {
                lines.push(format!(
                    "{}",
                    theme::warning(format!("~ [{short_id}] updated"))
                ));
                // Show content diff if both old and new are available
                if let (Some(old), Some(new)) = (&adr_diff.old_content, &adr_diff.new_content) {
                    if let Some(diff_lines) = format_content_diff(old, new) {
                        for dl in diff_lines.iter().take(5) {
                            lines.push(format!("  {dl}"));
                        }
                        if diff_lines.len() > 5 {
                            lines.push(format!("  ... and {} more changes", diff_lines.len() - 5));
                        }
                    }
                }
            }
            AdrChange::Removed => {
                lines.push(format!(
                    "{}",
                    theme::error(format!("- [{short_id}] removed"))
                ));
            }
            AdrChange::Unchanged => {
                // Skip unchanged sections
            }
        }
    }

    lines
}

/// Format a single file's change summary.
///
/// Returns a styled, human-readable string suitable for terminal output.
/// The header line has the path left-aligned and compact counts right-aligned.
/// Content-level diffs use tree-drawing prefixes (`├─` / `└─`).
///
/// When `adr_diffs` is non-empty, per-ADR display is used instead of the
/// whole-file diff for `Update` and `NewFile` variants.
pub fn format_file_diff(diff: &FileDiff) -> String {
    let mut output = String::new();
    let safe_path = console::strip_ansi_codes(&diff.path);

    match &diff.change {
        FileChange::NewFile { rule_count } => {
            let noun = if *rule_count == 1 { "rule" } else { "rules" };
            let _ = writeln!(output, "{safe_path}  (new file) + {rule_count} {noun}");
            if !diff.adr_diffs.is_empty() {
                let adr_lines = format_adr_diffs(&diff.adr_diffs);
                if !adr_lines.is_empty() {
                    append_tree_lines(&mut output, &adr_lines);
                }
            } else if let Some(diff_lines) = format_content_diff("", &diff.new_content) {
                append_tree_lines(&mut output, &diff_lines);
            }
        }
        FileChange::Update {
            added,
            updated,
            removed,
        } => {
            let _ = writeln!(
                output,
                "{safe_path}  + {added} new  ~ {updated} upd  - {removed} rm",
            );
            if !diff.adr_diffs.is_empty() {
                let adr_lines = format_adr_diffs(&diff.adr_diffs);
                if !adr_lines.is_empty() {
                    append_tree_lines(&mut output, &adr_lines);
                }
            } else if let Some(old) = &diff.old_content {
                if let Some(diff_lines) = format_content_diff(old, &diff.new_content) {
                    append_tree_lines(&mut output, &diff_lines);
                }
            }
        }
        FileChange::NoChanges => {
            let _ = writeln!(output, "{safe_path}  no changes");
        }
    }

    output
}

/// Append diff lines with tree-drawing prefixes (`├─` / `└─`).
fn append_tree_lines(output: &mut String, lines: &[String]) {
    let n = lines.len();
    for (i, line) in lines.iter().enumerate() {
        let prefix = if i == n - 1 { "└─" } else { "├─" };
        let _ = writeln!(output, "{prefix}  {line}");
    }
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
        let lines = format_content_diff("", "line one\nline two\n").unwrap();
        let joined = lines.join("\n");
        let plain = console::strip_ansi_codes(&joined);
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
        let lines = format_content_diff("old line\n", "").unwrap();
        let joined = lines.join("\n");
        let plain = console::strip_ansi_codes(&joined);
        assert!(
            plain.contains("- old line"),
            "expected deletion in: {plain}"
        );
    }

    #[test]
    fn content_diff_mixed_changes() {
        let old = "keep\nremove me\n";
        let new = "keep\nadd me\n";
        let lines = format_content_diff(old, new).unwrap();
        let joined = lines.join("\n");
        let plain = console::strip_ansi_codes(&joined);
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
        let lines = format_content_diff(&old_lines, &new_lines).unwrap();
        let joined = lines.join("\n");
        let plain = console::strip_ansi_codes(&joined);
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
        let lines = format_content_diff(&old_lines, &new_lines).unwrap();
        let joined = lines.join("\n");
        let plain = console::strip_ansi_codes(&joined);
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
            adr_diffs: vec![],
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
        assert!(plain.contains("CLAUDE.md"), "expected path in: {plain}");
    }

    #[test]
    fn format_new_file_includes_content_additions() {
        let diff = FileDiff {
            path: "CLAUDE.md".to_string(),
            change: FileChange::NewFile { rule_count: 1 },
            old_content: None,
            new_content: "brand new rule\n".to_string(),
            adr_diffs: vec![],
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
            adr_diffs: vec![],
        };
        let output = format_file_diff(&diff);
        let plain = console::strip_ansi_codes(&output);
        assert!(plain.contains("+ 3 new"), "expected '+ 3 new' in: {plain}");
        assert!(plain.contains("~ 1 upd"), "expected '~ 1 upd' in: {plain}");
        assert!(plain.contains("- 2 rm"), "expected '- 2 rm' in: {plain}");
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
            adr_diffs: vec![],
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
        // Tree-drawing chars should be present
        assert!(
            plain.contains("├─") || plain.contains("└─"),
            "expected tree-drawing chars in: {plain}"
        );
    }

    #[test]
    fn format_update_without_old_content_skips_diff() {
        let diff = FileDiff {
            path: "CLAUDE.md".to_string(),
            change: FileChange::Update {
                added: 1,
                updated: 0,
                removed: 0,
            },
            old_content: None,
            new_content: "new content\n".to_string(),
            adr_diffs: vec![],
        };
        let output = format_file_diff(&diff);
        let plain = console::strip_ansi_codes(&output);
        // Should show compact numeric summary but no content diff lines
        assert!(
            plain.contains("+ 1 new"),
            "expected numeric summary in: {plain}"
        );
        assert!(
            plain.contains("~ 0 upd"),
            "expected update count in: {plain}"
        );
        assert!(
            !plain.contains("+ new content"),
            "should not show content diff when old_content is None: {plain}"
        );
    }

    #[test]
    fn format_update_identical_content_no_diff() {
        let diff = FileDiff {
            path: "CLAUDE.md".to_string(),
            change: FileChange::Update {
                added: 1,
                updated: 0,
                removed: 0,
            },
            old_content: Some("same content\n".to_string()),
            new_content: "same content\n".to_string(),
            adr_diffs: vec![],
        };
        let output = format_file_diff(&diff);
        let plain = console::strip_ansi_codes(&output);
        // Should show compact numeric summary but no content diff
        assert!(
            plain.contains("+ 1 new"),
            "expected numeric summary in: {plain}"
        );
        assert!(
            !plain.contains("- same"),
            "should not show deletion for identical content: {plain}"
        );
        assert!(
            !plain.contains("+ same"),
            "should not show addition for identical content: {plain}"
        );
    }

    #[test]
    fn format_zero_changes_shows_no_changes() {
        let diff = FileDiff {
            path: "CLAUDE.md".to_string(),
            change: FileChange::NoChanges,
            old_content: Some("same".to_string()),
            new_content: "same".to_string(),
            adr_diffs: vec![],
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
            adr_diffs: vec![],
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
                adr_diffs: vec![],
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
                adr_diffs: vec![],
            },
            FileDiff {
                path: "apps/api/CLAUDE.md".to_string(),
                change: FileChange::NewFile { rule_count: 8 },
                old_content: None,
                new_content: String::new(),
                adr_diffs: vec![],
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

    // ── tree-drawing and compact count tests ──

    #[test]
    fn compact_count_format_in_update() {
        let diff = FileDiff {
            path: "CLAUDE.md".to_string(),
            change: FileChange::Update {
                added: 5,
                updated: 2,
                removed: 1,
            },
            old_content: Some(String::new()),
            new_content: String::new(),
            adr_diffs: vec![],
        };
        let output = format_file_diff(&diff);
        let plain = console::strip_ansi_codes(&output);
        assert!(
            plain.contains("+ 5 new  ~ 2 upd  - 1 rm"),
            "expected compact count format in: {plain}"
        );
    }

    #[test]
    fn single_diff_line_uses_last_prefix() {
        let diff = FileDiff {
            path: "CLAUDE.md".to_string(),
            change: FileChange::Update {
                added: 1,
                updated: 0,
                removed: 0,
            },
            old_content: Some(String::new()),
            new_content: "only line\n".to_string(),
            adr_diffs: vec![],
        };
        let output = format_file_diff(&diff);
        let plain = console::strip_ansi_codes(&output);
        // Single diff line should use └─ (last/only line)
        assert!(
            plain.contains("└─"),
            "expected └─ for single diff line in: {plain}"
        );
        assert!(
            !plain.contains("├─"),
            "should not have ├─ for single diff line in: {plain}"
        );
    }

    #[test]
    fn multiple_diff_lines_use_correct_tree_prefixes() {
        let diff = FileDiff {
            path: "CLAUDE.md".to_string(),
            change: FileChange::Update {
                added: 1,
                updated: 0,
                removed: 1,
            },
            old_content: Some("old line\n".to_string()),
            new_content: "new line\n".to_string(),
            adr_diffs: vec![],
        };
        let output = format_file_diff(&diff);
        let plain = console::strip_ansi_codes(&output);
        // Multiple diff lines: first uses ├─, last uses └─
        assert!(
            plain.contains("├─"),
            "expected ├─ for non-last diff line in: {plain}"
        );
        assert!(
            plain.contains("└─"),
            "expected └─ for last diff line in: {plain}"
        );
    }

    #[test]
    fn no_leading_indent_in_file_header() {
        let diff = FileDiff {
            path: "CLAUDE.md".to_string(),
            change: FileChange::NoChanges,
            old_content: Some("same".to_string()),
            new_content: "same".to_string(),
            adr_diffs: vec![],
        };
        let output = format_file_diff(&diff);
        // Should NOT start with 2-space indent
        assert!(
            output.starts_with("CLAUDE.md"),
            "expected path at start of line, got: {output}"
        );
    }

    #[test]
    fn new_file_header_format() {
        let diff = FileDiff {
            path: "apps/web/CLAUDE.md".to_string(),
            change: FileChange::NewFile { rule_count: 8 },
            old_content: None,
            new_content: String::new(),
            adr_diffs: vec![],
        };
        let output = format_file_diff(&diff);
        let plain = console::strip_ansi_codes(&output);
        assert!(
            plain.contains("apps/web/CLAUDE.md"),
            "expected path in: {plain}"
        );
        assert!(
            plain.contains("(new file) + 8 rules"),
            "expected '(new file) + 8 rules' in: {plain}"
        );
    }

    // ── from_sections tests ──

    #[test]
    fn test_from_sections_new_file_all_added() {
        use crate::generation::markers::AdrSection as MarkerSection;
        use crate::tailoring::types::AdrSection as TypeSection;

        let detection = ChangeDetection {
            new_ids: vec!["adr-1".into(), "adr-2".into()],
            updated_ids: vec![],
            removed_ids: vec![],
        };
        let new_sections = vec![
            TypeSection {
                adr_id: "adr-1".to_string(),
                content: "Rule one".to_string(),
            },
            TypeSection {
                adr_id: "adr-2".to_string(),
                content: "Rule two".to_string(),
            },
        ];
        let old_sections: Vec<MarkerSection> = vec![];

        let diff = FileDiff::from_sections(
            "CLAUDE.md",
            &detection,
            true,
            &old_sections,
            &new_sections,
            None,
            "Rule one\n\nRule two".to_string(),
        );

        assert_eq!(diff.change, FileChange::NewFile { rule_count: 2 });
        assert_eq!(diff.adr_diffs.len(), 2);
        assert_eq!(diff.adr_diffs[0].change, AdrChange::Added);
        assert_eq!(diff.adr_diffs[0].adr_id, "adr-1");
        assert_eq!(diff.adr_diffs[1].change, AdrChange::Added);
        assert_eq!(diff.adr_diffs[1].adr_id, "adr-2");
    }

    #[test]
    fn test_from_sections_update_mixed_changes() {
        use crate::generation::markers::AdrSection as MarkerSection;
        use crate::tailoring::types::AdrSection as TypeSection;

        let detection = ChangeDetection {
            new_ids: vec!["adr-3".into()],
            updated_ids: vec!["adr-1".into()],
            removed_ids: vec!["adr-2".into()],
        };
        let old_sections = vec![
            MarkerSection {
                id: "adr-1".to_string(),
                content: "Old rule one".to_string(),
            },
            MarkerSection {
                id: "adr-2".to_string(),
                content: "Old rule two".to_string(),
            },
        ];
        let new_sections = vec![
            TypeSection {
                adr_id: "adr-1".to_string(),
                content: "Updated rule one".to_string(),
            },
            TypeSection {
                adr_id: "adr-3".to_string(),
                content: "New rule three".to_string(),
            },
        ];

        let diff = FileDiff::from_sections(
            "CLAUDE.md",
            &detection,
            false,
            &old_sections,
            &new_sections,
            Some("Old rule one\n\nOld rule two".to_string()),
            "Updated rule one\n\nNew rule three".to_string(),
        );

        assert_eq!(
            diff.change,
            FileChange::Update {
                added: 1,
                updated: 1,
                removed: 1,
            }
        );
        assert_eq!(diff.adr_diffs.len(), 3);

        // adr-1: Updated (content differs)
        assert_eq!(diff.adr_diffs[0].adr_id, "adr-1");
        assert_eq!(diff.adr_diffs[0].change, AdrChange::Updated);

        // adr-3: Added
        assert_eq!(diff.adr_diffs[1].adr_id, "adr-3");
        assert_eq!(diff.adr_diffs[1].change, AdrChange::Added);

        // adr-2: Removed
        assert_eq!(diff.adr_diffs[2].adr_id, "adr-2");
        assert_eq!(diff.adr_diffs[2].change, AdrChange::Removed);
    }

    #[test]
    fn test_from_sections_content_identical_marks_unchanged() {
        use crate::generation::markers::AdrSection as MarkerSection;
        use crate::tailoring::types::AdrSection as TypeSection;

        let detection = ChangeDetection {
            new_ids: vec![],
            updated_ids: vec!["adr-1".into()],
            removed_ids: vec![],
        };
        let old_sections = vec![MarkerSection {
            id: "adr-1".to_string(),
            content: "Same content".to_string(),
        }];
        let new_sections = vec![TypeSection {
            adr_id: "adr-1".to_string(),
            content: "Same content".to_string(),
        }];

        let diff = FileDiff::from_sections(
            "CLAUDE.md",
            &detection,
            false,
            &old_sections,
            &new_sections,
            Some("Same content".to_string()),
            "Same content".to_string(),
        );

        assert_eq!(diff.adr_diffs.len(), 1);
        assert_eq!(diff.adr_diffs[0].change, AdrChange::Unchanged);
    }

    #[test]
    fn test_from_sections_removed_adr() {
        use crate::generation::markers::AdrSection as MarkerSection;
        use crate::tailoring::types::AdrSection as TypeSection;

        let detection = ChangeDetection {
            new_ids: vec![],
            updated_ids: vec![],
            removed_ids: vec!["adr-old".into()],
        };
        let old_sections = vec![MarkerSection {
            id: "adr-old".to_string(),
            content: "Old content".to_string(),
        }];
        let new_sections: Vec<TypeSection> = vec![];

        let diff = FileDiff::from_sections(
            "CLAUDE.md",
            &detection,
            false,
            &old_sections,
            &new_sections,
            Some("Old content".to_string()),
            String::new(),
        );

        assert_eq!(diff.adr_diffs.len(), 1);
        assert_eq!(diff.adr_diffs[0].adr_id, "adr-old");
        assert_eq!(diff.adr_diffs[0].change, AdrChange::Removed);
        assert!(diff.adr_diffs[0].old_content.is_some());
        assert!(diff.adr_diffs[0].new_content.is_none());
    }

    // ── format_adr_diffs tests ──

    #[test]
    fn test_format_adr_diffs_added() {
        let diffs = vec![AdrDiff {
            adr_id: "abcdef12-3456-7890".to_string(),
            change: AdrChange::Added,
            old_content: None,
            new_content: Some("New rule content".to_string()),
        }];
        let lines = format_adr_diffs(&diffs);
        let joined = lines.join("\n");
        let plain = console::strip_ansi_codes(&joined);
        assert!(
            plain.contains("+ [abcdef12] added"),
            "expected '+ [abcdef12] added' in: {plain}"
        );
    }

    #[test]
    fn test_format_adr_diffs_updated() {
        let diffs = vec![AdrDiff {
            adr_id: "abcdef12-3456-7890".to_string(),
            change: AdrChange::Updated,
            old_content: Some("Old text".to_string()),
            new_content: Some("New text".to_string()),
        }];
        let lines = format_adr_diffs(&diffs);
        let joined = lines.join("\n");
        let plain = console::strip_ansi_codes(&joined);
        assert!(
            plain.contains("~ [abcdef12] updated"),
            "expected '~ [abcdef12] updated' in: {plain}"
        );
        // Should show content diff
        assert!(
            plain.contains("- Old text") || plain.contains("+ New text"),
            "expected content diff in: {plain}"
        );
    }

    #[test]
    fn test_format_adr_diffs_removed() {
        let diffs = vec![AdrDiff {
            adr_id: "deadbeef-1234".to_string(),
            change: AdrChange::Removed,
            old_content: Some("Removed content".to_string()),
            new_content: None,
        }];
        let lines = format_adr_diffs(&diffs);
        let joined = lines.join("\n");
        let plain = console::strip_ansi_codes(&joined);
        assert!(
            plain.contains("- [deadbeef] removed"),
            "expected '- [deadbeef] removed' in: {plain}"
        );
    }

    #[test]
    fn test_format_adr_diffs_unchanged_hidden() {
        let diffs = vec![AdrDiff {
            adr_id: "unchanged-id".to_string(),
            change: AdrChange::Unchanged,
            old_content: Some("Same".to_string()),
            new_content: Some("Same".to_string()),
        }];
        let lines = format_adr_diffs(&diffs);
        assert!(
            lines.is_empty(),
            "unchanged ADR should produce no output, got: {:?}",
            lines
        );
    }

    #[test]
    fn test_format_file_diff_uses_adr_diffs_when_present() {
        let diff = FileDiff {
            path: "CLAUDE.md".to_string(),
            change: FileChange::Update {
                added: 1,
                updated: 0,
                removed: 0,
            },
            old_content: Some("old content\n".to_string()),
            new_content: "new content\n".to_string(),
            adr_diffs: vec![AdrDiff {
                adr_id: "adr-new-1".to_string(),
                change: AdrChange::Added,
                old_content: None,
                new_content: Some("new content".to_string()),
            }],
        };
        let output = format_file_diff(&diff);
        let plain = console::strip_ansi_codes(&output);
        // Should use per-ADR display
        assert!(
            plain.contains("[adr-new-"),
            "expected per-ADR display in: {plain}"
        );
        assert!(
            plain.contains("added"),
            "expected 'added' in per-ADR display: {plain}"
        );
    }

    #[test]
    fn test_format_file_diff_falls_back_without_adr_diffs() {
        let diff = FileDiff {
            path: "CLAUDE.md".to_string(),
            change: FileChange::Update {
                added: 1,
                updated: 0,
                removed: 1,
            },
            old_content: Some("old line\n".to_string()),
            new_content: "new line\n".to_string(),
            adr_diffs: vec![],
        };
        let output = format_file_diff(&diff);
        let plain = console::strip_ansi_codes(&output);
        // Should fall back to whole-file diff
        assert!(
            plain.contains("- old line"),
            "expected whole-file deletion in: {plain}"
        );
        assert!(
            plain.contains("+ new line"),
            "expected whole-file addition in: {plain}"
        );
    }

    // ── coverage: Added ADR with >3 lines triggers truncation ──

    #[test]
    fn test_format_adr_diffs_added_truncates_long_content() {
        let diffs = vec![AdrDiff {
            adr_id: "abcdef12-3456-7890".to_string(),
            change: AdrChange::Added,
            old_content: None,
            new_content: Some("line1\nline2\nline3\nline4\nline5".to_string()),
        }];
        let lines = format_adr_diffs(&diffs);
        let joined = lines.join("\n");
        let plain = console::strip_ansi_codes(&joined);
        assert!(
            plain.contains("... and 2 more lines"),
            "expected truncation message in: {plain}"
        );
    }

    // ── coverage: Updated ADR with >5 diff lines triggers truncation ──

    #[test]
    fn test_format_adr_diffs_updated_truncates_long_diff() {
        // Build old/new with enough differing lines to produce >5 diff lines.
        let old = "a\nb\nc\nd\ne\nf\ng\nh";
        let new = "1\n2\n3\n4\n5\n6\n7\n8";
        let diffs = vec![AdrDiff {
            adr_id: "abcdef12-3456-7890".to_string(),
            change: AdrChange::Updated,
            old_content: Some(old.to_string()),
            new_content: Some(new.to_string()),
        }];
        let lines = format_adr_diffs(&diffs);
        let joined = lines.join("\n");
        let plain = console::strip_ansi_codes(&joined);
        assert!(
            plain.contains("... and ") && plain.contains(" more changes"),
            "expected truncation message in: {plain}"
        );
    }

    // ── coverage: ADR ID with ≤8 chars uses full ID (else branch) ──

    #[test]
    fn test_format_adr_diffs_short_adr_id() {
        let diffs = vec![AdrDiff {
            adr_id: "short".to_string(),
            change: AdrChange::Added,
            old_content: None,
            new_content: Some("content".to_string()),
        }];
        let lines = format_adr_diffs(&diffs);
        let joined = lines.join("\n");
        let plain = console::strip_ansi_codes(&joined);
        assert!(
            plain.contains("[short]"),
            "expected full short ID '[short]' in: {plain}"
        );
    }

    // ── coverage: format_file_diff with NewFile + adr_diffs ──

    #[test]
    fn test_format_file_diff_new_file_with_adr_diffs() {
        let diff = FileDiff {
            path: "AGENTS.md".to_string(),
            change: FileChange::NewFile { rule_count: 2 },
            old_content: None,
            new_content: "new content\n".to_string(),
            adr_diffs: vec![AdrDiff {
                adr_id: "newfile-adr-id-1234".to_string(),
                change: AdrChange::Added,
                old_content: None,
                new_content: Some("new rule".to_string()),
            }],
        };
        let output = format_file_diff(&diff);
        let plain = console::strip_ansi_codes(&output);
        assert!(
            plain.contains("(new file)"),
            "expected '(new file)' header in: {plain}"
        );
        assert!(
            plain.contains("[newfile-") && plain.contains("added"),
            "expected per-ADR display in NewFile: {plain}"
        );
    }

    // ── coverage: Added ADR with new_content = None (line 283) ──

    #[test]
    fn test_format_adr_diffs_added_no_content() {
        let diffs = vec![AdrDiff {
            adr_id: "abcdef12-3456-7890".to_string(),
            change: AdrChange::Added,
            old_content: None,
            new_content: None,
        }];
        let lines = format_adr_diffs(&diffs);
        let joined = lines.join("\n");
        let plain = console::strip_ansi_codes(&joined);
        assert!(
            plain.contains("+ [abcdef12] added"),
            "expected '+ [abcdef12] added' in: {plain}"
        );
        // No content lines should follow
        assert_eq!(
            lines.len(),
            1,
            "expected only the header line, got: {lines:?}"
        );
    }

    // ── coverage: Updated ADR with identical old/new content (line 299) ──

    #[test]
    fn test_format_adr_diffs_updated_identical_content() {
        let diffs = vec![AdrDiff {
            adr_id: "abcdef12-3456-7890".to_string(),
            change: AdrChange::Updated,
            old_content: Some("same text".to_string()),
            new_content: Some("same text".to_string()),
        }];
        let lines = format_adr_diffs(&diffs);
        let joined = lines.join("\n");
        let plain = console::strip_ansi_codes(&joined);
        assert!(
            plain.contains("~ [abcdef12] updated"),
            "expected '~ [abcdef12] updated' in: {plain}"
        );
        // No diff lines should follow since content is identical
        assert_eq!(
            lines.len(),
            1,
            "expected only the header line, got: {lines:?}"
        );
    }

    // ── coverage: Updated ADR with missing old/new content (line 300) ──

    #[test]
    fn test_format_adr_diffs_updated_missing_content() {
        let diffs = vec![AdrDiff {
            adr_id: "abcdef12-3456-7890".to_string(),
            change: AdrChange::Updated,
            old_content: None,
            new_content: Some("new text".to_string()),
        }];
        let lines = format_adr_diffs(&diffs);
        let joined = lines.join("\n");
        let plain = console::strip_ansi_codes(&joined);
        assert!(
            plain.contains("~ [abcdef12] updated"),
            "expected '~ [abcdef12] updated' in: {plain}"
        );
        // No diff lines since old_content is None
        assert_eq!(
            lines.len(),
            1,
            "expected only the header line, got: {lines:?}"
        );
    }

    // ── from_change_detection backward compat test ──

    #[test]
    fn from_change_detection_sets_empty_adr_diffs() {
        let detection = ChangeDetection {
            new_ids: vec!["a".into()],
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
        assert!(
            diff.adr_diffs.is_empty(),
            "from_change_detection should set empty adr_diffs"
        );
    }
}
