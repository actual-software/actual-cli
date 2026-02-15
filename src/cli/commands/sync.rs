use std::path::Path;

use console::style;

use crate::cli::args::SyncArgs;
use crate::cli::ui::diff::{format_diff_summary, FileDiff};
use crate::cli::ui::file_confirm::{confirm_files, TerminalIO};
use crate::cli::ui::progress::{ERROR_SYMBOL, SUCCESS_SYMBOL};
use crate::cli::ui::real_terminal::RealTerminal;
use crate::error::ActualError;
use crate::generation::markers;
use crate::generation::writer::{write_files, WriteAction, WriteResult};
use crate::tailoring::types::{TailoringOutput, TailoringSummary};

/// Result summary from the confirm + write phase.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncResult {
    pub files_created: usize,
    pub files_updated: usize,
    pub files_failed: usize,
    pub files_rejected: usize,
}

pub fn exec(args: &SyncArgs) -> i32 {
    handle_result(run_sync(args))
}

fn handle_result(result: Result<(), ActualError>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{} {}", style("Error:").red().bold(), e);
            e.exit_code()
        }
    }
}

fn cwd_error(e: std::io::Error) -> ActualError {
    ActualError::ConfigError(format!("cannot determine working directory: {e}"))
}

fn run_sync(args: &SyncArgs) -> Result<(), ActualError> {
    let root_dir = std::env::current_dir().map_err(cwd_error)?;
    let term = RealTerminal::default();
    run_sync_inner(args, &root_dir, &term)
}

/// Core sync logic, separated for testability.
///
/// Accepts injected `root_dir` and `term` so unit tests can avoid
/// `RealTerminal` (which blocks on stdin) and control the working directory.
fn run_sync_inner(
    args: &SyncArgs,
    root_dir: &Path,
    term: &dyn TerminalIO,
) -> Result<(), ActualError> {
    // TODO: Phase 1 - env check + analysis (actual-cli-a59)
    // TODO: Phase 2 - fetch + tailor (actual-cli-e29)
    // For now, produce an empty TailoringOutput since upstream phases are not connected.

    eprintln!(
        "Sync pipeline: phases 1-2 not yet connected; running confirm + write with empty output."
    );

    let output = TailoringOutput {
        files: vec![],
        skipped_adrs: vec![],
        summary: TailoringSummary {
            total_input: 0,
            applicable: 0,
            not_applicable: 0,
            files_generated: 0,
        },
    };

    // Phase 3 - confirm + write (fully implemented)
    // --no-tailor: when Phase 2 is connected, this flag will skip Claude tailoring.
    // For now it has no additional effect since tailoring isn't wired yet.
    let _result = confirm_and_write(&output, root_dir, args.force, args.dry_run, args.full, term)?;
    Ok(())
}

/// Execute the confirm + write phase of the sync pipeline.
///
/// Given a `TailoringOutput` from the fetch+tailor phase, this function:
/// 1. Computes per-file diffs against existing CLAUDE.md files
/// 2. Displays a diff summary
/// 3. In dry-run mode: prints summary (and full content if --full), returns
/// 4. In normal mode: runs the confirmation flow (or skips with --force)
/// 5. Writes confirmed files to disk
/// 6. Reports per-file results (created/updated/failed)
///
/// Write errors on individual files do NOT abort the batch.
pub fn confirm_and_write(
    output: &TailoringOutput,
    root_dir: &Path,
    force: bool,
    dry_run: bool,
    full: bool,
    term: &dyn TerminalIO,
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
            FileDiff::from_change_detection(&file.path, &detection, is_new_file)
        })
        .collect();

    // Step 2: Display diff summary
    let summary = format_diff_summary(&diffs);
    if !summary.is_empty() {
        term.write_line("Changes:");
        term.write_line(&summary);
    }

    // Step 3: Handle --dry-run
    if dry_run {
        if full {
            for file in &output.files {
                term.write_line(&format!("── {} ──", file.path));
                term.write_line(&file.content);
                term.write_line("── end ──");
            }
        }
        return Ok(SyncResult {
            files_created: 0,
            files_updated: 0,
            files_failed: 0,
            files_rejected: 0,
        });
    }

    // Step 4: Run confirmation (if not dry-run)
    let confirmed = confirm_files(output, force, term)?;
    let files_rejected = output.files.len() - confirmed.len();

    if confirmed.is_empty() {
        term.write_line("No files to write.");
        return Ok(SyncResult {
            files_created: 0,
            files_updated: 0,
            files_failed: 0,
            files_rejected,
        });
    }

    // Step 5: Write confirmed files
    let results = write_files(root_dir, &confirmed);

    // Step 6: Report results and return SyncResult
    let (files_created, files_updated, files_failed) = report_write_results(&results, term);

    let sync_result = SyncResult {
        files_created,
        files_updated,
        files_failed,
        files_rejected,
    };

    term.write_line(&format!(
        "\nSync complete: {} created, {} updated, {} failed, {} rejected",
        sync_result.files_created,
        sync_result.files_updated,
        sync_result.files_failed,
        sync_result.files_rejected,
    ));

    Ok(sync_result)
}

/// Report per-file write results to the terminal.
///
/// Returns `(created, updated, failed)` counts.
fn report_write_results(results: &[WriteResult], term: &dyn TerminalIO) -> (usize, usize, usize) {
    let mut files_created = 0;
    let mut files_updated = 0;
    let mut files_failed = 0;

    for result in results {
        match result.action {
            WriteAction::Created => {
                files_created += 1;
                term.write_line(&format!(
                    "  {} {} (created, v{})",
                    style(SUCCESS_SYMBOL).green(),
                    result.path,
                    result.version
                ));
            }
            WriteAction::Updated => {
                files_updated += 1;
                term.write_line(&format!(
                    "  {} {} (updated, v{})",
                    style(SUCCESS_SYMBOL).green(),
                    result.path,
                    result.version
                ));
            }
            WriteAction::Failed => {
                files_failed += 1;
                let err_msg = result.error.as_deref().unwrap_or("unknown error");
                term.write_line(&format!(
                    "  {} {} ({})",
                    style(ERROR_SYMBOL).red(),
                    result.path,
                    err_msg
                ));
            }
        }
    }

    (files_created, files_updated, files_failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ui::file_confirm::TerminalIO;
    use crate::error::ActualError;
    use crate::generation::markers;
    use crate::tailoring::types::{FileOutput, TailoringSummary};
    use std::sync::Mutex;

    struct MockTerminal {
        inputs: Mutex<Vec<String>>,
        output: Mutex<Vec<String>>,
    }

    impl MockTerminal {
        fn new(inputs: Vec<&str>) -> Self {
            Self {
                inputs: Mutex::new(inputs.into_iter().map(|s| s.to_string()).collect()),
                output: Mutex::new(Vec::new()),
            }
        }

        fn output_text(&self) -> String {
            self.output.lock().unwrap().join("\n")
        }
    }

    impl TerminalIO for MockTerminal {
        fn read_line(&self, _prompt: &str) -> Result<String, ActualError> {
            let mut inputs = self.inputs.lock().unwrap();
            if inputs.is_empty() {
                return Err(ActualError::UserCancelled);
            }
            Ok(inputs.remove(0))
        }

        fn write_line(&self, text: &str) {
            self.output.lock().unwrap().push(text.to_string());
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

    // ── test_confirm_and_write_force_mode ──

    #[test]
    fn test_confirm_and_write_force_mode() {
        let dir = tempfile::tempdir().unwrap();
        let output = make_output(vec![
            make_file("CLAUDE.md", "Root rules", vec!["adr-001"]),
            make_file("apps/web/CLAUDE.md", "Web rules", vec!["adr-002"]),
        ]);
        let term = MockTerminal::new(vec![]);

        let result = confirm_and_write(&output, dir.path(), true, false, false, &term).unwrap();

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

        let result = confirm_and_write(&output, dir.path(), false, true, false, &term).unwrap();

        // Dry run: nothing written
        assert_eq!(result.files_created, 0);
        assert_eq!(result.files_updated, 0);
        assert_eq!(result.files_failed, 0);
        assert_eq!(result.files_rejected, 0);

        // File should NOT exist on disk
        assert!(!dir.path().join("CLAUDE.md").exists());

        // Diff summary should be displayed
        let text = term.output_text();
        assert!(
            text.contains("Changes:"),
            "expected changes header in: {text}"
        );
    }

    // ── test_confirm_and_write_dry_run_full ──

    #[test]
    fn test_confirm_and_write_dry_run_full() {
        let dir = tempfile::tempdir().unwrap();
        let output = make_output(vec![make_file(
            "CLAUDE.md",
            "Full content here",
            vec!["adr-001"],
        )]);
        let term = MockTerminal::new(vec![]);

        let result = confirm_and_write(&output, dir.path(), false, true, true, &term).unwrap();

        assert_eq!(result.files_created, 0);

        // Full content should be displayed
        let text = term.output_text();
        assert!(
            text.contains("── CLAUDE.md ──"),
            "expected file header in: {text}"
        );
        assert!(
            text.contains("Full content here"),
            "expected full content in: {text}"
        );
        assert!(text.contains("── end ──"), "expected end marker in: {text}");

        // File should NOT exist on disk
        assert!(!dir.path().join("CLAUDE.md").exists());
    }

    // ── test_confirm_and_write_reject_file ──

    #[test]
    fn test_confirm_and_write_reject_file() {
        let dir = tempfile::tempdir().unwrap();
        let output = make_output(vec![
            make_file("CLAUDE.md", "Root rules", vec!["adr-001"]),
            make_file("apps/web/CLAUDE.md", "Web rules", vec!["adr-002"]),
        ]);
        // Reject file 2, then accept
        let term = MockTerminal::new(vec!["r 2", "a"]);

        let result = confirm_and_write(&output, dir.path(), false, false, false, &term).unwrap();

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
        let term = MockTerminal::new(vec!["q"]);

        let result = confirm_and_write(&output, dir.path(), false, false, false, &term);

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

        let result = confirm_and_write(&output, dir.path(), true, false, false, &term).unwrap();

        // First file succeeds, second fails
        assert_eq!(result.files_created, 1);
        assert_eq!(result.files_failed, 1);
        assert_eq!(result.files_rejected, 0);

        // First file should exist
        assert!(dir.path().join("CLAUDE.md").exists());

        // Error should be reported in output
        let text = term.output_text();
        assert!(
            text.contains("bad/CLAUDE.md"),
            "expected failed file path in output: {text}"
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

        let result = confirm_and_write(&output, dir.path(), true, false, false, &term).unwrap();

        assert_eq!(result.files_created, 1);
        assert_eq!(result.files_updated, 0);

        let content = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(markers::has_managed_section(&content));
        assert!(content.contains("Brand new rules"));

        // Verify diff output showed new file
        let text = term.output_text();
        assert!(
            text.contains("new file"),
            "expected 'new file' in diff output: {text}"
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

        let result = confirm_and_write(&output, dir.path(), true, false, false, &term).unwrap();

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

        let result = confirm_and_write(&output, dir.path(), true, false, false, &term).unwrap();

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
        // Reject both files, then accept
        let term = MockTerminal::new(vec!["r 1", "r 2", "a"]);

        let result = confirm_and_write(&output, dir.path(), false, false, false, &term).unwrap();

        assert_eq!(result.files_created, 0);
        assert_eq!(result.files_updated, 0);
        assert_eq!(result.files_failed, 0);
        assert_eq!(result.files_rejected, 2);

        // No files should exist
        assert!(!dir.path().join("CLAUDE.md").exists());
        assert!(!dir.path().join("apps/web/CLAUDE.md").exists());

        let text = term.output_text();
        assert!(
            text.contains("No files to write."),
            "expected 'No files to write.' in: {text}"
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

        // Reject file 4 (apps/api/CLAUDE.md), accept rest
        let term = MockTerminal::new(vec!["r 4", "a"]);

        let result = confirm_and_write(&output, dir.path(), false, false, false, &term).unwrap();

        assert_eq!(result.files_created, 1, "expected 1 created (apps/web)");
        assert_eq!(result.files_updated, 1, "expected 1 updated (root)");
        assert_eq!(result.files_failed, 1, "expected 1 failed (fail/)");
        assert_eq!(result.files_rejected, 1, "expected 1 rejected (apps/api)");

        // Verify summary line
        let text = term.output_text();
        assert!(
            text.contains("Sync complete: 1 created, 1 updated, 1 failed, 1 rejected"),
            "expected correct summary line in: {text}"
        );
    }

    // ── cwd_error test ──

    #[test]
    fn test_cwd_error_formats_message() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "dir gone");
        let err = cwd_error(io_err);
        match err {
            ActualError::ConfigError(msg) => {
                assert!(
                    msg.contains("cannot determine working directory"),
                    "expected cwd context in: {msg}"
                );
                assert!(msg.contains("dir gone"), "expected io error in: {msg}");
            }
            other => panic!("expected ConfigError, got: {other:?}"),
        }
    }

    // ── run_sync_inner flag-passthrough tests ──

    fn make_sync_args(dry_run: bool, full: bool, force: bool, no_tailor: bool) -> SyncArgs {
        SyncArgs {
            dry_run,
            full,
            force,
            reset_rejections: false,
            projects: vec![],
            model: None,
            api_url: None,
            verbose: false,
            no_tailor,
            max_budget_usd: None,
        }
    }

    #[test]
    fn test_run_sync_inner_dry_run_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let args = make_sync_args(true, false, false, false);
        let result = run_sync_inner(&args, dir.path(), &term);
        assert!(
            result.is_ok(),
            "run_sync_inner with --dry-run should succeed"
        );
    }

    #[test]
    fn test_run_sync_inner_dry_run_full_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let args = make_sync_args(true, true, false, false);
        let result = run_sync_inner(&args, dir.path(), &term);
        assert!(
            result.is_ok(),
            "run_sync_inner with --dry-run --full should succeed"
        );
    }

    #[test]
    fn test_run_sync_inner_force_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let args = make_sync_args(false, false, true, false);
        let result = run_sync_inner(&args, dir.path(), &term);
        assert!(result.is_ok(), "run_sync_inner with --force should succeed");
        // With empty output + force, terminal should show "No files to write."
        let text = term.output_text();
        assert!(
            text.contains("No files to write."),
            "expected 'No files to write.' in: {text}"
        );
    }

    #[test]
    fn test_run_sync_inner_no_tailor_force_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let args = make_sync_args(false, false, true, true);
        let result = run_sync_inner(&args, dir.path(), &term);
        assert!(
            result.is_ok(),
            "run_sync_inner with --no-tailor --force should succeed"
        );
    }

    #[test]
    fn test_run_sync_inner_force_dry_run_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let args = make_sync_args(true, false, true, false);
        // dry-run takes precedence over force
        let result = run_sync_inner(&args, dir.path(), &term);
        assert!(
            result.is_ok(),
            "run_sync_inner with --force --dry-run should succeed"
        );
    }

    #[test]
    fn test_run_sync_inner_no_flags_prompts_user() {
        let dir = tempfile::tempdir().unwrap();
        // Provide "a" (accept) input so the interactive prompt completes
        let term = MockTerminal::new(vec!["a"]);
        let args = make_sync_args(false, false, false, false);
        let result = run_sync_inner(&args, dir.path(), &term);
        assert!(
            result.is_ok(),
            "run_sync_inner with no flags should succeed when user accepts"
        );
    }

    #[test]
    fn test_run_sync_inner_dry_run_writes_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let args = make_sync_args(true, false, false, false);
        run_sync_inner(&args, dir.path(), &term).unwrap();
        // Verify no CLAUDE.md was created
        assert!(
            !dir.path().join("CLAUDE.md").exists(),
            "dry-run should not create any files"
        );
    }

    // ── exec / handle_result tests ──

    #[test]
    fn test_exec_returns_zero() {
        // Use dry_run=true to avoid interactive prompt (RealTerminal blocks in tests).
        let args = make_sync_args(true, false, false, false);
        let code = exec(&args);
        assert_eq!(code, 0);
    }

    #[test]
    fn test_handle_result_ok() {
        assert_eq!(handle_result(Ok(())), 0);
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

    #[test]
    fn test_handle_result_user_cancelled() {
        let code = handle_result(Err(ActualError::UserCancelled));
        assert_eq!(code, 4);
    }

    #[test]
    fn test_handle_result_config_error() {
        let code = handle_result(Err(ActualError::ConfigError("bad".to_string())));
        assert_eq!(code, 1);
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
        let term = MockTerminal::new(vec![]);
        let results = vec![WriteResult {
            path: "CLAUDE.md".to_string(),
            action: WriteAction::Failed,
            version: 0,
            error: None,
        }];

        let (created, updated, failed) = report_write_results(&results, &term);

        assert_eq!(created, 0);
        assert_eq!(updated, 0);
        assert_eq!(failed, 1);

        let text = term.output_text();
        assert!(
            text.contains("unknown error"),
            "expected 'unknown error' fallback in: {text}"
        );
    }

    #[test]
    fn test_report_write_results_created() {
        let term = MockTerminal::new(vec![]);
        let results = vec![WriteResult {
            path: "CLAUDE.md".to_string(),
            action: WriteAction::Created,
            version: 1,
            error: None,
        }];

        let (created, updated, failed) = report_write_results(&results, &term);

        assert_eq!(created, 1);
        assert_eq!(updated, 0);
        assert_eq!(failed, 0);

        let text = term.output_text();
        assert!(
            text.contains("CLAUDE.md") && text.contains("created"),
            "expected created message in: {text}"
        );
    }

    #[test]
    fn test_report_write_results_updated() {
        let term = MockTerminal::new(vec![]);
        let results = vec![WriteResult {
            path: "apps/web/CLAUDE.md".to_string(),
            action: WriteAction::Updated,
            version: 3,
            error: None,
        }];

        let (created, updated, failed) = report_write_results(&results, &term);

        assert_eq!(created, 0);
        assert_eq!(updated, 1);
        assert_eq!(failed, 0);

        let text = term.output_text();
        assert!(
            text.contains("apps/web/CLAUDE.md") && text.contains("updated"),
            "expected updated message in: {text}"
        );
    }

    #[test]
    fn test_report_write_results_failed_with_error_message() {
        let term = MockTerminal::new(vec![]);
        let results = vec![WriteResult {
            path: "bad/CLAUDE.md".to_string(),
            action: WriteAction::Failed,
            version: 0,
            error: Some("permission denied".to_string()),
        }];

        let (created, updated, failed) = report_write_results(&results, &term);

        assert_eq!(created, 0);
        assert_eq!(updated, 0);
        assert_eq!(failed, 1);

        let text = term.output_text();
        assert!(
            text.contains("permission denied"),
            "expected error message in: {text}"
        );
    }

    #[test]
    fn test_report_write_results_mixed() {
        let term = MockTerminal::new(vec![]);
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

        let (created, updated, failed) = report_write_results(&results, &term);

        assert_eq!(created, 1);
        assert_eq!(updated, 1);
        assert_eq!(failed, 1);
    }
}
