//! The session-wide record of the decisions `rules brief` added to context,
//! for the session summary: "X of Y ADRs added to context".
//!
//! # Design
//!
//! **Its own file, beside [`super::brief_memory`].** The memory belongs to
//! one agent's context and is wiped when that context compacts, so the agent
//! is briefed again. This record belongs to the whole session, so nothing
//! resets it: `rules brief --claude-session-start` clears only the memory, and
//! a decision briefed again after a compaction is already here and adds
//! nothing.
//!
//! **Identity is `(session_id, rules_dir)`, with no `agent_id`.** The main
//! agent and every subagent write to one record, so a decision briefed to any
//! of them counts once. `rules_dir` is keyed for the reason the memory gives:
//! one conversation can govern several rule sets.
//!
//! **Folders, beside decisions.** A folder is the directory a file sits in,
//! named from the repository root. It is covered once a decision governs any
//! file the agents worked on in it, or any path a plan named there, and a gap
//! while every one was governed by none, whatever order they arrive in. Docs,
//! config, lockfiles and generated output never count toward one;
//! [`folder_of`] holds that list. Every read notes a folder, so
//! [`record_folders`] checks the record without the lock and takes it only
//! when a folder's state changes.
//!
//! **Writes take a lock.** [`crate::config::paths::write_secure`] stages
//! every write at one fixed `.tmp` path, so two hook runs writing at once can
//! leave a damaged file, and a damaged file reads as an empty session. Every
//! write holds the directory's lock from its read to its rename, so concurrent
//! writers lose neither the file nor each other's decisions. A writer that
//! cannot take the lock within [`LOCK_WAIT`] drops its change instead of
//! holding up the brief it was recording, and no write ever happens without
//! the lock.
//!
//! **Fail open means undercount.** A missing, damaged or other-version file
//! reads as an empty session, and a dropped change is a decision not counted.
//!
//! **Pruning.** As with the memory, records untouched for
//! [`LEDGER_MAX_AGE`] are removed whenever one is written.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Bumped whenever [`BriefLedger`]'s on-disk shape changes incompatibly. A
/// mismatch reads as an empty session.
const FORMAT_VERSION: u32 = 1;

/// Subdirectory of the config directory holding the records.
const LEDGER_DIR_NAME: &str = "brief-ledger";

/// The file every write locks, one for the whole directory. Nothing is ever
/// read from it or written to it.
const LOCK_FILE_NAME: &str = "ledger.lock";

/// A record older than this is pruned the next time any is written.
const LEDGER_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How long a write waits for another writer to finish. The hook that writes
/// runs under a timeout of a few seconds, and the brief it is about to print
/// matters more than the count.
const LOCK_WAIT: Duration = Duration::from_millis(500);

/// The pause between attempts while another writer holds the lock.
const LOCK_RETRY: Duration = Duration::from_millis(5);

/// What identifies one session's record within one rule set.
#[derive(Debug, Clone, Copy)]
pub struct LedgerKey<'a> {
    pub session_id: &'a str,
    pub rules_dir: &'a Path,
}

/// The decisions briefed to a session's agents, the counts last shown to the
/// user, and the folders the agents worked in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BriefLedger {
    format_version: u32,
    /// `AdrGroup::key` of every decision recorded as briefed, to any agent.
    pub briefed: BTreeSet<String>,
    /// Absent until the summary has been shown.
    pub last_reported: Option<Reported>,
    /// Folders holding a path a decision governs.
    pub covered_folders: BTreeSet<String>,
    /// Folders holding a path no decision governs, noted only while the
    /// folder is not covered. A gap is a folder here and not in
    /// `covered_folders`, whichever order the two were noted in.
    pub uncovered_folders: BTreeSet<String>,
}

impl BriefLedger {
    /// The gaps: folders the session worked in where no decision governed any
    /// of the files, sorted.
    pub fn gaps(&self) -> impl Iterator<Item = &str> {
        self.uncovered_folders
            .difference(&self.covered_folders)
            .map(String::as_str)
    }

    /// Whether noting `folder` changes anything. A covered folder stays
    /// covered, and an uncovered one changes only by becoming covered.
    fn would_change(&self, folder: &str, covered: bool) -> bool {
        !self.covered_folders.contains(folder)
            && (covered || !self.uncovered_folders.contains(folder))
    }

    fn note_folder(&mut self, folder: &str, covered: bool) {
        if self.would_change(folder, covered) {
            let folders = if covered {
                &mut self.covered_folders
            } else {
                &mut self.uncovered_folders
            };
            folders.insert(folder.to_string());
        }
    }
}

/// The counts behind the summary line the user last saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reported {
    /// Decisions added to context this session.
    pub added: usize,
    /// Decisions the rule set can add to context.
    pub available: usize,
    /// How many folders worked in no decision governs. Defaulted because a
    /// record written before gaps were shown has none, and a field that failed
    /// to load would empty the whole record.
    #[serde(default)]
    pub gaps: usize,
}

/// The directory the records live in, under the user's config directory.
pub fn ledger_dir() -> Option<PathBuf> {
    crate::config::paths::config_dir()
        .ok()
        .map(|dir| dir.join(LEDGER_DIR_NAME))
}

fn ledger_path(dir: &Path, key: &LedgerKey<'_>) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(key.session_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(key.rules_dir.as_os_str().as_encoded_bytes());
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    dir.join(format!("{hex}.json"))
}

/// The record for `key`, or an empty one when absent, unreadable, damaged or
/// written by another format version. A read takes no lock: every write
/// replaces the file whole, so a reader sees the old record or the new one.
pub fn load(dir: &Path, key: &LedgerKey<'_>) -> BriefLedger {
    std::fs::read_to_string(ledger_path(dir, key))
        .ok()
        .and_then(|text| serde_json::from_str::<BriefLedger>(&text).ok())
        .filter(|ledger| ledger.format_version == FORMAT_VERSION)
        .unwrap_or_default()
}

/// Add `decision_keys` to the record for `key`, best effort.
pub fn record<'k>(
    dir: &Path,
    key: &LedgerKey<'_>,
    decision_keys: impl IntoIterator<Item = &'k str>,
) {
    update(dir, key, |ledger| {
        ledger
            .briefed
            .extend(decision_keys.into_iter().map(str::to_string));
    });
}

/// Note that the session worked on a file in `folder`, and whether a decision
/// governs that file, best effort.
pub fn record_folder(dir: &Path, key: &LedgerKey<'_>, folder: &str, covered: bool) {
    record_folders(dir, key, &[(folder, covered)]);
}

/// [`record_folder`] for several folders under one lock, which is taken only
/// when the record's lock-free read says one of them would change.
pub fn record_folders<S: AsRef<str>>(dir: &Path, key: &LedgerKey<'_>, folders: &[(S, bool)]) {
    let recorded = load(dir, key);
    if folders
        .iter()
        .any(|(folder, covered)| recorded.would_change(folder.as_ref(), *covered))
    {
        update(dir, key, |ledger| {
            for (folder, covered) in folders {
                ledger.note_folder(folder.as_ref(), *covered);
            }
        });
    }
}

/// The folder a repository-relative file counts toward, `.` at the root, or
/// `None` for a file that never counts.
pub fn folder_of(relative: &str) -> Option<String> {
    if NEVER_COUNTED.iter().any(|rule| rule.matches(relative)) {
        return None;
    }
    let folder = relative.rsplit_once('/').map_or(".", |(folder, _)| folder);
    Some(folder.to_string())
}

/// Docs, config, lockfiles and generated output, which never count toward a
/// folder. Matched on the path alone: the brief never reads a file's contents.
const NEVER_COUNTED: &[Excluded] = &[
    // Docs.
    Excluded::Extension(&["md", "mdx", "rst", "txt"]),
    // Config, dotfiles included.
    Excluded::Extension(&["json", "yaml", "yml", "toml", "ini"]),
    Excluded::Hidden,
    Excluded::Infix(&[".config."]),
    // Lockfiles.
    Excluded::Extension(&["lock"]),
    Excluded::Name(&["go.sum", "bun.lockb"]),
    // Generated output. `.next/` is hidden, so it needs no entry of its own.
    Excluded::Under(super::SKIP_DIRS),
    Excluded::Infix(&[".min.", ".generated."]),
];

/// One way a path marks a file that never counts.
enum Excluded {
    /// The file's extension, in any case.
    Extension(&'static [&'static str]),
    /// The file's whole name.
    Name(&'static [&'static str]),
    /// Text within the file's name.
    Infix(&'static [&'static str]),
    /// A directory anywhere above the file.
    Under(&'static [&'static str]),
    /// A file or directory whose name starts with a dot.
    Hidden,
}

impl Excluded {
    fn matches(&self, relative: &str) -> bool {
        let (dirs, name) = relative.rsplit_once('/').unwrap_or(("", relative));
        match self {
            Self::Extension(extensions) => Path::new(name)
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| extensions.iter().any(|e| ext.eq_ignore_ascii_case(e))),
            Self::Name(names) => names.contains(&name),
            Self::Infix(infixes) => infixes.iter().any(|infix| name.contains(infix)),
            Self::Under(names) => dirs.split('/').any(|dir| names.contains(&dir)),
            Self::Hidden => relative.split('/').any(|part| part.starts_with('.')),
        }
    }
}

/// Apply `change` to the record for `key` and write the result back, best
/// effort, under the lock. A change that changes nothing writes nothing.
pub fn update(dir: &Path, key: &LedgerKey<'_>, change: impl FnOnce(&mut BriefLedger)) {
    update_within(dir, key, LOCK_WAIT, change);
}

fn update_within(
    dir: &Path,
    key: &LedgerKey<'_>,
    wait: Duration,
    change: impl FnOnce(&mut BriefLedger),
) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    // Held to the end of this function, so the read, the write and the prune
    // all happen under it.
    let Some(_lock) = lock(dir, wait) else {
        return;
    };
    let before = load(dir, key);
    let mut ledger = before.clone();
    change(&mut ledger);
    if ledger == before {
        return;
    }
    ledger.format_version = FORMAT_VERSION;
    // Serializing a version, sets of strings and three counts cannot fail, so
    // there is no error path to take here; writing nothing is still the right
    // fallback.
    if let Ok(json) = serde_json::to_string(&ledger) {
        let _ = crate::config::paths::write_secure(&ledger_path(dir, key), json.as_bytes());
    }
    prune_stale(dir);
}

/// Take the directory's write lock, waiting up to `wait` while another writer
/// holds it. `None` when the lock file cannot be opened, the platform has no
/// file locks, or the wait runs out. The lock is released when the returned
/// file is dropped.
fn lock(dir: &Path, wait: Duration) -> Option<File> {
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(dir.join(LOCK_FILE_NAME)).ok()?;
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Some(file),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(LOCK_RETRY);
            }
            Err(_) => return None,
        }
    }
}

fn prune_stale(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        // Records only: the lock file, and anything else in the directory,
        // is left alone however old it is.
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        // A file whose age cannot be read is kept: deleting a live session's
        // record on a failed stat would silently undercount it.
        let stale = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .is_some_and(|modified| {
                now.duration_since(modified).unwrap_or_default() > LEDGER_MAX_AGE
            });
        if stale {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::tempdir;

    const RULES: &str = "/repo/.actual/rules";

    fn key(session_id: &str) -> LedgerKey<'_> {
        LedgerKey {
            session_id,
            rules_dir: Path::new(RULES),
        }
    }

    fn briefed(dir: &Path, key: &LedgerKey<'_>) -> Vec<String> {
        load(dir, key).briefed.iter().cloned().collect()
    }

    fn age(path: &Path, by: Duration) {
        let file = File::options().write(true).open(path).unwrap();
        file.set_modified(std::time::SystemTime::now() - by)
            .unwrap();
    }

    const EIGHT_DAYS: Duration = Duration::from_secs(8 * 24 * 60 * 60);

    #[test]
    fn test_a_new_record_is_empty() {
        let dir = tempdir().unwrap();
        let ledger = load(dir.path(), &key("s1"));
        assert!(ledger.briefed.is_empty());
        assert_eq!(ledger.last_reported, None);
    }

    /// Writes add up, and a decision recorded twice counts once.
    #[test]
    fn test_recorded_decisions_add_up() {
        let dir = tempdir().unwrap();
        record(dir.path(), &key("s1"), ["Adopt RS256"]);
        record(dir.path(), &key("s1"), ["Pin Providers", "Adopt RS256"]);

        assert_eq!(
            briefed(dir.path(), &key("s1")),
            ["Adopt RS256", "Pin Providers"]
        );
    }

    /// One conversation over two rule sets keeps two records, and another
    /// session starts from nothing.
    #[test]
    fn test_records_are_keyed_by_session_and_rules_dir() {
        let dir = tempdir().unwrap();
        let other_rules = LedgerKey {
            session_id: "s1",
            rules_dir: Path::new("/repo/b/.actual/rules"),
        };
        record(dir.path(), &key("s1"), ["Adopt RS256"]);

        assert_eq!(briefed(dir.path(), &key("s1")), ["Adopt RS256"]);
        assert!(briefed(dir.path(), &other_rules).is_empty());
        assert!(briefed(dir.path(), &key("s2")).is_empty());
    }

    /// What the user was last shown survives later decisions, which the
    /// summary needs in order to tell a changed count from a repeated one.
    #[test]
    fn test_last_reported_survives_a_later_decision() {
        let dir = tempdir().unwrap();
        let shown = Reported {
            added: 1,
            available: 12,
            gaps: 2,
        };
        record(dir.path(), &key("s1"), ["Adopt RS256"]);
        update(dir.path(), &key("s1"), |ledger| {
            ledger.last_reported = Some(shown)
        });
        record(dir.path(), &key("s1"), ["Pin Providers"]);

        let ledger = load(dir.path(), &key("s1"));
        assert_eq!(ledger.last_reported, Some(shown));
        assert_eq!(ledger.briefed.len(), 2);
    }

    /// A change that changes nothing writes nothing, so a decision already
    /// counted costs no write.
    #[test]
    fn test_a_change_that_changes_nothing_writes_nothing() {
        let dir = tempdir().unwrap();
        record(dir.path(), &key("s1"), []);
        assert!(!ledger_path(dir.path(), &key("s1")).exists());

        record(dir.path(), &key("s1"), ["Adopt RS256"]);
        let path = ledger_path(dir.path(), &key("s1"));
        // Backdated, so a write within the clock's resolution still shows.
        age(&path, Duration::from_secs(60 * 60));
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();

        record(dir.path(), &key("s1"), ["Adopt RS256"]);

        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before
        );
    }

    /// Fail open: damaged, older-format or foreign-version state is an empty
    /// session, never an error.
    #[test]
    fn test_a_damaged_or_other_version_file_reads_as_empty() {
        let dir = tempdir().unwrap();
        let path = ledger_path(dir.path(), &key("s1"));
        for text in [
            "{ not json",
            r#"{"format_version":99,"briefed":["Adopt RS256"]}"#,
            r#"{"briefed":["Adopt RS256"]}"#,
        ] {
            std::fs::write(&path, text).unwrap();
            assert!(briefed(dir.path(), &key("s1")).is_empty(), "{text}");
        }
    }

    /// The next write over a damaged file starts the session again from
    /// what it adds, rather than failing.
    #[test]
    fn test_a_write_replaces_a_damaged_file() {
        let dir = tempdir().unwrap();
        std::fs::write(ledger_path(dir.path(), &key("s1")), "{ not json").unwrap();

        record(dir.path(), &key("s1"), ["Pin Providers"]);

        assert_eq!(briefed(dir.path(), &key("s1")), ["Pin Providers"]);
    }

    #[test]
    fn test_writing_into_an_unwritable_place_is_silent() {
        let dir = tempdir().unwrap();
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, "x").unwrap();
        // A directory cannot be created beneath a regular file.
        record(&blocker.join("sub"), &key("s1"), ["Adopt RS256"]);
        assert!(briefed(&blocker.join("sub"), &key("s1")).is_empty());
    }

    #[test]
    fn test_pruning_an_unreadable_directory_is_harmless() {
        let dir = tempdir().unwrap();
        prune_stale(&dir.path().join("never-created"));
    }

    /// Stale records go; a fresh one, the lock file and anything foreign
    /// stay, however old they are.
    #[test]
    fn test_stale_records_are_pruned_and_the_rest_kept() {
        let dir = tempdir().unwrap();
        record(dir.path(), &key("old"), ["Adopt RS256"]);
        let old = ledger_path(dir.path(), &key("old"));
        let lock_file = dir.path().join(LOCK_FILE_NAME);
        let foreign = dir.path().join("notes.txt");
        std::fs::write(&foreign, "not ours").unwrap();
        for path in [&old, &lock_file, &foreign] {
            age(path, EIGHT_DAYS);
        }

        record(dir.path(), &key("fresh"), ["Adopt RS256"]);

        assert!(!old.exists(), "a stale record was kept");
        assert!(ledger_path(dir.path(), &key("fresh")).exists());
        assert!(lock_file.exists(), "the lock file was pruned");
        assert!(foreign.exists(), "a foreign file was pruned");
    }

    // ── folders ──────────────────────────────────────────────────────────

    /// The covered folders and the gaps in `key`'s record.
    fn folders(dir: &Path, key: &LedgerKey<'_>) -> (Vec<String>, Vec<String>) {
        let ledger = load(dir, key);
        let gaps = ledger.gaps().map(str::to_string).collect();
        (ledger.covered_folders.into_iter().collect(), gaps)
    }

    #[test]
    fn test_a_governed_folder_is_covered_and_an_ungoverned_one_is_a_gap() {
        let dir = tempdir().unwrap();
        record_folder(dir.path(), &key("s1"), "services/auth", true);
        record_folder(dir.path(), &key("s1"), "infra/terraform", false);

        let (covered, gaps) = folders(dir.path(), &key("s1"));
        assert_eq!(covered, ["services/auth"]);
        assert_eq!(gaps, ["infra/terraform"]);
    }

    /// One governed file covers its folder whichever order the reads arrive
    /// in, and another session's record is untouched by either.
    #[test]
    fn test_a_folder_with_one_governed_file_is_covered_in_either_order() {
        let dir = tempdir().unwrap();
        record_folder(dir.path(), &key("s1"), "src/billing", false);
        record_folder(dir.path(), &key("s1"), "src/billing", true);
        record_folder(dir.path(), &key("s2"), "src/billing", true);
        record_folder(dir.path(), &key("s2"), "src/billing", false);

        for session in ["s1", "s2"] {
            let (covered, gaps) = folders(dir.path(), &key(session));
            assert_eq!(covered, ["src/billing"], "{session}");
            assert!(gaps.is_empty(), "{session}: {gaps:?}");
        }
        assert_eq!(folders(dir.path(), &key("s3")), (vec![], vec![]));
    }

    /// Noting what the record already says needs neither the lock nor a
    /// write, so with the lock held elsewhere it returns at once. A real
    /// change, the control, waits out the lock and is dropped.
    #[test]
    fn test_a_folder_that_changes_nothing_skips_the_lock() {
        let dir = tempdir().unwrap();
        record_folder(dir.path(), &key("s1"), "src/billing", true);
        record_folder(dir.path(), &key("s1"), "infra", false);
        let path = ledger_path(dir.path(), &key("s1"));
        let before = std::fs::read_to_string(&path).unwrap();
        let _held = lock(dir.path(), Duration::ZERO).expect("the lock");

        let started = Instant::now();
        for (folder, covered) in [
            ("src/billing", true),
            ("src/billing", false),
            ("infra", false),
        ] {
            record_folder(dir.path(), &key("s1"), folder, covered);
        }
        assert!(started.elapsed() < LOCK_WAIT, "a no-op waited on the lock");

        record_folder(dir.path(), &key("s1"), "infra", true);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    /// The rule is applied again under the lock, so a folder another writer
    /// covered after the lock-free read is not then noted as uncovered.
    #[test]
    fn test_under_the_lock_a_folder_covered_since_the_read_stays_as_it_is() {
        let mut ledger = BriefLedger::default();
        ledger.note_folder("src/billing", true);
        let covered = ledger.clone();

        ledger.note_folder("src/billing", false);

        assert_eq!(ledger, covered);
    }

    /// A record in the shape v0.7.0 writes loads with no folders, and with no
    /// gap count in `last_reported`, which reads as none rather than emptying
    /// the record. The first folder noted keeps its decisions and counts.
    #[test]
    fn test_a_record_written_before_folders_keeps_its_decisions() {
        let dir = tempdir().unwrap();
        std::fs::write(
            ledger_path(dir.path(), &key("s1")),
            r#"{"format_version":1,"briefed":["Adopt RS256"],"last_reported":{"added":1,"available":12}}"#,
        )
        .unwrap();
        assert_eq!(folders(dir.path(), &key("s1")), (vec![], vec![]));

        record_folder(dir.path(), &key("s1"), "infra/terraform", false);

        let ledger = load(dir.path(), &key("s1"));
        assert_eq!(briefed(dir.path(), &key("s1")), ["Adopt RS256"]);
        assert_eq!(
            ledger.last_reported,
            Some(Reported {
                added: 1,
                available: 12,
                gaps: 0
            })
        );
        assert_eq!(folders(dir.path(), &key("s1")).1, ["infra/terraform"]);
    }

    #[test]
    fn test_a_file_counts_toward_the_folder_it_sits_in() {
        for (path, folder) in [
            ("src/billing/invoice.ts", "src/billing"),
            ("services/auth/oauth/token.ts", "services/auth/oauth"),
            ("main.rs", "."),
            ("Makefile", "."),
            // Near an exclusion, and not one.
            ("src/admin.ts", "src"),
            ("src/configure.ts", "src"),
            ("src/build.rs", "src"),
            ("src/markdown/parse.rs", "src/markdown"),
            ("src/lockfile.ts", "src"),
        ] {
            assert_eq!(folder_of(path).as_deref(), Some(folder), "{path}");
        }
    }

    fn assert_never_counts(paths: &[&str]) {
        for path in paths {
            assert_eq!(folder_of(path), None, "{path}");
        }
    }

    #[test]
    fn test_docs_never_count() {
        assert_never_counts(&[
            "README.md",
            "docs/guide.mdx",
            "docs/index.rst",
            "src/billing/notes.txt",
            "src/CHANGES.MD",
        ]);
    }

    #[test]
    fn test_config_never_counts() {
        assert_never_counts(&[
            "package.json",
            "config/app.yaml",
            "deploy/values.yml",
            "pyproject.toml",
            "tox.ini",
            ".env",
            "src/.eslintrc",
            "apps/web/.prettierrc.js",
            ".github/workflows/release.sh",
            ".husky/pre-commit",
            "vite.config.ts",
            "apps/web/next.config.mjs",
        ]);
    }

    #[test]
    fn test_lockfiles_never_count() {
        assert_never_counts(&[
            "Cargo.lock",
            "web/yarn.lock",
            "poetry.lock",
            "go.sum",
            "services/api/go.sum",
            "bun.lockb",
        ]);
    }

    #[test]
    fn test_generated_output_never_counts() {
        assert_never_counts(&[
            "node_modules/react/index.js",
            "packages/ui/dist/index.js",
            "build/app.js",
            "target/debug/build/out.rs",
            "vendor/github.com/pkg/errors/errors.go",
            "app/__pycache__/models.cpython-312.pyc",
            ".next/server/page.js",
            "public/jquery.min.js",
            "src/schema.generated.ts",
        ]);
    }

    /// A batch is noted in one write. One that changes nothing, the empty one
    /// included, writes nothing, and returns at once with the lock held
    /// elsewhere.
    #[test]
    fn test_a_batch_of_folders_is_noted_under_one_lock() {
        let dir = tempdir().unwrap();
        let none: [(&str, bool); 0] = [];
        record_folders(dir.path(), &key("s1"), &none);
        assert!(!ledger_path(dir.path(), &key("s1")).exists());

        let batch = [
            ("src/billing", false),
            ("services/auth", true),
            ("services/auth", false),
        ];
        record_folders(dir.path(), &key("s1"), &batch);
        assert_eq!(
            folders(dir.path(), &key("s1")),
            (
                vec!["services/auth".to_string()],
                vec!["src/billing".to_string()]
            )
        );

        let _held = lock(dir.path(), Duration::ZERO).expect("the lock");
        let started = Instant::now();
        record_folders(dir.path(), &key("s1"), &batch);
        assert!(started.elapsed() < LOCK_WAIT, "a no-op waited on the lock");
    }

    // ── the lock ─────────────────────────────────────────────────────────

    /// A writer that cannot take the lock in time drops its change: it never
    /// writes without the lock. This waits out the real [`LOCK_WAIT`].
    #[test]
    fn test_a_write_that_cannot_take_the_lock_is_dropped() {
        let dir = tempdir().unwrap();
        let _held = lock(dir.path(), Duration::ZERO).expect("the lock");

        record(dir.path(), &key("s1"), ["Adopt RS256"]);

        assert!(!ledger_path(dir.path(), &key("s1")).exists());
    }

    /// A writer that finds the lock held waits for it, then writes.
    #[test]
    fn test_a_write_waits_for_the_lock_holder() {
        let dir = tempdir().unwrap();
        let held = lock(dir.path(), Duration::ZERO).expect("the lock");
        let path = dir.path().to_path_buf();
        let writer = std::thread::spawn(move || {
            update_within(&path, &key("s1"), Duration::from_secs(30), |l| {
                l.briefed.insert("Adopt RS256".to_string());
            });
        });
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            briefed(dir.path(), &key("s1")).is_empty(),
            "written while the lock was held"
        );

        drop(held);
        writer.join().unwrap();

        assert_eq!(briefed(dir.path(), &key("s1")), ["Adopt RS256"]);
    }

    /// Something other than a file where the lock belongs defeats the lock,
    /// and the change is dropped rather than written unlocked.
    #[test]
    fn test_an_unopenable_lock_file_drops_the_write() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join(LOCK_FILE_NAME)).unwrap();

        record(dir.path(), &key("s1"), ["Adopt RS256"]);

        assert!(!ledger_path(dir.path(), &key("s1")).exists());
    }

    /// Many hook runs writing at once: every decision lands and the file
    /// stays whole. Without the lock they race through `write_secure`'s one
    /// `.tmp` path, and decisions or the whole file are lost.
    #[test]
    fn test_concurrent_writers_lose_neither_the_file_nor_a_decision() {
        let dir = tempdir().unwrap();
        let writers = 16;
        let start = std::sync::Arc::new(std::sync::Barrier::new(writers));
        let handles: Vec<_> = (0..writers)
            .map(|i| {
                let path = dir.path().to_path_buf();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    update_within(&path, &key("s1"), Duration::from_secs(30), |l| {
                        l.briefed.insert(format!("Decision {i}"));
                    });
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let text = std::fs::read_to_string(ledger_path(dir.path(), &key("s1"))).unwrap();
        let ledger: BriefLedger = serde_json::from_str(&text).expect("a whole record");
        assert_eq!(ledger.briefed.len(), writers);
    }
}
