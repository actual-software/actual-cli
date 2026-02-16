use std::path::Path;

use crate::analysis::types::RepoAnalysis;
use crate::claude::options::InvocationOptions;
use crate::claude::prompts::analysis_prompt;
use crate::claude::schemas::REPO_ANALYSIS_SCHEMA;
use crate::claude::subprocess::ClaudeRunner;
use crate::error::ActualError;

/// Normalize a project path relative to a working directory.
///
/// Converts absolute paths to relative paths, strips leading `./` and trailing
/// `/`, and normalizes empty strings to `"."`.  Also handles macOS's
/// `/private/var` ↔ `/var` symlink by trying both the raw and canonicalized
/// `working_dir` when stripping prefixes.
///
/// `canonical_dir` should be `working_dir.canonicalize().ok()` — passed in
/// so the caller can compute it once for all projects.
pub fn normalize_project_path(
    path: &str,
    working_dir: &Path,
    canonical_dir: Option<&Path>,
) -> String {
    let p = Path::new(path);

    // If absolute, try to make relative to working_dir
    if p.is_absolute() {
        if let Ok(relative) = p.strip_prefix(working_dir) {
            return relative_or_dot(relative);
        }
        // Also try with canonicalized working_dir (macOS /var -> /private/var)
        if let Some(canonical) = canonical_dir {
            if let Ok(relative) = p.strip_prefix(canonical) {
                return relative_or_dot(relative);
            }
        }
        // Can't make relative — leave as-is (edge case)
        return path.to_string();
    }

    // Strip leading "./"
    let path = path.strip_prefix("./").unwrap_or(path);

    // Strip trailing "/"
    let path = path.strip_suffix('/').unwrap_or(path);

    // Empty string -> "."
    if path.is_empty() {
        return ".".to_string();
    }

    path.to_string()
}

/// Convert a relative path to a string, using `"."` for the empty path.
fn relative_or_dot(relative: &Path) -> String {
    if relative == Path::new("") {
        ".".to_string()
    } else {
        relative.to_string_lossy().to_string()
    }
}

/// Normalize all project paths in an analysis result.
fn normalize_analysis(analysis: &mut RepoAnalysis, working_dir: &Path) {
    let canonical = working_dir.canonicalize().ok();
    for project in &mut analysis.projects {
        project.path = normalize_project_path(&project.path, working_dir, canonical.as_deref());
    }
}

/// Run repository analysis using Claude Code.
///
/// Builds the analysis prompt + invocation options, calls the runner,
/// and validates the result. Retries once on JSON parse failure.
/// After successful parsing, normalizes project paths relative to
/// `working_dir`.
pub async fn run_analysis(
    runner: &impl ClaudeRunner,
    model_override: Option<&str>,
    working_dir: &Path,
) -> Result<RepoAnalysis, ActualError> {
    let mut opts = InvocationOptions::for_analysis(model_override);
    opts.json_schema = Some(REPO_ANALYSIS_SCHEMA.to_string());

    let prompt = analysis_prompt();
    let mut args = vec![prompt];
    args.extend(opts.to_args());

    // First attempt
    let result: Result<RepoAnalysis, ActualError> = runner.run(&args).await;

    let mut analysis = match result {
        Ok(a) => a,
        Err(ActualError::ClaudeOutputParse(_)) => {
            // Retry once on parse failure
            runner.run::<RepoAnalysis>(&args).await?
        }
        Err(e) => return Err(e),
    };

    if analysis.projects.is_empty() {
        return Err(ActualError::AnalysisEmpty);
    }

    // Normalize project paths (absolute -> relative, strip ./ and trailing /)
    normalize_analysis(&mut analysis, working_dir);

    Ok(analysis)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    /// Valid fixture JSON matching the RepoAnalysis schema.
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

    const EMPTY_PROJECTS_JSON: &str = r#"{
        "is_monorepo": false,
        "projects": []
    }"#;

    /// A canned response: either a JSON string (parsed by serde) or a
    /// non-parse error returned directly.
    enum MockResponse {
        Json(String),
        Error(ActualError),
    }

    /// Mock runner that returns a sequence of responses.
    /// Each call pops the next response from the queue.
    /// Using a single mock type avoids multiple monomorphizations of
    /// `run_analysis`, which keeps coverage at 100%.
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

    #[tokio::test]
    async fn test_valid_analysis_returns_correct_result() {
        let runner = MockRunner::from_json(vec![VALID_ANALYSIS_JSON]);
        let working_dir = Path::new("/tmp/test-repo");
        let result = run_analysis(&runner, None, working_dir).await.unwrap();

        assert!(!result.is_monorepo);
        assert_eq!(result.projects.len(), 1);
        assert_eq!(result.projects[0].name, "my-app");
        assert_eq!(result.projects[0].path, ".");
        assert_eq!(result.projects[0].languages.len(), 1);
        assert_eq!(result.projects[0].frameworks.len(), 1);
        assert_eq!(result.projects[0].frameworks[0].name, "actix-web");
        assert_eq!(runner.calls(), 1);
    }

    #[tokio::test]
    async fn test_empty_projects_returns_error() {
        let runner = MockRunner::from_json(vec![EMPTY_PROJECTS_JSON]);
        let working_dir = Path::new("/tmp/test-repo");
        let err = run_analysis(&runner, None, working_dir).await.unwrap_err();

        assert_eq!(err.to_string(), "Analysis returned no projects");
        assert_eq!(runner.calls(), 1);
    }

    #[tokio::test]
    async fn test_invalid_json_then_valid_retries_successfully() {
        let runner = MockRunner::from_json(vec!["not valid json", VALID_ANALYSIS_JSON]);
        let working_dir = Path::new("/tmp/test-repo");
        let result = run_analysis(&runner, None, working_dir).await.unwrap();

        assert_eq!(result.projects.len(), 1);
        assert_eq!(result.projects[0].name, "my-app");
        assert_eq!(runner.calls(), 2);
    }

    #[tokio::test]
    async fn test_invalid_json_both_times_returns_parse_error() {
        let runner = MockRunner::from_json(vec!["not valid json", "also not valid json"]);
        let working_dir = Path::new("/tmp/test-repo");
        let err = run_analysis(&runner, None, working_dir).await.unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("Failed to parse"));
        assert_eq!(runner.calls(), 2);
    }

    #[tokio::test]
    async fn test_non_parse_error_propagates_without_retry() {
        let runner = MockRunner::from_error(ActualError::ClaudeSubprocessFailed {
            message: "process crashed".to_string(),
            stderr: "segfault".to_string(),
        });
        let working_dir = Path::new("/tmp/test-repo");
        let err = run_analysis(&runner, None, working_dir).await.unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("subprocess failed"));
    }

    #[tokio::test]
    async fn test_model_override_passed_to_args() {
        let runner = MockRunner::from_json(vec![VALID_ANALYSIS_JSON]);
        let working_dir = Path::new("/tmp/test-repo");
        let result = run_analysis(&runner, Some("opus"), working_dir)
            .await
            .unwrap();

        assert_eq!(result.projects[0].name, "my-app");
        assert_eq!(runner.calls(), 1);
    }

    // -- normalize_project_path tests --

    #[test]
    fn test_normalize_absolute_path_matching_working_dir() {
        let working_dir = Path::new("/home/user/project");
        assert_eq!(
            normalize_project_path("/home/user/project", working_dir, None),
            "."
        );
    }

    #[test]
    fn test_normalize_absolute_path_subdir_of_working_dir() {
        let working_dir = Path::new("/home/user/project");
        assert_eq!(
            normalize_project_path("/home/user/project/apps/web", working_dir, None),
            "apps/web"
        );
    }

    #[test]
    fn test_normalize_canonical_fallback_root_match() {
        // Simulates macOS /var -> /private/var: working_dir is /var/folders/X
        // but Claude returns /private/var/folders/X. The raw strip_prefix fails,
        // but the canonical path matches.
        let working_dir = Path::new("/var/folders/X");
        let canonical = Path::new("/private/var/folders/X");
        assert_eq!(
            normalize_project_path("/private/var/folders/X", working_dir, Some(canonical)),
            "."
        );
    }

    #[test]
    fn test_normalize_canonical_fallback_subdir() {
        let working_dir = Path::new("/var/folders/X");
        let canonical = Path::new("/private/var/folders/X");
        assert_eq!(
            normalize_project_path(
                "/private/var/folders/X/apps/web",
                working_dir,
                Some(canonical)
            ),
            "apps/web"
        );
    }

    #[test]
    fn test_normalize_canonical_none_falls_through() {
        // When canonical is None (canonicalize failed), unrelated absolute paths
        // pass through unchanged.
        let working_dir = Path::new("/home/user/project");
        assert_eq!(
            normalize_project_path("/other/path", working_dir, None),
            "/other/path"
        );
    }

    #[test]
    fn test_normalize_canonical_some_but_no_match_falls_through() {
        // When canonical is Some but the absolute path matches neither
        // working_dir nor canonical_dir, the path passes through unchanged.
        let working_dir = Path::new("/home/user/project");
        let canonical = Path::new("/real/home/user/project");
        assert_eq!(
            normalize_project_path("/unrelated/path", working_dir, Some(canonical)),
            "/unrelated/path"
        );
    }

    #[test]
    fn test_normalize_strip_leading_dot_slash() {
        let working_dir = Path::new("/home/user/project");
        assert_eq!(
            normalize_project_path("./apps/web", working_dir, None),
            "apps/web"
        );
    }

    #[test]
    fn test_normalize_strip_trailing_slash() {
        let working_dir = Path::new("/home/user/project");
        assert_eq!(
            normalize_project_path("apps/web/", working_dir, None),
            "apps/web"
        );
    }

    #[test]
    fn test_normalize_empty_to_dot() {
        let working_dir = Path::new("/home/user/project");
        assert_eq!(normalize_project_path("", working_dir, None), ".");
    }

    #[test]
    fn test_normalize_dot_unchanged() {
        let working_dir = Path::new("/home/user/project");
        assert_eq!(normalize_project_path(".", working_dir, None), ".");
    }

    #[test]
    fn test_normalize_relative_path_unchanged() {
        let working_dir = Path::new("/home/user/project");
        assert_eq!(
            normalize_project_path("apps/web", working_dir, None),
            "apps/web"
        );
    }

    #[test]
    fn test_normalize_absolute_path_unrelated_to_working_dir() {
        let working_dir = Path::new("/home/user/project");
        assert_eq!(
            normalize_project_path("/other/path/entirely", working_dir, None),
            "/other/path/entirely"
        );
    }

    #[test]
    fn test_normalize_dot_slash_only() {
        let working_dir = Path::new("/home/user/project");
        assert_eq!(normalize_project_path("./", working_dir, None), ".");
    }

    #[tokio::test]
    async fn test_run_analysis_normalizes_absolute_paths() {
        // Use a real temp directory so canonicalize() succeeds inside
        // normalize_analysis, exercising the Ok path of canonicalize.
        let dir = tempfile::tempdir().unwrap();
        let working_dir = dir.path();
        let abs_path = working_dir.to_string_lossy().to_string();
        let json_with_absolute_path = format!(
            r#"{{
            "is_monorepo": false,
            "projects": [{{
                "path": "{}",
                "name": "my-app",
                "languages": ["rust"],
                "frameworks": [{{"name": "actix-web", "category": "web-backend"}}],
                "package_manager": "cargo",
                "description": "A web application"
            }}]
        }}"#,
            abs_path
        );

        let runner = MockRunner::from_json(vec![&json_with_absolute_path]);
        let result = run_analysis(&runner, None, working_dir).await.unwrap();

        assert_eq!(result.projects[0].path, ".");
    }

    #[tokio::test]
    async fn test_run_analysis_normalizes_absolute_subdir_path() {
        // Uses a real temp dir with a subdirectory absolute path to exercise
        // the non-empty branch of relative_or_dot through normalize_analysis.
        let dir = tempfile::tempdir().unwrap();
        let working_dir = dir.path();
        let sub_path = format!("{}/apps/web", working_dir.to_string_lossy());
        let json = format!(
            r#"{{
            "is_monorepo": true,
            "projects": [{{
                "path": "{}",
                "name": "web",
                "languages": ["typescript"],
                "frameworks": [],
                "package_manager": "npm",
                "description": "Web app"
            }}]
        }}"#,
            sub_path
        );

        let runner = MockRunner::from_json(vec![&json]);
        let result = run_analysis(&runner, None, working_dir).await.unwrap();

        assert_eq!(result.projects[0].path, "apps/web");
    }

    #[tokio::test]
    async fn test_run_analysis_normalizes_dot_slash_paths() {
        let working_dir = Path::new("/home/user/project");
        let json_with_dot_slash = r#"{
            "is_monorepo": true,
            "projects": [{
                "path": "./apps/web",
                "name": "web",
                "languages": ["typescript"],
                "frameworks": [],
                "package_manager": "npm",
                "description": "Web app"
            }, {
                "path": "./apps/api",
                "name": "api",
                "languages": ["typescript"],
                "frameworks": [],
                "package_manager": "npm",
                "description": "API"
            }]
        }"#;

        let runner = MockRunner::from_json(vec![json_with_dot_slash]);
        let result = run_analysis(&runner, None, working_dir).await.unwrap();

        assert_eq!(result.projects[0].path, "apps/web");
        assert_eq!(result.projects[1].path, "apps/api");
    }
}
