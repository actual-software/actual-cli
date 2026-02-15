use crate::analysis::types::RepoAnalysis;
use crate::claude::options::InvocationOptions;
use crate::claude::prompts::analysis_prompt;
use crate::claude::schemas::REPO_ANALYSIS_SCHEMA;
use crate::claude::subprocess::ClaudeRunner;
use crate::error::ActualError;

/// Run repository analysis using Claude Code.
///
/// Builds the analysis prompt + invocation options, calls the runner,
/// and validates the result. Retries once on JSON parse failure.
pub async fn run_analysis(
    runner: &impl ClaudeRunner,
    model_override: Option<&str>,
) -> Result<RepoAnalysis, ActualError> {
    let mut opts = InvocationOptions::for_analysis(model_override);
    opts.json_schema = Some(REPO_ANALYSIS_SCHEMA.to_string());

    let prompt = analysis_prompt();
    let mut args = vec![prompt];
    args.extend(opts.to_args());

    // First attempt
    let result: Result<RepoAnalysis, ActualError> = runner.run(&args).await;

    let analysis = match result {
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

    /// Mock runner that returns a sequence of responses.
    /// Each call pops the next response from the queue.
    struct SequenceMockRunner {
        responses: Mutex<Vec<String>>,
        call_count: AtomicU32,
    }

    impl SequenceMockRunner {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().map(String::from).collect()),
                call_count: AtomicU32::new(0),
            }
        }

        fn calls(&self) -> u32 {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    impl ClaudeRunner for SequenceMockRunner {
        async fn run<T: DeserializeOwned + Send>(
            &self,
            _args: &[String],
        ) -> Result<T, ActualError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            let response = {
                let mut responses = self.responses.lock().unwrap();
                responses.remove(0)
            };
            let parsed: T = serde_json::from_str(&response)?;
            Ok(parsed)
        }
    }

    #[tokio::test]
    async fn test_valid_analysis_returns_correct_result() {
        let runner = SequenceMockRunner::new(vec![VALID_ANALYSIS_JSON]);
        let result = run_analysis(&runner, None).await.unwrap();

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
        let runner = SequenceMockRunner::new(vec![EMPTY_PROJECTS_JSON]);
        let result = run_analysis(&runner, None).await;

        assert!(matches!(result, Err(ActualError::AnalysisEmpty)));
        assert_eq!(runner.calls(), 1);
    }

    #[tokio::test]
    async fn test_invalid_json_then_valid_retries_successfully() {
        let runner = SequenceMockRunner::new(vec!["not valid json", VALID_ANALYSIS_JSON]);
        let result = run_analysis(&runner, None).await.unwrap();

        assert_eq!(result.projects.len(), 1);
        assert_eq!(result.projects[0].name, "my-app");
        assert_eq!(runner.calls(), 2);
    }

    #[tokio::test]
    async fn test_invalid_json_both_times_returns_parse_error() {
        let runner = SequenceMockRunner::new(vec!["not valid json", "also not valid json"]);
        let result = run_analysis(&runner, None).await;

        assert!(matches!(result, Err(ActualError::ClaudeOutputParse(_))));
        assert_eq!(runner.calls(), 2);
    }

    /// Mock runner that always returns a specific error (non-parse).
    struct ErrorMockRunner;

    impl ClaudeRunner for ErrorMockRunner {
        async fn run<T: DeserializeOwned + Send>(
            &self,
            _args: &[String],
        ) -> Result<T, ActualError> {
            Err(ActualError::ClaudeSubprocessFailed {
                message: "process crashed".to_string(),
                stderr: "segfault".to_string(),
            })
        }
    }

    #[tokio::test]
    async fn test_non_parse_error_propagates_without_retry() {
        let runner = ErrorMockRunner;
        let result = run_analysis(&runner, None).await;

        assert!(matches!(
            result,
            Err(ActualError::ClaudeSubprocessFailed { .. })
        ));
    }

    #[tokio::test]
    async fn test_model_override_passed_to_args() {
        let runner = SequenceMockRunner::new(vec![VALID_ANALYSIS_JSON]);
        let result = run_analysis(&runner, Some("opus")).await.unwrap();

        assert_eq!(result.projects[0].name, "my-app");
        assert_eq!(runner.calls(), 1);
    }
}
