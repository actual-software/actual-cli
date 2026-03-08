use std::path::Path;

use sha2::{Digest, Sha256};

use crate::analysis::orchestrate::run_static_analysis;
use crate::analysis::types::RepoAnalysis;
use crate::config;
use crate::config::types::{CachedAnalysis, Config};
use crate::error::ActualError;

/// Whether the analysis result came from cache or was freshly computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheStatus {
    /// Cache hit — result deserialized from config.
    Hit,
    /// Cache miss — fresh static analysis was run.
    Miss,
    /// Force flag bypassed cache.
    ForcedMiss,
    /// Not a git repo — caching not applicable.
    Uncacheable,
}

impl CacheStatus {
    /// Human-readable label shown next to "Analysis complete" in the pipeline.
    pub fn label(self) -> &'static str {
        match self {
            CacheStatus::Hit => "(cached)",
            CacheStatus::Miss => "(fresh)",
            CacheStatus::ForcedMiss => "(forced refresh)",
            CacheStatus::Uncacheable => "(no cache)",
        }
    }
}

/// Result of [`run_analysis_cached`] with metadata.
#[derive(Debug)]
pub struct AnalysisOutcome {
    pub analysis: RepoAnalysis,
    pub cache_status: CacheStatus,
}

/// Compute a deterministic SHA-256 hash of the config fields that affect
/// analysis results: `include_categories`, `exclude_categories`,
/// `include_general`, and `max_per_framework`.
///
/// The hash is encoded as a lowercase hex string and is stable across runs
/// (sorted vecs, canonical `None` representation).
pub fn compute_config_hash(cfg: &Config) -> String {
    let mut hasher = Sha256::new();

    // include_categories — sort for determinism
    match &cfg.include_categories {
        None => hasher.update(b"include_categories:none\n"),
        Some(cats) => {
            let mut sorted = cats.clone();
            sorted.sort();
            hasher.update(b"include_categories:");
            for c in &sorted {
                hasher.update(c.as_bytes());
                hasher.update(b",");
            }
            hasher.update(b"\n");
        }
    }

    // exclude_categories — sort for determinism
    match &cfg.exclude_categories {
        None => hasher.update(b"exclude_categories:none\n"),
        Some(cats) => {
            let mut sorted = cats.clone();
            sorted.sort();
            hasher.update(b"exclude_categories:");
            for c in &sorted {
                hasher.update(c.as_bytes());
                hasher.update(b",");
            }
            hasher.update(b"\n");
        }
    }

    // include_general
    match cfg.include_general {
        None => hasher.update(b"include_general:none\n"),
        Some(v) => {
            hasher.update(b"include_general:");
            hasher.update(if v { b"true" as &[u8] } else { b"false" });
            hasher.update(b"\n");
        }
    }

    // max_per_framework
    match cfg.max_per_framework {
        None => hasher.update(b"max_per_framework:none\n"),
        Some(v) => {
            hasher.update(b"max_per_framework:");
            hasher.update(v.to_string().as_bytes());
            hasher.update(b"\n");
        }
    }

    format!("{:x}", hasher.finalize())
}

/// Parse the output of `git rev-parse HEAD` into a commit hash.
///
/// Returns `None` if the output is not a successful command, not valid UTF-8,
/// or the trimmed output is empty.
fn parse_git_head_output(output: std::process::Output) -> Option<String> {
    if !output.status.success() {
        return None;
    }

    let hash = String::from_utf8(output.stdout).ok()?;
    let trimmed = hash.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// Get the current HEAD commit hash for a git repository.
///
/// Returns `None` if the path is not a git repo, git is not installed,
/// or the command fails for any reason.
pub fn get_git_head(repo_path: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_path)
        .output()
        .ok()?;

    parse_git_head_output(output)
}

/// Get the current branch name for a git repository.
///
/// Returns `None` if the path is not a git repo, git is not installed,
/// the repo is in detached HEAD state, or the command fails for any reason.
pub fn get_git_branch(repo_path: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(repo_path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8(output.stdout).ok()?;
    let trimmed = branch.trim();
    // "HEAD" means detached HEAD state — not a named branch
    if trimmed.is_empty() || trimmed == "HEAD" {
        return None;
    }
    Some(trimmed.to_string())
}

/// Returns `true` if the serialized form of `cached` exceeds [`CACHE_MAX_SIZE_BYTES`].
///
/// When this returns `true`, the caller should skip persisting the cache entry.
fn is_analysis_cache_oversized(cached: &CachedAnalysis) -> bool {
    let size_result = serde_yml::to_string(cached)
        .map(|s| s.len())
        .map_err(|e| e.to_string());
    is_analysis_cache_oversized_inner(size_result)
}

/// Inner implementation that accepts a pre-computed serialized-size result,
/// enabling tests to simulate serialization failures.
fn is_analysis_cache_oversized_inner(size_result: Result<usize, String>) -> bool {
    use crate::config::types::CACHE_MAX_SIZE_BYTES;
    let serialized_size = match size_result {
        Ok(size) => size,
        Err(e) => {
            tracing::warn!(
                "failed to serialize analysis cache for size check: {e}; treating as oversized"
            );
            return true;
        }
    };
    serialized_size > CACHE_MAX_SIZE_BYTES
}

/// Run repository analysis with caching based on git HEAD.
///
/// If the repository has a git HEAD that matches the cached analysis,
/// the cached result is returned without running analysis. Otherwise,
/// a fresh static analysis is run and the result is cached.
///
/// When `force` is true, any cached result is ignored and a fresh analysis
/// is always run (the cache is still updated afterward).
///
/// Non-git repositories always run fresh analysis (no caching attempted).
pub fn run_analysis_cached(
    repo_path: &Path,
    config_path: &Path,
    force: bool,
) -> Result<AnalysisOutcome, ActualError> {
    let head_commit = get_git_head(repo_path);

    // Not a git repo — skip caching entirely
    let head_hash = match head_commit {
        Some(h) => h,
        None => {
            return Ok(AnalysisOutcome {
                analysis: run_static_analysis(repo_path)?,
                cache_status: CacheStatus::Uncacheable,
            });
        }
    };

    let mut cfg = config::paths::load_from(config_path)?;
    let cfg_hash = compute_config_hash(&cfg);

    // Check for cache hit (skip when force=true)
    let cache_status = if !force {
        if let Some(ref cached) = cfg.cached_analysis {
            if cached.repo_path == repo_path.to_string_lossy().as_ref()
                && cached.head_commit.as_deref() == Some(&head_hash)
                && cached.config_hash.as_deref() == Some(cfg_hash.as_str())
                && !cached.is_expired()
            {
                return Ok(AnalysisOutcome {
                    analysis: cached.analysis.clone(),
                    cache_status: CacheStatus::Hit,
                });
            }
        }
        CacheStatus::Miss
    } else {
        CacheStatus::ForcedMiss
    };

    // Cache miss: run fresh analysis
    let analysis = run_static_analysis(repo_path)?;

    // Cache the result (skips write if analysis is too large).
    let cached_analysis = CachedAnalysis {
        repo_path: repo_path.to_string_lossy().to_string(),
        head_commit: Some(head_hash),
        config_hash: Some(cfg_hash),
        analysis: analysis.clone(),
        analyzed_at: chrono::Utc::now(),
    };
    try_save_analysis_cache(&mut cfg, config_path, cached_analysis)?;

    Ok(AnalysisOutcome {
        analysis,
        cache_status,
    })
}

/// Persist `cached` to disk unless it exceeds the size limit.
///
/// This is extracted so it can be unit-tested independently of `run_analysis_cached`
/// (which cannot easily produce an oversized analysis in tests).
fn try_save_analysis_cache(
    cfg: &mut config::types::Config,
    config_path: &Path,
    cached: CachedAnalysis,
) -> Result<(), ActualError> {
    if is_analysis_cache_oversized(&cached) {
        tracing::warn!("analysis cache exceeds size limit; skipping cache write");
        return Ok(());
    }
    cfg.cached_analysis = Some(cached);
    config::paths::save_to(cfg, config_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // -- Helpers --

    /// Create a real git repo in a temp directory and return the HEAD hash.
    fn create_git_repo(dir: &Path) -> String {
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::fs::write(dir.join("file.txt"), "content").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir)
            .output()
            .unwrap();
        let output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    /// Build a valid RepoAnalysis for use in cache tests.
    fn valid_analysis() -> RepoAnalysis {
        RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![crate::analysis::types::Project {
                path: ".".to_string(),
                name: "test-project".to_string(),
                languages: vec![],
                frameworks: vec![],
                package_manager: None,
                description: None,
                dep_count: 0,
                dev_dep_count: 0,
                selection: None,
            }],
        }
    }

    /// Write a config with a pre-populated CachedAnalysis to disk.
    ///
    /// `config_hash` is the pre-computed config hash to store in the cached entry.
    /// Pass `None` to simulate a cache entry written before the config_hash field existed
    /// (which will always be treated as a cache miss).
    fn write_config_with_cache(
        config_path: &Path,
        repo_path: &str,
        head_commit: Option<&str>,
        config_hash: Option<&str>,
        analysis: RepoAnalysis,
    ) {
        let cfg = config::Config {
            cached_analysis: Some(CachedAnalysis {
                repo_path: repo_path.to_string(),
                head_commit: head_commit.map(|s| s.to_string()),
                config_hash: config_hash.map(|s| s.to_string()),
                analysis,
                analyzed_at: chrono::Utc::now(),
            }),
            ..config::Config::default()
        };
        config::paths::save_to(&cfg, config_path).unwrap();
    }

    // -- Tests --

    /// Helper: create a fake successful process output with given stdout.
    fn fake_output_success(stdout: &[u8]) -> std::process::Output {
        std::process::Command::new("true")
            .output()
            .map(|mut o| {
                o.stdout = stdout.to_vec();
                o
            })
            .unwrap()
    }

    /// Helper: create a fake failed process output.
    fn fake_output_failure() -> std::process::Output {
        std::process::Command::new("false").output().unwrap()
    }

    #[test]
    fn test_parse_git_head_output_success() {
        let output = fake_output_success(b"abc123def456\n");
        assert_eq!(
            parse_git_head_output(output),
            Some("abc123def456".to_string())
        );
    }

    #[test]
    fn test_parse_git_head_output_failure_status() {
        let output = fake_output_failure();
        assert_eq!(parse_git_head_output(output), None);
    }

    #[test]
    fn test_parse_git_head_output_empty_stdout() {
        let output = fake_output_success(b"");
        assert_eq!(parse_git_head_output(output), None);
    }

    #[test]
    fn test_parse_git_head_output_whitespace_only() {
        let output = fake_output_success(b"  \n  ");
        assert_eq!(parse_git_head_output(output), None);
    }

    #[test]
    fn test_parse_git_head_output_invalid_utf8() {
        let output = fake_output_success(&[0xff, 0xfe, 0xfd]);
        assert_eq!(parse_git_head_output(output), None);
    }

    #[test]
    fn test_get_git_head_returns_hash_for_git_repo() {
        let dir = tempdir().unwrap();
        let expected_hash = create_git_repo(dir.path());

        let result = get_git_head(dir.path());
        assert_eq!(result, Some(expected_hash));
    }

    #[test]
    fn test_get_git_head_returns_none_for_non_git_dir() {
        let dir = tempdir().unwrap();

        let result = get_git_head(dir.path());
        assert_eq!(result, None);
    }

    #[test]
    fn test_get_git_branch_returns_branch_for_git_repo() {
        let dir = tempdir().unwrap();
        create_git_repo(dir.path());

        let result = get_git_branch(dir.path());
        // The default branch name depends on git config (typically "main" or "master")
        assert!(result.is_some(), "expected a branch name for a git repo");
        let branch = result.unwrap();
        assert!(!branch.is_empty());
        assert_ne!(branch, "HEAD");
    }

    #[test]
    fn test_get_git_branch_returns_none_for_non_git_dir() {
        let dir = tempdir().unwrap();

        let result = get_git_branch(dir.path());
        assert_eq!(result, None);
    }

    #[test]
    fn test_get_git_branch_returns_none_for_nonexistent_path() {
        let result = get_git_branch(Path::new("/nonexistent/path/that/does/not/exist"));
        assert_eq!(result, None);
    }

    #[test]
    fn test_cache_hit_returns_cached_without_running_analysis() {
        let repo_dir = tempdir().unwrap();
        let head_hash = create_git_repo(repo_dir.path());

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        // Write config with cached analysis matching current HEAD
        let analysis = valid_analysis();
        let default_cfg = config::Config::default();
        let cfg_hash = compute_config_hash(&default_cfg);
        write_config_with_cache(
            &config_path,
            &repo_dir.path().to_string_lossy(),
            Some(&head_hash),
            Some(&cfg_hash),
            analysis,
        );

        let outcome = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap();

        assert_eq!(outcome.cache_status, CacheStatus::Hit);
        assert!(!outcome.analysis.is_monorepo);
        assert_eq!(outcome.analysis.projects.len(), 1);
        assert_eq!(outcome.analysis.projects[0].name, "test-project");
    }

    #[test]
    fn test_cache_miss_head_differs_runs_analysis_and_caches() {
        let repo_dir = tempdir().unwrap();
        let head_hash = create_git_repo(repo_dir.path());

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        // Write config with a different HEAD hash
        let analysis = valid_analysis();
        write_config_with_cache(
            &config_path,
            &repo_dir.path().to_string_lossy(),
            Some("different_head_hash"),
            None,
            analysis,
        );

        let outcome = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap();

        // Static analysis on the git repo dir succeeds
        assert_eq!(outcome.cache_status, CacheStatus::Miss);
        assert_eq!(outcome.analysis.projects.len(), 1);

        // Verify the config was updated with the new HEAD
        let cfg = config::paths::load_from(&config_path).unwrap();
        let cached = cfg.cached_analysis.unwrap();
        assert_eq!(cached.head_commit, Some(head_hash));
        assert_eq!(cached.repo_path, repo_dir.path().to_string_lossy().as_ref());
    }

    #[test]
    fn test_no_git_repo_always_runs_analysis() {
        let repo_dir = tempdir().unwrap();
        // No git init — not a git repo

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        let outcome = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap();

        // Static analysis on empty dir returns one project
        assert_eq!(outcome.cache_status, CacheStatus::Uncacheable);
        assert_eq!(outcome.analysis.projects.len(), 1);

        // Config should not have been written (no caching for non-git repos)
        assert!(!config_path.exists());
    }

    #[test]
    fn test_cache_miss_no_previous_cache() {
        let repo_dir = tempdir().unwrap();
        let head_hash = create_git_repo(repo_dir.path());

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        // Write config with no cached_analysis
        let cfg = config::Config::default();
        config::paths::save_to(&cfg, &config_path).unwrap();

        let outcome = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap();

        assert_eq!(outcome.cache_status, CacheStatus::Miss);
        assert_eq!(outcome.analysis.projects.len(), 1);

        // Verify result was cached
        let cfg = config::paths::load_from(&config_path).unwrap();
        let cached = cfg.cached_analysis.unwrap();
        assert_eq!(cached.head_commit, Some(head_hash));
    }

    #[test]
    fn test_cache_miss_repo_path_differs() {
        let repo_dir = tempdir().unwrap();
        create_git_repo(repo_dir.path());

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        // Write config with a different repo_path but same HEAD
        let head_hash = get_git_head(repo_dir.path()).unwrap();
        let analysis = valid_analysis();
        write_config_with_cache(
            &config_path,
            "/some/other/repo",
            Some(&head_hash),
            None,
            analysis,
        );

        let outcome = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap();

        assert_eq!(outcome.cache_status, CacheStatus::Miss);
        assert_eq!(outcome.analysis.projects.len(), 1);

        // Verify config now has the correct repo_path
        let cfg = config::paths::load_from(&config_path).unwrap();
        let cached = cfg.cached_analysis.unwrap();
        assert_eq!(cached.repo_path, repo_dir.path().to_string_lossy().as_ref());
    }

    #[test]
    fn test_get_git_head_returns_none_for_nonexistent_path() {
        let result = get_git_head(Path::new("/nonexistent/path/that/does/not/exist"));
        assert_eq!(result, None);
    }

    #[test]
    fn test_cache_miss_config_load_error_propagates() {
        let repo_dir = tempdir().unwrap();
        create_git_repo(repo_dir.path());

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        // Write invalid YAML to the config file
        std::fs::write(&config_path, "{{{{invalid yaml").unwrap();

        let err = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap_err();

        assert!(err.to_string().contains("Failed to parse config YAML"));
    }

    #[test]
    fn test_cache_miss_analysis_error_propagates() {
        let repo_dir = tempdir().unwrap();
        create_git_repo(repo_dir.path());

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        // Create a pnpm-workspace.yaml with invalid YAML to trigger an analysis error
        std::fs::write(
            repo_dir.path().join("pnpm-workspace.yaml"),
            "{{invalid yaml",
        )
        .unwrap();

        let err = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap_err();

        assert!(
            err.to_string().contains("Monorepo detection failed"),
            "expected monorepo error, got: {}",
            err
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_cache_miss_save_error_propagates() {
        use std::os::unix::fs::PermissionsExt;

        let repo_dir = tempdir().unwrap();
        create_git_repo(repo_dir.path());

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        // Create the config file, then make it read-only
        config::paths::save_to(&config::Config::default(), &config_path).unwrap();
        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o444)).unwrap();
        std::fs::set_permissions(config_dir.path(), std::fs::Permissions::from_mode(0o555))
            .unwrap();

        let err = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap_err();

        assert!(
            err.to_string()
                .contains("Failed to open config file for writing"),
            "Unexpected error: {err}"
        );

        // Restore permissions for cleanup
        std::fs::set_permissions(config_dir.path(), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn test_force_bypasses_valid_cache() {
        let repo_dir = tempdir().unwrap();
        let head_hash = create_git_repo(repo_dir.path());

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        // Write config with cached analysis matching current HEAD (would be a cache hit)
        let analysis = valid_analysis();
        let default_cfg = config::Config::default();
        let cfg_hash = compute_config_hash(&default_cfg);
        write_config_with_cache(
            &config_path,
            &repo_dir.path().to_string_lossy(),
            Some(&head_hash),
            Some(&cfg_hash),
            analysis,
        );

        // With force=false, this returns the cached "test-project" name
        let cached_outcome = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap();
        assert_eq!(cached_outcome.cache_status, CacheStatus::Hit);
        assert_eq!(cached_outcome.analysis.projects[0].name, "test-project");

        // With force=true, this bypasses the cache and runs fresh analysis
        let forced_outcome = run_analysis_cached(repo_dir.path(), &config_path, true).unwrap();
        assert_eq!(forced_outcome.cache_status, CacheStatus::ForcedMiss);
        assert_ne!(
            forced_outcome.analysis.projects[0].name, "test-project",
            "force=true should bypass cached analysis and return fresh result"
        );

        // Verify the cache was updated afterward
        let cfg = config::paths::load_from(&config_path).unwrap();
        let cached = cfg.cached_analysis.unwrap();
        assert_eq!(cached.head_commit, Some(head_hash));
        assert_eq!(cached.repo_path, repo_dir.path().to_string_lossy().as_ref());
    }

    #[test]
    fn test_force_updates_cache_after_fresh_analysis() {
        let repo_dir = tempdir().unwrap();
        let head_hash = create_git_repo(repo_dir.path());

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        // Write config with cached analysis using an old timestamp
        let old_time = chrono::Utc::now() - chrono::Duration::hours(1);
        let analysis = valid_analysis();
        let base_cfg = config::Config::default();
        let cfg_hash = compute_config_hash(&base_cfg);
        let cfg = config::Config {
            cached_analysis: Some(CachedAnalysis {
                repo_path: repo_dir.path().to_string_lossy().to_string(),
                head_commit: Some(head_hash.clone()),
                config_hash: Some(cfg_hash),
                analysis,
                analyzed_at: old_time,
            }),
            ..config::Config::default()
        };
        config::paths::save_to(&cfg, &config_path).unwrap();

        // Call with force=true
        let outcome = run_analysis_cached(repo_dir.path(), &config_path, true).unwrap();
        assert_eq!(outcome.cache_status, CacheStatus::ForcedMiss);
        assert_eq!(outcome.analysis.projects.len(), 1);

        // Verify the cache was updated with a fresh timestamp
        let updated_cfg = config::paths::load_from(&config_path).unwrap();
        let updated_cached = updated_cfg.cached_analysis.unwrap();
        assert_eq!(updated_cached.head_commit, Some(head_hash));
        assert!(
            updated_cached.analyzed_at > old_time,
            "force=true should update the cache with a newer analyzed_at timestamp"
        );
    }

    #[test]
    fn test_cache_save_and_retrieval_round_trip() {
        // Exercises the cache save path: analysis is stored as typed RepoAnalysis,
        // and a subsequent cache hit returns the same typed value directly.
        let repo_dir = tempdir().unwrap();
        let head_hash = create_git_repo(repo_dir.path());

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        // Write empty config — forces a cache miss and triggers the save path
        let cfg = config::Config::default();
        config::paths::save_to(&cfg, &config_path).unwrap();

        let outcome = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap();
        assert_eq!(outcome.analysis.projects.len(), 1);

        // Verify the analysis was saved to cache
        let saved_cfg = config::paths::load_from(&config_path).unwrap();
        let cached = saved_cfg.cached_analysis.unwrap();
        assert_eq!(cached.head_commit, Some(head_hash));

        // Verify the cached analysis is the typed RepoAnalysis directly
        assert_eq!(
            cached.analysis.projects.len(),
            outcome.analysis.projects.len()
        );
    }

    // ── actual-cli-jot.8: config-change cache invalidation ──

    /// Changing a config field that affects analysis (e.g. include_categories)
    /// must produce a different config hash, causing a cache miss.
    #[test]
    fn test_config_hash_differs_when_include_categories_changes() {
        let default_cfg = config::Config::default();
        let hash_default = compute_config_hash(&default_cfg);

        let modified_cfg = config::Config {
            include_categories: Some(vec!["security".to_string()]),
            ..config::Config::default()
        };
        let hash_modified = compute_config_hash(&modified_cfg);

        assert_ne!(
            hash_default, hash_modified,
            "config hash must differ when include_categories changes"
        );
    }

    #[test]
    fn test_config_hash_differs_when_exclude_categories_changes() {
        let default_cfg = config::Config::default();
        let hash_default = compute_config_hash(&default_cfg);

        let modified_cfg = config::Config {
            exclude_categories: Some(vec!["deprecated".to_string()]),
            ..config::Config::default()
        };
        let hash_modified = compute_config_hash(&modified_cfg);

        assert_ne!(
            hash_default, hash_modified,
            "config hash must differ when exclude_categories changes"
        );
    }

    #[test]
    fn test_config_hash_differs_when_include_general_changes() {
        let default_cfg = config::Config::default();
        let hash_default = compute_config_hash(&default_cfg);

        let modified_cfg = config::Config {
            include_general: Some(true),
            ..config::Config::default()
        };
        let hash_modified = compute_config_hash(&modified_cfg);

        assert_ne!(
            hash_default, hash_modified,
            "config hash must differ when include_general changes"
        );
    }

    #[test]
    fn test_config_hash_differs_when_max_per_framework_changes() {
        let default_cfg = config::Config::default();
        let hash_default = compute_config_hash(&default_cfg);

        let modified_cfg = config::Config {
            max_per_framework: Some(5),
            ..config::Config::default()
        };
        let hash_modified = compute_config_hash(&modified_cfg);

        assert_ne!(
            hash_default, hash_modified,
            "config hash must differ when max_per_framework changes"
        );
    }

    #[test]
    fn test_config_hash_stable_for_same_config() {
        let cfg = config::Config {
            include_categories: Some(vec!["testing".to_string(), "security".to_string()]),
            exclude_categories: Some(vec!["deprecated".to_string()]),
            include_general: Some(false),
            max_per_framework: Some(10),
            ..config::Config::default()
        };
        let hash1 = compute_config_hash(&cfg);
        let hash2 = compute_config_hash(&cfg);
        assert_eq!(hash1, hash2, "config hash must be deterministic");
    }

    #[test]
    fn test_config_hash_stable_regardless_of_category_order() {
        let cfg_a = config::Config {
            include_categories: Some(vec!["security".to_string(), "testing".to_string()]),
            ..config::Config::default()
        };
        let cfg_b = config::Config {
            include_categories: Some(vec!["testing".to_string(), "security".to_string()]),
            ..config::Config::default()
        };
        assert_eq!(
            compute_config_hash(&cfg_a),
            compute_config_hash(&cfg_b),
            "config hash must be order-independent for categories"
        );
    }

    /// Cache miss when config_hash in the cached entry is absent (old-format cache).
    /// Simulates a cache written before the config_hash field was introduced.
    #[test]
    fn test_cache_miss_when_config_hash_absent() {
        let repo_dir = tempdir().unwrap();
        let head_hash = create_git_repo(repo_dir.path());

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        // Write a cache entry with no config_hash (simulating old format)
        let analysis = valid_analysis();
        write_config_with_cache(
            &config_path,
            &repo_dir.path().to_string_lossy(),
            Some(&head_hash),
            None, // no config_hash — old-format entry
            analysis,
        );

        // Should be a cache miss: re-runs analysis instead of returning "test-project"
        let outcome = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap();
        assert_eq!(outcome.cache_status, CacheStatus::Miss);
        assert_ne!(
            outcome.analysis.projects[0].name, "test-project",
            "cache with missing config_hash must be treated as a miss"
        );

        // Verify the updated cache now contains a config_hash
        let cfg = config::paths::load_from(&config_path).unwrap();
        let cached = cfg.cached_analysis.unwrap();
        assert!(
            cached.config_hash.is_some(),
            "updated cache entry must include config_hash"
        );
    }

    /// Cache miss when config settings change even if the commit hash matches.
    #[test]
    fn test_cache_miss_when_config_hash_differs() {
        let repo_dir = tempdir().unwrap();
        let head_hash = create_git_repo(repo_dir.path());

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        // Store a cache entry built from the default config
        let analysis = valid_analysis();
        let default_cfg = config::Config::default();
        let default_hash = compute_config_hash(&default_cfg);
        write_config_with_cache(
            &config_path,
            &repo_dir.path().to_string_lossy(),
            Some(&head_hash),
            Some(&default_hash),
            analysis,
        );

        // Now write a config file with different settings (include_categories changed)
        let modified_cfg = config::Config {
            include_categories: Some(vec!["security".to_string()]),
            cached_analysis: config::paths::load_from(&config_path)
                .unwrap()
                .cached_analysis,
            ..config::Config::default()
        };
        config::paths::save_to(&modified_cfg, &config_path).unwrap();

        // Should be a cache miss because config_hash differs
        let outcome = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap();
        assert_eq!(outcome.cache_status, CacheStatus::Miss);
        assert_ne!(
            outcome.analysis.projects[0].name, "test-project",
            "cache must be invalidated when config settings change"
        );

        // Verify the cache was updated with the new config hash
        let updated_cfg = config::paths::load_from(&config_path).unwrap();
        let updated_cached = updated_cfg.cached_analysis.unwrap();
        let expected_hash = compute_config_hash(&config::Config {
            include_categories: Some(vec!["security".to_string()]),
            ..config::Config::default()
        });
        assert_eq!(
            updated_cached.config_hash,
            Some(expected_hash),
            "updated cache must reflect new config hash"
        );
    }

    /// Backward-compat regression test: old cache files (written when analysis was stored as
    /// serde_yml::Value) can still be deserialized correctly because RepoAnalysis serializes
    /// to the same YAML format. This test constructs the YAML representation directly and
    /// deserializes it into a Config containing a CachedAnalysis with a typed RepoAnalysis.
    #[test]
    fn test_old_cache_format_deserializes_correctly() {
        // Construct the YAML that an old-format cache entry would have produced.
        // The on-disk format is identical: serde_yml::Value would serialize RepoAnalysis
        // to the same YAML structure that the typed RepoAnalysis field now deserializes.
        let yaml = r#"
cached_analysis:
  repo_path: /home/user/project
  head_commit: abc123def456
  config_hash: deadbeef
  analysis:
    is_monorepo: false
    projects:
      - path: .
        name: legacy-project
        languages:
          - language: rust
            loc: 500
        frameworks: []
  analyzed_at: "2024-01-01T00:00:00Z"
"#;

        let cfg: config::Config = serde_yml::from_str(yaml).expect("should parse old cache YAML");
        let cached = cfg.cached_analysis.expect("should have cached_analysis");
        assert_eq!(cached.repo_path, "/home/user/project");
        assert_eq!(cached.head_commit.as_deref(), Some("abc123def456"));
        assert!(!cached.analysis.is_monorepo);
        assert_eq!(cached.analysis.projects.len(), 1);
        assert_eq!(cached.analysis.projects[0].name, "legacy-project");
    }

    /// Cache hit round-trip test with a full RepoAnalysis including all fields.
    #[test]
    fn test_cache_hit_round_trip_full_repo_analysis() {
        use crate::analysis::types::{Framework, FrameworkCategory, Language, LanguageStat};

        let repo_dir = tempdir().unwrap();
        let head_hash = create_git_repo(repo_dir.path());

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        // Build a rich RepoAnalysis with all field types exercised
        let rich_analysis = RepoAnalysis {
            is_monorepo: true,
            workspace_type: Some(crate::analysis::types::WorkspaceType::Cargo),
            projects: vec![crate::analysis::types::Project {
                path: "crates/core".to_string(),
                name: "core".to_string(),
                languages: vec![
                    LanguageStat {
                        language: Language::Rust,
                        loc: 4200,
                    },
                    LanguageStat {
                        language: Language::TypeScript,
                        loc: 800,
                    },
                ],
                frameworks: vec![Framework {
                    name: "actix-web".to_string(),
                    category: FrameworkCategory::WebBackend,
                    source: None,
                }],
                package_manager: Some("cargo".to_string()),
                description: Some("Core library".to_string()),
                dep_count: 15,
                dev_dep_count: 5,
                selection: None,
            }],
        };

        let default_cfg = config::Config::default();
        let cfg_hash = compute_config_hash(&default_cfg);
        write_config_with_cache(
            &config_path,
            &repo_dir.path().to_string_lossy(),
            Some(&head_hash),
            Some(&cfg_hash),
            rich_analysis.clone(),
        );

        let outcome = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap();

        assert_eq!(outcome.cache_status, CacheStatus::Hit);
        assert_eq!(outcome.analysis, rich_analysis);
    }

    #[test]
    fn cache_status_label_covers_all_variants() {
        assert_eq!(CacheStatus::Hit.label(), "(cached)");
        assert_eq!(CacheStatus::Miss.label(), "(fresh)");
        assert_eq!(CacheStatus::ForcedMiss.label(), "(forced refresh)");
        assert_eq!(CacheStatus::Uncacheable.label(), "(no cache)");
    }

    #[test]
    fn test_try_save_analysis_cache_skips_oversized_result() {
        use crate::analysis::types::Project;
        use crate::config::types::CACHE_MAX_SIZE_BYTES;

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        // Build an oversized CachedAnalysis (12 MiB > 10 MiB limit).
        let big_description = "x".repeat(1024);
        let projects: Vec<Project> = (0..12_000)
            .map(|i| Project {
                path: format!("proj-{i}"),
                name: format!("project-{i}"),
                languages: vec![],
                frameworks: vec![],
                package_manager: None,
                description: Some(big_description.clone()),
                dep_count: 0,
                dev_dep_count: 0,
                selection: None,
            })
            .collect();
        let big_cached = CachedAnalysis {
            repo_path: "/fake/repo".to_string(),
            head_commit: Some("abc123".to_string()),
            config_hash: Some("cfghash".to_string()),
            analysis: RepoAnalysis {
                is_monorepo: true,
                workspace_type: None,
                projects,
            },
            analyzed_at: chrono::Utc::now(),
        };

        // Sanity-check the fixture.
        let size = serde_yml::to_string(&big_cached)
            .map(|s| s.len())
            .unwrap_or(0);
        assert!(size > CACHE_MAX_SIZE_BYTES, "fixture must exceed limit");

        let mut cfg = config::Config::default();
        let result = try_save_analysis_cache(&mut cfg, &config_path, big_cached);

        // Should succeed (not an error) but the cache should NOT have been written.
        assert!(
            result.is_ok(),
            "oversized cache skipping must not be an error"
        );
        assert!(
            cfg.cached_analysis.is_none(),
            "oversized cache must not be stored in cfg"
        );
        assert!(
            !config_path.exists(),
            "oversized cache must not be written to disk"
        );
    }

    #[test]
    fn test_is_analysis_cache_oversized_returns_false_for_small_analysis() {
        let cached = CachedAnalysis {
            repo_path: "/fake/repo".to_string(),
            head_commit: Some("abc123".to_string()),
            config_hash: Some("cfghash".to_string()),
            analysis: valid_analysis(),
            analyzed_at: chrono::Utc::now(),
        };
        assert!(
            !is_analysis_cache_oversized(&cached),
            "small analysis should not be considered oversized"
        );
    }

    #[test]
    fn test_is_analysis_cache_oversized_returns_true_for_large_analysis() {
        use crate::analysis::types::Project;
        use crate::config::types::CACHE_MAX_SIZE_BYTES;

        // Each project description is 1 KiB; 12_000 projects ≈ 12 MiB > 10 MiB limit.
        let big_description = "x".repeat(1024);
        let projects: Vec<Project> = (0..12_000)
            .map(|i| Project {
                path: format!("proj-{i}"),
                name: format!("project-{i}"),
                languages: vec![],
                frameworks: vec![],
                package_manager: None,
                description: Some(big_description.clone()),
                dep_count: 0,
                dev_dep_count: 0,
                selection: None,
            })
            .collect();
        let big_analysis = RepoAnalysis {
            is_monorepo: true,
            workspace_type: None,
            projects,
        };
        let cached = CachedAnalysis {
            repo_path: "/fake/repo".to_string(),
            head_commit: Some("abc123".to_string()),
            config_hash: Some("cfghash".to_string()),
            analysis: big_analysis,
            analyzed_at: chrono::Utc::now(),
        };
        let size = serde_yml::to_string(&cached).map(|s| s.len()).unwrap_or(0);
        assert!(
            size > CACHE_MAX_SIZE_BYTES,
            "test fixture must exceed the limit ({size} bytes vs {CACHE_MAX_SIZE_BYTES} bytes)"
        );
        assert!(
            is_analysis_cache_oversized(&cached),
            "large analysis must be flagged as oversized"
        );
    }

    #[test]
    fn test_analysis_cache_expired_is_treated_as_miss() {
        let repo_dir = tempdir().unwrap();
        let head_hash = create_git_repo(repo_dir.path());

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        // Write config with a 8-day-old cached analysis (expired TTL)
        let analysis = valid_analysis();
        let base_cfg = config::Config::default();
        let cfg_hash = compute_config_hash(&base_cfg);
        let cfg = config::Config {
            cached_analysis: Some(CachedAnalysis {
                repo_path: repo_dir.path().to_string_lossy().to_string(),
                head_commit: Some(head_hash.clone()),
                config_hash: Some(cfg_hash),
                analysis,
                analyzed_at: chrono::Utc::now() - chrono::Duration::days(8),
            }),
            ..config::Config::default()
        };
        config::paths::save_to(&cfg, &config_path).unwrap();

        // Even though head commit and config hash match, TTL expired → cache miss
        let outcome = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap();
        assert_eq!(
            outcome.cache_status,
            CacheStatus::Miss,
            "expired cache must be treated as a miss"
        );
        // The fresh analysis returns the repo project (not our planted "test-project")
        assert_ne!(
            outcome.analysis.projects[0].name, "test-project",
            "expired cache should not return stale data"
        );
    }

    #[test]
    fn test_is_analysis_cache_oversized_inner_returns_true_on_serialization_failure() {
        // Simulate a serialization failure — should be treated as oversized
        assert!(
            is_analysis_cache_oversized_inner(Err("simulated failure".to_string())),
            "serialization failure should be treated as oversized"
        );
    }

    #[test]
    fn test_is_analysis_cache_oversized_inner_returns_false_for_small_size() {
        assert!(
            !is_analysis_cache_oversized_inner(Ok(100)),
            "small size should not be oversized"
        );
    }

    #[test]
    fn test_is_analysis_cache_oversized_inner_returns_true_for_large_size() {
        use crate::config::types::CACHE_MAX_SIZE_BYTES;
        assert!(
            is_analysis_cache_oversized_inner(Ok(CACHE_MAX_SIZE_BYTES + 1)),
            "size exceeding limit should be oversized"
        );
    }
}
