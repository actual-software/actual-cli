use std::collections::HashSet;

use console;
use serde::Serialize;

use crate::api::types::Adr;
use crate::error::ActualError;
use crate::generation::OutputFormat;
use crate::runner::prompts::tailoring_prompt;
use crate::runner::schemas::tailoring_output_schema;
use crate::runner::TailoringRunner;
use crate::tailoring::context_bundler::RepoBundleContext;
use crate::tailoring::types::TailoringOutput;

/// Format a [`RepoBundleContext`] into a string suitable for injection into the prompt.
///
/// The output is a series of `=== <name> ===` sections: first the file tree,
/// then each key file's content.
fn format_bundle_context(ctx: &RepoBundleContext) -> String {
    let mut out = String::new();
    out.push_str("=== file_tree ===\n");
    out.push_str(&ctx.file_tree);
    out.push('\n');
    for (path, content) in &ctx.key_files {
        out.push_str(&format!("\n=== {path} ===\n{content}\n"));
    }
    out
}

/// Invoke Claude to tailor ADRs for the given project context.
///
/// Builds the prompt and schema, calls the runner, validates the output,
/// and retries once on JSON parse failure.
///
/// If `root_dir` is `Some`, the repository context is pre-bundled via
/// [`bundle_context`] and injected into the prompt. On failure, bundling is
/// skipped with a warning and tailoring continues without bundled context.
#[allow(clippy::too_many_arguments)]
pub async fn invoke_tailoring<R: TailoringRunner>(
    runner: &R,
    adrs: &[Adr],
    projects_json: &str,
    existing_output_paths: &str,
    model_override: Option<&str>,
    max_budget_usd: Option<f64>,
    format: &OutputFormat,
    root_dir: Option<&std::path::Path>,
) -> Result<TailoringOutput, ActualError> {
    // Bundle repository context if a root directory is provided.
    let bundled_context_owned;
    let bundled_context = if let Some(dir) = root_dir {
        match crate::tailoring::context_bundler::bundle_context(dir) {
            Ok(ctx) => {
                bundled_context_owned = format_bundle_context(&ctx);
                &bundled_context_owned
            }
            Err(e) => {
                tracing::warn!("context bundling failed, continuing without it: {e}");
                bundled_context_owned = String::new();
                &bundled_context_owned
            }
        }
    } else {
        bundled_context_owned = String::new();
        &bundled_context_owned
    };

    let adr_json = serialize_json(adrs, "ADRs")?;
    // Note: split onto two lines so LLVM coverage instruments the call site and
    // the `?` propagation separately, avoiding a false "missed" line report.
    let prompt_result = build_prompt(
        &adr_json,
        projects_json,
        existing_output_paths,
        format,
        bundled_context,
    );
    let prompt = prompt_result?;

    let schema = tailoring_output_schema()?;
    let valid_ids: HashSet<&str> = adrs.iter().map(|a| a.id.as_str()).collect();

    // First attempt; retry once on JSON parse failure or structured output exhaustion.
    let output = match runner
        .run_tailoring(&prompt, &schema, model_override, max_budget_usd)
        .await
    {
        Ok(output) => output,
        Err(ActualError::RunnerOutputParse(_)) => {
            runner
                .run_tailoring(&prompt, &schema, model_override, max_budget_usd)
                .await?
        }
        Err(ActualError::RunnerFailed { ref message, .. })
            if message.contains("error_max_structured_output_retries") =>
        {
            tracing::warn!("structured output retries exhausted, retrying invocation");
            runner
                .run_tailoring(&prompt, &schema, model_override, max_budget_usd)
                .await?
        }
        Err(e) => return Err(e),
    };

    let result = validate_and_filter_output(output, &valid_ids, format)?;

    // Log reasoning for each output file after the full result is assembled.
    for file in &result.files {
        if !file.reasoning.is_empty() {
            tracing::debug!(path = %file.path, reasoning = %file.reasoning, "tailoring reasoning");
        }
    }

    Ok(result)
}

/// Serialize a value to JSON, mapping errors to `ActualError::InternalError`.
pub(crate) fn serialize_json<T: Serialize + ?Sized>(
    value: &T,
    label: &str,
) -> Result<String, ActualError> {
    serde_json::to_string(value)
        .map_err(|e| ActualError::InternalError(format!("Failed to serialize {label}: {e}")))
}

/// Convert a raw prompt-decode result into the canonical `ActualError`.
///
/// Extracted so the error-path conversion can be unit-tested without
/// having to corrupt the real obfuscated prompt constant.
fn prompt_result_to_error(raw: Result<String, ActualError>) -> Result<String, ActualError> {
    raw.map_err(|e| {
        ActualError::InternalError(format!(
            "Tailoring prompt constant is malformed (build artifact mismatch). Please reinstall. Error: {e}"
        ))
    })
}

/// Build the tailoring prompt string from the input data.
///
/// Returns `Err(ActualError::InternalError)` if the obfuscated prompt constant is
/// malformed (build artifact mismatch). This should never happen in a correctly built
/// binary.
pub(crate) fn build_prompt(
    adr_json: &str,
    projects_json: &str,
    existing_output_paths: &str,
    format: &OutputFormat,
    bundled_context: &str,
) -> Result<String, ActualError> {
    prompt_result_to_error(tailoring_prompt(
        projects_json,
        existing_output_paths,
        adr_json,
        format,
        bundled_context,
    ))
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
        if file.sections.is_empty() || file.sections.iter().all(|s| s.content.is_empty()) {
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
        // Filter out sections with hallucinated ADR IDs
        let invalid_ids: Vec<String> = file
            .sections
            .iter()
            .filter(|s| !valid_ids.contains(s.adr_id.as_str()))
            .map(|s| s.adr_id.clone())
            .collect();
        if !invalid_ids.is_empty() {
            log_hallucinated_adr_ids(&file.path, &invalid_ids);
            file.sections
                .retain(|s| valid_ids.contains(s.adr_id.as_str()));
        }
        // Deduplicate sections by adr_id (last wins)
        let mut seen = std::collections::HashSet::new();
        let mut deduped = Vec::new();
        for section in file.sections.drain(..).rev() {
            if seen.insert(section.adr_id.clone()) {
                deduped.push(section);
            }
        }
        deduped.reverse();
        file.sections = deduped;
    }
    // Remove files whose sections were all filtered out (e.g. all ADR IDs were hallucinated)
    let before = output.files.len();
    output.files.retain(|f| !f.sections.is_empty());
    let removed = before - output.files.len();
    if removed > 0 {
        tracing::warn!(
            "removed {removed} file(s) with no remaining sections after filtering hallucinated ADR IDs"
        );
    }
    Ok(output)
}

fn log_hallucinated_adr_ids(path: &str, invalid_ids: &[String]) {
    let count = invalid_ids.len();
    let clean_path = console::strip_ansi_codes(path);
    let ids = invalid_ids
        .iter()
        .map(|id| console::strip_ansi_codes(id).into_owned())
        .collect::<Vec<_>>()
        .join(", ");
    tracing::warn!("filtered {count} hallucinated ADR ID(s) from '{clean_path}': {ids}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{AdrCategory, AppliesTo};
    use crate::generation::OutputFormat;
    use crate::tailoring::types::{AdrSection, FileOutput, TailoringSummary};
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
                sections: adr_ids
                    .iter()
                    .map(|id| AdrSection {
                        adr_id: id.to_string(),
                        content: "# Rules\n\nFollow standards.".to_string(),
                    })
                    .collect(),
                reasoning: "Root level rules".to_string(),
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
            None,
        )
        .await;

        let output = result.unwrap();
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "CLAUDE.md");
        assert_eq!(output.files[0].adr_ids(), vec!["adr-001", "adr-002"]);
        assert_eq!(output.summary.total_input, 2);
        assert_eq!(output.summary.files_generated, 1);
    }

    #[tokio::test]
    async fn test_empty_content_validation_error() {
        let adrs = vec![make_adr("adr-001")];
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                sections: vec![AdrSection {
                    adr_id: "adr-001".to_string(),
                    content: String::new(),
                }],
                reasoning: "Root level rules".to_string(),
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
            None,
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
                sections: vec![AdrSection {
                    adr_id: "adr-001".to_string(),
                    content: "# Rules".to_string(),
                }],
                reasoning: "Root level rules".to_string(),
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
            None,
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
                sections: vec![
                    AdrSection {
                        adr_id: "adr-001".to_string(),
                        content: "# Rules".to_string(),
                    },
                    AdrSection {
                        adr_id: "adr-999".to_string(),
                        content: "# Rules".to_string(),
                    },
                ],
                reasoning: "Root level rules".to_string(),
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
            None,
        )
        .await;

        let output = result.unwrap();
        // The invalid ID should be filtered out, leaving only the valid one
        assert_eq!(output.files[0].adr_ids(), vec!["adr-001"]);
    }

    #[tokio::test]
    async fn test_all_adr_ids_hallucinated_results_in_empty_ids() {
        let adrs = vec![make_adr("adr-001")];
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                sections: vec![
                    AdrSection {
                        adr_id: "adr-999".to_string(),
                        content: "# Rules".to_string(),
                    },
                    AdrSection {
                        adr_id: "adr-888".to_string(),
                        content: "# Rules".to_string(),
                    },
                ],
                reasoning: "Root level rules".to_string(),
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
            None,
        )
        .await;

        let output = result.unwrap();
        assert!(
            output.files.is_empty(),
            "files with all sections filtered out should be excluded"
        );
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
            None,
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
    fn test_prompt_result_to_error_propagates_err_as_internal_error() {
        let raw: Result<String, ActualError> =
            Err(ActualError::InternalError("decode failed".to_string()));
        let result = prompt_result_to_error(raw);
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("malformed"),
            "expected 'malformed' in error message: {msg}"
        );
        // Verify the original error is not silently discarded
        assert!(
            msg.contains("decode failed"),
            "expected original error 'decode failed' to be preserved in: {msg}"
        );
        assert!(matches!(err, ActualError::InternalError(_)));
    }

    #[test]
    fn test_prompt_result_to_error_passes_ok_through() {
        let raw: Result<String, ActualError> = Ok("hello prompt".to_string());
        let result = prompt_result_to_error(raw);
        assert_eq!(result.unwrap(), "hello prompt");
    }

    #[test]
    fn test_build_prompt_with_valid_input() {
        let adr_json = r#"[{"id":"adr-001"},{"id":"adr-002"}]"#;
        let prompt = build_prompt(adr_json, "{}", "", &OutputFormat::ClaudeMd, "")
            .expect("build_prompt should succeed with valid input");

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
                sections: vec![AdrSection {
                    adr_id: "adr-001".to_string(),
                    content: "# Rules".to_string(),
                }],
                reasoning: "Root level rules".to_string(),
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
            None,
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
    async fn test_absolute_path_validation_error() {
        let adrs = vec![make_adr("adr-001")];
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "/etc/CLAUDE.md".to_string(),
                sections: vec![AdrSection {
                    adr_id: "adr-001".to_string(),
                    content: "# Rules".to_string(),
                }],
                reasoning: "Root level rules".to_string(),
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
            None,
        )
        .await;

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("absolute paths are not allowed"),
            "expected 'absolute paths are not allowed' in: {msg}"
        );
        assert!(matches!(err, ActualError::TailoringValidationError(_)));
    }

    #[tokio::test]
    async fn test_non_parse_error_not_retried() {
        let adrs = vec![make_adr("adr-001")];
        let runner =
            MockTailoringRunner::new(vec![MockResponse::Error(ActualError::RunnerFailed {
                message: "process crashed".to_string(),
                stderr: "segfault".to_string(),
            })]);

        let result = invoke_tailoring(
            &runner,
            &adrs,
            "{}",
            "",
            None,
            None,
            &OutputFormat::ClaudeMd,
            None,
        )
        .await;

        let err = result.unwrap_err();
        assert!(matches!(err, ActualError::RunnerFailed { .. }));
        assert_eq!(runner.call_count.load(Ordering::SeqCst), 1);
    }

    // ── format-aware validate_and_filter_output tests ──

    #[tokio::test]
    async fn test_agents_md_format_accepts_agents_md_path() {
        let adrs = vec![make_adr("adr-001")];
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "AGENTS.md".to_string(),
                sections: vec![AdrSection {
                    adr_id: "adr-001".to_string(),
                    content: "# Rules".to_string(),
                }],
                reasoning: "Root level rules".to_string(),
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
            None,
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
                sections: vec![AdrSection {
                    adr_id: "adr-001".to_string(),
                    content: "# Rules".to_string(),
                }],
                reasoning: "Subdir rules".to_string(),
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
            None,
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
                sections: vec![AdrSection {
                    adr_id: "adr-001".to_string(),
                    content: "# Rules".to_string(),
                }],
                reasoning: "Root level rules".to_string(),
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
            None,
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
                sections: vec![AdrSection {
                    adr_id: "adr-001".to_string(),
                    content: "# Rules\n\nUse Tailwind.".to_string(),
                }],
                reasoning: "Cursor rules".to_string(),
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
            None,
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
                sections: vec![AdrSection {
                    adr_id: "adr-001".to_string(),
                    content: "# Rules\n\nUse React.".to_string(),
                }],
                reasoning: "Web cursor rules".to_string(),
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
            None,
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
                sections: vec![AdrSection {
                    adr_id: "adr-001".to_string(),
                    content: "# Rules".to_string(),
                }],
                reasoning: "Wrong format".to_string(),
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
            None,
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
                sections: vec![AdrSection {
                    adr_id: "adr-001".to_string(),
                    content: "# Rules".to_string(),
                }],
                reasoning: "Wrong format".to_string(),
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
            None,
        )
        .await;

        let err = result.unwrap_err();
        assert!(matches!(err, ActualError::TailoringValidationError(_)));
    }

    // ── bundled context tests ──

    #[test]
    fn test_format_bundle_context_with_key_files() {
        let ctx = RepoBundleContext {
            file_tree: "src/\n  main.rs\n".to_string(),
            key_files: vec![
                ("src/main.rs".to_string(), "fn main() {}".to_string()),
                ("Cargo.toml".to_string(), "[package]".to_string()),
            ],
        };
        let formatted = format_bundle_context(&ctx);
        assert!(formatted.contains("=== file_tree ===\n"));
        assert!(formatted.contains("src/\n  main.rs\n"));
        assert!(formatted.contains("=== src/main.rs ===\n"));
        assert!(formatted.contains("fn main() {}"));
        assert!(formatted.contains("=== Cargo.toml ===\n"));
        assert!(formatted.contains("[package]"));
    }

    #[tokio::test]
    async fn test_bundled_context_ok_path_uses_real_dir() {
        // Use a real temp dir so bundle_context succeeds and exercises the Ok branch.
        let dir = tempfile::tempdir().unwrap();
        // Write a minimal Cargo.toml so the bundler finds something.
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();

        let adrs = vec![make_adr("adr-001")];
        let json = valid_output_json(&["adr-001"]);
        let runner = MockTailoringRunner::single(&json);

        let result = invoke_tailoring(
            &runner,
            &adrs,
            "{}",
            "",
            None,
            None,
            &OutputFormat::ClaudeMd,
            Some(dir.path()),
        )
        .await;

        assert!(
            result.is_ok(),
            "invoke_tailoring with real dir must succeed: {result:?}"
        );
    }

    #[test]
    fn test_bundled_context_included_in_prompt() {
        let adr_json = r#"[{"id":"adr-001"}]"#;
        let bundled_context = "=== file_tree ===\nsrc/\n  main.rs\n";
        let prompt = build_prompt(adr_json, "{}", "", &OutputFormat::ClaudeMd, bundled_context)
            .expect("build_prompt should succeed with valid input");
        assert!(
            prompt.contains(bundled_context),
            "prompt must contain the bundled context string"
        );
    }

    #[tokio::test]
    async fn test_bundled_context_error_handled_gracefully() {
        // Pass a nonexistent path as root_dir — bundle_context will fail, but
        // invoke_tailoring must still succeed (graceful degradation).
        let adrs = vec![make_adr("adr-001")];
        let json = valid_output_json(&["adr-001"]);
        let runner = MockTailoringRunner::single(&json);

        let nonexistent = std::path::Path::new("/nonexistent/path/that/does/not/exist");
        let result = invoke_tailoring(
            &runner,
            &adrs,
            "{}",
            "",
            None,
            None,
            &OutputFormat::ClaudeMd,
            Some(nonexistent),
        )
        .await;

        // Should succeed despite bundling failing — graceful degradation
        assert!(
            result.is_ok(),
            "invoke_tailoring must not fail when context bundling fails: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_empty_reasoning_skips_debug_log() {
        // When the LLM returns an empty reasoning field, the debug log should be
        // skipped (if !file.reasoning.is_empty() guard). Verify the call succeeds.
        let adrs = vec![make_adr("adr-001")];
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                sections: vec![AdrSection {
                    adr_id: "adr-001".to_string(),
                    content: "# Rules\n\nFollow standards.".to_string(),
                }],
                reasoning: String::new(), // empty reasoning — guard skips debug log
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
            None,
        )
        .await;

        let output = result.unwrap();
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].reasoning, "");
    }
}
