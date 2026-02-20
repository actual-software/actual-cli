use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::analysis::cache::{get_git_branch, get_git_head, run_analysis_cached};
use crate::analysis::confirm::ConfirmAction;
use crate::analysis::types::RepoAnalysis;
use crate::api::client::{build_match_request, ActualApiClient, DEFAULT_API_URL};
use crate::api::retry::{with_retry, RetryConfig};
use crate::cli::args::SyncArgs;
use crate::cli::ui::confirm::format_project_summary;
use crate::cli::ui::diff::{format_diff_summary, FileDiff};
use crate::cli::ui::file_confirm::confirm_files;
use crate::cli::ui::header::AuthDisplay;
use crate::cli::ui::panel::Panel;
use crate::cli::ui::progress::SyncPhase;
use crate::cli::ui::term_size;
use crate::cli::ui::terminal::TerminalIO;
use crate::cli::ui::theme;
use crate::cli::ui::tui::renderer::TuiRenderer;
use crate::config::paths::{load_from, save_to};
use crate::config::rejections::{clear_rejections, get_rejections};
use crate::config::types::{
    CachedTailoring, DEFAULT_BATCH_SIZE, DEFAULT_CONCURRENCY, DEFAULT_TIMEOUT_SECS,
};
use crate::error::ActualError;
use crate::generation::markers;
use crate::generation::writer::{write_files, WriteAction, WriteResult};
use crate::generation::OutputFormat;
use crate::runner::subprocess::TailoringRunner;
use crate::tailoring::concurrent::{tailor_all_projects, ConcurrentTailoringConfig};
use crate::tailoring::filter::pre_filter_rejected;
use crate::tailoring::types::{FileOutput, TailoringEvent, TailoringOutput, TailoringSummary};

/// Result summary from the confirm + write phase.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncResult {
    pub files_created: usize,
    pub files_updated: usize,
    pub files_failed: usize,
    pub files_rejected: usize,
}

/// Entry point for `actual sync`.
///
/// Production wiring (`CliClaudeRunner`, `RealTerminal`) lives in
/// `sync_wiring.rs` (which is excluded from coverage) so that the only
/// generic instantiation of `run_sync` in `sync.rs` is `MockRunner` from
/// unit tests.
pub fn exec(args: &SyncArgs) -> Result<(), ActualError> {
    super::sync_wiring::sync_run(args)
}

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
pub(crate) fn run_sync<R: TailoringRunner>(
    args: &SyncArgs,
    root_dir: &Path,
    cfg_path: &Path,
    term: &dyn TerminalIO,
    runner: &R,
    auth_display: Option<&AuthDisplay>,
) -> Result<(), ActualError> {
    // ── Phase 1: env check + analysis ──

    // 1. Create pipeline. The banner and version are shown in the left-column
    //    banner box (rendered by the TUI directly), not in the log pane.
    let mut pipeline = TuiRenderer::new_with_version(false, args.no_tui, env!("CARGO_PKG_VERSION"));

    pipeline.start(SyncPhase::Environment, "Checking environment...");

    // Load config early so we can show server URL and cache status in the
    // Environment step output. Falls back to default if the config is missing
    // or unreadable (non-fatal — errors surface later when config is required).
    let config = load_from(cfg_path).unwrap_or_default();

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
        Ok(a) => {
            pipeline.success(SyncPhase::Analysis, "Analysis complete");
            a
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
        let summary_width = term_size::terminal_width();
        let summary = format_project_summary(&analysis, summary_width);
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
        args.model.as_deref(),
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
        let tailoring_config = ConcurrentTailoringConfig::new(
            config.concurrency.unwrap_or(DEFAULT_CONCURRENCY),
            config.batch_size.unwrap_or(DEFAULT_BATCH_SIZE),
            &existing_paths,
            args.model.as_deref(),
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

        // In TUI mode (raw mode active) the terminal consumes keyboard input,
        // so OS-level SIGINT from Ctrl+C may not reach the process reliably.
        // `sync_kb_poller::setup` optionally spawns a background poller thread
        // and returns a combined OS-signal + keyboard cancel future.
        let (kb_poller, cancel) = super::sync_kb_poller::setup(pipeline.is_tui());

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
                pipeline.finish_remaining();
                return Err(e);
            }
        }
    };

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

/// Return the pipeline skip message for a tailoring cache hit.
///
/// Distinguishes between a cached result with no applicable ADRs (deterministic
/// empty output) and one with applicable ADRs, so users can tell at a glance
/// why tailoring was skipped.
fn tailoring_cache_skip_msg(applicable: usize) -> &'static str {
    if applicable == 0 {
        "No ADRs applicable to this project (cached)"
    } else {
        "Using cached tailoring result"
    }
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
        TailoringEvent::BatchCompleted {
            ref project_name,
            batch_index,
            batch_count,
            adr_count,
        } => {
            let adr_label = if adr_count == 1 { "ADR" } else { "ADRs" };
            pipeline.println(&format!(
                "    \u{2714} {project_name}: batch {batch_index}/{batch_count} ({adr_count} {adr_label})"
            ));
        }
        TailoringEvent::ProjectCompleted {
            ref project_name,
            files_generated,
            ..
        } => {
            *completed += 1;
            let file_label = if files_generated == 1 {
                "file"
            } else {
                "files"
            };
            pipeline.println(&format!(
                "  \u{2714} {project_name} done \u{2192} {files_generated} {file_label}"
            ));
        }
        TailoringEvent::ProjectFailed {
            ref project_name,
            ref error,
        } => {
            pipeline.println(&format!("  \u{2716} {project_name}: {error}"));
        }
    }
}

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
async fn run_tailoring_with_cancel<F, C>(
    tailor_fut: F,
    progress_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TailoringEvent>,
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

    loop {
        tokio::select! {
            result = &mut tailor_fut => {
                // Drain any remaining events before breaking
                while let Ok(event) = progress_rx.try_recv() {
                    apply_tailoring_event(event, pipeline, &mut completed);
                }
                break result;
            }
            Some(event) = progress_rx.recv() => {
                apply_tailoring_event(event, pipeline, &mut completed);
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

/// Compute a stable repo key by hashing the git origin URL.
///
/// Falls back to hashing the root directory path if not a git repo or
/// if the remote URL cannot be determined. Applies a 5-second timeout to
/// the git subprocess; on timeout, silently falls back to the path hash.
fn compute_repo_key(root_dir: &Path) -> String {
    compute_repo_key_with_timeout(root_dir, std::time::Duration::from_secs(5))
}

fn compute_repo_key_with_timeout(root_dir: &Path, timeout: std::time::Duration) -> String {
    let root_dir_buf = root_dir.to_path_buf();

    // Use tokio::process::Command with kill_on_drop(true) so that when the
    // timeout fires the child is sent SIGKILL rather than left as an orphan.
    // A single-threaded runtime is used to keep this function synchronous.
    let url = (|| -> Option<String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;

        rt.block_on(async move {
            let mut cmd = tokio::process::Command::new("git");
            cmd.args(["remote", "get-url", "origin"]);
            cmd.current_dir(&root_dir_buf);
            cmd.stdin(std::process::Stdio::null());
            cmd.kill_on_drop(true);

            let child = cmd
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .ok()?;

            let result = tokio::time::timeout(timeout, child.wait_with_output()).await;

            match result {
                Ok(Ok(output)) if output.status.success() => {
                    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if s.is_empty() {
                        None
                    } else {
                        Some(s)
                    }
                }
                _ => None,
            }
        })
    })();

    let input = url.unwrap_or_else(|| root_dir.to_string_lossy().to_string());

    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Serialize a value to `serde_yaml::Value`, logging a warning and returning
/// `None` if serialization fails.
///
/// In practice `TailoringOutput` contains only string/number/vec fields and
/// serialization never fails.  The `Option` return defends against a future
/// type change that adds a non-YAML-representable field (e.g. `f64::NAN`).
fn serialize_tailoring_output<T: serde::Serialize>(output: &T) -> Option<serde_yaml::Value> {
    match serde_yaml::to_value(output) {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("warning: failed to serialize tailoring cache: {e}");
            None
        }
    }
}

/// Store a tailoring result in the config cache (best-effort).
///
/// Skips caching when `cache_key` is `None`.
/// Disk persistence failures are silently ignored because the cache
/// is an optimisation — a write failure should never abort a sync.
///
/// The output is generic over any `serde::Serialize` type so that tests can
/// inject a type that always fails serialization to exercise the warning path.
/// In production the concrete type is always `TailoringOutput`.
fn store_tailoring_cache<T: serde::Serialize>(
    config: &mut crate::config::types::Config,
    cfg_path: &Path,
    cache_key: Option<&str>,
    root_dir: &Path,
    output: &T,
) {
    let Some(key) = cache_key else {
        return;
    };
    let Some(tailoring_value) = serialize_tailoring_output(output) else {
        return;
    };
    config.cached_tailoring = Some(CachedTailoring {
        cache_key: key.to_string(),
        repo_path: root_dir.to_string_lossy().to_string(),
        tailoring: tailoring_value,
        tailored_at: chrono::Utc::now(),
    });
    // Best-effort persist: if disk write fails, the in-memory cache is
    // still populated for subsequent operations in this process.
    if let Err(e) = save_to(config, cfg_path) {
        eprintln!("warning: failed to save tailoring cache: {e}");
    }
}

/// Compute a cache key for the tailoring step by hashing all inputs
/// that affect the tailoring output.
///
/// The key incorporates: full ADR content (sorted by ID for determinism),
/// the no_tailor flag, sorted project paths, existing CLAUDE.md content,
/// and model override. Any change in these inputs produces a different cache
/// key. The git HEAD commit is intentionally excluded so that commits
/// unrelated to ADR content or repo structure do not invalidate the cache.
fn compute_tailoring_cache_key(
    adrs: &[crate::api::types::Adr],
    no_tailor: bool,
    project_paths: &[String],
    existing_claude_md: &str,
    model: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();

    // Sort ADRs by ID for determinism regardless of iteration order.
    let mut sorted_adrs: Vec<&crate::api::types::Adr> = adrs.iter().collect();
    sorted_adrs.sort_by(|a, b| a.id.cmp(&b.id));

    // Field-boundary bytes used below:
    //   \x00 — value separator within a list element (prevents "ab"+"c" == "a"+"bc")
    //   \xfe — list-field separator within an ADR (prevents policies/instructions
    //           ambiguity: ["a"]+[] == []+["a"] without a separator)
    //   \xff — ADR separator between successive ADRs in the outer loop (prevents
    //           ["a","b"]+["c"] == ["a"]+["b","c"] across ADR boundaries)
    for adr in sorted_adrs {
        hasher.update(adr.id.as_bytes());
        hasher.update(b"\x00");
        hasher.update(adr.title.as_bytes());
        hasher.update(b"\x00");
        hasher.update(adr.context.as_deref().unwrap_or("").as_bytes());
        hasher.update(b"\x00");

        let mut policies = adr.policies.clone();
        policies.sort();
        for policy in &policies {
            hasher.update(policy.as_bytes());
            hasher.update(b"\x00");
        }
        hasher.update(b"\xfe"); // end of policies list

        let mut instructions = adr.instructions.clone().unwrap_or_default();
        instructions.sort();
        for instruction in &instructions {
            hasher.update(instruction.as_bytes());
            hasher.update(b"\x00");
        }
        hasher.update(b"\xfe"); // end of instructions list

        // AdrCategory: hash its id, name, and path fields.
        hasher.update(adr.category.id.as_bytes());
        hasher.update(b"\x00");
        hasher.update(adr.category.name.as_bytes());
        hasher.update(b"\x00");
        hasher.update(adr.category.path.as_bytes());
        hasher.update(b"\x00");

        // AppliesTo: hash sorted languages and frameworks.
        let mut languages = adr.applies_to.languages.clone();
        languages.sort();
        for lang in &languages {
            hasher.update(lang.as_bytes());
            hasher.update(b"\x00");
        }
        hasher.update(b"\xfe"); // end of languages list

        let mut frameworks = adr.applies_to.frameworks.clone();
        frameworks.sort();
        for fw in &frameworks {
            hasher.update(fw.as_bytes());
            hasher.update(b"\x00");
        }
        hasher.update(b"\xfe"); // end of frameworks list

        // matched_projects (sorted).
        let mut matched = adr.matched_projects.clone();
        matched.sort();
        for mp in &matched {
            hasher.update(mp.as_bytes());
            hasher.update(b"\x00");
        }
        hasher.update(b"\xfe"); // end of matched_projects list
                                // ADR boundary marker: prevents two distinct ADR sets from producing
                                // the same byte stream (e.g. ["a", "b"] + ["c"] vs ["a"] + ["b", "c"]).
        hasher.update(b"\xff");
    }

    hasher.update(if no_tailor {
        &b"no_tailor"[..]
    } else {
        &b"tailor"[..]
    });
    hasher.update(b"\x00");
    for path in project_paths {
        hasher.update(path.as_bytes());
        hasher.update(b"\x00");
    }
    hasher.update(existing_claude_md.as_bytes());
    hasher.update(b"\x00");
    if let Some(m) = model {
        hasher.update(m.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Produce contextual follow-up lines for a zero-ADR fetch result.
///
/// Examines the languages and frameworks in the match request and returns one
/// or more indented lines explaining *why* no ADRs were matched:
///
/// - No languages detected → indicates the repo language is not recognized.
/// - Languages but no frameworks → only general ADRs were eligible.
/// - Languages + frameworks → the combination has no ADR coverage yet.
fn zero_adr_context_lines(request: &crate::api::types::MatchRequest) -> Vec<String> {
    let mut langs: Vec<&str> = request
        .projects
        .iter()
        .flat_map(|p| p.languages.iter().map(|l| l.as_str()))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    langs.sort_unstable();

    let mut fws: Vec<&str> = request
        .projects
        .iter()
        .flat_map(|p| p.frameworks.iter().map(|f| f.name.as_str()))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    fws.sort_unstable();

    if langs.is_empty() {
        vec!["  No supported languages detected \u{2014} unable to match ADRs".to_string()]
    } else if fws.is_empty() {
        vec![
            format!("  Languages: {}", langs.join(", ")),
            "  No frameworks detected; only general ADRs were eligible".to_string(),
        ]
    } else {
        vec![
            format!("  Languages: {}", langs.join(", ")),
            format!("  Frameworks: {}", fws.join(", ")),
            "  No ADRs available for this stack yet".to_string(),
        ]
    }
}

/// Produce a user-friendly error message for an ADR fetch failure.
///
/// Distinguishes connection failures, timeouts, structured API errors (4xx/5xx),
/// and falls back to a raw message for unrecognized errors.
fn fetch_error_message(e: &ActualError, api_url: &str) -> String {
    match e {
        ActualError::ApiError(s)
            if s.contains("error trying to connect")
                || s.contains("Connection refused")
                || s.contains("connection refused")
                || s.contains("dns error") =>
        {
            format!(
                "Unable to connect to Actual AI ({api_url}) \u{2014} check your network connection"
            )
        }
        ActualError::ApiError(s)
            if s.contains("timed out") || s.contains("operation timed out") =>
        {
            "Connection to Actual AI timed out \u{2014} try again later".to_string()
        }
        ActualError::ApiResponseError { code, message } => {
            format!("Actual AI returned an error ({code}): {message}")
        }
        _ => format!("API request failed: {e}"),
    }
}

/// Strip non-printable ASCII control characters from server-controlled ADR content.
///
/// Keeps newlines (`\n`), carriage returns (`\r`), and tabs (`\t`) as they are
/// legitimate in markdown, but removes other control characters (bytes < 0x20
/// and DEL 0x7F) that could be used for terminal manipulation.
fn strip_control_chars(s: &str) -> String {
    s.chars()
        .filter(|&c| c >= ' ' || c == '\n' || c == '\r' || c == '\t')
        .filter(|&c| c != '\x7f')
        .collect()
}

/// Validate a server-controlled project path before using it to construct output file paths.
///
/// Rejects paths that:
/// - Contain `..` components (path traversal)
/// - Start with `/` or `\` (absolute paths)
/// - Contain null bytes
fn validate_project_path(p: &str) -> Result<(), ActualError> {
    if p.contains('\0') {
        return Err(ActualError::InternalError(format!(
            "project path '{p}' contains null bytes"
        )));
    }
    if p.starts_with('/') || p.starts_with('\\') {
        return Err(ActualError::InternalError(format!(
            "project path '{p}' must be a relative path"
        )));
    }
    if std::path::Path::new(p)
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        return Err(ActualError::InternalError(format!(
            "project path '{p}' contains path traversal components"
        )));
    }
    Ok(())
}

/// Convert raw ADRs into a [`TailoringOutput`] without Claude tailoring.
///
/// Groups ADRs by their `matched_projects` field. Each group produces a
/// `FileOutput` whose content is the ADR policies/instructions formatted as
/// markdown. ADRs with no `matched_projects` are assigned to the root output
/// file (e.g. `CLAUDE.md` or `AGENTS.md` depending on `format`).
fn raw_adrs_to_output(adrs: &[crate::api::types::Adr], format: &OutputFormat) -> TailoringOutput {
    let filename = format.filename();
    let mut project_adrs: HashMap<String, Vec<&crate::api::types::Adr>> = HashMap::new();

    for adr in adrs {
        if adr.matched_projects.is_empty() {
            project_adrs
                .entry(filename.to_string())
                .or_default()
                .push(adr);
        } else {
            for project_path in &adr.matched_projects {
                if validate_project_path(project_path).is_err() {
                    eprintln!(
                        "  warning: skipping invalid project path '{}' for ADR '{}'",
                        console::strip_ansi_codes(project_path),
                        adr.id
                    );
                    continue;
                }
                let file_path = if project_path == "." {
                    filename.to_string()
                } else {
                    format!("{project_path}/{filename}")
                };
                project_adrs.entry(file_path).or_default().push(adr);
            }
        }
    }

    let mut files: Vec<FileOutput> = project_adrs
        .into_iter()
        .map(|(path, adrs_for_file)| {
            let mut content_parts = Vec::new();
            let mut adr_ids = Vec::new();

            for adr in &adrs_for_file {
                adr_ids.push(adr.id.clone());
                let safe_title = strip_control_chars(&adr.title);
                let mut section = format!("## {safe_title}\n");
                for policy in &adr.policies {
                    let safe_policy = strip_control_chars(policy);
                    section.push_str(&format!("- {safe_policy}\n"));
                }
                if let Some(instructions) = &adr.instructions {
                    if !instructions.is_empty() {
                        section.push('\n');
                        for instruction in instructions {
                            let safe_instruction = strip_control_chars(instruction);
                            section.push_str(&format!("- {safe_instruction}\n"));
                        }
                    }
                }
                content_parts.push(section);
            }

            FileOutput {
                path,
                content: content_parts.join("\n"),
                reasoning: "Raw ADR output (--no-tailor mode)".to_string(),
                adr_ids,
            }
        })
        .collect();

    // Sort for deterministic output
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let applicable = adrs.len();
    let files_generated = files.len();

    TailoringOutput {
        files,
        skipped_adrs: vec![],
        summary: TailoringSummary {
            total_input: applicable,
            applicable,
            not_applicable: 0,
            files_generated,
        },
    }
}

/// Scan the repository for existing output files (CLAUDE.md or AGENTS.md depending on format)
/// and return their contents concatenated as a string, for use as context during tailoring.
fn find_existing_output_files(root_dir: &Path, format: &OutputFormat) -> String {
    let files = super::find_output_files(root_dir, format);
    // `find_output_files` returns paths in sorted order, and `filter_map`
    // preserves that order, so no additional sort is needed here.
    let results: Vec<String> = files
        .iter()
        .filter_map(|path| {
            let content = std::fs::read_to_string(path).ok()?;
            let rel = path
                .strip_prefix(root_dir)
                .unwrap_or(path)
                .to_string_lossy();
            let cleaned = markers::strip_managed_metadata(&content);
            Some(format!("=== {rel} ===\n{cleaned}"))
        })
        .collect();
    results.join("\n\n")
}

/// Execute the confirm + write phase of the sync pipeline.
///
/// Given a `TailoringOutput` from the fetch+tailor phase, this function:
/// 1. Computes per-file diffs against existing output files
/// 2. Displays a diff summary
/// 3. In dry-run mode: prints summary (and full content if --full), returns
/// 4. In normal mode: runs the confirmation flow (or skips with --force)
/// 5. Writes confirmed files to disk
/// 6. Reports per-file results (created/updated/failed)
///
/// The `format` parameter controls the header prepended to new root-level files.
///
/// Write errors on individual files do NOT abort the batch.
#[allow(clippy::too_many_arguments)]
pub fn confirm_and_write(
    output: &TailoringOutput,
    root_dir: &Path,
    force: bool,
    dry_run: bool,
    full: bool,
    format: &OutputFormat,
    term: &dyn TerminalIO,
    pipeline: &mut TuiRenderer,
) -> Result<SyncResult, ActualError> {
    // Step 1: Compute diffs
    let diffs: Vec<FileDiff> = output
        .files
        .iter()
        .map(|file| {
            let full_path = root_dir.join(&file.path);
            let existing_content = std::fs::read_to_string(&full_path).ok();
            let is_new_file = existing_content.is_none();
            let detection = markers::detect_changes(existing_content.as_deref(), &file.adr_ids);

            // Extract old managed content stripped of metadata for text diffing.
            let old_managed = existing_content
                .as_deref()
                .and_then(markers::extract_managed_content)
                .map(markers::strip_managed_metadata);

            FileDiff::from_change_detection(
                &file.path,
                &detection,
                is_new_file,
                old_managed,
                file.content.clone(),
            )
        })
        .collect();

    // Step 2: Display diff summary
    let summary = format_diff_summary(&diffs);
    if !summary.is_empty() {
        for line in summary.lines() {
            pipeline.println(line);
        }
    }

    // Step 3: Handle --dry-run
    if dry_run {
        if full {
            for file in &output.files {
                let safe_path = console::strip_ansi_codes(&file.path);
                let safe_content = console::strip_ansi_codes(&file.content);
                pipeline.println(&format!("── {safe_path} ──"));
                for line in safe_content.lines() {
                    pipeline.println(line);
                }
                pipeline.println("── end ──");
            }
        }
        return Ok(SyncResult {
            files_created: 0,
            files_updated: 0,
            files_failed: 0,
            files_rejected: 0,
        });
    }

    // Step 4: Run confirmation (if not dry-run).
    // When force=true, confirm_files returns all files immediately without
    // any interactive prompt, so there is no need to suspend the TUI.
    let confirmed = if force {
        confirm_files(output, force, term)?
    } else {
        pipeline.suspend(|| confirm_files(output, force, term))?
    };
    let files_rejected = output.files.len() - confirmed.len();

    if confirmed.is_empty() {
        pipeline.println("No files to write.");
        return Ok(SyncResult {
            files_created: 0,
            files_updated: 0,
            files_failed: 0,
            files_rejected,
        });
    }

    // Step 5: Write confirmed files
    pipeline.start(SyncPhase::Write, "Writing files...");
    let results = write_files(root_dir, &confirmed, format);

    // Report Write phase outcome
    let write_created: usize = results
        .iter()
        .filter(|r| r.action == WriteAction::Created)
        .count();
    let write_updated: usize = results
        .iter()
        .filter(|r| r.action == WriteAction::Updated)
        .count();
    let write_failed: usize = results
        .iter()
        .filter(|r| r.action == WriteAction::Failed)
        .count();
    let write_summary = format!(
        "{write_created} created \u{00b7} {write_updated} updated \u{00b7} {write_failed} failed"
    );
    if write_failed > 0 {
        pipeline.warn(SyncPhase::Write, &write_summary);
    } else {
        pipeline.success(SyncPhase::Write, &write_summary);
    }

    // Step 6: Report results and return SyncResult
    let width = term_size::terminal_width();
    let (files_created, files_updated, files_failed) = report_write_results(
        &results,
        files_rejected,
        pipeline.start_time().elapsed(),
        pipeline,
        width,
    );

    Ok(SyncResult {
        files_created,
        files_updated,
        files_failed,
        files_rejected,
    })
}

/// Report per-file write results to the terminal using a Panel.
///
/// Returns `(created, updated, failed)` counts.
fn report_write_results(
    results: &[WriteResult],
    rejected_count: usize,
    elapsed: Duration,
    pipeline: &mut TuiRenderer,
    width: usize,
) -> (usize, usize, usize) {
    let mut files_created = 0;
    let mut files_updated = 0;
    let mut files_failed = 0;

    let mut panel = Panel::titled("Results");

    for result in results {
        let safe_path = console::strip_ansi_codes(&result.path);
        match result.action {
            WriteAction::Created => {
                files_created += 1;
                panel = panel.line(&format!(
                    "{} {}    created   v{}",
                    theme::success(&theme::SUCCESS),
                    safe_path,
                    result.version
                ));
            }
            WriteAction::Updated => {
                files_updated += 1;
                panel = panel.line(&format!(
                    "{} {}    updated   v{}",
                    theme::success(&theme::SUCCESS),
                    safe_path,
                    result.version
                ));
            }
            WriteAction::Failed => {
                files_failed += 1;
                let err_msg = result.error.as_deref().unwrap_or("unknown error");
                panel = panel.line(&format!(
                    "{} {}    error     {}",
                    theme::error(&theme::ERROR),
                    safe_path,
                    err_msg
                ));
            }
        }
    }

    let elapsed_secs = elapsed.as_secs_f64();
    let summary = format!(
        "Sync complete: {files_created} created \u{00b7} {files_updated} updated \u{00b7} {files_failed} failed \u{00b7} {rejected_count} rejected    [{elapsed_secs:.1}s total]"
    );

    panel = panel.separator().footer(&summary);
    for line in panel.render(width).lines() {
        pipeline.println(line);
    }

    (files_created, files_updated, files_failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::types::{Framework, FrameworkCategory, Language, Project};
    use crate::cli::commands::handle_result;
    use crate::cli::ui::test_utils::MockTerminal;
    use crate::cli::ui::tui::renderer::TuiRenderer;
    use crate::error::ActualError;
    use crate::generation::markers;
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

    fn make_file(path: &str, content: &str, adr_ids: Vec<&str>) -> FileOutput {
        FileOutput {
            path: path.to_string(),
            content: content.to_string(),
            reasoning: format!("reason for {path}"),
            adr_ids: adr_ids.into_iter().map(|s| s.to_string()).collect(),
        }
    }

    fn make_monorepo_analysis() -> RepoAnalysis {
        RepoAnalysis {
            is_monorepo: true,
            projects: vec![
                Project {
                    path: "apps/web".to_string(),
                    name: "Web App".to_string(),
                    languages: vec![Language::TypeScript],
                    frameworks: vec![Framework {
                        name: "nextjs".to_string(),
                        category: FrameworkCategory::WebFrontend,
                    }],
                    package_manager: Some("npm".to_string()),
                    description: None,
                },
                Project {
                    path: "services/api".to_string(),
                    name: "API Service".to_string(),
                    languages: vec![Language::Rust],
                    frameworks: vec![],
                    package_manager: Some("cargo".to_string()),
                    description: None,
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
            log_text.contains("No files to write."),
            "expected 'No files to write.' in log: {log_text}"
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
            log_text.contains("Results"),
            "expected Results panel title in log: {log_text}"
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
        );
        assert!(
            result.is_ok(),
            "not-authenticated auth_display should still complete sync"
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
        // exec() now calls find_claude_binary() which will fail if Claude is not found
        // Set an invalid binary path to ensure it fails predictably
        let _lock = crate::testutil::ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _guard = crate::testutil::EnvGuard::set("CLAUDE_BINARY", "/nonexistent/path/to/claude");
        let args = make_sync_args(true, false, false, false, "http://unused");
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
        };
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
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
            report_write_results(&results, 0, Duration::from_secs(1), &mut pipeline, 80);

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
            report_write_results(&results, 0, Duration::from_secs(1), &mut pipeline, 80);

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
            report_write_results(&results, 0, Duration::from_secs(1), &mut pipeline, 80);

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
            report_write_results(&results, 0, Duration::from_secs(1), &mut pipeline, 80);

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
            report_write_results(&results, 0, Duration::from_secs(1), &mut pipeline, 80);

        assert_eq!(created, 1);
        assert_eq!(updated, 1);
        assert_eq!(failed, 1);
    }

    // ── Panel rendering tests ──

    #[test]
    fn test_report_write_results_panel_has_box_characters() {
        let results = vec![WriteResult {
            path: "CLAUDE.md".to_string(),
            action: WriteAction::Created,
            version: 1,
            error: None,
        }];
        let mut pipeline = TuiRenderer::new(false, true);

        report_write_results(&results, 0, Duration::from_secs(3), &mut pipeline, 80);

        let log_text = pipeline.all_log_text();
        let stripped = console::strip_ansi_codes(&log_text);
        assert!(
            stripped.contains("Results"),
            "expected panel title 'Results' in log: {stripped}"
        );
        assert!(
            stripped.contains('\u{256d}'), // ╭
            "expected top-left border in log: {stripped}"
        );
        assert!(
            stripped.contains('\u{2570}'), // ╰
            "expected bottom-left border in log: {stripped}"
        );
        assert!(
            stripped.contains('\u{2502}'), // │
            "expected side border in log: {stripped}"
        );
    }

    #[test]
    fn test_report_write_results_panel_has_separator() {
        let results = vec![WriteResult {
            path: "CLAUDE.md".to_string(),
            action: WriteAction::Created,
            version: 1,
            error: None,
        }];
        let mut pipeline = TuiRenderer::new(false, true);

        report_write_results(&results, 0, Duration::from_secs(1), &mut pipeline, 80);

        let log_text = pipeline.all_log_text();
        let stripped = console::strip_ansi_codes(&log_text);
        assert!(
            stripped.contains('\u{251c}') && stripped.contains('\u{2524}'), // ├ and ┤
            "expected separator T-junctions in log: {stripped}"
        );
    }

    #[test]
    fn test_report_write_results_panel_has_timing() {
        let results = vec![WriteResult {
            path: "CLAUDE.md".to_string(),
            action: WriteAction::Created,
            version: 1,
            error: None,
        }];
        let mut pipeline = TuiRenderer::new(false, true);

        report_write_results(&results, 0, Duration::from_millis(7300), &mut pipeline, 80);

        let log_text = pipeline.all_log_text();
        let stripped = console::strip_ansi_codes(&log_text);
        assert!(
            stripped.contains("[7.3s total]"),
            "expected timing display in log: {stripped}"
        );
    }

    #[test]
    fn test_report_write_results_panel_dot_separated_summary() {
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

        report_write_results(&results, 1, Duration::from_secs(2), &mut pipeline, 80);

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
    fn test_report_write_results_panel_mixed_results() {
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
            report_write_results(&results, 2, Duration::from_millis(4500), &mut pipeline, 80);

        assert_eq!(created, 1);
        assert_eq!(updated, 1);
        assert_eq!(failed, 1);

        let log_text = pipeline.all_log_text();
        let stripped = console::strip_ansi_codes(&log_text);
        // Verify all file entries
        assert!(stripped.contains("CLAUDE.md") && stripped.contains("updated"));
        assert!(stripped.contains("apps/web/CLAUDE.md") && stripped.contains("created"));
        assert!(stripped.contains("apps/api/CLAUDE.md") && stripped.contains("permission denied"));
        // Verify panel footer
        assert!(stripped.contains("1 created"));
        assert!(stripped.contains("1 updated"));
        assert!(stripped.contains("1 failed"));
        assert!(stripped.contains("2 rejected"));
        assert!(stripped.contains("[4.5s total]"));
        // Verify panel structure
        assert!(stripped.contains("Results"));
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

        // Both must produce valid 64-char hex hashes
        assert_eq!(key_target.len(), 64, "target key should be 64-char hex");
        assert_eq!(key_symlink.len(), 64, "symlink key should be 64-char hex");
        // Document: on macOS current_dir() resolves symlinks so keys match;
        // on Linux symlink path may differ from canonical path.
        // This test documents the actual behavior without enforcing a specific outcome.
        // If they differ, it means a user cd'ing into a symlink dir gets a different
        // rejection namespace — a known limitation documented here.
        println!("key_target: {}", key_target);
        println!("key_symlink: {}", key_symlink);
        // Just verify both are valid hashes (no assertion on equality)
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
            },
            matched_projects: projects.into_iter().map(String::from).collect(),
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
        assert!(output.files[0].content.contains("Test ADR"));
        assert!(output.files[0].content.contains("Policy for Test ADR"));
        assert!(output.files[0].content.contains("Instruction for Test ADR"));
        assert_eq!(output.files[0].adr_ids, vec!["adr-001"]);
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
        assert!(output.files[0].content.contains("Policy for Test ADR"));
        // With instructions=None, the instruction line should not appear
        let content = &output.files[0].content;
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
        assert!(output.files[0].content.contains("Policy for Test ADR"));
        // With empty instructions vec, no instruction lines should appear
        let content = &output.files[0].content;
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
        assert!(
            output.files.is_empty(),
            "expected no files for traversal path"
        );
    }

    #[test]
    fn test_raw_adrs_to_output_skips_absolute_project_path() {
        let adrs = vec![make_test_adr("adr-001", "Test ADR", vec!["/etc/passwd"])];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::ClaudeMd);
        assert!(
            output.files.is_empty(),
            "expected no files for absolute path"
        );
    }

    #[test]
    fn test_raw_adrs_to_output_strips_control_chars_from_content() {
        // Use actual control chars (not full ANSI sequences) to verify stripping
        let mut adr = make_test_adr("adr-001", "Test\x07ADR", vec![]);
        adr.policies = vec!["Policy\x01with\x02bells".to_string()];
        adr.instructions = Some(vec!["Instruction\x03normal".to_string()]);
        let output = raw_adrs_to_output(&[adr], &OutputFormat::ClaudeMd);
        assert_eq!(output.files.len(), 1);
        let content = &output.files[0].content;
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
        for skip in super::super::SKIP_DIRS {
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
        };
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
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
        };
        // This should succeed and print verbose output to stderr
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
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
        };
        let result = run_sync(&args, dir.path(), &cfg_path, &term, &runner, None);
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
        };
        let result = run_sync(&args, dir.path(), &cfg_path, &term, &runner, None);
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
        };
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            None,
        );
        assert!(result.is_err(), "expected tailoring to fail");
    }

    // ── mock runner success path coverage ──

    /// Valid `TailoringOutput` JSON that the mock runner can return successfully.
    const VALID_TAILORING_JSON: &str = "{\"files\":[{\"path\":\"CLAUDE.md\",\"content\":\"Rules.\",\"reasoning\":\"Root rules\",\"adr_ids\":[\"adr-001\"]}],\"skipped_adrs\":[],\"summary\":{\"total_input\":1,\"applicable\":1,\"not_applicable\":0,\"files_generated\":1}}";

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
        };
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
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
            },
            matched_projects: vec![".".to_string()],
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
            },
            matched_projects: vec![],
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
            },
            matched_projects: vec![],
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
        let restored: TailoringOutput = serde_yaml::from_value(cached.tailoring.clone()).unwrap();
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

    /// A test-only newtype that always fails to serialize, used to exercise the
    /// `serialize_tailoring_output` error path without changing production types.
    struct AlwaysFailsSerialize;

    impl serde::Serialize for AlwaysFailsSerialize {
        fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("intentional test failure"))
        }
    }

    #[test]
    fn test_serialize_tailoring_output_warns_on_error() {
        // AlwaysFailsSerialize always returns Err from serde, exercising the
        // eprintln! warning branch inside serialize_tailoring_output.
        let result = serialize_tailoring_output(&AlwaysFailsSerialize);
        assert!(
            result.is_none(),
            "serialize_tailoring_output should return None on error"
        );
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
    fn test_store_tailoring_cache_skips_on_serialization_error() {
        // When serialize_tailoring_output returns None (e.g. a non-YAML-
        // representable future field), store_tailoring_cache must return early
        // without populating the in-memory cache or writing to disk.
        //
        // store_tailoring_cache is generic over T: Serialize, so we can pass
        // AlwaysFailsSerialize to exercise the serialize error branch.
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.yaml");
        let mut config = crate::config::Config::default();
        store_tailoring_cache(
            &mut config,
            &cfg_path,
            Some("will-not-be-stored"),
            dir.path(),
            &AlwaysFailsSerialize,
        );
        // Serialization failed → no cache should be stored
        assert!(
            config.cached_tailoring.is_none(),
            "cache should not be set when serialization fails"
        );
    }

    // ── Tailoring cache round-trip test ──

    #[test]
    fn test_tailoring_output_round_trip_through_yaml_value() {
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                content: "# Rules\n\nDo things.".to_string(),
                reasoning: "Root rules".to_string(),
                adr_ids: vec!["adr-001".to_string(), "adr-002".to_string()],
            }],
            skipped_adrs: vec![],
            summary: TailoringSummary {
                total_input: 2,
                applicable: 2,
                not_applicable: 0,
                files_generated: 1,
            },
        };

        let value = serde_yaml::to_value(&output).unwrap();
        let restored: TailoringOutput = serde_yaml::from_value(value).unwrap();
        assert_eq!(output, restored);
    }

    #[test]
    fn test_corrupted_tailoring_cache_falls_through() {
        // Simulates a corrupted cache that cannot be deserialized as TailoringOutput
        let corrupted = serde_yaml::to_value("this is not a TailoringOutput").unwrap();
        let result = serde_yaml::from_value::<TailoringOutput>(corrupted);
        assert!(
            result.is_err(),
            "corrupted value should fail deserialization"
        );
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
        };
        run_sync(&args, dir.path(), &cfg_path, &term, &runner, None).unwrap();

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
        };

        // Provide "y" for project confirmation and select all files
        let term2 = MockTerminal::new(vec!["y"]).with_selection(Some(vec![0]));
        let result = run_sync(&args2, dir.path(), &cfg_path, &term2, &runner, None);
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
        };
        run_sync(&args, dir.path(), &cfg_path, &term, &runner, None).unwrap();

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
        };
        run_sync(&args2, dir.path(), &cfg_path, &term2, &runner, None).unwrap();

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
        };
        run_sync(&args, dir.path(), &cfg_path, &term, &runner, None).unwrap();

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
        };
        let result = run_sync(&args2, dir.path(), &cfg_path, &term2, &runner, None);
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
        };
        run_sync(&args, dir.path(), &cfg_path, &term, &runner, None).unwrap();

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
        };
        let result = run_sync(&args2, dir.path(), &cfg_path, &term2, &runner, None);
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

        let result = run_tailoring_with_cancel(tailor_fut, &mut rx, &mut pipeline, 0, cancel).await;

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

        let result = run_tailoring_with_cancel(tailor_fut, &mut rx, &mut pipeline, 0, cancel).await;

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
        })
        .unwrap();
        drop(tx);

        let output = make_output(vec![]);
        let tailor_fut = std::future::ready(Ok::<TailoringOutput, ActualError>(output));
        // cancel never fires
        let cancel = std::future::pending::<std::io::Result<()>>();

        let result = run_tailoring_with_cancel(tailor_fut, &mut rx, &mut pipeline, 1, cancel).await;

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
        let result = run_tailoring_with_cancel(tailor_fut, &mut rx, &mut pipeline, 2, cancel).await;

        assert!(result.is_ok(), "expected Ok result, got {result:?}");
        // The heartbeat must have set the step row message inline (Steps pane),
        // not pushed to the log pane.
        let step_msg = pipeline.steps.steps[3].message.clone();
        assert!(
            step_msg.contains("projects done"),
            "expected heartbeat message in step.message, got: {step_msg:?}"
        );
        // The log pane must NOT have been populated by update_message.
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
}
