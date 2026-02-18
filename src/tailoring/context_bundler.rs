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
}
