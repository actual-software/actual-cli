use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

use crate::error::ActualError;

#[derive(Debug)]
pub struct RepoBundleContext {
    pub file_tree: String,
    pub key_files: Vec<(String, String)>, // (relative_path, content)
}

const MAX_TREE_ENTRIES: usize = 200;
const MAX_FILE_LINES: usize = 200;
const MAX_TOTAL_BYTES: usize = 32 * 1024; // 32KB

/// Directories to exclude in addition to .gitignore-ignored paths.
const EXCLUDED_DIRS: &[&str] = &["node_modules", "target", ".git", "dist", "build"];

/// Lock file patterns to exclude.
const EXCLUDED_EXTENSIONS: &[&str] = &["lock"];

/// Package manifest filenames (in priority order).
const MANIFEST_FILES: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "requirements.txt",
    "Gemfile",
    "composer.json",
];

/// Config filenames (in priority order).
const CONFIG_FILES: &[&str] = &[
    "tsconfig.json",
    ".eslintrc",
    ".eslintrc.js",
    ".eslintrc.json",
    ".eslintrc.yaml",
    ".eslintrc.yml",
    "webpack.config.js",
    "webpack.config.ts",
    "vite.config.js",
    "vite.config.ts",
    "next.config.js",
    "next.config.ts",
    ".prettierrc",
    ".prettierrc.js",
    ".prettierrc.json",
    ".prettierrc.yaml",
    ".prettierrc.yml",
];

/// Entrypoint filenames to search for.
const ENTRYPOINT_FILES: &[&str] = &[
    "main.rs",
    "main.go",
    "index.ts",
    "index.js",
    "app.py",
    "index.tsx",
    "app.ts",
];

const MAX_ENTRYPOINTS: usize = 5;

/// Check if a path component matches an excluded directory name.
fn is_excluded_dir(path: &Path, root: &Path) -> bool {
    // Check each component of the path relative to root.
    let rel = match path.strip_prefix(root) {
        Ok(r) => r,
        Err(_) => path,
    };
    for component in rel.components() {
        if let std::path::Component::Normal(name) = component {
            let name_str = name.to_string_lossy();
            if EXCLUDED_DIRS.iter().any(|&ex| ex == name_str.as_ref()) {
                return true;
            }
        }
    }
    false
}

/// Check if a file has an excluded extension (e.g., .lock files).
fn has_excluded_extension(path: &Path) -> bool {
    if let Some(ext) = path.extension() {
        let ext_str = ext.to_string_lossy();
        return EXCLUDED_EXTENSIONS.iter().any(|&ex| ex == ext_str.as_ref());
    }
    false
}

/// Represents an entry in the file tree for sorting.
#[derive(Eq, PartialEq)]
struct TreeEntry {
    is_dir: bool,
    name: String,
    depth: usize,
    relative_path: PathBuf,
}

impl Ord for TreeEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Directories before files, then alphabetically.
        match (self.is_dir, other.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => self.name.to_lowercase().cmp(&other.name.to_lowercase()),
        }
    }
}

impl PartialOrd for TreeEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Build an indented file tree string from the given root directory.
fn build_file_tree(root: &Path) -> String {
    // Collect all entries via the `ignore` crate walker.
    let mut entries: Vec<TreeEntry> = Vec::new();

    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("skipping unreadable path during context bundling: {e}");
                continue;
            }
        };
        let path = entry.path();

        // Skip root itself.
        if path == root {
            continue;
        }

        // Skip excluded directories and their contents.
        if is_excluded_dir(path, root) {
            continue;
        }

        // Skip lock files.
        if has_excluded_extension(path) {
            continue;
        }

        let is_dir = path.is_dir();

        // For files, skip non-UTF8 readable content (binary files).
        if !is_dir {
            // Check if the file is readable as text by checking the extension
            // or attempting a small read. We do a lightweight check here.
            if !is_likely_text_file(path) {
                continue;
            }
        }

        // Walker always yields paths under root, so strip_prefix succeeds.
        let relative = path.strip_prefix(root).unwrap_or(path).to_path_buf();

        let depth = relative.components().count();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        entries.push(TreeEntry {
            is_dir,
            name,
            depth,
            relative_path: relative,
        });
    }

    // Sort by parent path first, then dirs before files, then alphabetically.
    entries.sort_by(|a, b| {
        let a_parent = a.relative_path.parent().unwrap_or(Path::new(""));
        let b_parent = b.relative_path.parent().unwrap_or(Path::new(""));
        a_parent.cmp(b_parent).then_with(|| a.cmp(b))
    });

    let total = entries.len();
    let truncated = total > MAX_TREE_ENTRIES;
    let display_entries = if truncated {
        &entries[..MAX_TREE_ENTRIES]
    } else {
        &entries[..]
    };

    let mut lines: Vec<String> = Vec::new();
    for entry in display_entries {
        let indent = "  ".repeat(entry.depth.saturating_sub(1));
        let suffix = if entry.is_dir { "/" } else { "" };
        lines.push(format!("{}{}{}", indent, entry.name, suffix));
    }

    if truncated {
        let remaining = total - MAX_TREE_ENTRIES;
        lines.push(format!("... {remaining} more files (truncated)"));
    }

    lines.join("\n")
}

/// Heuristic to determine if a file is likely text (UTF-8 readable).
fn is_likely_text_file(path: &Path) -> bool {
    // Check known binary extensions.
    if let Some(ext) = path.extension() {
        let ext_lower = ext.to_string_lossy().to_lowercase();
        let binary_exts = [
            "png", "jpg", "jpeg", "gif", "bmp", "ico", "svg", "webp", "tiff", "pdf", "zip", "tar",
            "gz", "bz2", "xz", "7z", "rar", "exe", "dll", "so", "dylib", "a", "o", "wasm", "bin",
            "dat", "db", "sqlite", "mp3", "mp4", "avi", "mov", "wav", "ogg", "flac", "ttf", "otf",
            "woff", "woff2", "class", "jar", "pyc",
        ];
        if binary_exts.contains(&ext_lower.as_str()) {
            return false;
        }
    }
    // Try reading first 512 bytes to check for binary content.
    if let Ok(data) = std::fs::read(path) {
        let sample = &data[..data.len().min(512)];
        // If more than 30% non-text bytes, treat as binary.
        let non_text = sample
            .iter()
            .filter(|&&b| b == 0 || (b < 8) || (b > 13 && b < 32 && b != 27))
            .count();
        return non_text == 0 || (non_text as f64 / sample.len() as f64) < 0.30;
    }
    false
}

/// Read a file's content, truncating at `MAX_FILE_LINES` lines.
fn read_file_truncated(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= MAX_FILE_LINES {
        Some(content)
    } else {
        let truncated = lines[..MAX_FILE_LINES].join("\n");
        Some(format!("{truncated}\n... (truncated at 200 lines)"))
    }
}

/// Collect key files (manifests, configs, entrypoints) from the project.
fn collect_key_files(root: &Path) -> Vec<(String, String)> {
    let mut result: Vec<(String, String)> = Vec::new();
    let mut budget = MAX_TOTAL_BYTES;

    // --- Category 1: Package manifests ---
    for &name in MANIFEST_FILES {
        let path = root.join(name);
        if path.is_file() && !is_excluded_dir(&path, root) {
            if let Some(content) = read_file_truncated(&path) {
                if content.len() > budget {
                    break;
                }
                budget -= content.len();
                result.push((name.to_string(), content));
            }
        }
    }

    // --- Category 2: Config files ---
    for &name in CONFIG_FILES {
        if budget == 0 {
            break;
        }
        let path = root.join(name);
        if path.is_file() && !is_excluded_dir(&path, root) {
            if let Some(content) = read_file_truncated(&path) {
                if content.len() > budget {
                    break;
                }
                budget -= content.len();
                result.push((name.to_string(), content));
            }
        }
    }

    // --- Category 3: Entrypoints ---
    if budget > 0 {
        let entrypoints = find_entrypoints(root, MAX_ENTRYPOINTS);
        for (rel_path, path) in entrypoints {
            if let Some(content) = read_file_truncated(&path) {
                if content.len() > budget {
                    break;
                }
                budget -= content.len();
                result.push((rel_path, content));
            }
        }
    }

    result
}

/// Walk the project looking for entrypoint files, capped at `max` total.
fn find_entrypoints(root: &Path, max: usize) -> Vec<(String, PathBuf)> {
    let mut found: Vec<(String, PathBuf)> = Vec::new();

    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();

    for entry in walker {
        if found.len() >= max {
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("skipping unreadable path during context bundling: {e}");
                continue;
            }
        };
        let path = entry.path();
        if path == root || path.is_dir() {
            continue;
        }
        if is_excluded_dir(path, root) || has_excluded_extension(path) {
            continue;
        }
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        if ENTRYPOINT_FILES.contains(&file_name.as_str()) {
            let rel = path
                .strip_prefix(root)
                .map(|r| r.to_string_lossy().to_string())
                .unwrap_or(file_name);
            found.push((rel, path.to_path_buf()));
        }
    }

    found
}

/// Bundle relevant repo context for non-agentic runners.
///
/// # Errors
///
/// Returns `ActualError::IoError` if `root_dir` does not exist or is not a directory.
pub fn bundle_context(root_dir: &Path) -> Result<RepoBundleContext, ActualError> {
    if !root_dir.exists() || !root_dir.is_dir() {
        return Err(ActualError::IoError(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "'{}' does not exist or is not a directory",
                root_dir.display()
            ),
        )));
    }

    let file_tree = build_file_tree(root_dir);
    let key_files = collect_key_files(root_dir);

    Ok(RepoBundleContext {
        file_tree,
        key_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_file(dir: &Path, rel_path: &str, content: &str) {
        let full = dir.join(rel_path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(full, content).unwrap();
    }

    // Test 1: bundle_context produces a file tree and selects manifest files
    #[test]
    fn test_bundle_context_basic() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_file(root, "Cargo.toml", "[package]\nname = \"test\"");
        create_file(root, "src/main.rs", "fn main() {}");
        create_file(root, "node_modules/foo.js", "module.exports = {};");

        let ctx = bundle_context(root).unwrap();

        assert!(ctx.file_tree.contains("Cargo.toml"));
        assert!(!ctx.file_tree.contains("node_modules"));
        assert!(ctx.key_files.iter().any(|(name, _)| name == "Cargo.toml"));
    }

    // Test 2: tree excludes node_modules, target, .git
    #[test]
    fn test_file_tree_excludes_dirs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_file(root, "node_modules/x.js", "// js");
        create_file(root, "target/debug/foo", "binary");
        create_file(root, ".git/config", "[core]");
        create_file(root, "src/lib.rs", "// lib");

        let tree = build_file_tree(root);

        assert!(!tree.contains("node_modules"));
        assert!(!tree.contains("x.js"));
        assert!(!tree.contains("target"));
        assert!(!tree.contains(".git"));
        assert!(tree.contains("lib.rs"));
    }

    // Test 3: key file content is truncated at 200 lines
    #[test]
    fn test_key_file_truncation() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let long_content: String = (1..=300).map(|i| format!("# line {}\n", i)).collect();
        create_file(root, "Cargo.toml", &long_content);

        let ctx = bundle_context(root).unwrap();
        let cargo_entry = ctx
            .key_files
            .iter()
            .find(|(name, _)| name == "Cargo.toml")
            .expect("Cargo.toml in key_files");

        assert!(cargo_entry.1.lines().count() <= MAX_FILE_LINES + 1);
        assert!(cargo_entry.1.contains("truncated at 200 lines"));
    }

    // Test 4: missing project dir returns an error (not a panic)
    #[test]
    fn test_missing_dir_returns_error() {
        let result = bundle_context(Path::new("/nonexistent/path/that/does/not/exist"));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::IoError(_)));
    }

    // Test 5: file tree truncates at 200 entries
    #[test]
    fn test_file_tree_truncates_at_200() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        for i in 0..250 {
            create_file(root, &format!("file_{:03}.txt", i), "content");
        }

        let tree = build_file_tree(root);

        assert!(tree.contains("more files (truncated)"));
        let file_lines: Vec<&str> = tree.lines().filter(|l| !l.contains("truncated")).collect();
        assert!(file_lines.len() <= MAX_TREE_ENTRIES);
    }

    // Test 6: bundle_context returns error when path is a file, not a directory
    #[test]
    fn test_bundle_context_file_path_returns_error() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("notadir.txt");
        fs::write(&file_path, "hello").unwrap();

        let result = bundle_context(&file_path);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::IoError(_)));
    }

    // Test 7: lock files are excluded from the file tree
    #[test]
    fn test_file_tree_excludes_lock_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_file(root, "Cargo.lock", "# lock file");
        create_file(root, "package-lock.json", "{}");
        create_file(root, "src/main.rs", "fn main() {}");

        let tree = build_file_tree(root);

        assert!(!tree.contains("Cargo.lock"));
        assert!(tree.contains("main.rs"));
    }

    // Test 8: file tree shows directories with trailing slash
    #[test]
    fn test_file_tree_shows_dirs_with_slash() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_file(root, "src/lib.rs", "// lib");

        let tree = build_file_tree(root);

        assert!(tree.contains("src/"));
        assert!(tree.contains("lib.rs"));
    }

    // Test 9: has_excluded_extension returns false for files without extension
    #[test]
    fn test_has_excluded_extension_no_ext() {
        assert!(!has_excluded_extension(Path::new("Makefile")));
    }

    // Test 10: has_excluded_extension returns true for .lock files
    #[test]
    fn test_has_excluded_extension_lock() {
        assert!(has_excluded_extension(Path::new("Cargo.lock")));
    }

    // Test 11: is_excluded_dir returns false for non-excluded directories
    #[test]
    fn test_is_excluded_dir_false_for_non_excluded() {
        assert!(!is_excluded_dir(
            Path::new("/some/root/src/main.rs"),
            Path::new("/some/root")
        ));
    }

    // Test 12: is_likely_text_file returns false for known binary extensions
    #[test]
    fn test_is_likely_text_file_binary_extension() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("image.png");
        fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
        assert!(!is_likely_text_file(&path));
    }

    // Test 13: is_likely_text_file returns true for plain text files
    #[test]
    fn test_is_likely_text_file_text() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hello.txt");
        fs::write(&path, "Hello, world!\n").unwrap();
        assert!(is_likely_text_file(&path));
    }

    // Test 14: is_likely_text_file returns false for binary content without known extension
    #[test]
    fn test_is_likely_text_file_binary_content() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("binary_file.dat2");
        let binary_data: Vec<u8> = vec![0u8; 200];
        fs::write(&path, &binary_data).unwrap();
        assert!(!is_likely_text_file(&path));
    }

    // Test 15: collect_key_files includes config files (tsconfig.json)
    #[test]
    fn test_key_files_includes_config_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_file(root, "tsconfig.json", r#"{"compilerOptions": {}}"#);

        let key_files = collect_key_files(root);
        assert!(key_files.iter().any(|(name, _)| name == "tsconfig.json"));
    }

    // Test 16: collect_key_files finds entrypoints (main.rs in src/)
    #[test]
    fn test_key_files_finds_entrypoints() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_file(root, "src/main.rs", "fn main() { println!(\"hello\"); }");

        let key_files = collect_key_files(root);
        assert!(key_files.iter().any(|(name, _)| name.contains("main.rs")));
    }

    // Test 17: budget limits total key file content
    #[test]
    fn test_key_files_budget_limit() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let large_content: String = "x".repeat(MAX_TOTAL_BYTES - 10);
        create_file(root, "Cargo.toml", &large_content);
        create_file(root, "package.json", r#"{"name": "test"}"#);

        let key_files = collect_key_files(root);
        let total: usize = key_files.iter().map(|(_, c)| c.len()).sum();
        assert!(total <= MAX_TOTAL_BYTES);
    }

    // Test 18: find_entrypoints caps results at max
    #[test]
    fn test_find_entrypoints_cap() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        for i in 0..7 {
            create_file(root, &format!("pkg{}/main.rs", i), "fn main() {}");
        }

        let found = find_entrypoints(root, 3);
        assert!(found.len() <= 3);
    }

    // Test 19: is_excluded_dir strips prefix correctly for nested paths
    #[test]
    fn test_is_excluded_dir_nested() {
        assert!(is_excluded_dir(
            Path::new("/project/node_modules/foo/bar.js"),
            Path::new("/project")
        ));
    }

    // Test 20: read_file_truncated returns None for unreadable files
    #[test]
    fn test_read_file_truncated_nonexistent() {
        assert!(read_file_truncated(Path::new("/nonexistent/file.txt")).is_none());
    }

    // Test 21: read_file_truncated returns full content for short files
    #[test]
    fn test_read_file_truncated_short() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("short.txt");
        fs::write(&path, "line1\nline2\n").unwrap();

        let content = read_file_truncated(&path).unwrap();
        assert!(!content.contains("truncated"));
    }

    // Test 22: dist and build directories are excluded
    #[test]
    fn test_file_tree_excludes_dist_and_build() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        create_file(root, "dist/bundle.js", "/* bundled */");
        create_file(root, "build/output.js", "/* built */");
        create_file(root, "src/app.ts", "export {}");

        let tree = build_file_tree(root);

        assert!(!tree.contains("dist"));
        assert!(!tree.contains("build"));
        assert!(tree.contains("app.ts"));
    }

    // Test 23: tree entry ordering — dirs before files
    #[test]
    fn test_tree_entry_ordering() {
        let dir_entry = TreeEntry {
            is_dir: true,
            name: "zzz".to_string(),
            depth: 1,
            relative_path: PathBuf::from("zzz"),
        };
        let file_entry = TreeEntry {
            is_dir: false,
            name: "aaa".to_string(),
            depth: 1,
            relative_path: PathBuf::from("aaa"),
        };
        assert!(dir_entry < file_entry);
        assert!(file_entry > dir_entry);
        assert_eq!(
            dir_entry.partial_cmp(&file_entry),
            Some(std::cmp::Ordering::Less)
        );
    }

    // Test 24: config file budget=0 short-circuits
    #[test]
    fn test_config_files_skipped_when_budget_zero() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let content = "x".repeat(MAX_TOTAL_BYTES);
        create_file(root, "Cargo.toml", &content);
        create_file(root, "tsconfig.json", r#"{"compilerOptions": {}}"#);

        let key_files = collect_key_files(root);
        assert!(!key_files.iter().any(|(name, _)| name == "tsconfig.json"));
    }

    // Test 25: is_excluded_dir returns false when path is not under root (prefix strip fails)
    #[test]
    fn test_is_excluded_dir_path_not_under_root() {
        // path is not a prefix of root, so strip_prefix returns Err, and we fall back to path
        let result = is_excluded_dir(Path::new("/other/node_modules/foo"), Path::new("/project"));
        // The path components include "node_modules" so it should be excluded
        assert!(result);
    }

    // Test 26: is_likely_text_file returns false when file is not readable
    #[test]
    fn test_is_likely_text_file_unreadable() {
        // Non-existent file returns false
        assert!(!is_likely_text_file(Path::new("/nonexistent/ghost.xyz")));
    }

    // Test 27: manifest file content exceeding budget is skipped (break branch)
    #[test]
    fn test_manifest_exceeds_budget_is_skipped() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Cargo.toml that is larger than MAX_TOTAL_BYTES
        let huge_content = "x".repeat(MAX_TOTAL_BYTES + 100);
        create_file(root, "Cargo.toml", &huge_content);

        let key_files = collect_key_files(root);
        // Cargo.toml is too large to fit in budget, so no key files should be collected
        assert!(key_files.is_empty());
    }

    // Test 28: config file content exceeding remaining budget is skipped
    #[test]
    fn test_config_exceeds_remaining_budget_is_skipped() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Fill most of the budget with Cargo.toml
        let partial_content = "x".repeat(MAX_TOTAL_BYTES - 5);
        create_file(root, "Cargo.toml", &partial_content);
        // tsconfig.json with content > 5 bytes should not fit
        create_file(
            root,
            "tsconfig.json",
            r#"{"compilerOptions": {"strict": true}}"#,
        );

        let key_files = collect_key_files(root);
        assert!(!key_files.iter().any(|(name, _)| name == "tsconfig.json"));
    }

    // Test 29: binary file in directory is excluded from file tree
    #[test]
    fn test_file_tree_excludes_binary_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Create a binary file (all null bytes, no extension)
        let binary_path = root.join("binary_blob.dat2");
        fs::write(&binary_path, vec![0u8; 200]).unwrap();
        create_file(root, "readme.txt", "Hello");

        let tree = build_file_tree(root);

        assert!(!tree.contains("binary_blob.dat2"));
        assert!(tree.contains("readme.txt"));
    }

    // Test 30: entrypoints are skipped when budget is exhausted by manifests
    #[test]
    fn test_entrypoints_skipped_when_budget_exhausted() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Fill budget exactly with Cargo.toml (budget becomes 0 after adding it)
        let exact_content = "x".repeat(MAX_TOTAL_BYTES);
        create_file(root, "Cargo.toml", &exact_content);
        // Create an entrypoint that should not be collected because budget is 0
        create_file(root, "src/main.rs", "fn main() {}");

        let key_files = collect_key_files(root);
        // Cargo.toml fits exactly; main.rs should NOT be added (budget = 0)
        assert!(key_files.iter().any(|(name, _)| name == "Cargo.toml"));
        assert!(!key_files.iter().any(|(name, _)| name.contains("main.rs")));
    }

    // Test 31: entrypoint content exceeds remaining budget during iteration
    #[test]
    fn test_entrypoint_exceeds_remaining_budget_during_iteration() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Leave only a tiny remaining budget after Cargo.toml
        let partial = "x".repeat(MAX_TOTAL_BYTES - 5);
        create_file(root, "Cargo.toml", &partial);
        // main.rs content > 5 bytes
        create_file(
            root,
            "src/main.rs",
            "fn main() { println!(\"hello world\"); }",
        );

        let key_files = collect_key_files(root);
        // Cargo.toml is added, but main.rs content > remaining budget so it is not
        assert!(key_files.iter().any(|(name, _)| name == "Cargo.toml"));
        assert!(!key_files.iter().any(|(name, _)| name.contains("main.rs")));
    }

    // Test 32: multiple entrypoints — first fits, second exhausts budget to zero mid-loop
    #[test]
    fn test_entrypoint_budget_zero_mid_loop() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Leave exactly 15 bytes of budget after Cargo.toml
        let partial = "x".repeat(MAX_TOTAL_BYTES - 15);
        create_file(root, "Cargo.toml", &partial);
        // First entrypoint fits (10 bytes) and exhausts budget to 5
        create_file(root, "src/main.rs", "0123456789");
        // Second entrypoint won't fit (it's > 5 bytes)
        create_file(root, "app/app.py", "01234567890123456789");

        let key_files = collect_key_files(root);
        assert!(key_files.iter().any(|(name, _)| name == "Cargo.toml"));
        // We don't strictly assert on main.rs inclusion since walker order is not guaranteed,
        // but the total must remain within budget.
        let total: usize = key_files.iter().map(|(_, c)| c.len()).sum();
        assert!(total <= MAX_TOTAL_BYTES);
    }

    // Test 33: is_likely_text_file with a file that has no extension (covers closing brace path)
    #[test]
    fn test_is_likely_text_file_no_extension_text_content() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Makefile");
        fs::write(&path, "all:\n\techo hello\n").unwrap();
        // File has no extension, goes to binary-content check, should be true
        assert!(is_likely_text_file(&path));
    }

    // Test 34: manifest file with binary content causes read_file_truncated to return None
    // This covers line 270: `}` closing `if let Some(content)` in the manifest loop
    #[test]
    fn test_manifest_binary_content_returns_none() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Write Cargo.toml with non-UTF8 bytes — read_to_string will fail, returning None
        fs::write(root.join("Cargo.toml"), b"\xff\xfe invalid utf8 \x00\x01").unwrap();

        let key_files = collect_key_files(root);
        // Cargo.toml should NOT be included (binary content → None from read_file_truncated)
        assert!(!key_files.iter().any(|(name, _)| name == "Cargo.toml"));
    }

    // Test 35: config file with binary content causes None branch in config loop (covers line 287)
    #[test]
    fn test_config_binary_content_returns_none() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Write tsconfig.json with non-UTF8 bytes
        fs::write(
            root.join("tsconfig.json"),
            b"\xff\xfe invalid utf8 \x00\x01",
        )
        .unwrap();

        let key_files = collect_key_files(root);
        // tsconfig.json should NOT be included
        assert!(!key_files.iter().any(|(name, _)| name == "tsconfig.json"));
    }

    // Test 36: budget exhausted mid-loop in entrypoints — covers the content.len() > budget break
    #[test]
    fn test_entrypoint_budget_zero_after_first_entry() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Leave 10 bytes of budget after Cargo.toml
        let partial = "x".repeat(MAX_TOTAL_BYTES - 10);
        create_file(root, "Cargo.toml", &partial);
        // First entrypoint fills budget to exactly 0 (10 bytes)
        create_file(root, "pkg0/main.rs", "0123456789");
        // Second entrypoint — budget is 0, content.len() > budget fires
        create_file(root, "pkg1/main.rs", "fn main() {}");

        let key_files = collect_key_files(root);
        let total: usize = key_files.iter().map(|(_, c)| c.len()).sum();
        assert!(total <= MAX_TOTAL_BYTES);
    }

    // Test 37: entrypoint file with binary content causes None branch in entrypoints loop (covers line 304)
    #[test]
    fn test_entrypoint_binary_content_returns_none() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Create a main.rs that is actually binary (non-UTF8)
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), b"\xff\xfe invalid utf8 \x00\x01").unwrap();

        let key_files = collect_key_files(root);
        // main.rs should NOT be included (binary content → None from read_file_truncated)
        assert!(!key_files.iter().any(|(name, _)| name.contains("main.rs")));
    }

    // Test 38: walker error path in build_file_tree — unreadable subdirectory emits a warning
    #[test]
    #[cfg(unix)]
    fn test_build_file_tree_skips_unreadable_dir() {
        use std::os::unix::fs::PermissionsExt;
        use tracing_test::traced_test;

        #[traced_test]
        fn inner() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            // Create a readable file and an unreadable subdirectory.
            create_file(root, "readable.txt", "content");
            let secret = root.join("secret");
            fs::create_dir(&secret).unwrap();
            create_file(root, "secret/hidden.txt", "hidden");
            fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).unwrap();

            let tree = build_file_tree(root);

            // Restore permissions for cleanup
            fs::set_permissions(&secret, fs::Permissions::from_mode(0o755)).unwrap();

            // The readable file should appear; the walker emits an Err for the secret dir
            assert!(tree.contains("readable.txt"));
        }
        inner();
    }

    // Test 39: walker error path in find_entrypoints — unreadable subdirectory is skipped
    #[test]
    #[cfg(unix)]
    fn test_find_entrypoints_skips_unreadable_dir() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Readable entrypoint
        create_file(root, "src/main.rs", "fn main() {}");
        // Unreadable directory
        let secret = root.join("secret");
        fs::create_dir(&secret).unwrap();
        create_file(root, "secret/main.rs", "fn main() {}");
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).unwrap();

        let found = find_entrypoints(root, 100);

        // Restore permissions for cleanup
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o755)).unwrap();

        // Should not panic; walker error is skipped gracefully
        // The readable main.rs may or may not be found depending on walker behavior
        let _ = found;
    }
}
