use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::analysis::static_analyzer::languages::normalize_language as canonical_normalize_language;

/// Languages for which semgrep rules may be applicable.
pub const SUPPORTED_LANGUAGES: &[&str] = &[
    "c",
    "cpp",
    "csharp",
    "go",
    "java",
    "javascript",
    "kotlin",
    "php",
    "python",
    "ruby",
    "rust",
    "scala",
    "swift",
    "typescript",
];

/// Discovers and resolves semgrep rule YAML files from a rules directory.
pub struct RuleResolver {
    rules_dir: PathBuf,
    rule_files: Vec<PathBuf>,
}

impl RuleResolver {
    /// Create a new resolver by scanning the given rules directory.
    pub fn new(rules_dir: &Path) -> Result<Self> {
        let rule_files = Self::scan_rules(rules_dir)?;
        Ok(Self {
            rules_dir: rules_dir.to_path_buf(),
            rule_files,
        })
    }

    /// Recursively scan `dir` for `.yml` and `.yaml` files.
    fn scan_rules(dir: &Path) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        Self::walk_dir(dir, &mut files)
            .with_context(|| format!("failed to scan rules directory {}", dir.display()))?;
        files.sort();
        Ok(files)
    }

    /// Recursive directory walker that collects YAML files.
    ///
    /// Symlinks are skipped to avoid infinite recursion from symlink cycles.
    fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        let entries = std::fs::read_dir(dir)
            .with_context(|| format!("failed to read directory {}", dir.display()))?;

        for entry in entries {
            let entry = entry?;
            let ft = entry.file_type()?;
            if ft.is_symlink() {
                continue;
            }
            let path = entry.path();
            if ft.is_dir() {
                Self::walk_dir(&path, out)?;
            } else if let Some(ext) = path.extension() {
                let ext = ext.to_string_lossy();
                if ext == "yml" || ext == "yaml" {
                    out.push(path);
                }
            }
        }
        Ok(())
    }

    /// Return all rule files. Semgrep handles language filtering internally
    /// via the `languages:` field in each rule, so no per-language filtering
    /// is performed here.
    pub fn resolve(&self) -> &[PathBuf] {
        &self.rule_files
    }

    /// Normalize a language name or alias to its canonical form.
    ///
    /// Delegates to the canonical implementation in
    /// [`crate::analysis::static_analyzer::languages::normalize_language`], which
    /// is the single source of truth for all language aliases.
    ///
    /// Returns `None` if the language is not recognized.
    pub fn normalize_language(lang: &str) -> Option<&'static str> {
        canonical_normalize_language(lang)
    }

    /// List the top-level category subdirectory names found in the rules directory.
    ///
    /// Files placed directly in the rules directory (not inside a subdirectory)
    /// are excluded — only subdirectory names are returned.
    pub fn list_categories(&self) -> Vec<String> {
        let categories: BTreeSet<String> = self
            .rule_files
            .iter()
            .filter_map(|p| {
                // Extract the first path component relative to rules_dir.
                // Skip files whose relative path has only one component (root-level files).
                let rel = p.strip_prefix(&self.rules_dir).ok()?;
                if rel.components().count() <= 1 {
                    return None;
                }
                rel.iter()
                    .next()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .collect();
        categories.into_iter().collect()
    }

    /// Total number of rule files discovered.
    pub fn rule_count(&self) -> usize {
        self.rule_files.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_language_typescript_aliases() {
        assert_eq!(RuleResolver::normalize_language("ts"), Some("typescript"));
        assert_eq!(RuleResolver::normalize_language("tsx"), Some("typescript"));
        assert_eq!(
            RuleResolver::normalize_language("typescript"),
            Some("typescript")
        );
        assert_eq!(
            RuleResolver::normalize_language("TypeScript"),
            Some("typescript")
        );
        assert_eq!(
            RuleResolver::normalize_language("TYPESCRIPT"),
            Some("typescript")
        );
    }

    #[test]
    fn test_normalize_language_javascript_aliases() {
        assert_eq!(RuleResolver::normalize_language("js"), Some("javascript"));
        assert_eq!(RuleResolver::normalize_language("jsx"), Some("javascript"));
        assert_eq!(
            RuleResolver::normalize_language("javascript"),
            Some("javascript")
        );
        assert_eq!(
            RuleResolver::normalize_language("JavaScript"),
            Some("javascript")
        );
    }

    #[test]
    fn test_normalize_language_python_aliases() {
        assert_eq!(RuleResolver::normalize_language("py"), Some("python"));
        assert_eq!(RuleResolver::normalize_language("python"), Some("python"));
        assert_eq!(RuleResolver::normalize_language("Python"), Some("python"));
    }

    #[test]
    fn test_normalize_language_go_aliases() {
        assert_eq!(RuleResolver::normalize_language("go"), Some("go"));
        assert_eq!(RuleResolver::normalize_language("golang"), Some("go"));
        assert_eq!(RuleResolver::normalize_language("GoLang"), Some("go"));
    }

    #[test]
    fn test_normalize_language_cpp_aliases() {
        assert_eq!(RuleResolver::normalize_language("cpp"), Some("cpp"));
        assert_eq!(RuleResolver::normalize_language("c++"), Some("cpp"));
        assert_eq!(RuleResolver::normalize_language("cxx"), Some("cpp"));
        assert_eq!(RuleResolver::normalize_language("CXX"), Some("cpp"));
    }

    #[test]
    fn test_normalize_language_csharp_aliases() {
        assert_eq!(RuleResolver::normalize_language("csharp"), Some("csharp"));
        assert_eq!(RuleResolver::normalize_language("c#"), Some("csharp"));
        assert_eq!(RuleResolver::normalize_language("cs"), Some("csharp"));
        assert_eq!(RuleResolver::normalize_language("c_sharp"), Some("csharp"));
        assert_eq!(RuleResolver::normalize_language("c-sharp"), Some("csharp"));
        assert_eq!(RuleResolver::normalize_language("C#"), Some("csharp"));
    }

    #[test]
    fn test_normalize_language_kotlin_aliases() {
        assert_eq!(RuleResolver::normalize_language("kt"), Some("kotlin"));
        assert_eq!(RuleResolver::normalize_language("kotlin"), Some("kotlin"));
    }

    #[test]
    fn test_normalize_language_ruby_aliases() {
        assert_eq!(RuleResolver::normalize_language("rb"), Some("ruby"));
        assert_eq!(RuleResolver::normalize_language("ruby"), Some("ruby"));
    }

    #[test]
    fn test_normalize_language_rust_aliases() {
        assert_eq!(RuleResolver::normalize_language("rs"), Some("rust"));
        assert_eq!(RuleResolver::normalize_language("rust"), Some("rust"));
        assert_eq!(RuleResolver::normalize_language("Rust"), Some("rust"));
    }

    #[test]
    fn test_normalize_language_direct_names() {
        assert_eq!(RuleResolver::normalize_language("java"), Some("java"));
        assert_eq!(RuleResolver::normalize_language("rust"), Some("rust"));
        assert_eq!(RuleResolver::normalize_language("php"), Some("php"));
        assert_eq!(RuleResolver::normalize_language("swift"), Some("swift"));
        assert_eq!(RuleResolver::normalize_language("c"), Some("c"));
        assert_eq!(RuleResolver::normalize_language("scala"), Some("scala"));
        assert_eq!(RuleResolver::normalize_language("elixir"), Some("elixir"));
    }

    #[test]
    fn test_normalize_language_unsupported() {
        assert_eq!(RuleResolver::normalize_language("haskell"), None);
        assert_eq!(RuleResolver::normalize_language(""), None);
        assert_eq!(RuleResolver::normalize_language("brainfuck"), None);
    }

    #[test]
    fn test_supported_languages_sorted() {
        let mut sorted = SUPPORTED_LANGUAGES.to_vec();
        sorted.sort();
        assert_eq!(sorted, SUPPORTED_LANGUAGES);
    }

    #[test]
    fn test_supported_languages_normalize_roundtrip() {
        // Every entry in SUPPORTED_LANGUAGES must be accepted by normalize_language
        // and must normalize to itself (it's the canonical form).
        for &lang in SUPPORTED_LANGUAGES {
            let normalized = RuleResolver::normalize_language(lang);
            assert_eq!(
                normalized,
                Some(lang),
                "SUPPORTED_LANGUAGES entry {lang:?} is not accepted by normalize_language"
            );
        }
    }

    #[test]
    fn test_normalize_language_output_in_supported() {
        // For the aliases that correspond to SUPPORTED_LANGUAGES entries, verify that
        // normalize_language returns a value that is actually in SUPPORTED_LANGUAGES.
        // Note: normalize_language may also recognize additional aliases (e.g. "elixir")
        // whose canonical forms are not in SUPPORTED_LANGUAGES; those are tested separately.
        let aliases = [
            "ts",
            "tsx",
            "typescript",
            "js",
            "jsx",
            "javascript",
            "py",
            "python",
            "golang",
            "go",
            "c++",
            "cxx",
            "cpp",
            "c#",
            "cs",
            "csharp",
            "c_sharp",
            "c-sharp",
            "kt",
            "kotlin",
            "rb",
            "ruby",
            "rs",
            "java",
            "rust",
            "php",
            "swift",
            "c",
            "scala",
        ];
        for alias in &aliases {
            let canonical = RuleResolver::normalize_language(alias)
                .unwrap_or_else(|| panic!("normalize_language({alias:?}) returned None"));
            assert!(
                SUPPORTED_LANGUAGES.contains(&canonical),
                "normalize_language({alias:?}) returned {canonical:?} which is not in SUPPORTED_LANGUAGES"
            );
        }
    }

    #[test]
    fn test_scan_rules_finds_actual_rules() {
        // Use the actual semgrep_rules directory in the repo
        let rules_dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/analysis/detectors/semgrep_rules");
        assert!(rules_dir.exists(), "semgrep_rules directory must exist");

        let resolver = RuleResolver::new(&rules_dir).unwrap();
        assert!(resolver.rule_count() > 0, "expected at least one rule file");
    }

    #[test]
    fn test_list_categories() {
        let rules_dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/analysis/detectors/semgrep_rules");
        assert!(rules_dir.exists(), "semgrep_rules directory must exist");

        let resolver = RuleResolver::new(&rules_dir).unwrap();
        let categories = resolver.list_categories();

        assert!(!categories.is_empty(), "expected at least one category");

        // Spot check some categories
        assert!(categories.contains(&"api".to_string()));
        assert!(categories.contains(&"security".to_string()));
        assert!(categories.contains(&"architecture".to_string()));
        assert!(categories.contains(&"data".to_string()));
        assert!(categories.contains(&"ml".to_string()));
    }

    #[test]
    fn test_resolve_returns_all_files() {
        let rules_dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/analysis/detectors/semgrep_rules");
        assert!(rules_dir.exists(), "semgrep_rules directory must exist");

        let resolver = RuleResolver::new(&rules_dir).unwrap();
        // resolve() returns all rule files regardless of language
        let files = resolver.resolve();
        assert!(!files.is_empty(), "expected at least one rule file");

        // All files should end in .yml or .yaml
        for f in files {
            let ext = f.extension().unwrap().to_string_lossy();
            assert!(ext == "yml" || ext == "yaml", "unexpected extension: {ext}");
        }
    }

    #[test]
    fn test_scan_rules_nonexistent_dir() {
        let result = RuleResolver::new(Path::new("/nonexistent/path/to/rules"));
        assert!(result.is_err());
    }

    #[test]
    fn test_scan_rules_empty_dir() {
        let temp = tempfile::tempdir().unwrap();
        let resolver = RuleResolver::new(temp.path()).unwrap();
        assert_eq!(resolver.rule_count(), 0);
        assert!(resolver.list_categories().is_empty());
        assert!(resolver.resolve().is_empty());
    }

    #[test]
    fn test_scan_rules_ignores_non_yaml() {
        let temp = tempfile::tempdir().unwrap();
        // Create a mix of yaml and non-yaml files
        let cat_dir = temp.path().join("category");
        std::fs::create_dir(&cat_dir).unwrap();
        std::fs::write(cat_dir.join("rule.yml"), "rules: []").unwrap();
        std::fs::write(cat_dir.join("rule.yaml"), "rules: []").unwrap();
        std::fs::write(cat_dir.join("readme.md"), "# Rules").unwrap();
        std::fs::write(cat_dir.join("config.json"), "{}").unwrap();

        let resolver = RuleResolver::new(temp.path()).unwrap();
        assert_eq!(resolver.rule_count(), 2);
    }

    #[test]
    fn test_scan_rules_ignores_files_without_extension() {
        let temp = tempfile::tempdir().unwrap();
        let cat_dir = temp.path().join("cat");
        std::fs::create_dir(&cat_dir).unwrap();
        std::fs::write(cat_dir.join("Makefile"), "all:").unwrap();
        std::fs::write(cat_dir.join("LICENSE"), "MIT").unwrap();
        std::fs::write(cat_dir.join("rule.yml"), "rules: []").unwrap();

        let resolver = RuleResolver::new(temp.path()).unwrap();
        assert_eq!(resolver.rule_count(), 1);
    }

    #[test]
    fn test_scan_rules_nested_directories() {
        let temp = tempfile::tempdir().unwrap();
        let deep = temp.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("deep.yml"), "rules: []").unwrap();
        std::fs::write(temp.path().join("a").join("shallow.yaml"), "rules: []").unwrap();

        let resolver = RuleResolver::new(temp.path()).unwrap();
        assert_eq!(resolver.rule_count(), 2);
    }

    #[test]
    fn test_list_categories_multiple_dirs() {
        let temp = tempfile::tempdir().unwrap();
        for cat in &["api", "security", "data"] {
            let cat_dir = temp.path().join(cat);
            std::fs::create_dir(&cat_dir).unwrap();
            std::fs::write(cat_dir.join("rules.yml"), "rules: []").unwrap();
        }

        let resolver = RuleResolver::new(temp.path()).unwrap();
        let categories = resolver.list_categories();
        assert_eq!(categories.len(), 3);
        assert!(categories.contains(&"api".to_string()));
        assert!(categories.contains(&"security".to_string()));
        assert!(categories.contains(&"data".to_string()));
    }

    #[test]
    fn test_list_categories_sorted() {
        let temp = tempfile::tempdir().unwrap();
        for cat in &["zz", "aa", "mm"] {
            let cat_dir = temp.path().join(cat);
            std::fs::create_dir(&cat_dir).unwrap();
            std::fs::write(cat_dir.join("r.yml"), "rules: []").unwrap();
        }

        let resolver = RuleResolver::new(temp.path()).unwrap();
        let categories = resolver.list_categories();
        assert_eq!(categories, vec!["aa", "mm", "zz"]);
    }

    #[test]
    fn test_list_categories_nested_dirs_uses_top_level() {
        // Verify that list_categories returns the top-level category even
        // when rules are in nested subdirectories (e.g., api/v2/rule.yml → "api")
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("api").join("v2");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("rule.yml"), "rules: []").unwrap();

        let flat = temp.path().join("security");
        std::fs::create_dir(&flat).unwrap();
        std::fs::write(flat.join("rule.yml"), "rules: []").unwrap();

        let resolver = RuleResolver::new(temp.path()).unwrap();
        let categories = resolver.list_categories();
        assert_eq!(categories, vec!["api", "security"]);
    }

    #[test]
    fn test_list_categories_root_files_excluded() {
        // Files directly in the rules_dir should not appear as categories
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("root.yml"), "rules: []").unwrap();
        let cat = temp.path().join("cat");
        std::fs::create_dir(&cat).unwrap();
        std::fs::write(cat.join("sub.yml"), "rules: []").unwrap();

        let resolver = RuleResolver::new(temp.path()).unwrap();
        let categories = resolver.list_categories();
        assert_eq!(resolver.rule_count(), 2);
        assert_eq!(categories, vec!["cat"]);
        assert!(!categories.contains(&"root.yml".to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn test_scan_rules_skips_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let cat = temp.path().join("cat");
        std::fs::create_dir(&cat).unwrap();
        std::fs::write(cat.join("real.yml"), "rules: []").unwrap();

        // Create a symlink to a file — should be skipped
        std::os::unix::fs::symlink(cat.join("real.yml"), cat.join("linked.yml")).unwrap();
        // Create a symlink directory pointing back to parent — would cause
        // infinite recursion if followed
        std::os::unix::fs::symlink(temp.path(), cat.join("cycle")).unwrap();

        let resolver = RuleResolver::new(temp.path()).unwrap();
        // Only the real file should be found, not the symlinked file or cycle
        assert_eq!(resolver.rule_count(), 1);
    }

    #[test]
    fn test_rule_files_sorted() {
        let temp = tempfile::tempdir().unwrap();
        let cat = temp.path().join("cat");
        std::fs::create_dir(&cat).unwrap();
        std::fs::write(cat.join("z.yml"), "rules: []").unwrap();
        std::fs::write(cat.join("a.yml"), "rules: []").unwrap();
        std::fs::write(cat.join("m.yml"), "rules: []").unwrap();

        let resolver = RuleResolver::new(temp.path()).unwrap();
        let files = resolver.resolve();
        let names: Vec<&str> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, vec!["a.yml", "m.yml", "z.yml"]);
    }
}
