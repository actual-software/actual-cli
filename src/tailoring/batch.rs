use std::collections::BTreeMap;

use crate::api::types::Adr;
use crate::tailoring::types::TailoringOutput;

/// Group ADRs into batches by category, respecting `batch_size` limits.
///
/// Categories are kept together in a single batch when they fit. A category is
/// split across multiple batches only when its own size exceeds `batch_size`.
/// Categories are iterated in stable order (sorted by `category.id`), and each
/// category is greedily packed into the current batch if it fits; otherwise a
/// new batch is started.
pub fn create_batches(adrs: &[Adr], batch_size: usize) -> Vec<Vec<Adr>> {
    // Group ADRs by category.id, preserving insertion order via BTreeMap.
    let mut by_category: BTreeMap<&str, Vec<&Adr>> = BTreeMap::new();
    for adr in adrs {
        by_category.entry(&adr.category.id).or_default().push(adr);
    }

    let mut batches: Vec<Vec<Adr>> = Vec::new();
    let mut current_batch: Vec<Adr> = Vec::new();

    for cat_adrs in by_category.values() {
        if cat_adrs.len() <= batch_size {
            // Category fits in a single batch — try to pack it into the current one.
            if current_batch.len() + cat_adrs.len() > batch_size && !current_batch.is_empty() {
                batches.push(std::mem::take(&mut current_batch));
            }
            for adr in cat_adrs {
                current_batch.push((*adr).clone());
            }
        } else {
            // Category exceeds batch_size — flush current batch, then chunk.
            if !current_batch.is_empty() {
                batches.push(std::mem::take(&mut current_batch));
            }
            for chunk in cat_adrs.chunks(batch_size) {
                let batch: Vec<Adr> = chunk.iter().map(|a| (*a).clone()).collect();
                batches.push(batch);
            }
        }
    }

    if !current_batch.is_empty() {
        batches.push(current_batch);
    }

    batches
}

/// Produce a human-readable summary of what previous batches produced.
///
/// This context string is passed to later batches so the LLM knows what files
/// and ADR placements were already decided. Returns an empty string when
/// `previous_outputs` is empty.
pub fn batch_context_summary(previous_outputs: &[TailoringOutput]) -> String {
    if previous_outputs.is_empty() {
        return String::new();
    }

    let mut lines: Vec<String> = Vec::new();
    let mut total_files: usize = 0;

    lines.push("Previous batches produced the following files:".to_string());

    for (i, output) in previous_outputs.iter().enumerate() {
        lines.push(format!("  Batch {}:", i + 1));
        for file in &output.files {
            lines.push(format!("    - {} ({} ADRs)", file.path, file.adr_ids.len()));
        }
        total_files += output.files.len();
    }

    lines.push(format!("Total files so far: {total_files}"));

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{AdrCategory, AppliesTo};
    use crate::tailoring::types::{FileOutput, TailoringSummary};

    fn make_adr(id: &str, category_id: &str, category_name: &str) -> Adr {
        Adr {
            id: id.to_string(),
            title: format!("ADR {id}"),
            context: None,
            policies: vec!["policy".to_string()],
            instructions: None,
            category: AdrCategory {
                id: category_id.to_string(),
                name: category_name.to_string(),
                path: category_name.to_string(),
            },
            applies_to: AppliesTo {
                languages: vec![],
                frameworks: vec![],
            },
            matched_projects: vec![],
        }
    }

    #[test]
    fn test_batch_30_adrs_4_categories_batch_size_15() {
        // 4 categories: cat-a(10), cat-b(8), cat-c(7), cat-d(5)
        let mut adrs = Vec::new();
        for i in 0..10 {
            adrs.push(make_adr(&format!("a-{i}"), "cat-a", "Category A"));
        }
        for i in 0..8 {
            adrs.push(make_adr(&format!("b-{i}"), "cat-b", "Category B"));
        }
        for i in 0..7 {
            adrs.push(make_adr(&format!("c-{i}"), "cat-c", "Category C"));
        }
        for i in 0..5 {
            adrs.push(make_adr(&format!("d-{i}"), "cat-d", "Category D"));
        }

        let batches = create_batches(&adrs, 15);

        // Should produce 2-3 batches
        assert!(
            (2..=3).contains(&batches.len()),
            "expected 2-3 batches, got {}",
            batches.len()
        );

        // Verify total ADRs preserved
        let total: usize = batches.iter().map(|b| b.len()).sum();
        assert_eq!(total, 30);

        // Verify no batch exceeds batch_size
        for (i, batch) in batches.iter().enumerate() {
            assert!(
                batch.len() <= 15,
                "batch {i} has {} items, exceeds batch_size 15",
                batch.len()
            );
        }

        // Verify categories are NOT split across batches when possible.
        // Each category that fits within 15 should appear in exactly one batch.
        for cat_id in &["cat-a", "cat-b", "cat-c", "cat-d"] {
            let batches_containing: Vec<usize> = batches
                .iter()
                .enumerate()
                .filter(|(_, b)| b.iter().any(|a| a.category.id == *cat_id))
                .map(|(i, _)| i)
                .collect();
            assert_eq!(
                batches_containing.len(),
                1,
                "category {cat_id} appears in {} batches (expected 1): {:?}",
                batches_containing.len(),
                batches_containing
            );
        }
    }

    #[test]
    fn test_batch_5_adrs_batch_size_15() {
        let adrs: Vec<Adr> = (0..5)
            .map(|i| make_adr(&format!("x-{i}"), "cat-x", "Category X"))
            .collect();

        let batches = create_batches(&adrs, 15);

        assert_eq!(batches.len(), 1, "expected 1 batch, got {}", batches.len());
        assert_eq!(batches[0].len(), 5);
    }

    #[test]
    fn test_batch_context_summary() {
        let output = TailoringOutput {
            files: vec![
                FileOutput {
                    path: "CLAUDE.md".to_string(),
                    content: "root rules".to_string(),
                    reasoning: "root".to_string(),
                    adr_ids: vec!["adr-001".to_string(), "adr-002".to_string()],
                },
                FileOutput {
                    path: "apps/web/CLAUDE.md".to_string(),
                    content: "web rules".to_string(),
                    reasoning: "web".to_string(),
                    adr_ids: vec!["adr-003".to_string()],
                },
            ],
            skipped_adrs: vec![],
            summary: TailoringSummary {
                total_input: 3,
                applicable: 3,
                not_applicable: 0,
                files_generated: 2,
            },
        };

        let summary = batch_context_summary(&[output]);

        assert!(
            summary.contains("CLAUDE.md"),
            "summary should mention CLAUDE.md: {summary}"
        );
        assert!(
            summary.contains("apps/web/CLAUDE.md"),
            "summary should mention apps/web/CLAUDE.md: {summary}"
        );
        assert!(
            summary.contains("2 ADRs"),
            "summary should mention 2 ADRs for CLAUDE.md: {summary}"
        );
        assert!(
            summary.contains("1 ADRs"),
            "summary should mention 1 ADRs for apps/web/CLAUDE.md: {summary}"
        );
        assert!(
            summary.contains("Total files so far: 2"),
            "summary should mention total files: {summary}"
        );
    }

    #[test]
    fn test_batch_context_summary_empty() {
        let summary = batch_context_summary(&[]);
        assert!(
            summary.is_empty(),
            "expected empty string for no previous outputs, got: {summary}"
        );
    }

    #[test]
    fn test_batch_large_category_split() {
        // A single category with 20 ADRs, batch_size=8 → should split into 3 batches
        let adrs: Vec<Adr> = (0..20)
            .map(|i| make_adr(&format!("big-{i}"), "cat-big", "Big Category"))
            .collect();

        let batches = create_batches(&adrs, 8);

        assert_eq!(
            batches.len(),
            3,
            "expected 3 batches for 20 ADRs with batch_size 8, got {}",
            batches.len()
        );
        assert_eq!(batches[0].len(), 8);
        assert_eq!(batches[1].len(), 8);
        assert_eq!(batches[2].len(), 4);
    }

    #[test]
    fn test_batch_empty_input() {
        let batches = create_batches(&[], 10);
        assert!(batches.is_empty(), "expected no batches for empty input");
    }
}
