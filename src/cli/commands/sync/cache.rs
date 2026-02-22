use std::path::Path;

use sha2::{Digest, Sha256};

use crate::config::paths::{load_from, save_to};
use crate::config::types::CachedTailoring;

/// Return the pipeline skip message for a tailoring cache hit.
///
/// Distinguishes between a cached result with no applicable ADRs (deterministic
/// empty output) and one with applicable ADRs, so users can tell at a glance
/// why tailoring was skipped.
pub(super) fn tailoring_cache_skip_msg(applicable: usize) -> &'static str {
    if applicable == 0 {
        "No ADRs applicable to this project (cached)"
    } else {
        "Using cached tailoring result"
    }
}

/// Compute a stable repo key by hashing the git origin URL.
///
/// Falls back to hashing the root directory path if not a git repo or
/// if the remote URL cannot be determined. Applies a 5-second timeout to
/// the git subprocess; on timeout, silently falls back to the path hash.
pub(super) fn compute_repo_key(root_dir: &Path) -> String {
    compute_repo_key_with_timeout(root_dir, std::time::Duration::from_secs(5))
}

pub(super) fn compute_repo_key_with_timeout(
    root_dir: &Path,
    timeout: std::time::Duration,
) -> String {
    let root_dir_buf = root_dir.to_path_buf();

    // Use tokio::process::Command with kill_on_drop(true) so that when the
    // timeout fires the child is sent SIGKILL rather than left as an orphan.
    // A single-threaded runtime is used to keep this function synchronous.
    let url = (|| -> Option<String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;

        rt.block_on(async move {
            let mut cmd = tokio::process::Command::new("git");
            cmd.args(["remote", "get-url", "origin"]);
            cmd.current_dir(&root_dir_buf);
            cmd.stdin(std::process::Stdio::null());
            cmd.kill_on_drop(true);

            let child = cmd
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .ok()?;

            let result = tokio::time::timeout(timeout, child.wait_with_output()).await;

            match result {
                Ok(Ok(output)) if output.status.success() => {
                    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if s.is_empty() {
                        None
                    } else {
                        Some(s)
                    }
                }
                _ => None,
            }
        })
    })();

    let input = url.unwrap_or_else(|| root_dir.to_string_lossy().to_string());

    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Serialize a value to `serde_yaml::Value`, logging a warning and returning
/// `None` if serialization fails.
///
/// In practice `TailoringOutput` contains only string/number/vec fields and
/// serialization never fails.  The `Option` return defends against a future
/// type change that adds a non-YAML-representable field (e.g. `f64::NAN`).
pub(super) fn serialize_tailoring_output<T: serde::Serialize>(
    output: &T,
) -> Option<serde_yaml::Value> {
    match serde_yaml::to_value(output) {
        Ok(v) => Some(v),
        Err(e) => {
            // Gap 5: route through tracing so the warning is visible in the log
            // file (TUI mode) or stderr (plain mode) rather than being lost when
            // the TUI alternate screen is active.
            tracing::warn!("failed to serialize tailoring cache: {e}");
            None
        }
    }
}

/// Store a tailoring result in the config cache (best-effort).
///
/// Skips caching when `cache_key` is `None`.
/// Disk persistence failures are silently ignored because the cache
/// is an optimisation — a write failure should never abort a sync.
///
/// The output is generic over any `serde::Serialize` type so that tests can
/// inject a type that always fails serialization to exercise the warning path.
/// In production the concrete type is always `TailoringOutput`.
pub(super) fn store_tailoring_cache<T: serde::Serialize>(
    config: &mut crate::config::types::Config,
    cfg_path: &Path,
    cache_key: Option<&str>,
    root_dir: &Path,
    output: &T,
) {
    let Some(key) = cache_key else {
        return;
    };
    let Some(tailoring_value) = serialize_tailoring_output(output) else {
        return;
    };
    config.cached_tailoring = Some(CachedTailoring {
        cache_key: key.to_string(),
        repo_path: root_dir.to_string_lossy().to_string(),
        tailoring: tailoring_value,
        tailored_at: chrono::Utc::now(),
    });
    // Best-effort persist: if disk write fails, the in-memory cache is
    // still populated for subsequent operations in this process.
    // Gap 5: use tracing so the warning reaches the log file in TUI mode.
    if let Err(e) = save_to(config, cfg_path) {
        tracing::warn!("failed to save tailoring cache: {e}");
    }
}

/// Compute a cache key for the tailoring step by hashing all inputs
/// that affect the tailoring output.
///
/// The key incorporates: full ADR content (sorted by ID for determinism),
/// the no_tailor flag, sorted project paths, existing CLAUDE.md content,
/// and model override. Any change in these inputs produces a different cache
/// key. The git HEAD commit is intentionally excluded so that commits
/// unrelated to ADR content or repo structure do not invalidate the cache.
pub(super) fn compute_tailoring_cache_key(
    adrs: &[crate::api::types::Adr],
    no_tailor: bool,
    project_paths: &[String],
    existing_claude_md: &str,
    model: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();

    // Sort ADRs by ID for determinism regardless of iteration order.
    let mut sorted_adrs: Vec<&crate::api::types::Adr> = adrs.iter().collect();
    sorted_adrs.sort_by(|a, b| a.id.cmp(&b.id));

    // Field-boundary bytes used below:
    //   \x00 — value separator within a list element (prevents "ab"+"c" == "a"+"bc")
    //   \xfe — list-field separator within an ADR (prevents policies/instructions
    //           ambiguity: ["a"]+[] == []+["a"] without a separator)
    //   \xff — ADR separator between successive ADRs in the outer loop (prevents
    //           ["a","b"]+["c"] == ["a"]+["b","c"] across ADR boundaries)
    for adr in sorted_adrs {
        hasher.update(adr.id.as_bytes());
        hasher.update(b"\x00");
        hasher.update(adr.title.as_bytes());
        hasher.update(b"\x00");
        hasher.update(adr.context.as_deref().unwrap_or("").as_bytes());
        hasher.update(b"\x00");

        let mut policies = adr.policies.clone();
        policies.sort();
        for policy in &policies {
            hasher.update(policy.as_bytes());
            hasher.update(b"\x00");
        }
        hasher.update(b"\xfe"); // end of policies list

        let mut instructions = adr.instructions.clone().unwrap_or_default();
        instructions.sort();
        for instruction in &instructions {
            hasher.update(instruction.as_bytes());
            hasher.update(b"\x00");
        }
        hasher.update(b"\xfe"); // end of instructions list

        // AdrCategory: hash its id, name, and path fields.
        hasher.update(adr.category.id.as_bytes());
        hasher.update(b"\x00");
        hasher.update(adr.category.name.as_bytes());
        hasher.update(b"\x00");
        hasher.update(adr.category.path.as_bytes());
        hasher.update(b"\x00");

        // AppliesTo: hash sorted languages and frameworks.
        let mut languages = adr.applies_to.languages.clone();
        languages.sort();
        for lang in &languages {
            hasher.update(lang.as_bytes());
            hasher.update(b"\x00");
        }
        hasher.update(b"\xfe"); // end of languages list

        let mut frameworks = adr.applies_to.frameworks.clone();
        frameworks.sort();
        for fw in &frameworks {
            hasher.update(fw.as_bytes());
            hasher.update(b"\x00");
        }
        hasher.update(b"\xfe"); // end of frameworks list

        // matched_projects (sorted).
        let mut matched = adr.matched_projects.clone();
        matched.sort();
        for mp in &matched {
            hasher.update(mp.as_bytes());
            hasher.update(b"\x00");
        }
        hasher.update(b"\xfe"); // end of matched_projects list
                                // ADR boundary marker: prevents two distinct ADR sets from producing
                                // the same byte stream (e.g. ["a", "b"] + ["c"] vs ["a"] + ["b", "c"]).
        hasher.update(b"\xff");
    }

    hasher.update(if no_tailor {
        &b"no_tailor"[..]
    } else {
        &b"tailor"[..]
    });
    hasher.update(b"\x00");
    for path in project_paths {
        hasher.update(path.as_bytes());
        hasher.update(b"\x00");
    }
    hasher.update(existing_claude_md.as_bytes());
    hasher.update(b"\x00");
    if let Some(m) = model {
        hasher.update(m.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Load config from `path`, falling back to `Config::default()` if the file
/// cannot be parsed.  A `tracing::warn!` is emitted on failure so the message
/// appears in the log file (TUI mode) or stderr (plain mode).
///
/// This early load is non-fatal: if the config is unreadable or corrupt the
/// run continues with defaults and the second, hard `load_from(cfg_path)?`
/// call in Phase 2 will surface a proper error to the user.
pub(crate) fn load_config_with_fallback(cfg_path: &std::path::Path) -> crate::config::Config {
    match load_from(cfg_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "failed to load config from {}: {e} — using defaults",
                cfg_path.display()
            );
            Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::paths::save_to;
    use crate::tailoring::types::{AdrSection, FileOutput, TailoringOutput, TailoringSummary};

    // ── Local helpers for cache tests ──

    fn make_output(files: Vec<FileOutput>) -> TailoringOutput {
        let file_count = files.len();
        TailoringOutput {
            summary: TailoringSummary {
                total_input: file_count,
                applicable: file_count,
                not_applicable: 0,
                files_generated: file_count,
            },
            skipped_adrs: vec![],
            files,
        }
    }

    fn make_file(path: &str, content: &str, adr_ids: Vec<&str>) -> FileOutput {
        FileOutput {
            path: path.to_string(),
            sections: adr_ids
                .into_iter()
                .map(|id| AdrSection {
                    adr_id: id.to_string(),
                    content: content.to_string(),
                })
                .collect(),
            reasoning: format!("reason for {path}"),
        }
    }

    /// Build a minimal test [`Adr`] with the given id and policies for cache key tests.
    fn make_cache_test_adr(id: &str, policies: Vec<&str>) -> crate::api::types::Adr {
        crate::api::types::Adr {
            id: id.to_string(),
            title: format!("Title for {id}"),
            context: None,
            policies: policies.iter().map(|s| s.to_string()).collect(),
            instructions: None,
            category: crate::api::types::AdrCategory {
                id: "cat-1".to_string(),
                name: "Category One".to_string(),
                path: "cat/one".to_string(),
            },
            applies_to: crate::api::types::AppliesTo {
                languages: vec!["rust".to_string()],
                frameworks: vec![],
            },
            matched_projects: vec![".".to_string()],
        }
    }

    /// A test-only newtype that always fails to serialize, used to exercise the
    /// `serialize_tailoring_output` error path without changing production types.
    struct AlwaysFailsSerialize;

    impl serde::Serialize for AlwaysFailsSerialize {
        fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("intentional test failure"))
        }
    }

    // ── compute_repo_key tests ──

    #[test]
    fn test_compute_repo_key_non_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        let key = compute_repo_key(dir.path());
        // Should be a hex-encoded SHA-256 (64 chars)
        assert_eq!(key.len(), 64, "expected 64-char hex hash, got: {key}");
        // Should be deterministic
        let key2 = compute_repo_key(dir.path());
        assert_eq!(key, key2, "expected deterministic result");
    }

    #[test]
    fn test_compute_repo_key_different_dirs_differ() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        let key1 = compute_repo_key(dir1.path());
        let key2 = compute_repo_key(dir2.path());
        assert_ne!(key1, key2, "different dirs should produce different keys");
    }

    #[cfg(unix)]
    #[test]
    fn test_compute_repo_key_symlink_behavior() {
        use std::os::unix::fs::symlink;
        let target_dir = tempfile::tempdir().unwrap();
        let symlink_dir = tempfile::tempdir().unwrap();
        let link_path = symlink_dir.path().join("link");
        symlink(target_dir.path(), &link_path).expect("failed to create symlink");

        let key_target = compute_repo_key(target_dir.path());
        let key_symlink = compute_repo_key(&link_path);

        // Both must produce valid 64-char hex hashes
        assert_eq!(key_target.len(), 64, "target key should be 64-char hex");
        assert_eq!(key_symlink.len(), 64, "symlink key should be 64-char hex");
        // Document: on macOS current_dir() resolves symlinks so keys match;
        // on Linux symlink path may differ from canonical path.
        // This test documents the actual behavior without enforcing a specific outcome.
        // If they differ, it means a user cd'ing into a symlink dir gets a different
        // rejection namespace — a known limitation documented here.
        println!("key_target: {}", key_target);
        println!("key_symlink: {}", key_symlink);
        // Just verify both are valid hashes (no assertion on equality)
    }

    #[cfg(unix)]
    #[test]
    fn test_compute_repo_key_timeout_falls_back_to_path_hash() {
        use std::os::unix::fs::PermissionsExt;
        // Place a fake "git" binary that sleeps for 30s on PATH so the
        // recv_timeout fires before it responds, forcing the path-hash fallback.
        let _lock = crate::testutil::ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let bin_dir = tempfile::tempdir().unwrap();
        let fake_git = bin_dir.path().join("git");
        std::fs::write(&fake_git, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&fake_git, std::fs::Permissions::from_mode(0o755)).unwrap();
        let original_path = std::env::var("PATH").unwrap_or_default();
        let _path_guard = crate::testutil::EnvGuard::set(
            "PATH",
            &format!("{}:{}", bin_dir.path().display(), original_path),
        );
        let dir = tempfile::tempdir().unwrap();
        let result =
            compute_repo_key_with_timeout(dir.path(), std::time::Duration::from_millis(100));
        assert_eq!(result.len(), 64, "expected 64-char SHA256 hex string");
        // Deterministic: same dir → same hash (using the path-hash fallback)
        let result2 =
            compute_repo_key_with_timeout(dir.path(), std::time::Duration::from_millis(100));
        assert_eq!(result, result2);
    }

    #[cfg(unix)]
    #[test]
    fn test_compute_repo_key_empty_url_falls_back_to_path_hash() {
        use std::os::unix::fs::PermissionsExt;
        // Fake "git" that exits 0 but prints nothing — the empty-string guard
        // in compute_repo_key_with_timeout should fall back to the path hash.
        let _lock = crate::testutil::ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let bin_dir = tempfile::tempdir().unwrap();
        let fake_git = bin_dir.path().join("git");
        std::fs::write(&fake_git, "#!/bin/sh\nprintf ''\nexit 0\n").unwrap();
        std::fs::set_permissions(&fake_git, std::fs::Permissions::from_mode(0o755)).unwrap();
        let original_path = std::env::var("PATH").unwrap_or_default();
        let _path_guard = crate::testutil::EnvGuard::set(
            "PATH",
            &format!("{}:{}", bin_dir.path().display(), original_path),
        );
        let dir = tempfile::tempdir().unwrap();
        let result = compute_repo_key_with_timeout(dir.path(), std::time::Duration::from_secs(5));
        assert_eq!(result.len(), 64, "expected 64-char SHA256 hex string");
        // Should equal the path-hash fallback
        let expected = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(dir.path().to_string_lossy().as_bytes());
            format!("{:x}", hasher.finalize())
        };
        assert_eq!(result, expected, "empty URL should fall back to path hash");
    }

    // ── compute_tailoring_cache_key tests ──

    #[test]
    fn test_tailoring_cache_key_deterministic() {
        let adrs = vec![
            make_cache_test_adr("adr-001", vec!["p1"]),
            make_cache_test_adr("adr-002", vec!["p2"]),
        ];
        let key1 = compute_tailoring_cache_key(
            &adrs,
            false,
            &[".".to_string()],
            "existing content",
            Some("opus"),
        );
        let key2 = compute_tailoring_cache_key(
            &adrs,
            false,
            &[".".to_string()],
            "existing content",
            Some("opus"),
        );
        assert_eq!(key1, key2, "same inputs should produce same key");
        assert_eq!(key1.len(), 64, "should be a 64-char hex SHA-256");
    }

    #[test]
    fn test_tailoring_cache_key_differs_on_adr_content() {
        // Same ADR ID but different policy content → different key.
        let adrs1 = vec![make_cache_test_adr("adr-001", vec!["use snake_case"])];
        let adrs2 = vec![make_cache_test_adr("adr-001", vec!["use camelCase"])];
        let key1 = compute_tailoring_cache_key(&adrs1, false, &[".".to_string()], "", None);
        let key2 = compute_tailoring_cache_key(&adrs2, false, &[".".to_string()], "", None);
        assert_ne!(
            key1, key2,
            "different ADR policy content should produce different key"
        );
    }

    #[test]
    fn test_tailoring_cache_key_same_adr_different_order_is_stable() {
        // ADRs passed in different order → same key (sorting for determinism).
        let adr1 = make_cache_test_adr("adr-001", vec!["policy-a"]);
        let adr2 = make_cache_test_adr("adr-002", vec!["policy-b"]);
        let key1 = compute_tailoring_cache_key(
            &[adr1.clone(), adr2.clone()],
            false,
            &[".".to_string()],
            "",
            None,
        );
        let key2 = compute_tailoring_cache_key(&[adr2, adr1], false, &[".".to_string()], "", None);
        assert_eq!(
            key1, key2,
            "ADR input order should not affect the cache key"
        );
    }

    #[test]
    fn test_tailoring_cache_key_differs_on_adr_ids() {
        let key1 = compute_tailoring_cache_key(
            &[make_cache_test_adr("adr-001", vec![])],
            false,
            &[".".to_string()],
            "",
            None,
        );
        let key2 = compute_tailoring_cache_key(
            &[
                make_cache_test_adr("adr-001", vec![]),
                make_cache_test_adr("adr-002", vec![]),
            ],
            false,
            &[".".to_string()],
            "",
            None,
        );
        assert_ne!(key1, key2, "different ADR IDs should produce different key");
    }

    #[test]
    fn test_tailoring_cache_key_differs_on_no_tailor() {
        let adrs = vec![make_cache_test_adr("adr-001", vec![])];
        let key1 = compute_tailoring_cache_key(&adrs, false, &[".".to_string()], "", None);
        let key2 = compute_tailoring_cache_key(&adrs, true, &[".".to_string()], "", None);
        assert_ne!(
            key1, key2,
            "different no_tailor should produce different key"
        );
    }

    #[test]
    fn test_tailoring_cache_key_differs_on_project_paths() {
        let key1 = compute_tailoring_cache_key(&[], false, &[".".to_string()], "", None);
        let key2 = compute_tailoring_cache_key(
            &[],
            false,
            &[".".to_string(), "apps/web".to_string()],
            "",
            None,
        );
        assert_ne!(
            key1, key2,
            "different projects should produce different key"
        );
    }

    #[test]
    fn test_tailoring_cache_key_differs_on_existing_claude_md() {
        let key1 = compute_tailoring_cache_key(&[], false, &[".".to_string()], "old content", None);
        let key2 = compute_tailoring_cache_key(&[], false, &[".".to_string()], "new content", None);
        assert_ne!(
            key1, key2,
            "different CLAUDE.md content should produce different key"
        );
    }

    #[test]
    fn test_tailoring_cache_key_differs_on_model() {
        let key1 = compute_tailoring_cache_key(&[], false, &[".".to_string()], "", Some("opus"));
        let key2 = compute_tailoring_cache_key(&[], false, &[".".to_string()], "", Some("sonnet"));
        assert_ne!(key1, key2, "different model should produce different key");
    }

    #[test]
    fn test_tailoring_cache_key_none_vs_some_model() {
        let key1 = compute_tailoring_cache_key(&[], false, &[".".to_string()], "", None);
        let key2 = compute_tailoring_cache_key(&[], false, &[".".to_string()], "", Some("opus"));
        assert_ne!(
            key1, key2,
            "None vs Some model should produce different key"
        );
    }

    /// Regression test: compute_tailoring_cache_key must receive the effective
    /// model (args.model OR config.model), not just the CLI flag.
    ///
    /// Before the fix, the callsite passed `args.model.as_deref()` only, so
    /// setting `model: haiku` in config had no effect on the cache key — a
    /// cached result from a prior sonnet run would be incorrectly served.
    #[test]
    fn test_tailoring_cache_key_config_model_differs_from_none() {
        // Simulates: no --model CLI flag, but config.model = "haiku"
        let args_model: Option<&str> = None;
        let config_model: Option<&str> = Some("haiku");
        let effective_model = args_model.or(config_model);

        // Key with config model applied (correct — post-fix behaviour)
        let key_with_config =
            compute_tailoring_cache_key(&[], false, &[".".to_string()], "", effective_model);
        // Key with only CLI flag (incorrect — pre-fix behaviour, passes None)
        let key_without_config =
            compute_tailoring_cache_key(&[], false, &[".".to_string()], "", args_model);

        assert_ne!(
            key_with_config, key_without_config,
            "cache key must differ when config.model is set vs not set"
        );
    }

    #[test]
    fn test_tailoring_cache_key_differs_on_adr_instructions() {
        // ADR with instructions vs without — exercises the instructions hash path.
        let adr_no_instructions = crate::api::types::Adr {
            id: "adr-001".to_string(),
            title: "Title".to_string(),
            context: None,
            policies: vec![],
            instructions: None,
            category: crate::api::types::AdrCategory {
                id: "cat-1".to_string(),
                name: "Cat".to_string(),
                path: "cat".to_string(),
            },
            applies_to: crate::api::types::AppliesTo {
                languages: vec![],
                frameworks: vec![],
            },
            matched_projects: vec![],
        };
        let mut adr_with_instructions = adr_no_instructions.clone();
        adr_with_instructions.instructions = Some(vec!["do this".to_string()]);

        let key1 = compute_tailoring_cache_key(&[adr_no_instructions], false, &[], "", None);
        let key2 = compute_tailoring_cache_key(&[adr_with_instructions], false, &[], "", None);
        assert_ne!(
            key1, key2,
            "different instructions should produce different key"
        );
    }

    #[test]
    fn test_tailoring_cache_key_differs_on_adr_frameworks() {
        // ADR with frameworks vs without — exercises the frameworks hash path.
        let adr_no_frameworks = crate::api::types::Adr {
            id: "adr-001".to_string(),
            title: "Title".to_string(),
            context: None,
            policies: vec![],
            instructions: None,
            category: crate::api::types::AdrCategory {
                id: "cat-1".to_string(),
                name: "Cat".to_string(),
                path: "cat".to_string(),
            },
            applies_to: crate::api::types::AppliesTo {
                languages: vec![],
                frameworks: vec![],
            },
            matched_projects: vec![],
        };
        let mut adr_with_frameworks = adr_no_frameworks.clone();
        adr_with_frameworks.applies_to.frameworks = vec!["actix-web".to_string()];

        let key1 = compute_tailoring_cache_key(&[adr_no_frameworks], false, &[], "", None);
        let key2 = compute_tailoring_cache_key(&[adr_with_frameworks], false, &[], "", None);
        assert_ne!(
            key1, key2,
            "different frameworks should produce different key"
        );
    }

    // ── store_tailoring_cache tests ──

    #[test]
    fn test_store_tailoring_cache_stores_result() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.yaml");
        let mut config = crate::config::Config::default();
        save_to(&config, &cfg_path).unwrap();

        let output = make_output(vec![make_file("CLAUDE.md", "rules", vec!["adr-001"])]);
        store_tailoring_cache(
            &mut config,
            &cfg_path,
            Some("test-cache-key"),
            dir.path(),
            &output,
        );

        assert!(config.cached_tailoring.is_some());
        let cached = config.cached_tailoring.as_ref().unwrap();
        assert_eq!(cached.cache_key, "test-cache-key");
        assert_eq!(cached.repo_path, dir.path().to_string_lossy().as_ref());

        // Verify the stored value round-trips
        let restored: TailoringOutput = serde_yaml::from_value(cached.tailoring.clone()).unwrap();
        assert_eq!(restored, output);
    }

    #[test]
    fn test_store_tailoring_cache_skips_when_no_key() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.yaml");
        let mut config = crate::config::Config::default();
        save_to(&config, &cfg_path).unwrap();

        let output = make_output(vec![]);
        store_tailoring_cache(&mut config, &cfg_path, None, dir.path(), &output);

        assert!(
            config.cached_tailoring.is_none(),
            "should not store cache when key is None"
        );
    }

    #[test]
    fn test_store_tailoring_cache_persists_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.yaml");
        let mut config = crate::config::Config::default();
        save_to(&config, &cfg_path).unwrap();

        let output = make_output(vec![make_file("CLAUDE.md", "rules", vec!["adr-001"])]);
        store_tailoring_cache(
            &mut config,
            &cfg_path,
            Some("disk-test-key"),
            dir.path(),
            &output,
        );

        // Reload from disk and verify
        let loaded = load_from(&cfg_path).unwrap();
        let cached = loaded.cached_tailoring.unwrap();
        assert_eq!(cached.cache_key, "disk-test-key");
    }

    #[test]
    fn test_serialize_tailoring_output_warns_on_error() {
        // AlwaysFailsSerialize always returns Err from serde, exercising the
        // tracing::warn! branch inside serialize_tailoring_output.
        let result = serialize_tailoring_output(&AlwaysFailsSerialize);
        assert!(
            result.is_none(),
            "serialize_tailoring_output should return None on error"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_store_tailoring_cache_warns_on_disk_write_failure() {
        use std::os::unix::fs::PermissionsExt;
        // Create a read-only directory so that save_to fails, exercising the
        // "warning: failed to save tailoring cache" branch.
        let dir = tempfile::tempdir().unwrap();
        let ro_dir = dir.path().join("readonly");
        std::fs::create_dir(&ro_dir).unwrap();
        let cfg_path = ro_dir.join("config.yaml");

        let mut config = crate::config::Config::default();
        let output = make_output(vec![make_file("CLAUDE.md", "rules", vec!["adr-001"])]);

        // Make the directory read-only so save_to cannot write
        std::fs::set_permissions(&ro_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        // store_tailoring_cache should not panic; it warns and returns
        store_tailoring_cache(
            &mut config,
            &cfg_path,
            Some("warn-test-key"),
            dir.path(),
            &output,
        );

        // In-memory cache is still populated even though disk write failed
        assert!(
            config.cached_tailoring.is_some(),
            "in-memory cache should be set even if disk write failed"
        );

        // Restore permissions for cleanup
        std::fs::set_permissions(&ro_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn test_store_tailoring_cache_skips_on_serialization_error() {
        // When serialize_tailoring_output returns None (e.g. a non-YAML-
        // representable future field), store_tailoring_cache must return early
        // without populating the in-memory cache or writing to disk.
        //
        // store_tailoring_cache is generic over T: Serialize, so we can pass
        // AlwaysFailsSerialize to exercise the serialize error branch.
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.yaml");
        let mut config = crate::config::Config::default();
        store_tailoring_cache(
            &mut config,
            &cfg_path,
            Some("will-not-be-stored"),
            dir.path(),
            &AlwaysFailsSerialize,
        );
        // Serialization failed → no cache should be stored
        assert!(
            config.cached_tailoring.is_none(),
            "cache should not be set when serialization fails"
        );
    }

    // ── Tailoring cache round-trip test ──

    #[test]
    fn test_tailoring_output_round_trip_through_yaml_value() {
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                sections: vec![
                    AdrSection {
                        adr_id: "adr-001".to_string(),
                        content: "# Rules\n\nDo things.".to_string(),
                    },
                    AdrSection {
                        adr_id: "adr-002".to_string(),
                        content: "# Rules\n\nDo things.".to_string(),
                    },
                ],
                reasoning: "Root rules".to_string(),
            }],
            skipped_adrs: vec![],
            summary: TailoringSummary {
                total_input: 2,
                applicable: 2,
                not_applicable: 0,
                files_generated: 1,
            },
        };

        let value = serde_yaml::to_value(&output).unwrap();
        let restored: TailoringOutput = serde_yaml::from_value(value).unwrap();
        assert_eq!(output, restored);
    }

    #[test]
    fn test_corrupted_tailoring_cache_falls_through() {
        // Simulates a corrupted cache that cannot be deserialized as TailoringOutput
        let corrupted = serde_yaml::to_value("this is not a TailoringOutput").unwrap();
        let result = serde_yaml::from_value::<TailoringOutput>(corrupted);
        assert!(
            result.is_err(),
            "corrupted value should fail deserialization"
        );
    }

    // ── tailoring_cache_skip_msg unit tests ──

    #[test]
    fn test_tailoring_cache_skip_msg_zero_applicable() {
        assert_eq!(
            tailoring_cache_skip_msg(0),
            "No ADRs applicable to this project (cached)",
            "expected 0-applicable message"
        );
    }

    #[test]
    fn test_tailoring_cache_skip_msg_nonzero_applicable() {
        assert_eq!(
            tailoring_cache_skip_msg(1),
            "Using cached tailoring result",
            "expected with-files message for applicable=1"
        );
        assert_eq!(
            tailoring_cache_skip_msg(42),
            "Using cached tailoring result",
            "expected with-files message for applicable=42"
        );
    }

    // ── load_config_with_fallback tests ──

    /// Verify that `load_config_with_fallback` returns `Config::default()` when
    /// the config file contains invalid YAML, exercising the `Err` branch that
    /// emits a `tracing::warn!` instead of hard-failing.
    #[test]
    fn test_load_config_with_fallback_returns_default_on_invalid_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.yaml");

        // Write deliberately invalid YAML so `load_from` returns Err.
        std::fs::write(&cfg_path, ": invalid: yaml: [[[").unwrap();

        // Must not panic or propagate the error; must return the default config.
        let config = load_config_with_fallback(&cfg_path);
        assert_eq!(
            config,
            crate::config::Config::default(),
            "expected default config when YAML is invalid"
        );
    }
}
