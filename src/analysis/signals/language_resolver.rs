use std::path::Path;

/// Extensions (without dots) that cannot be parsed with tree-sitter.
const KNOWN_UNPARSEABLE_EXTENSIONS: &[&str] = &[
    "baml", "toml", "ini", "env", "lock", "tf", "tfvars", "hcl", "tfstate",
];

/// Dotfile names (exact filenames) that cannot be parsed with tree-sitter.
const KNOWN_UNPARSEABLE_FILENAMES: &[&str] = &[
    ".env",
    ".gitignore",
    ".dockerignore",
    ".editorconfig",
    ".prettierrc",
    ".eslintrc",
    ".nvmrc",
];

/// Tree-sitter language identifiers.
///
/// Each variant maps to a tree-sitter grammar crate and its `LANGUAGE` constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TreeSitterLanguage {
    JavaScript,
    TypeScript,
    Tsx,
    Python,
    Go,
    Java,
    Kotlin,
    Rust,
    C,
    Cpp,
    CSharp,
    Ruby,
    Php,
    Swift,
}

impl TreeSitterLanguage {
    /// Get the string identifier for this language.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::Python => "python",
            Self::Go => "go",
            Self::Java => "java",
            Self::Kotlin => "kotlin",
            Self::Rust => "rust",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::CSharp => "csharp",
            Self::Ruby => "ruby",
            Self::Php => "php",
            Self::Swift => "swift",
        }
    }

    /// Get the tree-sitter [`tree_sitter::Language`] for this variant.
    ///
    /// Returns `None` for languages whose grammar crates are not available
    /// (e.g. Kotlin's crate is incompatible with tree-sitter 0.24).
    pub fn tree_sitter_language(&self) -> Option<tree_sitter::Language> {
        Some(match self {
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::Java => tree_sitter_java::LANGUAGE.into(),
            Self::Kotlin => return None, // tree-sitter-kotlin 0.3 requires tree-sitter 0.20
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::C => tree_sitter_c::LANGUAGE.into(),
            Self::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Self::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Self::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            Self::Php => tree_sitter_php::LANGUAGE_PHP.into(),
            Self::Swift => tree_sitter_swift::LANGUAGE.into(),
        })
    }

    /// Human-readable display name for this language.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::JavaScript => "JavaScript",
            Self::TypeScript => "TypeScript",
            Self::Tsx => "TypeScript (TSX)",
            Self::Python => "Python",
            Self::Go => "Go",
            Self::Java => "Java",
            Self::Kotlin => "Kotlin",
            Self::Rust => "Rust",
            Self::C => "C",
            Self::Cpp => "C++",
            Self::CSharp => "C#",
            Self::Ruby => "Ruby",
            Self::Php => "PHP",
            Self::Swift => "Swift",
        }
    }
}

/// Resolve a file path (and optional language hint) to a [`TreeSitterLanguage`].
///
/// Resolution order:
/// 1. For `.tsx` / `.jsx` files the extension always wins (they need specific grammars).
/// 2. Try the `language_hint` (e.g. `"rust"`, `"ts"`, `"golang"`).
/// 3. Fall back to the file extension.
///
/// Returns `None` when no supported language can be determined.
pub fn resolve_language(
    file_path: &Path,
    language_hint: Option<&str>,
) -> Option<TreeSitterLanguage> {
    let ext = file_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    let ext_ref = ext.as_deref();

    // 1. JSX/TSX always use extension-based grammar
    if ext_ref == Some("tsx") {
        return Some(TreeSitterLanguage::Tsx);
    }
    if ext_ref == Some("jsx") {
        return Some(TreeSitterLanguage::JavaScript);
    }

    // 2. Try language hint
    if let Some(hint) = language_hint {
        if let Some(lang) = resolve_from_hint(hint) {
            return Some(lang);
        }
    }

    // 3. Fall back to extension
    ext_ref.and_then(resolve_from_extension)
}

/// Map a file extension (lowercase, no leading dot) to a language.
fn resolve_from_extension(ext: &str) -> Option<TreeSitterLanguage> {
    match ext {
        // JavaScript / TypeScript
        "js" | "mjs" | "cjs" | "jsx" => Some(TreeSitterLanguage::JavaScript),
        "ts" | "mts" | "cts" => Some(TreeSitterLanguage::TypeScript),
        "tsx" => Some(TreeSitterLanguage::Tsx),
        // Python
        "py" | "pyi" => Some(TreeSitterLanguage::Python),
        // Go
        "go" => Some(TreeSitterLanguage::Go),
        // Java
        "java" => Some(TreeSitterLanguage::Java),
        // Kotlin
        "kt" | "kts" => Some(TreeSitterLanguage::Kotlin),
        // Rust
        "rs" => Some(TreeSitterLanguage::Rust),
        // C / C++
        "c" | "h" => Some(TreeSitterLanguage::C),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => Some(TreeSitterLanguage::Cpp),
        // C#
        "cs" => Some(TreeSitterLanguage::CSharp),
        // Ruby
        "rb" => Some(TreeSitterLanguage::Ruby),
        // PHP
        "php" => Some(TreeSitterLanguage::Php),
        // Swift
        "swift" => Some(TreeSitterLanguage::Swift),
        _ => None,
    }
}

/// Map a language hint / alias (case-insensitive) to a language.
fn resolve_from_hint(hint: &str) -> Option<TreeSitterLanguage> {
    match hint.to_lowercase().as_str() {
        "javascript" | "js" | "jsx" => Some(TreeSitterLanguage::JavaScript),
        "typescript" | "ts" => Some(TreeSitterLanguage::TypeScript),
        "tsx" => Some(TreeSitterLanguage::Tsx),
        "python" | "py" => Some(TreeSitterLanguage::Python),
        "go" | "golang" => Some(TreeSitterLanguage::Go),
        "java" => Some(TreeSitterLanguage::Java),
        "kotlin" | "kt" => Some(TreeSitterLanguage::Kotlin),
        "rust" | "rs" => Some(TreeSitterLanguage::Rust),
        "c" => Some(TreeSitterLanguage::C),
        "cpp" | "c++" | "cxx" => Some(TreeSitterLanguage::Cpp),
        "csharp" | "c#" | "cs" => Some(TreeSitterLanguage::CSharp),
        "ruby" | "rb" => Some(TreeSitterLanguage::Ruby),
        "php" => Some(TreeSitterLanguage::Php),
        "swift" => Some(TreeSitterLanguage::Swift),
        _ => None,
    }
}

/// Check whether `file_path` has a known extension or filename that cannot be
/// parsed by tree-sitter.
pub fn is_known_unparseable(file_path: &Path) -> bool {
    // Check by extension first (e.g. "Cargo.toml" -> ext "toml")
    if let Some(ext) = file_path.extension().and_then(|e| e.to_str()) {
        let lower = ext.to_lowercase();
        if KNOWN_UNPARSEABLE_EXTENSIONS.contains(&lower.as_str()) {
            return true;
        }
    }

    // Check by exact filename (e.g. ".env", ".gitignore")
    if let Some(name) = file_path.file_name().and_then(|n| n.to_str()) {
        if KNOWN_UNPARSEABLE_FILENAMES.contains(&name) {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ----- resolve_from_extension -----

    #[test]
    fn extension_rs_resolves_to_rust() {
        let p = PathBuf::from("src/main.rs");
        assert_eq!(resolve_language(&p, None), Some(TreeSitterLanguage::Rust));
    }

    #[test]
    fn extension_ts_resolves_to_typescript() {
        let p = PathBuf::from("index.ts");
        assert_eq!(
            resolve_language(&p, None),
            Some(TreeSitterLanguage::TypeScript)
        );
    }

    #[test]
    fn extension_tsx_resolves_to_tsx() {
        let p = PathBuf::from("App.tsx");
        assert_eq!(resolve_language(&p, None), Some(TreeSitterLanguage::Tsx));
    }

    #[test]
    fn extension_jsx_resolves_to_javascript() {
        let p = PathBuf::from("Component.jsx");
        assert_eq!(
            resolve_language(&p, None),
            Some(TreeSitterLanguage::JavaScript)
        );
    }

    #[test]
    fn extension_py_resolves_to_python() {
        let p = PathBuf::from("app.py");
        assert_eq!(resolve_language(&p, None), Some(TreeSitterLanguage::Python));
    }

    #[test]
    fn extension_go_resolves_to_go() {
        let p = PathBuf::from("main.go");
        assert_eq!(resolve_language(&p, None), Some(TreeSitterLanguage::Go));
    }

    #[test]
    fn extension_java_resolves_to_java() {
        let p = PathBuf::from("Main.java");
        assert_eq!(resolve_language(&p, None), Some(TreeSitterLanguage::Java));
    }

    #[test]
    fn extension_kt_resolves_to_kotlin() {
        let p = PathBuf::from("App.kt");
        assert_eq!(resolve_language(&p, None), Some(TreeSitterLanguage::Kotlin));
    }

    #[test]
    fn extension_c_resolves_to_c() {
        let p = PathBuf::from("main.c");
        assert_eq!(resolve_language(&p, None), Some(TreeSitterLanguage::C));
    }

    #[test]
    fn extension_cpp_resolves_to_cpp() {
        for ext in &["cc", "cpp", "cxx", "hpp", "hh", "hxx"] {
            let p = PathBuf::from(format!("main.{ext}"));
            assert_eq!(resolve_language(&p, None), Some(TreeSitterLanguage::Cpp));
        }
    }

    #[test]
    fn extension_cs_resolves_to_csharp() {
        let p = PathBuf::from("Program.cs");
        assert_eq!(resolve_language(&p, None), Some(TreeSitterLanguage::CSharp));
    }

    #[test]
    fn extension_rb_resolves_to_ruby() {
        let p = PathBuf::from("app.rb");
        assert_eq!(resolve_language(&p, None), Some(TreeSitterLanguage::Ruby));
    }

    #[test]
    fn extension_php_resolves_to_php() {
        let p = PathBuf::from("index.php");
        assert_eq!(resolve_language(&p, None), Some(TreeSitterLanguage::Php));
    }

    #[test]
    fn extension_swift_resolves_to_swift() {
        let p = PathBuf::from("App.swift");
        assert_eq!(resolve_language(&p, None), Some(TreeSitterLanguage::Swift));
    }

    #[test]
    fn extension_h_resolves_to_c() {
        let p = PathBuf::from("header.h");
        assert_eq!(resolve_language(&p, None), Some(TreeSitterLanguage::C));
    }

    #[test]
    fn extension_mjs_resolves_to_javascript() {
        let p = PathBuf::from("module.mjs");
        assert_eq!(
            resolve_language(&p, None),
            Some(TreeSitterLanguage::JavaScript)
        );
    }

    #[test]
    fn extension_pyi_resolves_to_python() {
        let p = PathBuf::from("stubs.pyi");
        assert_eq!(resolve_language(&p, None), Some(TreeSitterLanguage::Python));
    }

    // ----- resolve_from_hint -----

    #[test]
    fn hint_rust_resolves() {
        let p = PathBuf::from("file.txt");
        assert_eq!(
            resolve_language(&p, Some("rust")),
            Some(TreeSitterLanguage::Rust)
        );
    }

    #[test]
    fn hint_ts_resolves_to_typescript() {
        let p = PathBuf::from("file.txt");
        assert_eq!(
            resolve_language(&p, Some("ts")),
            Some(TreeSitterLanguage::TypeScript)
        );
    }

    #[test]
    fn hint_golang_resolves_to_go() {
        let p = PathBuf::from("file.txt");
        assert_eq!(
            resolve_language(&p, Some("golang")),
            Some(TreeSitterLanguage::Go)
        );
    }

    #[test]
    fn hint_cxx_resolves_to_cpp() {
        let p = PathBuf::from("file.txt");
        assert_eq!(
            resolve_language(&p, Some("c++")),
            Some(TreeSitterLanguage::Cpp)
        );
    }

    #[test]
    fn hint_csharp_resolves() {
        let p = PathBuf::from("file.txt");
        assert_eq!(
            resolve_language(&p, Some("c#")),
            Some(TreeSitterLanguage::CSharp)
        );
    }

    #[test]
    fn hint_is_case_insensitive() {
        let p = PathBuf::from("file.txt");
        assert_eq!(
            resolve_language(&p, Some("RUST")),
            Some(TreeSitterLanguage::Rust)
        );
        assert_eq!(
            resolve_language(&p, Some("Python")),
            Some(TreeSitterLanguage::Python)
        );
    }

    // ----- JSX/TSX override -----

    #[test]
    fn tsx_extension_overrides_hint() {
        let p = PathBuf::from("Component.tsx");
        // Even with a JavaScript hint, .tsx should win
        assert_eq!(
            resolve_language(&p, Some("javascript")),
            Some(TreeSitterLanguage::Tsx)
        );
    }

    #[test]
    fn jsx_extension_overrides_hint() {
        let p = PathBuf::from("Component.jsx");
        assert_eq!(
            resolve_language(&p, Some("typescript")),
            Some(TreeSitterLanguage::JavaScript)
        );
    }

    // ----- unknown / None -----

    #[test]
    fn unknown_extension_returns_none() {
        let p = PathBuf::from("data.xyz");
        assert_eq!(resolve_language(&p, None), None);
    }

    #[test]
    fn no_extension_returns_none() {
        let p = PathBuf::from("Makefile");
        assert_eq!(resolve_language(&p, None), None);
    }

    #[test]
    fn unknown_hint_with_unknown_extension_returns_none() {
        let p = PathBuf::from("file.xyz");
        assert_eq!(resolve_language(&p, Some("brainfuck")), None);
    }

    // ----- is_known_unparseable -----

    #[test]
    fn toml_is_known_unparseable() {
        assert!(is_known_unparseable(Path::new("Cargo.toml")));
    }

    #[test]
    fn env_is_known_unparseable() {
        assert!(is_known_unparseable(Path::new(".env")));
    }

    #[test]
    fn lock_is_known_unparseable() {
        assert!(is_known_unparseable(Path::new("Cargo.lock")));
    }

    #[test]
    fn rs_is_not_known_unparseable() {
        assert!(!is_known_unparseable(Path::new("main.rs")));
    }

    #[test]
    fn no_extension_is_not_known_unparseable() {
        assert!(!is_known_unparseable(Path::new("Makefile")));
    }

    // ----- display_name -----

    #[test]
    fn display_name_covers_all_variants() {
        let expected = [
            (TreeSitterLanguage::JavaScript, "JavaScript"),
            (TreeSitterLanguage::TypeScript, "TypeScript"),
            (TreeSitterLanguage::Tsx, "TypeScript (TSX)"),
            (TreeSitterLanguage::Python, "Python"),
            (TreeSitterLanguage::Go, "Go"),
            (TreeSitterLanguage::Java, "Java"),
            (TreeSitterLanguage::Kotlin, "Kotlin"),
            (TreeSitterLanguage::Rust, "Rust"),
            (TreeSitterLanguage::C, "C"),
            (TreeSitterLanguage::Cpp, "C++"),
            (TreeSitterLanguage::CSharp, "C#"),
            (TreeSitterLanguage::Ruby, "Ruby"),
            (TreeSitterLanguage::Php, "PHP"),
            (TreeSitterLanguage::Swift, "Swift"),
        ];
        for (lang, name) in &expected {
            assert_eq!(lang.display_name(), *name);
        }
    }

    // ----- is_known_unparseable (dotfile names) -----

    #[test]
    fn gitignore_is_known_unparseable() {
        assert!(is_known_unparseable(Path::new(".gitignore")));
    }

    #[test]
    fn dockerignore_is_known_unparseable() {
        assert!(is_known_unparseable(Path::new(".dockerignore")));
    }

    #[test]
    fn editorconfig_is_known_unparseable() {
        assert!(is_known_unparseable(Path::new(".editorconfig")));
    }

    #[test]
    fn prettierrc_is_known_unparseable() {
        assert!(is_known_unparseable(Path::new(".prettierrc")));
    }

    #[test]
    fn eslintrc_is_known_unparseable() {
        assert!(is_known_unparseable(Path::new(".eslintrc")));
    }

    #[test]
    fn nvmrc_is_known_unparseable() {
        assert!(is_known_unparseable(Path::new(".nvmrc")));
    }

    // ----- is_known_unparseable (extension-based) -----

    #[test]
    fn baml_is_known_unparseable() {
        assert!(is_known_unparseable(Path::new("schema.baml")));
    }

    #[test]
    fn ini_is_known_unparseable() {
        assert!(is_known_unparseable(Path::new("config.ini")));
    }

    #[test]
    fn tf_is_known_unparseable() {
        assert!(is_known_unparseable(Path::new("main.tf")));
    }

    #[test]
    fn tfvars_is_known_unparseable() {
        assert!(is_known_unparseable(Path::new("prod.tfvars")));
    }

    #[test]
    fn hcl_is_known_unparseable() {
        assert!(is_known_unparseable(Path::new("config.hcl")));
    }

    #[test]
    fn tfstate_is_known_unparseable() {
        assert!(is_known_unparseable(Path::new("state.tfstate")));
    }

    #[test]
    fn known_unparseable_with_subdirectory_env() {
        // Dotfiles in subdirectories should still match.
        assert!(is_known_unparseable(Path::new("config/.env")));
    }

    #[test]
    fn env_extension_is_known_unparseable() {
        // Files like "local.env" have extension "env".
        assert!(is_known_unparseable(Path::new("local.env")));
    }

    #[test]
    fn root_path_is_not_known_unparseable() {
        // An empty path or root path shouldn't be flagged.
        assert!(!is_known_unparseable(Path::new("")));
    }

    // ----- tree_sitter_language integration -----

    #[test]
    fn supported_languages_return_some_tree_sitter_language() {
        // Languages with working grammar crates should return Some.
        let supported = [
            TreeSitterLanguage::JavaScript,
            TreeSitterLanguage::TypeScript,
            TreeSitterLanguage::Tsx,
            TreeSitterLanguage::Python,
            TreeSitterLanguage::Go,
            TreeSitterLanguage::Java,
            TreeSitterLanguage::Rust,
            TreeSitterLanguage::C,
            TreeSitterLanguage::Cpp,
            TreeSitterLanguage::CSharp,
            TreeSitterLanguage::Ruby,
            TreeSitterLanguage::Php,
            TreeSitterLanguage::Swift,
        ];
        for lang in &supported {
            assert!(lang.tree_sitter_language().is_some());
        }
    }

    #[test]
    fn unsupported_languages_return_none() {
        // Kotlin's grammar crate is incompatible with tree-sitter 0.24+.
        assert!(TreeSitterLanguage::Kotlin.tree_sitter_language().is_none());
    }

    #[test]
    fn as_str_round_trips_through_hint() {
        let languages = [
            TreeSitterLanguage::JavaScript,
            TreeSitterLanguage::TypeScript,
            TreeSitterLanguage::Tsx,
            TreeSitterLanguage::Python,
            TreeSitterLanguage::Go,
            TreeSitterLanguage::Java,
            TreeSitterLanguage::Kotlin,
            TreeSitterLanguage::Rust,
            TreeSitterLanguage::C,
            TreeSitterLanguage::Cpp,
            TreeSitterLanguage::CSharp,
            TreeSitterLanguage::Ruby,
            TreeSitterLanguage::Php,
            TreeSitterLanguage::Swift,
        ];
        for lang in &languages {
            let resolved = resolve_from_hint(lang.as_str());
            assert_eq!(resolved, Some(*lang));
        }
    }

    // ----- additional extension coverage -----

    #[test]
    fn extension_cjs_resolves_to_javascript() {
        let p = PathBuf::from("config.cjs");
        assert_eq!(
            resolve_language(&p, None),
            Some(TreeSitterLanguage::JavaScript)
        );
    }

    #[test]
    fn extension_mts_resolves_to_typescript() {
        let p = PathBuf::from("module.mts");
        assert_eq!(
            resolve_language(&p, None),
            Some(TreeSitterLanguage::TypeScript)
        );
    }

    #[test]
    fn extension_cts_resolves_to_typescript() {
        let p = PathBuf::from("config.cts");
        assert_eq!(
            resolve_language(&p, None),
            Some(TreeSitterLanguage::TypeScript)
        );
    }

    #[test]
    fn extension_kts_resolves_to_kotlin() {
        let p = PathBuf::from("build.kts");
        assert_eq!(resolve_language(&p, None), Some(TreeSitterLanguage::Kotlin));
    }

    // ----- additional hint coverage -----

    #[test]
    fn hint_shell_returns_none() {
        let p = PathBuf::from("script.txt");
        assert_eq!(resolve_language(&p, Some("shell")), None);
        assert_eq!(resolve_language(&p, Some("bash")), None);
        assert_eq!(resolve_language(&p, Some("sh")), None);
    }

    #[test]
    fn hint_aliases_all_covered() {
        // Cover every alias not already tested
        let p = PathBuf::from("file.txt");
        assert_eq!(
            resolve_language(&p, Some("js")),
            Some(TreeSitterLanguage::JavaScript)
        );
        assert_eq!(
            resolve_language(&p, Some("py")),
            Some(TreeSitterLanguage::Python)
        );
        assert_eq!(
            resolve_language(&p, Some("rs")),
            Some(TreeSitterLanguage::Rust)
        );
        assert_eq!(
            resolve_language(&p, Some("kt")),
            Some(TreeSitterLanguage::Kotlin)
        );
        assert_eq!(
            resolve_language(&p, Some("rb")),
            Some(TreeSitterLanguage::Ruby)
        );
        assert_eq!(
            resolve_language(&p, Some("cs")),
            Some(TreeSitterLanguage::CSharp)
        );
        assert_eq!(
            resolve_language(&p, Some("cxx")),
            Some(TreeSitterLanguage::Cpp)
        );
    }

    #[test]
    fn hint_specific_variants() {
        let p = PathBuf::from("file.txt");
        assert_eq!(
            resolve_language(&p, Some("javascript")),
            Some(TreeSitterLanguage::JavaScript)
        );
        assert_eq!(
            resolve_language(&p, Some("typescript")),
            Some(TreeSitterLanguage::TypeScript)
        );
        assert_eq!(
            resolve_language(&p, Some("tsx")),
            Some(TreeSitterLanguage::Tsx)
        );
        assert_eq!(
            resolve_language(&p, Some("python")),
            Some(TreeSitterLanguage::Python)
        );
        assert_eq!(
            resolve_language(&p, Some("go")),
            Some(TreeSitterLanguage::Go)
        );
        assert_eq!(
            resolve_language(&p, Some("java")),
            Some(TreeSitterLanguage::Java)
        );
        assert_eq!(
            resolve_language(&p, Some("kotlin")),
            Some(TreeSitterLanguage::Kotlin)
        );
        assert_eq!(
            resolve_language(&p, Some("ruby")),
            Some(TreeSitterLanguage::Ruby)
        );
        assert_eq!(
            resolve_language(&p, Some("php")),
            Some(TreeSitterLanguage::Php)
        );
        assert_eq!(
            resolve_language(&p, Some("swift")),
            Some(TreeSitterLanguage::Swift)
        );
    }
}
