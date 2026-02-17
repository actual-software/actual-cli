use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::analysis::types::Project;
use crate::api::types::Adr;
use crate::claude::ClaudeRunner;
use crate::error::ActualError;
use crate::generation::merge::merge_outputs;
use crate::tailoring::batch::{batch_context_summary, create_batches};
use crate::tailoring::invoke::invoke_tailoring;
use crate::tailoring::types::TailoringOutput;

/// Configuration for concurrent project tailoring.
pub struct ConcurrentTailoringConfig<'a> {
    /// Maximum number of projects to process concurrently.
    pub concurrency: usize,
    /// Maximum number of ADRs per batch sent to a single invocation.
    pub batch_size: usize,
    /// Pre-existing CLAUDE.md file paths to include as context.
    pub existing_claude_md_paths: &'a str,
    /// Optional model override for the Claude invocation.
    pub model_override: Option<&'a str>,
    /// Optional maximum budget in USD for each invocation.
    pub max_budget_usd: Option<f64>,
}

/// Tailor ADRs for multiple projects concurrently, then merge results.
///
/// Uses a semaphore to limit the number of concurrent tailoring invocations.
/// Each project is processed independently: its ADRs are batched, each batch
/// is run through `invoke_tailoring`, and the per-project batch results are
/// merged. Finally, all per-project outputs are merged into a single result.
pub async fn tailor_all_projects<R: ClaudeRunner>(
    runner: &R,
    projects: &[Project],
    adrs: &[Adr],
    config: &ConcurrentTailoringConfig<'_>,
) -> Result<TailoringOutput, ActualError> {
    let semaphore = Arc::new(Semaphore::new(config.concurrency));

    // Create one future per project. The semaphore limits how many run concurrently.
    let futures: Vec<_> = projects
        .iter()
        .map(|project| {
            let sem = semaphore.clone();
            async move {
                let _permit = sem.acquire().await.expect("semaphore should not be closed");
                tailor_single_project(runner, project, adrs, config).await
            }
        })
        .collect();

    let outputs = futures::future::try_join_all(futures).await?;

    Ok(merge_outputs(outputs))
}

/// Process a single project: batch its ADRs, invoke tailoring for each batch,
/// and merge all batch outputs into a single per-project result.
async fn tailor_single_project<R: ClaudeRunner>(
    runner: &R,
    project: &Project,
    adrs: &[Adr],
    config: &ConcurrentTailoringConfig<'_>,
) -> Result<TailoringOutput, ActualError> {
    let project_json =
        serde_json::to_string(project).expect("Project serialization should not fail");
    let batches = create_batches(adrs, config.batch_size);

    let mut previous_outputs: Vec<TailoringOutput> = Vec::new();

    for batch_adrs in &batches {
        let context = batch_context_summary(&previous_outputs);
        let claude_md_context = if context.is_empty() {
            config.existing_claude_md_paths.to_string()
        } else {
            format!("{}\n\n{context}", config.existing_claude_md_paths)
        };

        let output = invoke_tailoring(
            runner,
            batch_adrs,
            &project_json,
            &claude_md_context,
            config.model_override,
            config.max_budget_usd,
        )
        .await;
        let output = output?;

        previous_outputs.push(output);
    }

    Ok(merge_outputs(previous_outputs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::types::{Framework, FrameworkCategory, Language};
    use crate::api::types::{AdrCategory, AppliesTo};
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

    impl ClaudeRunner for ConcurrentMockRunner {
        async fn run<T: serde::de::DeserializeOwned + Send>(
            &self,
            _args: &[String],
        ) -> Result<T, ActualError> {
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
        };
        let result = tailor_all_projects(&runner, &projects, &adrs, &config).await;

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
        };
        let result = tailor_all_projects(&runner, &projects, &adrs, &config).await;

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
        };
        let result = tailor_all_projects(&runner, &projects, &adrs, &config).await;

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
        };
        let result = tailor_all_projects(&runner, &projects, &adrs, &config).await;

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
        };
        let result = tailor_all_projects(&runner, &projects, &adrs, &config).await;

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

        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_inner = call_count.clone();

        // A custom runner that increments a shared counter and fails on the first call.
        struct EarlyCancelRunner {
            call_count: Arc<AtomicU32>,
        }

        impl ClaudeRunner for EarlyCancelRunner {
            async fn run<T: serde::de::DeserializeOwned + Send>(
                &self,
                _args: &[String],
            ) -> Result<T, ActualError> {
                let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
                if idx == 0 {
                    // First call: wait briefly so the future is clearly in-flight,
                    // then return an error.
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    return Err(ActualError::ClaudeSubprocessFailed {
                        message: "boom".to_string(),
                        stderr: String::new(),
                    });
                }
                // Subsequent calls: succeed (but should never be reached).
                let json = make_output_json("dummy.md", "should not appear", &["adr-001"]);
                serde_json::from_str(&json).map_err(Into::into)
            }
        }

        let runner = EarlyCancelRunner {
            call_count: call_count_inner,
        };

        let config = ConcurrentTailoringConfig {
            concurrency: 1,
            batch_size: 15,
            existing_claude_md_paths: "",
            model_override: None,
            max_budget_usd: None,
        };

        let result = tailor_all_projects(&runner, &projects, &adrs, &config).await;
        assert!(result.is_err(), "expected an error from the first project");

        let calls = call_count.load(Ordering::SeqCst);
        assert!(
            calls < 3,
            "expected fewer than 3 calls due to early cancellation, got {calls}"
        );
    }

    // --- Test 7: multi-batch per project passes context to later batches ---

    #[tokio::test]
    async fn test_multi_batch_passes_context() {
        let projects = vec![make_project("a")];
        // 3 ADRs in different categories with batch_size=1 → 3 batches
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

        let runner = ConcurrentMockRunner::new(responses, Duration::from_millis(10));

        let config = ConcurrentTailoringConfig {
            concurrency: 1,
            batch_size: 1,
            existing_claude_md_paths: "existing context",
            model_override: None,
            max_budget_usd: None,
        };
        let result = tailor_all_projects(&runner, &projects, &adrs, &config).await;

        let output = result.unwrap();

        // Verify both batch outputs merged
        assert_eq!(output.files.len(), 2);
        let paths: Vec<&str> = output.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"CLAUDE.md"));
        assert!(paths.contains(&"apps/a/CLAUDE.md"));

        // Both batches should have been called
        assert_eq!(runner.call_count.load(Ordering::SeqCst), 2);
    }
}
