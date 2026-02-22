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

/// Result summary from the confirm + write phase.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncResult {
    pub files_created: usize,
    pub files_updated: usize,
    pub files_failed: usize,
    pub files_rejected: usize,
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
            let detection = markers::detect_changes(existing_content.as_deref(), &file.adr_ids());

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
fn report_write_results(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ui::test_utils::MockTerminal;
    use crate::cli::ui::tui::renderer::TuiRenderer;
    use crate::error::ActualError;
    use crate::generation::markers;
    use crate::tailoring::types::{AdrSection, FileOutput, TailoringOutput, TailoringSummary};

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

        report_write_results(&results, 0, Duration::from_secs(3), &mut pipeline, 80);

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

        report_write_results(&results, 0, Duration::from_secs(1), &mut pipeline, 80);

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

        report_write_results(&results, 0, Duration::from_millis(7300), &mut pipeline, 80);

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
        // Verify summary line
        assert!(stripped.contains("1 created"));
        assert!(stripped.contains("1 updated"));
        assert!(stripped.contains("1 failed"));
        assert!(stripped.contains("2 rejected"));
        assert!(stripped.contains("[4.5s total]"));
    }
}
