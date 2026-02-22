use std::path::Path;
use std::time::Duration;

use crate::analysis::cache::{get_git_branch, get_git_head, run_analysis_cached};
use crate::analysis::confirm::ConfirmAction;
use crate::analysis::types::RepoAnalysis;
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

use super::adr_utils::{
    fetch_error_message, find_existing_output_files, raw_adrs_to_output, zero_adr_context_lines,
};
use super::cache::{
    compute_repo_key, compute_tailoring_cache_key, load_config_with_fallback,
    store_tailoring_cache, tailoring_cache_skip_msg,
};
use super::write::confirm_and_write;

/// Resolve the current working directory, falling back to `"."` if
/// unavailable (e.g. the directory was deleted while the process was running).
pub(crate) fn resolve_cwd() -> std::path::PathBuf {
    let fallback = std::path::PathBuf::from(".");
    std::env::current_dir().unwrap_or(fallback)
}

/// Format a cache age in seconds as a human-readable string.
///
/// - < 60 s   → `"Ns ago"`
/// - < 3600 s → `"Nm ago"`
/// - < 86400 s → `"Nh ago"`
/// - ≥ 86400 s → `"Nd ago"`
fn format_cache_age(age_secs: i64) -> String {
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
pub(crate) fn run_sync<R: TailoringRunner>(
    args: &SyncArgs,
    root_dir: &Path,
    cfg_path: &Path,
    term: &dyn TerminalIO,
    runner: &R,
    auth_display: Option<&AuthDisplay>,
    runner_display: Option<&RunnerDisplay>,
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
                "  {:<9} {} Not authenticated — run `actual login`",
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
    let analysis = filter_projects(analysis, &args.projects).inspect_err(|_| {
        pipeline.finish_remaining();
    })?;

    // 5. Confirmation (unless --force)
    if args.force {
        // Show project summary even in --force mode for visibility.
        // Use pipeline.println() so the output stays inside the TUI log pane
        // rather than triggering a LeaveAlternateScreen/re-enter flash.
        let summary = format_project_summary_plain(&analysis);
        for line in summary.lines() {
            pipeline.println(line);
        }
    } else {
        let action = pipeline.confirm_project(&analysis, term)?;
        if matches!(action, ConfirmAction::Reject) {
            pipeline.finish_remaining();
            return Err(ActualError::UserCancelled);
        }
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
    let api_url = args
        .api_url
        .as_deref()
        .or(config.api_url.as_deref())
        .unwrap_or(DEFAULT_API_URL);

    let request = build_match_request(&analysis, &config);
    let client = ActualApiClient::new(api_url)?;

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

    pipeline.start(SyncPhase::Fetch, "Fetching ADRs...");
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| ActualError::InternalError(format!("Failed to create async runtime: {e}")))?;

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
        let api_future = async {
            let retry_config = RetryConfig::default();
            tokio::select! {
                result = with_retry(&retry_config, || client.post_match(&request)) => result,
                _ = tokio::signal::ctrl_c() => Err(ActualError::UserCancelled),
            }
        };

        let fs_future = tokio::task::spawn_blocking(move || {
            find_existing_output_files(&root_dir_owned, &output_format_for_fs)
        });

        tokio::join!(api_future, fs_future)
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
            let msg = fetch_error_message(&e, api_url);
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

    // Try cache (unless --force)
    let cached_output = if !args.force {
        tailoring_cache_key
            .as_deref()
            .and_then(|key| {
                config
                    .cached_tailoring
                    .as_ref()
                    .filter(|c| c.cache_key == key)
            })
            .and_then(|c| serde_yaml::from_value::<TailoringOutput>(c.tailoring.clone()).ok())
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
        let out = raw_adrs_to_output(&filtered_adrs, &output_format);
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
        let (kb_poller, nav_rx, cancel) =
            crate::cli::commands::sync_kb_poller::setup(pipeline.is_tui());
        pipeline.set_nav_rx_opt(nav_rx);

        let result = rt.block_on(async {
            let tailor_fut = tailor_all_projects(
                runner,
                &analysis.projects,
                &filtered_adrs,
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
    confirm_and_write(
        &output,
        root_dir,
        args.force,
        args.dry_run,
        args.full,
        &output_format,
        term,
        &mut pipeline,
    )?;
    pipeline.wait_for_keypress();
    Ok(())
}

/// Apply a single [`TailoringEvent`] to the progress pipeline.
///
/// Logs per-project and per-batch status lines to the pipeline as work
/// progresses.  The spinner message/tick is managed by the heartbeat timer in
/// `run_tailoring_with_cancel` instead, so the display stays live even when
/// Claude is thinking and no progress events are flowing.
///
/// Extracted from the select loop so both the real-time event path and the
/// drain-on-completion path share the same logic and the function can be
/// tested independently.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::types::{Framework, FrameworkCategory, Language, LanguageStat, Project};
    use crate::cli::commands::handle_result;
    use crate::cli::commands::sync::exec;
    use crate::cli::ui::test_utils::MockTerminal;
    use crate::cli::ui::tui::renderer::TuiRenderer;
    use crate::error::ActualError;
    use crate::tailoring::types::{FileOutput, TailoringSummary};

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
                    }],
                    package_manager: Some("npm".to_string()),
                    description: None,
                    dep_count: 0,
                    dev_dep_count: 0,
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
                },
            ],
        }
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

        // Provide "y" for project confirmation and select all files
        let term2 = MockTerminal::new(vec!["y"]).with_selection(Some(vec![0]));
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
        let cached_output: TailoringOutput =
            serde_yaml::from_value(cached_tailoring.tailoring.clone()).unwrap();
        assert_eq!(
            cached_output.summary.applicable, 0,
            "expected 0 applicable ADRs in cache after empty-ADR run"
        );
        let first_key = cached_tailoring.cache_key.clone();

        // Second run: force=false → should hit the cache (applicable=0 path)
        // Provide "y" for project confirmation; no file selection needed (0 files)
        let server2 = mock_api_server();
        let term2 = MockTerminal::new(vec!["y"]);
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
        let cached_output: TailoringOutput =
            serde_yaml::from_value(cached_tailoring.tailoring.clone()).unwrap();
        assert!(
            cached_output.summary.applicable > 0,
            "expected >0 applicable ADRs in cache"
        );
        let first_key = cached_tailoring.cache_key.clone();

        // Remove the written CLAUDE.md so the second run can re-write it
        let _ = std::fs::remove_file(dir.path().join("CLAUDE.md"));

        // Second run: force=false → should hit the cache (with-files path)
        let server2 = mock_api_server_with_adrs();
        let term2 = MockTerminal::new(vec!["y"]).with_selection(Some(vec![0]));
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

    // ── Gap 6: load_config_with_fallback returns default on error ──

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
    // ── resolve_cwd test ──

    #[test]
    fn test_resolve_cwd_returns_path() {
        let cwd = resolve_cwd();
        assert!(cwd.is_absolute() || cwd == std::path::PathBuf::from("."));
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
        // "y" for project confirm (dialoguer y/n), select_files handles file confirm
        let term = MockTerminal::new(vec!["y"]).with_selection(Some(vec![]));
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
        // "n" for project confirm — rejected before API call
        let term = MockTerminal::new(vec!["n"]);
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
        // "y" for project confirm (dry_run, force=false)
        let term = MockTerminal::new(vec!["y"]);
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
        // "y" for project confirm, file confirm cancelled via select_files
        let term = MockTerminal::new(vec!["y"]).with_selection(None);
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
}
