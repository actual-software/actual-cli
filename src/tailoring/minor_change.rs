use std::collections::HashSet;
use std::path::Path;

use crate::generation::markers;
use crate::tailoring::types::{FileOutput, TailoringOutput};

/// Normalize text for comparison: lowercase, strip non-alphanumeric characters,
/// and collapse all whitespace into single spaces.
pub(crate) fn normalize_text(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Returns `true` if the change from `old` to `new` is considered minor.
///
/// A change is minor if:
/// - The normalized texts are identical (handles whitespace/punctuation-only diffs), or
/// - The Jaccard similarity of the normalized word sets is at or above 0.85
///   (handles word reordering and small additions/removals).
pub(crate) fn is_minor_change(old: &str, new: &str) -> bool {
    let old_norm = normalize_text(old);
    let new_norm = normalize_text(new);

    if old_norm == new_norm {
        return true;
    }

    let old_words: HashSet<&str> = old_norm.split_whitespace().collect();
    let new_words: HashSet<&str> = new_norm.split_whitespace().collect();

    // union > 0 here: if both word sets were empty, old_norm == new_norm would
    // have returned true above.
    let union = old_words.union(&new_words).count();
    let intersection = old_words.intersection(&new_words).count();
    (intersection as f64 / union as f64) >= 0.85
}

/// Returns `true` if all changes in `file` relative to `existing_content` are minor.
///
/// A file has only minor changes when:
/// - The existing file contains a managed section.
/// - The set of ADR IDs is unchanged (no additions or removals).
/// - Every section's content differs only by minor wording from the existing content.
fn file_has_only_minor_changes(file: &FileOutput, existing_content: &str) -> bool {
    let managed = match markers::extract_managed_content(existing_content) {
        Some(m) => m,
        None => return false,
    };

    let existing_sections = markers::extract_adr_sections(managed);

    let new_ids: HashSet<&str> = file.sections.iter().map(|s| s.adr_id.as_str()).collect();
    let existing_ids: HashSet<&str> = existing_sections.iter().map(|s| s.id.as_str()).collect();

    if new_ids != existing_ids {
        return false;
    }

    for section in &file.sections {
        // The id-set equality check above guarantees every new id exists in
        // existing_sections, so `find` will always return Some here.
        let existing = existing_sections
            .iter()
            .find(|s| s.id == section.adr_id)
            .expect("invariant: new_ids == existing_ids");
        if !is_minor_change(&existing.content, &section.content) {
            return false;
        }
    }

    true
}

/// Filter files from the tailoring output where the only changes are minor
/// (word reordering or small wording differences).
///
/// Files that don't exist on disk yet (new files) are never filtered.
/// Files where at least one section has a significant change are kept.
/// When files are removed, the summary's `files_generated` count is updated.
pub fn filter_minor_changes(mut output: TailoringOutput, root_dir: &Path) -> TailoringOutput {
    let before = output.files.len();

    output.files.retain(|file| {
        let full_path = root_dir.join(&file.path);
        match std::fs::read_to_string(&full_path) {
            Ok(existing_content) => !file_has_only_minor_changes(file, &existing_content),
            Err(_) => true,
        }
    });

    let removed = before - output.files.len();
    if removed > 0 {
        tracing::info!("skipped {removed} file(s) with only minor changes during tailoring");
        output.summary.files_generated = output.files.len();
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::markers;
    use crate::tailoring::types::{
        AdrSection, FileOutput, SkippedAdr, TailoringOutput, TailoringSummary,
    };

    // ── normalize_text ──

    #[test]
    fn normalize_empty() {
        assert_eq!(normalize_text(""), "");
    }

    #[test]
    fn normalize_lowercases() {
        assert_eq!(normalize_text("Hello World"), "hello world");
    }

    #[test]
    fn normalize_strips_punctuation() {
        assert_eq!(normalize_text("hello, world!"), "hello world");
    }

    #[test]
    fn normalize_collapses_whitespace() {
        assert_eq!(
            normalize_text("hello   world\n\tthere"),
            "hello world there"
        );
    }

    #[test]
    fn normalize_mixed() {
        assert_eq!(normalize_text("Use HTTPS — always!"), "use https always");
    }

    // ── is_minor_change ──

    #[test]
    fn minor_change_identical_texts() {
        assert!(is_minor_change("hello world", "hello world"));
    }

    #[test]
    fn minor_change_both_empty() {
        assert!(is_minor_change("", ""));
    }

    #[test]
    fn minor_change_word_reordering() {
        // Same words, different order → identical normalized word sets → minor
        assert!(is_minor_change("quick brown fox", "brown fox quick"));
    }

    #[test]
    fn minor_change_punctuation_only_diff() {
        assert!(is_minor_change("Use HTTPS.", "Use HTTPS!"));
    }

    #[test]
    fn minor_change_case_only_diff() {
        assert!(is_minor_change("Use HTTPS always", "use https always"));
    }

    #[test]
    fn minor_change_small_addition() {
        // Adding one word to a long sentence should stay above 0.85 threshold
        let old = "always use https for all api calls in production";
        let new = "always use https for all api calls in production environments";
        assert!(is_minor_change(old, new));
    }

    #[test]
    fn minor_change_significant_rewrite() {
        // Completely different content should not be considered minor
        assert!(!is_minor_change(
            "always use https for all api calls",
            "prefer dependency injection over global state management"
        ));
    }

    #[test]
    fn minor_change_completely_different_same_length() {
        assert!(!is_minor_change(
            "alpha beta gamma delta",
            "one two three four"
        ));
    }

    #[test]
    fn minor_change_old_empty_new_nonempty() {
        // Empty old vs non-empty new: union = new_words, intersection = 0 → not minor
        assert!(!is_minor_change("", "some content here now"));
    }

    #[test]
    fn minor_change_old_nonempty_new_empty() {
        assert!(!is_minor_change("some content here now", ""));
    }

    // ── file_has_only_minor_changes ──

    fn make_file_output(path: &str, sections: Vec<(&str, &str)>) -> FileOutput {
        FileOutput {
            path: path.to_string(),
            sections: sections
                .into_iter()
                .map(|(id, content)| AdrSection {
                    adr_id: id.to_string(),
                    content: content.to_string(),
                })
                .collect(),
            reasoning: String::new(),
        }
    }

    fn make_managed_content(sections: Vec<(&str, &str)>) -> String {
        let inner: Vec<String> = sections
            .into_iter()
            .map(|(id, content)| markers::wrap_adr_section(id, content))
            .collect();
        markers::wrap_in_markers(&inner.join("\n\n"), 1, &[])
    }

    #[test]
    fn file_minor_changes_no_managed_section() {
        let file = make_file_output("CLAUDE.md", vec![("adr-1", "some content")]);
        let existing = "# My Header\n\nSome user content without managed section";
        assert!(!file_has_only_minor_changes(&file, existing));
    }

    #[test]
    fn file_minor_changes_different_adr_ids_new_added() {
        let file = make_file_output(
            "CLAUDE.md",
            vec![("adr-1", "content"), ("adr-2", "new content")],
        );
        let existing = make_managed_content(vec![("adr-1", "content")]);
        assert!(!file_has_only_minor_changes(&file, &existing));
    }

    #[test]
    fn file_minor_changes_different_adr_ids_removed() {
        let file = make_file_output("CLAUDE.md", vec![("adr-1", "content")]);
        let existing = make_managed_content(vec![("adr-1", "content"), ("adr-2", "extra content")]);
        assert!(!file_has_only_minor_changes(&file, &existing));
    }

    #[test]
    fn file_minor_changes_significant_content_change() {
        let file = make_file_output(
            "CLAUDE.md",
            vec![("adr-1", "prefer dependency injection over globals")],
        );
        let existing = make_managed_content(vec![("adr-1", "always use https for api calls")]);
        assert!(!file_has_only_minor_changes(&file, &existing));
    }

    #[test]
    fn file_minor_changes_all_sections_minor() {
        let file = make_file_output(
            "CLAUDE.md",
            vec![
                ("adr-1", "always use https for all api calls in production"),
                ("adr-2", "prefer functional patterns over imperative"),
            ],
        );
        let existing = make_managed_content(vec![
            // Same content — is_minor_change returns true
            ("adr-1", "always use https for all api calls in production"),
            ("adr-2", "prefer functional patterns over imperative"),
        ]);
        assert!(file_has_only_minor_changes(&file, &existing));
    }

    #[test]
    fn file_minor_changes_word_reordering_is_minor() {
        let file = make_file_output("CLAUDE.md", vec![("adr-1", "fox brown quick")]);
        let existing = make_managed_content(vec![("adr-1", "quick brown fox")]);
        assert!(file_has_only_minor_changes(&file, &existing));
    }

    #[test]
    fn file_minor_changes_one_section_significant() {
        let file = make_file_output(
            "CLAUDE.md",
            vec![
                ("adr-1", "always use https for all api calls"),
                ("adr-2", "prefer dependency injection over global state"),
            ],
        );
        let existing = make_managed_content(vec![
            ("adr-1", "always use https for all api calls"),
            // adr-2 is significantly different
            ("adr-2", "completely different content about something else"),
        ]);
        assert!(!file_has_only_minor_changes(&file, &existing));
    }

    // ── filter_minor_changes ──

    fn make_output(files: Vec<FileOutput>) -> TailoringOutput {
        let files_generated = files.len();
        TailoringOutput {
            files,
            skipped_adrs: vec![],
            summary: TailoringSummary {
                total_input: 4,
                applicable: 3,
                not_applicable: 1,
                files_generated,
            },
        }
    }

    #[test]
    fn filter_keeps_new_files() {
        let dir = tempfile::tempdir().unwrap();
        let output = make_output(vec![make_file_output(
            "CLAUDE.md",
            vec![("adr-1", "some rules here")],
        )]);
        // File doesn't exist on disk → keep it
        let result = filter_minor_changes(output, dir.path());
        assert_eq!(result.files.len(), 1);
    }

    #[test]
    fn filter_keeps_file_with_significant_change() {
        let dir = tempfile::tempdir().unwrap();

        // Write an existing file
        let existing = make_managed_content(vec![("adr-1", "always use https")]);
        std::fs::write(dir.path().join("CLAUDE.md"), &existing).unwrap();

        let output = make_output(vec![make_file_output(
            "CLAUDE.md",
            // Completely different content
            vec![(
                "adr-1",
                "prefer dependency injection over globals everywhere",
            )],
        )]);
        let result = filter_minor_changes(output, dir.path());
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.summary.files_generated, 1);
    }

    #[test]
    fn filter_removes_file_with_only_minor_changes() {
        let dir = tempfile::tempdir().unwrap();

        let existing = make_managed_content(vec![("adr-1", "always use https for all api calls")]);
        std::fs::write(dir.path().join("CLAUDE.md"), &existing).unwrap();

        let output = make_output(vec![make_file_output(
            "CLAUDE.md",
            // Same words reordered → minor
            vec![("adr-1", "use https always for api calls all")],
        )]);
        let result = filter_minor_changes(output, dir.path());
        assert_eq!(result.files.len(), 0);
        assert_eq!(result.summary.files_generated, 0);
    }

    #[test]
    fn filter_mixed_files_keeps_significant_removes_minor() {
        let dir = tempfile::tempdir().unwrap();

        // minor change file
        let existing_root =
            make_managed_content(vec![("adr-1", "always use https for all api calls")]);
        std::fs::write(dir.path().join("CLAUDE.md"), &existing_root).unwrap();

        // significant change file
        std::fs::create_dir_all(dir.path().join("apps/web")).unwrap();
        let existing_web = make_managed_content(vec![("adr-2", "use react for the frontend")]);
        std::fs::write(dir.path().join("apps/web/CLAUDE.md"), &existing_web).unwrap();

        let output = make_output(vec![
            make_file_output(
                "CLAUDE.md",
                vec![("adr-1", "use https always for api calls all")], // minor
            ),
            make_file_output(
                "apps/web/CLAUDE.md",
                vec![("adr-2", "prefer vue over react for new projects")], // significant
            ),
        ]);

        let result = filter_minor_changes(output, dir.path());
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0].path, "apps/web/CLAUDE.md");
        assert_eq!(result.summary.files_generated, 1);
    }

    #[test]
    fn filter_no_files_removed_summary_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let output = make_output(vec![
            make_file_output("CLAUDE.md", vec![("adr-1", "content one")]),
            make_file_output("apps/web/CLAUDE.md", vec![("adr-2", "content two")]),
        ]);
        // Neither file exists on disk
        let result = filter_minor_changes(output, dir.path());
        assert_eq!(result.files.len(), 2);
        // summary.files_generated should be unchanged when no files are removed
        assert_eq!(result.summary.files_generated, 2);
    }

    #[test]
    fn filter_skipped_adrs_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let mut output = make_output(vec![]);
        output.skipped_adrs = vec![SkippedAdr {
            id: "adr-99".to_string(),
            reason: "not applicable".to_string(),
        }];
        let result = filter_minor_changes(output, dir.path());
        assert_eq!(result.skipped_adrs.len(), 1);
        assert_eq!(result.skipped_adrs[0].id, "adr-99");
    }
}
