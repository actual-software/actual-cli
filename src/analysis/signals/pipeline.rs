use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::analysis::types::RepoAnalysis;
use super::ir::{build_canonical_ir, CanonicalIR};
use super::tree_sitter::TreeSitterAnalyzer;
use super::semgrep::{extract_embedded_rules, SemgrepScanner};
use super::language_resolver::TreeSitterLanguage;

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
    "node_modules", "target", ".git", "dist", "build", ".next",
    "__pycache__", ".venv", "venv", "vendor", ".cargo",
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
                tracing::warn!(
                    "failed to extract semgrep rules: {e}; tree-sitter signals only"
                );
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

    Ok(build_canonical_ir(project_path, &primary_lang, &all_matches))
}
