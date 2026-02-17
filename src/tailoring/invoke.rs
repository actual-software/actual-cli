use std::collections::HashSet;

use crate::api::types::Adr;
use crate::claude::prompts::tailoring_prompt;
use crate::claude::schemas::TAILORING_OUTPUT_SCHEMA;
use crate::claude::ClaudeRunner;
use crate::claude::InvocationOptions;
use crate::error::ActualError;
use crate::tailoring::types::TailoringOutput;

/// Invoke Claude to tailor ADRs for the given project context.
///
/// Builds the prompt and CLI args, calls the runner, validates the output,
/// and retries once on JSON parse failure.
pub async fn invoke_tailoring<R: ClaudeRunner>(
    runner: &R,
    adrs: &[Adr],
    projects_json: &str,
    existing_claude_md_paths: &str,
    model_override: Option<&str>,
    max_budget_usd: Option<f64>,
) -> Result<TailoringOutput, ActualError> {
    let args = build_args(
        adrs,
        projects_json,
        existing_claude_md_paths,
        model_override,
        max_budget_usd,
    );
    let valid_ids: HashSet<&str> = adrs.iter().map(|a| a.id.as_str()).collect();

    // First attempt
    match runner.run::<TailoringOutput>(&args).await {
        Ok(mut output) => {
            validate_output(&mut output, &valid_ids)?;
            Ok(output)
        }
        Err(ActualError::ClaudeOutputParse(_)) => {
            // Retry once on JSON parse failure
            let mut output: TailoringOutput = runner.run(&args).await?;
            validate_output(&mut output, &valid_ids)?;
            Ok(output)
        }
        Err(e) => Err(e),
    }
}

/// Build the CLI argument list for the tailoring invocation.
fn build_args(
    adrs: &[Adr],
    projects_json: &str,
    existing_claude_md_paths: &str,
    model_override: Option<&str>,
    max_budget_usd: Option<f64>,
) -> Vec<String> {
    let mut opts = InvocationOptions::for_tailoring(model_override);
    opts.json_schema = Some(TAILORING_OUTPUT_SCHEMA.to_string());
    opts.max_budget_usd = max_budget_usd;

    let adr_json = serde_json::to_string(adrs).expect("ADR serialization should not fail");
    let prompt = tailoring_prompt(projects_json, existing_claude_md_paths, &adr_json);

    let mut args = opts.to_args();
    args.push("-p".to_string());
    args.push(prompt);
    args
}

/// Validate the tailoring output against the input ADR set.
///
/// Empty content and wrong file paths are fatal errors. Unknown ADR IDs are
/// filtered out with a warning (they are likely hallucinated from existing
/// CLAUDE.md metadata).
fn validate_output(
    output: &mut TailoringOutput,
    valid_ids: &HashSet<&str>,
) -> Result<(), ActualError> {
    for file in &mut output.files {
        if file.content.is_empty() {
            return Err(ActualError::TailoringValidationError(format!(
                "file '{}' has empty content",
                file.path
            )));
        }
        if !file.path.ends_with("CLAUDE.md") {
            return Err(ActualError::TailoringValidationError(format!(
                "file path '{}' does not end with CLAUDE.md",
                file.path
            )));
        }
        let before_count = file.adr_ids.len();
        file.adr_ids.retain(|id| {
            if valid_ids.contains(id.as_str()) {
                true
            } else {
                tracing::warn!(
                    adr_id = %id,
                    file = %file.path,
                    "Filtered out unknown ADR ID from tailoring output (likely hallucinated)"
                );
                false
            }
        });
        if file.adr_ids.len() < before_count {
            tracing::info!(
                file = %file.path,
                filtered = before_count - file.adr_ids.len(),
                remaining = file.adr_ids.len(),
                "Filtered hallucinated ADR IDs from tailoring output"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{AdrCategory, AppliesTo};
    use crate::tailoring::types::{FileOutput, TailoringSummary};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    /// A response entry for the mock runner: either a JSON string to parse,
    /// or a non-parse error to return directly.
    enum MockResponse {
        Json(String),
        Error(ActualError),
    }

    /// Mock runner that returns pre-configured responses in sequence.
    /// First call returns responses[0], second returns responses[1], etc.
    struct MockClaudeRunner {
        responses: Mutex<Vec<MockResponse>>,
        call_count: AtomicU32,
    }

    impl MockClaudeRunner {
        fn new(responses: Vec<MockResponse>) -> Self {
            Self {
                responses: Mutex::new(responses),
                call_count: AtomicU32::new(0),
            }
        }

        fn single(json: &str) -> Self {
            Self::new(vec![MockResponse::Json(json.to_string())])
        }

        fn from_json_strings(strings: Vec<String>) -> Self {
            Self::new(strings.into_iter().map(MockResponse::Json).collect())
        }
    }

    impl ClaudeRunner for MockClaudeRunner {
        async fn run<T: serde::de::DeserializeOwned + Send>(
            &self,
            _args: &[String],
        ) -> Result<T, ActualError> {
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst) as usize;
            let mut responses = self.responses.lock().unwrap();
            // Take ownership by swapping with a placeholder
            let entry = std::mem::replace(&mut responses[idx], MockResponse::Json(String::new()));
            match entry {
                MockResponse::Json(json) => {
                    let parsed: T = serde_json::from_str(&json)?;
                    Ok(parsed)
                }
                MockResponse::Error(e) => Err(e),
            }
        }
    }

    fn make_adr(id: &str) -> Adr {
        Adr {
            id: id.to_string(),
            title: format!("ADR {id}"),
            context: None,
            policies: vec!["policy".to_string()],
            instructions: None,
            category: AdrCategory {
                id: "cat-1".to_string(),
                name: "General".to_string(),
                path: "General".to_string(),
            },
            applies_to: AppliesTo {
                languages: vec![],
                frameworks: vec![],
            },
            matched_projects: vec![],
        }
    }

    fn valid_output_json(adr_ids: &[&str]) -> String {
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                content: "# Rules\n\nFollow standards.".to_string(),
                reasoning: "Root level rules".to_string(),
                adr_ids: adr_ids.iter().map(|s| s.to_string()).collect(),
            }],
            skipped_adrs: vec![],
            summary: TailoringSummary {
                total_input: adr_ids.len(),
                applicable: adr_ids.len(),
                not_applicable: 0,
                files_generated: 1,
            },
        };
        serde_json::to_string(&output).unwrap()
    }

    #[tokio::test]
    async fn test_valid_output() {
        let adrs = vec![make_adr("adr-001"), make_adr("adr-002")];
        let json = valid_output_json(&["adr-001", "adr-002"]);
        let runner = MockClaudeRunner::single(&json);

        let result = invoke_tailoring(&runner, &adrs, "{}", "", None, None).await;

        let output = result.unwrap();
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "CLAUDE.md");
        assert_eq!(output.files[0].adr_ids, vec!["adr-001", "adr-002"]);
        assert_eq!(output.summary.total_input, 2);
        assert_eq!(output.summary.files_generated, 1);
    }

    #[tokio::test]
    async fn test_empty_content_validation_error() {
        let adrs = vec![make_adr("adr-001")];
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                content: String::new(),
                reasoning: "Root level rules".to_string(),
                adr_ids: vec!["adr-001".to_string()],
            }],
            skipped_adrs: vec![],
            summary: TailoringSummary {
                total_input: 1,
                applicable: 1,
                not_applicable: 0,
                files_generated: 1,
            },
        };
        let json = serde_json::to_string(&output).unwrap();
        let runner = MockClaudeRunner::single(&json);

        let result = invoke_tailoring(&runner, &adrs, "{}", "", None, None).await;

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("empty content"),
            "expected 'empty content' in: {msg}"
        );
        assert!(matches!(err, ActualError::TailoringValidationError(_)));
    }

    #[tokio::test]
    async fn test_wrong_path_validation_error() {
        let adrs = vec![make_adr("adr-001")];
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "README.md".to_string(),
                content: "# Rules".to_string(),
                reasoning: "Root level rules".to_string(),
                adr_ids: vec!["adr-001".to_string()],
            }],
            skipped_adrs: vec![],
            summary: TailoringSummary {
                total_input: 1,
                applicable: 1,
                not_applicable: 0,
                files_generated: 1,
            },
        };
        let json = serde_json::to_string(&output).unwrap();
        let runner = MockClaudeRunner::single(&json);

        let result = invoke_tailoring(&runner, &adrs, "{}", "", None, None).await;

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("CLAUDE.md"), "expected 'CLAUDE.md' in: {msg}");
        assert!(matches!(err, ActualError::TailoringValidationError(_)));
    }

    #[tokio::test]
    async fn test_unknown_adr_id_is_filtered() {
        let adrs = vec![make_adr("adr-001")];
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                content: "# Rules".to_string(),
                reasoning: "Root level rules".to_string(),
                adr_ids: vec!["adr-001".to_string(), "adr-999".to_string()],
            }],
            skipped_adrs: vec![],
            summary: TailoringSummary {
                total_input: 1,
                applicable: 1,
                not_applicable: 0,
                files_generated: 1,
            },
        };
        let json = serde_json::to_string(&output).unwrap();
        let runner = MockClaudeRunner::single(&json);

        let result = invoke_tailoring(&runner, &adrs, "{}", "", None, None).await;

        // Should succeed with the hallucinated ID filtered out
        let output = result.unwrap();
        assert_eq!(output.files[0].adr_ids, vec!["adr-001"]);
    }

    #[tokio::test]
    async fn test_hallucinated_adr_ids_filtered_from_output() {
        let adrs = vec![make_adr("adr-001"), make_adr("adr-002")];
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                content: "# Rules\nFollow standards.".to_string(),
                reasoning: "Root level rules".to_string(),
                adr_ids: vec![
                    "adr-001".to_string(),
                    "adr-hallucinated".to_string(),
                    "adr-002".to_string(),
                    "adr-fake".to_string(),
                ],
            }],
            skipped_adrs: vec![],
            summary: TailoringSummary {
                total_input: 2,
                applicable: 2,
                not_applicable: 0,
                files_generated: 1,
            },
        };
        let json = serde_json::to_string(&output).unwrap();
        let runner = MockClaudeRunner::single(&json);

        let result = invoke_tailoring(&runner, &adrs, "{}", "", None, None).await;

        let output = result.unwrap();
        // Only valid IDs should remain
        assert_eq!(output.files[0].adr_ids, vec!["adr-001", "adr-002"]);
    }

    #[tokio::test]
    async fn test_retry_on_invalid_json() {
        let adrs = vec![make_adr("adr-001")];
        let valid_json = valid_output_json(&["adr-001"]);
        let runner =
            MockClaudeRunner::from_json_strings(vec!["not valid json".to_string(), valid_json]);

        let result = invoke_tailoring(&runner, &adrs, "{}", "", None, None).await;

        let output = result.unwrap();
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "CLAUDE.md");
        assert_eq!(runner.call_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_non_parse_error_not_retried() {
        let adrs = vec![make_adr("adr-001")];
        let runner = MockClaudeRunner::new(vec![MockResponse::Error(
            ActualError::ClaudeSubprocessFailed {
                message: "process crashed".to_string(),
                stderr: "segfault".to_string(),
            },
        )]);

        let result = invoke_tailoring(&runner, &adrs, "{}", "", None, None).await;

        let err = result.unwrap_err();
        assert!(matches!(err, ActualError::ClaudeSubprocessFailed { .. }));
        assert_eq!(runner.call_count.load(Ordering::SeqCst), 1);
    }
}
