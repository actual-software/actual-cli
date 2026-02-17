//! Baseline benchmark for Stage 2 repository analysis (LLM-based).
//!
//! Runs `run_analysis` with the real `CliClaudeRunner` against a set of local
//! repositories and records wall-clock time + full output for each.
//!
//! Results are written to `benches/analysis_baseline_results.json` for later
//! comparison with the static analyzer replacement.
//!
//! # Usage
//!
//! ```sh
//! # Benchmark the current repo:
//! cargo run --release --bin analysis_baseline
//!
//! # Or with a specific model:
//! MODEL=haiku cargo run --release --bin analysis_baseline
//!
//! # Or target specific repos:
//! BENCH_REPOS=/path/to/repo1,/path/to/repo2 cargo run --release --bin analysis_baseline
//! ```
//!
//! # Environment Variables
//!
//! - `MODEL`: Claude model to use (default: "haiku")
//! - `BENCH_REPOS`: Comma-separated list of absolute repo paths to benchmark.
//!   If not set, benchmarks the current working directory only.
//! - `BENCH_RUNS`: Number of runs per repo (default: 3)
//! - `CLAUDE_BINARY`: Override Claude binary path

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use actual_cli::analysis::orchestrate::run_analysis;
use actual_cli::analysis::types::RepoAnalysis;
use actual_cli::claude::subprocess::CliClaudeRunner;
use actual_cli::error::ActualError;

use serde::Serialize;

/// A single benchmark run result.
#[derive(Serialize)]
struct RunResult {
    /// Short name derived from the directory (e.g. "actual-cli").
    repo_name: String,
    run_index: usize,
    duration_ms: u128,
    success: bool,
    error: Option<String>,
    analysis: Option<AnalysisSnapshot>,
}

/// Snapshot of the analysis output for comparison.
#[derive(Serialize)]
struct AnalysisSnapshot {
    is_monorepo: bool,
    project_count: usize,
    projects: Vec<ProjectSnapshot>,
}

#[derive(Serialize)]
struct ProjectSnapshot {
    path: String,
    name: String,
    languages: Vec<String>,
    frameworks: Vec<FrameworkSnapshot>,
    package_manager: Option<String>,
    description: Option<String>,
}

#[derive(Serialize)]
struct FrameworkSnapshot {
    name: String,
    category: String,
}

/// Full benchmark report.
#[derive(Serialize)]
struct BenchmarkReport {
    timestamp: String,
    model: String,
    runs_per_repo: usize,
    results: Vec<RunResult>,
    summary: Vec<RepoSummary>,
}

#[derive(Serialize)]
struct RepoSummary {
    repo_name: String,
    total_runs: usize,
    successful_runs: usize,
    min_ms: u128,
    max_ms: u128,
    median_ms: u128,
    mean_ms: u128,
    /// Whether all successful runs produced identical analysis output.
    deterministic: bool,
}

fn snapshot_analysis(analysis: &RepoAnalysis) -> AnalysisSnapshot {
    AnalysisSnapshot {
        is_monorepo: analysis.is_monorepo,
        project_count: analysis.projects.len(),
        projects: analysis
            .projects
            .iter()
            .map(|p| ProjectSnapshot {
                path: p.path.clone(),
                name: p.name.clone(),
                languages: p.languages.iter().map(|l| format!("{l:?}")).collect(),
                frameworks: p
                    .frameworks
                    .iter()
                    .map(|f| FrameworkSnapshot {
                        name: f.name.clone(),
                        category: format!("{:?}", f.category),
                    })
                    .collect(),
                package_manager: p.package_manager.clone(),
                description: p.description.clone(),
            })
            .collect(),
    }
}

fn resolve_claude_binary() -> PathBuf {
    if let Ok(path) = std::env::var("CLAUDE_BINARY") {
        return PathBuf::from(path);
    }
    which::which("claude").expect("Claude binary not found. Install or set CLAUDE_BINARY env var.")
}

/// Resolve the repos to benchmark.
///
/// If `BENCH_REPOS` is set, split it by comma. Otherwise default to the
/// current working directory — the most portable default since you can
/// `cd` into any repo before running the benchmark.
fn resolve_repos() -> Vec<PathBuf> {
    if let Ok(paths) = std::env::var("BENCH_REPOS") {
        paths
            .split(',')
            .map(|s| PathBuf::from(s.trim()))
            .filter(|p| p.exists())
            .collect()
    } else {
        vec![std::env::current_dir().expect("Failed to get current directory")]
    }
}

fn repo_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

/// Run a single benchmark iteration by spawning `run_analysis` in a child
/// process whose CWD is set to the target repo.
///
/// We use `set_current_dir` because `run_subprocess` (inside `CliClaudeRunner`)
/// inherits the parent's CWD. This is safe because the tokio runtime is
/// single-threaded (`current_thread` flavor — see `main`).
async fn run_single_benchmark(
    runner: &CliClaudeRunner,
    model: &str,
    repo_path: &Path,
    run_index: usize,
    original_cwd: &Path,
) -> RunResult {
    let name = repo_name(repo_path);
    eprintln!("  Run {}: {} ...", run_index + 1, repo_path.display());

    // Claude Code subprocess inherits CWD from the parent process.
    // We must chdir to the target repo so Claude analyzes the right codebase.
    // This is safe because we use a single-threaded tokio runtime.
    std::env::set_current_dir(repo_path).unwrap_or_else(|e| {
        panic!("Failed to chdir to {}: {}", repo_path.display(), e);
    });

    let start = Instant::now();
    let result: Result<RepoAnalysis, ActualError> =
        run_analysis(runner, Some(model), repo_path).await;
    let elapsed = start.elapsed();

    // Restore original CWD
    let _ = std::env::set_current_dir(original_cwd);

    match result {
        Ok(analysis) => {
            eprintln!(
                "    OK in {:.1}s — {} project(s), {} language(s)",
                elapsed.as_secs_f64(),
                analysis.projects.len(),
                analysis.projects.iter().flat_map(|p| &p.languages).count(),
            );
            RunResult {
                repo_name: name,
                run_index,
                duration_ms: elapsed.as_millis(),
                success: true,
                error: None,
                analysis: Some(snapshot_analysis(&analysis)),
            }
        }
        Err(e) => {
            eprintln!("    FAILED in {:.1}s — {e}", elapsed.as_secs_f64());
            RunResult {
                repo_name: name,
                run_index,
                duration_ms: elapsed.as_millis(),
                success: false,
                error: Some(e.to_string()),
                analysis: None,
            }
        }
    }
}

fn compute_summary(repo_name: &str, results: &[&RunResult]) -> RepoSummary {
    let successful: Vec<u128> = results
        .iter()
        .filter(|r| r.success)
        .map(|r| r.duration_ms)
        .collect();

    let mut sorted = successful.clone();
    sorted.sort();

    let min_ms = sorted.first().copied().unwrap_or(0);
    let max_ms = sorted.last().copied().unwrap_or(0);
    let median_ms = if sorted.is_empty() {
        0
    } else {
        sorted[sorted.len() / 2]
    };
    let mean_ms = if sorted.is_empty() {
        0
    } else {
        sorted.iter().sum::<u128>() / sorted.len() as u128
    };

    // Check determinism: compare analysis snapshots as JSON
    let snapshots: Vec<String> = results
        .iter()
        .filter(|r| r.success)
        .filter_map(|r| r.analysis.as_ref())
        .map(|a| serde_json::to_string(a).unwrap_or_default())
        .collect();
    let deterministic = if snapshots.len() > 1 {
        snapshots.windows(2).all(|w| w[0] == w[1])
    } else {
        true
    };

    RepoSummary {
        repo_name: repo_name.to_string(),
        total_runs: results.len(),
        successful_runs: successful.len(),
        min_ms,
        max_ms,
        median_ms,
        mean_ms,
        deterministic,
    }
}

fn print_summary_table(report: &BenchmarkReport) {
    eprintln!();
    eprintln!("╔══════════════════════════════════════════════════════════════════════╗");
    eprintln!("║                   ANALYSIS BENCHMARK RESULTS                        ║");
    eprintln!("╠══════════════════════════════════════════════════════════════════════╣");
    eprintln!(
        "║ Model: {:<15}  Runs per repo: {:<4}  Time: {} ║",
        report.model,
        report.runs_per_repo,
        &report.timestamp[..19],
    );
    eprintln!("╠══════════════════════════════════════════════════════════════════════╣");
    eprintln!(
        "║ {:<20} {:>6} {:>6} {:>6} {:>6} {:>5} {:>5}  ║",
        "Repo", "Min", "Med", "Mean", "Max", "OK", "Det"
    );
    eprintln!("╠══════════════════════════════════════════════════════════════════════╣");
    for s in &report.summary {
        eprintln!(
            "║ {:<20} {:>5}ms {:>5}ms {:>5}ms {:>5}ms {:>2}/{:<2} {:<5}  ║",
            truncate(&s.repo_name, 20),
            s.min_ms,
            s.median_ms,
            s.mean_ms,
            s.max_ms,
            s.successful_runs,
            s.total_runs,
            if s.deterministic { "yes" } else { "NO" },
        );
    }
    eprintln!("╚══════════════════════════════════════════════════════════════════════╝");

    // Print per-repo analysis details for the first successful run
    for s in &report.summary {
        if let Some(result) = report
            .results
            .iter()
            .find(|r| r.repo_name == s.repo_name && r.success)
        {
            if let Some(analysis) = &result.analysis {
                eprintln!();
                eprintln!(
                    "  {} — monorepo={}, {} project(s):",
                    s.repo_name, analysis.is_monorepo, analysis.project_count
                );
                for p in &analysis.projects {
                    let langs: Vec<&str> = p.languages.iter().map(|s| s.as_str()).collect();
                    let fws: Vec<&str> = p.frameworks.iter().map(|f| f.name.as_str()).collect();
                    eprintln!(
                        "    [{:<15}] {:<20} langs=[{}] fws=[{}] pm={:?}",
                        p.path,
                        p.name,
                        langs.join(", "),
                        fws.join(", "),
                        p.package_manager.as_deref().unwrap_or("none"),
                    );
                }
            }
        }
    }
}

/// Truncate a string to `max` characters, appending "…" if truncated.
/// Uses char boundaries to avoid panicking on multi-byte UTF-8 strings.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let end = s
            .char_indices()
            .nth(max - 1)
            .map(|(i, _)| i)
            .unwrap_or(s.len());
        format!("{}…", &s[..end])
    }
}

/// Use `current_thread` flavor to make `set_current_dir` safe — there is only
/// one thread, so no other tokio worker can observe a stale CWD.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let model = std::env::var("MODEL").unwrap_or_else(|_| "haiku".to_string());
    let runs_per_repo: usize = std::env::var("BENCH_RUNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);

    let repos = resolve_repos();

    if repos.is_empty() {
        eprintln!(
            "No repos found to benchmark. Set BENCH_REPOS to a comma-separated list of repo paths."
        );
        std::process::exit(1);
    }

    let binary = resolve_claude_binary();
    let runner = CliClaudeRunner::new(binary.clone(), Duration::from_secs(120));
    let original_cwd = std::env::current_dir().expect("Failed to get current directory");

    eprintln!("Analysis Baseline Benchmark");
    eprintln!("===========================");
    eprintln!("Model: {model}");
    eprintln!("Runs per repo: {runs_per_repo}");
    eprintln!("Claude binary: {}", binary.display());
    eprintln!("Repos: {}", repos.len());
    for r in &repos {
        eprintln!("  - {}", r.display());
    }
    eprintln!();

    let mut all_results = Vec::new();

    for repo_path in &repos {
        eprintln!("Benchmarking: {}", repo_path.display());
        for run_idx in 0..runs_per_repo {
            let result =
                run_single_benchmark(&runner, &model, repo_path, run_idx, &original_cwd).await;
            all_results.push(result);
        }
        eprintln!();
    }

    // Compute summaries keyed by repo_name (portable, no absolute paths)
    let summaries: Vec<RepoSummary> = repos
        .iter()
        .map(|repo_path| {
            let name = repo_name(repo_path);
            let repo_results: Vec<&RunResult> =
                all_results.iter().filter(|r| r.repo_name == name).collect();
            compute_summary(&name, &repo_results)
        })
        .collect();

    let report = BenchmarkReport {
        timestamp: chrono::Utc::now().to_rfc3339(),
        model: model.clone(),
        runs_per_repo,
        results: all_results,
        summary: summaries,
    };

    // Restore CWD before writing results
    let _ = std::env::set_current_dir(&original_cwd);

    // Print summary table
    print_summary_table(&report);

    // Write to file (use absolute path since CWD may have changed)
    let output_path = original_cwd.join("benches/analysis_baseline_results.json");
    let json = serde_json::to_string_pretty(&report).expect("Failed to serialize report");
    std::fs::write(&output_path, &json).expect("Failed to write results file");
    eprintln!();
    eprintln!("Results written to: {}", output_path.display());
}
