//! Fingerprinting and on-disk caching of a built [`ScopeIndex`].
//!
//! # Design
//!
//! The index must be cheap enough to sit in front of an interactive command, so
//! it is cached; it must never be *stale*, so the cache is keyed by a digest of
//! the rule files' **contents** rather than by a timestamp or a TTL.
//!
//! An earlier version keyed on each file's path, byte length and modification
//! time. That is not a content identity: a rewrite that preserves both size and
//! mtime — `cp -p`, `rsync -t`, an unpacked archive, or any edit at all on a
//! filesystem with coarse mtime granularity, such as a network mount — reused an
//! index built from text that no longer existed. For a governance tool, silently
//! selecting rules from deleted text is the worst available failure, and the
//! measured cost of reading 425 files to hash them is a few milliseconds against
//! a build of roughly 130.
//!
//! So [`crate::rules::read_rule_sources`] reads the files once and hands back
//! both their text and a digest of it. On a hit the parse and the index build
//! are skipped, which is where the time actually goes; on a miss the bytes are
//! already in hand. Hashing and parsing therefore see one snapshot, so the cache
//! can never be keyed to bytes that were not the ones indexed.
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

/// Load a cached index, if one exists and still matches `content_digest`.
///
/// Returns `None` for every failure mode — absent, unreadable, unparseable,
/// written by an older format, or stale — because all of them have the same
/// remedy: rebuild.
pub fn load(rules_dir: &Path, content_digest: &str) -> Option<ScopeIndex> {
    let path = cache_path(rules_dir)?;
    let text = std::fs::read_to_string(path).ok()?;
    let index: ScopeIndex = serde_json::from_str(&text).ok()?;
    if index.format_version != INDEX_FORMAT_VERSION || index.content_digest != content_digest {
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
    // `ScopeIndex` is plain owned data with no map keys that can fail to
    // serialize, so this cannot return `Err`. Asserting that is honest about
    // the contract, and leaves no dead error arm pretending to be reachable.
    let json = serde_json::to_string(index)
        .expect("scope index is serializable — this is a programmer error");
    // `cache_path` builds `<config>/scope-index/<key>.json`, so a parent always
    // exists. Naming that is honest about the contract and leaves no dead arm.
    let parent = path
        .parent()
        .expect("cache path always has a parent — this is a programmer error");
    if std::fs::create_dir_all(parent).is_err() {
        return;
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
#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::{tempdir, TempDir};

    use crate::rules::scope::index::ScopeIndex;
    use crate::rules::{parse_rule_sources, read_rule_sources, rules_dir};
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
        let report = parse_rule_sources(read_rule_sources(root).unwrap());
        let key = report.digest.clone();
        ScopeIndex::build(&report, root, key)
    }

    /// With no resolvable config directory there is nowhere to cache. Both the
    /// single-entry write and the prune must degrade quietly rather than fail:
    /// the only cost is a rebuild.
    #[test]
    fn test_cache_is_inert_without_a_resolvable_config_directory() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // A relative `ACTUAL_CONFIG` is rejected by the config loader, so
        // `config_dir()` errors and every cache path resolves to `None`.
        let _guard = EnvGuard::set("ACTUAL_CONFIG", "relative/config.yaml");
        let _clear = EnvGuard::remove("ACTUAL_CONFIG_DIR");

        let repo = seed(&[("a.md", DOC)]);
        let dir = rules_dir(repo.path());

        assert!(cache_dir().is_none());
        assert!(cache_path(&dir).is_none());
        // Must not panic, and must report nothing pruned.
        store(&dir, &build(repo.path()));
        clear(&dir);
        assert_eq!(clear_all(), 0);
        assert!(load(&dir, "anything").is_none());
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

        let loaded = load(&dir, &index.content_digest).expect("cache hit");
        assert_eq!(loaded, index);
    }

    #[test]
    fn test_load_misses_when_the_digest_no_longer_matches() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _clear = EnvGuard::remove("ACTUAL_CONFIG");

        let repo = seed(&[("a.md", DOC)]);
        let dir = rules_dir(repo.path());
        store(&dir, &build(repo.path()));

        assert!(load(&dir, "some-other-digest").is_none());
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

        assert!(load(&dir, &index.content_digest).is_none());
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
        assert!(load(&dir, &index.content_digest).is_some());

        clear(&dir);
        assert!(load(&dir, &index.content_digest).is_none());
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
        assert!(load(&dir_a, &index_a.content_digest).is_some());
        assert!(load(&dir_b, &index_b.content_digest).is_some());

        assert_eq!(clear_all(), 2);
        assert!(load(&dir_a, &index_a.content_digest).is_none());
        assert!(load(&dir_b, &index_b.content_digest).is_none());
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
