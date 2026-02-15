use std::path::Path;
use std::time::Duration;

use console::style;

use crate::analysis::cache::{get_git_head, run_analysis_cached};
use crate::analysis::confirm::ConfirmAction;
use crate::analysis::types::RepoAnalysis;
use crate::branding::banner::print_banner;
use crate::claude::binary::find_claude_binary;
use crate::claude::subprocess::{ClaudeRunner, CliClaudeRunner};
use crate::cli::args::SyncArgs;
use crate::cli::commands::auth::check_auth;
use crate::cli::ui::confirm::{format_project_summary, prompt_project_confirmation, InputReader};
use crate::cli::ui::diff::{format_diff_summary, FileDiff};
use crate::cli::ui::file_confirm::{confirm_files, TerminalIO};
use crate::cli::ui::progress::{Spinner, ERROR_SYMBOL, SUCCESS_SYMBOL};
use crate::cli::ui::real_terminal::RealTerminal;
use crate::config::paths::config_path;
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
    handle_result(run(args))
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

/// Thin wrapper that resolves system dependencies (cwd, terminal, Claude binary)
/// and delegates to [`run_sync`].  Kept minimal so nearly all logic lives
/// in the fully-testable [`run_sync`].
fn run(args: &SyncArgs) -> Result<(), ActualError> {
    let root_dir = resolve_cwd();
    let term = RealTerminal::default();

    // Phase 1 env check: locate Claude binary and verify auth
    let binary_path = find_claude_binary()?;
    let auth_status = check_auth(&binary_path)?;
    if !auth_status.is_usable() {
        return Err(ActualError::ClaudeNotAuthenticated);
    }

    let runner = CliClaudeRunner::new(binary_path, Duration::from_secs(300));
    let reader = crate::cli::ui::real_terminal::StdinReader;
    run_sync(args, &root_dir, &term, &runner, &reader)
}

/// Resolve the current working directory, falling back to `"."` if
/// unavailable (e.g. the directory was deleted while the process was running).
fn resolve_cwd() -> std::path::PathBuf {
    let fallback = std::path::PathBuf::from(".");
    std::env::current_dir().unwrap_or(fallback)
}

/// Core sync logic.
///
/// Accepts injected `root_dir`, `term`, `runner`, and `reader` so unit tests
/// can avoid `RealTerminal` (which blocks on stdin) and control the working
/// directory, Claude runner, and user input.
fn run_sync<R: ClaudeRunner>(
    args: &SyncArgs,
    root_dir: &Path,
    term: &dyn TerminalIO,
    runner: &R,
    reader: &dyn InputReader,
) -> Result<(), ActualError> {
    // ── Phase 1: env check + analysis ──

    // 1. Show banner
    print_banner(false);

    // 2. Check environment (git status)
    let spinner = Spinner::new("Checking environment...", false);
    let is_git = get_git_head(root_dir).is_some();
    if is_git {
        spinner.success("Environment OK");
    } else {
        spinner.warn("Not a git repository (caching disabled)");
    }

    // 3. Run analysis (cached if in a git repo)
    let spinner = Spinner::new("Analyzing repository...", false);
    let cfg_path = config_path()?;
    let rt = tokio::runtime::Runtime::new().expect("Failed to create async runtime");
    let analysis = rt.block_on(run_analysis_cached(
        runner,
        args.model.as_deref(),
        root_dir,
        &cfg_path,
    ))?;
    spinner.success("Analysis complete");

    // 4. Display projects
    let summary = format_project_summary(&analysis);
    eprintln!("{summary}");

    // 5. Filter by --project if specified
    let analysis = filter_projects(analysis, &args.projects)?;

    // 6. Confirmation (unless --force)
    if !args.force {
        let action = prompt_project_confirmation(&analysis, reader, &mut std::io::stderr());
        if matches!(action, ConfirmAction::Reject) {
            return Err(ActualError::UserCancelled);
        }
    }

    // ── Phase 2: fetch + tailor (TODO: actual-cli-e29) ──
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

    // ── Phase 3: confirm + write (fully implemented) ──
    // --no-tailor: when Phase 2 is connected, this flag will skip Claude tailoring.
    // For now it has no additional effect since tailoring isn't wired yet.
    let _result = confirm_and_write(&output, root_dir, args.force, args.dry_run, args.full, term)?;
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
    use crate::analysis::types::{Framework, FrameworkCategory, Language, Project};
    use crate::cli::ui::file_confirm::TerminalIO;
    use crate::error::ActualError;
    use crate::generation::markers;
    use crate::tailoring::types::{FileOutput, TailoringSummary};
    use serde::de::DeserializeOwned;
    use std::sync::atomic::{AtomicUsize, Ordering};
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

    /// Mock runner that returns a predetermined JSON response.
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

    impl ClaudeRunner for MockRunner {
        async fn run<T: DeserializeOwned + Send>(
            &self,
            _args: &[String],
        ) -> Result<T, ActualError> {
            let parsed: T = serde_json::from_str(&self.json_response)?;
            Ok(parsed)
        }
    }

    /// Mock reader that returns a fixed sequence of responses.
    struct MockInputReader {
        responses: Vec<String>,
        index: AtomicUsize,
    }

    impl MockInputReader {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                responses: responses.into_iter().map(String::from).collect(),
                index: AtomicUsize::new(0),
            }
        }

        /// A reader that immediately accepts.
        fn accept() -> Self {
            Self::new(vec!["a"])
        }

        /// A reader that immediately rejects.
        fn reject() -> Self {
            Self::new(vec!["r"])
        }
    }

    impl InputReader for MockInputReader {
        fn read_line(&self) -> std::io::Result<String> {
            let i = self.index.fetch_add(1, Ordering::SeqCst);
            if i < self.responses.len() {
                Ok(self.responses[i].clone())
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "no more input",
                ))
            }
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

    const MONOREPO_ANALYSIS_JSON: &str = r#"{
        "is_monorepo": true,
        "projects": [
            {
                "path": "apps/web",
                "name": "Web App",
                "languages": ["typescript"],
                "frameworks": [{"name": "nextjs", "category": "web-frontend"}],
                "package_manager": "npm"
            },
            {
                "path": "services/api",
                "name": "API Service",
                "languages": ["rust"],
                "frameworks": [],
                "package_manager": "cargo"
            }
        ]
    }"#;

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

    // ── resolve_cwd test ──

    #[test]
    fn test_resolve_cwd_returns_path() {
        let cwd = resolve_cwd();
        assert!(cwd.is_absolute() || cwd == std::path::PathBuf::from("."));
    }

    // ── run_sync flag-passthrough tests ──

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

    fn make_sync_args_with_projects(force: bool, projects: Vec<&str>) -> SyncArgs {
        SyncArgs {
            dry_run: false,
            full: false,
            force,
            reset_rejections: false,
            projects: projects.into_iter().map(String::from).collect(),
            model: None,
            api_url: None,
            verbose: false,
            no_tailor: false,
            max_budget_usd: None,
        }
    }

    #[test]
    fn test_run_sync_force_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let reader = MockInputReader::accept();
        let args = make_sync_args(false, false, true, false);
        let result = run_sync(&args, dir.path(), &term, &runner, &reader);
        assert!(result.is_ok(), "run_sync with --force should succeed");
        // With empty tailoring output + force, terminal should show "No files to write."
        let text = term.output_text();
        assert!(
            text.contains("No files to write."),
            "expected 'No files to write.' in: {text}"
        );
    }

    #[test]
    fn test_run_sync_no_tailor_force_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let reader = MockInputReader::accept();
        let args = make_sync_args(false, false, true, true);
        let result = run_sync(&args, dir.path(), &term, &runner, &reader);
        assert!(
            result.is_ok(),
            "run_sync with --no-tailor --force should succeed"
        );
    }

    #[test]
    fn test_run_sync_force_dry_run_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let reader = MockInputReader::accept();
        let args = make_sync_args(true, false, true, false);
        // dry-run takes precedence over force
        let result = run_sync(&args, dir.path(), &term, &runner, &reader);
        assert!(
            result.is_ok(),
            "run_sync with --force --dry-run should succeed"
        );
    }

    #[test]
    fn test_run_sync_no_flags_prompts_user() {
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec!["a"]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        // Accept project confirmation prompt
        let reader = MockInputReader::accept();
        let args = make_sync_args(false, false, false, false);
        let result = run_sync(&args, dir.path(), &term, &runner, &reader);
        assert!(
            result.is_ok(),
            "run_sync with no flags should succeed when user accepts"
        );
    }

    #[test]
    fn test_run_sync_user_rejects_returns_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        // Reject project confirmation prompt
        let reader = MockInputReader::reject();
        let args = make_sync_args(false, false, false, false);
        let result = run_sync(&args, dir.path(), &term, &runner, &reader);
        assert!(
            matches!(result, Err(ActualError::UserCancelled)),
            "run_sync should return UserCancelled when user rejects"
        );
    }

    #[test]
    fn test_run_sync_force_skips_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        // Reader would reject, but --force should skip it
        let reader = MockInputReader::reject();
        let args = make_sync_args(false, false, true, false);
        let result = run_sync(&args, dir.path(), &term, &runner, &reader);
        assert!(
            result.is_ok(),
            "run_sync with --force should skip confirmation and succeed"
        );
    }

    #[test]
    fn test_run_sync_dry_run_writes_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let reader = MockInputReader::accept();
        let args = make_sync_args(true, false, false, false);
        run_sync(&args, dir.path(), &term, &runner, &reader).unwrap();
        // Verify no CLAUDE.md was created
        assert!(
            !dir.path().join("CLAUDE.md").exists(),
            "dry-run should not create any files"
        );
    }

    // ── project filtering tests ──

    #[test]
    fn test_run_sync_project_filter_matches() {
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(MONOREPO_ANALYSIS_JSON);
        let reader = MockInputReader::accept();
        let args = make_sync_args_with_projects(true, vec!["apps/web"]);
        let result = run_sync(&args, dir.path(), &term, &runner, &reader);
        assert!(
            result.is_ok(),
            "run_sync with --project apps/web should succeed"
        );
    }

    #[test]
    fn test_run_sync_project_filter_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(MONOREPO_ANALYSIS_JSON);
        let reader = MockInputReader::accept();
        let args = make_sync_args_with_projects(true, vec!["nonexistent/path"]);
        let result = run_sync(&args, dir.path(), &term, &runner, &reader);
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
        let _lock = crate::testutil::ENV_MUTEX.lock().unwrap();
        std::env::set_var("CLAUDE_BINARY", "/nonexistent/path/to/claude");
        let args = make_sync_args(true, false, false, false);
        let code = exec(&args);
        std::env::remove_var("CLAUDE_BINARY");
        assert_eq!(code, 2, "expected exit code 2 (ClaudeNotFound)");
    }

    /// Exercises `run()` → `run_sync<CliClaudeRunner>` in the **unit-test binary**
    /// so that LLVM coverage counts the `CliClaudeRunner` monomorphization as hit.
    /// Without this test the generic instantiation exists (because `run()` references
    /// it) but is never called, causing 1 missed line in `llvm-cov report`.
    #[cfg(unix)]
    #[test]
    fn test_exec_with_fake_claude_binary() {
        use std::os::unix::fs::PermissionsExt;
        let _lock = crate::testutil::ENV_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");

        let auth_json = r#"{"loggedIn":true,"authMethod":"claude.ai","email":"t@t.com"}"#;
        let analysis_json = r#"{"is_monorepo":false,"projects":[{"path":".","name":"app","languages":["rust"],"frameworks":[],"package_manager":"cargo"}]}"#;

        let script = dir.path().join("fake-claude");
        let body = format!(
            "#!/bin/sh\nif [ \"$1\" = \"auth\" ]; then\nprintf '%s\\n' '{}'\nexit 0\n\
             elif [ \"$1\" = \"--print\" ]; then\nprintf '%s\\n' '{}'\nexit 0\n\
             else\nexit 1\nfi\n",
            auth_json, analysis_json,
        );
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        std::env::set_var("CLAUDE_BINARY", script.to_str().unwrap());
        std::env::set_var("ACTUAL_CONFIG", config_file.to_str().unwrap());

        let args = make_sync_args(false, false, true, false);
        let code = exec(&args);

        std::env::remove_var("CLAUDE_BINARY");
        std::env::remove_var("ACTUAL_CONFIG");

        assert_eq!(code, 0, "exec with fake Claude binary should succeed");
    }

    #[test]
    fn test_handle_result_ok() {
        assert_eq!(handle_result(Ok(())), 0);
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

    // ── MockInputReader edge case tests ──

    #[test]
    fn test_mock_input_reader_exhausted() {
        let reader = MockInputReader::new(vec!["one"]);
        // First call succeeds
        assert_eq!(reader.read_line().unwrap(), "one");
        // Second call returns an error because responses are exhausted
        let err = reader.read_line().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    // ── run_sync analysis error test ──

    #[test]
    fn test_run_sync_analysis_error() {
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        // Invalid JSON causes parse error in run_analysis_cached
        let runner = MockRunner::new("not valid json");
        let reader = MockInputReader::accept();
        let args = make_sync_args(false, false, true, false);
        let result = run_sync(&args, dir.path(), &term, &runner, &reader);
        assert!(
            matches!(result, Err(ActualError::ClaudeOutputParse(_))),
            "expected ClaudeOutputParse error, got: {result:?}"
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
