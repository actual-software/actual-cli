use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::ir::{build_canonical_ir, CanonicalIR};
use super::language_resolver::TreeSitterLanguage;
use super::semgrep::{extract_embedded_rules, SemgrepScanner};
use super::tree_sitter::TreeSitterAnalyzer;
use crate::analysis::types::RepoAnalysis;

/// File extensions to include in signals analysis, mapped to TreeSitterLanguage.
fn extension_to_language(ext: &str) -> Option<TreeSitterLanguage> {
    match ext {
        "rs" => Some(TreeSitterLanguage::Rust),
        "ts" => Some(TreeSitterLanguage::TypeScript),
        "tsx" => Some(TreeSitterLanguage::Tsx),
        "js" | "jsx" | "mjs" | "cjs" => Some(TreeSitterLanguage::JavaScript),
        "py" => Some(TreeSitterLanguage::Python),
        "go" => Some(TreeSitterLanguage::Go),
        "java" => Some(TreeSitterLanguage::Java),
        "cs" => Some(TreeSitterLanguage::CSharp),
        "cpp" | "cc" | "cxx" => Some(TreeSitterLanguage::Cpp),
        "c" => Some(TreeSitterLanguage::C),
        "php" => Some(TreeSitterLanguage::Php),
        _ => None,
    }
}

/// Directories to exclude when walking for source files.
const EXCLUDED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    "dist",
    "build",
    ".next",
    "__pycache__",
    ".venv",
    "venv",
    "vendor",
    ".cargo",
];

/// Max source file size to analyze (500 KB).
const MAX_FILE_BYTES: u64 = 512 * 1024;

/// Walk a directory and collect (path, language) pairs for all source files.
fn collect_source_files(dir: &Path) -> Vec<(PathBuf, TreeSitterLanguage)> {
    let mut files = Vec::new();
    walk_dir(dir, &mut files);
    files
}

fn walk_dir(current: &Path, out: &mut Vec<(PathBuf, TreeSitterLanguage)>) {
    let Ok(entries) = std::fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if path.is_dir() {
            if EXCLUDED_DIRS.contains(&name_str.as_ref()) {
                continue;
            }
            walk_dir(&path, out);
        } else if path.is_file() {
            // Skip large files
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if size > MAX_FILE_BYTES {
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if let Some(lang) = extension_to_language(ext) {
                out.push((path, lang));
            }
        }
    }
}

/// Run the full signals analysis for all projects in a RepoAnalysis.
/// Returns a map from project path → CanonicalIR.
/// Errors are non-fatal: if tree-sitter or semgrep fails for a project,
/// that project is skipped with a warning.
pub async fn run_signals_analysis(
    working_dir: &Path,
    analysis: &RepoAnalysis,
) -> HashMap<String, CanonicalIR> {
    // Build TreeSitterAnalyzer from embedded query packs (shared across projects).
    // Build SemgrepScanner (needs semgrep in PATH; None if unavailable).
    let ts_result = TreeSitterAnalyzer::from_embedded();
    let scanner = SemgrepScanner::new(Duration::from_secs(60))
        .inspect_err(|e| tracing::warn!("semgrep not available: {e}; tree-sitter signals only"))
        .ok();

    run_signals_analysis_inner(working_dir, analysis, ts_result, scanner).await
}

/// Inner implementation of signals analysis.
///
/// Accepts the tree-sitter analyzer as a `Result` so that tests can inject a
/// pre-built analyzer (passing `Ok(analyzer)`) or exercise the failure path
/// (passing `Err(...)`) without going through `TreeSitterAnalyzer::from_embedded()`.
async fn run_signals_analysis_inner(
    working_dir: &Path,
    analysis: &RepoAnalysis,
    ts_result: anyhow::Result<TreeSitterAnalyzer>,
    scanner: Option<SemgrepScanner>,
) -> HashMap<String, CanonicalIR> {
    let mut ts_analyzer = match ts_result {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(
                "failed to load tree-sitter query packs: {e}; skipping signals analysis"
            );
            return HashMap::new();
        }
    };

    let mut results = HashMap::new();

    // Extract embedded semgrep rules to temp dir (once, shared across projects).
    let (_rule_tmp, rule_paths) = if scanner.is_some() {
        extract_embedded_rules()
            .inspect_err(|e| {
                tracing::warn!("failed to extract semgrep rules: {e}; tree-sitter signals only");
            })
            .ok()
            .map_or((None, Vec::new()), |r| (Some(r.0), r.1))
    } else {
        (None, Vec::new())
    };

    for project in &analysis.projects {
        let project_dir = working_dir.join(&project.path);

        match analyze_project(
            &project_dir,
            &project.path,
            &mut ts_analyzer,
            scanner.as_ref(),
            &rule_paths,
        )
        .await
        {
            Ok(ir) => {
                results.insert(project.path.clone(), ir);
            }
            Err(e) => {
                tracing::warn!(project = %project.path, "signals analysis failed: {e}");
            }
        }
    }

    results
}

async fn analyze_project(
    project_dir: &Path,
    project_path: &str,
    ts_analyzer: &mut TreeSitterAnalyzer,
    scanner: Option<&SemgrepScanner>,
    rule_paths: &[PathBuf],
) -> anyhow::Result<CanonicalIR> {
    let source_files = collect_source_files(project_dir);
    if source_files.is_empty() {
        anyhow::bail!("no source files found");
    }

    let mut all_matches = Vec::new();

    // Tree-sitter analysis — per file.
    for (file_path, lang) in &source_files {
        let rel = file_path
            .strip_prefix(project_dir)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        let Ok(content) = std::fs::read_to_string(file_path) else {
            continue;
        };

        all_matches.extend(
            ts_analyzer
                .query_file(&content, &rel, *lang)
                .unwrap_or_default(),
        );
    }

    // Semgrep analysis — batch.
    if let Some(scanner) = scanner {
        if !rule_paths.is_empty() {
            let batch: Vec<(PathBuf, String)> = source_files
                .iter()
                .filter_map(|(p, _)| {
                    let rel = p.strip_prefix(project_dir).unwrap_or(p).to_path_buf();
                    std::fs::read_to_string(p).ok().map(|c| (rel, c))
                })
                .collect();

            match scanner.scan_batch(&batch, rule_paths).await {
                Ok(matches) => all_matches.extend(matches),
                Err(e) => {
                    tracing::warn!(project = %project_path, "semgrep scan failed: {e}")
                }
            }
        }
    }

    // Determine primary language (most files as a proxy for LOC).
    let primary_lang = {
        let mut lang_counts: HashMap<&str, usize> = HashMap::new();
        for (_, lang) in &source_files {
            *lang_counts.entry(lang.as_str()).or_insert(0) += 1;
        }
        lang_counts
            .into_iter()
            .max_by_key(|(_, c)| *c)
            .map(|(l, _)| l.to_string())
            .unwrap_or_default()
    };

    Ok(build_canonical_ir(
        project_path,
        &primary_lang,
        &all_matches,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::types::{Project, RepoAnalysis};
    use std::fs;
    use tempfile::TempDir;

    fn make_temp_project(files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new().unwrap();
        for (name, content) in files {
            let path = dir.path().join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, content).unwrap();
        }
        dir
    }

    fn make_project(path: &str) -> Project {
        Project {
            path: path.to_string(),
            name: "test".to_string(),
            languages: vec![],
            frameworks: vec![],
            package_manager: None,
            description: None,
            dep_count: 0,
            dev_dep_count: 0,
            selection: None,
        }
    }

    #[test]
    fn extension_to_language_known() {
        assert!(extension_to_language("rs").is_some());
        assert!(extension_to_language("ts").is_some());
        assert!(extension_to_language("tsx").is_some());
        assert!(extension_to_language("js").is_some());
        assert!(extension_to_language("jsx").is_some());
        assert!(extension_to_language("mjs").is_some());
        assert!(extension_to_language("cjs").is_some());
        assert!(extension_to_language("py").is_some());
        assert!(extension_to_language("go").is_some());
        assert!(extension_to_language("java").is_some());
        assert!(extension_to_language("cs").is_some());
        assert!(extension_to_language("cpp").is_some());
        assert!(extension_to_language("cc").is_some());
        assert!(extension_to_language("cxx").is_some());
        assert!(extension_to_language("c").is_some());
        assert!(extension_to_language("php").is_some());
    }

    #[test]
    fn extension_to_language_unknown() {
        assert!(extension_to_language("xyz").is_none());
        assert!(extension_to_language("md").is_none());
        assert!(extension_to_language("").is_none());
        assert!(extension_to_language("txt").is_none());
        assert!(extension_to_language("json").is_none());
    }

    #[test]
    fn collect_source_files_finds_rust_files() {
        let dir = make_temp_project(&[("src/main.rs", "fn main() {}"), ("README.md", "# readme")]);
        let files = collect_source_files(dir.path());
        assert_eq!(files.len(), 1);
        assert!(files[0].0.to_string_lossy().contains("main.rs"));
    }

    #[test]
    fn collect_source_files_excludes_node_modules() {
        let dir = make_temp_project(&[
            ("src/index.ts", "export const x = 1;"),
            ("node_modules/foo/index.ts", "// ignored"),
        ]);
        let files = collect_source_files(dir.path());
        assert_eq!(files.len(), 1);
        assert!(files[0].0.to_string_lossy().contains("index.ts"));
        assert!(!files[0].0.to_string_lossy().contains("node_modules"));
    }

    #[test]
    fn collect_source_files_excludes_target_dir() {
        let dir = make_temp_project(&[
            ("src/lib.rs", "pub fn foo() {}"),
            ("target/debug/build.rs", "fn main() {}"),
        ]);
        let files = collect_source_files(dir.path());
        assert_eq!(files.len(), 1);
        assert!(!files[0].0.to_string_lossy().contains("target"));
    }

    #[test]
    fn collect_source_files_empty_dir() {
        let dir = TempDir::new().unwrap();
        let files = collect_source_files(dir.path());
        assert!(files.is_empty());
    }

    #[test]
    fn collect_source_files_skips_non_source_files() {
        let dir = make_temp_project(&[
            ("README.md", "# readme"),
            ("Cargo.toml", "[package]"),
            ("src/main.rs", "fn main() {}"),
        ]);
        let files = collect_source_files(dir.path());
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn collect_source_files_multiple_languages() {
        let dir = make_temp_project(&[
            ("src/main.rs", "fn main() {}"),
            ("src/app.ts", "const x = 1;"),
            ("src/util.py", "def foo(): pass"),
            ("src/server.go", "package main"),
        ]);
        let files = collect_source_files(dir.path());
        assert_eq!(files.len(), 4);
    }

    #[test]
    fn collect_source_files_skips_large_files() {
        let dir = TempDir::new().unwrap();
        // Write a file larger than MAX_FILE_BYTES (512 * 1024 = 524288 bytes)
        let large_content = "x".repeat(512 * 1024 + 1);
        fs::write(dir.path().join("large.rs"), large_content).unwrap();
        fs::write(dir.path().join("small.rs"), "fn main() {}").unwrap();
        let files = collect_source_files(dir.path());
        assert_eq!(files.len(), 1);
        assert!(files[0].0.to_string_lossy().contains("small.rs"));
    }

    #[test]
    fn walk_dir_unreadable_dir_does_not_panic() {
        // Pass a nonexistent directory — walk_dir should return early without panic
        let nonexistent = std::path::Path::new("/nonexistent/does/not/exist");
        let mut out = Vec::new();
        walk_dir(nonexistent, &mut out);
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn run_signals_analysis_empty_project_dir() {
        let dir = TempDir::new().unwrap();
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![make_project(".")],
        };
        // Empty dir → no source files → analyze_project returns Err → project skipped
        let results = run_signals_analysis(dir.path(), &analysis).await;
        assert!(results.is_empty(), "empty project should produce no IR");
    }

    #[tokio::test]
    async fn run_signals_analysis_no_projects() {
        let dir = TempDir::new().unwrap();
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![],
        };
        let results = run_signals_analysis(dir.path(), &analysis).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn run_signals_analysis_with_rust_file() {
        let dir = make_temp_project(&[(
            "src/main.rs",
            "pub fn greet(name: &str) -> String { format!(\"Hello, {}!\", name) }",
        )]);
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![make_project(".")],
        };
        let results = run_signals_analysis(dir.path(), &analysis).await;
        // Should produce an IR for the "." project (tree-sitter at minimum)
        assert!(results.contains_key("."), "expected IR for root project");
    }

    #[tokio::test]
    async fn run_signals_analysis_with_multiple_projects() {
        let dir = TempDir::new().unwrap();
        // Create two sub-project directories
        fs::create_dir_all(dir.path().join("backend/src")).unwrap();
        fs::write(dir.path().join("backend/src/main.rs"), "pub fn serve() {}").unwrap();
        fs::create_dir_all(dir.path().join("frontend/src")).unwrap();
        fs::write(
            dir.path().join("frontend/src/app.ts"),
            "export const app = {};",
        )
        .unwrap();

        let analysis = RepoAnalysis {
            is_monorepo: true,
            workspace_type: None,
            projects: vec![make_project("backend"), make_project("frontend")],
        };
        let results = run_signals_analysis(dir.path(), &analysis).await;
        assert!(results.contains_key("backend"), "expected IR for backend");
        assert!(results.contains_key("frontend"), "expected IR for frontend");
    }

    // ---- analyze_project direct tests ----

    #[tokio::test]
    async fn analyze_project_no_scanner_no_rules() {
        let dir = make_temp_project(&[("src/main.rs", "pub fn hello() {}")]);
        let mut ts_analyzer =
            TreeSitterAnalyzer::from_embedded().expect("from_embedded should succeed");
        let result = analyze_project(dir.path(), ".", &mut ts_analyzer, None, &[]).await;
        assert!(
            result.is_ok(),
            "analyze_project should succeed: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn analyze_project_returns_err_when_no_source_files() {
        // An empty dir has no source files → bail!("no source files found")
        let dir = TempDir::new().unwrap();
        let mut ts_analyzer =
            TreeSitterAnalyzer::from_embedded().expect("from_embedded should succeed");
        let result = analyze_project(dir.path(), ".", &mut ts_analyzer, None, &[]).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("no source files found"));
    }

    #[tokio::test]
    async fn analyze_project_with_scanner_and_empty_rule_paths() {
        // scanner.is_some() but rule_paths is empty → inner semgrep block is skipped
        // (covers `if let Some(scanner)` = Some branch AND `if !rule_paths.is_empty()` = false)
        let dir = make_temp_project(&[("src/main.rs", "pub fn hello() {}")]);
        let mut ts_analyzer =
            TreeSitterAnalyzer::from_embedded().expect("from_embedded should succeed");

        // Build a fake scanner pointing to /bin/true (or /usr/bin/true) which
        // will not be invoked since rule_paths is empty.
        let true_bin = which::which("true").unwrap_or_else(|_| PathBuf::from("/usr/bin/true"));
        let scanner = SemgrepScanner::with_binary_for_pipeline_test(
            true_bin,
            std::time::Duration::from_secs(5),
        );
        let result = analyze_project(dir.path(), ".", &mut ts_analyzer, Some(&scanner), &[]).await;
        assert!(
            result.is_ok(),
            "should succeed with scanner present but no rule_paths: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn analyze_project_with_scanner_and_rule_paths_scan_ok() {
        // scanner.is_some() AND rule_paths is non-empty → scan_batch is called.
        // Use a fake semgrep script that returns empty results so we can cover
        // lines 184-199 (the semgrep batch section).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let dir = make_temp_project(&[("src/main.rs", "pub fn hello() {}")]);
            let mut ts_analyzer =
                TreeSitterAnalyzer::from_embedded().expect("from_embedded should succeed");

            // Create a fake semgrep script that outputs valid empty results.
            let script_dir = TempDir::new().unwrap();
            let script_path = script_dir.path().join("fake_semgrep.sh");
            fs::write(
                &script_path,
                "#!/bin/sh\necho '{\"results\": [], \"errors\": []}'",
            )
            .unwrap();
            fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

            // Create a fake rule file so rule_paths is non-empty.
            let rule_path = script_dir.path().join("test_rule.yml");
            fs::write(&rule_path, "rules: []").unwrap();

            let scanner = SemgrepScanner::with_binary_for_pipeline_test(
                script_path,
                std::time::Duration::from_secs(10),
            );
            let result = analyze_project(
                dir.path(),
                ".",
                &mut ts_analyzer,
                Some(&scanner),
                &[rule_path],
            )
            .await;
            assert!(
                result.is_ok(),
                "analyze_project with fake semgrep should succeed: {:?}",
                result
            );
        }
    }

    #[tokio::test]
    async fn analyze_project_with_scanner_scan_batch_error() {
        // scanner.is_some() AND rule_paths non-empty AND scan_batch returns Err
        // → covers lines 195-197 (the Err arm of scan_batch match).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let dir = make_temp_project(&[("src/main.rs", "pub fn hello() {}")]);
            let mut ts_analyzer =
                TreeSitterAnalyzer::from_embedded().expect("from_embedded should succeed");

            // Create a fake semgrep script that exits non-zero (causes scan error).
            let script_dir = TempDir::new().unwrap();
            let script_path = script_dir.path().join("failing_semgrep.sh");
            fs::write(&script_path, "#!/bin/sh\nexit 1").unwrap();
            fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

            // Create a fake rule file so rule_paths is non-empty.
            let rule_path = script_dir.path().join("test_rule.yml");
            fs::write(&rule_path, "rules: []").unwrap();

            let scanner = SemgrepScanner::with_binary_for_pipeline_test(
                script_path,
                std::time::Duration::from_secs(10),
            );
            // analyze_project should still succeed (semgrep error is non-fatal,
            // tree-sitter results are returned).
            let result = analyze_project(
                dir.path(),
                ".",
                &mut ts_analyzer,
                Some(&scanner),
                &[rule_path],
            )
            .await;
            assert!(
                result.is_ok(),
                "semgrep scan error should be non-fatal: {:?}",
                result
            );
        }
    }

    #[test]
    fn walk_dir_skips_symlinks_to_nonexistent_targets() {
        // Create a directory with a broken symlink — the entry is neither
        // is_dir() nor is_file(), so it should be silently skipped.
        let dir = TempDir::new().unwrap();
        // Write a real source file so the overall collect is non-empty
        fs::write(dir.path().join("real.rs"), "fn foo() {}").unwrap();
        // Create a broken symlink (target does not exist)
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink("/nonexistent/broken_target", dir.path().join("broken.rs")).unwrap();
        }
        // On non-Unix the symlink creation is skipped; the test still covers
        // the real file path and walk_dir's happy path.
        let files = collect_source_files(dir.path());
        // Only the real file should be present; the broken symlink is skipped.
        assert_eq!(files.len(), 1);
    }

    #[tokio::test]
    async fn run_signals_analysis_project_dir_not_exist() {
        // If the project dir doesn't exist, collect_source_files returns empty →
        // analyze_project returns Err → project is skipped (covers the Err branch).
        let dir = TempDir::new().unwrap();
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![make_project("nonexistent_subdir")],
        };
        let results = run_signals_analysis(dir.path(), &analysis).await;
        assert!(
            results.is_empty(),
            "nonexistent project dir should produce no IR"
        );
    }

    // ---- run_signals_analysis_inner tests (inject pre-built components) ----

    #[tokio::test]
    async fn run_signals_analysis_inner_ts_analyzer_err() {
        // When the ts_analyzer Result is Err, run_signals_analysis_inner should
        // return an empty map immediately (covers the Err branch of the match).
        let dir = TempDir::new().unwrap();
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![make_project(".")],
        };
        let ts_err: anyhow::Result<TreeSitterAnalyzer> =
            Err(anyhow::anyhow!("simulated from_embedded failure"));
        let results = run_signals_analysis_inner(dir.path(), &analysis, ts_err, None).await;
        assert!(results.is_empty(), "Err ts_result should produce empty map");
    }

    #[tokio::test]
    async fn run_signals_analysis_inner_no_scanner() {
        // ts_analyzer succeeds, scanner is None → extract_embedded_rules is skipped
        // (covers the `else` branch of `if scanner.is_some()`).
        let dir = make_temp_project(&[("src/main.rs", "pub fn hello() {}")]);
        let ts_analyzer =
            TreeSitterAnalyzer::from_embedded().expect("from_embedded should succeed");
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![make_project(".")],
        };
        let results =
            run_signals_analysis_inner(dir.path(), &analysis, Ok(ts_analyzer), None).await;
        assert!(results.contains_key("."), "expected IR for root project");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_signals_analysis_skips_unreadable_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        // Create one readable and one unreadable Rust file
        let readable = dir.path().join("lib.rs");
        let unreadable = dir.path().join("secret.rs");
        std::fs::write(&readable, "pub fn ok() {}").unwrap();
        std::fs::write(&unreadable, "pub fn secret() {}").unwrap();
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();

        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![make_project(".")],
        };
        // Should produce an IR (from the readable file) rather than failing entirely
        let results = run_signals_analysis(dir.path(), &analysis).await;
        assert!(results.contains_key("."));

        // Restore permissions so TempDir can clean up
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[tokio::test]
    async fn run_signals_analysis_inner_with_scanner_no_semgrep_rules() {
        // scanner.is_some() → extract_embedded_rules is called (covers the
        // `if scanner.is_some()` branch and the Ok arm of extract_embedded_rules).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let dir = make_temp_project(&[("src/main.rs", "pub fn hello() {}")]);
            let ts_analyzer =
                TreeSitterAnalyzer::from_embedded().expect("from_embedded should succeed");

            // Create a fake semgrep script that outputs valid empty results.
            let script_dir = TempDir::new().unwrap();
            let script_path = script_dir.path().join("fake_semgrep.sh");
            fs::write(
                &script_path,
                "#!/bin/sh\necho '{\"results\": [], \"errors\": []}'",
            )
            .unwrap();
            fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

            let scanner = SemgrepScanner::with_binary_for_pipeline_test(
                script_path,
                std::time::Duration::from_secs(10),
            );
            let analysis = RepoAnalysis {
                is_monorepo: false,
                workspace_type: None,
                projects: vec![make_project(".")],
            };
            let results =
                run_signals_analysis_inner(dir.path(), &analysis, Ok(ts_analyzer), Some(scanner))
                    .await;
            assert!(results.contains_key("."), "expected IR for root project");
        }
    }
}
