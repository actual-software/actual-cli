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
            if let Ok(meta) = entry.metadata() {
                if meta.len() > MAX_FILE_BYTES {
                    continue;
                }
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
    let mut results = HashMap::new();

    // Build TreeSitterAnalyzer from embedded query packs (shared across projects).
    let mut ts_analyzer = match TreeSitterAnalyzer::from_embedded() {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(
                "failed to load tree-sitter query packs: {e}; skipping signals analysis"
            );
            return results;
        }
    };

    // Build SemgrepScanner (needs semgrep in PATH).
    let scanner = match SemgrepScanner::new(Duration::from_secs(60)) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!("semgrep not available: {e}; tree-sitter signals only");
            None
        }
    };

    // Extract embedded semgrep rules to temp dir (once, shared across projects).
    let (_rule_tmp, rule_paths) = if scanner.is_some() {
        match extract_embedded_rules() {
            Ok(r) => (Some(r.0), r.1),
            Err(e) => {
                tracing::warn!("failed to extract semgrep rules: {e}; tree-sitter signals only");
                (None, Vec::new())
            }
        }
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

        match ts_analyzer.query_file(&content, &rel, *lang) {
            Ok(matches) => all_matches.extend(matches),
            Err(e) => tracing::debug!(file = %rel, "tree-sitter query failed: {e}"),
        }
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
}
