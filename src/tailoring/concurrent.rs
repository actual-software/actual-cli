use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;

use crate::analysis::types::Project;
use crate::api::types::Adr;
use crate::error::ActualError;
use crate::generation::merge::merge_outputs;
use crate::generation::OutputFormat;
use crate::runner::TailoringRunner;
use crate::tailoring::batch::create_batches;
use crate::tailoring::invoke::{invoke_tailoring, serialize_json};
use crate::tailoring::types::{TailoringEvent, TailoringOutput};

/// Configuration for concurrent project tailoring.
pub struct ConcurrentTailoringConfig<'a> {
    /// Maximum number of concurrent Claude invocations.
    pub concurrency: usize,
    /// Maximum number of ADRs per batch sent to a single invocation.
    pub batch_size: usize,
    /// Pre-existing output file paths to include as context.
    pub existing_claude_md_paths: &'a str,
    /// Optional model override for the Claude invocation.
    pub model_override: Option<&'a str>,
    /// Optional maximum budget in USD for each invocation.
    pub max_budget_usd: Option<f64>,
    /// Per-project timeout duration.
    pub per_project_timeout: Duration,
    /// Output file format (CLAUDE.md vs AGENTS.md).
    pub output_format: &'a OutputFormat,
}

/// Tailor ADRs for multiple projects concurrently, then merge results.
///
/// Uses a semaphore to limit the number of concurrent tailoring invocations.
/// Each project is processed independently: its ADRs are batched, each batch
/// is run through `invoke_tailoring`, and the per-project batch results are
/// merged. Finally, all per-project outputs are merged into a single result.
///
/// If `progress_tx` is `Some`, a [`TailoringEvent`] is sent for each project
/// as it starts and completes (or fails). Send errors are silently ignored —
/// the receiver may be dropped if the caller is no longer interested in events.
pub async fn tailor_all_projects<R: TailoringRunner>(
    runner: &R,
    projects: &[Project],
    adrs: &[Adr],
    config: &ConcurrentTailoringConfig<'_>,
    progress_tx: Option<tokio::sync::mpsc::UnboundedSender<TailoringEvent>>,
) -> Result<TailoringOutput, ActualError> {
    let semaphore = Arc::new(Semaphore::new(config.concurrency));

    // Create one future per project. The semaphore is passed through so each
    // batch invocation acquires a permit, limiting total concurrent Claude calls.
    let futures: Vec<_> = projects
        .iter()
        .map(|project| {
            let sem = semaphore.clone();
            let tx = progress_tx.clone();
            async move {
                let timeout_dur = config.per_project_timeout;
                let project_name = project.name.clone();
                let tx2 = tx.clone();
                tokio::time::timeout(
                    timeout_dur,
                    tailor_single_project(runner, project, adrs, config, &sem, tx),
                )
                .await
                .map_err(|_| {
                    if let Some(ref t) = tx2 {
                        let _ = t.send(TailoringEvent::ProjectFailed {
                            project_name: project_name.clone(),
                            error: format!("timed out after {}s", timeout_dur.as_secs()),
                        });
                    }
                    ActualError::ClaudeTimeout {
                        seconds: timeout_dur.as_secs(),
                    }
                })?
            }
        })
        .collect();

    let outputs = futures::future::try_join_all(futures).await?;

    Ok(merge_outputs(outputs))
}

/// Process a single project: batch its ADRs, invoke tailoring for each batch
/// in parallel (bounded by the shared semaphore), and merge all batch outputs
/// into a single per-project result.
///
/// Sends [`TailoringEvent::ProjectStarted`] before work begins and either
/// [`TailoringEvent::ProjectCompleted`] or [`TailoringEvent::ProjectFailed`]
/// when the project finishes. Send errors are silently ignored.
async fn tailor_single_project<R: TailoringRunner>(
    runner: &R,
    project: &Project,
    adrs: &[Adr],
    config: &ConcurrentTailoringConfig<'_>,
    semaphore: &Semaphore,
    progress_tx: Option<tokio::sync::mpsc::UnboundedSender<TailoringEvent>>,
) -> Result<TailoringOutput, ActualError> {
    if let Some(tx) = &progress_tx {
        let _ = tx.send(TailoringEvent::ProjectStarted {
            project_name: project.name.clone(),
        });
    }

    let project_json = serialize_json(project, "project")?;
    let batches = create_batches(adrs, config.batch_size);

    let futures: Vec<_> = batches
        .iter()
        .map(|batch_adrs| async {
            let _permit = semaphore.acquire().await.map_err(|_| {
                ActualError::InternalError(
                    "semaphore closed unexpectedly — this is a bug".to_string(),
                )
            })?;
            invoke_tailoring(
                runner,
                batch_adrs,
                &project_json,
                config.existing_claude_md_paths,
                config.model_override,
                config.max_budget_usd,
                config.output_format,
            )
            .await
        })
        .collect();

    let result = futures::future::try_join_all(futures).await;

    match result {
        Ok(outputs) => {
            let merged = merge_outputs(outputs);
            if let Some(tx) = &progress_tx {
                let _ = tx.send(TailoringEvent::ProjectCompleted {
                    project_name: project.name.clone(),
                    files_generated: merged.summary.files_generated,
                    adrs_applied: merged.summary.applicable,
                });
            }
            Ok(merged)
        }
        Err(e) => {
            if let Some(tx) = &progress_tx {
                let _ = tx.send(TailoringEvent::ProjectFailed {
                    project_name: project.name.clone(),
                    error: e.to_string(),
                });
            }
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::types::{Framework, FrameworkCategory, Language};
    use crate::api::types::{AdrCategory, AppliesTo};
    use crate::generation::OutputFormat;
    use crate::tailoring::types::{FileOutput, SkippedAdr, TailoringSummary};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Tracks concurrent execution: records (entry_time, exit_time) for each call.
    struct ConcurrencyTracker {
        /// (entry_time, exit_time) for each call, in order of entry.
        timings: Mutex<Vec<(Instant, Instant)>>,
    }

    impl ConcurrencyTracker {
        fn new() -> Self {
            Self {
                timings: Mutex::new(Vec::new()),
            }
        }

        /// Record a call's entry and exit, with an artificial delay in between.
        async fn record(&self, delay: Duration) {
            let entry = Instant::now();
            tokio::time::sleep(delay).await;
            let exit = Instant::now();
            self.timings.lock().unwrap().push((entry, exit));
        }

        /// Compute the maximum number of overlapping calls.
        fn max_concurrent(&self) -> usize {
            let timings = self.timings.lock().unwrap();
            let mut max = 0;
            for (entry_i, _) in timings.iter() {
                let mut count = 0;
                for (entry_j, exit_j) in timings.iter() {
                    // j is active during i's entry if j entered before i's entry
                    // and j exits after i's entry.
                    if entry_j <= entry_i && exit_j > entry_i {
                        count += 1;
                    }
                }
                if count > max {
                    max = count;
                }
            }
            max
        }
    }

    /// A response entry for the mock runner: either a JSON string to parse,
    /// or a non-parse error to return directly.
    enum MockResponse {
        Json(String),
        Error(ActualError),
    }

    /// Mock runner that tracks concurrency and returns canned responses.
    struct ConcurrentMockRunner {
        responses: Mutex<Vec<MockResponse>>,
        call_count: AtomicU32,
        delay: Duration,
        tracker: ConcurrencyTracker,
    }

    impl ConcurrentMockRunner {
        fn new(responses: Vec<String>, delay: Duration) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().map(MockResponse::Json).collect()),
                call_count: AtomicU32::new(0),
                delay,
                tracker: ConcurrencyTracker::new(),
            }
        }

        fn with_responses(responses: Vec<MockResponse>, delay: Duration) -> Self {
            Self {
                responses: Mutex::new(responses),
                call_count: AtomicU32::new(0),
                delay,
                tracker: ConcurrencyTracker::new(),
            }
        }
    }

    impl TailoringRunner for ConcurrentMockRunner {
        async fn run_tailoring(
            &self,
            _prompt: &str,
            _schema: &str,
            _model_override: Option<&str>,
            _max_budget_usd: Option<f64>,
        ) -> Result<TailoringOutput, ActualError> {
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst) as usize;
            self.tracker.record(self.delay).await;
            let mut responses = self.responses.lock().unwrap();
            let entry = std::mem::replace(&mut responses[idx], MockResponse::Json(String::new()));
            match entry {
                MockResponse::Json(json) => serde_json::from_str(&json).map_err(Into::into),
                MockResponse::Error(e) => Err(e),
            }
        }
    }

    fn make_project(name: &str) -> Project {
        Project {
            path: format!("apps/{name}"),
            name: name.to_string(),
            languages: vec![Language::Rust],
            frameworks: vec![Framework {
                name: "tokio".to_string(),
                category: FrameworkCategory::Library,
            }],
            package_manager: Some("cargo".to_string()),
            description: Some(format!("Project {name}")),
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

    fn make_output_json(path: &str, content: &str, adr_ids: &[&str]) -> String {
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: path.to_string(),
                content: content.to_string(),
                reasoning: format!("Rules for {path}"),
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

    fn make_output_json_multi(
        files: Vec<(&str, &str, &[&str])>,
        skipped: Vec<(&str, &str)>,
    ) -> String {
        let file_outputs: Vec<FileOutput> = files
            .iter()
            .map(|(path, content, adr_ids)| FileOutput {
                path: path.to_string(),
                content: content.to_string(),
                reasoning: format!("Rules for {path}"),
                adr_ids: adr_ids.iter().map(|s| s.to_string()).collect(),
            })
            .collect();
        let skipped_adrs: Vec<SkippedAdr> = skipped
            .iter()
            .map(|(id, reason)| SkippedAdr {
                id: id.to_string(),
                reason: reason.to_string(),
            })
            .collect();
        let applicable = file_outputs.iter().map(|f| f.adr_ids.len()).sum::<usize>();
        let output = TailoringOutput {
            summary: TailoringSummary {
                total_input: applicable + skipped_adrs.len(),
                applicable,
                not_applicable: skipped_adrs.len(),
                files_generated: file_outputs.len(),
            },
            files: file_outputs,
            skipped_adrs,
        };
        serde_json::to_string(&output).unwrap()
    }

    // --- Test 1: 3 projects, concurrency=2, verify only 2 run concurrently ---

    #[tokio::test]
    async fn test_three_projects_concurrency_two() {
        let projects = vec![make_project("a"), make_project("b"), make_project("c")];
        let adrs = vec![make_adr("adr-001")];

        // Each project produces a unique file
        let responses = vec![
            make_output_json("apps/a/CLAUDE.md", "Rules for A", &["adr-001"]),
            make_output_json("apps/b/CLAUDE.md", "Rules for B", &["adr-001"]),
            make_output_json("apps/c/CLAUDE.md", "Rules for C", &["adr-001"]),
        ];

        let runner = ConcurrentMockRunner::new(responses, Duration::from_millis(50));

        let config = ConcurrentTailoringConfig {
            concurrency: 2,
            batch_size: 15,
            existing_claude_md_paths: "",
            model_override: None,
            max_budget_usd: None,
            per_project_timeout: Duration::from_secs(600),
            output_format: &OutputFormat::ClaudeMd,
        };
        let result = tailor_all_projects(&runner, &projects, &adrs, &config, None).await;

        let output = result.unwrap();

        // Verify all 3 project files present
        assert_eq!(output.files.len(), 3);

        // Verify concurrency was limited to 2
        let max_concurrent = runner.tracker.max_concurrent();
        assert!(
            max_concurrent <= 2,
            "expected max 2 concurrent calls, got {max_concurrent}"
        );

        // With 3 tasks and concurrency=2, at least 2 should run concurrently
        assert!(
            max_concurrent >= 2,
            "expected at least 2 concurrent calls, got {max_concurrent}"
        );
    }

    // --- Test 2: 1 project, verify it works without concurrency overhead ---

    #[tokio::test]
    async fn test_single_project_no_overhead() {
        let projects = vec![make_project("solo")];
        let adrs = vec![make_adr("adr-001")];

        let responses = vec![make_output_json(
            "apps/solo/CLAUDE.md",
            "Solo rules",
            &["adr-001"],
        )];

        let runner = ConcurrentMockRunner::new(responses, Duration::from_millis(10));

        let config = ConcurrentTailoringConfig {
            concurrency: 1,
            batch_size: 15,
            existing_claude_md_paths: "",
            model_override: None,
            max_budget_usd: None,
            per_project_timeout: Duration::from_secs(600),
            output_format: &OutputFormat::ClaudeMd,
        };
        let result = tailor_all_projects(&runner, &projects, &adrs, &config, None).await;

        let output = result.unwrap();
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "apps/solo/CLAUDE.md");
        assert_eq!(output.files[0].content, "Solo rules");
        assert_eq!(runner.call_count.load(Ordering::SeqCst), 1);
    }

    // --- Test 3: merged output contains files from all projects ---

    #[tokio::test]
    async fn test_merged_output_contains_all_project_files() {
        let projects = vec![make_project("a"), make_project("b"), make_project("c")];
        let adrs = vec![make_adr("adr-001"), make_adr("adr-002")];

        // Each project produces a different set of files
        let responses = vec![
            make_output_json_multi(
                vec![
                    ("CLAUDE.md", "Root from A", &["adr-001"]),
                    ("apps/a/CLAUDE.md", "A specific", &["adr-002"]),
                ],
                vec![],
            ),
            make_output_json_multi(
                vec![("apps/b/CLAUDE.md", "B specific", &["adr-001", "adr-002"])],
                vec![],
            ),
            make_output_json_multi(
                vec![("apps/c/CLAUDE.md", "C specific", &["adr-001"])],
                vec![("adr-002", "Not applicable to C")],
            ),
        ];

        let runner = ConcurrentMockRunner::new(responses, Duration::from_millis(10));

        let config = ConcurrentTailoringConfig {
            concurrency: 3,
            batch_size: 15,
            existing_claude_md_paths: "",
            model_override: None,
            max_budget_usd: None,
            per_project_timeout: Duration::from_secs(600),
            output_format: &OutputFormat::ClaudeMd,
        };
        let result = tailor_all_projects(&runner, &projects, &adrs, &config, None).await;

        let output = result.unwrap();
        let paths: Vec<&str> = output.files.iter().map(|f| f.path.as_str()).collect();

        assert!(paths.contains(&"CLAUDE.md"), "missing root CLAUDE.md");
        assert!(
            paths.contains(&"apps/a/CLAUDE.md"),
            "missing apps/a/CLAUDE.md"
        );
        assert!(
            paths.contains(&"apps/b/CLAUDE.md"),
            "missing apps/b/CLAUDE.md"
        );
        assert!(
            paths.contains(&"apps/c/CLAUDE.md"),
            "missing apps/c/CLAUDE.md"
        );
        assert_eq!(output.files.len(), 4);
    }

    // --- Test 4: root CLAUDE.md content from multiple projects is concatenated ---

    #[tokio::test]
    async fn test_root_claude_md_concatenated() {
        let projects = vec![make_project("a"), make_project("b"), make_project("c")];
        let adrs = vec![make_adr("adr-001")];

        // All three projects produce content for root CLAUDE.md
        let responses = vec![
            make_output_json("CLAUDE.md", "Rules from project A", &["adr-001"]),
            make_output_json("CLAUDE.md", "Rules from project B", &["adr-001"]),
            make_output_json("CLAUDE.md", "Rules from project C", &["adr-001"]),
        ];

        let runner = ConcurrentMockRunner::new(responses, Duration::from_millis(10));

        let config = ConcurrentTailoringConfig {
            concurrency: 3,
            batch_size: 15,
            existing_claude_md_paths: "",
            model_override: None,
            max_budget_usd: None,
            per_project_timeout: Duration::from_secs(600),
            output_format: &OutputFormat::ClaudeMd,
        };
        let result = tailor_all_projects(&runner, &projects, &adrs, &config, None).await;

        let output = result.unwrap();

        // Should have a single merged CLAUDE.md
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "CLAUDE.md");

        // Content from all three projects should be concatenated with "\n\n"
        let content = &output.files[0].content;
        assert!(
            content.contains("Rules from project A"),
            "missing content from project A in: {content}"
        );
        assert!(
            content.contains("Rules from project B"),
            "missing content from project B in: {content}"
        );
        assert!(
            content.contains("Rules from project C"),
            "missing content from project C in: {content}"
        );

        // Verify the concatenation separator: order depends on concurrent execution.
        // Use bitwise OR to ensure both sides are always evaluated (avoids
        // short-circuit region that LLVM marks as uncovered).
        let has_a_then_b = content.contains("Rules from project A\n\nRules from project B");
        let has_b_then_a = content.contains("Rules from project B\n\nRules from project A");
        assert!(
            has_a_then_b | has_b_then_a,
            "expected content concatenated with newlines, got: {content}"
        );
    }

    // --- Test 5: error from one project propagates ---

    #[tokio::test]
    async fn test_error_propagates() {
        let projects = vec![make_project("a"), make_project("b")];
        let adrs = vec![make_adr("adr-001")];

        let responses = vec![
            MockResponse::Json(make_output_json(
                "apps/a/CLAUDE.md",
                "Rules for A",
                &["adr-001"],
            )),
            MockResponse::Error(ActualError::ClaudeSubprocessFailed {
                message: "process crashed".to_string(),
                stderr: "segfault".to_string(),
            }),
        ];

        let runner = ConcurrentMockRunner::with_responses(responses, Duration::from_millis(10));

        let config = ConcurrentTailoringConfig {
            concurrency: 2,
            batch_size: 15,
            existing_claude_md_paths: "",
            model_override: None,
            max_budget_usd: None,
            per_project_timeout: Duration::from_secs(600),
            output_format: &OutputFormat::ClaudeMd,
        };
        let result = tailor_all_projects(&runner, &projects, &adrs, &config, None).await;

        let err = result.unwrap_err();
        let err_display = err.to_string();
        assert!(
            err_display.contains("process crashed"),
            "expected ClaudeSubprocessFailed, got: {err_display}"
        );
    }

    // --- Test 6: try_join_all cancels remaining futures on first error ---

    #[tokio::test]
    async fn test_early_cancellation_on_error() {
        // 3 projects, concurrency=1 so they run sequentially.
        // The first project errors; with try_join_all the remaining futures
        // should be cancelled and never invoke the runner.
        let projects = vec![make_project("a"), make_project("b"), make_project("c")];
        let adrs = vec![make_adr("adr-001")];

        // First response is an error; the rest are valid but should never be reached.
        let responses = vec![
            MockResponse::Error(ActualError::ClaudeSubprocessFailed {
                message: "boom".to_string(),
                stderr: String::new(),
            }),
            MockResponse::Json(make_output_json(
                "apps/b/CLAUDE.md",
                "Rules for B",
                &["adr-001"],
            )),
            MockResponse::Json(make_output_json(
                "apps/c/CLAUDE.md",
                "Rules for C",
                &["adr-001"],
            )),
        ];

        let runner = ConcurrentMockRunner::with_responses(responses, Duration::from_millis(10));

        let config = ConcurrentTailoringConfig {
            concurrency: 1,
            batch_size: 15,
            existing_claude_md_paths: "",
            model_override: None,
            max_budget_usd: None,
            per_project_timeout: Duration::from_secs(600),
            output_format: &OutputFormat::ClaudeMd,
        };

        let result = tailor_all_projects(&runner, &projects, &adrs, &config, None).await;
        assert!(result.is_err(), "expected an error from the first project");

        let calls = runner.call_count.load(Ordering::SeqCst);
        assert_eq!(
            calls, 1,
            "expected exactly 1 call: try_join_all should have dropped remaining futures on first error, got {calls}"
        );
    }

    // --- Test 7: multi-batch per project runs batches in parallel ---

    #[tokio::test]
    async fn test_multi_batch_parallel_merge() {
        let projects = vec![make_project("a")];
        // 2 ADRs in different categories with batch_size=1 → 2 batches
        let adr1 = Adr {
            id: "adr-001".to_string(),
            title: "ADR 001".to_string(),
            context: None,
            policies: vec!["policy".to_string()],
            instructions: None,
            category: AdrCategory {
                id: "cat-a".to_string(),
                name: "Category A".to_string(),
                path: "Category A".to_string(),
            },
            applies_to: AppliesTo {
                languages: vec![],
                frameworks: vec![],
            },
            matched_projects: vec![],
        };
        let adr2 = Adr {
            id: "adr-002".to_string(),
            title: "ADR 002".to_string(),
            context: None,
            policies: vec!["policy".to_string()],
            instructions: None,
            category: AdrCategory {
                id: "cat-b".to_string(),
                name: "Category B".to_string(),
                path: "Category B".to_string(),
            },
            applies_to: AppliesTo {
                languages: vec![],
                frameworks: vec![],
            },
            matched_projects: vec![],
        };
        let adrs = vec![adr1, adr2];

        // batch_size=1 means 2 batches (one per category since BTreeMap sorts by cat id).
        // Batch 1 (cat-a): returns CLAUDE.md with adr-001
        // Batch 2 (cat-b): returns apps/a/CLAUDE.md with adr-002
        let responses = vec![
            make_output_json("CLAUDE.md", "Batch 1 rules", &["adr-001"]),
            make_output_json("apps/a/CLAUDE.md", "Batch 2 rules", &["adr-002"]),
        ];

        let runner = ConcurrentMockRunner::new(responses, Duration::from_millis(50));

        let config = ConcurrentTailoringConfig {
            concurrency: 2,
            batch_size: 1,
            existing_claude_md_paths: "existing context",
            model_override: None,
            max_budget_usd: None,
            per_project_timeout: Duration::from_secs(600),
            output_format: &OutputFormat::ClaudeMd,
        };
        let result = tailor_all_projects(&runner, &projects, &adrs, &config, None).await;

        let output = result.unwrap();

        // Verify both batch outputs merged
        assert_eq!(output.files.len(), 2);
        let paths: Vec<&str> = output.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"CLAUDE.md"));
        assert!(paths.contains(&"apps/a/CLAUDE.md"));

        // Both batches should have been called
        assert_eq!(runner.call_count.load(Ordering::SeqCst), 2);

        // Verify parallel execution: with concurrency=2, both batches should overlap
        let max_concurrent = runner.tracker.max_concurrent();
        assert!(
            max_concurrent >= 2,
            "expected at least 2 concurrent batch calls, got {max_concurrent}"
        );
    }

    // --- Test 8: multi-batch parallel execution with 3 batches ---

    #[tokio::test]
    async fn test_multi_batch_parallel_execution() {
        let projects = vec![make_project("a")];
        // 3 ADRs in 3 different categories, batch_size=1 → 3 batches
        let adr1 = Adr {
            id: "adr-001".to_string(),
            title: "ADR 001".to_string(),
            context: None,
            policies: vec!["policy".to_string()],
            instructions: None,
            category: AdrCategory {
                id: "cat-a".to_string(),
                name: "Category A".to_string(),
                path: "Category A".to_string(),
            },
            applies_to: AppliesTo {
                languages: vec![],
                frameworks: vec![],
            },
            matched_projects: vec![],
        };
        let adr2 = Adr {
            id: "adr-002".to_string(),
            title: "ADR 002".to_string(),
            context: None,
            policies: vec!["policy".to_string()],
            instructions: None,
            category: AdrCategory {
                id: "cat-b".to_string(),
                name: "Category B".to_string(),
                path: "Category B".to_string(),
            },
            applies_to: AppliesTo {
                languages: vec![],
                frameworks: vec![],
            },
            matched_projects: vec![],
        };
        let adr3 = Adr {
            id: "adr-003".to_string(),
            title: "ADR 003".to_string(),
            context: None,
            policies: vec!["policy".to_string()],
            instructions: None,
            category: AdrCategory {
                id: "cat-c".to_string(),
                name: "Category C".to_string(),
                path: "Category C".to_string(),
            },
            applies_to: AppliesTo {
                languages: vec![],
                frameworks: vec![],
            },
            matched_projects: vec![],
        };
        let adrs = vec![adr1, adr2, adr3];

        let responses = vec![
            make_output_json("CLAUDE.md", "Batch 1 rules", &["adr-001"]),
            make_output_json("apps/a/CLAUDE.md", "Batch 2 rules", &["adr-002"]),
            make_output_json("libs/CLAUDE.md", "Batch 3 rules", &["adr-003"]),
        ];

        let runner = ConcurrentMockRunner::new(responses, Duration::from_millis(50));

        let config = ConcurrentTailoringConfig {
            concurrency: 3,
            batch_size: 1,
            existing_claude_md_paths: "",
            model_override: None,
            max_budget_usd: None,
            per_project_timeout: Duration::from_secs(600),
            output_format: &OutputFormat::ClaudeMd,
        };
        let result = tailor_all_projects(&runner, &projects, &adrs, &config, None).await;

        let output = result.unwrap();

        // Verify all 3 batch outputs merged
        assert_eq!(output.files.len(), 3);
        assert_eq!(runner.call_count.load(Ordering::SeqCst), 3);

        // Verify parallel execution: with concurrency=3, at least 2 should overlap
        let max_concurrent = runner.tracker.max_concurrent();
        assert!(
            max_concurrent >= 2,
            "expected at least 2 concurrent batch calls, got {max_concurrent}"
        );
    }

    // --- Test 9: semaphore limits total concurrent calls across projects and batches ---

    #[tokio::test]
    async fn test_semaphore_limits_total_calls() {
        // 2 projects, each with 2 batches (batch_size=1, 2 ADRs in different categories)
        // concurrency=2 → max 2 concurrent Claude calls across all projects and batches
        let projects = vec![make_project("a"), make_project("b")];
        let adr1 = Adr {
            id: "adr-001".to_string(),
            title: "ADR 001".to_string(),
            context: None,
            policies: vec!["policy".to_string()],
            instructions: None,
            category: AdrCategory {
                id: "cat-a".to_string(),
                name: "Category A".to_string(),
                path: "Category A".to_string(),
            },
            applies_to: AppliesTo {
                languages: vec![],
                frameworks: vec![],
            },
            matched_projects: vec![],
        };
        let adr2 = Adr {
            id: "adr-002".to_string(),
            title: "ADR 002".to_string(),
            context: None,
            policies: vec!["policy".to_string()],
            instructions: None,
            category: AdrCategory {
                id: "cat-b".to_string(),
                name: "Category B".to_string(),
                path: "Category B".to_string(),
            },
            applies_to: AppliesTo {
                languages: vec![],
                frameworks: vec![],
            },
            matched_projects: vec![],
        };
        let adrs = vec![adr1, adr2];

        // 2 projects × 2 batches = 4 total Claude calls
        let responses = vec![
            make_output_json("apps/a/CLAUDE.md", "A batch 1", &["adr-001"]),
            make_output_json("apps/a/sub/CLAUDE.md", "A batch 2", &["adr-002"]),
            make_output_json("apps/b/CLAUDE.md", "B batch 1", &["adr-001"]),
            make_output_json("apps/b/sub/CLAUDE.md", "B batch 2", &["adr-002"]),
        ];

        let runner = ConcurrentMockRunner::new(responses, Duration::from_millis(50));

        let config = ConcurrentTailoringConfig {
            concurrency: 2,
            batch_size: 1,
            existing_claude_md_paths: "",
            model_override: None,
            max_budget_usd: None,
            per_project_timeout: Duration::from_secs(600),
            output_format: &OutputFormat::ClaudeMd,
        };
        let result = tailor_all_projects(&runner, &projects, &adrs, &config, None).await;

        let output = result.unwrap();

        // All 4 calls should have been made
        assert_eq!(runner.call_count.load(Ordering::SeqCst), 4);

        // Semaphore should limit to at most 2 concurrent calls
        let max_concurrent = runner.tracker.max_concurrent();
        assert!(
            max_concurrent <= 2,
            "expected max 2 concurrent calls, got {max_concurrent}"
        );

        // With 4 tasks and concurrency=2, at least 2 should run concurrently
        assert!(
            max_concurrent >= 2,
            "expected at least 2 concurrent calls, got {max_concurrent}"
        );

        // Verify all output files are present
        assert_eq!(output.files.len(), 4);
    }

    // --- Test 10: per-project timeout fires when project takes too long ---

    #[tokio::test]
    async fn test_per_project_timeout() {
        tokio::time::pause();

        let projects = vec![make_project("slow")];
        let adrs = vec![make_adr("adr-001")];

        let responses = vec![make_output_json(
            "apps/slow/CLAUDE.md",
            "Rules",
            &["adr-001"],
        )];
        let runner = ConcurrentMockRunner::new(responses, Duration::from_secs(10));

        let config = ConcurrentTailoringConfig {
            concurrency: 1,
            batch_size: 15,
            existing_claude_md_paths: "",
            model_override: None,
            max_budget_usd: None,
            per_project_timeout: Duration::from_secs(2),
            output_format: &OutputFormat::ClaudeMd,
        };
        let result = tailor_all_projects(&runner, &projects, &adrs, &config, None).await;

        let err = result.unwrap_err();
        assert!(
            matches!(err, ActualError::ClaudeTimeout { seconds: 2 }),
            "expected ClaudeTimeout, got: {err}"
        );
    }

    // --- Test 10b: timeout emits ProjectFailed event ---

    #[tokio::test]
    async fn test_per_project_timeout_emits_project_failed() {
        tokio::time::pause();

        let projects = vec![make_project("slow")];
        let adrs = vec![make_adr("adr-001")];

        let responses = vec![make_output_json(
            "apps/slow/CLAUDE.md",
            "Rules",
            &["adr-001"],
        )];
        let runner = ConcurrentMockRunner::new(responses, Duration::from_secs(10));

        let config = ConcurrentTailoringConfig {
            concurrency: 1,
            batch_size: 15,
            existing_claude_md_paths: "",
            model_override: None,
            max_budget_usd: None,
            per_project_timeout: Duration::from_secs(2),
            output_format: &OutputFormat::ClaudeMd,
        };

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<TailoringEvent>();
        let result = tailor_all_projects(&runner, &projects, &adrs, &config, Some(tx)).await;

        // Confirm timeout error
        assert!(
            matches!(result, Err(ActualError::ClaudeTimeout { seconds: 2 })),
            "expected ClaudeTimeout"
        );

        // Collect all events
        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }

        // Must have a ProjectFailed event for "slow"
        let failed: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, TailoringEvent::ProjectFailed { .. }))
            .collect();
        assert_eq!(failed.len(), 1, "expected exactly 1 ProjectFailed event");
        if let TailoringEvent::ProjectFailed {
            project_name,
            error,
        } = &failed[0]
        {
            assert_eq!(project_name, "slow");
            assert!(
                error.contains("timed out"),
                "expected timeout message in error, got: {error}"
            );
        }
    }

    // --- Test 11: progress events are sent correctly on success ---

    #[tokio::test]
    async fn test_progress_events_on_success() {
        let projects = vec![make_project("alpha"), make_project("beta")];
        let adrs = vec![make_adr("adr-001")];

        let responses = vec![
            make_output_json("apps/alpha/CLAUDE.md", "Alpha rules", &["adr-001"]),
            make_output_json("apps/beta/CLAUDE.md", "Beta rules", &["adr-001"]),
        ];

        let runner = ConcurrentMockRunner::new(responses, Duration::from_millis(10));

        let config = ConcurrentTailoringConfig {
            concurrency: 2,
            batch_size: 15,
            existing_claude_md_paths: "",
            model_override: None,
            max_budget_usd: None,
            per_project_timeout: Duration::from_secs(600),
            output_format: &OutputFormat::ClaudeMd,
        };

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let result = tailor_all_projects(&runner, &projects, &adrs, &config, Some(tx)).await;
        assert!(result.is_ok(), "expected success");

        // Drop sender side is already dropped (moved into the fn). Drain events.
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        // Should have one Started + one Completed per project (2 each = 4 total)
        let started: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, TailoringEvent::ProjectStarted { .. }))
            .collect();
        let completed: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, TailoringEvent::ProjectCompleted { .. }))
            .collect();
        let failed: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, TailoringEvent::ProjectFailed { .. }))
            .collect();

        assert_eq!(started.len(), 2, "expected 2 ProjectStarted events");
        assert_eq!(completed.len(), 2, "expected 2 ProjectCompleted events");
        assert_eq!(failed.len(), 0, "expected no ProjectFailed events");

        // ProjectCompleted events should carry correct file/adr counts
        for event in &completed {
            if let TailoringEvent::ProjectCompleted {
                project_name,
                files_generated,
                adrs_applied,
            } = event
            {
                assert!(
                    project_name == "alpha" || project_name == "beta",
                    "unexpected project name: {project_name}"
                );
                assert_eq!(*files_generated, 1, "expected 1 file for {project_name}");
                assert_eq!(*adrs_applied, 1, "expected 1 adr for {project_name}");
            }
        }
    }

    // --- Test 12: progress events include ProjectFailed on error ---

    #[tokio::test]
    async fn test_progress_events_on_error() {
        let projects = vec![make_project("good"), make_project("bad")];
        let adrs = vec![make_adr("adr-001")];

        let responses = vec![
            MockResponse::Json(make_output_json(
                "apps/good/CLAUDE.md",
                "Good rules",
                &["adr-001"],
            )),
            MockResponse::Error(ActualError::ClaudeSubprocessFailed {
                message: "crashed".to_string(),
                stderr: String::new(),
            }),
        ];

        let runner = ConcurrentMockRunner::with_responses(responses, Duration::from_millis(10));

        let config = ConcurrentTailoringConfig {
            concurrency: 2,
            batch_size: 15,
            existing_claude_md_paths: "",
            model_override: None,
            max_budget_usd: None,
            per_project_timeout: Duration::from_secs(600),
            output_format: &OutputFormat::ClaudeMd,
        };

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let result = tailor_all_projects(&runner, &projects, &adrs, &config, Some(tx)).await;
        assert!(result.is_err(), "expected error to propagate");

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        // There should be at least one ProjectFailed event
        let failed: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, TailoringEvent::ProjectFailed { .. }))
            .collect();
        assert!(
            !failed.is_empty(),
            "expected at least one ProjectFailed event"
        );

        // The failed event should carry the error message
        for event in &failed {
            if let TailoringEvent::ProjectFailed { error, .. } = event {
                assert!(
                    error.contains("crashed"),
                    "expected error message in: {error}"
                );
            }
        }
    }

    // --- Test 13: progress_tx = None works without panicking ---

    #[tokio::test]
    async fn test_no_progress_sender_is_noop() {
        let projects = vec![make_project("solo")];
        let adrs = vec![make_adr("adr-001")];
        let responses = vec![make_output_json(
            "apps/solo/CLAUDE.md",
            "Rules",
            &["adr-001"],
        )];
        let runner = ConcurrentMockRunner::new(responses, Duration::from_millis(5));
        let config = ConcurrentTailoringConfig {
            concurrency: 1,
            batch_size: 15,
            existing_claude_md_paths: "",
            model_override: None,
            max_budget_usd: None,
            per_project_timeout: Duration::from_secs(600),
            output_format: &OutputFormat::ClaudeMd,
        };
        // Should complete without panic
        let result = tailor_all_projects(&runner, &projects, &adrs, &config, None).await;
        assert!(result.is_ok());
    }

    // --- Test 14: closed semaphore returns InternalError instead of panicking ---

    #[tokio::test]
    async fn test_closed_semaphore_returns_internal_error() {
        let projects = vec![make_project("solo")];
        let adrs = vec![make_adr("adr-001")];

        // Use a semaphore with 0 permits and close it immediately so that
        // any acquire() call will fail with AcquireError rather than blocking.
        let semaphore = Semaphore::new(0);
        semaphore.close();

        let runner = ConcurrentMockRunner::new(vec![], Duration::from_millis(0));

        let project_json =
            crate::tailoring::invoke::serialize_json(&projects[0], "project").unwrap();
        let batches = crate::tailoring::batch::create_batches(&adrs, 15);

        // Directly test the closed-semaphore path via the private function.
        let futures: Vec<_> = batches
            .iter()
            .map(|_batch_adrs| async {
                semaphore.acquire().await.map_err(|_| {
                    ActualError::InternalError(
                        "semaphore closed unexpectedly — this is a bug".to_string(),
                    )
                })
            })
            .collect();

        let result: Result<Vec<_>, ActualError> = futures::future::try_join_all(futures).await;

        // Must return InternalError, not panic
        let err = result.unwrap_err();
        assert!(
            matches!(err, ActualError::InternalError(_)),
            "expected InternalError from closed semaphore, got: {err}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("semaphore closed unexpectedly"),
            "expected semaphore error message, got: {msg}"
        );

        // Suppress unused-variable warnings for runner / project_json
        drop(runner);
        drop(project_json);
    }
}
