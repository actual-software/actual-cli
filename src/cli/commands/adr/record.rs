//! What `actual adr create --all` last did with each file under
//! `docs/adr/`.
//!
//! One record per organization and repository lives under the user's config
//! directory, so every checkout of the repository on this machine shares it.
//! It keeps a file's own draft from reading as its duplicate, remembers the
//! accepted files saved as drafts until they reach the default branch, and
//! lets the signed-in refresh leave out of its list a file a run already
//! sent.
//!
//! A missing, damaged or other-version record reads as empty, as a
//! teammate's machine does: a file an earlier save made a draft from reads
//! as that draft's look-alike and is skipped, and a draft waiting for the
//! default branch is never accepted from here, until `create --all
//! --allow-duplicate` sends the file and its path key returns that draft.
//!
//! Each change is one entry, written under the directory's lock from its
//! read to its rename, as [`super::super::brief_ledger`] writes, so two runs
//! at once keep each other's entries. A change that can't take the lock is
//! dropped, and the next run sends that file again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cli::commands::brief_ledger;

/// The record format this build reads and writes.
const FORMAT_VERSION: u32 = 1;

/// The directory under the config directory the records live in.
const DIR_NAME: &str = "adr-create-all";

/// How long a change waits for another run's write.
const LOCK_WAIT: Duration = Duration::from_millis(500);

/// What a run last did with one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Outcome {
    /// Saved as a cloud draft.
    Draft,
    /// Marked accepted and saved as a cloud draft, which a run accepts once
    /// the file is on the default branch.
    WaitsForMerge,
    /// Accepted, so Actual AI owns the decision now.
    Accepted,
    /// Left alone for a reason that holds until the file changes: its id or
    /// a look-alike in Actual AI, a status that doesn't govern, the check, or
    /// a refusal.
    Skipped,
}

/// One file's entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Entry {
    /// [`text_hash`] of the file when the outcome was recorded.
    pub(super) text: String,
    pub(super) outcome: Outcome,
    /// The decision the file was saved as, when it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) adr_id: Option<String>,
    pub(super) at: DateTime<Utc>,
}

impl Entry {
    pub(super) fn new(text: &str, outcome: Outcome, adr_id: Option<&str>) -> Self {
        Self {
            text: text_hash(text),
            outcome,
            adr_id: adr_id.map(str::to_string),
            at: Utc::now(),
        }
    }
}

/// Every file's entry, by its path under the repository root.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Record {
    format_version: u32,
    pub(super) files: BTreeMap<String, Entry>,
}

/// Whose record: the organization of the sign-in and the repository the run
/// sends to, named as `--repo` gives it or, without one, by the origin remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Key {
    pub(super) org_id: String,
    pub(super) repo: String,
}

/// The directory the records live in, under the user's config directory.
pub(super) fn dir() -> Option<PathBuf> {
    crate::config::paths::config_dir()
        .ok()
        .map(|dir| dir.join(DIR_NAME))
}

/// SHA-256 of `text`, in hex.
pub(super) fn text_hash(text: &str) -> String {
    hex(Sha256::digest(text.as_bytes()).as_slice())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn record_path(dir: &Path, key: &Key) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(key.org_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(key.repo.to_lowercase().as_bytes());
    dir.join(format!("{}.json", hex(hasher.finalize().as_slice())))
}

/// The record for `key`, or an empty one when it is missing, unreadable,
/// damaged or another format's. A read takes no lock: every write replaces
/// the file whole.
pub(super) fn load(dir: &Path, key: &Key) -> Record {
    std::fs::read_to_string(record_path(dir, key))
        .ok()
        .and_then(|text| serde_json::from_str::<Record>(&text).ok())
        .filter(|record| record.format_version == FORMAT_VERSION)
        .unwrap_or_default()
}

/// Set `path`'s entry in the record for `key`, best effort. Whether it was
/// written.
pub(super) fn save(dir: &Path, key: &Key, path: &str, entry: Entry) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let Some(_lock) = brief_ledger::lock(dir, LOCK_WAIT) else {
        return false;
    };
    let mut record = load(dir, key);
    record.format_version = FORMAT_VERSION;
    record.files.insert(path.to_string(), entry);
    serde_json::to_string(&record).is_ok_and(|json| {
        crate::config::paths::write_secure(&record_path(dir, key), json.as_bytes()).is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn key(repo: &str) -> Key {
        Key {
            org_id: "org".to_string(),
            repo: repo.to_string(),
        }
    }

    #[test]
    fn a_saved_entry_reads_back_for_its_key_alone() {
        let dir = tempdir().unwrap();
        let entry = Entry::new("text", Outcome::Draft, Some("id-1"));
        assert!(save(
            dir.path(),
            &key("o/r"),
            "docs/adr/a.md",
            entry.clone()
        ));
        assert!(save(
            dir.path(),
            &key("o/r"),
            "docs/adr/b.md",
            Entry::new("other", Outcome::Skipped, None)
        ));
        // The repository is named case-insensitively, as GitHub names it.
        let record = load(dir.path(), &key("O/R"));
        assert_eq!(record.files.len(), 2);
        assert_eq!(record.files["docs/adr/a.md"], entry);
        assert!(load(dir.path(), &key("o/other")).files.is_empty());
        let other_org = Key {
            org_id: "org-2".to_string(),
            ..key("o/r")
        };
        assert!(load(dir.path(), &other_org).files.is_empty());
    }

    #[test]
    fn a_damaged_or_other_version_record_reads_as_empty() {
        let dir = tempdir().unwrap();
        let path = record_path(dir.path(), &key("o/r"));
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(load(dir.path(), &key("o/r")), Record::default());
        std::fs::write(&path, r#"{"format_version":2,"files":{}}"#).unwrap();
        assert_eq!(load(dir.path(), &key("o/r")), Record::default());
        // A later save replaces it whole.
        assert!(save(
            dir.path(),
            &key("o/r"),
            "docs/adr/a.md",
            Entry::new("t", Outcome::Accepted, Some("id"))
        ));
        assert_eq!(load(dir.path(), &key("o/r")).files.len(), 1);
    }

    #[test]
    fn a_save_that_cannot_write_reports_it() {
        let dir = tempdir().unwrap();
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, "").unwrap();
        let entry = Entry::new("t", Outcome::Draft, None);
        assert!(!save(
            &blocker.join("sub"),
            &key("o/r"),
            "docs/adr/a.md",
            entry
        ));
    }

    #[test]
    fn a_save_while_another_run_holds_the_lock_is_dropped() {
        let dir = tempdir().unwrap();
        let _held = brief_ledger::lock(dir.path(), Duration::ZERO).unwrap();
        let entry = Entry::new("t", Outcome::Draft, None);
        assert!(!save(dir.path(), &key("o/r"), "docs/adr/a.md", entry));
        assert!(load(dir.path(), &key("o/r")).files.is_empty());
    }

    #[test]
    fn text_hash_is_hex_sha256() {
        assert_eq!(
            text_hash(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn dir_sits_under_the_config_directory() {
        let _lock = crate::testutil::ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _g1 = crate::testutil::EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 =
            crate::testutil::EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        assert_eq!(dir(), Some(home.path().join(DIR_NAME)));
    }
}
