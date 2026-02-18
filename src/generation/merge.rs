use std::collections::BTreeMap;
use std::collections::HashSet;

use super::markers::{self, END_MARKER};
use crate::tailoring::types::{FileOutput, SkippedAdr, TailoringOutput, TailoringSummary};

/// Merge multiple [`TailoringOutput`]s into a single combined output.
///
/// - Overlapping file paths (same `path`): content is concatenated with `"\n\n"`,
///   `adr_ids` are merged (deduplicated, preserving first-seen order), and
///   `reasoning` strings are combined with `"; "`.
/// - Different file paths: both are included in the result.
/// - `skipped_adrs` are deduplicated by `id` (first occurrence wins).
/// - `summary` is recomputed from deduplicated data: `applicable` counts unique ADR IDs
///   across all merged files, `not_applicable` counts deduplicated skipped ADRs,
///   `total_input` is `applicable + not_applicable`, and `files_generated` is the
///   number of unique file paths in the merged result.
pub fn merge_outputs(outputs: Vec<TailoringOutput>) -> TailoringOutput {
    // Merge files, preserving insertion order by path.
    let mut file_map: BTreeMap<String, FileOutput> = BTreeMap::new();

    for output in &outputs {
        for file in &output.files {
            file_map
                .entry(file.path.clone())
                .and_modify(|existing| {
                    // Concatenate content
                    existing.content.push_str("\n\n");
                    existing.content.push_str(&file.content);

                    // Merge adr_ids (deduplicated, preserving order)
                    let existing_ids: HashSet<String> = existing.adr_ids.iter().cloned().collect();
                    for id in &file.adr_ids {
                        if !existing_ids.contains(id) {
                            existing.adr_ids.push(id.clone());
                        }
                    }

                    // Combine reasoning
                    existing.reasoning.push_str("; ");
                    existing.reasoning.push_str(&file.reasoning);
                })
                .or_insert_with(|| file.clone());
        }
    }

    // Deduplicate skipped_adrs by id (first occurrence wins).
    let mut seen_skipped: HashSet<String> = HashSet::new();
    let mut skipped_adrs: Vec<SkippedAdr> = Vec::new();
    for output in &outputs {
        for skipped in &output.skipped_adrs {
            if seen_skipped.insert(skipped.id.clone()) {
                skipped_adrs.push(skipped.clone());
            }
        }
    }

    // Recompute summary from deduplicated data.
    let files: Vec<FileOutput> = file_map.into_values().collect();
    let applicable: usize = {
        let mut unique_adr_ids: HashSet<&str> = HashSet::new();
        for file in &files {
            for id in &file.adr_ids {
                unique_adr_ids.insert(id.as_str());
            }
        }
        unique_adr_ids.len()
    };
    let not_applicable = skipped_adrs.len(); // Already deduplicated (lines 46-55)
    let total_input = applicable + not_applicable;
    let files_generated = files.len();

    TailoringOutput {
        files,
        skipped_adrs,
        summary: TailoringSummary {
            total_input,
            applicable,
            not_applicable,
            files_generated,
        },
    }
}

/// The result of merging managed content into a file.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeResult {
    pub content: String,
}

/// Merge new managed content into an existing (or new) output file.
///
/// # Arguments
/// * `existing_content` - The current file content, or `None` for a new file.
/// * `managed_content` - The new AI-generated content to place inside markers.
/// * `version` - The version number for the managed section header.
/// * `is_root` - Whether this is the root-level output file (adds a format-specific
///   header for new files).
/// * `adr_ids` - ADR IDs to include in the managed section metadata.
/// * `root_header` - Optional header string to prepend for new root files (e.g.
///   `"# Project Guidelines"` for ClaudeMd/AgentsMd, or the YAML frontmatter block
///   for CursorRules).  Pass `None` to suppress the header.
///
/// # Behavior
/// 1. New root file (`existing_content` is `None`, `is_root` is `true`):
///    Output starts with `root_header` (if `Some`), followed by managed section.
/// 2. New subdirectory file (`existing_content` is `None`, `is_root` is `false`):
///    Output is just the managed section.
/// 3. Existing file with markers: content between markers is replaced (from start of
///    START_MARKER line to end of END_MARKER line), content outside preserved exactly.
/// 4. Existing file without markers: managed section is appended after existing content
///    (separated by `"\n\n"`).
pub fn merge_content(
    existing_content: Option<&str>,
    managed_content: &str,
    version: u32,
    is_root: bool,
    adr_ids: &[String],
    root_header: Option<&str>,
) -> MergeResult {
    let managed_section = markers::wrap_in_markers(managed_content, version, adr_ids);

    let content = match existing_content {
        None => {
            // New file
            if is_root {
                if let Some(header) = root_header {
                    format!("{header}\n\n{managed_section}\n")
                } else {
                    format!("{managed_section}\n")
                }
            } else {
                format!("{managed_section}\n")
            }
        }
        Some(existing) => {
            if let Some((start_idx, end_idx)) = markers::find_managed_section_bounds(existing) {
                // Replace content between markers (inclusive of marker lines)
                let end_idx = end_idx + END_MARKER.len();
                // Also consume the trailing newline after END_MARKER if present
                let end_idx = if existing[end_idx..].starts_with('\n') {
                    end_idx + 1
                } else {
                    end_idx
                };
                let before = &existing[..start_idx];
                let after = &existing[end_idx..];
                format!("{before}{managed_section}\n{after}")
            } else {
                // Append managed section after existing content
                if existing.ends_with('\n') {
                    format!("{existing}\n{managed_section}\n")
                } else {
                    format!("{existing}\n\n{managed_section}\n")
                }
            }
        }
    };

    MergeResult { content }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::markers::START_MARKER;

    fn make_output(files: Vec<FileOutput>, skipped: Vec<SkippedAdr>) -> TailoringOutput {
        let applicable = files.iter().map(|f| f.adr_ids.len()).sum::<usize>();
        let not_applicable = skipped.len();
        let total_input = applicable + not_applicable;
        TailoringOutput {
            summary: TailoringSummary {
                total_input,
                applicable,
                not_applicable,
                files_generated: files.len(),
            },
            files,
            skipped_adrs: skipped,
        }
    }

    #[test]
    fn test_merge_overlapping_paths() {
        let out1 = make_output(
            vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                content: "# Batch 1 rules".to_string(),
                reasoning: "First batch".to_string(),
                adr_ids: vec!["adr-001".to_string(), "adr-002".to_string()],
            }],
            vec![],
        );

        let out2 = make_output(
            vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                content: "# Batch 2 rules".to_string(),
                reasoning: "Second batch".to_string(),
                adr_ids: vec!["adr-002".to_string(), "adr-003".to_string()],
            }],
            vec![],
        );

        let merged = merge_outputs(vec![out1, out2]);

        // Should have a single CLAUDE.md
        assert_eq!(merged.files.len(), 1);
        let file = &merged.files[0];
        assert_eq!(file.path, "CLAUDE.md");

        // Content concatenated with "\n\n"
        assert_eq!(file.content, "# Batch 1 rules\n\n# Batch 2 rules");

        // adr_ids deduplicated, preserving order
        assert_eq!(file.adr_ids, vec!["adr-001", "adr-002", "adr-003"]);

        // Reasoning combined with "; "
        assert_eq!(file.reasoning, "First batch; Second batch");

        // Summary recomputed from deduplicated ADR IDs
        assert_eq!(merged.summary.files_generated, 1);
        assert_eq!(merged.summary.total_input, 3); // 3 unique ADRs + 0 skipped
        assert_eq!(merged.summary.applicable, 3); // adr-001, adr-002, adr-003
        assert_eq!(merged.summary.not_applicable, 0);
    }

    #[test]
    fn test_merge_different_paths() {
        let out1 = make_output(
            vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                content: "root rules".to_string(),
                reasoning: "root".to_string(),
                adr_ids: vec!["adr-001".to_string()],
            }],
            vec![],
        );

        let out2 = make_output(
            vec![FileOutput {
                path: "apps/web/CLAUDE.md".to_string(),
                content: "web rules".to_string(),
                reasoning: "web".to_string(),
                adr_ids: vec!["adr-002".to_string()],
            }],
            vec![],
        );

        let merged = merge_outputs(vec![out1, out2]);

        assert_eq!(merged.files.len(), 2);

        let paths: Vec<&str> = merged.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"CLAUDE.md"), "missing CLAUDE.md");
        assert!(
            paths.contains(&"apps/web/CLAUDE.md"),
            "missing apps/web/CLAUDE.md"
        );

        assert_eq!(merged.summary.files_generated, 2);
        assert_eq!(merged.summary.total_input, 2);
        assert_eq!(merged.summary.applicable, 2);
    }

    #[test]
    fn test_merge_outputs_deduplicates_summary_counts() {
        // Two outputs that share the same ADR
        let shared_adr = "adr-shared";
        let out1 = make_output(
            vec![FileOutput {
                path: "project-a/CLAUDE.md".to_string(),
                content: "content A".to_string(),
                reasoning: "reason A".to_string(),
                adr_ids: vec!["adr-a1".to_string(), shared_adr.to_string()],
            }],
            vec![SkippedAdr {
                id: "adr-skip1".to_string(),
                reason: "not applicable".to_string(),
            }],
        );
        let out2 = make_output(
            vec![FileOutput {
                path: "project-b/CLAUDE.md".to_string(),
                content: "content B".to_string(),
                reasoning: "reason B".to_string(),
                adr_ids: vec!["adr-b1".to_string(), shared_adr.to_string()],
            }],
            vec![SkippedAdr {
                id: "adr-skip1".to_string(), // Same skipped ADR
                reason: "not applicable".to_string(),
            }],
        );

        let merged = merge_outputs(vec![out1, out2]);

        // Unique applicable ADRs: adr-a1, adr-shared, adr-b1 = 3
        assert_eq!(merged.summary.applicable, 3);
        // Unique skipped ADRs: adr-skip1 = 1
        assert_eq!(merged.summary.not_applicable, 1);
        // Total: 3 + 1 = 4
        assert_eq!(merged.summary.total_input, 4);
        // Two different paths = 2 files
        assert_eq!(merged.summary.files_generated, 2);
    }

    #[test]
    fn test_merge_deduplicates_skipped_adrs() {
        let out1 = make_output(
            vec![],
            vec![SkippedAdr {
                id: "skip-1".to_string(),
                reason: "Not applicable".to_string(),
            }],
        );

        let out2 = make_output(
            vec![],
            vec![
                SkippedAdr {
                    id: "skip-1".to_string(),
                    reason: "Different reason".to_string(),
                },
                SkippedAdr {
                    id: "skip-2".to_string(),
                    reason: "Also not applicable".to_string(),
                },
            ],
        );

        let merged = merge_outputs(vec![out1, out2]);

        assert_eq!(merged.skipped_adrs.len(), 2);
        assert_eq!(merged.skipped_adrs[0].id, "skip-1");
        assert_eq!(merged.skipped_adrs[0].reason, "Not applicable"); // first occurrence wins
        assert_eq!(merged.skipped_adrs[1].id, "skip-2");
    }

    #[test]
    fn test_merge_empty_inputs() {
        let merged = merge_outputs(vec![]);

        assert!(merged.files.is_empty());
        assert!(merged.skipped_adrs.is_empty());
        assert_eq!(merged.summary.total_input, 0);
        assert_eq!(merged.summary.applicable, 0);
        assert_eq!(merged.summary.not_applicable, 0);
        assert_eq!(merged.summary.files_generated, 0);
    }

    // ---- merge_content tests ----

    const PROJECT_GUIDELINES_HEADER: &str = "# Project Guidelines";
    const CURSOR_FRONTMATTER: &str =
        "---\ndescription: actual.ai ADR policies for this project\nalwaysApply: true\n---";

    #[test]
    fn test_new_root_file_has_header_and_managed_section() {
        let result = merge_content(
            None,
            "some content",
            1,
            true,
            &[],
            Some(PROJECT_GUIDELINES_HEADER),
        );
        assert!(
            result.content.starts_with("# Project Guidelines\n\n"),
            "expected root header, got: {}",
            result.content
        );
        assert!(
            markers::has_managed_section(&result.content),
            "expected managed section in output"
        );
        assert!(
            result.content.contains("some content"),
            "expected managed content in output"
        );
    }

    #[test]
    fn test_new_subdirectory_file_is_managed_section_only() {
        let result = merge_content(None, "some content", 1, false, &[], Some("# Header"));
        assert!(
            result.content.starts_with(START_MARKER),
            "expected output to start with START_MARKER, got: {}",
            result.content
        );
        assert!(
            !result.content.contains("# Project Guidelines"),
            "subdirectory file should NOT have Project Guidelines header"
        );
        assert!(
            markers::has_managed_section(&result.content),
            "expected managed section in output"
        );
        assert!(
            result.content.contains("some content"),
            "expected managed content in output"
        );
    }

    #[test]
    fn test_new_root_no_header_when_none() {
        let result = merge_content(None, "some content", 1, true, &[], None);
        assert!(
            result.content.starts_with(START_MARKER),
            "expected output to start with START_MARKER when root_header is None, got: {}",
            result.content
        );
        assert!(
            markers::has_managed_section(&result.content),
            "expected managed section in output"
        );
    }

    #[test]
    fn test_existing_with_markers_replaces_content() {
        let existing = format!("{}\n", markers::wrap_in_markers("old content", 1, &[]));
        let result = merge_content(Some(&existing), "new content", 2, false, &[], None);
        assert!(
            result.content.contains("new content"),
            "expected new content in output: {}",
            result.content
        );
        assert!(
            !result.content.contains("old content"),
            "expected old content to be replaced: {}",
            result.content
        );
        assert!(
            markers::has_managed_section(&result.content),
            "expected managed section in output"
        );
    }

    #[test]
    fn test_existing_without_markers_appends() {
        let existing = "# My Custom Rules\n\nDo stuff";
        let result = merge_content(Some(existing), "managed stuff", 1, false, &[], None);
        assert!(
            result.content.starts_with("# My Custom Rules\n\nDo stuff"),
            "expected existing content preserved at start: {}",
            result.content
        );
        assert!(
            markers::has_managed_section(&result.content),
            "expected managed section appended"
        );
        assert!(
            result.content.contains("managed stuff"),
            "expected managed content in output"
        );
    }

    #[test]
    fn test_existing_preserves_content_above_and_below_markers() {
        let managed = markers::wrap_in_markers("old managed", 1, &[]);
        let existing = format!("# User Header\n\n{managed}\n\n## User Footer\n");
        let result = merge_content(Some(&existing), "new managed", 2, false, &[], None);
        assert!(
            result.content.contains("# User Header"),
            "expected user header preserved: {}",
            result.content
        );
        assert!(
            result.content.contains("## User Footer"),
            "expected user footer preserved: {}",
            result.content
        );
        assert!(
            result.content.contains("new managed"),
            "expected new managed content: {}",
            result.content
        );
        assert!(
            !result.content.contains("old managed"),
            "expected old managed content replaced: {}",
            result.content
        );
    }

    #[test]
    fn test_empty_managed_content() {
        let result = merge_content(None, "", 1, false, &[], None);
        assert!(
            markers::has_managed_section(&result.content),
            "expected valid managed section even with empty content"
        );
        assert!(
            result.content.contains(START_MARKER),
            "expected START_MARKER"
        );
        assert!(result.content.contains(END_MARKER), "expected END_MARKER");
    }

    #[test]
    fn test_existing_markers_only_replaces() {
        let existing = format!("{}\n", markers::wrap_in_markers("original", 1, &[]));
        let result = merge_content(Some(&existing), "replacement", 2, false, &[], None);
        assert!(
            result.content.contains("replacement"),
            "expected replacement content: {}",
            result.content
        );
        assert!(
            !result.content.contains("original"),
            "expected original content removed: {}",
            result.content
        );
        assert!(
            markers::has_managed_section(&result.content),
            "expected valid managed section"
        );
    }

    #[test]
    fn test_existing_without_markers_appends_when_trailing_newline() {
        let existing = "# My Custom Rules\n\nDo stuff\n";
        let result = merge_content(Some(existing), "managed stuff", 1, false, &[], None);
        assert!(
            result
                .content
                .starts_with("# My Custom Rules\n\nDo stuff\n"),
            "expected existing content preserved at start: {}",
            result.content
        );
        assert!(
            markers::has_managed_section(&result.content),
            "expected managed section appended"
        );
    }

    #[test]
    fn test_existing_with_markers_no_trailing_newline() {
        // END_MARKER is not followed by \n (no trailing newline)
        let managed = markers::wrap_in_markers("old content", 1, &[]);
        let existing = managed.clone(); // no trailing \n
        let result = merge_content(Some(&existing), "new content", 2, false, &[], None);
        assert!(
            result.content.contains("new content"),
            "expected new content: {}",
            result.content
        );
        assert!(
            !result.content.contains("old content"),
            "expected old content replaced: {}",
            result.content
        );
        assert!(
            markers::has_managed_section(&result.content),
            "expected managed section in output"
        );
    }

    #[test]
    fn test_new_root_file_roundtrip() {
        let original = "line one\nline two\nline three";
        let result = merge_content(
            None,
            original,
            1,
            true,
            &[],
            Some(PROJECT_GUIDELINES_HEADER),
        );
        let extracted =
            markers::extract_managed_content(&result.content).expect("should extract content");
        assert!(
            extracted.contains(original),
            "expected original content in extracted: {:?}",
            extracted
        );
    }

    #[test]
    fn test_existing_with_reversed_markers_appends_instead_of_replacing() {
        // When END_MARKER appears before START_MARKER, find_managed_section_bounds returns None,
        // so merge_content must fall through to the append path rather than replacing.
        // We verify by checking the bounds helper and confirming the new content is appended.
        let existing = format!("{END_MARKER}\nsome content\n{START_MARKER}\n");

        // find_managed_section_bounds must return None for reversed markers.
        assert!(
            markers::find_managed_section_bounds(&existing).is_none(),
            "expected None for reversed markers"
        );

        let result = merge_content(Some(&existing), "new managed", 1, false, &[], None);

        // The original reversed-marker content must be preserved verbatim at the start.
        assert!(
            result.content.starts_with(&existing),
            "expected existing content preserved at start: {}",
            result.content
        );

        // The new managed content must be present.
        assert!(
            result.content.contains("new managed"),
            "expected new managed content in output: {}",
            result.content
        );

        // The new managed content must appear AFTER the existing block (append, not replace).
        let existing_end = existing.len();
        assert!(
            result.content[existing_end..].contains("new managed"),
            "expected new managed content appended after existing: {}",
            result.content
        );
    }

    // ---- CursorRules-specific merge_content tests ----

    #[test]
    fn test_new_cursor_rules_root_file_has_frontmatter() {
        let result = merge_content(
            None,
            "Use Tailwind for all styling.",
            1,
            true,
            &[],
            Some(CURSOR_FRONTMATTER),
        );
        assert!(
            result.content.starts_with("---\n"),
            "cursor-rules root file must start with YAML front-matter delimiter, got: {}",
            result.content
        );
        assert!(
            result.content.contains("alwaysApply: true"),
            "cursor-rules root file must contain alwaysApply: true"
        );
        assert!(
            markers::has_managed_section(&result.content),
            "expected managed section in output"
        );
        assert!(
            result.content.contains("Use Tailwind for all styling."),
            "expected managed content"
        );
    }

    #[test]
    fn test_cursor_rules_frontmatter_separator() {
        // The frontmatter should be separated from the managed section by \n\n
        let result = merge_content(None, "content", 1, true, &[], Some(CURSOR_FRONTMATTER));
        let frontmatter_end = CURSOR_FRONTMATTER.len();
        let after_frontmatter = &result.content[frontmatter_end..];
        assert!(
            after_frontmatter.starts_with("\n\n"),
            "expected \\n\\n between frontmatter and managed section, got: {:?}",
            &after_frontmatter[..after_frontmatter.len().min(20)]
        );
    }
}
