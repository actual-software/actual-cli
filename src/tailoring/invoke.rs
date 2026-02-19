use std::collections::HashSet;

use console;
use serde::Serialize;

use crate::api::types::Adr;
use crate::error::ActualError;
use crate::generation::OutputFormat;
use crate::runner::prompts::tailoring_prompt;
use crate::runner::schemas::tailoring_output_schema;
use crate::runner::TailoringRunner;
use crate::tailoring::types::TailoringOutput;

/// Invoke Claude to tailor ADRs for the given project context.
///
/// Builds the prompt and schema, calls the runner, validates the output,
/// and retries once on JSON parse failure.
pub async fn invoke_tailoring<R: TailoringRunner>(
    runner: &R,
    adrs: &[Adr],
    projects_json: &str,
    existing_output_paths: &str,
    model_override: Option<&str>,
    max_budget_usd: Option<f64>,
    format: &OutputFormat,
) -> Result<TailoringOutput, ActualError> {
    let adr_json = serialize_json(adrs, "ADRs")?;
    let prompt = build_prompt(&adr_json, projects_json, existing_output_paths, format)?;
    let schema = tailoring_output_schema()?;
    let valid_ids: HashSet<&str> = adrs.iter().map(|a| a.id.as_str()).collect();

    // First attempt
    match runner
        .run_tailoring(&prompt, &schema, model_override, max_budget_usd)
        .await
    {
        Ok(output) => validate_and_filter_output(output, &valid_ids, format),
        Err(ActualError::ClaudeOutputParse(_)) => {
            // Retry once on JSON parse failure
            let output = runner
                .run_tailoring(&prompt, &schema, model_override, max_budget_usd)
                .await?;
            validate_and_filter_output(output, &valid_ids, format)
        }
        Err(e) => Err(e),
    }
}

/// Serialize a value to JSON, mapping errors to `ActualError::InternalError`.
pub(crate) fn serialize_json<T: Serialize + ?Sized>(
    value: &T,
    label: &str,
) -> Result<String, ActualError> {
    serde_json::to_string(value)
        .map_err(|e| ActualError::InternalError(format!("Failed to serialize {label}: {e}")))
}

/// Build the tailoring prompt string from the input data.
pub(crate) fn build_prompt(
    adr_json: &str,
    projects_json: &str,
    existing_output_paths: &str,
    format: &OutputFormat,
) -> Result<String, ActualError> {
    tailoring_prompt(projects_json, existing_output_paths, adr_json, format)
}

/// Returns `true` if `path` is a valid output file path for the given `format`.
///
/// - `ClaudeMd`: path must end with `"CLAUDE.md"`
/// - `AgentsMd`: path must end with `"AGENTS.md"`
/// - `CursorRules`: path must end with `".cursor/rules/actual-policies.mdc"`
///   (or equal it, which is the same thing for relative paths)
fn is_valid_output_path(path: &str, format: &OutputFormat) -> bool {
    use crate::generation::format::CURSOR_RULES_PATH;
    match format {
        OutputFormat::CursorRules => {
            // Accept the exact relative path or a subdirectory variant like
            // "apps/web/.cursor/rules/actual-policies.mdc".
            path.ends_with(CURSOR_RULES_PATH)
        }
        _ => path.ends_with(format.filename()),
    }
}

/// Validate the tailoring output against the input ADR set.
///
/// - Errors on empty content or invalid file paths (these are fatal).
/// - Filters out unknown ADR IDs with a warning (defense-in-depth against LLM hallucination).
pub(crate) fn validate_and_filter_output(
    mut output: TailoringOutput,
    valid_ids: &HashSet<&str>,
    format: &OutputFormat,
) -> Result<TailoringOutput, ActualError> {
    let expected_filename = format.filename();
    for file in &mut output.files {
        if file.content.is_empty() {
            return Err(ActualError::TailoringValidationError(format!(
                "file '{}' has empty content",
                file.path
            )));
        }
        if !is_valid_output_path(&file.path, format) {
            return Err(ActualError::TailoringValidationError(format!(
                "file path '{}' does not end with {expected_filename}",
                file.path
            )));
        }
        let has_unsafe_component = std::path::Path::new(&file.path).components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        });
        if has_unsafe_component {
            let is_absolute = std::path::Path::new(&file.path).is_absolute();
            let reason = if is_absolute {
                "absolute paths are not allowed"
            } else {
                "path contains path traversal components"
            };
            return Err(ActualError::TailoringValidationError(format!(
                "file path '{}': {reason}",
                file.path
            )));
        }
        // Sanitize LLM-generated path to prevent ANSI injection in terminal output
        file.path = console::strip_ansi_codes(&file.path).into_owned();
        let invalid_ids: Vec<String> = file
            .adr_ids
            .iter()
            .filter(|id| !valid_ids.contains(id.as_str()))
            .cloned()
            .collect();
        if !invalid_ids.is_empty() {
            eprintln!(
                "  warning: filtered {} hallucinated ADR ID(s) from '{}': {}",
                invalid_ids.len(),
                console::strip_ansi_codes(&file.path),
                invalid_ids
                    .iter()
                    .map(|id| console::strip_ansi_codes(id).into_owned())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            file.adr_ids.retain(|id| valid_ids.contains(id.as_str()));
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{AdrCategory, AppliesTo};
    use crate::generation::OutputFormat;
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
    struct MockTailoringRunner {
        responses: Mutex<Vec<MockResponse>>,
        call_count: AtomicU32,
    }

    impl MockTailoringRunner {
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

    impl TailoringRunner for MockTailoringRunner {
        async fn run_tailoring(
            &self,
            _prompt: &str,
            _schema: &str,
            _model_override: Option<&str>,
            _max_budget_usd: Option<f64>,
        ) -> Result<TailoringOutput, ActualError> {
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst) as usize;
            let mut responses = self.responses.lock().unwrap();
            // Take ownership by swapping with a placeholder
            let entry = std::mem::replace(&mut responses[idx], MockResponse::Json(String::new()));
            match entry {
                MockResponse::Json(json) => {
                    let parsed: TailoringOutput = serde_json::from_str(&json)?;
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
        let runner = MockTailoringRunner::single(&json);

        let result = invoke_tailoring(
            &runner,
            &adrs,
            "{}",
            "",
            None,
            None,
            &OutputFormat::ClaudeMd,
        )
        .await;

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
        let runner = MockTailoringRunner::single(&json);

        let result = invoke_tailoring(
            &runner,
            &adrs,
            "{}",
            "",
            None,
            None,
            &OutputFormat::ClaudeMd,
        )
        .await;

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
        let runner = MockTailoringRunner::single(&json);

        let result = invoke_tailoring(
            &runner,
            &adrs,
            "{}",
            "",
            None,
            None,
            &OutputFormat::ClaudeMd,
        )
        .await;

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("CLAUDE.md"), "expected 'CLAUDE.md' in: {msg}");
        assert!(matches!(err, ActualError::TailoringValidationError(_)));
    }

    #[tokio::test]
    async fn test_unknown_adr_id_filtered_not_error() {
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
        let runner = MockTailoringRunner::single(&json);

        let result = invoke_tailoring(
            &runner,
            &adrs,
            "{}",
            "",
            None,
            None,
            &OutputFormat::ClaudeMd,
        )
        .await;

        let output = result.unwrap();
        // The invalid ID should be filtered out, leaving only the valid one
        assert_eq!(output.files[0].adr_ids, vec!["adr-001"]);
    }

    #[tokio::test]
    async fn test_all_adr_ids_hallucinated_results_in_empty_ids() {
        let adrs = vec![make_adr("adr-001")];
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                content: "# Rules".to_string(),
                reasoning: "Root level rules".to_string(),
                adr_ids: vec!["adr-999".to_string(), "adr-888".to_string()],
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
        let runner = MockTailoringRunner::single(&json);

        let result = invoke_tailoring(
            &runner,
            &adrs,
            "{}",
            "",
            None,
            None,
            &OutputFormat::ClaudeMd,
        )
        .await;

        let output = result.unwrap();
        assert!(output.files[0].adr_ids.is_empty());
    }

    #[tokio::test]
    async fn test_retry_on_invalid_json() {
        let adrs = vec![make_adr("adr-001")];
        let valid_json = valid_output_json(&["adr-001"]);
        let runner =
            MockTailoringRunner::from_json_strings(vec!["not valid json".to_string(), valid_json]);

        let result = invoke_tailoring(
            &runner,
            &adrs,
            "{}",
            "",
            None,
            None,
            &OutputFormat::ClaudeMd,
        )
        .await;

        let output = result.unwrap();
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "CLAUDE.md");
        assert_eq!(runner.call_count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_serialize_json_success() {
        let adrs = vec![make_adr("adr-001")];
        let result = serialize_json(&adrs, "ADRs");
        assert!(result.is_ok());
        let json = result.unwrap();
        assert!(json.contains("adr-001"));
    }

    #[test]
    fn test_serialize_json_error() {
        use serde::ser::{self, Serializer};

        // A type whose Serialize impl always fails.
        struct AlwaysFails;
        impl Serialize for AlwaysFails {
            fn serialize<S: Serializer>(&self, _s: S) -> Result<S::Ok, S::Error> {
                Err(ser::Error::custom("deliberate failure"))
            }
        }

        let result = serialize_json(&AlwaysFails, "test");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Failed to serialize test"),
            "expected 'Failed to serialize test' in: {msg}"
        );
        assert!(matches!(err, ActualError::InternalError(_)));
    }

    #[test]
    fn test_build_prompt_with_valid_input() {
        let adr_json = r#"[{"id":"adr-001"},{"id":"adr-002"}]"#;
        let prompt = build_prompt(adr_json, "{}", "", &OutputFormat::ClaudeMd)
            .expect("build_prompt must succeed for valid obfuscated constant");

        // The prompt should contain the ADR JSON
        assert!(
            prompt.contains("adr-001"),
            "prompt should contain ADR ID adr-001"
        );
        assert!(
            prompt.contains("adr-002"),
            "prompt should contain ADR ID adr-002"
        );
    }

    #[tokio::test]
    async fn test_path_traversal_validation_error() {
        let adrs = vec![make_adr("adr-001")];
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "../CLAUDE.md".to_string(),
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
        let runner = MockTailoringRunner::single(&json);

        let result = invoke_tailoring(
            &runner,
            &adrs,
            "{}",
            "",
            None,
            None,
            &OutputFormat::ClaudeMd,
        )
        .await;

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("path traversal"),
            "expected 'path traversal' in: {msg}"
        );
        assert!(matches!(err, ActualError::TailoringValidationError(_)));
    }

    #[tokio::test]
    async fn test_non_parse_error_not_retried() {
        let adrs = vec![make_adr("adr-001")];
        let runner = MockTailoringRunner::new(vec![MockResponse::Error(
            ActualError::ClaudeSubprocessFailed {
                message: "process crashed".to_string(),
                stderr: "segfault".to_string(),
            },
        )]);

        let result = invoke_tailoring(
            &runner,
            &adrs,
            "{}",
            "",
            None,
            None,
            &OutputFormat::ClaudeMd,
        )
        .await;

        let err = result.unwrap_err();
        assert!(matches!(err, ActualError::ClaudeSubprocessFailed { .. }));
        assert_eq!(runner.call_count.load(Ordering::SeqCst), 1);
    }

    // ── format-aware validate_and_filter_output tests ──

    #[tokio::test]
    async fn test_agents_md_format_accepts_agents_md_path() {
        let adrs = vec![make_adr("adr-001")];
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "AGENTS.md".to_string(),
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
        let runner = MockTailoringRunner::single(&json);

        let result = invoke_tailoring(
            &runner,
            &adrs,
            "{}",
            "",
            None,
            None,
            &OutputFormat::AgentsMd,
        )
        .await;

        let output = result.unwrap();
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "AGENTS.md");
    }

    #[tokio::test]
    async fn test_agents_md_format_accepts_subdir_agents_md_path() {
        let adrs = vec![make_adr("adr-001")];
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "subdir/AGENTS.md".to_string(),
                content: "# Rules".to_string(),
                reasoning: "Subdir rules".to_string(),
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
        let runner = MockTailoringRunner::single(&json);

        let result = invoke_tailoring(
            &runner,
            &adrs,
            "{}",
            "",
            None,
            None,
            &OutputFormat::AgentsMd,
        )
        .await;

        let output = result.unwrap();
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "subdir/AGENTS.md");
    }

    #[tokio::test]
    async fn test_agents_md_format_rejects_claude_md_path() {
        let adrs = vec![make_adr("adr-001")];
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "CLAUDE.md".to_string(),
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
        let runner = MockTailoringRunner::single(&json);

        let result = invoke_tailoring(
            &runner,
            &adrs,
            "{}",
            "",
            None,
            None,
            &OutputFormat::AgentsMd,
        )
        .await;

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("AGENTS.md"),
            "expected 'AGENTS.md' in error for agents-md format, got: {msg}"
        );
        assert!(matches!(err, ActualError::TailoringValidationError(_)));
    }

    // ── CursorRules format validation tests ──

    #[tokio::test]
    async fn test_cursor_rules_format_accepts_mdc_path() {
        let adrs = vec![make_adr("adr-001")];
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: ".cursor/rules/actual-policies.mdc".to_string(),
                content: "# Rules\n\nUse Tailwind.".to_string(),
                reasoning: "Cursor rules".to_string(),
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
        let runner = MockTailoringRunner::single(&json);

        let result = invoke_tailoring(
            &runner,
            &adrs,
            "{}",
            "",
            None,
            None,
            &OutputFormat::CursorRules,
        )
        .await;

        let output = result.unwrap();
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, ".cursor/rules/actual-policies.mdc");
    }

    #[tokio::test]
    async fn test_cursor_rules_format_accepts_subdir_mdc_path() {
        let adrs = vec![make_adr("adr-001")];
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "apps/web/.cursor/rules/actual-policies.mdc".to_string(),
                content: "# Rules\n\nUse React.".to_string(),
                reasoning: "Web cursor rules".to_string(),
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
        let runner = MockTailoringRunner::single(&json);

        let result = invoke_tailoring(
            &runner,
            &adrs,
            "{}",
            "",
            None,
            None,
            &OutputFormat::CursorRules,
        )
        .await;

        let output = result.unwrap();
        assert_eq!(output.files.len(), 1);
        assert_eq!(
            output.files[0].path,
            "apps/web/.cursor/rules/actual-policies.mdc"
        );
    }

    #[tokio::test]
    async fn test_cursor_rules_format_rejects_claude_md_path() {
        let adrs = vec![make_adr("adr-001")];
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                content: "# Rules".to_string(),
                reasoning: "Wrong format".to_string(),
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
        let runner = MockTailoringRunner::single(&json);

        let result = invoke_tailoring(
            &runner,
            &adrs,
            "{}",
            "",
            None,
            None,
            &OutputFormat::CursorRules,
        )
        .await;

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(".cursor/rules/actual-policies.mdc"),
            "expected cursor rules path in error, got: {msg}"
        );
        assert!(matches!(err, ActualError::TailoringValidationError(_)));
    }

    #[tokio::test]
    async fn test_cursor_rules_format_rejects_agents_md_path() {
        let adrs = vec![make_adr("adr-001")];
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "AGENTS.md".to_string(),
                content: "# Rules".to_string(),
                reasoning: "Wrong format".to_string(),
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
        let runner = MockTailoringRunner::single(&json);

        let result = invoke_tailoring(
            &runner,
            &adrs,
            "{}",
            "",
            None,
            None,
            &OutputFormat::CursorRules,
        )
        .await;

        let err = result.unwrap_err();
        assert!(matches!(err, ActualError::TailoringValidationError(_)));
    }
}
