use std::path::Path;

use crate::analysis::orchestrate::run_static_analysis;
use crate::analysis::types::RepoAnalysis;
use crate::config;
use crate::config::types::CachedAnalysis;
use crate::error::ActualError;

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
) -> Result<RepoAnalysis, ActualError> {
    let head_commit = get_git_head(repo_path);

    // Not a git repo — skip caching entirely
    let head_hash = match head_commit {
        Some(h) => h,
        None => return run_static_analysis(repo_path),
    };

    let mut cfg = config::paths::load_from(config_path)?;

    // Check for cache hit (skip when force=true)
    if !force {
        if let Some(ref cached) = cfg.cached_analysis {
            if cached.repo_path == repo_path.to_string_lossy().as_ref()
                && cached.head_commit.as_deref() == Some(&head_hash)
            {
                // Try to deserialize the cached analysis
                if let Ok(analysis) =
                    serde_yaml::from_value::<RepoAnalysis>(cached.analysis.clone())
                {
                    return Ok(analysis);
                }
                // Deserialization failed — fall through to cache miss
            }
        }
    }

    // Cache miss: run fresh analysis
    let analysis = run_static_analysis(repo_path)?;

    // Cache the result
    let cached_analysis = CachedAnalysis {
        repo_path: repo_path.to_string_lossy().to_string(),
        head_commit: Some(head_hash),
        analysis: serde_yaml::to_value(&analysis).unwrap(),
        analyzed_at: chrono::Utc::now(),
    };
    cfg.cached_analysis = Some(cached_analysis);
    config::paths::save_to(&cfg, config_path)?;

    Ok(analysis)
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
            projects: vec![crate::analysis::types::Project {
                path: ".".to_string(),
                name: "test-project".to_string(),
                languages: vec![],
                frameworks: vec![],
                package_manager: None,
                description: None,
            }],
        }
    }

    /// Write a config with a pre-populated CachedAnalysis to disk.
    fn write_config_with_cache(
        config_path: &Path,
        repo_path: &str,
        head_commit: Option<&str>,
        analysis_value: serde_yaml::Value,
    ) {
        let cfg = config::Config {
            cached_analysis: Some(CachedAnalysis {
                repo_path: repo_path.to_string(),
                head_commit: head_commit.map(|s| s.to_string()),
                analysis: analysis_value,
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
    fn test_cache_hit_returns_cached_without_running_analysis() {
        let repo_dir = tempdir().unwrap();
        let head_hash = create_git_repo(repo_dir.path());

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        // Write config with cached analysis matching current HEAD
        let analysis = valid_analysis();
        let analysis_value = serde_yaml::to_value(&analysis).unwrap();
        write_config_with_cache(
            &config_path,
            &repo_dir.path().to_string_lossy(),
            Some(&head_hash),
            analysis_value,
        );

        let result = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap();

        assert!(!result.is_monorepo);
        assert_eq!(result.projects.len(), 1);
        assert_eq!(result.projects[0].name, "test-project");
    }

    #[test]
    fn test_cache_miss_head_differs_runs_analysis_and_caches() {
        let repo_dir = tempdir().unwrap();
        let head_hash = create_git_repo(repo_dir.path());

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        // Write config with a different HEAD hash
        let analysis = valid_analysis();
        let analysis_value = serde_yaml::to_value(&analysis).unwrap();
        write_config_with_cache(
            &config_path,
            &repo_dir.path().to_string_lossy(),
            Some("different_head_hash"),
            analysis_value,
        );

        let result = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap();

        // Static analysis on the git repo dir succeeds
        assert_eq!(result.projects.len(), 1);

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

        let result = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap();

        // Static analysis on empty dir returns one project
        assert_eq!(result.projects.len(), 1);

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

        let result = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap();

        assert_eq!(result.projects.len(), 1);

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
        let analysis_value = serde_yaml::to_value(&analysis).unwrap();
        write_config_with_cache(
            &config_path,
            "/some/other/repo",
            Some(&head_hash),
            analysis_value,
        );

        let result = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap();

        assert_eq!(result.projects.len(), 1);

        // Verify config now has the correct repo_path
        let cfg = config::paths::load_from(&config_path).unwrap();
        let cached = cfg.cached_analysis.unwrap();
        assert_eq!(cached.repo_path, repo_dir.path().to_string_lossy().as_ref());
    }

    #[test]
    fn test_corrupted_cache_reruns_analysis() {
        let repo_dir = tempdir().unwrap();
        let head_hash = create_git_repo(repo_dir.path());

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        // Write config with matching HEAD but corrupted analysis value
        let corrupted_value = serde_yaml::to_value("this is a string, not a RepoAnalysis").unwrap();
        write_config_with_cache(
            &config_path,
            &repo_dir.path().to_string_lossy(),
            Some(&head_hash),
            corrupted_value,
        );

        let result = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap();

        assert_eq!(result.projects.len(), 1);

        // Verify config was updated with valid cached data
        let cfg = config::paths::load_from(&config_path).unwrap();
        let cached = cfg.cached_analysis.unwrap();
        assert_eq!(cached.head_commit, Some(head_hash));
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

        assert!(err.to_string().contains("Failed to write config file"));

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
        let analysis_value = serde_yaml::to_value(&analysis).unwrap();
        write_config_with_cache(
            &config_path,
            &repo_dir.path().to_string_lossy(),
            Some(&head_hash),
            analysis_value,
        );

        // With force=false, this returns the cached "test-project" name
        let cached_result = run_analysis_cached(repo_dir.path(), &config_path, false).unwrap();
        assert_eq!(cached_result.projects[0].name, "test-project");

        // With force=true, this bypasses the cache and runs fresh analysis
        let forced_result = run_analysis_cached(repo_dir.path(), &config_path, true).unwrap();
        assert_ne!(
            forced_result.projects[0].name, "test-project",
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
        let cfg = config::Config {
            cached_analysis: Some(CachedAnalysis {
                repo_path: repo_dir.path().to_string_lossy().to_string(),
                head_commit: Some(head_hash.clone()),
                analysis: serde_yaml::to_value(&analysis).unwrap(),
                analyzed_at: old_time,
            }),
            ..config::Config::default()
        };
        config::paths::save_to(&cfg, &config_path).unwrap();

        // Call with force=true
        let result = run_analysis_cached(repo_dir.path(), &config_path, true).unwrap();
        assert_eq!(result.projects.len(), 1);

        // Verify the cache was updated with a fresh timestamp
        let updated_cfg = config::paths::load_from(&config_path).unwrap();
        let updated_cached = updated_cfg.cached_analysis.unwrap();
        assert_eq!(updated_cached.head_commit, Some(head_hash));
        assert!(
            updated_cached.analyzed_at > old_time,
            "force=true should update the cache with a newer analyzed_at timestamp"
        );
    }
}
