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

use sha2::{Digest, Sha256};

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

/// One rule file as read from disk, before parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSource {
    pub path: PathBuf,
    /// The file's text, or the failure that stopped it being read.
    pub text: Result<String, RuleFileError>,
}

/// Everything under `.actual/rules/`, read but not yet parsed, with a digest of
/// exactly those bytes.
///
/// Reading and parsing are separate steps so the scope-index cache can hash
/// what it is about to parse and then skip the parse on a hit. Hashing one
/// snapshot and parsing another would leave the cache keyed to bytes that were
/// never indexed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSources {
    pub rules_dir: PathBuf,
    pub sources: Vec<RuleSource>,
    /// Directory-listing failures, which belong to no single file.
    pub errors: Vec<RuleFileError>,
    /// SHA-256 over every rule file's name and content, in path order.
    pub digest: String,
}

/// Bump when the digest's construction changes, so a digest computed by an
/// older build can never compare equal to a new one.
const DIGEST_VERSION: u32 = 1;

/// Read every rule file under `<root_dir>/.actual/rules/` without parsing it.
///
/// Returns `Err` on the same two conditions as [`load_rule_set`]: a repository
/// root that does not exist or is not a directory, and a rules directory that
/// cannot be listed at all.
pub fn read_rule_sources(root_dir: &Path) -> Result<RuleSources, ActualError> {
    validate_root(root_dir)?;
    let dir = rules_dir(root_dir);
    let mut listing_errors: Vec<RuleFileError> = Vec::new();

    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RuleSources {
                digest: digest_of(&[]),
                rules_dir: dir,
                sources: Vec::new(),
                errors: listing_errors,
            })
        }
        Err(e) => {
            return Err(ActualError::IoError(std::io::Error::new(
                e.kind(),
                format!("Failed to read rules directory {}: {e}", dir.display()),
            )))
        }
    };

    // Collect first, then sort, so the digest and both result lists come back in
    // a stable order regardless of what the filesystem hands back. Listing
    // errors are recorded, not dropped: `flatten()` would hide a vanished entry.
    let files = collect_rule_files(&dir, entries, &mut listing_errors);

    let mut sources = Vec::with_capacity(files.len());
    for path in files {
        // INVARIANT: no `?` below this line. Every failure becomes a
        // `RuleFileError` carried on the source, so one unreadable document
        // never costs the set.
        let text = match read_rule_file(&path) {
            Ok(bytes) => String::from_utf8(bytes).map_err(|e| {
                RuleFileError::new(
                    &path,
                    RuleIssueKind::NotUtf8,
                    None,
                    format!("file is not valid UTF-8: {e}"),
                )
            }),
            Err(err) => Err(err),
        };
        sources.push(RuleSource { path, text });
    }

    Ok(RuleSources {
        digest: digest_of(&sources),
        rules_dir: dir,
        sources,
        errors: listing_errors,
    })
}

/// A content digest over the rule files, in path order.
///
/// Every component is length-prefixed, so no rearrangement of names and bodies
/// can produce the same byte stream. A file that could not be read contributes
/// its failure *kind* rather than the error text, which keeps the digest stable
/// across runs while still changing when a file becomes readable.
fn digest_of(sources: &[RuleSource]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("digest-v{DIGEST_VERSION}\n").as_bytes());
    for source in sources {
        let name = source
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        update_field(&mut hasher, name.as_bytes());
        match &source.text {
            Ok(text) => {
                update_field(&mut hasher, b"ok");
                update_field(&mut hasher, text.as_bytes());
            }
            Err(err) => {
                update_field(&mut hasher, b"err");
                update_field(&mut hasher, format!("{:?}", err.issue.kind).as_bytes());
            }
        }
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Feed one length-prefixed field into the digest.
fn update_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// Parse sources that have already been read.
pub fn parse_rule_sources(sources: RuleSources) -> RuleSetLoadReport {
    let mut report = RuleSetLoadReport {
        rules_dir: sources.rules_dir,
        documents: Vec::new(),
        errors: sources.errors,
        digest: sources.digest,
    };
    for source in sources.sources {
        match source.text {
            Ok(text) => match parse_rule_document(&source.path, &text) {
                Ok(doc) => report.documents.push(doc),
                Err(err) => report.errors.push(err),
            },
            Err(err) => report.errors.push(err),
        }
    }
    report
}

/// Load every rule document under `<root_dir>/.actual/rules/`.
///
/// Returns `Err` when the supplied repository root does not exist or is not a
/// directory, and when the rules directory exists but cannot be listed at all.
/// Per-file failures are collected in [`RuleSetLoadReport::errors`] and do not
/// stop the scan.
pub fn load_rule_set(root_dir: &Path) -> Result<RuleSetLoadReport, ActualError> {
    Ok(parse_rule_sources(read_rule_sources(root_dir)?))
}

/// Reject a repository root that does not exist or is not a directory.
///
/// This exists to keep two states apart that are otherwise indistinguishable in
/// the result. A real repository with no `.actual/rules` is an ordinary state —
/// it has never been synced — and yields an empty report. A root that is not
/// there at all is a bad argument. Without this check both collapse into the
/// same successful empty report, so a typo or a stale checkout path reports
/// "no governance documents" rather than "no such directory", and the CLI exits
/// zero while having scanned nothing.
///
/// The check is here rather than in the CLI so that a direct library caller and
/// the command get the same contract. `metadata` follows symlinks, so a
/// symlinked checkout is still accepted.
fn validate_root(root_dir: &Path) -> Result<(), ActualError> {
    match std::fs::metadata(root_dir) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(ActualError::IoError(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            format!("Repository root {} is not a directory", root_dir.display()),
        ))),
        Err(e) => Err(ActualError::IoError(std::io::Error::new(
            e.kind(),
            format!("Failed to read repository root {}: {e}", root_dir.display()),
        ))),
    }
}

/// Markdown rule files in `dir`, sorted by path. Per-entry listing failures are
/// recorded on `report` and skipped so they cannot hide a sibling file.
fn collect_rule_files(
    dir: &Path,
    entries: impl Iterator<Item = std::io::Result<std::fs::DirEntry>>,
    errors: &mut Vec<RuleFileError>,
) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                errors.push(RuleFileError::new(
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

    // ── content digest ───────────────────────────────────────────────────

    /// Helper: the content digest of the rule set at `root`.
    fn digest(root: &Path) -> String {
        read_rule_sources(root).unwrap().digest
    }

    #[test]
    fn test_digest_is_stable_for_an_unchanged_directory() {
        let root = seed(&[("a.md", DOC_A), ("b.md", DOC_B)]);
        assert_eq!(digest(root.path()), digest(root.path()));
    }

    /// The defect this digest exists to prevent, and the case a
    /// size-and-timestamp fingerprint gets wrong: content is replaced with
    /// **different bytes of the same length** and the modification time is put
    /// back. Nothing observable about the file's metadata changed, so a stat
    /// key would reuse an index built from text that is no longer on disk.
    #[test]
    fn test_digest_changes_when_content_changes_at_constant_size_and_mtime() {
        let original =
            "# Alpha\n\nGoverns oauth token signing.\n\n### Rules\n\n- **R-A-001** MUST: a.\n";
        let rewritten =
            "# Alpha\n\nGoverns terraform provider pins.\n\n### Rules\n\n- **R-A-001** MUST: a.\n";
        // Pad to equal length so size cannot be what distinguishes them.
        let width = original.len().max(rewritten.len());
        let original = format!("{original:width$}");
        let rewritten = format!("{rewritten:width$}");
        assert_eq!(original.len(), rewritten.len());

        let root = seed(&[("a.md", &original)]);
        let path = rules_dir(root.path()).join("a.md");
        let before = digest(root.path());
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();

        std::fs::write(&path, &rewritten).unwrap();
        // Restore the timestamp, exactly as `cp -p`, `rsync -t` or an unpacked
        // archive would.
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();

        let after = std::fs::metadata(&path).unwrap();
        assert_eq!(after.len() as usize, original.len(), "size must be equal");
        assert_eq!(after.modified().unwrap(), mtime, "mtime must be restored");
        assert_ne!(
            digest(root.path()),
            before,
            "digest must follow content, not metadata"
        );
    }

    #[test]
    fn test_digest_changes_when_a_file_is_added_or_removed() {
        let root = seed(&[("a.md", DOC_A)]);
        let before = digest(root.path());

        let extra = rules_dir(root.path()).join("b.md");
        std::fs::write(&extra, DOC_B).unwrap();
        assert_ne!(digest(root.path()), before);

        std::fs::remove_file(&extra).unwrap();
        assert_eq!(digest(root.path()), before);
    }

    /// Identical content under a different name must digest differently, or a
    /// rename would go unnoticed.
    #[test]
    fn test_digest_covers_file_names_not_only_content() {
        let a = seed(&[("a.md", DOC_A)]);
        let b = seed(&[("b.md", DOC_A)]);
        assert_ne!(digest(a.path()), digest(b.path()));
    }

    #[test]
    fn test_digest_ignores_non_markdown_files() {
        let root = seed(&[("a.md", DOC_A)]);
        let before = digest(root.path());
        std::fs::write(rules_dir(root.path()).join("notes.txt"), "irrelevant").unwrap();
        assert_eq!(digest(root.path()), before);
    }

    /// A directory named like a rule file is a read failure that
    /// [`load_rule_set`] reports, so the digest must cover it too. Otherwise the
    /// cached index and the reported errors could disagree.
    #[test]
    fn test_digest_covers_a_directory_named_like_a_rule_file() {
        let root = seed(&[("a.md", DOC_A)]);
        let before = digest(root.path());
        std::fs::create_dir(rules_dir(root.path()).join("b-dir.md")).unwrap();
        assert_ne!(digest(root.path()), before);
        assert_eq!(load_rule_set(root.path()).unwrap().errors.len(), 1);
    }

    /// An absent rules directory and an empty one are the same rule set, so
    /// they must digest alike.
    #[test]
    fn test_digest_of_an_absent_directory_matches_an_empty_one() {
        let missing = tempdir().unwrap();
        let empty = seed(&[]);
        assert_eq!(digest(missing.path()), digest(empty.path()));
    }

    /// A file that cannot be read contributes its failure kind, so the digest
    /// still changes when it becomes readable.
    #[test]
    fn test_digest_covers_files_that_could_not_be_read() {
        let root = seed(&[("a.md", DOC_A)]);
        let dir = rules_dir(root.path());
        let before = digest(root.path());

        let big = "x".repeat((MAX_RULE_FILE_SIZE + 1) as usize);
        std::fs::write(dir.join("b.md"), &big).unwrap();
        let with_unreadable = digest(root.path());
        assert_ne!(with_unreadable, before);

        std::fs::write(dir.join("b.md"), DOC_B).unwrap();
        assert_ne!(digest(root.path()), with_unreadable);
    }

    #[test]
    fn test_parse_rule_sources_carries_the_digest_onto_the_report() {
        let root = seed(&[("a.md", DOC_A)]);
        let sources = read_rule_sources(root.path()).unwrap();
        let expected = sources.digest.clone();
        let report = parse_rule_sources(sources);
        assert_eq!(report.digest, expected);
        assert_eq!(report.documents.len(), 1);
    }

    #[test]
    fn test_load_rule_set_missing_directory_is_an_empty_report() {
        let root = tempdir().unwrap();
        let report = load_rule_set(root.path()).unwrap();
        assert!(report.is_empty());
        assert_eq!(report.rules_dir, rules_dir(root.path()));
    }

    /// The distinction APR-002 is about: a root that exists but was never
    /// synced is an empty report, while a root that does not exist is an error.
    /// Collapsing the two makes a mistyped path look like a governed repository
    /// with nothing in it.
    #[test]
    fn test_load_rule_set_rejects_a_missing_root() {
        let root = tempdir().unwrap();
        let missing = root.path().join("no-such-checkout");

        let err = load_rule_set(&missing).unwrap_err();
        assert!(matches!(err, ActualError::IoError(_)));
        let message = err.to_string();
        assert!(
            message.contains("Failed to read repository root"),
            "unexpected message: {message}"
        );
        assert!(message.contains("no-such-checkout"), "{message}");
    }

    #[test]
    fn test_load_rule_set_rejects_a_root_that_is_a_file() {
        let root = tempdir().unwrap();
        let file = root.path().join("a-file");
        std::fs::write(&file, "not a checkout").unwrap();

        let err = load_rule_set(&file).unwrap_err();
        assert!(matches!(err, ActualError::IoError(_)));
        let message = err.to_string();
        assert!(
            message.contains("is not a directory"),
            "unexpected message: {message}"
        );
        assert!(message.contains("a-file"), "{message}");
    }

    /// A root reached through a symlink is a normal checkout layout, so the
    /// validation must follow the link rather than reject it.
    #[cfg(unix)]
    #[test]
    fn test_load_rule_set_accepts_a_symlinked_root() {
        let real = seed(&[("a.md", DOC_A)]);
        let parent = tempdir().unwrap();
        let link = parent.path().join("linked-root");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();

        let report = load_rule_set(&link).unwrap();
        assert_eq!(report.documents.len(), 1);
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
        let mut errors: Vec<RuleFileError> = Vec::new();
        let files = collect_rule_files(
            Path::new("/x/.actual/rules"),
            std::iter::once(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "entry vanished",
            ))),
            &mut errors,
        );
        assert!(files.is_empty());
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].issue.kind, RuleIssueKind::Io);
        assert!(errors[0].to_string().contains("directory entry"));
        assert!(errors[0].to_string().contains("entry vanished"));
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
