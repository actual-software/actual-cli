//! Discovery and reading of the `.actual/rules/` directory.
//!
//! # Design
//!
//! The repository root is **injected**, never discovered. `run_sync` in
//! [`crate::cli::commands::sync`] takes its `root_dir` the same way, and that is
//! what lets the whole path be exercised against a temporary directory.
//!
//! The governing guarantee is that one bad file never costs the rest of the
//! set. That is enforced structurally rather than by discipline: the per-file
//! loop contains **no `?` operator**. Every fallible step is an explicit match
//! that records a [`RuleFileError`] and continues, so the invariant can be
//! checked by looking for `?` inside the loop. The one `?`-equivalent is on
//! `read_dir` itself, before the loop begins.
//!
//! A missing `.actual/rules/` is not an error. A repository that has never been
//! synced is an ordinary state, and the useful response is an empty rule set,
//! not a failed command.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::ActualError;
use crate::rules::parse::parse_rule_document;
use crate::rules::types::{RuleFileError, RuleIssueKind, RuleSetLoadReport};

/// Directory, relative to the repository root, holding rule files.
pub const RULES_DIR_NAME: &str = ".actual/rules";

/// Per-file size cap, mirroring the config loader's limit in
/// [`crate::config::paths`]. Enforced against the bytes actually read, not
/// `metadata().len()`, so a failed stat cannot bypass it. The read itself is
/// limited to one byte past the cap so a huge file cannot be fully buffered.
pub const MAX_RULE_FILE_SIZE: u64 = 1024 * 1024; // 1 MiB

/// The rules directory for a repository rooted at `root_dir`.
pub fn rules_dir(root_dir: &Path) -> PathBuf {
    root_dir.join(RULES_DIR_NAME)
}

/// Load every rule document under `<root_dir>/.actual/rules/`.
///
/// Returns `Err` only when the rules directory exists but cannot be listed at
/// all. Per-file failures are collected in [`RuleSetLoadReport::errors`] and do
/// not stop the scan.
pub fn load_rule_set(root_dir: &Path) -> Result<RuleSetLoadReport, ActualError> {
    let dir = rules_dir(root_dir);
    let mut report = RuleSetLoadReport {
        rules_dir: dir.clone(),
        documents: Vec::new(),
        errors: Vec::new(),
    };

    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(e) => {
            return Err(ActualError::IoError(std::io::Error::new(
                e.kind(),
                format!("Failed to read rules directory {}: {e}", dir.display()),
            )))
        }
    };

    // Collect first, then sort, so both `documents` and `errors` come back in a
    // stable order regardless of what the filesystem hands back. Listing errors
    // are recorded, not dropped: `flatten()` would hide a vanished entry.
    let files = collect_rule_files(&dir, entries, &mut report);

    for path in files {
        // INVARIANT: no `?` below this line. Every failure becomes a
        // `RuleFileError` in `report.errors` and the loop moves to the next
        // file, so one unreadable or malformed document never costs the set.
        let bytes = match read_rule_file(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                report.errors.push(err);
                continue;
            }
        };
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(e) => {
                report.errors.push(RuleFileError::new(
                    &path,
                    RuleIssueKind::NotUtf8,
                    None,
                    format!("file is not valid UTF-8: {e}"),
                ));
                continue;
            }
        };
        match parse_rule_document(&path, &text) {
            Ok(doc) => report.documents.push(doc),
            Err(err) => report.errors.push(err),
        }
    }

    Ok(report)
}

/// Markdown rule files in `dir`, sorted by path. Per-entry listing failures are
/// recorded on `report` and skipped so they cannot hide a sibling file.
fn collect_rule_files(
    dir: &Path,
    entries: impl Iterator<Item = std::io::Result<std::fs::DirEntry>>,
    report: &mut RuleSetLoadReport,
) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                report.errors.push(RuleFileError::new(
                    dir,
                    RuleIssueKind::Io,
                    None,
                    format!("failed to read directory entry: {e}"),
                ));
                continue;
            }
        };
        let path = entry.path();
        let symlink = entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(false);
        if symlink || !is_markdown(&path) {
            continue;
        }
        files.push(path);
    }
    files.sort();
    files
}

/// Read one rule file, stopping one byte past [`MAX_RULE_FILE_SIZE`].
fn read_rule_file(path: &Path) -> Result<Vec<u8>, RuleFileError> {
    let file = std::fs::File::open(path).map_err(|e| {
        RuleFileError::new(
            path,
            RuleIssueKind::Io,
            None,
            format!("failed to read file: {e}"),
        )
    })?;
    let mut bytes = Vec::new();
    let mut limited = file.take(MAX_RULE_FILE_SIZE + 1);
    limited.read_to_end(&mut bytes).map_err(|e| {
        RuleFileError::new(
            path,
            RuleIssueKind::Io,
            None,
            format!("failed to read file: {e}"),
        )
    })?;
    let size = bytes.len() as u64;
    if size > MAX_RULE_FILE_SIZE {
        return Err(RuleFileError::new(
            path,
            RuleIssueKind::TooLarge,
            None,
            format!("file is {size} bytes, over the {MAX_RULE_FILE_SIZE} byte limit"),
        ));
    }
    Ok(bytes)
}

/// True for a `.md` path, case-insensitively — a case-insensitive checkout can
/// hand back `.MD`.
fn is_markdown(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::{tempdir, TempDir};

    const DOC_A: &str = "# Alpha\n\nScope A.\n\n### Rules\n\n- **R-A-001** MUST: alpha.\n";
    const DOC_B: &str = "# Beta\n\nScope B.\n\n### Rules\n\n- **R-B-001** SHOULD: beta.\n";
    const NOT_A_RULE_FILE: &str = "just some prose, no rules section at all\n";

    /// Helper: a temp dir with `<root>/.actual/rules/` populated from `files`.
    fn seed(files: &[(&str, &str)]) -> TempDir {
        let root = tempdir().unwrap();
        let dir = rules_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        for (name, contents) in files {
            std::fs::write(dir.join(name), contents).unwrap();
        }
        root
    }

    #[test]
    fn test_rules_dir_appends_the_rules_directory_name() {
        assert_eq!(
            rules_dir(Path::new("/repo")),
            PathBuf::from("/repo/.actual/rules")
        );
        assert_eq!(RULES_DIR_NAME, ".actual/rules");
    }

    #[test]
    fn test_load_rule_set_missing_directory_is_an_empty_report() {
        let root = tempdir().unwrap();
        let report = load_rule_set(root.path()).unwrap();
        assert!(report.is_empty());
        assert_eq!(report.rules_dir, rules_dir(root.path()));
    }

    #[test]
    fn test_load_rule_set_empty_directory_is_an_empty_report() {
        let root = seed(&[]);
        let report = load_rule_set(root.path()).unwrap();
        assert!(report.is_empty());
    }

    #[test]
    fn test_load_rule_set_returns_documents_sorted_by_path() {
        let root = seed(&[("c.md", DOC_A), ("a.md", DOC_B), ("b.md", DOC_A)]);
        let report = load_rule_set(root.path()).unwrap();
        let slugs: Vec<&str> = report.documents.iter().map(|d| d.slug().unwrap()).collect();
        assert_eq!(slugs, vec!["a", "b", "c"]);
        assert_eq!(report.rule_count(), 3);
        assert_eq!(report.warning_count(), 0);
    }

    #[test]
    fn test_load_rule_set_skips_non_markdown_and_accepts_uppercase_extension() {
        let root = seed(&[
            ("keep.md", DOC_A),
            ("keep-too.MD", DOC_B),
            ("skip.txt", DOC_A),
            ("skip-no-extension", DOC_A),
        ]);
        let report = load_rule_set(root.path()).unwrap();
        assert_eq!(report.documents.len(), 2);
        assert!(report.errors.is_empty());
    }

    /// The headline acceptance criterion: a file that fails to parse is
    /// reported and skipped, and the rest of the set still loads.
    #[test]
    fn test_load_rule_set_reports_a_bad_file_and_still_loads_the_rest() {
        let root = seed(&[
            ("a-good.md", DOC_A),
            ("b-bad.md", NOT_A_RULE_FILE),
            ("c-good.md", DOC_B),
        ]);
        let report = load_rule_set(root.path()).unwrap();

        assert_eq!(report.documents.len(), 2);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(
            report.errors[0].path,
            rules_dir(root.path()).join("b-bad.md")
        );
        assert_eq!(
            report.errors[0].issue.kind,
            RuleIssueKind::MissingRulesSection
        );
        assert!(!report.is_empty());
    }

    #[test]
    fn test_load_rule_set_reports_an_oversize_file() {
        let root = seed(&[("a-good.md", DOC_A)]);
        let big = "x".repeat((MAX_RULE_FILE_SIZE + 1) as usize);
        std::fs::write(rules_dir(root.path()).join("b-big.md"), &big).unwrap();

        let report = load_rule_set(root.path()).unwrap();
        assert_eq!(report.documents.len(), 1);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].issue.kind, RuleIssueKind::TooLarge);
        assert!(report.errors[0].to_string().contains("over the"));
    }

    /// A `read_dir` entry error is recorded instead of being dropped by
    /// `flatten()`, so a vanished entry cannot hide the rest of the set.
    #[test]
    fn test_collect_rule_files_records_a_listing_error() {
        let mut report = RuleSetLoadReport {
            rules_dir: PathBuf::from("/x/.actual/rules"),
            documents: Vec::new(),
            errors: Vec::new(),
        };
        let files = collect_rule_files(
            Path::new("/x/.actual/rules"),
            std::iter::once(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "entry vanished",
            ))),
            &mut report,
        );
        assert!(files.is_empty());
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].issue.kind, RuleIssueKind::Io);
        assert!(report.errors[0].to_string().contains("directory entry"));
        assert!(report.errors[0].to_string().contains("entry vanished"));
    }

    #[test]
    fn test_load_rule_set_reports_a_non_utf8_file() {
        let root = seed(&[("a-good.md", DOC_A)]);
        std::fs::write(
            rules_dir(root.path()).join("b-binary.md"),
            [0xff, 0xfe, 0x00, 0x41],
        )
        .unwrap();

        let report = load_rule_set(root.path()).unwrap();
        assert_eq!(report.documents.len(), 1);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].issue.kind, RuleIssueKind::NotUtf8);
    }

    /// A directory whose name ends in `.md` survives the extension filter and
    /// then fails to be read, which is the deterministic way to exercise the
    /// I/O arm without depending on file permissions.
    #[test]
    fn test_load_rule_set_reports_a_directory_named_like_a_rule_file() {
        let root = seed(&[("a-good.md", DOC_A)]);
        std::fs::create_dir(rules_dir(root.path()).join("b-dir.md")).unwrap();

        let report = load_rule_set(root.path()).unwrap();
        assert_eq!(report.documents.len(), 1);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].issue.kind, RuleIssueKind::Io);
    }

    #[test]
    fn test_read_rule_file_reports_an_open_failure() {
        let err = read_rule_file(Path::new("/no/such/actual-rule-file.md")).unwrap_err();
        assert_eq!(err.issue.kind, RuleIssueKind::Io);
        assert!(err.to_string().contains("failed to read file"));
    }

    #[cfg(unix)]
    #[test]
    fn test_load_rule_set_skips_symlinks() {
        let root = seed(&[("a-good.md", DOC_A)]);
        let dir = rules_dir(root.path());
        std::os::unix::fs::symlink(dir.join("a-good.md"), dir.join("b-link.md")).unwrap();

        let report = load_rule_set(root.path()).unwrap();
        assert_eq!(report.documents.len(), 1);
        assert!(report.errors.is_empty());
    }

    /// A rules path that is a file rather than a directory fails with
    /// `ENOTDIR`, which is not `NotFound`, so it surfaces as a hard error.
    #[test]
    fn test_load_rule_set_errors_when_the_rules_path_is_a_file() {
        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".actual")).unwrap();
        std::fs::write(rules_dir(root.path()), "not a directory").unwrap();

        let err = load_rule_set(root.path()).unwrap_err();
        assert!(matches!(err, ActualError::IoError(_)));
        assert!(err.to_string().contains("Failed to read rules directory"));
    }

    #[test]
    fn test_is_markdown_requires_an_extension() {
        assert!(is_markdown(Path::new("a.md")));
        assert!(is_markdown(Path::new("a.MD")));
        assert!(!is_markdown(Path::new("a.markdown")));
        assert!(!is_markdown(Path::new("a")));
    }
}
