use std::path::Path;
use std::time::Duration;

use crate::cli::ui::diff::{format_diff_summary, FileDiff};
use crate::cli::ui::progress::SyncPhase;
use crate::cli::ui::term_size;
use crate::cli::ui::terminal::TerminalIO;
use crate::cli::ui::theme;
use crate::cli::ui::tui::renderer::TuiRenderer;
use crate::error::ActualError;
use crate::generation::markers;
use crate::generation::writer::{write_files, WriteAction, WriteResult};
use crate::generation::OutputFormat;
use crate::tailoring::types::TailoringOutput;

#[derive(Debug, Clone, PartialEq)]
pub struct SyncResult {
    pub files_created: usize,
    pub files_updated: usize,
    pub files_failed: usize,
    pub files_rejected: usize,
}

/// Entry point for `actual adr-bot`.
///
/// Production wiring (`CliClaudeRunner`, `RealTerminal`) lives in
/// `sync_wiring.rs` (which is excluded from coverage) so that the only
/// generic instantiation of `run_sync` in `sync.rs` is `MockRunner` from
/// unit tests.
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
            let detection = markers::detect_changes(existing_content.as_deref(), &file.adr_ids());

            // Extract old managed content stripped of metadata for text diffing.
            let old_managed = existing_content
                .as_deref()
                .and_then(markers::extract_managed_content)
                .map(markers::strip_managed_metadata);

            // Extract old per-ADR sections for per-section diffing.
            let old_sections = existing_content
                .as_deref()
                .and_then(markers::extract_managed_content)
                .map(markers::extract_adr_sections)
                .unwrap_or_default();

            FileDiff::from_sections(
                &file.path,
                &detection,
                is_new_file,
                &old_sections,
                &file.sections,
                old_managed,
                file.content(),
            )
        })
        .collect();

    // Step 2: Start the Write phase so the steps panel shows it as active.
    // File confirmation is part of the Write phase — the user needs to see
    // the diff summary and confirm which files to write.
    pipeline.start(SyncPhase::Write, "Confirming file changes...");

    // Step 3: Display diff summary (now renders in the Write step's log pane)
    let summary = format_diff_summary(&diffs);
    if !summary.is_empty() {
        for line in summary.lines() {
            pipeline.println(line);
        }
    }

    // Step 4: Handle --dry-run
    if dry_run {
        if full {
            for file in &output.files {
                let safe_path = console::strip_ansi_codes(&file.path);
                let content = file.content();
                let safe_content = console::strip_ansi_codes(&content);
                pipeline.println(&format!("── {safe_path} ──"));
                for line in safe_content.lines() {
                    pipeline.println(line);
                }
                pipeline.println("── end ──");
            }
        }
        pipeline.skip(SyncPhase::Write, "Dry run — no files written");
        return Ok(SyncResult {
            files_created: 0,
            files_updated: 0,
            files_failed: 0,
            files_rejected: 0,
        });
    }

    // Step 5: Run confirmation (if not dry-run).
    // Use the native TUI multi-select so the user stays inside the TUI
    // (no suspend/dialoguer teardown).  Plain/Quiet mode falls back to
    // the existing dialoguer-based confirm_files path automatically.
    //
    // Record prompt duration so we can exclude user wait time from the
    // Write phase elapsed timer.
    let prompt_start = std::time::Instant::now();
    let confirmed = pipeline.select_files_in_tui(output, force, term)?;
    pipeline.adjust_start_for_pause(SyncPhase::Write, prompt_start.elapsed());
    let files_rejected = output.files.len() - confirmed.len();

    if confirmed.is_empty() {
        pipeline.skip(SyncPhase::Write, "No files to write");
        return Ok(SyncResult {
            files_created: 0,
            files_updated: 0,
            files_failed: 0,
            files_rejected,
        });
    }

    // Step 6: Write confirmed files
    pipeline.println("Writing files...");
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
        pipeline.steps_elapsed(),
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

/// Report per-file write results to the terminal.
///
/// Returns `(created, updated, failed)` counts.
pub(crate) fn report_write_results(
    results: &[WriteResult],
    rejected_count: usize,
    elapsed: Duration,
    pipeline: &mut TuiRenderer,
    _width: usize,
) -> (usize, usize, usize) {
    let mut files_created = 0;
    let mut files_updated = 0;
    let mut files_failed = 0;

    for result in results {
        let safe_path = console::strip_ansi_codes(&result.path);
        match result.action {
            WriteAction::Created => {
                files_created += 1;
                pipeline.println(&format!(
                    "  {} {}    created   v{}",
                    theme::success(&theme::SUCCESS),
                    safe_path,
                    result.version
                ));
            }
            WriteAction::Updated => {
                files_updated += 1;
                pipeline.println(&format!(
                    "  {} {}    updated   v{}",
                    theme::success(&theme::SUCCESS),
                    safe_path,
                    result.version
                ));
            }
            WriteAction::Failed => {
                files_failed += 1;
                let err_msg = result.error.as_deref().unwrap_or("unknown error");
                pipeline.println(&format!(
                    "  {} {}    error     {}",
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

    pipeline.println("");
    pipeline.println(&format!("  {}", theme::muted(&summary)));

    (files_created, files_updated, files_failed)
}
