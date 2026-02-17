use std::collections::HashMap;
use std::path::Path;

use console::style;
use sha2::{Digest, Sha256};

use crate::analysis::cache::{get_git_head, run_analysis_cached};
use crate::analysis::confirm::ConfirmAction;
use crate::analysis::types::RepoAnalysis;
use crate::api::client::{build_match_request, ActualApiClient, DEFAULT_API_URL};
use crate::api::retry::{with_retry, RetryConfig};
use crate::branding::banner::print_banner;
use crate::claude::subprocess::ClaudeRunner;
use crate::cli::args::SyncArgs;
use crate::cli::ui::confirm::{format_project_summary, prompt_project_confirmation, InputReader};
use crate::cli::ui::diff::{format_diff_summary, FileDiff};
use crate::cli::ui::file_confirm::{confirm_files, TerminalIO};
use crate::cli::ui::progress::{Spinner, ERROR_SYMBOL, SUCCESS_SYMBOL};
use crate::config::paths::{load_from, save_to};
use crate::config::rejections::{clear_rejections, get_rejections};
use crate::error::ActualError;
use crate::generation::markers;
use crate::generation::writer::{write_files, WriteAction, WriteResult};
use crate::tailoring::concurrent::{tailor_all_projects, ConcurrentTailoringConfig};
use crate::tailoring::filter::pre_filter_rejected;
use crate::tailoring::types::{FileOutput, TailoringOutput, TailoringSummary};

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
/// Production wiring (`CliClaudeRunner`, `RealTerminal`, `StdinReader`) lives
/// in `real_terminal.rs` (which is excluded from coverage) so that the only
/// generic instantiation of `run_sync` in `sync.rs` is `MockRunner` from
/// unit tests.
pub fn exec(args: &SyncArgs) -> i32 {
    handle_result(crate::cli::ui::real_terminal::sync_run(args))
}

pub(crate) fn handle_result(result: Result<(), ActualError>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{} {}", style("Error:").red().bold(), e);
            e.exit_code()
        }
    }
}

/// Resolve the current working directory, falling back to `"."` if
/// unavailable (e.g. the directory was deleted while the process was running).
pub(crate) fn resolve_cwd() -> std::path::PathBuf {
    let fallback = std::path::PathBuf::from(".");
    std::env::current_dir().unwrap_or(fallback)
}

/// Core sync logic.
///
/// Accepts injected `root_dir`, `term`, `runner`, and `reader` so unit tests
/// can avoid `RealTerminal` (which blocks on stdin) and control the working
/// directory, Claude runner, and user input.
pub(crate) fn run_sync<R: ClaudeRunner>(
    args: &SyncArgs,
    root_dir: &Path,
    cfg_path: &Path,
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
    let rt = tokio::runtime::Runtime::new().expect("Failed to create async runtime");
    let analysis = rt.block_on(run_analysis_cached(
        runner,
        args.model.as_deref(),
        root_dir,
        cfg_path,
    ))?;
    spinner.success("Analysis complete");

    // 4. Filter by --project if specified
    let analysis = filter_projects(analysis, &args.projects)?;

    // 5. Confirmation (unless --force)
    if args.force {
        // Show project summary even in --force mode for visibility
        let summary = format_project_summary(&analysis);
        eprintln!("{summary}");
    } else {
        // prompt_project_confirmation displays the project summary itself
        let action = prompt_project_confirmation(&analysis, reader, &mut std::io::stderr());
        if matches!(action, ConfirmAction::Reject) {
            return Err(ActualError::UserCancelled);
        }
    }

    // ── Phase 2: fetch + tailor ──

    // 2a. Load config and handle --reset-rejections
    let mut config = load_from(cfg_path)?;
    let repo_key = compute_repo_key(root_dir);

    if args.reset_rejections {
        clear_rejections(&mut config, &repo_key);
        save_to(&config, cfg_path)?;
        eprintln!(
            "{} Cleared ADR rejection memory for this repository",
            style(SUCCESS_SYMBOL).green()
        );
    }

    let rejected_ids = get_rejections(&config, &repo_key);

    // 2b. Fetch ADRs from API with retry
    let api_url = args
        .api_url
        .as_deref()
        .or(config.api_url.as_deref())
        .unwrap_or(DEFAULT_API_URL);

    let request = build_match_request(&analysis, &config);
    let client = ActualApiClient::new(api_url);

    if args.verbose {
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
    }

    let spinner = Spinner::new("Fetching ADRs...", false);
    let response = rt.block_on(async {
        with_retry(&RetryConfig::default(), || client.post_match(&request)).await
    });

    let response = match response {
        Ok(r) => {
            spinner.success(&format!("Fetched {} ADRs", r.matched_adrs.len()));
            r
        }
        Err(e) => {
            spinner.error(&format!("API request failed: {e}"));
            return Err(e);
        }
    };

    if args.verbose {
        eprintln!(
            "  matched: {}, by_framework: {:?}",
            response.metadata.total_matched, response.metadata.by_framework
        );
    }

    // 2c. Filter by rejections
    let filtered_adrs = pre_filter_rejected(&response.matched_adrs, &rejected_ids);

    if !rejected_ids.is_empty() && args.verbose {
        let removed = response.matched_adrs.len() - filtered_adrs.len();
        eprintln!("  filtered out {removed} previously rejected ADRs");
    }

    // 2d. Tailor or skip (--no-tailor)
    let existing_paths = find_existing_claude_md(root_dir);

    let output = if args.no_tailor {
        raw_adrs_to_output(&filtered_adrs)
    } else {
        let spinner = Spinner::new("Tailoring ADRs...", false);
        let tailoring_config = ConcurrentTailoringConfig {
            concurrency: config.concurrency.unwrap_or(3),
            batch_size: config.batch_size.unwrap_or(15),
            existing_claude_md_paths: &existing_paths,
            model_override: args.model.as_deref(),
            max_budget_usd: args.max_budget_usd.or(config.max_budget_usd),
        };
        let result = rt.block_on(tailor_all_projects(
            runner,
            &analysis.projects,
            &filtered_adrs,
            &tailoring_config,
        ));
        match result {
            Ok(output) => {
                spinner.success(&format!(
                    "Tailored {} ADRs into {} files",
                    output.summary.applicable, output.summary.files_generated
                ));
                output
            }
            Err(e) => {
                spinner.error(&format!("Tailoring failed: {e}"));
                return Err(e);
            }
        }
    };

    // ── Phase 3: confirm + write (fully implemented) ──
    confirm_and_write(&output, root_dir, args.force, args.dry_run, args.full, term)?;
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
/// if the remote URL cannot be determined.
fn compute_repo_key(root_dir: &Path) -> String {
    let input = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(root_dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| root_dir.to_string_lossy().to_string());

    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Convert raw ADRs into a [`TailoringOutput`] without Claude tailoring.
///
/// Groups ADRs by their `matched_projects` field. Each group produces a
/// `FileOutput` whose content is the ADR policies/instructions formatted as
/// markdown. ADRs with no `matched_projects` are assigned to root `CLAUDE.md`.
fn raw_adrs_to_output(adrs: &[crate::api::types::Adr]) -> TailoringOutput {
    let mut project_adrs: HashMap<String, Vec<&crate::api::types::Adr>> = HashMap::new();

    for adr in adrs {
        if adr.matched_projects.is_empty() {
            project_adrs
                .entry("CLAUDE.md".to_string())
                .or_default()
                .push(adr);
        } else {
            for project_path in &adr.matched_projects {
                let file_path = if project_path == "." {
                    "CLAUDE.md".to_string()
                } else {
                    format!("{project_path}/CLAUDE.md")
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
                let mut section = format!("## {}\n", adr.title);
                for policy in &adr.policies {
                    section.push_str(&format!("- {policy}\n"));
                }
                if let Some(instructions) = &adr.instructions {
                    if !instructions.is_empty() {
                        section.push('\n');
                        for instruction in instructions {
                            section.push_str(&format!("- {instruction}\n"));
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

/// Scan the repository for existing CLAUDE.md files and return their
/// contents concatenated as a string, for use as context during tailoring.
fn find_existing_claude_md(root_dir: &Path) -> String {
    let mut results = Vec::new();

    fn walk(dir: &Path, root: &Path, results: &mut Vec<String>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Skip hidden directories and common non-project dirs
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with('.')
                    || name_str == "node_modules"
                    || name_str == "target"
                    || name_str == "dist"
                    || name_str == "build"
                {
                    continue;
                }
                walk(&path, root, results);
            } else if entry.file_name() == "CLAUDE.md" {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy();
                    let cleaned = markers::strip_managed_metadata(&content);
                    results.push(format!("=== {rel} ===\n{cleaned}"));
                }
            }
        }
    }

    walk(root_dir, root_dir, &mut results);
    results.sort();
    results.join("\n\n")
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
        }
    }

    #[test]
    fn test_run_sync_force_returns_ok() {
        let server = mock_api_server();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let reader = MockInputReader::accept();
        let args = make_sync_args(false, false, true, false, &server.url());
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            &reader,
        );
        assert!(result.is_ok(), "run_sync with --force should succeed");
        // With empty API response + force, terminal should show "No files to write."
        let text = term.output_text();
        assert!(
            text.contains("No files to write."),
            "expected 'No files to write.' in: {text}"
        );
    }

    #[test]
    fn test_run_sync_no_tailor_force_returns_ok() {
        let server = mock_api_server();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let reader = MockInputReader::accept();
        let args = make_sync_args(false, false, true, true, &server.url());
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            &reader,
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
        let reader = MockInputReader::accept();
        let args = make_sync_args(true, false, true, false, &server.url());
        // dry-run takes precedence over force
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            &reader,
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
        let term = MockTerminal::new(vec!["a"]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        // Accept project confirmation prompt
        let reader = MockInputReader::accept();
        let args = make_sync_args(false, false, false, false, &server.url());
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            &reader,
        );
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
        // Reject project confirmation prompt — this happens before API call
        let reader = MockInputReader::reject();
        let args = make_sync_args(false, false, false, false, "http://unused");
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            &reader,
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
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        // Reader would reject, but --force should skip it
        let reader = MockInputReader::reject();
        let args = make_sync_args(false, false, true, false, &server.url());
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            &reader,
        );
        assert!(
            result.is_ok(),
            "run_sync with --force should skip confirmation and succeed"
        );
    }

    #[test]
    fn test_run_sync_dry_run_writes_no_files() {
        let server = mock_api_server();
        let dir = tempfile::tempdir().unwrap();
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let reader = MockInputReader::accept();
        let args = make_sync_args(true, false, false, false, &server.url());
        run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            &reader,
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
        let term = MockTerminal::new(vec![]);
        let runner = MockRunner::new(MONOREPO_ANALYSIS_JSON);
        let reader = MockInputReader::accept();
        let args = make_sync_args_with_projects(true, vec!["apps/web"], &server.url());
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            &reader,
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
        let runner = MockRunner::new(MONOREPO_ANALYSIS_JSON);
        let reader = MockInputReader::accept();
        // No server needed — fails before API call
        let args = make_sync_args_with_projects(true, vec!["nonexistent/path"], "http://unused");
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            &reader,
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
        let _lock = crate::testutil::ENV_MUTEX.lock().unwrap();
        std::env::set_var("CLAUDE_BINARY", "/nonexistent/path/to/claude");
        let args = make_sync_args(true, false, false, false, "http://unused");
        let code = exec(&args);
        std::env::remove_var("CLAUDE_BINARY");
        assert_eq!(code, 2, "expected exit code 2 (ClaudeNotFound)");
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
        // Invalid JSON causes parse error in run_analysis_cached — before API call
        let runner = MockRunner::new("not valid json");
        let reader = MockInputReader::accept();
        let args = make_sync_args(false, false, true, false, "http://unused");
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            &reader,
        );
        assert!(
            matches!(result, Err(ActualError::ClaudeOutputParse(_))),
            "expected ClaudeOutputParse error, got: {result:?}"
        );
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
        // MockTerminal: user quits the confirmation flow
        let term = MockTerminal::new(vec!["q"]);
        let runner = MockRunner::new(VALID_ANALYSIS_JSON);
        let reader = MockInputReader::accept();
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
        };
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            &reader,
        );

        assert!(
            matches!(result, Err(ActualError::UserCancelled)),
            "expected UserCancelled from confirm_and_write, got: {result:?}"
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
        let output = raw_adrs_to_output(&[]);
        assert!(output.files.is_empty());
        assert_eq!(output.summary.total_input, 0);
        assert_eq!(output.summary.applicable, 0);
        assert_eq!(output.summary.files_generated, 0);
    }

    #[test]
    fn test_raw_adrs_to_output_single_adr_no_projects() {
        let adrs = vec![make_test_adr("adr-001", "Test ADR", vec![])];
        let output = raw_adrs_to_output(&adrs);
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
        let output = raw_adrs_to_output(&adrs);
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "apps/web/CLAUDE.md");
    }

    #[test]
    fn test_raw_adrs_to_output_adr_with_dot_project() {
        let adrs = vec![make_test_adr("adr-001", "Root Rules", vec!["."])];
        let output = raw_adrs_to_output(&adrs);
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
        let output = raw_adrs_to_output(&adrs);
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
        let output = raw_adrs_to_output(&[adr]);
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
        let output = raw_adrs_to_output(&[adr]);
        assert_eq!(output.files.len(), 1);
        assert!(output.files[0].content.contains("Policy for Test ADR"));
        // With empty instructions vec, no instruction lines should appear
        let content = &output.files[0].content;
        assert!(
            !content.contains("Instruction for"),
            "expected no instruction lines"
        );
    }

    // ── find_existing_claude_md tests ──

    #[test]
    fn test_find_existing_claude_md_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = find_existing_claude_md(dir.path());
        assert!(result.is_empty());
    }

    #[test]
    fn test_find_existing_claude_md_finds_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "Root content").unwrap();
        let result = find_existing_claude_md(dir.path());
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
        let result = find_existing_claude_md(dir.path());
        assert!(result.contains("Web content"));
    }

    #[test]
    fn test_find_existing_claude_md_skips_hidden_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git").join("CLAUDE.md"), "Git content").unwrap();
        let result = find_existing_claude_md(dir.path());
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
        let result = find_existing_claude_md(dir.path());
        assert!(result.is_empty());
    }

    #[test]
    fn test_find_existing_claude_md_strips_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let content = "# My Rules\n\n<!-- managed:actual-start -->\n<!-- last-synced: 2024-01-01T00:00:00Z -->\n<!-- version: 1 -->\n<!-- adr-ids: abc-123,def-456 -->\n\n## Some Rules\n\nDo this.\n\n<!-- managed:actual-end -->";
        std::fs::write(dir.path().join("CLAUDE.md"), content).unwrap();
        let result = find_existing_claude_md(dir.path());
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
        let result = find_existing_claude_md(dir.path());
        assert!(result.is_empty());
        // Restore permissions for cleanup
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o755)).unwrap();
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
        let reader = MockInputReader::accept();
        let args = make_sync_args(false, false, true, false, &server.url());
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            &reader,
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
        let reader = MockInputReader::accept();
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
        };
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            &reader,
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
        let reader = MockInputReader::accept();
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
        };
        // This should succeed and print verbose output to stderr
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            &reader,
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
        let reader = MockInputReader::accept();
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
        };
        let result = run_sync(&args, dir.path(), &cfg_path, &term, &runner, &reader);
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
        let reader = MockInputReader::accept();
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
        };
        let result = run_sync(&args, dir.path(), &cfg_path, &term, &runner, &reader);
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
        let reader = MockInputReader::accept();
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
        };
        let result = run_sync(
            &args,
            dir.path(),
            &dir.path().join("config.yaml"),
            &term,
            &runner,
            &reader,
        );
        assert!(result.is_err(), "expected tailoring to fail");
    }
}
