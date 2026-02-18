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

    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
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

        let relative = match path.strip_prefix(root) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };

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
            if budget == 0 {
                break;
            }
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

    for result in walker {
        if found.len() >= max {
            break;
        }
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
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

        // File tree should contain Cargo.toml and src/
        assert!(
            ctx.file_tree.contains("Cargo.toml"),
            "file_tree should contain 'Cargo.toml', got: {}",
            ctx.file_tree
        );

        // File tree should NOT contain node_modules
        assert!(
            !ctx.file_tree.contains("node_modules"),
            "file_tree should NOT contain 'node_modules', got: {}",
            ctx.file_tree
        );

        // key_files should include Cargo.toml
        let has_cargo_toml = ctx.key_files.iter().any(|(name, _)| name == "Cargo.toml");
        assert!(
            has_cargo_toml,
            "key_files should include Cargo.toml, got: {:?}",
            ctx.key_files.iter().map(|(n, _)| n).collect::<Vec<_>>()
        );
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

        assert!(
            !tree.contains("node_modules"),
            "tree should not contain 'node_modules', got: {}",
            tree
        );
        assert!(
            !tree.contains("x.js"),
            "tree should not contain 'x.js' (inside node_modules), got: {}",
            tree
        );
        assert!(
            !tree.contains("target"),
            "tree should not contain 'target', got: {}",
            tree
        );
        assert!(
            !tree.contains(".git"),
            "tree should not contain '.git', got: {}",
            tree
        );
        // src/lib.rs should appear
        assert!(
            tree.contains("lib.rs"),
            "tree should contain 'lib.rs', got: {}",
            tree
        );
    }

    // Test 3: key file content is truncated at 200 lines
    #[test]
    fn test_key_file_truncation() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Write a Cargo.toml with 300 lines
        let long_content: String = (1..=300).map(|i| format!("# line {}\n", i)).collect();
        create_file(root, "Cargo.toml", &long_content);

        let ctx = bundle_context(root).unwrap();

        let cargo_entry = ctx
            .key_files
            .iter()
            .find(|(name, _)| name == "Cargo.toml")
            .expect("Cargo.toml should be in key_files");

        let line_count = cargo_entry.1.lines().count();
        // Should be 200 content lines + 1 truncation note line
        assert!(
            line_count <= MAX_FILE_LINES + 1,
            "key file should have at most {} lines, got: {}",
            MAX_FILE_LINES + 1,
            line_count
        );
        assert!(
            cargo_entry.1.contains("truncated at 200 lines"),
            "truncation note should be present, got: {}",
            cargo_entry.1
        );
    }

    // Test 4: missing project dir returns an error (not a panic)
    #[test]
    fn test_missing_dir_returns_error() {
        let path = Path::new("/nonexistent/path/that/does/not/exist");
        let result = bundle_context(path);
        assert!(
            result.is_err(),
            "expected an error for missing directory, got Ok"
        );
        match result.unwrap_err() {
            ActualError::IoError(_) => {} // expected
            other => panic!("expected ActualError::IoError, got: {:?}", other),
        }
    }

    // Test 5: file tree truncates at 200 entries
    #[test]
    fn test_file_tree_truncates_at_200() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create 250 files
        for i in 0..250 {
            create_file(root, &format!("file_{:03}.txt", i), "content");
        }

        let tree = build_file_tree(root);

        assert!(
            tree.contains("more files (truncated)"),
            "tree should contain truncation notice for 250 files, got: {}",
            tree
        );

        // Count actual file lines (non-truncation-note lines)
        let file_lines: Vec<&str> = tree.lines().filter(|l| !l.contains("truncated")).collect();
        assert!(
            file_lines.len() <= MAX_TREE_ENTRIES,
            "tree should show at most {} entries, got: {}",
            MAX_TREE_ENTRIES,
            file_lines.len()
        );
    }

    // Test 6: bundle_context returns error when path is a file, not a directory
    #[test]
    fn test_bundle_context_file_path_returns_error() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("notadir.txt");
        fs::write(&file_path, "hello").unwrap();

        let result = bundle_context(&file_path);
        assert!(result.is_err(), "expected error for file path, got Ok");
        match result.unwrap_err() {
            ActualError::IoError(_) => {}
            other => panic!("expected ActualError::IoError, got: {:?}", other),
        }
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

        assert!(
            !tree.contains("Cargo.lock"),
            "tree should not contain 'Cargo.lock', got: {}",
            tree
        );
        assert!(
            tree.contains("main.rs"),
            "tree should contain 'main.rs', got: {}",
            tree
        );
    }

    // Test 8: file tree shows directories with trailing slash and subdirectories with indent
    #[test]
    fn test_file_tree_shows_dirs_with_slash() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        create_file(root, "src/lib.rs", "// lib");

        let tree = build_file_tree(root);

        // Directory should appear with trailing slash
        assert!(
            tree.contains("src/"),
            "tree should show directories with '/', got: {}",
            tree
        );
        // Nested file should be indented
        assert!(
            tree.contains("lib.rs"),
            "tree should contain nested file 'lib.rs', got: {}",
            tree
        );
    }

    // Test 9: has_excluded_extension returns false for files without extension
    #[test]
    fn test_has_excluded_extension_no_ext() {
        let path = Path::new("Makefile");
        assert!(!has_excluded_extension(path));
    }

    // Test 10: has_excluded_extension returns true for .lock files
    #[test]
    fn test_has_excluded_extension_lock() {
        let path = Path::new("Cargo.lock");
        assert!(has_excluded_extension(path));
    }

    // Test 11: is_excluded_dir returns false for non-excluded directories
    #[test]
    fn test_is_excluded_dir_false_for_non_excluded() {
        let root = Path::new("/some/root");
        let path = Path::new("/some/root/src/main.rs");
        assert!(!is_excluded_dir(path, root));
    }

    // Test 12: is_likely_text_file returns false for known binary extensions
    #[test]
    fn test_is_likely_text_file_binary_extension() {
        // PNG file extension should be treated as binary without reading
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
        // Write mostly null bytes (clearly binary)
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

        let has_tsconfig = key_files.iter().any(|(name, _)| name == "tsconfig.json");
        assert!(
            has_tsconfig,
            "key_files should include tsconfig.json, got: {:?}",
            key_files.iter().map(|(n, _)| n).collect::<Vec<_>>()
        );
    }

    // Test 16: collect_key_files finds entrypoints (main.rs in src/)
    #[test]
    fn test_key_files_finds_entrypoints() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        create_file(root, "src/main.rs", "fn main() { println!(\"hello\"); }");

        let key_files = collect_key_files(root);

        let has_main = key_files.iter().any(|(name, _)| name.contains("main.rs"));
        assert!(
            has_main,
            "key_files should include src/main.rs entrypoint, got: {:?}",
            key_files.iter().map(|(n, _)| n).collect::<Vec<_>>()
        );
    }

    // Test 17: budget limits total key file content
    #[test]
    fn test_key_files_budget_limit() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create a Cargo.toml that is just under 32KB
        let large_content: String = "x".repeat(MAX_TOTAL_BYTES - 10);
        create_file(root, "Cargo.toml", &large_content);
        // package.json would exceed the budget — should be skipped
        create_file(root, "package.json", r#"{"name": "test"}"#);

        let key_files = collect_key_files(root);

        // Total content should not exceed MAX_TOTAL_BYTES
        let total: usize = key_files.iter().map(|(_, c)| c.len()).sum();
        assert!(
            total <= MAX_TOTAL_BYTES,
            "total key file content ({} bytes) should not exceed budget ({} bytes)",
            total,
            MAX_TOTAL_BYTES
        );
    }

    // Test 18: find_entrypoints caps results at max
    #[test]
    fn test_find_entrypoints_cap() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create 7 main.rs files in different subdirs
        for i in 0..7 {
            create_file(root, &format!("pkg{}/main.rs", i), "fn main() {}");
        }

        let found = find_entrypoints(root, 3);
        assert!(
            found.len() <= 3,
            "find_entrypoints should cap at 3, got: {}",
            found.len()
        );
    }

    // Test 19: is_excluded_dir strips prefix correctly for nested paths
    #[test]
    fn test_is_excluded_dir_nested() {
        let root = Path::new("/project");
        let path = Path::new("/project/node_modules/foo/bar.js");
        assert!(is_excluded_dir(path, root));
    }

    // Test 20: read_file_truncated returns None for unreadable files
    #[test]
    fn test_read_file_truncated_nonexistent() {
        let result = read_file_truncated(Path::new("/nonexistent/file.txt"));
        assert!(result.is_none());
    }

    // Test 21: read_file_truncated returns full content for short files
    #[test]
    fn test_read_file_truncated_short() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("short.txt");
        fs::write(&path, "line1\nline2\n").unwrap();

        let result = read_file_truncated(&path);
        assert!(result.is_some());
        let content = result.unwrap();
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

        assert!(
            !tree.contains("dist"),
            "tree should not contain 'dist', got: {}",
            tree
        );
        assert!(
            !tree.contains("build"),
            "tree should not contain 'build', got: {}",
            tree
        );
        assert!(
            tree.contains("app.ts"),
            "tree should contain 'app.ts', got: {}",
            tree
        );
    }

    // Test 23: tree entry ordering — dirs before files
    #[test]
    fn test_tree_entry_ordering() {
        // Test the Ord/PartialOrd impl for TreeEntry
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
        // Directories should sort before files regardless of name
        assert!(dir_entry < file_entry);
        assert!(file_entry > dir_entry);
        // partial_cmp should agree
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

        // Fill budget exactly with a Cargo.toml of MAX_TOTAL_BYTES bytes
        let content = "x".repeat(MAX_TOTAL_BYTES);
        create_file(root, "Cargo.toml", &content);
        create_file(root, "tsconfig.json", r#"{"compilerOptions": {}}"#);

        let key_files = collect_key_files(root);

        // tsconfig.json should not be included because budget was exhausted
        let has_tsconfig = key_files.iter().any(|(name, _)| name == "tsconfig.json");
        assert!(
            !has_tsconfig,
            "tsconfig.json should be excluded when budget is exhausted"
        );
    }
}
