//! Fingerprinting and on-disk caching of a built [`ScopeIndex`].
//!
//! # Design
//!
//! The index must be cheap enough to sit in front of an interactive command, so
//! it is cached; it must never be *stale*, so the cache is keyed by a
//! fingerprint of the rule files themselves rather than by a timestamp or a TTL.
//!
//! The fingerprint is a SHA-256 over each rule file's repository-relative path,
//! byte length and modification time, taken in sorted order, plus the index
//! format version. Computing it costs one `stat` per file and no reads, so the
//! validity check stays far cheaper than the build it guards. Any edit, add,
//! remove or rename changes it; a format-version bump invalidates every cached
//! index written by an older build.
//!
//! The cache lives under the user's config directory, **never** inside the
//! repository. `.actual/rules/` is committed, and writing a derived artifact
//! next to committed source invites it into a commit. One file is stored per
//! `rules_dir`, so [`clear_all`] is the prune for entries left by repositories
//! that are no longer in play.
//!
//! Every cache operation is best-effort. A cache that cannot be read, written
//! or parsed degrades to a rebuild, which is slower and always correct. Cache
//! I/O therefore returns no errors to the caller — there is nothing a caller
//! could usefully do with one.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::rules::scope::index::{ScopeIndex, INDEX_FORMAT_VERSION};

/// Subdirectory of the config directory holding cached indexes.
pub const CACHE_DIR_NAME: &str = "scope-index";

/// Directory holding cached indexes, under the user's config directory.
pub fn cache_dir() -> Option<PathBuf> {
    crate::config::paths::config_dir()
        .ok()
        .map(|dir| dir.join(CACHE_DIR_NAME))
}

/// Where the cache entry for the rule set under `rules_dir` lives.
///
/// The file is named for a hash of the absolute rules directory, so two
/// checkouts of the same repository cache independently and neither can be
/// confused for the other.
pub fn cache_path(rules_dir: &Path) -> Option<PathBuf> {
    let mut hasher = Sha256::new();
    hasher.update(rules_dir.as_os_str().as_encoded_bytes());
    let key = hex(&hasher.finalize());
    cache_dir().map(|dir| dir.join(format!("{key}.json")))
}

/// Fingerprint the rule files under `rules_dir`.
///
/// Stat-only: no file contents are read, so this stays cheap enough to run on
/// every invocation. A missing directory fingerprints as the empty rule set
/// rather than failing, matching [`crate::rules::load_rule_set`].
pub fn fingerprint(rules_dir: &Path) -> String {
    let mut entries: Vec<(String, u64, Option<u128>)> = Vec::new();
    if let Ok(dir) = std::fs::read_dir(rules_dir) {
        for entry in dir.flatten() {
            let path = entry.path();
            if !path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
            {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            entries.push((name, metadata.len(), mtime_nanos(&metadata)));
        }
    }
    entries.sort();

    let mut hasher = Sha256::new();
    hasher.update(format!("v{INDEX_FORMAT_VERSION}\n").as_bytes());
    for (name, len, mtime) in &entries {
        hasher.update(format!("{name}\0{len}\0{}\n", encode_mtime(*mtime)).as_bytes());
    }
    hex(&hasher.finalize())
}

/// Load a cached index, if one exists and still matches `fingerprint`.
///
/// Returns `None` for every failure mode — absent, unreadable, unparseable,
/// written by an older format, or stale — because all of them have the same
/// remedy: rebuild.
pub fn load(rules_dir: &Path, fingerprint: &str) -> Option<ScopeIndex> {
    let path = cache_path(rules_dir)?;
    let text = std::fs::read_to_string(path).ok()?;
    let index: ScopeIndex = serde_json::from_str(&text).ok()?;
    if index.format_version != INDEX_FORMAT_VERSION || index.fingerprint != fingerprint {
        return None;
    }
    Some(index)
}

/// Write `index` to the cache. Best-effort: failure is silent, and costs only a
/// rebuild next time.
pub fn store(rules_dir: &Path, index: &ScopeIndex) {
    let Some(path) = cache_path(rules_dir) else {
        return;
    };
    let Ok(json) = serde_json::to_string(index) else {
        return;
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let _ = std::fs::write(&path, json);
    // 0600, consistent with everything else the CLI writes into the config
    // directory. Rule text is not secret, but the directory's posture is uniform.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
}

/// Remove the cached index for `rules_dir`. Best-effort.
pub fn clear(rules_dir: &Path) {
    if let Some(path) = cache_path(rules_dir) {
        let _ = std::fs::remove_file(path);
    }
}

/// Remove every cached index. Best-effort.
///
/// One file is stored per `rules_dir` ever seen, so a machine that works in
/// many repositories accumulates stale entries. This is the prune. Returns
/// how many entries were present; an absent or unreadable cache is zero, not
/// an error.
pub fn clear_all() -> usize {
    let Some(dir) = cache_dir() else {
        return 0;
    };
    let count = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .flatten()
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
            })
            .count(),
        Err(_) => 0,
    };
    let _ = std::fs::remove_dir_all(&dir);
    count
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Nanoseconds since the Unix epoch, or `None` when the filesystem has no
/// mtime or the timestamp is before the epoch.
fn mtime_nanos(metadata: &std::fs::Metadata) -> Option<u128> {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
}

/// Encode one file's mtime for the fingerprint hash.
///
/// `None` contributes the constant `-1`, which weakens the fingerprint to
/// path+size rather than breaking it. Nanos stay `u128` so a far-future
/// mtime cannot wrap through an `i128` cast (year ~2262).
fn encode_mtime(mtime_nanos: Option<u128>) -> String {
    mtime_nanos
        .map(|ns| ns.to_string())
        .unwrap_or_else(|| "-1".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::{tempdir, TempDir};

    use crate::rules::scope::index::ScopeIndex;
    use crate::rules::{load_rule_set, rules_dir};
    use crate::testutil::{EnvGuard, ENV_MUTEX};

    const DOC: &str = "# T\n\nScope.\n\n### Rules\n\n- **R-A-001** MUST: a.\n";

    /// Helper: a repository root whose `.actual/rules/` holds `files`.
    fn seed(files: &[(&str, &str)]) -> TempDir {
        let root = tempdir().unwrap();
        let dir = rules_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        for (name, contents) in files {
            std::fs::write(dir.join(name), contents).unwrap();
        }
        root
    }

    /// Helper: an index built from the rule set at `root`.
    fn build(root: &Path) -> ScopeIndex {
        let report = load_rule_set(root).unwrap();
        ScopeIndex::build(&report, root, fingerprint(&rules_dir(root)))
    }

    // ── fingerprinting ───────────────────────────────────────────────────

    #[test]
    fn test_fingerprint_is_stable_for_an_unchanged_directory() {
        let root = seed(&[("a.md", DOC), ("b.md", DOC)]);
        let dir = rules_dir(root.path());
        assert_eq!(fingerprint(&dir), fingerprint(&dir));
    }

    #[test]
    fn test_fingerprint_changes_when_a_file_is_added_or_removed() {
        let root = seed(&[("a.md", DOC)]);
        let dir = rules_dir(root.path());
        let before = fingerprint(&dir);

        std::fs::write(dir.join("b.md"), DOC).unwrap();
        let added = fingerprint(&dir);
        assert_ne!(before, added);

        std::fs::remove_file(dir.join("b.md")).unwrap();
        assert_eq!(fingerprint(&dir), before);
    }

    /// The case a timestamp-only or TTL cache gets wrong: an edit that keeps
    /// the file the same length must still invalidate.
    #[test]
    fn test_fingerprint_changes_when_a_file_is_edited() {
        let root = seed(&[("a.md", DOC)]);
        let dir = rules_dir(root.path());
        let before = fingerprint(&dir);
        // A longer body, so the change is caught by size even where the
        // filesystem's modification time has coarse resolution.
        std::fs::write(dir.join("a.md"), format!("{DOC}\n- **R-A-002** MAY: b.\n")).unwrap();
        assert_ne!(fingerprint(&dir), before);
    }

    #[test]
    fn test_fingerprint_ignores_non_markdown_files() {
        let root = seed(&[("a.md", DOC)]);
        let dir = rules_dir(root.path());
        let before = fingerprint(&dir);
        std::fs::write(dir.join("notes.txt"), "irrelevant").unwrap();
        assert_eq!(fingerprint(&dir), before);
    }

    #[test]
    fn test_fingerprint_of_a_missing_directory_is_stable() {
        let root = tempdir().unwrap();
        let dir = rules_dir(root.path());
        assert_eq!(fingerprint(&dir), fingerprint(&dir));
        // An empty directory fingerprints the same as an absent one: both are
        // the empty rule set.
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(fingerprint(&dir), fingerprint(Path::new("/no/such/dir")));
    }

    #[test]
    fn test_fingerprint_distinguishes_two_directories_with_different_names() {
        let a = seed(&[("a.md", DOC)]);
        let b = seed(&[("b.md", DOC)]);
        assert_ne!(
            fingerprint(&rules_dir(a.path())),
            fingerprint(&rules_dir(b.path()))
        );
    }

    /// A far-future mtime must hash as its full u128 nanos, not wrap through
    /// an i128 cast. Missing mtime still encodes as `-1`.
    #[test]
    fn test_encode_mtime_does_not_truncate_past_i128_max() {
        assert_eq!(encode_mtime(None), "-1");
        assert_eq!(encode_mtime(Some(0)), "0");
        let beyond_i128 = i128::MAX as u128 + 1;
        assert_eq!(encode_mtime(Some(beyond_i128)), beyond_i128.to_string());
        assert!(!encode_mtime(Some(beyond_i128)).starts_with('-'));
    }

    // ── cache location ───────────────────────────────────────────────────

    /// The cache must never land inside the repository: `.actual/rules/` is
    /// committed, and a derived artifact beside committed source gets committed
    /// with it.
    #[test]
    fn test_cache_path_is_outside_the_repository() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _clear = EnvGuard::remove("ACTUAL_CONFIG");

        let repo = seed(&[("a.md", DOC)]);
        let path = cache_path(&rules_dir(repo.path())).unwrap();
        assert!(path.starts_with(home.path()));
        assert!(!path.starts_with(repo.path()));
        assert_eq!(path.parent().unwrap().file_name().unwrap(), CACHE_DIR_NAME);
    }

    #[test]
    fn test_cache_path_differs_per_rules_directory() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _clear = EnvGuard::remove("ACTUAL_CONFIG");

        let a = cache_path(Path::new("/repo-one/.actual/rules")).unwrap();
        let b = cache_path(Path::new("/repo-two/.actual/rules")).unwrap();
        assert_ne!(a, b);
    }

    // ── store and load ───────────────────────────────────────────────────

    #[test]
    fn test_store_then_load_returns_the_same_index() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _clear = EnvGuard::remove("ACTUAL_CONFIG");

        let repo = seed(&[("a.md", DOC)]);
        let dir = rules_dir(repo.path());
        let index = build(repo.path());
        store(&dir, &index);

        let loaded = load(&dir, &index.fingerprint).expect("cache hit");
        assert_eq!(loaded, index);
    }

    #[test]
    fn test_load_misses_when_the_fingerprint_no_longer_matches() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _clear = EnvGuard::remove("ACTUAL_CONFIG");

        let repo = seed(&[("a.md", DOC)]);
        let dir = rules_dir(repo.path());
        store(&dir, &build(repo.path()));

        assert!(load(&dir, "some-other-fingerprint").is_none());
    }

    #[test]
    fn test_load_misses_on_an_older_format_version() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _clear = EnvGuard::remove("ACTUAL_CONFIG");

        let repo = seed(&[("a.md", DOC)]);
        let dir = rules_dir(repo.path());
        let mut index = build(repo.path());
        index.format_version = INDEX_FORMAT_VERSION + 1;
        store(&dir, &index);

        assert!(load(&dir, &index.fingerprint).is_none());
    }

    #[test]
    fn test_load_misses_on_an_absent_or_corrupt_entry() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _clear = EnvGuard::remove("ACTUAL_CONFIG");

        let repo = seed(&[("a.md", DOC)]);
        let dir = rules_dir(repo.path());
        assert!(load(&dir, "anything").is_none());

        let path = cache_path(&dir).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load(&dir, "anything").is_none());
    }

    #[test]
    fn test_clear_removes_the_entry() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _clear = EnvGuard::remove("ACTUAL_CONFIG");

        let repo = seed(&[("a.md", DOC)]);
        let dir = rules_dir(repo.path());
        let index = build(repo.path());
        store(&dir, &index);
        assert!(load(&dir, &index.fingerprint).is_some());

        clear(&dir);
        assert!(load(&dir, &index.fingerprint).is_none());
        // Clearing an absent entry is a no-op, not a failure.
        clear(&dir);
    }

    /// One file per rules directory, so a machine that works in many
    /// repositories accumulates stale entries. `clear_all` is the prune.
    #[test]
    fn test_clear_all_removes_every_entry() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _clear = EnvGuard::remove("ACTUAL_CONFIG");

        let a = seed(&[("a.md", DOC)]);
        let b = seed(&[("b.md", DOC)]);
        let dir_a = rules_dir(a.path());
        let dir_b = rules_dir(b.path());
        let index_a = build(a.path());
        let index_b = build(b.path());
        store(&dir_a, &index_a);
        store(&dir_b, &index_b);
        assert!(load(&dir_a, &index_a.fingerprint).is_some());
        assert!(load(&dir_b, &index_b.fingerprint).is_some());

        assert_eq!(clear_all(), 2);
        assert!(load(&dir_a, &index_a.fingerprint).is_none());
        assert!(load(&dir_b, &index_b.fingerprint).is_none());
        // Clearing an empty cache is a no-op, not a failure.
        assert_eq!(clear_all(), 0);
    }

    /// Every cache operation is best-effort: an unwritable location degrades to
    /// a rebuild rather than failing the command.
    #[test]
    fn test_store_is_silent_when_the_cache_cannot_be_written() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let blocker = tempdir().unwrap();
        let file = blocker.path().join("not-a-dir");
        std::fs::write(&file, "x").unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", file.to_str().unwrap());
        let _clear = EnvGuard::remove("ACTUAL_CONFIG");

        let repo = seed(&[("a.md", DOC)]);
        let dir = rules_dir(repo.path());
        // Must not panic; the next load simply misses.
        store(&dir, &build(repo.path()));
        assert!(load(&dir, "anything").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn test_stored_cache_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _clear = EnvGuard::remove("ACTUAL_CONFIG");

        let repo = seed(&[("a.md", DOC)]);
        let dir = rules_dir(repo.path());
        store(&dir, &build(repo.path()));

        let mode = std::fs::metadata(cache_path(&dir).unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
