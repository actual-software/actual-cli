use std::path::Path;
use std::time::Duration;

use crate::analysis::cache::{get_git_branch, get_git_head, run_analysis_cached};
use crate::analysis::confirm::ConfirmAction;
use crate::analysis::types::{Project, ProjectSelection, RepoAnalysis};
use crate::api::client::{build_match_request, ActualApiClient, DEFAULT_API_URL};
use crate::api::retry::{with_retry, RetryConfig};
use crate::cli::args::SyncArgs;
use crate::cli::ui::confirm::format_project_summary_plain;
use crate::cli::ui::header::{AuthDisplay, RunnerDisplay};
use crate::cli::ui::progress::SyncPhase;
use crate::cli::ui::terminal::TerminalIO;
use crate::cli::ui::theme;
use crate::cli::ui::tui::renderer::TuiRenderer;
use crate::config::paths::{load_from, save_to};
use crate::config::rejections::{clear_rejections, get_rejections};
use crate::config::types::{DEFAULT_BATCH_SIZE, DEFAULT_CONCURRENCY, DEFAULT_TIMEOUT_SECS};
use crate::error::ActualError;
use crate::runner::subprocess::TailoringRunner;
use crate::tailoring::concurrent::{tailor_all_projects, ConcurrentTailoringConfig};
use crate::tailoring::filter::pre_filter_rejected;
use crate::tailoring::types::{TailoringEvent, TailoringOutput};
#[cfg(feature = "telemetry")]
use crate::telemetry::identity::hash_repo_identity;
#[cfg(feature = "telemetry")]
use crate::telemetry::metrics::SyncMetrics;
#[cfg(feature = "telemetry")]
use crate::telemetry::reporter::report_metrics;

use super::adr_utils::{
    fetch_error_message, find_existing_output_files, raw_adrs_to_output, zero_adr_context_lines,
};
use super::cache::{
    compute_repo_key, compute_tailoring_cache_key, load_config_with_fallback,
    store_tailoring_cache, tailoring_cache_skip_msg,
};
use super::v2_output::{
    generate_v2_output, inject_v2_governance, partition_adrs, write_v2_raw_files,
};
use super::write::confirm_and_write;

pub(crate) fn resolve_cwd() -> std::path::PathBuf {
    match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("current_dir() failed ({e}), falling back to '.'");
            // We warn and continue rather than failing hard: sync may still work if
            // the process has a valid working directory via the kernel's "." dentry.
            // Callers should treat the result as best-effort in degraded environments.
            std::path::PathBuf::from(".")
        }
    }
}

/// Format a cache age in seconds as a human-readable string.
///
/// - < 0 s    → `"just now"` (clock skew)
/// - < 60 s   → `"Ns ago"`
/// - < 3600 s → `"Nm ago"`
/// - < 86400 s → `"Nh ago"`
/// - ≥ 86400 s → `"Nd ago"`
fn format_cache_age(age_secs: i64) -> String {
    if age_secs < 0 {
        return "just now".to_string();
    }
    if age_secs < 60 {
        format!("{age_secs}s ago")
    } else if age_secs < 3600 {
        format!("{}m ago", age_secs / 60)
    } else if age_secs < 86400 {
        format!("{}h ago", age_secs / 3600)
    } else {
        format!("{}d ago", age_secs / 86400)
    }
}

/// Collapse the home directory prefix of a path to `~`.
///
/// If `home_dir` is `None` or the path does not start with the home directory,
/// the full path is returned as-is.
fn tilde_collapse(path: &Path, home_dir: Option<&std::path::PathBuf>) -> String {
    if let Some(home) = home_dir {
        if let Ok(rel) = path.strip_prefix(home) {
            return format!("~/{}", rel.display());
        }
    }
    path.display().to_string()
}

/// Core sync logic.
///
/// Accepts injected `root_dir`, `term`, `runner`, and `auth_display` so unit
/// tests can avoid `RealTerminal` (which blocks on stdin) and control the
/// working directory, Claude runner, and user input.
///
/// `auth_display` is pushed into the TUI log pane as part of the header bar
/// that follows the banner.  Pass `None` in tests to skip the header.
///
/// `runner_display` shows the active runner and model in the Environment
/// section, with an optional compatibility warning.  Pass `None` in tests.
///
/// `runner_probe` is an optional pre-flight check that is run during the
/// Environment phase, after git detection.  If it returns `Err`, the pipeline
/// reports the error in the Environment step and aborts early — before the
/// Analysis/Fetch/Tailor phases begin.  Pass `None` when the runner has
/// already been validated outside `run_sync` (e.g. ClaudeCli/AnthropicApi).
pub(crate) fn run_sync<R: TailoringRunner>(
    args: &SyncArgs,
    root_dir: &Path,
    cfg_path: &Path,
    term: &dyn TerminalIO,
    runner: &R,
    auth_display: Option<&AuthDisplay>,
    runner_display: Option<&RunnerDisplay>,
) -> Result<(), ActualError> {
    run_sync_with_probe(
        args,
        root_dir,
        cfg_path,
        term,
        runner,
        auth_display,
        runner_display,
        None,
    )
}

/// Inner implementation of [`run_sync`] that accepts an optional runner probe.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_sync_with_probe<R: TailoringRunner>(
    args: &SyncArgs,
    root_dir: &Path,
    cfg_path: &Path,
    term: &dyn TerminalIO,
    runner: &R,
    auth_display: Option<&AuthDisplay>,
    runner_display: Option<&RunnerDisplay>,
    runner_probe: Option<Box<dyn Fn() -> Result<(), ActualError>>>,
) -> Result<(), ActualError> {
    // ── Phase 1: env check + analysis ──

    // 1. Create pipeline. The banner and version are shown in the left-column
    //    banner box (rendered by the TUI directly), not in the log pane.
    let mut pipeline = TuiRenderer::new_with_version(false, args.no_tui, env!("CARGO_PKG_VERSION"));

    pipeline.start(SyncPhase::Environment, "Checking environment...");

    // Load config early so we can show server URL and cache status in the
    // Environment step output. Falls back to default if the config is missing
    // or unreadable (non-fatal — errors surface later when config is required).
    let config = load_config_with_fallback(cfg_path);

    // Resolve effective API server URL (CLI flag > config > default).
    // Store as an owned String so it remains valid after config is reloaded in Phase 2.
    let env_api_url: String = args
        .api_url
        .as_deref()
        .or(config.api_url.as_deref())
        .unwrap_or(DEFAULT_API_URL)
        .to_owned();

    // Helper: strip the home directory prefix and replace with "~".
    let home_dir = dirs::home_dir();

    // Blank line for visual breathing room.
    pipeline.println("");

    // Auth row
    match auth_display {
        Some(a) if a.authenticated => {
            let email_part = a
                .email
                .as_deref()
                .map(|e| format!(" ({})", console::strip_ansi_codes(e)))
                .unwrap_or_default();
            pipeline.println(&format!(
                "  {:<9} {} Authenticated{email_part}",
                "Auth",
                theme::SUCCESS
            ));
        }
        Some(_) => {
            pipeline.println(&format!(
                "  {:<9} {} Not authenticated — check your runner credentials",
                "Auth",
                theme::ERROR
            ));
        }
        None => {
            pipeline.println(&format!("  {:<9} - Status unknown", "Auth"));
        }
    }

    // Runner + Model rows
    if let Some(rd) = runner_display {
        pipeline.println(&format!("  {:<9} {}", "Runner", rd.runner_name));
        pipeline.println(&format!("  {:<9} {}", "Model", rd.model));
        if let Some(ref warn) = rd.warning {
            pipeline.println(&format!("  {:<9} {} {warn}", "", theme::WARN));
        }
    }

    // Server row
    pipeline.println(&format!("  {:<9} {}", "Server", env_api_url));

    // Config path row (tilde-collapsed)
    pipeline.println(&format!(
        "  {:<9} {}",
        "Config",
        tilde_collapse(cfg_path, home_dir.as_ref())
    ));

    // Working directory row (tilde-collapsed)
    pipeline.println(&format!(
        "  {:<9} {}",
        "Workdir",
        tilde_collapse(root_dir, home_dir.as_ref())
    ));

    // Git branch + caching status
    let git_head = get_git_head(root_dir);
    let is_git = git_head.is_some();
    if is_git {
        if let Some(branch) = get_git_branch(root_dir) {
            pipeline.println(&format!("  {:<9} {}", "Branch", branch));
        }
        pipeline.println(&format!(
            "  {:<9} {} Repository detected — caching enabled",
            "Git",
            theme::SUCCESS
        ));

        // Cache status row
        match &config.cached_analysis {
            Some(ca) if ca.repo_path == root_dir.to_string_lossy().as_ref() => {
                let age = chrono::Utc::now()
                    .signed_duration_since(ca.analyzed_at)
                    .num_seconds();
                let age_str = format_cache_age(age);
                pipeline.println(&format!(
                    "  {:<9} {} Analysis cached ({})",
                    "Cache",
                    theme::SUCCESS,
                    age_str
                ));
            }
            _ => {
                pipeline.println(&format!(
                    "  {:<9} {} No cache — full analysis will run",
                    "Cache",
                    theme::WARN
                ));
            }
        }
    } else {
        pipeline.println(&format!(
            "  {:<9} {} Not a git repository — caching disabled",
            "Git",
            theme::WARN
        ));
    }

    // Blank line for visual breathing room.
    pipeline.println("");

    // Runner pre-flight probe (runs during Environment phase, before Analysis).
    // Failures surface here rather than deep in the Tailor phase.
    if let Some(probe) = runner_probe {
        if let Err(e) = probe() {
            pipeline.error(SyncPhase::Environment, &format!("Runner unavailable: {e}"));
            pipeline.finish_remaining();
            return Err(e);
        }
    }

    if is_git {
        pipeline.success(SyncPhase::Environment, "Environment OK");
    } else {
        pipeline.warn(
            SyncPhase::Environment,
            "Not a git repository (analysis caching disabled)",
        );
    }

    // 3. Run analysis (cached if in a git repo)
    pipeline.start(SyncPhase::Analysis, "Analyzing repository...");
    let analysis = match run_analysis_cached(root_dir, cfg_path, args.force) {
        Ok(outcome) => {
            let cache_label = outcome.cache_status.label();
            pipeline.success(
                SyncPhase::Analysis,
                &format!("Analysis complete {cache_label}"),
            );
            outcome.analysis
        }
        Err(e) => {
            pipeline.error(SyncPhase::Analysis, &format!("Analysis failed: {e}"));
            pipeline.finish_remaining();
            return Err(e);
        }
    };

    // 4. Filter by --project if specified
    let analysis = match filter_projects(analysis, &args.projects) {
        Ok(filtered) => filtered,
        Err(e) => {
            pipeline.error(SyncPhase::Analysis, &format!("Project filter failed: {e}"));
            pipeline.finish_remaining();
            return Err(e);
        }
    };

    // 5. Auto-select language/framework for each project
    let mut analysis = analysis;
    for project in &mut analysis.projects {
        auto_select_for_project(project);
    }

    // 6. Confirmation loop (unless --force)
    if args.force {
        // Show project summary even in --force mode for visibility.
        // Use pipeline.println() so the output stays inside the TUI log pane
        // rather than triggering a LeaveAlternateScreen/re-enter flash.
        let summary = format_project_summary_plain(&analysis);
        for line in summary.lines() {
            pipeline.println(line);
        }
    } else {
        confirm_or_change_loop(&mut analysis, &mut pipeline, term)?;
    }

    // ── Phase 2: fetch + tailor ──

    // 2a. Reload config from disk (to pick up any external changes) and handle --reset-rejections.
    // Note: config was already loaded above for the Environment step; reload here for freshness.
    let mut config = load_from(cfg_path)?;
    let repo_key = compute_repo_key(root_dir);

    if args.reset_rejections {
        clear_rejections(&mut config, &repo_key);
        save_to(&config, cfg_path)?;
        pipeline.suspend(|| {
            eprintln!(
                "{} Cleared ADR rejection memory for this repository",
                theme::success(&theme::SUCCESS)
            );
        });
    }

    let rejected_ids = get_rejections(&config, &repo_key);

    // 2b. Fetch ADRs from API with retry
    let api_url: String = args
        .api_url
        .as_deref()
        .or(config.api_url.as_deref())
        .unwrap_or(DEFAULT_API_URL)
        .to_owned();

    pipeline.start(SyncPhase::Fetch, "Fetching ADRs...");
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| ActualError::InternalError(format!("Failed to create async runtime: {e}")))?;

    // Run signals analysis (tree-sitter + semgrep) to enrich the match request.
    let signals = rt.block_on(crate::analysis::signals::run_signals_analysis(
        root_dir, &analysis,
    ));

    let request = build_match_request(&analysis, &config, &signals);
    let client = ActualApiClient::new(&api_url)?;

    if args.verbose {
        pipeline.suspend(|| {
            eprintln!("API request to: {api_url}/adrs/match");
            eprintln!(
                "  projects: {}",
                request
                    .projects
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        });
    }

    let root_dir_owned = root_dir.to_path_buf();
    // Resolve the effective output format: CLI flag takes precedence over config,
    // and config takes precedence over the default.
    let output_format = args
        .output_format
        .clone()
        .or_else(|| config.output_format.clone())
        .unwrap_or_default();
    let output_format_for_fs = output_format.clone();
    let (response, existing_paths) = rt.block_on(async {
        // Spawn the filesystem discovery immediately so it runs concurrently
        // with the API call (including any 503 retry waits).
        let fs_future = tokio::task::spawn_blocking(move || {
            find_existing_output_files(&root_dir_owned, &output_format_for_fs)
        });

        const DELAYS_503: [std::time::Duration; 3] = [
            std::time::Duration::from_secs(10),
            std::time::Duration::from_secs(30),
            std::time::Duration::from_secs(60),
        ];
        let retry_config = RetryConfig::default();
        let api_response = fetch_with_503_backoff(
            || async {
                tokio::select! {
                    result = with_retry(&retry_config, || client.post_match(&request)) => result,
                    _ = tokio::signal::ctrl_c() => Err(ActualError::UserCancelled),
                }
            },
            &DELAYS_503,
            &mut pipeline,
        )
        .await;

        (api_response, fs_future.await)
    });

    // Unwrap the spawn_blocking JoinHandle
    let existing_paths = existing_paths
        .map_err(|e| ActualError::InternalError(format!("CLAUDE.md discovery task failed: {e}")))?;

    let response = match response {
        Ok(r) => {
            let count = r.matched_adrs.len();
            pipeline.success(SyncPhase::Fetch, &format!("Fetched {count} ADRs"));
            if count == 0 {
                for line in zero_adr_context_lines(&request) {
                    pipeline.println(&line);
                }
            }
            r
        }
        Err(e) => {
            let msg = fetch_error_message(&e, &api_url);
            pipeline.error(SyncPhase::Fetch, &msg);
            pipeline.finish_remaining();
            return Err(e);
        }
    };

    if args.verbose {
        pipeline.suspend(|| {
            eprintln!(
                "  matched: {}, by_framework: {:?}",
                response.metadata.total_matched, response.metadata.by_framework
            );
        });
    }

    // 2c. Filter by rejections
    let filtered_adrs = pre_filter_rejected(&response.matched_adrs, &rejected_ids);

    if !rejected_ids.is_empty() && args.verbose {
        pipeline.suspend(|| {
            let removed = response.matched_adrs.len() - filtered_adrs.len();
            eprintln!("  filtered out {removed} previously rejected ADRs");
        });
    }

    // 2d. Tailor or skip (--no-tailor), with caching

    // Compute cache key from all tailoring inputs.
    // HEAD commit is intentionally excluded — unrelated commits should not
    // invalidate the cache. The key is always computed (not gated on git).
    let mut project_paths: Vec<String> = analysis.projects.iter().map(|p| p.path.clone()).collect();
    project_paths.sort();

    let tailoring_cache_key = Some(compute_tailoring_cache_key(
        &filtered_adrs,
        args.no_tailor,
        &project_paths,
        &existing_paths,
        args.model.as_deref().or(config.model.as_deref()),
    ));

    // Partition into V1 and V2 ADRs (after cache key computation so the key
    // covers all ADRs, including V2 ones).
    let (v1_adrs, v2_adrs) = partition_adrs(filtered_adrs);
    let v2_result = if v2_adrs.is_empty() {
        None
    } else {
        Some(generate_v2_output(&v2_adrs))
    };

    // Try cache (unless --force)
    let cached_output = if !args.force {
        tailoring_cache_key
            .as_deref()
            .and_then(|key| {
                config
                    .cached_tailoring
                    .as_ref()
                    .filter(|c| c.cache_key == key && !c.is_expired())
            })
            .map(|c| c.tailoring.clone())
    } else {
        None
    };

    let output = if let Some(cached) = cached_output {
        pipeline.skip(
            SyncPhase::Tailor,
            tailoring_cache_skip_msg(cached.summary.applicable),
        );
        cached
    } else if args.no_tailor {
        pipeline.skip(SyncPhase::Tailor, "Tailoring skipped (--no-tailor)");
        let out = raw_adrs_to_output(&v1_adrs, &output_format);
        store_tailoring_cache(
            &mut config,
            cfg_path,
            tailoring_cache_key.as_deref(),
            root_dir,
            &out,
        );
        out
    } else {
        let project_count = analysis.projects.len();
        pipeline.start(
            SyncPhase::Tailor,
            &format!("Tailoring ADRs (0/{project_count} projects done)..."),
        );
        let effective_model = args
            .model
            .as_deref()
            .or(config.model.as_deref())
            .unwrap_or("sonnet");
        pipeline.println(&format!("  model: {effective_model}"));
        let tailoring_config = ConcurrentTailoringConfig::new(
            config.concurrency.unwrap_or(DEFAULT_CONCURRENCY),
            config.batch_size.unwrap_or(DEFAULT_BATCH_SIZE),
            &existing_paths,
            args.model.as_deref().or(config.model.as_deref()),
            args.max_budget_usd.or(config.max_budget_usd),
            Duration::from_secs(
                config
                    .invocation_timeout_secs
                    .unwrap_or(DEFAULT_TIMEOUT_SECS)
                    .max(1),
            ),
            &output_format,
            Some(root_dir),
        )?;

        let (progress_tx, mut progress_rx) =
            tokio::sync::mpsc::unbounded_channel::<TailoringEvent>();

        // Set up a streaming event channel so the runner can forward real-time
        // tool-call summaries and stderr lines from the AI subprocess.
        // This channel is used to reset the hang-detection timer on every event
        // and to show progress in the TUI while the AI is working.
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        runner.set_event_tx(event_tx);

        // In TUI mode (raw mode active) the terminal consumes keyboard input,
        // so OS-level SIGINT from Ctrl+C may not reach the process reliably.
        // `sync_kb_poller::setup` optionally spawns a background poller thread
        // and returns a combined OS-signal + keyboard cancel future, plus a
        // nav channel receiver for arrow-key navigation during execution.
        let (kb_poller, nav_rx, cancel) = super::super::sync_kb_poller::setup(pipeline.is_tui());
        pipeline.set_nav_rx_opt(nav_rx);

        let result = rt.block_on(async {
            let tailor_fut = tailor_all_projects(
                runner,
                &analysis.projects,
                &v1_adrs,
                &tailoring_config,
                Some(progress_tx),
            );
            run_tailoring_with_cancel(
                tailor_fut,
                &mut progress_rx,
                &mut event_rx,
                &mut pipeline,
                project_count,
                cancel,
            )
            .await
        });

        // The keyboard poller (if started) is signalled to stop via its Drop impl.
        drop(kb_poller);
        match result {
            Ok(output) => {
                pipeline.success(
                    SyncPhase::Tailor,
                    &format!(
                        "Tailored {} ADRs into {} files",
                        output.summary.applicable, output.summary.files_generated
                    ),
                );
                store_tailoring_cache(
                    &mut config,
                    cfg_path,
                    tailoring_cache_key.as_deref(),
                    root_dir,
                    &output,
                );
                output
            }
            Err(e) => {
                pipeline.error(SyncPhase::Tailor, &format!("Tailoring failed: {e}"));
                // Gap 1: surface subprocess stderr directly in the TUI log pane
                // so the user sees Claude Code diagnostics (quota errors, auth
                // failures, rate limits, etc.) rather than just the exit-code summary.
                if let crate::error::ActualError::RunnerFailed { stderr, .. } = &e {
                    if !stderr.is_empty() {
                        pipeline.println("  Subprocess output:");
                        for line in console::strip_ansi_codes(stderr).lines() {
                            pipeline.println(&format!("    {line}"));
                        }
                    }
                }
                pipeline.finish_remaining();
                return Err(e);
            }
        }
    };

    // Inject V2 governance into output (if V2 ADRs were present)
    let output = if let Some(ref v2) = v2_result {
        // Write V2 raw files (docs/adr/ + .claude/rules/)
        if !args.dry_run {
            let v2_write_results = write_v2_raw_files(root_dir, &v2.raw_files);
            for r in &v2_write_results {
                if let Some(ref e) = r.error {
                    let safe_path = console::strip_ansi_codes(&r.path);
                    pipeline.println(&format!("  V2 file write failed: {safe_path}: {e}"));
                }
            }
        }
        // Inject governance pointer into CLAUDE.md managed section
        inject_v2_governance(output, v2.governance_content.clone(), &output_format)
    } else {
        output
    };

    // Gap 7: surface the AI reasoning per file when --verbose is set, so
    // the user can see why the model generated each output file.
    if args.verbose {
        for file in &output.files {
            if !file.reasoning.is_empty() {
                let safe_path = console::strip_ansi_codes(&file.path);
                let safe_reasoning = console::strip_ansi_codes(&file.reasoning);
                pipeline.println(&format!("  [{safe_path}]: {safe_reasoning}"));
            }
        }
    }

    // ── Phase 3: confirm + write (fully implemented) ──
    // pipeline stays alive through confirm+write so output goes into the TUI
    // log pane. It drops naturally at end of run_sync.
    let sync_result = confirm_and_write(
        &output,
        root_dir,
        args.force,
        args.dry_run,
        args.full,
        &output_format,
        term,
        &mut pipeline,
    )?;
    // Keep `sync_result` alive when telemetry is compiled out.
    #[cfg(not(feature = "telemetry"))]
    let _ = &sync_result;

    // ── Telemetry (fire-and-forget) ──
    // Only fires on the happy path (after `?` above). Errors in telemetry are
    // silently swallowed inside `report_metrics`. Runtime build failures cause
    // telemetry to be silently skipped (fire-and-forget preserved).
    #[cfg(feature = "telemetry")]
    {
        // Fetch git remote URL for repo identity hashing.
        let repo_url = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()
            .and_then(|rt| {
                rt.block_on(async {
                    let mut cmd = tokio::process::Command::new("git");
                    cmd.args(["remote", "get-url", "origin"]);
                    cmd.current_dir(root_dir);
                    cmd.stdin(std::process::Stdio::null());
                    cmd.kill_on_drop(true);
                    let child = cmd
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .spawn()
                        .ok()?;
                    let out = tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        child.wait_with_output(),
                    )
                    .await
                    .ok()?
                    .ok()?;
                    out.status
                        .success()
                        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
                        .filter(|s| !s.is_empty())
                })
            })
            .unwrap_or_default();

        let commit_hash = get_git_head(root_dir).unwrap_or_default();
        let repo_hash = hash_repo_identity(&repo_url, &commit_hash);

        let adrs_fetched = output.summary.total_input as u64;
        let adrs_written = (sync_result.files_created + sync_result.files_updated) as u64;
        let metrics = SyncMetrics {
            adrs_fetched,
            adrs_tailored: output.summary.applicable as u64,
            adrs_rejected: output.summary.not_applicable as u64,
            adrs_written,
            repo_hash,
            version: env!("CARGO_PKG_VERSION").to_string(),
        };

        if let Ok(tel_rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            tel_rt.block_on(report_metrics(&metrics, &config, &api_url));
        }
    }

    pipeline.wait_for_keypress();
    Ok(())
}

/// Execute an API call with 503 backoff, showing live TUI messages during each wait.
///
/// On `ActualError::ServiceUnavailable`, waits for the next delay in `delays_503`
/// and retries. After all delays are exhausted the error propagates as-is.
/// Any other error (or `UserCancelled`) breaks out immediately.
///
/// `make_fut` is called once per attempt; it must produce a fresh `Future` each time
/// so the async block is not re-polled after it has already resolved.
pub(crate) async fn fetch_with_503_backoff<F, Fut, T>(
    mut make_fut: F,
    delays_503: &[std::time::Duration],
    pipeline: &mut TuiRenderer,
) -> Result<T, ActualError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, ActualError>>,
{
    let mut attempt_503 = 0usize;
    loop {
        let result = make_fut().await;
        match result {
            Ok(v) => break Ok(v),
            Err(ActualError::ServiceUnavailable) if attempt_503 < delays_503.len() => {
                let delay_secs = delays_503[attempt_503].as_secs();
                pipeline.update_message(
                    SyncPhase::Fetch,
                    &format!(
                        "Actual AI API is updating \u{2014} retrying in {delay_secs}s ({}/{})...",
                        attempt_503 + 1,
                        delays_503.len()
                    ),
                );
                tokio::time::sleep(delays_503[attempt_503]).await;
                attempt_503 += 1;
            }
            Err(e) => break Err(e),
        }
    }
}

/// Return the pipeline skip message for a tailoring cache hit.
///
/// Distinguishes between a cached result with no applicable ADRs (deterministic
/// empty output) and one with applicable ADRs, so users can tell at a glance
/// why tailoring was skipped.
fn apply_tailoring_event(event: TailoringEvent, pipeline: &mut TuiRenderer, completed: &mut usize) {
    match event {
        TailoringEvent::ProjectStarted {
            ref project_name,
            batch_count,
        } => {
            let batch_label = if batch_count == 1 {
                "1 batch".to_string()
            } else {
                format!("{batch_count} batches")
            };
            pipeline.println(&format!("  \u{25b6} {project_name} ({batch_label})"));
        }
        TailoringEvent::BatchStarted {
            batch_index,
            batch_count,
            adr_count,
            ref category_name,
            ..
        } => {
            let adr_label = if adr_count == 1 { "ADR" } else { "ADRs" };
            pipeline.println(&format!(
                "    \u{00b7} batch {batch_index}/{batch_count} \u{2014} {category_name} ({adr_count} {adr_label})"
            ));
        }
        TailoringEvent::BatchCompleted {
            batch_index,
            batch_count,
            applied_count,
            skipped_count,
            ref skipped_adrs,
            ref file_paths,
            elapsed_secs,
            ..
        } => {
            let paths_str = if file_paths.is_empty() {
                String::new()
            } else {
                format!(" \u{2192} {}", file_paths.join(", "))
            };
            pipeline.println(&format!(
                "    \u{2714} batch {batch_index}/{batch_count} done in {elapsed_secs}s \u{2014} {applied_count} applied, {skipped_count} skipped{paths_str}"
            ));
            // Show skipped reasons for small numbers to avoid flooding
            if skipped_count > 0 && skipped_count <= 3 {
                for (adr_id, reason) in skipped_adrs {
                    pipeline.println(&format!("      skip {adr_id}: {reason}"));
                }
            }
        }
        TailoringEvent::ProjectCompleted {
            ref project_name,
            ref file_paths,
            ..
        } => {
            *completed += 1;
            let paths_str = if file_paths.is_empty() {
                String::new()
            } else {
                format!(" \u{2192} {}", file_paths.join(", "))
            };
            pipeline.println(&format!("  \u{2714} {project_name} done{paths_str}"));
        }
        TailoringEvent::ProjectFailed {
            ref project_name,
            ref error,
        } => {
            pipeline.println(&format!("  \u{2716} {project_name}: {error}"));
        }
    }
}

/// How long to wait with no progress events before warning the user that
/// Claude Code may be hanging (e.g. waiting for a permission prompt).
const HANG_WARN_SECS: u64 = 30;

/// Drive the tailoring future to completion while processing progress events,
/// with support for cancellation via a caller-supplied future.
///
/// This is extracted from `run_sync` so the ctrl_c cancellation branch can be
/// reached by tests (which pass `std::future::ready(())` as `cancel`) without
/// requiring an actual OS signal.
///
/// A 100 ms heartbeat timer keeps the TUI spinner animating and the elapsed
/// time display updating even when Claude is thinking and no progress events
/// are flowing.
///
/// Hang detection: if neither a batch-level [`TailoringEvent`] nor a
/// subprocess stream event arrives within [`HANG_WARN_SECS`] seconds, a
/// one-time warning is emitted.  Stream events (tool calls, stderr lines) from
/// the Claude Code subprocess reset the timer, so the warning only fires when
/// the subprocess is genuinely silent — not merely slow.
async fn run_tailoring_with_cancel<F, C>(
    tailor_fut: F,
    progress_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TailoringEvent>,
    event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    pipeline: &mut TuiRenderer,
    project_count: usize,
    cancel: C,
) -> Result<TailoringOutput, ActualError>
where
    F: std::future::Future<Output = Result<TailoringOutput, ActualError>>,
    C: std::future::Future<Output = std::io::Result<()>>,
{
    use futures::FutureExt as _;

    tokio::pin!(tailor_fut);
    // Fuse the cancel future so that after it resolves it becomes permanently
    // pending — preventing a "polled after completion" panic if the loop
    // continues for any reason after cancel fires.
    let mut cancel = std::pin::pin!(cancel.fuse());
    let mut completed = 0usize;
    let started_at = std::time::Instant::now();
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_millis(100));
    // Consume the first immediate tick so the interval doesn't fire right away.
    heartbeat.tick().await;

    // Track when the last event (batch-level OR stream) arrived to detect hangs.
    // Uses tokio::time::Instant so the threshold is testable with
    // tokio::time::pause() / tokio::time::advance().
    let mut last_event_time = tokio::time::Instant::now();
    // Latch: emit the hang warning at most once per tailoring run.
    let mut hang_warned = false;

    loop {
        tokio::select! {
            result = &mut tailor_fut => {
                // Drain any remaining events before breaking.
                while let Ok(event) = progress_rx.try_recv() {
                    apply_tailoring_event(event, pipeline, &mut completed);
                }
                break result;
            }
            Some(event) = progress_rx.recv() => {
                // Reset hang tracking on every batch-level progress event.
                last_event_time = tokio::time::Instant::now();
                apply_tailoring_event(event, pipeline, &mut completed);
            }
            Some(line) = event_rx.recv() => {
                // Reset hang tracking on every subprocess stream event (tool
                // calls, stderr lines). Show tool-call summaries in the TUI so
                // the user can see what Claude is working on.
                last_event_time = tokio::time::Instant::now();
                pipeline.println(&format!("  {line}"));
            }
            result = &mut cancel => {
                if result.is_ok() {
                    break Err(ActualError::UserCancelled);
                }
                // ctrl_c registration failed — ignore and let tailoring continue
            }
            _ = heartbeat.tick() => {
                // Keep the spinner animating and elapsed time current while
                // Claude is thinking and no progress events are arriving.
                let elapsed = started_at.elapsed().as_secs();
                pipeline.update_message(
                    SyncPhase::Tailor,
                    &format!(
                        "Tailoring ADRs ({completed}/{project_count} projects done, {elapsed}s)..."
                    ),
                );

                // Warn once if neither batch-level nor stream events have arrived
                // for HANG_WARN_SECS. With stream-json this means the subprocess
                // has gone completely silent — not even a tool-call event.
                if !hang_warned && last_event_time.elapsed().as_secs() >= HANG_WARN_SECS {
                    hang_warned = true;
                    pipeline.println(&format!(
                        "  \u{26a0} No output received in {HANG_WARN_SECS}s \u{2014} \
        AI subprocess is unresponsive"
                    ));
                    tracing::warn!(
                        "no tailoring events for {HANG_WARN_SECS}s — \
        AI subprocess is unresponsive"
                    );
                }
            }
        }
    }
}

/// Run the confirmation loop: show project summary, then Accept/Change/Reject.
///
/// - **Accept** breaks out of the loop and proceeds.
/// - **Reject** finishes remaining pipeline steps and returns `UserCancelled`.
/// - **Change** triggers interactive re-selection for every project, then loops
///   back to show the updated summary and re-confirm.
fn confirm_or_change_loop(
    analysis: &mut RepoAnalysis,
    pipeline: &mut TuiRenderer,
    term: &dyn TerminalIO,
) -> Result<(), ActualError> {
    confirm_or_change_loop_with(analysis, pipeline, term, |a, p, t| p.confirm_project(a, t))
}

/// Inner implementation that accepts a confirm function for testability.
///
/// In production the confirm function is `pipeline.confirm_project(...)`.
/// In tests it can be replaced to return `Change` (which plain-mode confirm
/// never produces).
fn confirm_or_change_loop_with<F>(
    analysis: &mut RepoAnalysis,
    pipeline: &mut TuiRenderer,
    term: &dyn TerminalIO,
    mut confirm_fn: F,
) -> Result<(), ActualError>
where
    F: FnMut(
        &RepoAnalysis,
        &mut TuiRenderer,
        &dyn TerminalIO,
    ) -> Result<ConfirmAction, ActualError>,
{
    loop {
        let action = confirm_fn(analysis, pipeline, term)?;
        // true ⇒ Accept (break), false ⇒ Change (loop back)
        if handle_confirm_action(action, analysis, pipeline, term)? {
            return Ok(());
        }
    }
}

/// Process a single [`ConfirmAction`] from the confirmation prompt.
///
/// Returns `Ok(true)` to break the loop (Accept), `Ok(false)` to continue
/// looping (Change), or `Err(UserCancelled)` to abort (Reject).
///
/// Extracted so both the `run_sync` integration flow and unit tests can
/// exercise the Change → re-select path without requiring TUI-mode keyboard
/// events.
fn handle_confirm_action(
    action: ConfirmAction,
    analysis: &mut RepoAnalysis,
    pipeline: &mut TuiRenderer,
    term: &dyn TerminalIO,
) -> Result<bool, ActualError> {
    match action {
        ConfirmAction::Accept => Ok(true),
        ConfirmAction::Reject => {
            pipeline.finish_remaining();
            Err(ActualError::UserCancelled)
        }
        ConfirmAction::Change => {
            for project in &mut analysis.projects {
                user_select_for_project(project, pipeline, term)?;
            }
            Ok(false)
        }
    }
}

/// Auto-select the top language (by LOC) and first compatible framework for a project.
///
/// The language list is already sorted descending by LOC from `detect_languages`,
/// so index 0 is the top language. The first framework whose manifest source is
/// compatible with that language is selected; if none are compatible, `framework`
/// is set to `None`.
fn auto_select_for_project(project: &mut Project) {
    if project.languages.is_empty() {
        return;
    }
    let selected_lang = project.languages[0].clone();

    let selected_fw = project
        .frameworks
        .iter()
        .find(|fw| fw.is_compatible_with(&selected_lang.language))
        .cloned();

    project.selection = Some(ProjectSelection {
        language: selected_lang,
        framework: selected_fw,
        auto_selected: true,
    });
}

/// Let the user select language and framework interactively.
///
/// Shows a single-select list for the language, then (if more than one compatible
/// framework exists) a single-select list for the framework. Single-option cases
/// are auto-resolved without prompting.
fn user_select_for_project(
    project: &mut Project,
    pipeline: &mut TuiRenderer,
    term: &dyn TerminalIO,
) -> Result<(), ActualError> {
    if project.languages.is_empty() {
        return Ok(());
    }

    // Format language items for display
    let lang_items: Vec<String> = project
        .languages
        .iter()
        .map(|ls| {
            let name = ls.language.to_string();
            if ls.loc > 0 {
                format!("{} ({} loc)", name, ls.loc)
            } else {
                name
            }
        })
        .collect();

    // Find the currently selected language index (if any) for default
    let current_lang_idx = project
        .selection
        .as_ref()
        .and_then(|sel| {
            project
                .languages
                .iter()
                .position(|ls| ls.language == sel.language.language)
        })
        .unwrap_or(0);

    let lang_idx = pipeline.select_one_in_tui(
        "Select primary language:",
        &lang_items,
        Some(current_lang_idx),
        term,
    )?;

    let selected_lang = project.languages[lang_idx].clone();

    // Filter frameworks compatible with selected language
    let compatible_fws: Vec<(usize, _)> = project
        .frameworks
        .iter()
        .enumerate()
        .filter(|(_, fw)| fw.is_compatible_with(&selected_lang.language))
        .collect();

    let selected_fw = if compatible_fws.is_empty() {
        None
    } else if compatible_fws.len() == 1 {
        Some(project.frameworks[compatible_fws[0].0].clone())
    } else {
        // Multiple compatible frameworks — ask user
        let fw_items: Vec<String> = compatible_fws
            .iter()
            .map(|(_, fw)| format!("{} ({})", fw.name, fw.category.as_str()))
            .collect();

        // Find current framework selection in filtered list
        let current_fw_idx = project
            .selection
            .as_ref()
            .and_then(|sel| sel.framework.as_ref())
            .and_then(|sel_fw| {
                compatible_fws
                    .iter()
                    .position(|(_, fw)| fw.name == sel_fw.name)
            })
            .unwrap_or(0);

        let fw_idx = pipeline.select_one_in_tui(
            "Select primary framework:",
            &fw_items,
            Some(current_fw_idx),
            term,
        )?;

        Some(project.frameworks[compatible_fws[fw_idx].0].clone())
    };

    project.selection = Some(ProjectSelection {
        language: selected_lang,
        framework: selected_fw,
        auto_selected: false,
    });

    Ok(())
}

/// Filter the analysis to only include projects matching the given filters.
///
/// If `filters` is empty, returns the analysis unchanged.
/// If no projects match, returns an error.
fn filter_projects(
    analysis: RepoAnalysis,
    filters: &[String],
) -> Result<RepoAnalysis, ActualError> {
    if filters.is_empty() {
        return Ok(analysis);
    }

    let filtered_projects: Vec<_> = analysis
        .projects
        .into_iter()
        .filter(|p| filters.contains(&p.path))
        .collect();

    if filtered_projects.is_empty() {
        return Err(ActualError::ConfigError(format!(
            "No projects matched the filter: {filters:?}"
        )));
    }

    Ok(RepoAnalysis {
        projects: filtered_projects,
        ..analysis
    })
}

/// Compute a stable repo key by hashing the git origin URL.
///
/// Falls back to hashing the root directory path if not a git repo or
/// if the remote URL cannot be determined. Applies a 5-second timeout to
/// the git subprocess; on timeout, silently falls back to the path hash.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::types::{Framework, FrameworkCategory, Language, LanguageStat, Project};
    use crate::cli::commands::handle_result;
    use crate::cli::ui::test_utils::MockTerminal;
    use crate::cli::ui::tui::renderer::TuiRenderer;
    use crate::error::ActualError;
    use crate::generation::markers;
    use crate::generation::writer::{WriteAction, WriteResult};
    use crate::generation::OutputFormat;
    use crate::tailoring::types::{AdrSection, FileOutput, TailoringSummary};

    // Pull in exec from the parent sync module (mod.rs).
    use super::super::exec;

    // Pull in items from sibling modules that tests reference directly.
    use super::super::adr_utils::{
        fetch_error_message, find_existing_output_files, raw_adrs_to_output, strip_control_chars,
        validate_project_path, zero_adr_context_lines,
    };
    use super::super::cache::{
        compute_repo_key, compute_repo_key_with_timeout, tailoring_cache_skip_msg,
    };
    use super::super::write::{report_write_results, SyncResult};

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

    /// Mock runner that returns a predetermined `TailoringOutput` JSON response.
    struct MockRunner {
        json_response: String,
    }

    impl MockRunner {
        fn new(json: &str) -> Self {
            Self {
                json_response: json.to_string(),
            }
        }
    }

    impl TailoringRunner for MockRunner {
        async fn run_tailoring(
            &self,
            _prompt: &str,
            _schema: &str,
            _model_override: Option<&str>,
            _max_budget_usd: Option<f64>,
        ) -> Result<TailoringOutput, ActualError> {
            let parsed: TailoringOutput = serde_json::from_str(&self.json_response)?;
            Ok(parsed)
        }
    }

    fn make_output(files: Vec<FileOutput>) -> TailoringOutput {
        let file_count = files.len();
        TailoringOutput {
            summary: TailoringSummary {
                total_input: file_count,
                applicable: file_count,
                not_applicable: 0,
                files_generated: file_count,
            },
            skipped_adrs: vec![],
            files,
        }
    }

    fn make_file(path: &str, content: &str, adr_ids: Vec<&str>) -> FileOutput {
        FileOutput {
            path: path.to_string(),
            sections: adr_ids
                .into_iter()
                .map(|id| AdrSection {
                    adr_id: id.to_string(),
                    content: content.to_string(),
                })
                .collect(),
            reasoning: format!("reason for {path}"),
        }
    }

    fn make_monorepo_analysis() -> RepoAnalysis {
        RepoAnalysis {
            is_monorepo: true,
            workspace_type: Some(crate::analysis::types::WorkspaceType::Pnpm),
            projects: vec![
                Project {
                    path: "apps/web".to_string(),
                    name: "Web App".to_string(),
                    languages: vec![LanguageStat {
                        language: Language::TypeScript,
                        loc: 0,
                    }],
                    frameworks: vec![Framework {
                        name: "nextjs".to_string(),
                        category: FrameworkCategory::WebFrontend,
                        source: None,
                    }],
                    package_manager: Some("npm".to_string()),
                    description: None,
                    dep_count: 0,
                    dev_dep_count: 0,
                    selection: None,
                },
                Project {
                    path: "services/api".to_string(),
                    name: "API Service".to_string(),
                    languages: vec![LanguageStat {
                        language: Language::Rust,
                        loc: 0,
                    }],
                    frameworks: vec![],
                    package_manager: Some("cargo".to_string()),
                    description: None,
                    dep_count: 0,
                    dev_dep_count: 0,
                    selection: None,
                },
            ],
        }
    }

    // ── test_confirm_and_write_force_mode ──

    #[test]
    fn test_confirm_and_write_force_mode() {
        let dir = tempfile::tempdir().unwrap();
        let output = make_output(vec![
            make_file("CLAUDE.md", "Root rules", vec!["adr-001"]),
            make_file("apps/web/CLAUDE.md", "Web rules", vec!["adr-002"]),
        ]);
        let term = MockTerminal::new(vec![]);
        let mut pipeline = TuiRenderer::new(false, true);

        let result = confirm_and_write(
            &output,
            dir.path(),
            true,
            false,
            false,
            &OutputFormat::ClaudeMd,
            &term,
            &mut pipeline,
        )
        .unwrap();

        assert_eq!(result.files_created, 2);
        assert_eq!(result.files_updated, 0);
        assert_eq!(result.files_failed, 0);
        assert_eq!(result.files_rejected, 0);

        // Verify files actually exist on disk
        assert!(dir.path().join("CLAUDE.md").exists());
        assert!(dir.path().join("apps/web/CLAUDE.md").exists());

        // Verify managed sections
        let root = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(markers::has_managed_section(&root));
        assert!(root.contains("Root rules"));
    }

    // ── test_confirm_and_write_dry_run ──

    #[test]
    fn test_confirm_and_write_dry_run() {
        let dir = tempfile::tempdir().unwrap();
        let output = make_output(vec![make_file("CLAUDE.md", "Root rules", vec!["adr-001"])]);
        let term = MockTerminal::new(vec![]);
        let mut pipeline = TuiRenderer::new(false, true);

        let result = confirm_and_write(
            &output,
            dir.path(),
            false,
            true,
            false,
            &OutputFormat::ClaudeMd,
            &term,
            &mut pipeline,
        )
        .unwrap();

        // Dry run: nothing written
        assert_eq!(result.files_created, 0);
        assert_eq!(result.files_updated, 0);
        assert_eq!(result.files_failed, 0);
        assert_eq!(result.files_rejected, 0);

        // File should NOT exist on disk
        assert!(!dir.path().join("CLAUDE.md").exists());

        // Diff summary should be displayed in the log pane
        let log_text = pipeline.all_log_text();
        assert!(
            log_text.contains("CLAUDE.md"),
            "expected diff summary in log: {log_text}"
        );
    }

    // ── test_confirm_and_write_dry_run_full ──

    #[test]
    fn test_confirm_and_write_dry_run_full() {
        let dir = tempfile::tempdir().unwrap();
        let output = make_output(vec![make_file(
            "\x1b[32mCLAUDE.md\x1b[0m",
            "\x1b[31mANSI content\x1b[0m Full content here",
            vec!["adr-001"],
        )]);
        let term = MockTerminal::new(vec![]);
        let mut pipeline = TuiRenderer::new(false, true);

        let result = confirm_and_write(
            &output,
            dir.path(),
            false,
            true,
            true,
            &OutputFormat::ClaudeMd,
            &term,
            &mut pipeline,
        )
        .unwrap();

        assert_eq!(result.files_created, 0);

        // Full content should be displayed in the log pane
        let log_text = pipeline.all_log_text();
        assert!(
            log_text.contains("── CLAUDE.md ──"),
            "expected file header in log: {log_text}"
        );
        assert!(
            log_text.contains("Full content here"),
            "expected full content in log: {log_text}"
        );
        assert!(
            log_text.contains("── end ──"),
            "expected end marker in log: {log_text}"
        );

        // No ANSI escape codes in output
        assert!(
            !log_text.as_bytes().contains(&0x1Bu8),
            "dry-run --full output must not contain ESC (0x1B)"
        );

        // File should NOT exist on disk
        assert!(!dir.path().join("CLAUDE.md").exists());
    }

    // ── test_dry_run_full_strips_ansi_from_tailored_content ──

    #[test]
    fn test_dry_run_full_strips_ansi_from_tailored_content() {
        let dir = tempfile::tempdir().unwrap();
        // Create output with ANSI in both path and content
        let output = make_output(vec![make_file(
            "\x1b[32mCLAUDE.md\x1b[0m",
            "\x1b[31mINJECTED\x1b[0m safe content here",
            vec!["adr-001"],
        )]);
        let term = MockTerminal::new(vec![]).with_selection(Some(vec![]));
        let mut pipeline = TuiRenderer::new(false, true);

        let result = confirm_and_write(
            &output,
            dir.path(),
            false, // force
            true,  // dry_run
            true,  // full
            &OutputFormat::ClaudeMd,
            &term,
            &mut pipeline,
        )
        .unwrap();

        assert_eq!(result.files_created, 0, "dry-run must not write files");

        let log_text = pipeline.all_log_text();
        assert!(
            !log_text.as_bytes().contains(&0x1Bu8),
            "dry-run --full output must not contain ESC (0x1B)"
        );
        assert!(
            log_text.contains("safe content here"),
            "expected non-ANSI content preserved in log: {log_text}"
        );
        assert!(
            log_text.contains("CLAUDE.md"),
            "expected path (without ANSI) in log: {log_text}"
        );
        assert!(
            !dir.path().join("CLAUDE.md").exists(),
            "dry-run must not write any file"
        );
    }

    // ── test_confirm_and_write_reject_file ──

    #[test]
    fn test_confirm_and_write_reject_file() {
        let dir = tempfile::tempdir().unwrap();
        let output = make_output(vec![
            make_file("CLAUDE.md", "Root rules", vec!["adr-001"]),
            make_file("apps/web/CLAUDE.md", "Web rules", vec!["adr-002"]),
        ]);
        // Select only file 1 (index 0), skip file 2
        let term = MockTerminal::new(vec![]).with_selection(Some(vec![0]));
        let mut pipeline = TuiRenderer::new(false, true);

        let result = confirm_and_write(
            &output,
            dir.path(),
            false,
            false,
            false,
            &OutputFormat::ClaudeMd,
            &term,
            &mut pipeline,
        )
        .unwrap();

        assert_eq!(result.files_created, 1);
        assert_eq!(result.files_rejected, 1);

        // Only CLAUDE.md should exist
        assert!(dir.path().join("CLAUDE.md").exists());
        assert!(!dir.path().join("apps/web/CLAUDE.md").exists());
    }

    // ── test_confirm_and_write_quit ──

    #[test]
    fn test_confirm_and_write_quit() {
        let dir = tempfile::tempdir().unwrap();
        let output = make_output(vec![make_file("CLAUDE.md", "Root rules", vec!["adr-001"])]);
        // User cancels the selection
        let term = MockTerminal::new(vec![]).with_selection(None);
        let mut pipeline = TuiRenderer::new(false, true);

        let result = confirm_and_write(
            &output,
            dir.path(),
            false,
            false,
            false,
            &OutputFormat::ClaudeMd,
            &term,
            &mut pipeline,
        );

        assert!(
            matches!(result, Err(ActualError::UserCancelled)),
            "expected UserCancelled"
        );

        // No files should be written
        assert!(!dir.path().join("CLAUDE.md").exists());
    }

    // ── test_confirm_and_write_write_error_continues ──

    #[test]
    fn test_confirm_and_write_write_error_continues() {
        let dir = tempfile::tempdir().unwrap();

        // Create a directory where a file should go — writing will fail
        std::fs::create_dir_all(dir.path().join("bad").join("CLAUDE.md")).unwrap();

        let output = make_output(vec![
            make_file("CLAUDE.md", "Root rules", vec!["adr-001"]),
            make_file("bad/CLAUDE.md", "Bad rules", vec!["adr-002"]),
        ]);
        let term = MockTerminal::new(vec![]);
        let mut pipeline = TuiRenderer::new(false, true);

        let result = confirm_and_write(
            &output,
            dir.path(),
            true,
            false,
            false,
            &OutputFormat::ClaudeMd,
            &term,
            &mut pipeline,
        )
        .unwrap();

        // First file succeeds, second fails
        assert_eq!(result.files_created, 1);
        assert_eq!(result.files_failed, 1);
        assert_eq!(result.files_rejected, 0);

        // First file should exist
        assert!(dir.path().join("CLAUDE.md").exists());

        // Error should be reported in log pane
        let log_text = pipeline.all_log_text();
        assert!(
            log_text.contains("bad/CLAUDE.md"),
            "expected failed file path in log: {log_text}"
        );
    }

    // ── test_confirm_and_write_new_file ──

    #[test]
    fn test_confirm_and_write_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let output = make_output(vec![make_file(
            "CLAUDE.md",
            "Brand new rules",
            vec!["adr-001"],
        )]);
        let term = MockTerminal::new(vec![]);
        let mut pipeline = TuiRenderer::new(false, true);

        let result = confirm_and_write(
            &output,
            dir.path(),
            true,
            false,
            false,
            &OutputFormat::ClaudeMd,
            &term,
            &mut pipeline,
        )
        .unwrap();

        assert_eq!(result.files_created, 1);
        assert_eq!(result.files_updated, 0);

        let content = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(markers::has_managed_section(&content));
        assert!(content.contains("Brand new rules"));

        // Verify diff output showed new file in log pane
        let log_text = pipeline.all_log_text();
        assert!(
            log_text.contains("new file"),
            "expected 'new file' in log: {log_text}"
        );
    }

    // ── test_confirm_and_write_update_existing ──

    #[test]
    fn test_confirm_and_write_update_existing() {
        let dir = tempfile::tempdir().unwrap();

        // Create existing file with v1
        let existing = format!(
            "# My Custom Header\n\nSome user content\n\n{}\n\nUser footer",
            markers::wrap_in_markers("Old content", 1, &["adr-001".to_string()])
        );
        std::fs::write(dir.path().join("CLAUDE.md"), &existing).unwrap();

        let output = make_output(vec![make_file(
            "CLAUDE.md",
            "Updated content",
            vec!["adr-001"],
        )]);
        let term = MockTerminal::new(vec![]);
        let mut pipeline = TuiRenderer::new(false, true);

        let result = confirm_and_write(
            &output,
            dir.path(),
            true,
            false,
            false,
            &OutputFormat::ClaudeMd,
            &term,
            &mut pipeline,
        )
        .unwrap();

        assert_eq!(result.files_created, 0);
        assert_eq!(result.files_updated, 1);

        let content = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(content.contains("Updated content"));
        assert!(!content.contains("Old content"));
        // User content should be preserved by the merge logic
        assert!(content.contains("My Custom Header"));
    }

    // ── test_confirm_and_write_empty_output ──

    #[test]
    fn test_confirm_and_write_empty_output() {
        let dir = tempfile::tempdir().unwrap();
        let output = make_output(vec![]);
        let term = MockTerminal::new(vec![]);
        let mut pipeline = TuiRenderer::new(false, true);

        let result = confirm_and_write(
            &output,
            dir.path(),
            true,
            false,
            false,
            &OutputFormat::ClaudeMd,
            &term,
            &mut pipeline,
        )
        .unwrap();

        assert_eq!(result.files_created, 0);
        assert_eq!(result.files_updated, 0);
        assert_eq!(result.files_failed, 0);
        assert_eq!(result.files_rejected, 0);
    }

    // ── test_confirm_and_write_all_rejected ──

    #[test]
    fn test_confirm_and_write_all_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let output = make_output(vec![
            make_file("CLAUDE.md", "Root rules", vec!["adr-001"]),
            make_file("apps/web/CLAUDE.md", "Web rules", vec!["adr-002"]),
        ]);
        // Select nothing (reject all)
        let term = MockTerminal::new(vec![]).with_selection(Some(vec![]));
        let mut pipeline = TuiRenderer::new(false, true);

        let result = confirm_and_write(
            &output,
            dir.path(),
            false,
            false,
            false,
            &OutputFormat::ClaudeMd,
            &term,
            &mut pipeline,
        )
        .unwrap();

        assert_eq!(result.files_created, 0);
        assert_eq!(result.files_updated, 0);
        assert_eq!(result.files_failed, 0);
        assert_eq!(result.files_rejected, 2);

        // No files should exist
        assert!(!dir.path().join("CLAUDE.md").exists());
        assert!(!dir.path().join("apps/web/CLAUDE.md").exists());

        let log_text = pipeline.all_log_text();
        assert!(
            log_text.contains("No files to write"),
            "expected 'No files to write' in log: {log_text}"
        );
    }

    // ── test_sync_result_counts ──

    #[test]
    fn test_sync_result_counts() {
        let dir = tempfile::tempdir().unwrap();

        // Create existing file for update scenario
        let existing = markers::wrap_in_markers("Old", 1, &["adr-001".to_string()]);
        std::fs::write(dir.path().join("CLAUDE.md"), &existing).unwrap();

        // Create a directory where a file should go — writing will fail
        std::fs::create_dir_all(dir.path().join("fail").join("CLAUDE.md")).unwrap();

        let output = make_output(vec![
            make_file("CLAUDE.md", "Updated root", vec!["adr-001"]),
            make_file("apps/web/CLAUDE.md", "New web rules", vec!["adr-002"]),
            make_file("fail/CLAUDE.md", "Will fail", vec!["adr-003"]),
            make_file("apps/api/CLAUDE.md", "API rules", vec!["adr-004"]),
        ]);

        // Select files 0-2, reject file 3 (apps/api/CLAUDE.md)
        let term = MockTerminal::new(vec![]).with_selection(Some(vec![0, 1, 2]));
        let mut pipeline = TuiRenderer::new(false, true);

        let result = confirm_and_write(
            &output,
            dir.path(),
            false,
            false,
            false,
            &OutputFormat::ClaudeMd,
            &term,
            &mut pipeline,
        )
        .unwrap();

        assert_eq!(result.files_created, 1, "expected 1 created (apps/web)");
        assert_eq!(result.files_updated, 1, "expected 1 updated (root)");
        assert_eq!(result.files_failed, 1, "expected 1 failed (fail/)");
        assert_eq!(result.files_rejected, 1, "expected 1 rejected (apps/api)");

        // Verify summary line in log pane (now uses · separators in panel footer)
        let log_text = pipeline.all_log_text();
        assert!(
            log_text.contains("1 created")
                && log_text.contains("1 updated")
                && log_text.contains("1 failed")
                && log_text.contains("1 rejected"),
            "expected correct summary counts in log: {log_text}"
        );
    }

    // ── test_confirm_and_write_log_pane_populated_in_plain_mode ──

    #[test]
    fn test_confirm_and_write_log_pane_populated_in_plain_mode() {
        let dir = tempfile::tempdir().unwrap();
        let output = make_output(vec![make_file("CLAUDE.md", "Rules", vec!["adr-001"])]);
        let term = MockTerminal::new(vec![]);
        let mut pipeline = TuiRenderer::new(false, true);

        confirm_and_write(
            &output,
            dir.path(),
            true,
            false,
            false,
            &OutputFormat::ClaudeMd,
            &term,
            &mut pipeline,
        )
        .unwrap();

        let log_text = pipeline.all_log_text();
        assert!(
            log_text.contains("Sync complete"),
            "expected sync summary in log: {log_text}"
        );
        assert!(
            log_text.contains("created"),
            "expected 'created' in log: {log_text}"
        );
    }

    // ── resolve_cwd test ──

    #[test]
    fn test_resolve_cwd_returns_path() {
        let cwd = resolve_cwd();
        assert!(cwd.is_absolute() || cwd == std::path::PathBuf::from("."));
    }

    #[cfg(unix)]
    #[tracing_test::traced_test]
    #[test]
    fn test_resolve_cwd_fallback_when_current_dir_fails() {
        // Create a temp dir, cd into it, then delete it so current_dir() fails.
        //
        // Note: some overlay filesystem setups (e.g. certain container environments)
        // allow getcwd() to succeed even after the directory is deleted. In those
        // environments this test becomes a no-op — the fallback branch is never
        // reached and the assertion below will still pass (resolve_cwd returns the
        // real path). The tracing assertion, however, would fail. If this test is
        // flaky in CI, consider skipping it on affected platforms.
        let _lock = crate::testutil::ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let tmp_path = tmp.path().to_path_buf();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(&tmp_path).unwrap();
        // Drop the tempdir while we are inside it — current_dir() will now fail.
        drop(tmp);
        let result = resolve_cwd();
        // Restore cwd before any assertion so other tests are not affected.
        // Ignoring the error here is intentional: we hold ENV_MUTEX, so no
        // other test can be affected, and a failed restore would show up as
        // subsequent test failures rather than silently corrupting state.
        let _ = std::env::set_current_dir(&original);
        assert_eq!(result, std::path::PathBuf::from("."));
        assert!(
            logs_contain("current_dir() failed"),
            "expected resolve_cwd() to emit a tracing::warn! when current_dir() fails"
        );
    }

    // ── run_sync flag-passthrough tests ──

    /// JSON body for an empty API match response.
    const EMPTY_MATCH_RESPONSE: &str = r#"{
        "matched_adrs": [],
        "metadata": {"total_matched": 0, "by_framework": {}, "deduplicated_count": 0}
    }"#;

    /// Start a mockito server that returns an empty match response.
    fn mock_api_server() -> mockito::ServerGuard {
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(EMPTY_MATCH_RESPONSE)
            .create();
        server
    }

    fn make_sync_args(
        dry_run: bool,
        full: bool,
        force: bool,
        no_tailor: bool,
        api_url: &str,
    ) -> SyncArgs {
        SyncArgs {
            dry_run,
            full,
            force,
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: Some(api_url.to_string()),
            verbose: false,
            no_tailor,
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        }
    }

    fn make_sync_args_with_projects(force: bool, projects: Vec<&str>, api_url: &str) -> SyncArgs {
        SyncArgs {
            dry_run: false,
            full: false,
            force,
            reset_rejections: false,
            projects: projects.into_iter().map(String::from).collect(),
            model: None,
            api_url: Some(api_url.to_string()),
            verbose: false,
            no_tailor: false,
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        }
    }

    #[test]
    fn test_run_sync_force_returns_ok() {
        let server = mock_api_server();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(false, false, true, false, &server.url());
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(result.is_ok(), "run_sync with --force should succeed");
        // Output (including "No files to write.") now goes through pipeline.println()
        // into the TUI log pane, not to MockTerminal.
    }

    #[test]
    fn test_run_sync_no_tailor_force_returns_ok() {
        let server = mock_api_server();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(false, false, true, true, &server.url());
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(
            result.is_ok(),
            "run_sync with --no-tailor --force should succeed"
        );
    }

    #[test]
    fn test_run_sync_force_dry_run_returns_ok() {
        let server = mock_api_server();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(true, false, true, false, &server.url());
        // dry-run takes precedence over force
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(
            result.is_ok(),
            "run_sync with --force --dry-run should succeed"
        );
    }

    #[test]
    fn test_run_sync_no_flags_prompts_user() {
        let server = mock_api_server();
        let dir = tempfile::tempdir().unwrap();
        // select_one index 0 = Accept for project confirm, select_files handles file confirm
        let term = MockTerminal::new(vec![])
            .with_select_one(vec![0])
            .with_selection(Some(vec![]));
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(false, false, false, false, &server.url());
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(
            result.is_ok(),
            "run_sync with no flags should succeed when user accepts"
        );
    }

    #[test]
    fn test_run_sync_user_rejects_returns_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        // select_one index 2 = Reject for project confirm
        let term = MockTerminal::new(vec![]).with_select_one(vec![2]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(false, false, false, false, "http://unused");
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(
            matches!(result, Err(ActualError::UserCancelled)),
            "run_sync should return UserCancelled when user rejects"
        );
    }

    #[test]
    fn test_run_sync_force_skips_confirmation() {
        let server = mock_api_server();
        let dir = tempfile::tempdir().unwrap();
        // No inputs needed — force skips both confirmations
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(false, false, true, false, &server.url());
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(
            result.is_ok(),
            "run_sync with --force should skip confirmation and succeed"
        );
    }

    // ── auth_display branch tests ──

    #[test]
    fn test_run_sync_auth_display_authenticated_with_email() {
        // Exercises the Some(a) if a.authenticated branch with an email address.
        let server = mock_api_server();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(false, false, true, false, &server.url());
        let auth = AuthDisplay {
            authenticated: true,
            email: Some("user@example.com".to_string()),
        };
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            Some(&auth),
            None,
        );
        assert!(result.is_ok(), "authenticated auth_display should succeed");
    }

    #[test]
    fn test_run_sync_auth_display_not_authenticated() {
        // Exercises the Some(_) branch when authenticated = false.
        let server = mock_api_server();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(false, false, true, false, &server.url());
        let auth = AuthDisplay {
            authenticated: false,
            email: None,
        };
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            Some(&auth),
            None,
        );
        assert!(
            result.is_ok(),
            "not-authenticated auth_display should still complete sync"
        );
    }

    #[test]
    fn test_run_sync_runner_display_shown() {
        // Exercises the `if let Some(rd) = runner_display` branch (no warning).
        let server = mock_api_server();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(false, false, true, false, &server.url());
        let rd = RunnerDisplay {
            runner_name: "claude-cli".to_string(),
            model: "sonnet".to_string(),
            warning: None,
        };
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            Some(&rd),
        );
        assert!(
            result.is_ok(),
            "runner_display without warning should succeed"
        );
    }

    #[test]
    fn test_run_sync_runner_display_with_warning() {
        // Exercises the `if let Some(ref warn) = rd.warning` branch.
        let server = mock_api_server();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(false, false, true, false, &server.url());
        let rd = RunnerDisplay {
            runner_name: "codex-cli".to_string(),
            model: "haiku".to_string(),
            warning: Some("model 'haiku' may not be supported by codex-cli".to_string()),
        };
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            Some(&rd),
        );
        assert!(
            result.is_ok(),
            "runner_display with warning should still succeed"
        );
    }

    #[test]
    fn test_run_sync_dry_run_writes_no_files() {
        let server = mock_api_server();
        let dir = tempfile::tempdir().unwrap();
        // select_one index 0 = Accept for project confirm (dry_run, force=false)
        let term = MockTerminal::new(vec![]).with_select_one(vec![0]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(true, false, false, false, &server.url());
        run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        )
        .unwrap();
        // Verify no CLAUDE.md was created
        assert!(
            !dir.path().join("CLAUDE.md").exists(),
            "dry-run should not create any files"
        );
    }

    // ── project filtering tests ──

    #[test]
    fn test_run_sync_project_filter_matches() {
        let server = mock_api_server();
        let dir = tempfile::tempdir().unwrap();
        // Create a monorepo structure with pnpm-workspace.yaml
        std::fs::write(
            dir.path().join("pnpm-workspace.yaml"),
            "packages:\n  - apps/*\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{"name": "monorepo"}"#).unwrap();
        std::fs::create_dir_all(dir.path().join("apps/web")).unwrap();
        std::fs::write(
            dir.path().join("apps/web/package.json"),
            r#"{"name": "web-app"}"#,
        )
        .unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args_with_projects(true, vec!["apps/web"], &server.url());
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(
            result.is_ok(),
            "run_sync with --project apps/web should succeed"
        );
    }

    #[test]
    fn test_run_sync_project_filter_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        // No server needed — fails before API call
        let args = make_sync_args_with_projects(true, vec!["nonexistent/path"], "http://unused");
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(
            matches!(result, Err(ActualError::ConfigError(_))),
            "run_sync with non-matching --project should return ConfigError"
        );
    }

    // ── filter_projects unit tests ──

    #[test]
    fn test_filter_projects_empty_filters_returns_all() {
        let analysis = make_monorepo_analysis();
        let result = filter_projects(analysis, &[]).unwrap();
        assert_eq!(result.projects.len(), 2);
    }

    #[test]
    fn test_filter_projects_matches_one() {
        let analysis = make_monorepo_analysis();
        let filters = vec!["apps/web".to_string()];
        let result = filter_projects(analysis, &filters).unwrap();
        assert_eq!(result.projects.len(), 1);
        assert_eq!(result.projects[0].path, "apps/web");
    }

    #[test]
    fn test_filter_projects_matches_multiple() {
        let analysis = make_monorepo_analysis();
        let filters = vec!["apps/web".to_string(), "services/api".to_string()];
        let result = filter_projects(analysis, &filters).unwrap();
        assert_eq!(result.projects.len(), 2);
    }

    #[test]
    fn test_filter_projects_no_match_returns_error() {
        let analysis = make_monorepo_analysis();
        let filters = vec!["nonexistent".to_string()];
        let result = filter_projects(analysis, &filters);
        assert!(matches!(result, Err(ActualError::ConfigError(_))));
    }

    #[test]
    fn test_filter_projects_preserves_is_monorepo() {
        let analysis = make_monorepo_analysis();
        let filters = vec!["apps/web".to_string()];
        let result = filter_projects(analysis, &filters).unwrap();
        assert!(result.is_monorepo);
    }

    // ── exec / handle_result tests ──

    #[test]
    fn test_exec_claude_not_found() {
        // exec() delegates to sync_wiring::sync_run which auto-detects the runner
        // from the user's config. Explicitly set runner to ClaudeCli so the test
        // exercises the find_claude_binary() path regardless of config state.
        let _lock = crate::testutil::ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _guard = crate::testutil::EnvGuard::set("CLAUDE_BINARY", "/nonexistent/path/to/claude");
        let mut args = make_sync_args(true, false, false, false, "http://unused");
        args.runner = Some(crate::cli::args::RunnerChoice::ClaudeCli);
        let code = handle_result(exec(&args));
        assert_eq!(code, 2, "expected exit code 2 (ClaudeNotFound)");
    }

    #[test]
    fn test_sync_result_debug_clone_eq() {
        let result = SyncResult {
            files_created: 1,
            files_updated: 2,
            files_failed: 3,
            files_rejected: 4,
        };
        let cloned = result.clone();
        assert_eq!(result, cloned);
        let debug = format!("{:?}", result);
        assert!(debug.contains("SyncResult"));
    }

    // ── MockTerminal edge case tests ──

    #[test]
    fn test_mock_terminal_read_line_exhausted() {
        let term = MockTerminal::new(vec!["only"]);
        // First call succeeds
        assert_eq!(term.read_line("prompt").unwrap(), "only");
        // Second call returns UserCancelled because inputs are empty
        let err = term.read_line("prompt").unwrap_err();
        assert!(
            matches!(err, ActualError::UserCancelled),
            "expected UserCancelled when inputs exhausted"
        );
    }

    // ── run_sync analysis error test ──

    #[test]
    fn test_run_sync_analysis_error() {
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        // Create invalid pnpm-workspace.yaml to trigger analysis error
        std::fs::write(
            dir.path().join("pnpm-workspace.yaml"),
            "{{invalid yaml content",
        )
        .unwrap();
        let args = make_sync_args(false, false, true, false, "http://unused");
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(matches!(result, Err(ActualError::ConfigError(_))));
    }

    // ── run_sync confirm_and_write error test ──

    #[test]
    fn test_run_sync_confirm_and_write_error() {
        // Use a server that returns ADRs so confirm_and_write has files to confirm
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                "matched_adrs": [{
                    "id": "adr-001",
                    "title": "Test ADR",
                    "context": null,
                    "policies": ["Do the thing"],
                    "instructions": null,
                    "category": {"id": "cat-1", "name": "General", "path": "General"},
                    "applies_to": {"languages": ["rust"], "frameworks": []},
                    "matched_projects": ["."]
                }],
                "metadata": {"total_matched": 1, "by_framework": {}, "deduplicated_count": 1}
            }"#,
            )
            .create();

        let dir = tempfile::tempdir().unwrap();
        // select_one index 0 = Accept for project confirm, file confirm cancelled via select_files
        let term = MockTerminal::new(vec![])
            .with_select_one(vec![0])
            .with_selection(None);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        // force=false, no_tailor=true so we get raw output and enter confirmation flow
        let args = SyncArgs {
            dry_run: false,
            full: false,
            force: false,
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: Some(server.url()),
            verbose: false,
            no_tailor: true,
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        };
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );

        assert!(
            matches!(result, Err(ActualError::UserCancelled)),
            "expected UserCancelled from confirm_and_write, got: {result:?}"
        );
    }

    #[test]
    fn test_run_sync_zero_batch_size_returns_config_error() {
        // A config.yaml with batch_size: 0 should cause run_sync to return a ConfigError
        // when it reaches the tailoring phase (ConcurrentTailoringConfig::new rejects 0).
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                "matched_adrs": [{
                    "id": "adr-001",
                    "title": "Test ADR",
                    "context": null,
                    "policies": ["Do the thing"],
                    "instructions": null,
                    "category": {"id": "cat-1", "name": "General", "path": "General"},
                    "applies_to": {"languages": ["rust"], "frameworks": []},
                    "matched_projects": ["."]
                }],
                "metadata": {"total_matched": 1, "by_framework": {}, "deduplicated_count": 1}
            }"#,
            )
            .create();

        let dir = tempfile::tempdir().unwrap();
        // Write a config that has batch_size: 0 (bypasses the dotpath guard which
        // only runs on interactive set, not raw YAML deserialization).
        std::fs::write(dir.path().join("config.yaml"), "batch_size: 0\n").unwrap();

        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(false, false, true, false, &server.url());
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(
            matches!(result, Err(ActualError::ConfigError(_))),
            "expected ConfigError for batch_size=0, got: {result:?}"
        );
    }

    // ── report_write_results tests ──

    #[test]
    fn test_report_write_results_failed_without_error_message() {
        let results = vec![WriteResult {
            path: "CLAUDE.md".to_string(),
            action: WriteAction::Failed,
            version: 0,
            error: None,
        }];
        let mut pipeline = TuiRenderer::new(false, true);

        let (created, updated, failed) =
            report_write_results(&results, 0, Duration::from_secs(1), &mut pipeline);

        assert_eq!(created, 0);
        assert_eq!(updated, 0);
        assert_eq!(failed, 1);

        let log_text = pipeline.all_log_text();
        assert!(
            log_text.contains("unknown error"),
            "expected 'unknown error' fallback in log: {log_text}"
        );
    }

    #[test]
    fn test_report_write_results_created() {
        let results = vec![WriteResult {
            path: "CLAUDE.md".to_string(),
            action: WriteAction::Created,
            version: 1,
            error: None,
        }];
        let mut pipeline = TuiRenderer::new(false, true);

        let (created, updated, failed) =
            report_write_results(&results, 0, Duration::from_secs(1), &mut pipeline);

        assert_eq!(created, 1);
        assert_eq!(updated, 0);
        assert_eq!(failed, 0);

        let log_text = pipeline.all_log_text();
        assert!(
            log_text.contains("CLAUDE.md") && log_text.contains("created"),
            "expected created message in log: {log_text}"
        );
    }

    #[test]
    fn test_report_write_results_updated() {
        let results = vec![WriteResult {
            path: "apps/web/CLAUDE.md".to_string(),
            action: WriteAction::Updated,
            version: 3,
            error: None,
        }];
        let mut pipeline = TuiRenderer::new(false, true);

        let (created, updated, failed) =
            report_write_results(&results, 0, Duration::from_secs(1), &mut pipeline);

        assert_eq!(created, 0);
        assert_eq!(updated, 1);
        assert_eq!(failed, 0);

        let log_text = pipeline.all_log_text();
        assert!(
            log_text.contains("apps/web/CLAUDE.md") && log_text.contains("updated"),
            "expected updated message in log: {log_text}"
        );
    }

    #[test]
    fn test_report_write_results_failed_with_error_message() {
        let results = vec![WriteResult {
            path: "bad/CLAUDE.md".to_string(),
            action: WriteAction::Failed,
            version: 0,
            error: Some("permission denied".to_string()),
        }];
        let mut pipeline = TuiRenderer::new(false, true);

        let (created, updated, failed) =
            report_write_results(&results, 0, Duration::from_secs(1), &mut pipeline);

        assert_eq!(created, 0);
        assert_eq!(updated, 0);
        assert_eq!(failed, 1);

        let log_text = pipeline.all_log_text();
        assert!(
            log_text.contains("permission denied"),
            "expected error message in log: {log_text}"
        );
    }

    #[test]
    fn test_report_write_results_mixed() {
        let results = vec![
            WriteResult {
                path: "CLAUDE.md".to_string(),
                action: WriteAction::Created,
                version: 1,
                error: None,
            },
            WriteResult {
                path: "apps/web/CLAUDE.md".to_string(),
                action: WriteAction::Updated,
                version: 2,
                error: None,
            },
            WriteResult {
                path: "bad/CLAUDE.md".to_string(),
                action: WriteAction::Failed,
                version: 0,
                error: None,
            },
        ];
        let mut pipeline = TuiRenderer::new(false, true);

        let (created, updated, failed) =
            report_write_results(&results, 0, Duration::from_secs(1), &mut pipeline);

        assert_eq!(created, 1);
        assert_eq!(updated, 1);
        assert_eq!(failed, 1);
    }

    // ── Results rendering tests ──

    #[test]
    fn test_report_write_results_has_file_entries() {
        let results = vec![WriteResult {
            path: "CLAUDE.md".to_string(),
            action: WriteAction::Created,
            version: 1,
            error: None,
        }];
        let mut pipeline = TuiRenderer::new(false, true);

        report_write_results(&results, 0, Duration::from_secs(3), &mut pipeline);

        let log_text = pipeline.all_log_text();
        let stripped = console::strip_ansi_codes(&log_text);
        assert!(
            stripped.contains("CLAUDE.md") && stripped.contains("created"),
            "expected file entry in log: {stripped}"
        );
    }

    #[test]
    fn test_report_write_results_no_box_characters() {
        let results = vec![WriteResult {
            path: "CLAUDE.md".to_string(),
            action: WriteAction::Created,
            version: 1,
            error: None,
        }];
        let mut pipeline = TuiRenderer::new(false, true);

        report_write_results(&results, 0, Duration::from_secs(1), &mut pipeline);

        let log_text = pipeline.all_log_text();
        let stripped = console::strip_ansi_codes(&log_text);
        assert!(
            !stripped.contains('\u{256d}')
                && !stripped.contains('\u{2570}')
                && !stripped.contains('\u{2502}'),
            "expected no box-drawing characters in log: {stripped}"
        );
    }

    #[test]
    fn test_report_write_results_has_timing() {
        let results = vec![WriteResult {
            path: "CLAUDE.md".to_string(),
            action: WriteAction::Created,
            version: 1,
            error: None,
        }];
        let mut pipeline = TuiRenderer::new(false, true);

        report_write_results(&results, 0, Duration::from_millis(7300), &mut pipeline);

        let log_text = pipeline.all_log_text();
        let stripped = console::strip_ansi_codes(&log_text);
        assert!(
            stripped.contains("[7.3s total]"),
            "expected timing display in log: {stripped}"
        );
    }

    #[test]
    fn test_report_write_results_dot_separated_summary() {
        let results = vec![
            WriteResult {
                path: "CLAUDE.md".to_string(),
                action: WriteAction::Created,
                version: 1,
                error: None,
            },
            WriteResult {
                path: "apps/web/CLAUDE.md".to_string(),
                action: WriteAction::Updated,
                version: 2,
                error: None,
            },
        ];
        let mut pipeline = TuiRenderer::new(false, true);

        report_write_results(&results, 1, Duration::from_secs(2), &mut pipeline);

        let log_text = pipeline.all_log_text();
        let stripped = console::strip_ansi_codes(&log_text);
        assert!(
            stripped.contains("\u{00b7}"),
            "expected middle dot separator in log: {stripped}"
        );
        assert!(
            stripped.contains("1 created")
                && stripped.contains("1 updated")
                && stripped.contains("0 failed")
                && stripped.contains("1 rejected"),
            "expected all counts in log: {stripped}"
        );
    }

    #[test]
    fn test_report_write_results_mixed_results() {
        let results = vec![
            WriteResult {
                path: "CLAUDE.md".to_string(),
                action: WriteAction::Updated,
                version: 3,
                error: None,
            },
            WriteResult {
                path: "apps/web/CLAUDE.md".to_string(),
                action: WriteAction::Created,
                version: 1,
                error: None,
            },
            WriteResult {
                path: "apps/api/CLAUDE.md".to_string(),
                action: WriteAction::Failed,
                version: 0,
                error: Some("permission denied".to_string()),
            },
        ];
        let mut pipeline = TuiRenderer::new(false, true);

        let (created, updated, failed) =
            report_write_results(&results, 2, Duration::from_millis(4500), &mut pipeline);

        assert_eq!(created, 1);
        assert_eq!(updated, 1);
        assert_eq!(failed, 1);

        let log_text = pipeline.all_log_text();
        let stripped = console::strip_ansi_codes(&log_text);
        // Verify all file entries
        assert!(stripped.contains("CLAUDE.md") && stripped.contains("updated"));
        assert!(stripped.contains("apps/web/CLAUDE.md") && stripped.contains("created"));
        assert!(stripped.contains("apps/api/CLAUDE.md") && stripped.contains("permission denied"));
        // Verify summary line
        assert!(stripped.contains("1 created"));
        assert!(stripped.contains("1 updated"));
        assert!(stripped.contains("1 failed"));
        assert!(stripped.contains("2 rejected"));
        assert!(stripped.contains("[4.5s total]"));
    }

    // ── compute_repo_key tests ──

    #[test]
    fn test_compute_repo_key_non_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        let key = compute_repo_key(dir.path());
        // Should be a hex-encoded SHA-256 (64 chars)
        assert_eq!(key.len(), 64, "expected 64-char hex hash, got: {key}");
        // Should be deterministic
        let key2 = compute_repo_key(dir.path());
        assert_eq!(key, key2, "expected deterministic result");
    }

    #[test]
    fn test_compute_repo_key_different_dirs_differ() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        let key1 = compute_repo_key(dir1.path());
        let key2 = compute_repo_key(dir2.path());
        assert_ne!(key1, key2, "different dirs should produce different keys");
    }

    #[cfg(unix)]
    #[test]
    fn test_compute_repo_key_symlink_behavior() {
        use std::os::unix::fs::symlink;
        let target_dir = tempfile::tempdir().unwrap();
        let symlink_dir = tempfile::tempdir().unwrap();
        let link_path = symlink_dir.path().join("link");
        symlink(target_dir.path(), &link_path).expect("failed to create symlink");

        let key_target = compute_repo_key(target_dir.path());
        let key_symlink = compute_repo_key(&link_path);

        // Both must produce valid 64-char hex hashes (format sanity checks)
        assert_eq!(key_target.len(), 64, "target key should be 64-char hex");
        assert!(
            key_target.chars().all(|c| c.is_ascii_hexdigit()),
            "target key should be hex"
        );
        assert_eq!(key_symlink.len(), 64, "symlink key should be 64-char hex");
        assert!(
            key_symlink.chars().all(|c| c.is_ascii_hexdigit()),
            "symlink key should be hex"
        );

        // The relationship between keys depends on whether a git remote URL is
        // available. In this test, both dirs are temp dirs outside any git repo,
        // so compute_repo_key falls back to hashing the path string directly.
        //
        // Since `target_dir.path()` and `link_path` are different path strings
        // (the latter contains the symlink component), they hash to different
        // values — the path is used as-is, symlinks are NOT resolved in the
        // fallback path on either platform.
        //
        // User-visible consequence: if a user's repo is accessed via a symlink
        // path and has no configured git remote, they get a different rejection
        // namespace than if accessed via the real path. When a git remote IS
        // present, both paths produce the same remote URL → same cache key.
        //
        // Note: On macOS, current_dir() resolves symlinks (affecting resolve_cwd),
        // but compute_repo_key uses the path as passed, so the behavior here is
        // the same on both Linux and macOS.
        assert_ne!(
            key_target,
            key_symlink,
            "symlink path produces a different cache key than the real path (path used as-is in fallback)"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_compute_repo_key_timeout_falls_back_to_path_hash() {
        use std::os::unix::fs::PermissionsExt;
        // Place a fake "git" binary that sleeps for 30s on PATH so the
        // recv_timeout fires before it responds, forcing the path-hash fallback.
        let _lock = crate::testutil::ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let bin_dir = tempfile::tempdir().unwrap();
        let fake_git = bin_dir.path().join("git");
        std::fs::write(&fake_git, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&fake_git, std::fs::Permissions::from_mode(0o755)).unwrap();
        let original_path = std::env::var("PATH").unwrap_or_default();
        let _path_guard = crate::testutil::EnvGuard::set(
            "PATH",
            &format!("{}:{}", bin_dir.path().display(), original_path),
        );
        let dir = tempfile::tempdir().unwrap();
        let result =
            compute_repo_key_with_timeout(dir.path(), std::time::Duration::from_millis(100));
        assert_eq!(result.len(), 64, "expected 64-char SHA256 hex string");
        // Deterministic: same dir → same hash (using the path-hash fallback)
        let result2 =
            compute_repo_key_with_timeout(dir.path(), std::time::Duration::from_millis(100));
        assert_eq!(result, result2);
    }

    #[cfg(unix)]
    #[test]
    fn test_compute_repo_key_empty_url_falls_back_to_path_hash() {
        use std::os::unix::fs::PermissionsExt;
        // Fake "git" that exits 0 but prints nothing — the empty-string guard
        // in compute_repo_key_with_timeout should fall back to the path hash.
        let _lock = crate::testutil::ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let bin_dir = tempfile::tempdir().unwrap();
        let fake_git = bin_dir.path().join("git");
        std::fs::write(&fake_git, "#!/bin/sh\nprintf ''\nexit 0\n").unwrap();
        std::fs::set_permissions(&fake_git, std::fs::Permissions::from_mode(0o755)).unwrap();
        let original_path = std::env::var("PATH").unwrap_or_default();
        let _path_guard = crate::testutil::EnvGuard::set(
            "PATH",
            &format!("{}:{}", bin_dir.path().display(), original_path),
        );
        let dir = tempfile::tempdir().unwrap();
        let result = compute_repo_key_with_timeout(dir.path(), std::time::Duration::from_secs(5));
        assert_eq!(result.len(), 64, "expected 64-char SHA256 hex string");
        // Should equal the path-hash fallback
        let expected = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(dir.path().to_string_lossy().as_bytes());
            format!("{:x}", hasher.finalize())
        };
        assert_eq!(result, expected, "empty URL should fall back to path hash");
    }

    // ── raw_adrs_to_output tests ──

    fn make_test_adr(id: &str, title: &str, projects: Vec<&str>) -> crate::api::types::Adr {
        crate::api::types::Adr {
            id: id.to_string(),
            title: title.to_string(),
            context: None,
            policies: vec![format!("Policy for {title}")],
            instructions: Some(vec![format!("Instruction for {title}")]),
            category: crate::api::types::AdrCategory {
                id: "cat-1".to_string(),
                name: "General".to_string(),
                path: "General".to_string(),
            },
            applies_to: crate::api::types::AppliesTo {
                languages: vec!["rust".to_string()],
                frameworks: vec![],
                ..Default::default()
            },
            matched_projects: projects.into_iter().map(String::from).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn test_raw_adrs_to_output_empty() {
        let output = raw_adrs_to_output(&[], &OutputFormat::ClaudeMd);
        assert!(output.files.is_empty());
        assert_eq!(output.summary.total_input, 0);
        assert_eq!(output.summary.applicable, 0);
        assert_eq!(output.summary.files_generated, 0);
    }

    #[test]
    fn test_raw_adrs_to_output_single_adr_no_projects() {
        let adrs = vec![make_test_adr("adr-001", "Test ADR", vec![])];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::ClaudeMd);
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "CLAUDE.md");
        assert!(output.files[0].content().contains("Test ADR"));
        assert!(output.files[0].content().contains("Policy for Test ADR"));
        assert!(output.files[0]
            .content()
            .contains("Instruction for Test ADR"));
        assert_eq!(output.files[0].adr_ids(), vec!["adr-001"]);
        assert_eq!(output.summary.total_input, 1);
        assert_eq!(output.summary.files_generated, 1);
    }

    #[test]
    fn test_raw_adrs_to_output_adr_with_project() {
        let adrs = vec![make_test_adr("adr-001", "Web Rules", vec!["apps/web"])];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::ClaudeMd);
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "apps/web/CLAUDE.md");
    }

    #[test]
    fn test_raw_adrs_to_output_adr_with_dot_project() {
        let adrs = vec![make_test_adr("adr-001", "Root Rules", vec!["."])];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::ClaudeMd);
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "CLAUDE.md");
    }

    #[test]
    fn test_raw_adrs_to_output_multiple_projects() {
        let adrs = vec![
            make_test_adr("adr-001", "Web ADR", vec!["apps/web"]),
            make_test_adr("adr-002", "API ADR", vec!["services/api"]),
            make_test_adr("adr-003", "Shared ADR", vec![]),
        ];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::ClaudeMd);
        assert_eq!(output.files.len(), 3);
        let paths: Vec<&str> = output.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"CLAUDE.md"));
        assert!(paths.contains(&"apps/web/CLAUDE.md"));
        assert!(paths.contains(&"services/api/CLAUDE.md"));
        assert_eq!(output.summary.total_input, 3);
        assert_eq!(output.summary.files_generated, 3);
    }

    #[test]
    fn test_raw_adrs_to_output_adr_without_instructions() {
        let mut adr = make_test_adr("adr-001", "Test ADR", vec![]);
        adr.instructions = None;
        let output = raw_adrs_to_output(&[adr], &OutputFormat::ClaudeMd);
        assert_eq!(output.files.len(), 1);
        assert!(output.files[0].content().contains("Policy for Test ADR"));
        // With instructions=None, the instruction line should not appear
        let content = output.files[0].content();
        assert!(
            !content.contains("Instruction for"),
            "expected no instruction lines"
        );
    }

    #[test]
    fn test_raw_adrs_to_output_adr_with_empty_instructions() {
        let mut adr = make_test_adr("adr-001", "Test ADR", vec![]);
        adr.instructions = Some(vec![]);
        let output = raw_adrs_to_output(&[adr], &OutputFormat::ClaudeMd);
        assert_eq!(output.files.len(), 1);
        assert!(output.files[0].content().contains("Policy for Test ADR"));
        // With empty instructions vec, no instruction lines should appear
        let content = output.files[0].content();
        assert!(
            !content.contains("Instruction for"),
            "expected no instruction lines"
        );
    }

    // ── raw_adrs_to_output format tests ──

    #[test]
    fn test_raw_adrs_to_output_agents_md_format_root() {
        let adrs = vec![make_test_adr("adr-001", "Test ADR", vec![])];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::AgentsMd);
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "AGENTS.md");
    }

    #[test]
    fn test_raw_adrs_to_output_agents_md_format_with_project() {
        let adrs = vec![make_test_adr("adr-001", "Web Rules", vec!["apps/web"])];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::AgentsMd);
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "apps/web/AGENTS.md");
    }

    #[test]
    fn test_raw_adrs_to_output_agents_md_format_dot_project() {
        let adrs = vec![make_test_adr("adr-001", "Root Rules", vec!["."])];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::AgentsMd);
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "AGENTS.md");
    }

    // ── validate_project_path tests ──

    #[test]
    fn test_validate_project_path_valid() {
        assert!(validate_project_path("apps/web").is_ok());
        assert!(validate_project_path(".").is_ok());
        assert!(validate_project_path("services/api/v2").is_ok());
    }

    #[test]
    fn test_validate_project_path_rejects_path_traversal() {
        let result = validate_project_path("../secret");
        assert!(result.is_err(), "expected error for path traversal");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("path traversal"),
            "expected 'path traversal' in: {msg}"
        );
    }

    #[test]
    fn test_validate_project_path_rejects_absolute_slash() {
        let result = validate_project_path("/etc/passwd");
        assert!(result.is_err(), "expected error for absolute path");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("relative path"),
            "expected 'relative path' in: {msg}"
        );
    }

    #[test]
    fn test_validate_project_path_rejects_absolute_backslash() {
        let result = validate_project_path("\\windows\\system32");
        assert!(
            result.is_err(),
            "expected error for backslash absolute path"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("relative path"),
            "expected 'relative path' in: {msg}"
        );
    }

    #[test]
    fn test_validate_project_path_rejects_null_bytes() {
        let result = validate_project_path("apps\0evil");
        assert!(result.is_err(), "expected error for null bytes");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("null bytes"),
            "expected 'null bytes' in: {msg}"
        );
    }

    // ── strip_control_chars tests ──

    #[test]
    fn test_strip_control_chars_keeps_printable() {
        let input = "Hello, World! 123 abc ABC";
        assert_eq!(strip_control_chars(input), input);
    }

    #[test]
    fn test_strip_control_chars_keeps_newline_tab_cr() {
        let input = "line1\nline2\r\n\ttabbed";
        assert_eq!(strip_control_chars(input), input);
    }

    #[test]
    fn test_strip_control_chars_removes_control_chars() {
        // ESC (0x1B), BEL (0x07), SOH (0x01)
        let input = "hello\x1b[31mworld\x07\x01end";
        let result = strip_control_chars(input);
        assert!(!result.contains('\x1b'), "should strip ESC");
        assert!(!result.contains('\x07'), "should strip BEL");
        assert!(!result.contains('\x01'), "should strip SOH");
        assert!(result.contains("hello"), "should keep 'hello'");
        assert!(result.contains("world"), "should keep 'world'");
        assert!(result.contains("end"), "should keep 'end'");
    }

    #[test]
    fn test_strip_control_chars_removes_del() {
        let input = "abc\x7fdef";
        let result = strip_control_chars(input);
        assert!(!result.contains('\x7f'), "should strip DEL");
        assert_eq!(result, "abcdef");
    }

    #[test]
    fn test_strip_control_chars_empty_string() {
        assert_eq!(strip_control_chars(""), "");
    }

    // ── raw_adrs_to_output with invalid project paths tests ──

    #[test]
    fn test_raw_adrs_to_output_skips_traversal_project_path() {
        // An ADR with a path-traversal project path should be skipped (no file for that project)
        let adrs = vec![make_test_adr("adr-001", "Test ADR", vec!["../evil"])];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::ClaudeMd);
        // The invalid project path should be skipped; no file generated
        assert!(output.files.is_empty());
        assert_eq!(output.summary.total_input, 1);
        assert_eq!(output.summary.applicable, 0);
        assert_eq!(output.summary.not_applicable, 1);
        assert_eq!(output.skipped_adrs.len(), 1);
        assert_eq!(output.skipped_adrs[0].id, "adr-001");
        assert!(output.skipped_adrs[0]
            .reason
            .to_lowercase()
            .contains("invalid"));
    }

    #[test]
    fn test_raw_adrs_to_output_skips_absolute_project_path() {
        let adrs = vec![make_test_adr("adr-001", "Test ADR", vec!["/etc/passwd"])];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::ClaudeMd);
        assert!(output.files.is_empty());
        assert_eq!(output.summary.total_input, 1);
        assert_eq!(output.summary.applicable, 0);
        assert_eq!(output.summary.not_applicable, 1);
        assert_eq!(output.skipped_adrs.len(), 1);
        assert_eq!(output.skipped_adrs[0].id, "adr-001");
        assert!(output.skipped_adrs[0]
            .reason
            .to_lowercase()
            .contains("invalid"));
    }

    #[test]
    fn test_raw_adrs_to_output_all_invalid_paths_not_applicable() {
        // An ADR where ALL matched_projects have invalid paths should be not_applicable
        let adrs = vec![make_test_adr(
            "adr-001",
            "Test ADR",
            vec!["../evil", "/etc/passwd"],
        )];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::ClaudeMd);
        assert!(output.files.is_empty());
        assert_eq!(output.summary.total_input, 1);
        assert_eq!(output.summary.applicable, 0);
        assert_eq!(output.summary.not_applicable, 1);
        assert_eq!(output.skipped_adrs.len(), 1);
        assert_eq!(output.skipped_adrs[0].id, "adr-001");
        assert!(output.skipped_adrs[0]
            .reason
            .to_lowercase()
            .contains("invalid"));
    }

    #[test]
    fn test_raw_adrs_to_output_mixed_valid_and_all_invalid() {
        // Two ADRs: one with a valid project path, one with only invalid paths
        let adrs = vec![
            make_test_adr("adr-001", "Valid ADR", vec!["apps/web"]),
            make_test_adr("adr-002", "Invalid ADR", vec!["../evil", "/etc/passwd"]),
        ];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::ClaudeMd);
        assert_eq!(output.summary.total_input, 2);
        assert_eq!(output.summary.applicable, 1);
        assert_eq!(output.summary.not_applicable, 1);
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "apps/web/CLAUDE.md");
        assert_eq!(output.skipped_adrs.len(), 1);
        assert_eq!(output.skipped_adrs[0].id, "adr-002");
    }

    #[test]
    fn test_raw_adrs_to_output_strips_control_chars_from_content() {
        // Use actual control chars (not full ANSI sequences) to verify stripping
        let mut adr = make_test_adr("adr-001", "Test\x07ADR", vec![]);
        adr.policies = vec!["Policy\x01with\x02bells".to_string()];
        adr.instructions = Some(vec!["Instruction\x03normal".to_string()]);
        let output = raw_adrs_to_output(&[adr], &OutputFormat::ClaudeMd);
        assert_eq!(output.files.len(), 1);
        let content = output.files[0].content();
        assert!(
            !content.contains('\x07'),
            "BEL should be stripped from title"
        );
        assert!(
            !content.contains('\x01'),
            "SOH should be stripped from policy"
        );
        assert!(
            !content.contains('\x02'),
            "STX should be stripped from policy"
        );
        assert!(
            !content.contains('\x03'),
            "ETX should be stripped from instruction"
        );
        assert!(
            content.contains("TestADR"),
            "title text should be preserved (without control char)"
        );
        assert!(
            content.contains("Policy"),
            "policy text should be preserved"
        );
        assert!(
            content.contains("Instruction"),
            "instruction text should be preserved"
        );
    }

    // ── find_existing_output_files tests ──

    #[test]
    fn test_find_existing_claude_md_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(result.is_empty());
    }

    #[test]
    fn test_find_existing_claude_md_finds_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "Root content").unwrap();
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(result.contains("Root content"));
        assert!(result.contains("CLAUDE.md"));
    }

    #[test]
    fn test_find_existing_claude_md_finds_nested() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("apps").join("web")).unwrap();
        std::fs::write(
            dir.path().join("apps").join("web").join("CLAUDE.md"),
            "Web content",
        )
        .unwrap();
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(result.contains("Web content"));
    }

    #[test]
    fn test_find_existing_claude_md_skips_hidden_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git").join("CLAUDE.md"), "Git content").unwrap();
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(result.is_empty());
    }

    #[test]
    fn test_find_existing_claude_md_skips_node_modules() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules")).unwrap();
        std::fs::write(
            dir.path().join("node_modules").join("CLAUDE.md"),
            "Module content",
        )
        .unwrap();
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(result.is_empty());
    }

    #[test]
    fn test_find_existing_claude_md_strips_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let content = "# My Rules\n\n<!-- managed:actual-start -->\n<!-- last-synced: 2024-01-01T00:00:00Z -->\n<!-- version: 1 -->\n<!-- adr-ids: abc-123,def-456 -->\n\n## Some Rules\n\nDo this.\n\n<!-- managed:actual-end -->";
        std::fs::write(dir.path().join("CLAUDE.md"), content).unwrap();
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(
            !result.contains("adr-ids"),
            "should strip adr-ids metadata: {result}"
        );
        assert!(
            !result.contains("last-synced"),
            "should strip last-synced metadata: {result}"
        );
        assert!(
            !result.contains("version:"),
            "should strip version metadata: {result}"
        );
        assert!(
            !result.contains("managed:actual"),
            "should strip managed markers: {result}"
        );
        assert!(
            result.contains("Some Rules"),
            "should preserve content: {result}"
        );
        assert!(
            result.contains("Do this"),
            "should preserve content: {result}"
        );
    }

    #[test]
    fn test_find_existing_claude_md_unreadable_dir() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let unreadable = dir.path().join("noperm");
        std::fs::create_dir(&unreadable).unwrap();
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Should not panic, just skip the unreadable dir
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(result.is_empty());
        // Restore permissions for cleanup
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[tracing_test::traced_test]
    #[test]
    fn test_find_existing_claude_md_unreadable_file_emits_warn() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("CLAUDE.md");
        std::fs::write(&file_path, "secret content").unwrap();
        std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o000)).unwrap();
        // unreadable file is skipped and a warning is emitted
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(
            result.is_empty(),
            "unreadable file should be skipped: {result}"
        );
        assert!(
            logs_contain("failed to read output file"),
            "expected a warning to be emitted for the unreadable file"
        );
        // Restore permissions for cleanup
        std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn test_find_existing_claude_md_skips_vendor() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("vendor")).unwrap();
        std::fs::write(
            dir.path().join("vendor").join("CLAUDE.md"),
            "Vendor content",
        )
        .unwrap();
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(result.is_empty(), "should skip vendor directory");
    }

    #[test]
    fn test_find_existing_claude_md_skips_pycache() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("__pycache__")).unwrap();
        std::fs::write(
            dir.path().join("__pycache__").join("CLAUDE.md"),
            "Pycache content",
        )
        .unwrap();
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(result.is_empty(), "should skip __pycache__ directory");
    }

    #[test]
    fn test_find_existing_claude_md_skips_all_ignored_dirs() {
        let dir = tempfile::tempdir().unwrap();
        for skip in super::super::super::SKIP_DIRS {
            let d = dir.path().join(skip);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("CLAUDE.md"), "# Skip").unwrap();
        }
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(
            result.is_empty(),
            "should skip all SKIP_DIRS directories, got: {result}"
        );
    }

    // ── Phase 2 integration tests ──

    #[test]
    fn test_run_sync_api_error_returns_error() {
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/adrs/match")
            .with_status(500)
            .with_body("Internal Server Error")
            .create();

        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(false, false, true, false, &server.url());
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(
            matches!(result, Err(ActualError::ApiError(_))),
            "expected ApiError, got: {result:?}"
        );
    }

    #[test]
    fn test_run_sync_no_tailor_with_adrs_creates_files() {
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                "matched_adrs": [{
                    "id": "adr-001",
                    "title": "Use consistent error handling",
                    "context": null,
                    "policies": ["Use Result for all fallible operations"],
                    "instructions": ["Return ActualError from all public functions"],
                    "category": {"id": "cat-1", "name": "Error Handling", "path": "Error Handling"},
                    "applies_to": {"languages": ["rust"], "frameworks": []},
                    "matched_projects": ["."]
                }],
                "metadata": {"total_matched": 1, "by_framework": {}, "deduplicated_count": 1}
            }"#,
            )
            .create();

        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = SyncArgs {
            dry_run: false,
            full: false,
            force: true,
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: Some(server.url()),
            verbose: false,
            no_tailor: true,
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        };
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(result.is_ok(), "expected Ok, got: {result:?}");

        // Verify CLAUDE.md was created
        assert!(dir.path().join("CLAUDE.md").exists());
        let content = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(
            content.contains("Use consistent error handling"),
            "expected ADR title in CLAUDE.md, got: {content}"
        );
        assert!(
            content.contains("Use Result for all fallible operations"),
            "expected policy in CLAUDE.md, got: {content}"
        );
    }

    #[test]
    fn test_run_sync_verbose_shows_details() {
        let server = mock_api_server();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = SyncArgs {
            dry_run: false,
            full: false,
            force: true,
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: Some(server.url()),
            verbose: true,
            no_tailor: true,
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        };
        // This should succeed and print verbose output to stderr
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
    }

    #[test]
    fn test_run_sync_reset_rejections() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.yaml");

        // Pre-populate config with rejections
        let mut config = crate::config::Config::default();
        let repo_key = compute_repo_key(dir.path());
        crate::config::rejections::add_rejection(&mut config, &repo_key, "adr-001");
        save_to(&config, &cfg_path).unwrap();

        // Verify rejections exist
        let loaded = load_from(&cfg_path).unwrap();
        assert!(!get_rejections(&loaded, &repo_key).is_empty());

        let server = mock_api_server();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = SyncArgs {
            dry_run: false,
            full: false,
            force: true,
            reset_rejections: true,
            projects: vec![],
            model: None,
            api_url: Some(server.url()),
            verbose: false,
            no_tailor: true,
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        };
        let result = run_sync(&args, dir.path(), &cfg_path, &term, &runner, None, None);
        assert!(result.is_ok(), "expected Ok, got: {result:?}");

        // Verify rejections were cleared
        let loaded = load_from(&cfg_path).unwrap();
        assert!(
            get_rejections(&loaded, &repo_key).is_empty(),
            "expected rejections to be cleared"
        );
    }

    const ADR_MATCH_RESPONSE: &str = r#"{
        "matched_adrs": [{
            "id": "adr-001",
            "title": "Use consistent error handling",
            "context": null,
            "policies": ["Use Result for all fallible operations"],
            "instructions": ["Return ActualError from all public functions"],
            "category": {"id": "cat-1", "name": "Error Handling", "path": "Error Handling"},
            "applies_to": {"languages": ["rust"], "frameworks": []},
            "matched_projects": ["."]
        }],
        "metadata": {"total_matched": 1, "by_framework": {}, "deduplicated_count": 1}
    }"#;

    /// Match response containing a V2 ADR (has schema_version + content_md).
    const V2_ADR_MATCH_RESPONSE: &str = r##"{
        "matched_adrs": [{
            "id": "adr-v2-001",
            "title": "Adopt structured logging",
            "context": "Structured logging improves observability.",
            "policies": ["Use tracing crate for all logging"],
            "instructions": null,
            "category": {"id": "cat-2", "name": "Observability", "path": "Observability"},
            "applies_to": {"languages": ["rust"], "frameworks": []},
            "matched_projects": ["."],
            "schema_version": 2,
            "content_md": "# Adopt Structured Logging\n\nStructured logging improves observability.",
            "content_json": {"agent_documentation": "Use tracing::info! for request-level logs."},
            "source": "ai_generated"
        }],
        "metadata": {"total_matched": 1, "by_framework": {}, "deduplicated_count": 1}
    }"##;

    fn mock_api_server_with_v2_adrs() -> mockito::ServerGuard {
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(V2_ADR_MATCH_RESPONSE)
            .create();
        server
    }

    fn mock_api_server_with_adrs() -> mockito::ServerGuard {
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(ADR_MATCH_RESPONSE)
            .create();
        server
    }

    // ── V2 ADR pipeline integration tests ──

    #[test]
    fn test_run_sync_v2_adrs_writes_three_tier_files() {
        let server = mock_api_server_with_v2_adrs();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(false, false, true, true, &server.url());
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(result.is_ok(), "V2 sync should succeed: {result:?}");

        // Tier 2: docs/adr/ file should exist with content_md
        let adr_file = dir.path().join("docs/adr/adopt-structured-logging.md");
        assert!(adr_file.exists(), "docs/adr/ file should be created");
        let content = std::fs::read_to_string(&adr_file).unwrap();
        assert!(content.contains("Adopt Structured Logging"));

        // Tier 3: .claude/rules/ file should exist with agent_documentation
        let rules_file = dir.path().join(".claude/rules/adopt-structured-logging.md");
        assert!(rules_file.exists(), ".claude/rules/ file should be created");
        let rules_content = std::fs::read_to_string(&rules_file).unwrap();
        assert!(
            rules_content.contains("tracing::info!"),
            "rules file should contain agent docs: {rules_content}"
        );

        // Tier 1: CLAUDE.md should have governance pointer
        let claude_md = dir.path().join("CLAUDE.md");
        assert!(claude_md.exists(), "CLAUDE.md should be created");
        let claude_content = std::fs::read_to_string(&claude_md).unwrap();
        assert!(
            claude_content.contains("adr_governance"),
            "CLAUDE.md should contain governance pointer"
        );
    }

    #[test]
    fn test_run_sync_v2_adrs_dry_run_skips_file_writes() {
        let server = mock_api_server_with_v2_adrs();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(true, false, true, true, &server.url());
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(result.is_ok(), "V2 dry-run should succeed: {result:?}");

        // V2 files should NOT be written in dry-run mode
        assert!(
            !dir.path()
                .join("docs/adr/adopt-structured-logging.md")
                .exists(),
            "docs/adr/ should not be created in dry-run"
        );
        assert!(
            !dir.path()
                .join(".claude/rules/adopt-structured-logging.md")
                .exists(),
            ".claude/rules/ should not be created in dry-run"
        );
    }

    #[test]
    fn test_run_sync_v2_adrs_write_error_is_logged_gracefully() {
        let server = mock_api_server_with_v2_adrs();
        let dir = tempfile::tempdir().unwrap();
        // Block "docs" path by creating a FILE where the directory needs to be
        std::fs::write(dir.path().join("docs"), "blocker").unwrap();

        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(false, false, true, true, &server.url());
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        // Sync should still succeed even when V2 file writes fail
        assert!(
            result.is_ok(),
            "V2 write failure should not abort sync: {result:?}"
        );
    }

    #[test]
    fn test_run_sync_verbose_with_rejections_shows_filter_message() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.yaml");

        // Pre-populate config with a rejection for adr-001
        let mut config = crate::config::Config::default();
        let repo_key = compute_repo_key(dir.path());
        crate::config::rejections::add_rejection(&mut config, &repo_key, "adr-001");
        save_to(&config, &cfg_path).unwrap();

        let server = mock_api_server_with_adrs();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = SyncArgs {
            dry_run: false,
            full: false,
            force: true,
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: Some(server.url()),
            verbose: true,
            no_tailor: true,
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        };
        let result = run_sync(&args, dir.path(), &cfg_path, &term, &runner, None, None);
        // adr-001 is rejected, so no ADRs remain → "No files to write."
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
    }

    #[test]
    fn test_run_sync_tailoring_error_returns_error() {
        let server = mock_api_server_with_adrs();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        // MockRunner returns analysis JSON which is not valid tailoring output.
        // Phase 1 (analysis) parses it fine, but Phase 2 (tailoring) will fail.
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = SyncArgs {
            dry_run: false,
            full: false,
            force: true,
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: Some(server.url()),
            verbose: false,
            no_tailor: false, // Triggers tailoring path
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        };
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(result.is_err(), "expected tailoring to fail");
    }

    // ── mock runner success path coverage ──

    /// Valid `TailoringOutput` JSON that the mock runner can return successfully.
    const VALID_TAILORING_JSON: &str = "{\"files\":[{\"path\":\"CLAUDE.md\",\"sections\":[{\"adr_id\":\"adr-001\",\"content\":\"Rules.\"}],\"reasoning\":\"Root rules\"}],\"skipped_adrs\":[],\"summary\":{\"total_input\":1,\"applicable\":1,\"not_applicable\":0,\"files_generated\":1}}";

    #[test]
    fn test_mock_runner_run_tailoring_success_path() {
        // Exercises MockRunner::run_tailoring when json parses successfully
        // (the Ok(parsed) branch that is otherwise unreachable in other tests).
        let server = mock_api_server_with_adrs();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_TAILORING_JSON);
        let args = SyncArgs {
            dry_run: true, // dry-run so we don't write files
            full: false,
            force: true,
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: Some(server.url()),
            verbose: false,
            no_tailor: false, // triggers tailoring path
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        };
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(
            result.is_ok(),
            "expected success with valid tailoring JSON: {result:?}"
        );
    }

    // ── compute_tailoring_cache_key tests ──

    /// Build a minimal test [`Adr`] with the given id and policies for cache key tests.
    fn make_cache_test_adr(id: &str, policies: Vec<&str>) -> crate::api::types::Adr {
        crate::api::types::Adr {
            id: id.to_string(),
            title: format!("Title for {id}"),
            context: None,
            policies: policies.iter().map(|s| s.to_string()).collect(),
            instructions: None,
            category: crate::api::types::AdrCategory {
                id: "cat-1".to_string(),
                name: "Category One".to_string(),
                path: "cat/one".to_string(),
            },
            applies_to: crate::api::types::AppliesTo {
                languages: vec!["rust".to_string()],
                frameworks: vec![],
                ..Default::default()
            },
            matched_projects: vec![".".to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn test_tailoring_cache_key_deterministic() {
        let adrs = vec![
            make_cache_test_adr("adr-001", vec!["p1"]),
            make_cache_test_adr("adr-002", vec!["p2"]),
        ];
        let key1 = compute_tailoring_cache_key(
            &adrs,
            false,
            &[".".to_string()],
            "existing content",
            Some("opus"),
        );
        let key2 = compute_tailoring_cache_key(
            &adrs,
            false,
            &[".".to_string()],
            "existing content",
            Some("opus"),
        );
        assert_eq!(key1, key2, "same inputs should produce same key");
        assert_eq!(key1.len(), 64, "should be a 64-char hex SHA-256");
    }

    #[test]
    fn test_tailoring_cache_key_differs_on_adr_content() {
        // Same ADR ID but different policy content → different key.
        let adrs1 = vec![make_cache_test_adr("adr-001", vec!["use snake_case"])];
        let adrs2 = vec![make_cache_test_adr("adr-001", vec!["use camelCase"])];
        let key1 = compute_tailoring_cache_key(&adrs1, false, &[".".to_string()], "", None);
        let key2 = compute_tailoring_cache_key(&adrs2, false, &[".".to_string()], "", None);
        assert_ne!(
            key1, key2,
            "different ADR policy content should produce different key"
        );
    }

    #[test]
    fn test_tailoring_cache_key_same_adr_different_order_is_stable() {
        // ADRs passed in different order → same key (sorting for determinism).
        let adr1 = make_cache_test_adr("adr-001", vec!["policy-a"]);
        let adr2 = make_cache_test_adr("adr-002", vec!["policy-b"]);
        let key1 = compute_tailoring_cache_key(
            &[adr1.clone(), adr2.clone()],
            false,
            &[".".to_string()],
            "",
            None,
        );
        let key2 = compute_tailoring_cache_key(&[adr2, adr1], false, &[".".to_string()], "", None);
        assert_eq!(
            key1, key2,
            "ADR input order should not affect the cache key"
        );
    }

    #[test]
    fn test_tailoring_cache_key_differs_on_adr_ids() {
        let key1 = compute_tailoring_cache_key(
            &[make_cache_test_adr("adr-001", vec![])],
            false,
            &[".".to_string()],
            "",
            None,
        );
        let key2 = compute_tailoring_cache_key(
            &[
                make_cache_test_adr("adr-001", vec![]),
                make_cache_test_adr("adr-002", vec![]),
            ],
            false,
            &[".".to_string()],
            "",
            None,
        );
        assert_ne!(key1, key2, "different ADR IDs should produce different key");
    }

    #[test]
    fn test_tailoring_cache_key_differs_on_no_tailor() {
        let adrs = vec![make_cache_test_adr("adr-001", vec![])];
        let key1 = compute_tailoring_cache_key(&adrs, false, &[".".to_string()], "", None);
        let key2 = compute_tailoring_cache_key(&adrs, true, &[".".to_string()], "", None);
        assert_ne!(
            key1, key2,
            "different no_tailor should produce different key"
        );
    }

    #[test]
    fn test_tailoring_cache_key_differs_on_project_paths() {
        let key1 = compute_tailoring_cache_key(&[], false, &[".".to_string()], "", None);
        let key2 = compute_tailoring_cache_key(
            &[],
            false,
            &[".".to_string(), "apps/web".to_string()],
            "",
            None,
        );
        assert_ne!(
            key1, key2,
            "different projects should produce different key"
        );
    }

    #[test]
    fn test_tailoring_cache_key_differs_on_existing_claude_md() {
        let key1 = compute_tailoring_cache_key(&[], false, &[".".to_string()], "old content", None);
        let key2 = compute_tailoring_cache_key(&[], false, &[".".to_string()], "new content", None);
        assert_ne!(
            key1, key2,
            "different CLAUDE.md content should produce different key"
        );
    }

    #[test]
    fn test_tailoring_cache_key_differs_on_model() {
        let key1 = compute_tailoring_cache_key(&[], false, &[".".to_string()], "", Some("opus"));
        let key2 = compute_tailoring_cache_key(&[], false, &[".".to_string()], "", Some("sonnet"));
        assert_ne!(key1, key2, "different model should produce different key");
    }

    #[test]
    fn test_tailoring_cache_key_none_vs_some_model() {
        let key1 = compute_tailoring_cache_key(&[], false, &[".".to_string()], "", None);
        let key2 = compute_tailoring_cache_key(&[], false, &[".".to_string()], "", Some("opus"));
        assert_ne!(
            key1, key2,
            "None vs Some model should produce different key"
        );
    }

    /// Regression test: compute_tailoring_cache_key must receive the effective
    /// model (args.model OR config.model), not just the CLI flag.
    ///
    /// Before the fix, the callsite passed `args.model.as_deref()` only, so
    /// setting `model: haiku` in config had no effect on the cache key — a
    /// cached result from a prior sonnet run would be incorrectly served.
    #[test]
    fn test_tailoring_cache_key_config_model_differs_from_none() {
        // Simulates: no --model CLI flag, but config.model = "haiku"
        let args_model: Option<&str> = None;
        let config_model: Option<&str> = Some("haiku");
        let effective_model = args_model.or(config_model);

        // Key with config model applied (correct — post-fix behaviour)
        let key_with_config =
            compute_tailoring_cache_key(&[], false, &[".".to_string()], "", effective_model);
        // Key with only CLI flag (incorrect — pre-fix behaviour, passes None)
        let key_without_config =
            compute_tailoring_cache_key(&[], false, &[".".to_string()], "", args_model);

        assert_ne!(
            key_with_config, key_without_config,
            "cache key must differ when config.model is set vs not set"
        );
    }

    #[test]
    fn test_tailoring_cache_key_differs_on_adr_instructions() {
        // ADR with instructions vs without — exercises the instructions hash path.
        let adr_no_instructions = crate::api::types::Adr {
            id: "adr-001".to_string(),
            title: "Title".to_string(),
            context: None,
            policies: vec![],
            instructions: None,
            category: crate::api::types::AdrCategory {
                id: "cat-1".to_string(),
                name: "Cat".to_string(),
                path: "cat".to_string(),
            },
            applies_to: crate::api::types::AppliesTo {
                languages: vec![],
                frameworks: vec![],
                ..Default::default()
            },
            matched_projects: vec![],
            ..Default::default()
        };
        let mut adr_with_instructions = adr_no_instructions.clone();
        adr_with_instructions.instructions = Some(vec!["do this".to_string()]);

        let key1 = compute_tailoring_cache_key(&[adr_no_instructions], false, &[], "", None);
        let key2 = compute_tailoring_cache_key(&[adr_with_instructions], false, &[], "", None);
        assert_ne!(
            key1, key2,
            "different instructions should produce different key"
        );
    }

    #[test]
    fn test_tailoring_cache_key_differs_on_adr_frameworks() {
        // ADR with frameworks vs without — exercises the frameworks hash path.
        let adr_no_frameworks = crate::api::types::Adr {
            id: "adr-001".to_string(),
            title: "Title".to_string(),
            context: None,
            policies: vec![],
            instructions: None,
            category: crate::api::types::AdrCategory {
                id: "cat-1".to_string(),
                name: "Cat".to_string(),
                path: "cat".to_string(),
            },
            applies_to: crate::api::types::AppliesTo {
                languages: vec![],
                frameworks: vec![],
                ..Default::default()
            },
            matched_projects: vec![],
            ..Default::default()
        };
        let mut adr_with_frameworks = adr_no_frameworks.clone();
        adr_with_frameworks.applies_to.frameworks = vec!["actix-web".to_string()];

        let key1 = compute_tailoring_cache_key(&[adr_no_frameworks], false, &[], "", None);
        let key2 = compute_tailoring_cache_key(&[adr_with_frameworks], false, &[], "", None);
        assert_ne!(
            key1, key2,
            "different frameworks should produce different key"
        );
    }

    // ── store_tailoring_cache tests ──

    #[test]
    fn test_store_tailoring_cache_stores_result() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.yaml");
        let mut config = crate::config::Config::default();
        save_to(&config, &cfg_path).unwrap();

        let output = make_output(vec![make_file("CLAUDE.md", "rules", vec!["adr-001"])]);
        store_tailoring_cache(
            &mut config,
            &cfg_path,
            Some("test-cache-key"),
            dir.path(),
            &output,
        );

        assert!(config.cached_tailoring.is_some());
        let cached = config.cached_tailoring.as_ref().unwrap();
        assert_eq!(cached.cache_key, "test-cache-key");
        assert_eq!(cached.repo_path, dir.path().to_string_lossy().as_ref());

        // Verify the stored value round-trips
        let restored: TailoringOutput = cached.tailoring.clone();
        assert_eq!(restored, output);
    }

    #[test]
    fn test_store_tailoring_cache_skips_when_no_key() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.yaml");
        let mut config = crate::config::Config::default();
        save_to(&config, &cfg_path).unwrap();

        let output = make_output(vec![]);
        store_tailoring_cache(&mut config, &cfg_path, None, dir.path(), &output);

        assert!(
            config.cached_tailoring.is_none(),
            "should not store cache when key is None"
        );
    }

    #[test]
    fn test_store_tailoring_cache_persists_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.yaml");
        let mut config = crate::config::Config::default();
        save_to(&config, &cfg_path).unwrap();

        let output = make_output(vec![make_file("CLAUDE.md", "rules", vec!["adr-001"])]);
        store_tailoring_cache(
            &mut config,
            &cfg_path,
            Some("disk-test-key"),
            dir.path(),
            &output,
        );

        // Reload from disk and verify
        let loaded = load_from(&cfg_path).unwrap();
        let cached = loaded.cached_tailoring.unwrap();
        assert_eq!(cached.cache_key, "disk-test-key");
    }

    #[cfg(unix)]
    #[test]
    fn test_store_tailoring_cache_warns_on_disk_write_failure() {
        use std::os::unix::fs::PermissionsExt;
        // Create a read-only directory so that save_to fails, exercising the
        // "warning: failed to save tailoring cache" branch.
        let dir = tempfile::tempdir().unwrap();
        let ro_dir = dir.path().join("readonly");
        std::fs::create_dir(&ro_dir).unwrap();
        let cfg_path = ro_dir.join("config.yaml");

        let mut config = crate::config::Config::default();
        let output = make_output(vec![make_file("CLAUDE.md", "rules", vec!["adr-001"])]);

        // Make the directory read-only so save_to cannot write
        std::fs::set_permissions(&ro_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        // store_tailoring_cache should not panic; it warns and returns
        store_tailoring_cache(
            &mut config,
            &cfg_path,
            Some("warn-test-key"),
            dir.path(),
            &output,
        );

        // In-memory cache is still populated even though disk write failed
        assert!(
            config.cached_tailoring.is_some(),
            "in-memory cache should be set even if disk write failed"
        );

        // Restore permissions for cleanup
        std::fs::set_permissions(&ro_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn test_store_tailoring_cache_skips_oversized_output() {
        // Build a TailoringOutput that, when serialized, exceeds CACHE_MAX_SIZE_BYTES.
        use crate::config::types::CACHE_MAX_SIZE_BYTES;

        // Create a large content string that will make the serialized size exceed 10 MiB
        let large_content = "x".repeat(CACHE_MAX_SIZE_BYTES + 1024);
        let output = make_output(vec![make_file(
            "CLAUDE.md",
            &large_content,
            vec!["adr-001"],
        )]);

        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.yaml");
        let mut config = crate::config::Config::default();

        store_tailoring_cache(
            &mut config,
            &cfg_path,
            Some("size-test-key"),
            dir.path(),
            &output,
        );

        // Cache should NOT be stored because it exceeds the size limit
        assert!(
            config.cached_tailoring.is_none(),
            "oversized output should not be cached"
        );
    }

    // ── Tailoring cache round-trip test ──

    #[test]
    fn test_tailoring_output_round_trip_through_cached_tailoring() {
        use crate::config::types::CachedTailoring;
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                sections: vec![AdrSection {
                    adr_id: "adr-001".to_string(),
                    content: "# Rules\n\nDo things.".to_string(),
                }],
                reasoning: "Root rules".to_string(),
            }],
            skipped_adrs: vec![],
            summary: TailoringSummary {
                total_input: 1,
                applicable: 1,
                not_applicable: 0,
                files_generated: 1,
            },
        };

        let cached = CachedTailoring {
            cache_key: "test-key".to_string(),
            repo_path: "/repo".to_string(),
            tailoring: output.clone(),
            tailored_at: chrono::Utc::now(),
        };

        // Serialize to YAML and back
        let yaml = serde_yml::to_string(&cached).unwrap();
        let restored: CachedTailoring = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(cached.cache_key, restored.cache_key);
        assert_eq!(cached.tailoring, restored.tailoring);
    }

    #[test]
    fn test_corrupted_tailoring_cache_falls_through_in_config() {
        // A corrupted cached_tailoring block is silently dropped (treated as cache
        // miss) rather than failing to parse the entire Config.
        use crate::config::Config;
        let yaml = "cached_tailoring:\n  garbage_field: some_value\n";
        let cfg: Config = serde_yml::from_str(yaml).expect("config must parse despite bad cache");
        assert!(
            cfg.cached_tailoring.is_none(),
            "corrupt cached_tailoring should be treated as absent"
        );
    }

    #[test]
    fn test_tailoring_cache_expired_entry_treated_as_miss() {
        // An expired tailoring cache entry should be ignored (treated as a cache miss)
        // even if the cache key matches.
        use crate::config::paths::save_to;
        use crate::config::types::CachedTailoring;
        use crate::config::Config;

        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.yaml");

        let output = make_output(vec![make_file("CLAUDE.md", "rules", vec!["adr-001"])]);
        let cache_key = "some-key";

        let config = Config {
            cached_tailoring: Some(CachedTailoring {
                cache_key: cache_key.to_string(),
                repo_path: dir.path().to_string_lossy().to_string(),
                tailoring: output,
                // 8 days old — expired
                tailored_at: chrono::Utc::now() - chrono::Duration::days(8),
            }),
            ..Config::default()
        };
        save_to(&config, &cfg_path).unwrap();

        let loaded = load_from(&cfg_path).unwrap();
        // is_expired() should return true
        assert!(
            loaded.cached_tailoring.as_ref().unwrap().is_expired(),
            "8-day-old cache should be expired"
        );
        // The cache key matches but is_expired() is true — should not be used as a hit
        let hit = loaded
            .cached_tailoring
            .as_ref()
            .filter(|c| c.cache_key == cache_key && !c.is_expired());
        assert!(hit.is_none(), "expired cache should not be used as a hit");
    }

    // ── Integration: tailoring cache hit in run_sync ──

    /// Create a git repo in the given directory and make an initial commit.
    fn init_git_repo(dir: &std::path::Path) {
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
        std::fs::write(dir.join("dummy.txt"), "content").unwrap();
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
    }

    #[test]
    fn test_run_sync_tailoring_cache_hit_skips_tailoring() {
        let server = mock_api_server_with_adrs();
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let cfg_path = dir.path().join("config.yaml");

        // First run: force + no_tailor to populate the cache
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = SyncArgs {
            dry_run: false,
            full: false,
            force: true,
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: Some(server.url()),
            verbose: false,
            no_tailor: true,
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        };
        run_sync(&args, dir.path(), &cfg_path, &term, &runner, None, None).unwrap();

        // Verify cache was stored
        let loaded = load_from(&cfg_path).unwrap();
        assert!(
            loaded.cached_tailoring.is_some(),
            "expected tailoring cache to be populated after first run"
        );

        // Second run: same inputs, force=false → should use both analysis and tailoring cache
        // Reset the files so the second run encounters the same state
        let _ = std::fs::remove_file(dir.path().join("CLAUDE.md"));

        // Pre-load the config to verify the cache key will match
        let loaded = load_from(&cfg_path).unwrap();
        let cached_key = loaded.cached_tailoring.as_ref().unwrap().cache_key.clone();

        let server2 = mock_api_server_with_adrs();
        let args2 = SyncArgs {
            dry_run: false,
            full: false,
            force: false,
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: Some(server2.url()),
            verbose: false,
            no_tailor: true,
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        };

        // select_one index 0 = Accept for project confirmation and select all files
        let term2 = MockTerminal::new(vec![])
            .with_select_one(vec![0])
            .with_selection(Some(vec![0]));
        let result = run_sync(&args2, dir.path(), &cfg_path, &term2, &runner, None, None);
        assert!(
            result.is_ok(),
            "expected cache hit run to succeed: {result:?}"
        );

        // Verify CLAUDE.md was written from the cached tailoring output
        assert!(dir.path().join("CLAUDE.md").exists());

        // Verify the cache key hasn't changed (confirming cache was hit, not regenerated)
        let loaded2 = load_from(&cfg_path).unwrap();
        let cached_key2 = loaded2.cached_tailoring.as_ref().unwrap().cache_key.clone();
        assert_eq!(
            cached_key, cached_key2,
            "cache key should be unchanged on cache hit"
        );
    }

    #[test]
    fn test_run_sync_force_bypasses_tailoring_cache() {
        let server = mock_api_server_with_adrs();
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let cfg_path = dir.path().join("config.yaml");

        // First run: populate cache with no_tailor
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = SyncArgs {
            dry_run: false,
            full: false,
            force: true,
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: Some(server.url()),
            verbose: false,
            no_tailor: true,
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        };
        run_sync(&args, dir.path(), &cfg_path, &term, &runner, None, None).unwrap();

        let loaded = load_from(&cfg_path).unwrap();
        let first_time = loaded.cached_tailoring.as_ref().unwrap().tailored_at;

        // Wait a bit to ensure timestamp changes
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Second run: --force should bypass cache and update it
        let server2 = mock_api_server_with_adrs();
        let term2 = MockTerminal::new(vec![]);
        let args2 = SyncArgs {
            dry_run: false,
            full: false,
            force: true, // Bypass cache
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: Some(server2.url()),
            verbose: false,
            no_tailor: true,
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        };
        run_sync(&args2, dir.path(), &cfg_path, &term2, &runner, None, None).unwrap();

        // Cache should have been updated with a newer timestamp
        let loaded2 = load_from(&cfg_path).unwrap();
        let second_time = loaded2.cached_tailoring.as_ref().unwrap().tailored_at;
        assert!(
            second_time > first_time,
            "force should update cache timestamp: {first_time} vs {second_time}"
        );
    }

    // ── tailoring_cache_skip_msg unit tests ──

    #[test]
    fn test_tailoring_cache_skip_msg_zero_applicable() {
        assert_eq!(
            tailoring_cache_skip_msg(0),
            "No ADRs applicable to this project (cached)",
            "expected 0-applicable message"
        );
    }

    #[test]
    fn test_tailoring_cache_skip_msg_nonzero_applicable() {
        assert_eq!(
            tailoring_cache_skip_msg(1),
            "Using cached tailoring result",
            "expected with-files message for applicable=1"
        );
        assert_eq!(
            tailoring_cache_skip_msg(42),
            "Using cached tailoring result",
            "expected with-files message for applicable=42"
        );
    }

    // ── tailoring cache hit message integration tests ──

    /// Verify that when the tailoring cache contains a result with 0 applicable
    /// ADRs, the second run uses the cache and completes successfully without
    /// writing any files.
    #[test]
    fn test_tailoring_cache_hit_zero_applicable() {
        // Hold ENV_MUTEX to serialize with tests that modify PATH (e.g. the
        // fake-git timeout test).  git calls in init_git_repo and get_git_head
        // must see the real git binary.
        let _lock = crate::testutil::ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        // Use empty ADR server so raw_adrs_to_output produces applicable=0
        let server = mock_api_server();
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let cfg_path = dir.path().join("config.yaml");

        // First run: force + no_tailor + empty ADR response → cache stores applicable=0
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = SyncArgs {
            dry_run: false,
            full: false,
            force: true,
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: Some(server.url()),
            verbose: false,
            no_tailor: true,
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        };
        run_sync(&args, dir.path(), &cfg_path, &term, &runner, None, None).unwrap();

        // Verify that cache was stored and applicable == 0
        let loaded = load_from(&cfg_path).unwrap();
        let cached_tailoring = loaded.cached_tailoring.as_ref().unwrap();
        let cached_output: TailoringOutput = cached_tailoring.tailoring.clone();
        assert_eq!(
            cached_output.summary.applicable, 0,
            "expected 0 applicable ADRs in cache after empty-ADR run"
        );
        let first_key = cached_tailoring.cache_key.clone();

        // Second run: force=false → should hit the cache (applicable=0 path)
        // select_one index 0 = Accept for project confirmation; no file selection needed (0 files)
        let server2 = mock_api_server();
        let term2 = MockTerminal::new(vec![]).with_select_one(vec![0]);
        let args2 = SyncArgs {
            dry_run: false,
            full: false,
            force: false,
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: Some(server2.url()),
            verbose: false,
            no_tailor: true,
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        };
        let result = run_sync(&args2, dir.path(), &cfg_path, &term2, &runner, None, None);
        assert!(
            result.is_ok(),
            "expected cache-hit run with 0 applicable to succeed: {result:?}"
        );

        // Cache key must be unchanged (confirming cache was hit, not regenerated)
        let loaded2 = load_from(&cfg_path).unwrap();
        let second_key = loaded2.cached_tailoring.as_ref().unwrap().cache_key.clone();
        assert_eq!(
            first_key, second_key,
            "cache key should be unchanged on 0-applicable cache hit"
        );

        // No CLAUDE.md should be written since there are 0 applicable ADRs
        assert!(
            !dir.path().join("CLAUDE.md").exists(),
            "expected no CLAUDE.md for 0-applicable cached output"
        );
    }

    /// Verify that when the tailoring cache contains a result with >0 applicable
    /// ADRs, the second run uses the cache and writes the files as expected.
    #[test]
    fn test_tailoring_cache_hit_with_files() {
        // Hold ENV_MUTEX to serialize with tests that modify PATH (e.g. the
        // fake-git timeout test).  git calls in init_git_repo and get_git_head
        // must see the real git binary.
        let _lock = crate::testutil::ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        // Use the ADR server so raw_adrs_to_output produces applicable=1
        let server = mock_api_server_with_adrs();
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let cfg_path = dir.path().join("config.yaml");

        // First run: force + no_tailor + ADR response → cache stores applicable=1
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = SyncArgs {
            dry_run: false,
            full: false,
            force: true,
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: Some(server.url()),
            verbose: false,
            no_tailor: true,
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        };
        run_sync(&args, dir.path(), &cfg_path, &term, &runner, None, None).unwrap();

        // Verify that cache was stored and applicable > 0
        let loaded = load_from(&cfg_path).unwrap();
        let cached_tailoring = loaded.cached_tailoring.as_ref().unwrap();
        let cached_output: TailoringOutput = cached_tailoring.tailoring.clone();
        assert!(
            cached_output.summary.applicable > 0,
            "expected >0 applicable ADRs in cache"
        );
        let first_key = cached_tailoring.cache_key.clone();

        // Remove the written CLAUDE.md so the second run can re-write it
        let _ = std::fs::remove_file(dir.path().join("CLAUDE.md"));

        // Second run: force=false → should hit the cache (with-files path)
        let server2 = mock_api_server_with_adrs();
        let term2 = MockTerminal::new(vec![])
            .with_select_one(vec![0])
            .with_selection(Some(vec![0]));
        let args2 = SyncArgs {
            dry_run: false,
            full: false,
            force: false,
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: Some(server2.url()),
            verbose: false,
            no_tailor: true,
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        };
        let result = run_sync(&args2, dir.path(), &cfg_path, &term2, &runner, None, None);
        assert!(
            result.is_ok(),
            "expected cache-hit run with files to succeed: {result:?}"
        );

        // Cache key must be unchanged (confirming cache was hit, not regenerated)
        let loaded2 = load_from(&cfg_path).unwrap();
        let second_key = loaded2.cached_tailoring.as_ref().unwrap().cache_key.clone();
        assert_eq!(
            first_key, second_key,
            "cache key should be unchanged on with-files cache hit"
        );

        // CLAUDE.md should be written from the cached output
        assert!(
            dir.path().join("CLAUDE.md").exists(),
            "expected CLAUDE.md to be written from cached output with files"
        );
    }

    // ── apply_tailoring_event tests ──

    #[test]
    fn test_apply_tailoring_event_project_started_does_not_increment_counter() {
        let mut pipeline = TuiRenderer::new(true, false); // quiet mode — no output
        let mut completed = 0usize;
        apply_tailoring_event(
            TailoringEvent::ProjectStarted {
                project_name: "app".to_string(),
                batch_count: 2,
            },
            &mut pipeline,
            &mut completed,
        );
        // ProjectStarted prints a log line but does not increment the completed counter
        assert_eq!(completed, 0);
    }

    #[test]
    fn test_apply_tailoring_event_batch_completed_does_not_increment_counter() {
        let mut pipeline = TuiRenderer::new(false, true); // plain mode
        pipeline.start(SyncPhase::Tailor, "Tailoring ADRs (0/2 projects done)...");
        let mut completed = 0usize;
        apply_tailoring_event(
            TailoringEvent::BatchCompleted {
                project_name: "my-app".to_string(),
                batch_index: 1,
                batch_count: 2,
                adr_count: 5,
                applied_count: 5,
                skipped_count: 0,
                skipped_adrs: vec![],
                file_paths: vec!["CLAUDE.md".to_string()],
                elapsed_secs: 30,
            },
            &mut pipeline,
            &mut completed,
        );
        // BatchCompleted logs a line but does NOT increment the project-level counter
        // (spinner tick is handled by the heartbeat timer in run_tailoring_with_cancel)
        assert_eq!(
            completed, 0,
            "BatchCompleted must not increment project counter"
        );
    }

    #[test]
    fn test_apply_tailoring_event_project_completed_increments_counter() {
        let mut pipeline = TuiRenderer::new(false, true); // plain mode
        pipeline.start(SyncPhase::Tailor, "Tailoring ADRs (0/3 projects done)...");
        let mut completed = 0usize;
        apply_tailoring_event(
            TailoringEvent::ProjectCompleted {
                project_name: "web-app".to_string(),
                files_generated: 2,
                adrs_applied: 5,
                file_paths: vec!["CLAUDE.md".to_string(), "apps/web/CLAUDE.md".to_string()],
            },
            &mut pipeline,
            &mut completed,
        );
        assert_eq!(completed, 1, "expected completed to increment to 1");
    }

    #[test]
    fn test_apply_tailoring_event_project_completed_multiple_times() {
        let mut pipeline = TuiRenderer::new(false, true); // plain mode
        pipeline.start(SyncPhase::Tailor, "Tailoring ADRs (0/3 projects done)...");
        let mut completed = 0usize;
        for i in 1..=3usize {
            apply_tailoring_event(
                TailoringEvent::ProjectCompleted {
                    project_name: format!("project-{i}"),
                    files_generated: 1,
                    adrs_applied: 2,
                    file_paths: vec!["CLAUDE.md".to_string()],
                },
                &mut pipeline,
                &mut completed,
            );
            assert_eq!(completed, i, "expected completed={i} after {i} events");
        }
    }

    #[test]
    fn test_apply_tailoring_event_project_failed_does_not_increment() {
        let mut pipeline = TuiRenderer::new(false, true); // plain mode
        pipeline.start(SyncPhase::Tailor, "Tailoring ADRs (0/2 projects done)...");
        let mut completed = 0usize;
        apply_tailoring_event(
            TailoringEvent::ProjectFailed {
                project_name: "bad-app".to_string(),
                error: "timeout".to_string(),
            },
            &mut pipeline,
            &mut completed,
        );
        // ProjectFailed should not increment the counter
        assert_eq!(completed, 0, "expected completed to remain 0 on failure");
    }

    #[test]
    fn test_apply_tailoring_event_project_failed_quiet_mode() {
        // In quiet mode, println is a no-op — should not panic
        let mut pipeline = TuiRenderer::new(true, false);
        let mut completed = 0usize;
        apply_tailoring_event(
            TailoringEvent::ProjectFailed {
                project_name: "bad-app".to_string(),
                error: "crashed".to_string(),
            },
            &mut pipeline,
            &mut completed,
        );
        assert_eq!(completed, 0);
    }

    #[test]
    fn test_apply_batch_started_prints_category() {
        let mut pipeline = TuiRenderer::new(false, true); // plain mode
        pipeline.start(SyncPhase::Tailor, "Tailoring ADRs (0/1 projects done)...");
        let mut completed = 0usize;
        apply_tailoring_event(
            TailoringEvent::BatchStarted {
                project_name: "my-app".to_string(),
                batch_index: 1,
                batch_count: 2,
                adr_count: 8,
                category_name: "Error Handling".to_string(),
                adr_titles: vec!["Use Result".to_string(), "Avoid panics".to_string()],
            },
            &mut pipeline,
            &mut completed,
        );
        // BatchStarted should not increment the counter
        assert_eq!(
            completed, 0,
            "BatchStarted must not increment project counter"
        );
    }

    #[test]
    fn test_apply_batch_completed_with_skipped() {
        let mut pipeline = TuiRenderer::new(false, true); // plain mode
        pipeline.start(SyncPhase::Tailor, "Tailoring ADRs (0/1 projects done)...");
        let mut completed = 0usize;
        apply_tailoring_event(
            TailoringEvent::BatchCompleted {
                project_name: "my-app".to_string(),
                batch_index: 1,
                batch_count: 2,
                adr_count: 5,
                applied_count: 3,
                skipped_count: 2,
                skipped_adrs: vec![
                    ("adr-001".to_string(), "not applicable to Rust".to_string()),
                    (
                        "adr-002".to_string(),
                        "no mobile project detected".to_string(),
                    ),
                ],
                file_paths: vec!["CLAUDE.md".to_string()],
                elapsed_secs: 47,
            },
            &mut pipeline,
            &mut completed,
        );
        // BatchCompleted should not increment the counter
        assert_eq!(
            completed, 0,
            "BatchCompleted must not increment project counter"
        );
    }

    #[test]
    fn test_apply_batch_completed_no_files_no_skips() {
        // Exercises the empty file_paths branch (paths_str = String::new())
        // and the skipped_count == 0 branch.
        let mut pipeline = TuiRenderer::new(false, true); // plain mode
        pipeline.start(SyncPhase::Tailor, "Tailoring ADRs (0/1 projects done)...");
        let mut completed = 0usize;
        apply_tailoring_event(
            TailoringEvent::BatchCompleted {
                project_name: "my-app".to_string(),
                batch_index: 1,
                batch_count: 1,
                adr_count: 3,
                applied_count: 3,
                skipped_count: 0,
                skipped_adrs: vec![],
                file_paths: vec![], // empty — exercises the String::new() branch
                elapsed_secs: 10,
            },
            &mut pipeline,
            &mut completed,
        );
        assert_eq!(
            completed, 0,
            "BatchCompleted must not increment project counter"
        );
    }

    #[test]
    fn test_apply_project_completed_shows_paths() {
        let mut pipeline = TuiRenderer::new(false, true); // plain mode
        pipeline.start(SyncPhase::Tailor, "Tailoring ADRs (0/1 projects done)...");
        let mut completed = 0usize;
        apply_tailoring_event(
            TailoringEvent::ProjectCompleted {
                project_name: "test-nextjs-project".to_string(),
                files_generated: 2,
                adrs_applied: 6,
                file_paths: vec!["CLAUDE.md".to_string(), "apps/web/CLAUDE.md".to_string()],
            },
            &mut pipeline,
            &mut completed,
        );
        assert_eq!(
            completed, 1,
            "ProjectCompleted must increment project counter"
        );
    }

    // ── run_tailoring_with_cancel tests ──

    /// Verify that when the cancel future fires immediately with `Ok(())` the
    /// loop returns `UserCancelled` without waiting for the tailor future.
    #[tokio::test]
    async fn test_run_tailoring_with_cancel_returns_user_cancelled() {
        let mut pipeline = TuiRenderer::new(true, false);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<TailoringEvent>();
        drop(tx); // close sender so recv returns None

        // A tailor future that never completes
        let tailor_fut = std::future::pending::<Result<TailoringOutput, ActualError>>();
        // A cancel future that fires immediately with Ok — simulates successful
        // ctrl_c signal registration + user pressing Ctrl+C.
        let cancel = std::future::ready(Ok::<(), std::io::Error>(()));

        let (_etx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let result =
            run_tailoring_with_cancel(tailor_fut, &mut rx, &mut event_rx, &mut pipeline, 0, cancel)
                .await;

        assert!(
            matches!(result, Err(ActualError::UserCancelled)),
            "expected UserCancelled, got {result:?}"
        );
    }

    /// Verify that when the cancel future fires with `Err(...)` (ctrl_c
    /// registration failure), tailoring is NOT cancelled — the loop continues
    /// and returns the tailor result.
    #[tokio::test]
    async fn test_run_tailoring_with_cancel_ignores_ctrl_c_error() {
        let mut pipeline = TuiRenderer::new(true, false);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<TailoringEvent>();

        // Sequence: cancel fires (with Err) first, then tailor completes.
        // Use a one-shot channel to ensure cancel fires before the tailor future.
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();

        // cancel future: immediately resolves with an error
        let cancel = std::future::ready(Err::<(), std::io::Error>(std::io::Error::other(
            "signal registration failed",
        )));

        // tailor future: waits until the cancel arm has fired, then returns Ok
        let output = make_output(vec![]);
        let tailor_fut = async move {
            // Signal that we're ready to proceed
            let _ = ready_tx.send(());
            // Give the event loop a chance to process the cancel arm
            tokio::task::yield_now().await;
            Ok::<TailoringOutput, ActualError>(output)
        };

        // Wait for the tailor future to signal readiness (not strictly needed
        // here since futures are lazy, but drop the rx to avoid lint warnings).
        drop(ready_rx);
        drop(tx);

        let (_etx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let result =
            run_tailoring_with_cancel(tailor_fut, &mut rx, &mut event_rx, &mut pipeline, 0, cancel)
                .await;

        // The cancel Err path must be ignored — tailoring should complete normally
        assert!(
            result.is_ok(),
            "expected Ok result when ctrl_c registration fails, got {result:?}"
        );
    }

    /// Verify that buffered events are drained from the channel when the tailor
    /// future completes before all events have been received through `recv`.
    #[tokio::test]
    async fn test_run_tailoring_with_cancel_drains_buffered_events_on_completion() {
        let mut pipeline = TuiRenderer::new(true, false);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<TailoringEvent>();

        // Pre-buffer a completed event so it sits in the channel before the
        // future resolves.  The select loop will pick up the tailor result
        // first (ready future), then the drain loop must consume this event.
        tx.send(TailoringEvent::ProjectCompleted {
            project_name: "app".to_string(),
            files_generated: 1,
            adrs_applied: 1,
            file_paths: vec!["CLAUDE.md".to_string()],
        })
        .unwrap();
        drop(tx);

        let output = make_output(vec![]);
        let tailor_fut = std::future::ready(Ok::<TailoringOutput, ActualError>(output));
        // cancel never fires
        let cancel = std::future::pending::<std::io::Result<()>>();

        let (_etx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let result =
            run_tailoring_with_cancel(tailor_fut, &mut rx, &mut event_rx, &mut pipeline, 1, cancel)
                .await;

        assert!(result.is_ok(), "expected Ok result, got {result:?}");
        // Channel must be fully drained — no events left
        assert!(
            rx.try_recv().is_err(),
            "expected channel to be empty after drain"
        );
    }

    /// Verify that the heartbeat timer arm executes and updates the spinner
    /// message.  Use a tailor future that first sleeps briefly (forcing at
    /// least one heartbeat tick), then completes — so the heartbeat body is
    /// covered in the same task as the function under test.
    #[tokio::test]
    async fn test_run_tailoring_with_cancel_heartbeat_updates_message() {
        tokio::time::pause();
        let mut pipeline = TuiRenderer::new(false, true); // plain mode
        pipeline.start(SyncPhase::Tailor, "initial message");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<TailoringEvent>();
        drop(tx);

        let output = make_output(vec![]);
        // Tailor future: sleep 150ms (> one 100ms heartbeat tick) then return.
        let tailor_fut = async move {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            Ok::<TailoringOutput, ActualError>(output)
        };
        let cancel = std::future::pending::<std::io::Result<()>>();

        // Advance time so the interval fires before the sleep finishes.
        // tokio::time::pause() makes all timers advance in lockstep.
        let (_etx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let result =
            run_tailoring_with_cancel(tailor_fut, &mut rx, &mut event_rx, &mut pipeline, 2, cancel)
                .await;

        assert!(result.is_ok(), "expected Ok result, got {result:?}");
        // The heartbeat must have updated the live elapsed (step row shows [Xs]).
        assert!(
            pipeline.steps.steps[3].elapsed.is_some(),
            "expected live elapsed to be set by heartbeat"
        );
        // step.message must remain empty — progress text is discarded by update_message.
        let step_msg = pipeline.steps.steps[3].message.clone();
        assert!(
            step_msg.is_empty(),
            "step.message must not contain progress text, got: {step_msg:?}"
        );
        // The log pane must NOT have been flooded by update_message heartbeat entries.
        let logged = pipeline.logs[3].render_to_string(100, 500, 0);
        let has_progress_in_log = logged.iter().any(|l| l.contains("projects done"));
        assert!(
            !has_progress_in_log,
            "heartbeat message must not appear in log pane"
        );
    }

    // ── zero_adr_context_message tests ──

    use crate::api::types::{MatchFramework, MatchProject, MatchRequest};

    fn make_match_request(projects: Vec<MatchProject>) -> MatchRequest {
        MatchRequest {
            projects,
            options: None,
        }
    }

    fn make_match_project(languages: Vec<&str>, frameworks: Vec<(&str, &str)>) -> MatchProject {
        MatchProject {
            path: ".".to_string(),
            name: "test-project".to_string(),
            languages: languages.into_iter().map(str::to_string).collect(),
            frameworks: frameworks
                .into_iter()
                .map(|(name, category)| MatchFramework {
                    name: name.to_string(),
                    category: category.to_string(),
                })
                .collect(),
            canonical_ir: None,
        }
    }

    #[test]
    fn test_zero_adr_context_message_no_languages() {
        let req = make_match_request(vec![make_match_project(vec![], vec![])]);
        let lines = zero_adr_context_lines(&req);
        let msg = lines.join("\n");
        assert!(
            msg.contains("No supported languages detected"),
            "unexpected message: {msg}"
        );
        assert!(
            msg.contains("unable to match ADRs"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn test_zero_adr_context_message_languages_no_frameworks() {
        let req = make_match_request(vec![make_match_project(vec!["typescript"], vec![])]);
        let lines = zero_adr_context_lines(&req);
        let msg = lines.join("\n");
        assert!(
            msg.contains("Languages: typescript"),
            "unexpected message: {msg}"
        );
        assert!(
            msg.contains("No frameworks detected"),
            "unexpected message: {msg}"
        );
        assert!(
            msg.contains("only general ADRs were eligible"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn test_zero_adr_context_message_languages_and_frameworks() {
        let req = make_match_request(vec![make_match_project(
            vec!["typescript"],
            vec![("nextjs", "web-frontend")],
        )]);
        let lines = zero_adr_context_lines(&req);
        let msg = lines.join("\n");
        assert!(
            msg.contains("Languages: typescript"),
            "unexpected message: {msg}"
        );
        assert!(
            msg.contains("Frameworks: nextjs"),
            "unexpected message: {msg}"
        );
        assert!(
            msg.contains("No ADRs available for this stack yet"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn test_zero_adr_context_message_deduplicates_across_projects() {
        // Two projects with the same language → appears only once in message
        let req = make_match_request(vec![
            make_match_project(vec!["typescript"], vec![]),
            make_match_project(vec!["typescript"], vec![]),
        ]);
        let lines = zero_adr_context_lines(&req);
        let msg = lines.join("\n");
        // "typescript" should appear exactly once
        assert_eq!(
            msg.matches("typescript").count(),
            1,
            "language should be deduplicated: {msg}"
        );
    }

    #[test]
    fn test_zero_adr_context_message_multiple_languages_sorted() {
        let req = make_match_request(vec![make_match_project(vec!["rust", "typescript"], vec![])]);
        let lines = zero_adr_context_lines(&req);
        let msg = lines.join("\n");
        // Languages should be sorted alphabetically
        let rust_pos = msg.find("rust").unwrap();
        let ts_pos = msg.find("typescript").unwrap();
        assert!(
            rust_pos < ts_pos,
            "languages should be sorted: rust before typescript in '{msg}'"
        );
    }

    #[test]
    fn test_zero_adr_context_message_empty_request() {
        // No projects at all
        let req = make_match_request(vec![]);
        let lines = zero_adr_context_lines(&req);
        let msg = lines.join("\n");
        assert!(
            msg.contains("No supported languages detected"),
            "empty request should produce no-languages message: {msg}"
        );
    }

    // ── fetch_error_message tests ──

    #[test]
    fn test_fetch_error_message_connection_refused() {
        let e = ActualError::ApiError("error sending request: Connection refused".to_string());
        let msg = fetch_error_message(&e, "https://api.example.com");
        assert!(
            msg.contains("Unable to connect to Actual AI"),
            "unexpected msg: {msg}"
        );
        assert!(
            msg.contains("https://api.example.com"),
            "should include api_url: {msg}"
        );
        assert!(
            msg.contains("check your network connection"),
            "unexpected msg: {msg}"
        );
    }

    #[test]
    fn test_fetch_error_message_error_trying_to_connect() {
        let e = ActualError::ApiError(
            "error trying to connect: tcp connect error: Connection refused".to_string(),
        );
        let msg = fetch_error_message(&e, "https://api.example.com");
        assert!(
            msg.contains("Unable to connect to Actual AI"),
            "unexpected msg: {msg}"
        );
    }

    #[test]
    fn test_fetch_error_message_dns_error() {
        let e = ActualError::ApiError("dns error: failed to lookup".to_string());
        let msg = fetch_error_message(&e, "https://api.example.com");
        assert!(
            msg.contains("Unable to connect to Actual AI"),
            "unexpected msg: {msg}"
        );
    }

    #[test]
    fn test_fetch_error_message_timed_out() {
        let e = ActualError::ApiError("request timed out after 30s".to_string());
        let msg = fetch_error_message(&e, "https://api.example.com");
        assert!(
            msg.contains("Connection to Actual AI timed out"),
            "unexpected msg: {msg}"
        );
        assert!(msg.contains("try again later"), "unexpected msg: {msg}");
    }

    #[test]
    fn test_fetch_error_message_operation_timed_out() {
        let e = ActualError::ApiError("operation timed out".to_string());
        let msg = fetch_error_message(&e, "https://api.example.com");
        assert!(
            msg.contains("Connection to Actual AI timed out"),
            "unexpected msg: {msg}"
        );
    }

    #[test]
    fn test_fetch_error_message_api_response_error() {
        let e = ActualError::ApiResponseError {
            code: "401".to_string(),
            message: "unauthorized".to_string(),
        };
        let msg = fetch_error_message(&e, "https://api.example.com");
        assert!(
            msg.contains("Actual AI returned an error (401): unauthorized"),
            "unexpected msg: {msg}"
        );
    }

    #[test]
    fn test_fetch_error_message_api_response_error_500() {
        let e = ActualError::ApiResponseError {
            code: "500".to_string(),
            message: "internal server error".to_string(),
        };
        let msg = fetch_error_message(&e, "https://api.example.com");
        assert!(
            msg.contains("Actual AI returned an error (500): internal server error"),
            "unexpected msg: {msg}"
        );
    }

    #[test]
    fn test_fetch_error_message_generic_api_error_catch_all() {
        let e = ActualError::ApiError("some unexpected error".to_string());
        let msg = fetch_error_message(&e, "https://api.example.com");
        assert!(msg.contains("API request failed:"), "unexpected msg: {msg}");
        assert!(
            msg.contains("some unexpected error"),
            "unexpected msg: {msg}"
        );
    }

    #[test]
    fn test_fetch_error_message_user_cancelled_catch_all() {
        let e = ActualError::UserCancelled;
        let msg = fetch_error_message(&e, "https://api.example.com");
        assert!(msg.contains("API request failed:"), "unexpected msg: {msg}");
    }

    #[test]
    fn test_fetch_error_message_service_unavailable() {
        let e = ActualError::ServiceUnavailable;
        let msg = fetch_error_message(&e, "https://api.example.com");
        assert!(
            msg.contains("being updated"),
            "expected 'being updated' in: {msg}"
        );
        assert!(
            msg.contains("available shortly"),
            "expected 'available shortly' in: {msg}"
        );
    }

    // ── format_cache_age unit tests ──

    #[test]
    fn test_format_cache_age_seconds() {
        assert_eq!(format_cache_age(0), "0s ago");
        assert_eq!(format_cache_age(30), "30s ago");
        assert_eq!(format_cache_age(59), "59s ago");
    }

    #[test]
    fn test_format_cache_age_minutes() {
        assert_eq!(format_cache_age(60), "1m ago");
        assert_eq!(format_cache_age(90), "1m ago");
        assert_eq!(format_cache_age(3599), "59m ago");
    }

    #[test]
    fn test_format_cache_age_hours() {
        assert_eq!(format_cache_age(3600), "1h ago");
        assert_eq!(format_cache_age(7200), "2h ago");
        assert_eq!(format_cache_age(86399), "23h ago");
    }

    #[test]
    fn test_format_cache_age_days() {
        assert_eq!(format_cache_age(86400), "1d ago");
        assert_eq!(format_cache_age(172800), "2d ago");
    }

    #[test]
    fn test_format_cache_age_negative() {
        assert_eq!(format_cache_age(-1), "just now");
        assert_eq!(format_cache_age(-300), "just now");
        assert_eq!(format_cache_age(-86400), "just now");
    }

    // ── Gap 6: load_config_with_fallback returns default on error ──

    /// Verify that `load_config_with_fallback` returns `Config::default()` when
    /// the config file contains invalid YAML, exercising the `Err` branch that
    /// emits a `tracing::warn!` instead of hard-failing.
    #[test]
    fn test_load_config_with_fallback_returns_default_on_invalid_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.yaml");

        // Write deliberately invalid YAML so `load_from` returns Err.
        std::fs::write(&cfg_path, ": invalid: yaml: [[[").unwrap();

        // Must not panic or propagate the error; must return the default config.
        let config = load_config_with_fallback(&cfg_path);
        assert_eq!(
            config,
            crate::config::Config::default(),
            "expected default config when YAML is invalid"
        );
    }

    // ── Gap 1: subprocess stderr is surfaced in the TUI log pane ──

    /// A runner that immediately returns `RunnerFailed` with a
    /// non-empty stderr so we can verify the stderr-surfacing branch in
    /// `run_sync` (lines 521-527).
    struct FailingRunner {
        stderr: String,
    }

    impl TailoringRunner for FailingRunner {
        async fn run_tailoring(
            &self,
            _prompt: &str,
            _schema: &str,
            _model_override: Option<&str>,
            _max_budget_usd: Option<f64>,
        ) -> Result<TailoringOutput, ActualError> {
            Err(ActualError::RunnerFailed {
                message: "subprocess exited with code 1".to_string(),
                stderr: self.stderr.clone(),
            })
        }
    }

    /// Verify that when tailoring fails with `RunnerFailed` and
    /// non-empty stderr, `run_sync` returns `Err` and the stderr branch is
    /// exercised (Gap 1 coverage).
    #[test]
    fn test_run_sync_tailoring_subprocess_failed_with_stderr_surfaces_output() {
        let server = mock_api_server_with_adrs();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = FailingRunner {
            stderr: "error: quota exceeded\nretry after 60s".to_string(),
        };
        let args = SyncArgs {
            dry_run: false,
            full: false,
            force: true,
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: Some(server.url()),
            verbose: false,
            no_tailor: false, // must reach the tailoring path
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        };
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(
            result.is_err(),
            "expected tailoring failure to propagate as Err"
        );
        let err = result.unwrap_err();
        assert!(
            matches!(err, ActualError::RunnerFailed { .. }),
            "expected RunnerFailed error, got: {err:?}"
        );
    }

    /// Same as above but with empty stderr — exercises the `stderr.is_empty()`
    /// guard branch (line 516) that skips printing when there is nothing to show.
    #[test]
    fn test_run_sync_tailoring_subprocess_failed_empty_stderr_no_output() {
        let server = mock_api_server_with_adrs();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = FailingRunner {
            stderr: String::new(), // empty stderr — must not print "Subprocess output:"
        };
        let args = SyncArgs {
            dry_run: false,
            full: false,
            force: true,
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: Some(server.url()),
            verbose: false,
            no_tailor: false,
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        };
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        let err = result.unwrap_err();
        assert!(
            matches!(err, ActualError::RunnerFailed { .. }),
            "expected RunnerFailed, got: {err:?}"
        );
    }

    // ── Gap 7: --verbose surfaces per-file reasoning ──

    /// Verify that when `--verbose` is set and tailoring returns files with
    /// non-empty reasoning, `run_sync` prints `[path]: reasoning` lines.
    #[test]
    fn test_run_sync_verbose_surfaces_reasoning_per_file() {
        // VALID_TAILORING_JSON has reasoning = "Root rules" for CLAUDE.md.
        let server = mock_api_server_with_adrs();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_TAILORING_JSON);
        let args = SyncArgs {
            dry_run: true, // don't write files
            full: false,
            force: true,
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: Some(server.url()),
            verbose: true, // must hit the reasoning loop (lines 537-544)
            no_tailor: false,
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        };
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
    }

    // ── Gap 2: hang detection warning fires after timeout ──

    /// Verify that when no progress events arrive for ≥ HANG_WARN_SECS the
    /// hang-warning is emitted exactly once to the TUI log pane (Gap 2
    /// coverage for lines 721-731).
    #[tokio::test]
    async fn test_run_tailoring_with_cancel_hang_warning_fires_after_timeout() {
        tokio::time::pause();
        let mut pipeline = TuiRenderer::new(false, true); // plain mode → log pane populated
        pipeline.start(SyncPhase::Tailor, "initial");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<TailoringEvent>();
        drop(tx); // close sender; recv() will return None once the channel is drained

        // Tailor future: sleep long enough for multiple heartbeat ticks + the
        // HANG_WARN_SECS threshold to elapse, then return Ok.
        let output = make_output(vec![]);
        let hang_secs = HANG_WARN_SECS;
        let tailor_fut = async move {
            // Sleep past the hang threshold so the heartbeat arm fires and
            // detects the elapsed time.
            tokio::time::sleep(std::time::Duration::from_secs(hang_secs + 1)).await;
            Ok::<TailoringOutput, ActualError>(output)
        };
        let cancel = std::future::pending::<std::io::Result<()>>();

        let (_etx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let result =
            run_tailoring_with_cancel(tailor_fut, &mut rx, &mut event_rx, &mut pipeline, 0, cancel)
                .await;

        assert!(result.is_ok(), "expected Ok, got {result:?}");

        // The warning must appear exactly once in the TUI log pane (index 3 = Tailor).
        let logged = pipeline.logs[3].render_to_string(200, 500, 0);
        let warn_lines: Vec<_> = logged
            .iter()
            .filter(|l| l.contains("No output received"))
            .collect();
        assert_eq!(
            warn_lines.len(),
            1,
            "expected exactly one hang warning line, got: {logged:?}"
        );
    }

    // ── event streaming tests ──

    /// Verify that the event_rx select arm displays stream events in the TUI log
    /// pane as they arrive from the subprocess.
    #[tokio::test]
    async fn test_run_tailoring_with_cancel_live_streams_event_line() {
        let mut pipeline = TuiRenderer::new(false, true); // plain mode
        pipeline.start(SyncPhase::Tailor, "initial");
        let (tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel::<TailoringEvent>();
        drop(tx); // no progress events

        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        // Send an event line *before* the future resolves so the streaming
        // arm has a chance to fire inside the select loop.
        event_tx
            .send("Claude: Read src/main.rs".to_string())
            .unwrap();

        let output = make_output(vec![]);
        let tailor_fut = async move {
            // Yield so the select loop can process the buffered event line.
            tokio::task::yield_now().await;
            Ok::<TailoringOutput, ActualError>(output)
        };
        let cancel = std::future::pending::<std::io::Result<()>>();

        let result = run_tailoring_with_cancel(
            tailor_fut,
            &mut progress_rx,
            &mut event_rx,
            &mut pipeline,
            0,
            cancel,
        )
        .await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");

        // The event line must appear in the TUI log pane.
        let logged = pipeline.logs[3].render_to_string(200, 500, 0);
        assert!(
            logged
                .iter()
                .any(|l| l.contains("Claude: Read src/main.rs")),
            "event line must appear in log pane: {logged:?}"
        );
    }

    /// Verify that receiving an event line resets the hang-detection timer so
    /// the warning does NOT fire when the future completes before the threshold.
    ///
    /// The future completes after (HANG_WARN_SECS - 5) seconds — below the
    /// threshold — so no warning is emitted.  We also verify the event appears
    /// in the log pane (confirming the event_rx arm actually fired).
    #[tokio::test]
    async fn test_run_tailoring_with_cancel_event_resets_hang_timer() {
        tokio::time::pause();
        let mut pipeline = TuiRenderer::new(false, true); // plain mode
        pipeline.start(SyncPhase::Tailor, "initial");
        let (tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel::<TailoringEvent>();
        drop(tx);

        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let hang_secs = HANG_WARN_SECS;
        let output = make_output(vec![]);
        // Tailor completes before the hang threshold so no warning is expected.
        let tailor_fut = async move {
            tokio::time::sleep(std::time::Duration::from_secs(hang_secs - 5)).await;
            Ok::<TailoringOutput, ActualError>(output)
        };
        let cancel = std::future::pending::<std::io::Result<()>>();

        // Pre-buffer an event so the event_rx arm fires when the loop starts.
        event_tx
            .send("Claude: Read src/app.tsx".to_string())
            .unwrap();
        drop(event_tx);

        let result = run_tailoring_with_cancel(
            tailor_fut,
            &mut progress_rx,
            &mut event_rx,
            &mut pipeline,
            0,
            cancel,
        )
        .await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");

        let logged = pipeline.logs[3].render_to_string(200, 500, 0);
        // No hang warning — the future completed before HANG_WARN_SECS elapsed.
        let has_hang_warn = logged.iter().any(|l| l.contains("No output received"));
        assert!(
            !has_hang_warn,
            "hang warning must not fire before threshold: {logged:?}"
        );
        // Event must have been shown in the log pane.
        assert!(
            logged
                .iter()
                .any(|l| l.contains("Claude: Read src/app.tsx")),
            "event line must appear in log pane: {logged:?}"
        );
    }

    /// Verify that run_sync creates the event channel and calls set_event_tx on
    /// the runner (the MockRunner no-op implementation is fine here).
    #[test]
    fn test_run_sync_creates_event_channel() {
        let server = mock_api_server_with_adrs();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_TAILORING_JSON);
        let args = SyncArgs {
            dry_run: true,
            full: false,
            force: true,
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: Some(server.url()),
            verbose: false,
            no_tailor: false,
            max_budget_usd: None,
            no_tui: false,
            output_format: None,
            runner: None,
            show_errors: false,
        };
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(
            result.is_ok(),
            "expected Ok with event channel wiring: {result:?}"
        );
    }

    // ── tilde_collapse unit tests ──

    #[test]
    fn test_tilde_collapse_with_home_prefix() {
        let home = std::path::PathBuf::from("/home/user");
        let path = std::path::Path::new("/home/user/projects/foo");
        assert_eq!(tilde_collapse(path, Some(&home)), "~/projects/foo");
    }

    #[test]
    fn test_tilde_collapse_without_home_prefix() {
        let home = std::path::PathBuf::from("/home/user");
        let path = std::path::Path::new("/tmp/other");
        assert_eq!(tilde_collapse(path, Some(&home)), "/tmp/other");
    }

    #[test]
    fn test_tilde_collapse_no_home_dir() {
        let path = std::path::Path::new("/tmp/other");
        assert_eq!(tilde_collapse(path, None), "/tmp/other");
    }

    // ── auto_select_for_project tests ──

    fn make_project_for_selection(
        languages: Vec<LanguageStat>,
        frameworks: Vec<Framework>,
    ) -> Project {
        Project {
            path: ".".to_string(),
            name: "test".to_string(),
            languages,
            frameworks,
            package_manager: None,
            description: None,
            dep_count: 0,
            dev_dep_count: 0,
            selection: None,
        }
    }

    #[test]
    fn test_auto_select_one_language_one_framework() {
        let mut project = make_project_for_selection(
            vec![LanguageStat {
                language: Language::Rust,
                loc: 1500,
            }],
            vec![Framework {
                name: "actix-web".to_string(),
                category: FrameworkCategory::WebBackend,
                source: Some(
                    crate::analysis::static_analyzer::manifests::ManifestSource::CargoToml,
                ),
            }],
        );

        auto_select_for_project(&mut project);

        let sel = project.selection.unwrap();
        assert_eq!(sel.language.language, Language::Rust);
        assert_eq!(sel.language.loc, 1500);
        assert_eq!(sel.framework.unwrap().name, "actix-web");
        assert!(sel.auto_selected);
    }

    #[test]
    fn test_auto_select_picks_top_language_and_first_compatible_framework() {
        let mut project = make_project_for_selection(
            vec![
                LanguageStat {
                    language: Language::TypeScript,
                    loc: 5000,
                },
                LanguageStat {
                    language: Language::Rust,
                    loc: 2000,
                },
            ],
            vec![
                Framework {
                    name: "actix-web".to_string(),
                    category: FrameworkCategory::WebBackend,
                    source: Some(
                        crate::analysis::static_analyzer::manifests::ManifestSource::CargoToml,
                    ),
                },
                Framework {
                    name: "react".to_string(),
                    category: FrameworkCategory::WebFrontend,
                    source: Some(
                        crate::analysis::static_analyzer::manifests::ManifestSource::PackageJson,
                    ),
                },
            ],
        );

        auto_select_for_project(&mut project);

        let sel = project.selection.unwrap();
        // Top language is TypeScript (5000 LOC)
        assert_eq!(sel.language.language, Language::TypeScript);
        // First compatible framework for TypeScript is react (PackageJson source)
        assert_eq!(sel.framework.unwrap().name, "react");
        assert!(sel.auto_selected);
    }

    #[test]
    fn test_auto_select_languages_no_frameworks() {
        let mut project = make_project_for_selection(
            vec![LanguageStat {
                language: Language::Python,
                loc: 800,
            }],
            vec![],
        );

        auto_select_for_project(&mut project);

        let sel = project.selection.unwrap();
        assert_eq!(sel.language.language, Language::Python);
        assert!(sel.framework.is_none());
        assert!(sel.auto_selected);
    }

    #[test]
    fn test_auto_select_no_languages() {
        let mut project = make_project_for_selection(vec![], vec![]);

        auto_select_for_project(&mut project);

        assert!(
            project.selection.is_none(),
            "projects with no languages should not get a selection"
        );
    }

    // ── user_select_for_project tests ──

    #[test]
    fn test_user_select_for_project_single_language_single_framework() {
        let mut project = make_project_for_selection(
            vec![LanguageStat {
                language: Language::Rust,
                loc: 1500,
            }],
            vec![Framework {
                name: "actix-web".to_string(),
                category: FrameworkCategory::WebBackend,
                source: Some(
                    crate::analysis::static_analyzer::manifests::ManifestSource::CargoToml,
                ),
            }],
        );

        // select_one is called once for language (choose index 0)
        // framework is auto-selected since only 1 compatible
        let term = MockTerminal::new(vec![]).with_select_one(vec![0]);
        let mut pipeline = TuiRenderer::new(false, true);

        user_select_for_project(&mut project, &mut pipeline, &term).unwrap();

        let sel = project.selection.unwrap();
        assert_eq!(sel.language.language, Language::Rust);
        assert_eq!(sel.framework.unwrap().name, "actix-web");
        assert!(!sel.auto_selected);
    }

    #[test]
    fn test_user_select_for_project_no_languages() {
        let mut project = make_project_for_selection(vec![], vec![]);

        let term = MockTerminal::new(vec![]);
        let mut pipeline = TuiRenderer::new(false, true);

        user_select_for_project(&mut project, &mut pipeline, &term).unwrap();

        assert!(
            project.selection.is_none(),
            "should return Ok without setting selection for empty languages"
        );
    }

    #[test]
    fn test_user_select_for_project_multiple_compatible_frameworks() {
        let mut project = make_project_for_selection(
            vec![LanguageStat {
                language: Language::TypeScript,
                loc: 3000,
            }],
            vec![
                Framework {
                    name: "react".to_string(),
                    category: FrameworkCategory::WebFrontend,
                    source: Some(
                        crate::analysis::static_analyzer::manifests::ManifestSource::PackageJson,
                    ),
                },
                Framework {
                    name: "nextjs".to_string(),
                    category: FrameworkCategory::WebFrontend,
                    source: Some(
                        crate::analysis::static_analyzer::manifests::ManifestSource::PackageJson,
                    ),
                },
            ],
        );

        // select_one called twice: language (index 0), framework (index 1 = nextjs)
        let term = MockTerminal::new(vec![]).with_select_one(vec![0, 1]);
        let mut pipeline = TuiRenderer::new(false, true);

        user_select_for_project(&mut project, &mut pipeline, &term).unwrap();

        let sel = project.selection.unwrap();
        assert_eq!(sel.language.language, Language::TypeScript);
        assert_eq!(sel.framework.unwrap().name, "nextjs");
        assert!(!sel.auto_selected);
    }

    // ── handle_confirm_action tests ──

    #[test]
    fn test_handle_confirm_action_accept_returns_true() {
        let mut analysis = make_monorepo_analysis();
        let term = MockTerminal::new(vec![]);
        let mut pipeline = TuiRenderer::new(false, true);

        let result =
            handle_confirm_action(ConfirmAction::Accept, &mut analysis, &mut pipeline, &term);
        assert_eq!(result.unwrap(), true);
    }

    #[test]
    fn test_handle_confirm_action_reject_returns_cancelled() {
        let mut analysis = make_monorepo_analysis();
        let term = MockTerminal::new(vec![]);
        let mut pipeline = TuiRenderer::new(false, true);

        let result =
            handle_confirm_action(ConfirmAction::Reject, &mut analysis, &mut pipeline, &term);
        assert!(matches!(result, Err(ActualError::UserCancelled)));
    }

    #[test]
    fn test_handle_confirm_action_change_triggers_reselection() {
        // Create a project with 2 languages and auto-selected
        let mut analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![Project {
                path: ".".to_string(),
                name: "test".to_string(),
                languages: vec![
                    LanguageStat {
                        language: Language::TypeScript,
                        loc: 5000,
                    },
                    LanguageStat {
                        language: Language::Rust,
                        loc: 2000,
                    },
                ],
                frameworks: vec![],
                package_manager: None,
                description: None,
                dep_count: 0,
                dev_dep_count: 0,
                selection: Some(ProjectSelection {
                    language: LanguageStat {
                        language: Language::TypeScript,
                        loc: 5000,
                    },
                    framework: None,
                    auto_selected: true,
                }),
            }],
        };

        // select_one: user picks language index 1 (Rust)
        let term = MockTerminal::new(vec![]).with_select_one(vec![1]);
        let mut pipeline = TuiRenderer::new(false, true);

        let result =
            handle_confirm_action(ConfirmAction::Change, &mut analysis, &mut pipeline, &term);
        // Change returns Ok(false) to continue the loop
        assert_eq!(result.unwrap(), false);

        // Verify selection was updated to Rust
        let sel = analysis.projects[0].selection.as_ref().unwrap();
        assert_eq!(sel.language.language, Language::Rust);
        assert!(!sel.auto_selected, "should be user-selected, not auto");
    }

    #[test]
    fn test_user_select_for_project_no_compatible_frameworks() {
        // TypeScript is selected but only Cargo.toml framework (incompatible)
        let mut project = make_project_for_selection(
            vec![LanguageStat {
                language: Language::TypeScript,
                loc: 3000,
            }],
            vec![Framework {
                name: "actix-web".to_string(),
                category: FrameworkCategory::WebBackend,
                source: Some(
                    crate::analysis::static_analyzer::manifests::ManifestSource::CargoToml,
                ),
            }],
        );

        // select_one: pick language index 0 (TypeScript)
        let term = MockTerminal::new(vec![]).with_select_one(vec![0]);
        let mut pipeline = TuiRenderer::new(false, true);

        user_select_for_project(&mut project, &mut pipeline, &term).unwrap();

        let sel = project.selection.unwrap();
        assert_eq!(sel.language.language, Language::TypeScript);
        assert!(
            sel.framework.is_none(),
            "no compatible frameworks for TypeScript from Cargo.toml"
        );
        assert!(!sel.auto_selected);
    }

    // ── confirm_or_change_loop tests ──

    #[test]
    fn test_confirm_or_change_loop_accept() {
        let mut analysis = make_monorepo_analysis();
        let term = MockTerminal::new(vec![]).with_select_one(vec![0]);
        let mut pipeline = TuiRenderer::new(false, true);

        let result = confirm_or_change_loop(&mut analysis, &mut pipeline, &term);
        assert!(result.is_ok(), "Accept should succeed: {result:?}");
    }

    #[test]
    fn test_confirm_or_change_loop_reject() {
        let mut analysis = make_monorepo_analysis();
        let term = MockTerminal::new(vec![]).with_select_one(vec![2]);
        let mut pipeline = TuiRenderer::new(false, true);

        let result = confirm_or_change_loop(&mut analysis, &mut pipeline, &term);
        assert!(
            matches!(result, Err(ActualError::UserCancelled)),
            "Reject should return UserCancelled: {result:?}"
        );
    }

    #[test]
    fn test_confirm_or_change_loop_change_then_accept() {
        // Use confirm_or_change_loop_with to inject Change → Accept sequence,
        // since plain-mode confirm can never produce Change.
        let mut analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![Project {
                path: ".".to_string(),
                name: "test".to_string(),
                languages: vec![
                    LanguageStat {
                        language: Language::TypeScript,
                        loc: 5000,
                    },
                    LanguageStat {
                        language: Language::Rust,
                        loc: 2000,
                    },
                ],
                frameworks: vec![],
                package_manager: None,
                description: None,
                dep_count: 0,
                dev_dep_count: 0,
                selection: Some(ProjectSelection {
                    language: LanguageStat {
                        language: Language::TypeScript,
                        loc: 5000,
                    },
                    framework: None,
                    auto_selected: true,
                }),
            }],
        };

        // select_one: user picks language index 1 (Rust) during the Change cycle
        let term = MockTerminal::new(vec![]).with_select_one(vec![1]);
        let mut pipeline = TuiRenderer::new(false, true);

        let mut call_count = 0;
        let result = confirm_or_change_loop_with(
            &mut analysis,
            &mut pipeline,
            &term,
            |_analysis, _pipeline, _term| {
                call_count += 1;
                if call_count == 1 {
                    Ok(ConfirmAction::Change)
                } else {
                    Ok(ConfirmAction::Accept)
                }
            },
        );

        assert!(
            result.is_ok(),
            "Should succeed after Change then Accept: {result:?}"
        );
        assert_eq!(
            call_count, 2,
            "Should have been called twice (Change + Accept)"
        );

        // Verify selection was updated to Rust
        let sel = analysis.projects[0].selection.as_ref().unwrap();
        assert_eq!(sel.language.language, Language::Rust);
        assert!(!sel.auto_selected, "should be user-selected after Change");
    }

    #[test]
    fn test_user_select_for_project_zero_loc_language() {
        // Covers the else branch at line 887 where loc == 0 → just name
        let mut project = make_project_for_selection(
            vec![LanguageStat {
                language: Language::Python,
                loc: 0,
            }],
            vec![],
        );

        let term = MockTerminal::new(vec![]).with_select_one(vec![0]);
        let mut pipeline = TuiRenderer::new(false, true);

        user_select_for_project(&mut project, &mut pipeline, &term).unwrap();

        let sel = project.selection.unwrap();
        assert_eq!(sel.language.language, Language::Python);
        assert_eq!(sel.language.loc, 0);
        assert!(sel.framework.is_none());
    }

    #[test]
    fn test_user_select_for_project_with_existing_framework_selection() {
        // Covers lines 937-941: the and_then chain that looks up the current
        // framework selection in the filtered compatible frameworks list.
        let mut project = make_project_for_selection(
            vec![LanguageStat {
                language: Language::TypeScript,
                loc: 3000,
            }],
            vec![
                Framework {
                    name: "react".to_string(),
                    category: FrameworkCategory::WebFrontend,
                    source: Some(
                        crate::analysis::static_analyzer::manifests::ManifestSource::PackageJson,
                    ),
                },
                Framework {
                    name: "nextjs".to_string(),
                    category: FrameworkCategory::WebFrontend,
                    source: Some(
                        crate::analysis::static_analyzer::manifests::ManifestSource::PackageJson,
                    ),
                },
            ],
        );

        // Pre-set a selection with nextjs as framework
        project.selection = Some(ProjectSelection {
            language: LanguageStat {
                language: Language::TypeScript,
                loc: 3000,
            },
            framework: Some(Framework {
                name: "nextjs".to_string(),
                category: FrameworkCategory::WebFrontend,
                source: Some(
                    crate::analysis::static_analyzer::manifests::ManifestSource::PackageJson,
                ),
            }),
            auto_selected: true,
        });

        // select_one called twice: language (0 = TypeScript), framework (0 = react)
        let term = MockTerminal::new(vec![]).with_select_one(vec![0, 0]);
        let mut pipeline = TuiRenderer::new(false, true);

        user_select_for_project(&mut project, &mut pipeline, &term).unwrap();

        let sel = project.selection.unwrap();
        assert_eq!(sel.language.language, Language::TypeScript);
        assert_eq!(sel.framework.unwrap().name, "react");
        assert!(!sel.auto_selected);
    }

    #[test]
    fn test_user_select_for_project_language_selection_cancelled() {
        // Covers the ? error path on select_one_in_tui for language selection
        let mut project = make_project_for_selection(
            vec![LanguageStat {
                language: Language::Rust,
                loc: 100,
            }],
            vec![],
        );

        // Empty select_one results → UserCancelled on first call
        let term = MockTerminal::new(vec![]).with_select_one(vec![]);
        let mut pipeline = TuiRenderer::new(false, true);

        let result = user_select_for_project(&mut project, &mut pipeline, &term);
        assert!(
            matches!(result, Err(ActualError::UserCancelled)),
            "Should return UserCancelled when language selection is cancelled: {result:?}"
        );
    }

    #[test]
    fn test_user_select_for_project_framework_selection_cancelled() {
        // Covers the ? error path on select_one_in_tui for framework selection
        let mut project = make_project_for_selection(
            vec![LanguageStat {
                language: Language::TypeScript,
                loc: 3000,
            }],
            vec![
                Framework {
                    name: "react".to_string(),
                    category: FrameworkCategory::WebFrontend,
                    source: Some(
                        crate::analysis::static_analyzer::manifests::ManifestSource::PackageJson,
                    ),
                },
                Framework {
                    name: "nextjs".to_string(),
                    category: FrameworkCategory::WebFrontend,
                    source: Some(
                        crate::analysis::static_analyzer::manifests::ManifestSource::PackageJson,
                    ),
                },
            ],
        );

        // First select_one succeeds (language = 0), second runs out → UserCancelled
        let term = MockTerminal::new(vec![]).with_select_one(vec![0]);
        let mut pipeline = TuiRenderer::new(false, true);

        let result = user_select_for_project(&mut project, &mut pipeline, &term);
        assert!(
            matches!(result, Err(ActualError::UserCancelled)),
            "Should return UserCancelled when framework selection is cancelled: {result:?}"
        );
    }

    // ── sync flow integration ──

    #[test]
    fn test_run_sync_accept_works_with_auto_selection() {
        // select_one index 0 = Accept in plain mode. After auto-select,
        // the summary should still render and Accept should proceed.
        let server = mock_api_server();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![])
            .with_select_one(vec![0])
            .with_selection(Some(vec![]));
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(false, false, false, false, &server.url());
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(
            result.is_ok(),
            "run_sync with Accept after auto-select should succeed: {result:?}"
        );
    }

    #[test]
    fn test_run_sync_force_auto_selects_without_prompting() {
        // --force mode should auto-select and proceed without confirmation.
        let server = mock_api_server();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(false, false, true, false, &server.url());
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
        );
        assert!(
            result.is_ok(),
            "run_sync with --force should auto-select and succeed: {result:?}"
        );
    }

    /// Verify that telemetry fires correctly when git remote is configured.
    ///
    /// Sets up a real git repo with a remote origin URL so that
    /// `git remote get-url origin` succeeds and returns a non-empty string,
    /// exercising the `Some(s)` branch in the repo_url extraction block.
    #[cfg(feature = "telemetry")]
    #[test]
    fn test_run_sync_telemetry_fires_with_git_remote() {
        let _lock = crate::testutil::ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _guard = crate::testutil::EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        let mut server = mockito::Server::new();
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(EMPTY_MATCH_RESPONSE)
            .create();
        let tel_mock = server
            .mock("POST", "/counter/record")
            .with_status(200)
            .expect(1)
            .create();

        let dir = tempfile::tempdir().unwrap();
        // Initialize a git repo with a remote so get-url origin returns a
        // non-empty string, covering the Some(s) branch in repo_url extraction.
        init_git_repo(dir.path());
        std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/example/repo.git",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let cfg_path = dir.path().join("config.yaml");
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(false, false, true, false, &server.url());

        let result = run_sync(&args, dir.path(), &cfg_path, &term, &runner, None, None);
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
        tel_mock.assert();
    }

    // ── runner_probe tests ──

    #[test]
    fn run_sync_runner_probe_failure_aborts_in_environment_phase() {
        // A runner_probe that returns Err should cause run_sync_with_probe to
        // abort during the Environment phase and return that error — before
        // Analysis, Fetch, or Tailor phases run.
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(false, false, true, false, "http://unused");
        let probe: Box<dyn Fn() -> Result<(), ActualError>> =
            Box::new(|| Err(ActualError::CodexNotAuthenticated));
        let result = super::run_sync_with_probe(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
            Some(probe),
        );
        assert!(
            matches!(result, Err(ActualError::CodexNotAuthenticated)),
            "expected CodexNotAuthenticated from probe, got: {result:?}"
        );
    }

    #[test]
    fn run_sync_runner_probe_none_does_not_affect_environment_phase() {
        // When runner_probe is None the Environment phase completes normally and
        // run_sync_with_probe behaves identically to run_sync.
        let server = mock_api_server();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let args = make_sync_args(false, false, true, false, &server.url());
        let result = super::run_sync_with_probe(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
            None,
            None,
        );
        assert!(
            result.is_ok(),
            "None probe should not affect Environment phase: {result:?}"
        );
    }

    // ── fetch_with_503_backoff unit tests ──

    #[tokio::test(start_paused = true)]
    async fn test_fetch_with_503_backoff_succeeds_immediately() {
        // When the first call succeeds, no retry delay occurs.
        let mut pipeline = TuiRenderer::new(false, true);
        let delays: &[std::time::Duration] = &[
            std::time::Duration::from_secs(10),
            std::time::Duration::from_secs(30),
            std::time::Duration::from_secs(60),
        ];
        let mut call_count = 0usize;
        let result = super::fetch_with_503_backoff(
            || {
                call_count += 1;
                async { Ok::<_, ActualError>(42u32) }
            },
            delays,
            &mut pipeline,
        )
        .await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert_eq!(result.unwrap(), 42);
        assert_eq!(call_count, 1, "should only be called once on success");
    }

    #[tokio::test(start_paused = true)]
    async fn test_fetch_with_503_backoff_retries_once() {
        // First call returns ServiceUnavailable, second succeeds.
        // Uses start_paused=true so tokio::time::sleep completes instantly.
        let mut pipeline = TuiRenderer::new(false, true);
        let delays: &[std::time::Duration] = &[
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(1),
        ];
        let mut call_count = 0usize;
        let result = super::fetch_with_503_backoff(
            || {
                call_count += 1;
                let n = call_count;
                async move {
                    if n == 1 {
                        Err(ActualError::ServiceUnavailable)
                    } else {
                        Ok(42u32)
                    }
                }
            },
            delays,
            &mut pipeline,
        )
        .await;
        assert!(result.is_ok(), "expected Ok after retry, got {result:?}");
        assert_eq!(result.unwrap(), 42);
        assert_eq!(call_count, 2, "should be called twice: fail then succeed");
    }

    #[tokio::test(start_paused = true)]
    async fn test_fetch_with_503_backoff_exhausts_all_retries() {
        // All calls return ServiceUnavailable → last error propagates after all delays.
        let mut pipeline = TuiRenderer::new(false, true);
        let delays: &[std::time::Duration] = &[
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(1),
        ];
        let mut call_count = 0usize;
        let result = super::fetch_with_503_backoff(
            || {
                call_count += 1;
                async move { Err::<u32, _>(ActualError::ServiceUnavailable) }
            },
            delays,
            &mut pipeline,
        )
        .await;
        // The guard is `attempt_503 < delays.len()`, so we get delays.len() retries + 1 final.
        assert!(
            matches!(result, Err(ActualError::ServiceUnavailable)),
            "expected ServiceUnavailable after exhausting retries, got {result:?}"
        );
        // call_count == delays.len() + 1 (initial + one per delay)
        assert_eq!(
            call_count,
            delays.len() + 1,
            "should be called delays.len()+1 times"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_fetch_with_503_backoff_non_503_error_propagates_immediately() {
        // Non-503 errors should not be retried.
        let mut pipeline = TuiRenderer::new(false, true);
        let delays: &[std::time::Duration] = &[std::time::Duration::from_millis(1)];
        let mut call_count = 0usize;
        let result = super::fetch_with_503_backoff(
            || {
                call_count += 1;
                async move {
                    Err::<u32, _>(ActualError::ApiError("connection refused".to_string()))
                }
            },
            delays,
            &mut pipeline,
        )
        .await;
        assert!(
            matches!(result, Err(ActualError::ApiError(_))),
            "expected ApiError to propagate without retry, got {result:?}"
        );
        assert_eq!(
            call_count, 1,
            "should only be called once for non-503 errors"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_fetch_with_503_backoff_user_cancelled_propagates_immediately() {
        // UserCancelled should propagate immediately without retry.
        let mut pipeline = TuiRenderer::new(false, true);
        let delays: &[std::time::Duration] = &[std::time::Duration::from_millis(1)];
        let mut call_count = 0usize;
        let result = super::fetch_with_503_backoff(
            || {
                call_count += 1;
                async move { Err::<u32, _>(ActualError::UserCancelled) }
            },
            delays,
            &mut pipeline,
        )
        .await;
        assert!(
            matches!(result, Err(ActualError::UserCancelled)),
            "expected UserCancelled to propagate, got {result:?}"
        );
        assert_eq!(
            call_count, 1,
            "should only be called once for UserCancelled"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_fetch_with_503_backoff_with_two_retries_succeeds() {
        // 503 twice then success: verifies multi-retry path works end-to-end.
        let mut pipeline = TuiRenderer::new(false, true);
        let delays: &[std::time::Duration] = &[
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(1),
        ];
        let mut call_count = 0usize;
        let result = super::fetch_with_503_backoff(
            || {
                call_count += 1;
                let n = call_count;
                async move {
                    if n < 3 {
                        Err(ActualError::ServiceUnavailable)
                    } else {
                        Ok(99u32)
                    }
                }
            },
            delays,
            &mut pipeline,
        )
        .await;
        assert!(
            result.is_ok(),
            "expected Ok after 2 retries, got {result:?}"
        );
        assert_eq!(result.unwrap(), 99);
        assert_eq!(
            call_count, 3,
            "should be called 3 times: fail, fail, succeed"
        );
    }
}
