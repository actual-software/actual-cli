use std::collections::BTreeMap;
use std::collections::HashSet;

use crate::tailoring::types::{FileOutput, SkippedAdr, TailoringOutput, TailoringSummary};

/// Merge multiple [`TailoringOutput`]s into a single combined output.
///
/// - Overlapping file paths (same `path`): content is concatenated with `"\n\n"`,
///   `adr_ids` are merged (deduplicated, preserving first-seen order), and
///   `reasoning` strings are combined with `"; "`.
/// - Different file paths: both are included in the result.
/// - `skipped_adrs` are deduplicated by `id` (first occurrence wins).
/// - `summary` is recomputed: `total_input` and `applicable` and `not_applicable`
///   are summed across all inputs, and `files_generated` is the number of unique
///   file paths in the merged result.
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

    // Recompute summary.
    let total_input: usize = outputs.iter().map(|o| o.summary.total_input).sum();
    let applicable: usize = outputs.iter().map(|o| o.summary.applicable).sum();
    let not_applicable: usize = outputs.iter().map(|o| o.summary.not_applicable).sum();
    let files_generated = file_map.len();

    let files: Vec<FileOutput> = file_map.into_values().collect();

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

#[cfg(test)]
mod tests {
    use super::*;

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

        // Summary recomputed
        assert_eq!(merged.summary.files_generated, 1);
        assert_eq!(merged.summary.total_input, 4); // 2 + 2
        assert_eq!(merged.summary.applicable, 4);
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
}
