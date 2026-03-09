use std::collections::BTreeMap;
use std::collections::HashSet;

use super::markers::{self, END_MARKER};
use crate::tailoring::types::{
    AdrSection, FileOutput, SkippedAdr, TailoringOutput, TailoringSummary,
};

/// Merge multiple [`TailoringOutput`]s into a single combined output.
///
/// - Overlapping file paths (same `path`): sections are merged by `adr_id`. When the
///   same `adr_id` appears in multiple outputs for the same path, their content is
///   concatenated with `"\n\n"`. New `adr_id`s are appended in encounter order.
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
                    // Merge sections: same adr_id → concatenate content; new adr_id → append.
                    for section in &file.sections {
                        if let Some(pos) = existing
                            .sections
                            .iter()
                            .position(|s| s.adr_id == section.adr_id)
                        {
                            // Same adr_id: concatenate content from both outputs
                            existing.sections[pos].content.push_str("\n\n");
                            existing.sections[pos].content.push_str(&section.content);
                        } else {
                            existing.sections.push(section.clone());
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
            for section in &file.sections {
                unique_adr_ids.insert(section.adr_id.as_str());
            }
        }
        unique_adr_ids.len()
    };
    let not_applicable = skipped_adrs.len(); // Already deduplicated
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

/// Merge per-ADR sections from a managed block with new sections.
///
/// Performs surgical section-level merge:
/// - Existing sections that are also in `new_sections`: replaced (in existing order)
/// - Existing sections NOT in `new_sections`: dropped (removed)
/// - New sections NOT in existing: appended at the end
fn merge_adr_sections(existing_managed: &str, new_sections: &[AdrSection]) -> String {
    let existing_sections = markers::extract_adr_sections(existing_managed);
    let existing_ids: HashSet<&str> = existing_sections.iter().map(|s| s.id.as_str()).collect();

    let mut result: Vec<String> = Vec::new();

    // Keep existing order for sections being updated
    for existing in &existing_sections {
        if let Some(new) = new_sections.iter().find(|s| s.adr_id == existing.id) {
            result.push(markers::wrap_adr_section(&new.adr_id, &new.content));
        }
        // else: section removed, skip
    }

    // Append new sections not in existing
    for new in new_sections {
        if !existing_ids.contains(new.adr_id.as_str()) {
            result.push(markers::wrap_adr_section(&new.adr_id, &new.content));
        }
    }

    result.join("\n\n")
}

/// Merge new managed content into an existing (or new) output file.
///
/// # Arguments
/// * `existing_content` - The current file content, or `None` for a new file.
/// * `sections` - Per-ADR sections to place inside the managed block. Each section
///   is wrapped with `<!-- adr:UUID start/end -->` markers.
/// * `version` - The version number for the managed section header.
/// * `is_root` - Whether this is the root-level output file (adds a format-specific
///   header for new files).
/// * `root_header` - Optional header string to prepend for new root files (e.g.
///   `"# Project Guidelines"` for ClaudeMd/AgentsMd, or the YAML frontmatter block
///   for CursorRules).  Pass `None` to suppress the header.
///
/// # Behavior
/// 1. New root file (`existing_content` is `None`, `is_root` is `true`):
///    Output starts with `root_header` (if `Some`), followed by managed section.
/// 2. New subdirectory file (`existing_content` is `None`, `is_root` is `false`):
///    Output is just the managed section.
/// 3. Existing file with markers AND per-ADR section markers inside: surgical
///    section-level merge (preserve order, replace updated, add new, remove deleted).
/// 4. Existing file with markers but NO per-ADR section markers (old format):
///    whole managed block is replaced (one-time migration to per-ADR format).
/// 5. Existing file without markers: managed section is appended after existing content
///    (separated by `"\n\n"`).
pub fn merge_content(
    existing_content: Option<&str>,
    sections: &[AdrSection],
    version: u32,
    is_root: bool,
    root_header: Option<&str>,
) -> MergeResult {
    // Build managed content from sections with per-ADR markers
    let managed_content = sections
        .iter()
        .map(|s| markers::wrap_adr_section(&s.adr_id, &s.content))
        .collect::<Vec<_>>()
        .join("\n\n");
    let adr_ids: Vec<String> = sections.iter().map(|s| s.adr_id.clone()).collect();

    let content = match existing_content {
        None => {
            // New file
            let managed_section = markers::wrap_in_markers(&managed_content, version, &adr_ids);
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
                // Extract existing managed content to check for per-ADR markers
                let existing_managed =
                    markers::extract_managed_content(existing).unwrap_or_default();
                let existing_adr_sections = markers::extract_adr_sections(existing_managed);

                let final_managed = if existing_adr_sections.is_empty() {
                    // Old format: no per-ADR markers, replace whole block (migration)
                    managed_content
                } else {
                    // Surgical section-level merge
                    merge_adr_sections(existing_managed, sections)
                };

                let managed_section = markers::wrap_in_markers(&final_managed, version, &adr_ids);

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
                let managed_section = markers::wrap_in_markers(&managed_content, version, &adr_ids);
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
    use crate::tailoring::types::AdrSection;

    fn make_output(files: Vec<FileOutput>, skipped: Vec<SkippedAdr>) -> TailoringOutput {
        let applicable = files.iter().map(|f| f.sections.len()).sum::<usize>();
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
                sections: vec![
                    AdrSection {
                        adr_id: "adr-001".to_string(),
                        content: "# Batch 1 rules".to_string(),
                    },
                    AdrSection {
                        adr_id: "adr-002".to_string(),
                        content: "# Batch 1 rules".to_string(),
                    },
                ],
                reasoning: "First batch".to_string(),
            }],
            vec![],
        );

        let out2 = make_output(
            vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                sections: vec![
                    AdrSection {
                        adr_id: "adr-002".to_string(),
                        content: "# Batch 2 rules".to_string(),
                    },
                    AdrSection {
                        adr_id: "adr-003".to_string(),
                        content: "# Batch 2 rules".to_string(),
                    },
                ],
                reasoning: "Second batch".to_string(),
            }],
            vec![],
        );

        let merged = merge_outputs(vec![out1, out2]);

        // Should have a single CLAUDE.md
        assert_eq!(merged.files.len(), 1);
        let file = &merged.files[0];
        assert_eq!(file.path, "CLAUDE.md");

        // adr_ids deduplicated, preserving insertion order
        assert_eq!(file.adr_ids(), vec!["adr-001", "adr-002", "adr-003"]);

        // Same adr_id from two outputs → content concatenated with "\n\n"
        let adr002 = file
            .sections
            .iter()
            .find(|s| s.adr_id == "adr-002")
            .unwrap();
        assert_eq!(adr002.content, "# Batch 1 rules\n\n# Batch 2 rules");

        // Reasoning combined with "; "
        assert_eq!(file.reasoning, "First batch; Second batch");

        // Summary recomputed from deduplicated ADR IDs
        assert_eq!(merged.summary.files_generated, 1);
        assert_eq!(merged.summary.total_input, 3); // 3 unique ADRs + 0 skipped
        assert_eq!(merged.summary.applicable, 3); // adr-001, adr-002, adr-003
        assert_eq!(merged.summary.not_applicable, 0);
    }

    #[test]
    fn test_merge_outputs_duplicate_adr_id_content_concatenated() {
        // When the same adr_id appears in multiple outputs for the same file path,
        // the content from both outputs should be concatenated with "\n\n".
        let out1 = make_output(
            vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                sections: vec![AdrSection {
                    adr_id: "adr-dup".to_string(),
                    content: "First content".to_string(),
                }],
                reasoning: "first".to_string(),
            }],
            vec![],
        );
        let out2 = make_output(
            vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                sections: vec![AdrSection {
                    adr_id: "adr-dup".to_string(),
                    content: "Second content".to_string(),
                }],
                reasoning: "second".to_string(),
            }],
            vec![],
        );

        let merged = merge_outputs(vec![out1, out2]);

        assert_eq!(merged.files.len(), 1);
        let file = &merged.files[0];
        // Only one section with the shared adr_id
        assert_eq!(file.sections.len(), 1);
        assert_eq!(file.sections[0].adr_id, "adr-dup");
        // Content from both outputs concatenated with "\n\n"
        assert_eq!(
            file.sections[0].content, "First content\n\nSecond content",
            "expected concatenated content: {}",
            file.sections[0].content
        );
    }

    #[test]
    fn test_merge_different_paths() {
        let out1 = make_output(
            vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                sections: vec![AdrSection {
                    adr_id: "adr-001".to_string(),
                    content: "root rules".to_string(),
                }],
                reasoning: "root".to_string(),
            }],
            vec![],
        );

        let out2 = make_output(
            vec![FileOutput {
                path: "apps/web/CLAUDE.md".to_string(),
                sections: vec![AdrSection {
                    adr_id: "adr-002".to_string(),
                    content: "web rules".to_string(),
                }],
                reasoning: "web".to_string(),
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
                sections: vec![
                    AdrSection {
                        adr_id: "adr-a1".to_string(),
                        content: "content A".to_string(),
                    },
                    AdrSection {
                        adr_id: shared_adr.to_string(),
                        content: "content A".to_string(),
                    },
                ],
                reasoning: "reason A".to_string(),
            }],
            vec![SkippedAdr {
                id: "adr-skip1".to_string(),
                reason: "not applicable".to_string(),
            }],
        );
        let out2 = make_output(
            vec![FileOutput {
                path: "project-b/CLAUDE.md".to_string(),
                sections: vec![
                    AdrSection {
                        adr_id: "adr-b1".to_string(),
                        content: "content B".to_string(),
                    },
                    AdrSection {
                        adr_id: shared_adr.to_string(),
                        content: "content B".to_string(),
                    },
                ],
                reasoning: "reason B".to_string(),
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

    fn make_section(adr_id: &str, content: &str) -> AdrSection {
        AdrSection {
            adr_id: adr_id.to_string(),
            content: content.to_string(),
        }
    }

    fn make_sections(pairs: &[(&str, &str)]) -> Vec<AdrSection> {
        pairs.iter().map(|(id, c)| make_section(id, c)).collect()
    }

    #[test]
    fn test_new_root_file_has_header_and_managed_section() {
        let sections = make_sections(&[("adr-1", "some content")]);
        let result = merge_content(None, &sections, 1, true, Some(PROJECT_GUIDELINES_HEADER));
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
        let sections = make_sections(&[("adr-1", "some content")]);
        let result = merge_content(None, &sections, 1, false, Some("# Header"));
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
        let sections = make_sections(&[("adr-1", "some content")]);
        let result = merge_content(None, &sections, 1, true, None);
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
        // Old format: no per-ADR markers inside existing managed block
        let existing = format!("{}\n", markers::wrap_in_markers("old content", 1, &[]));
        let sections = make_sections(&[("adr-1", "new content")]);
        let result = merge_content(Some(&existing), &sections, 2, false, None);
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
        let sections = make_sections(&[("adr-1", "managed stuff")]);
        let result = merge_content(Some(existing), &sections, 1, false, None);
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
        // Old format: no per-ADR markers
        let managed = markers::wrap_in_markers("old managed", 1, &[]);
        let existing = format!("# User Header\n\n{managed}\n\n## User Footer\n");
        let sections = make_sections(&[("adr-1", "new managed")]);
        let result = merge_content(Some(&existing), &sections, 2, false, None);
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
        let sections: Vec<AdrSection> = vec![];
        let result = merge_content(None, &sections, 1, false, None);
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
        // Old format: no per-ADR markers inside existing managed block
        let existing = format!("{}\n", markers::wrap_in_markers("original", 1, &[]));
        let sections = make_sections(&[("adr-1", "replacement")]);
        let result = merge_content(Some(&existing), &sections, 2, false, None);
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
        let sections = make_sections(&[("adr-1", "managed stuff")]);
        let result = merge_content(Some(existing), &sections, 1, false, None);
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
        // Old format: END_MARKER is not followed by \n (no trailing newline)
        let managed = markers::wrap_in_markers("old content", 1, &[]);
        let existing = managed.clone(); // no trailing \n
        let sections = make_sections(&[("adr-1", "new content")]);
        let result = merge_content(Some(&existing), &sections, 2, false, None);
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
        let sections = make_sections(&[("adr-1", "line one\nline two\nline three")]);
        let result = merge_content(None, &sections, 1, true, Some(PROJECT_GUIDELINES_HEADER));
        let extracted =
            markers::extract_managed_content(&result.content).expect("should extract content");
        assert!(
            extracted.contains("line one\nline two\nline three"),
            "expected original content in extracted: {:?}",
            extracted
        );
    }

    #[test]
    fn test_existing_with_reversed_markers_appends_instead_of_replacing() {
        // When END_MARKER appears before START_MARKER, find_managed_section_bounds returns None,
        // so merge_content must fall through to the append path rather than replacing.
        let existing = format!("{END_MARKER}\nsome content\n{START_MARKER}\n");

        // find_managed_section_bounds must return None for reversed markers.
        assert!(
            markers::find_managed_section_bounds(&existing).is_none(),
            "expected None for reversed markers"
        );

        let sections = make_sections(&[("adr-1", "new managed")]);
        let result = merge_content(Some(&existing), &sections, 1, false, None);

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
        let sections = make_sections(&[("adr-1", "Use Tailwind for all styling.")]);
        let result = merge_content(None, &sections, 1, true, Some(CURSOR_FRONTMATTER));
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
        let sections = make_sections(&[("adr-1", "content")]);
        let result = merge_content(None, &sections, 1, true, Some(CURSOR_FRONTMATTER));
        let frontmatter_end = CURSOR_FRONTMATTER.len();
        let after_frontmatter = &result.content[frontmatter_end..];
        assert!(after_frontmatter.starts_with("\n\n"));
    }

    // ---- Per-ADR section merge tests ----

    #[test]
    fn test_merge_content_writes_per_adr_section_markers() {
        let sections = make_sections(&[("adr-001", "Rule one"), ("adr-002", "Rule two")]);
        let result = merge_content(None, &sections, 1, false, None);
        assert!(
            result.content.contains("<!-- adr:adr-001 start -->"),
            "expected adr-001 start marker in: {}",
            result.content
        );
        assert!(
            result.content.contains("<!-- adr:adr-001 end -->"),
            "expected adr-001 end marker in: {}",
            result.content
        );
        assert!(
            result.content.contains("<!-- adr:adr-002 start -->"),
            "expected adr-002 start marker in: {}",
            result.content
        );
        assert!(
            result.content.contains("<!-- adr:adr-002 end -->"),
            "expected adr-002 end marker in: {}",
            result.content
        );
        assert!(
            result.content.contains("Rule one"),
            "expected content for adr-001"
        );
        assert!(
            result.content.contains("Rule two"),
            "expected content for adr-002"
        );
    }

    #[test]
    fn test_merge_content_resync_replaces_changed_section() {
        // Existing has A (old), B; new has A (updated), B
        let old_managed = format!(
            "{}\n\n{}",
            markers::wrap_adr_section("adr-A", "Old A content"),
            markers::wrap_adr_section("adr-B", "B content"),
        );
        let existing = format!(
            "{}\n",
            markers::wrap_in_markers(&old_managed, 1, &["adr-A".to_string(), "adr-B".to_string()])
        );

        let sections = make_sections(&[("adr-A", "New A content"), ("adr-B", "B content")]);
        let result = merge_content(Some(&existing), &sections, 2, false, None);

        assert!(
            result.content.contains("New A content"),
            "expected updated A content: {}",
            result.content
        );
        assert!(
            !result.content.contains("Old A content"),
            "expected old A content replaced: {}",
            result.content
        );
        assert!(
            result.content.contains("B content"),
            "expected B content preserved: {}",
            result.content
        );
    }

    #[test]
    fn test_merge_content_resync_adds_new_section() {
        // Existing has A; new has A, B — verify B is appended
        let old_managed = markers::wrap_adr_section("adr-A", "A content");
        let existing = format!(
            "{}\n",
            markers::wrap_in_markers(&old_managed, 1, &["adr-A".to_string()])
        );

        let sections = make_sections(&[("adr-A", "A content"), ("adr-B", "B content")]);
        let result = merge_content(Some(&existing), &sections, 2, false, None);

        assert!(
            result.content.contains("A content"),
            "expected A content: {}",
            result.content
        );
        assert!(
            result.content.contains("B content"),
            "expected new B content appended: {}",
            result.content
        );
        assert!(
            result.content.contains("<!-- adr:adr-B start -->"),
            "expected adr-B section markers: {}",
            result.content
        );
    }

    #[test]
    fn test_merge_content_resync_removes_deleted_section() {
        // Existing has A, B; new has A only — verify B is removed
        let old_managed = format!(
            "{}\n\n{}",
            markers::wrap_adr_section("adr-A", "A content"),
            markers::wrap_adr_section("adr-B", "B content"),
        );
        let existing = format!(
            "{}\n",
            markers::wrap_in_markers(&old_managed, 1, &["adr-A".to_string(), "adr-B".to_string()])
        );

        let sections = make_sections(&[("adr-A", "A content")]);
        let result = merge_content(Some(&existing), &sections, 2, false, None);

        assert!(
            result.content.contains("A content"),
            "expected A content: {}",
            result.content
        );
        assert!(
            !result.content.contains("B content"),
            "expected B content removed: {}",
            result.content
        );
        assert!(
            !result.content.contains("<!-- adr:adr-B"),
            "expected adr-B markers removed: {}",
            result.content
        );
    }

    #[test]
    fn test_merge_content_resync_preserves_order() {
        // Existing has B, A; new has A', B' — verify order is B', A' (existing order)
        let old_managed = format!(
            "{}\n\n{}",
            markers::wrap_adr_section("adr-B", "Old B"),
            markers::wrap_adr_section("adr-A", "Old A"),
        );
        let existing = format!(
            "{}\n",
            markers::wrap_in_markers(&old_managed, 1, &["adr-B".to_string(), "adr-A".to_string()])
        );

        let sections = make_sections(&[("adr-A", "New A"), ("adr-B", "New B")]);
        let result = merge_content(Some(&existing), &sections, 2, false, None);

        // B should come before A in the output (existing order preserved)
        let b_pos = result
            .content
            .find("<!-- adr:adr-B start -->")
            .expect("adr-B should exist");
        let a_pos = result
            .content
            .find("<!-- adr:adr-A start -->")
            .expect("adr-A should exist");
        assert!(
            b_pos < a_pos,
            "expected adr-B before adr-A (preserving existing order), B at {b_pos}, A at {a_pos}"
        );
        assert!(
            result.content.contains("New B"),
            "expected updated B content"
        );
        assert!(
            result.content.contains("New A"),
            "expected updated A content"
        );
    }

    #[test]
    fn test_merge_content_backward_compat_no_adr_markers() {
        // Existing managed block has NO per-ADR markers (old format) — should be
        // replaced wholesale with new per-ADR format
        let existing = format!(
            "{}\n",
            markers::wrap_in_markers("Old flat content", 1, &["adr-old".to_string()])
        );

        let sections = make_sections(&[("adr-new", "New content with markers")]);
        let result = merge_content(Some(&existing), &sections, 2, false, None);

        assert!(
            !result.content.contains("Old flat content"),
            "expected old flat content replaced: {}",
            result.content
        );
        assert!(
            result.content.contains("New content with markers"),
            "expected new content: {}",
            result.content
        );
        assert!(
            result.content.contains("<!-- adr:adr-new start -->"),
            "expected per-ADR markers in migrated output: {}",
            result.content
        );
    }

    #[test]
    fn test_merge_content_roundtrip_with_adr_sections() {
        // Write sections, then extract them back via markers::extract_adr_sections
        let sections = make_sections(&[
            ("adr-alpha", "Alpha rules\nMulti-line"),
            ("adr-beta", "Beta rules"),
        ]);
        let result = merge_content(None, &sections, 1, false, None);

        let managed =
            markers::extract_managed_content(&result.content).expect("should have managed section");
        let extracted = markers::extract_adr_sections(managed);

        assert_eq!(extracted.len(), 2, "expected 2 sections extracted");
        assert_eq!(extracted[0].id, "adr-alpha");
        assert_eq!(extracted[0].content, "Alpha rules\nMulti-line");
        assert_eq!(extracted[1].id, "adr-beta");
        assert_eq!(extracted[1].content, "Beta rules");
    }
}
