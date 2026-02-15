use std::path::Path;

use crate::analysis::orchestrate::run_analysis;
use crate::analysis::types::RepoAnalysis;
use crate::claude::subprocess::ClaudeRunner;
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
/// the cached result is returned without calling the runner. Otherwise,
/// a fresh analysis is run and the result is cached.
///
/// Non-git repositories always run fresh analysis (no caching attempted).
pub async fn run_analysis_cached(
    runner: &impl ClaudeRunner,
    model_override: Option<&str>,
    repo_path: &Path,
    config_path: &Path,
) -> Result<RepoAnalysis, ActualError> {
    let head_commit = get_git_head(repo_path);

    // Not a git repo — skip caching entirely
    let head_hash = match head_commit {
        Some(h) => h,
        None => return run_analysis(runner, model_override).await,
    };

    let mut cfg = config::paths::load_from(config_path)?;

    // Check for cache hit
    if let Some(ref cached) = cfg.cached_analysis {
        if cached.repo_path == repo_path.to_string_lossy().as_ref()
            && cached.head_commit.as_deref() == Some(&head_hash)
        {
            // Try to deserialize the cached analysis
            if let Ok(analysis) = serde_yaml::from_value::<RepoAnalysis>(cached.analysis.clone()) {
                return Ok(analysis);
            }
            // Deserialization failed — fall through to cache miss
        }
    }

    // Cache miss: run fresh analysis
    let analysis = run_analysis(runner, model_override).await?;

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
    use crate::error::ActualError;
    use serde::de::DeserializeOwned;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;
    use tempfile::tempdir;

    const VALID_ANALYSIS_JSON: &str = r#"{
        "is_monorepo": false,
        "projects": [{
            "path": ".",
            "name": "my-app",
            "languages": ["rust"],
            "frameworks": [{"name": "actix-web", "category": "web-backend"}],
            "package_manager": "cargo",
            "description": "A web application"
        }]
    }"#;

    // -- MockRunner (copied from orchestrate.rs, since it's cfg(test)-only there) --

    enum MockResponse {
        Json(String),
        Error(ActualError),
    }

    struct MockRunner {
        responses: Mutex<Vec<MockResponse>>,
        call_count: AtomicU32,
    }

    impl MockRunner {
        fn from_json(responses: Vec<&str>) -> Self {
            Self {
                responses: Mutex::new(
                    responses
                        .into_iter()
                        .map(|s| MockResponse::Json(s.to_string()))
                        .collect(),
                ),
                call_count: AtomicU32::new(0),
            }
        }

        fn from_error(error: ActualError) -> Self {
            Self {
                responses: Mutex::new(vec![MockResponse::Error(error)]),
                call_count: AtomicU32::new(0),
            }
        }

        fn calls(&self) -> u32 {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    impl ClaudeRunner for MockRunner {
        async fn run<T: DeserializeOwned + Send>(
            &self,
            _args: &[String],
        ) -> Result<T, ActualError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            let response = {
                let mut responses = self.responses.lock().unwrap();
                responses.remove(0)
            };
            match response {
                MockResponse::Json(json) => {
                    let parsed: T = serde_json::from_str(&json)?;
                    Ok(parsed)
                }
                MockResponse::Error(e) => Err(e),
            }
        }
    }

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

    /// Build a valid RepoAnalysis from the test fixture JSON.
    fn valid_analysis() -> RepoAnalysis {
        serde_json::from_str(VALID_ANALYSIS_JSON).unwrap()
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

    #[tokio::test]
    async fn test_cache_hit_returns_cached_without_calling_runner() {
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

        // MockRunner has a valid response but we expect 0 calls
        let runner = MockRunner::from_json(vec![VALID_ANALYSIS_JSON]);

        let result = run_analysis_cached(&runner, None, repo_dir.path(), &config_path)
            .await
            .unwrap();

        assert_eq!(runner.calls(), 0);
        assert!(!result.is_monorepo);
        assert_eq!(result.projects.len(), 1);
        assert_eq!(result.projects[0].name, "my-app");
    }

    #[tokio::test]
    async fn test_cache_miss_head_differs_calls_runner_and_caches() {
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

        let runner = MockRunner::from_json(vec![VALID_ANALYSIS_JSON]);

        let result = run_analysis_cached(&runner, None, repo_dir.path(), &config_path)
            .await
            .unwrap();

        assert_eq!(runner.calls(), 1);
        assert_eq!(result.projects[0].name, "my-app");

        // Verify the config was updated with the new HEAD
        let cfg = config::paths::load_from(&config_path).unwrap();
        let cached = cfg.cached_analysis.unwrap();
        assert_eq!(cached.head_commit, Some(head_hash));
        assert_eq!(cached.repo_path, repo_dir.path().to_string_lossy().as_ref());
    }

    #[tokio::test]
    async fn test_no_git_repo_always_calls_runner() {
        let repo_dir = tempdir().unwrap();
        // No git init — not a git repo

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        let runner = MockRunner::from_json(vec![VALID_ANALYSIS_JSON]);

        let result = run_analysis_cached(&runner, None, repo_dir.path(), &config_path)
            .await
            .unwrap();

        assert_eq!(runner.calls(), 1);
        assert_eq!(result.projects[0].name, "my-app");

        // Config should not have been written (no caching for non-git repos)
        assert!(!config_path.exists());
    }

    #[tokio::test]
    async fn test_cache_miss_no_previous_cache() {
        let repo_dir = tempdir().unwrap();
        let head_hash = create_git_repo(repo_dir.path());

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        // Write config with no cached_analysis
        let cfg = config::Config::default();
        config::paths::save_to(&cfg, &config_path).unwrap();

        let runner = MockRunner::from_json(vec![VALID_ANALYSIS_JSON]);

        let result = run_analysis_cached(&runner, None, repo_dir.path(), &config_path)
            .await
            .unwrap();

        assert_eq!(runner.calls(), 1);
        assert_eq!(result.projects[0].name, "my-app");

        // Verify result was cached
        let cfg = config::paths::load_from(&config_path).unwrap();
        let cached = cfg.cached_analysis.unwrap();
        assert_eq!(cached.head_commit, Some(head_hash));
    }

    #[tokio::test]
    async fn test_cache_miss_repo_path_differs() {
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

        let runner = MockRunner::from_json(vec![VALID_ANALYSIS_JSON]);

        let result = run_analysis_cached(&runner, None, repo_dir.path(), &config_path)
            .await
            .unwrap();

        assert_eq!(runner.calls(), 1);
        assert_eq!(result.projects[0].name, "my-app");

        // Verify config now has the correct repo_path
        let cfg = config::paths::load_from(&config_path).unwrap();
        let cached = cfg.cached_analysis.unwrap();
        assert_eq!(cached.repo_path, repo_dir.path().to_string_lossy().as_ref());
    }

    #[tokio::test]
    async fn test_corrupted_cache_reruns_analysis() {
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

        let runner = MockRunner::from_json(vec![VALID_ANALYSIS_JSON]);

        let result = run_analysis_cached(&runner, None, repo_dir.path(), &config_path)
            .await
            .unwrap();

        assert_eq!(runner.calls(), 1);
        assert_eq!(result.projects[0].name, "my-app");

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

    #[tokio::test]
    async fn test_cache_miss_config_load_error_propagates() {
        let repo_dir = tempdir().unwrap();
        create_git_repo(repo_dir.path());

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        // Write invalid YAML to the config file
        std::fs::write(&config_path, "{{{{invalid yaml").unwrap();

        let runner = MockRunner::from_json(vec![VALID_ANALYSIS_JSON]);

        let err = run_analysis_cached(&runner, None, repo_dir.path(), &config_path)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("Failed to parse config YAML"));
        assert_eq!(runner.calls(), 0);
    }

    #[tokio::test]
    async fn test_cache_miss_runner_error_propagates() {
        let repo_dir = tempdir().unwrap();
        create_git_repo(repo_dir.path());

        let config_dir = tempdir().unwrap();
        let config_path = config_dir.path().join("config.yaml");

        let runner = MockRunner::from_error(ActualError::ClaudeSubprocessFailed {
            message: "process crashed".to_string(),
            stderr: "segfault".to_string(),
        });

        let err = run_analysis_cached(&runner, None, repo_dir.path(), &config_path)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("subprocess failed"));
        assert_eq!(runner.calls(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_cache_miss_save_error_propagates() {
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

        let runner = MockRunner::from_json(vec![VALID_ANALYSIS_JSON]);

        let err = run_analysis_cached(&runner, None, repo_dir.path(), &config_path)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("Failed to write config file"));
        assert_eq!(runner.calls(), 1);

        // Restore permissions for cleanup
        std::fs::set_permissions(config_dir.path(), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
}
