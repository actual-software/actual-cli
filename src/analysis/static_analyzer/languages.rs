use std::collections::HashMap;
use std::path::Path;

use tokei::{Config, LanguageType, Languages};

use crate::analysis::types::Language;

/// Returns true if a `LanguageType` is data/markup rather than a programming language.
/// These are filtered out of detection results.
fn is_data_or_markup(lang_type: LanguageType) -> bool {
    matches!(
        lang_type,
        LanguageType::Json
            | LanguageType::Yaml
            | LanguageType::Html
            | LanguageType::Css
            | LanguageType::Markdown
            | LanguageType::Toml
            | LanguageType::Xml
            | LanguageType::Svg
            | LanguageType::Sass
            | LanguageType::Less
            | LanguageType::IntelHex
            | LanguageType::Ini
            | LanguageType::Protobuf
            | LanguageType::Dockerfile
            | LanguageType::MsBuild
    )
}

/// Maps a tokei `LanguageType` to the project's `Language` enum.
fn map_language_type(lang_type: LanguageType) -> Language {
    match lang_type {
        LanguageType::TypeScript | LanguageType::Tsx => Language::TypeScript,
        LanguageType::JavaScript | LanguageType::Jsx => Language::JavaScript,
        LanguageType::Python => Language::Python,
        LanguageType::Rust => Language::Rust,
        LanguageType::Go => Language::Go,
        LanguageType::Java => Language::Java,
        LanguageType::Kotlin => Language::Kotlin,
        LanguageType::Swift => Language::Swift,
        LanguageType::Ruby => Language::Ruby,
        LanguageType::Php => Language::Php,
        LanguageType::C | LanguageType::CHeader => Language::C,
        LanguageType::Cpp | LanguageType::CppHeader => Language::Cpp,
        LanguageType::CSharp => Language::CSharp,
        LanguageType::Scala => Language::Scala,
        LanguageType::Elixir => Language::Elixir,
        _ => Language::Other,
    }
}

/// Detect programming languages in the given directory, sorted by lines of code descending.
///
/// Uses tokei to scan the directory (respecting `.gitignore` by default).
/// Filters out data/markup languages (JSON, YAML, HTML, CSS, Markdown, etc.).
/// Languages not in the `Language` enum are aggregated into a single `Language::Other` entry.
///
/// Returns a list of `(Language, usize)` tuples where `usize` is lines of code.
pub fn detect_languages(path: &Path) -> Vec<(Language, usize)> {
    let config = Config::default();
    let mut languages = Languages::new();
    languages.get_statistics(&[path], &[], &config);

    // Aggregate LOC per Language variant
    let mut loc_map: HashMap<Language, usize> = HashMap::new();

    for (lang_type, stats) in &languages {
        if is_data_or_markup(*lang_type) {
            continue;
        }

        let code_lines = stats.code;
        if code_lines == 0 {
            continue;
        }

        let language = map_language_type(*lang_type);
        *loc_map.entry(language).or_default() += code_lines;
    }

    // Build result vec, separating Other from the rest
    let mut other_loc = 0usize;
    let mut results: Vec<(Language, usize)> = Vec::new();

    for (lang, loc) in loc_map {
        if lang == Language::Other {
            other_loc += loc;
        } else {
            results.push((lang, loc));
        }
    }

    // Sort by LOC descending
    results.sort_by(|a, b| b.1.cmp(&a.1));

    // Append aggregated Other at the end
    if other_loc > 0 {
        results.push((Language::Other, other_loc));
    }

    results
}

/// Normalize a language name or alias to its canonical lowercase form.
///
/// Returns `Some(canonical)` when the input is a recognized language name
/// (including canonical forms themselves, to handle case normalization).
/// Returns `None` only for completely unrecognized input.
///
/// This ensures that case variants like `"CSharp"` or `"Rust"` are always
/// lowercased to their canonical form, and aliases like `"ts"` or `"c#"` are
/// expanded.
///
/// # Examples
///
/// ```
/// use actual_cli::analysis::static_analyzer::languages::normalize_language;
///
/// assert_eq!(normalize_language("ts"), Some("typescript"));   // alias → canonical
/// assert_eq!(normalize_language("CSharp"), Some("csharp"));   // case variant → canonical
/// assert_eq!(normalize_language("rust"), Some("rust"));       // already canonical
/// assert_eq!(normalize_language("haskell"), None);            // unrecognized
/// ```
pub fn normalize_language(value: &str) -> Option<&'static str> {
    match value.to_lowercase().as_str() {
        "c#" | "c_sharp" | "c-sharp" | "cs" | "csharp" => Some("csharp"),
        "js" | "jsx" | "javascript" => Some("javascript"),
        "ts" | "tsx" | "typescript" => Some("typescript"),
        "py" | "python" => Some("python"),
        "golang" | "go" => Some("go"),
        "c++" | "cxx" | "cpp" => Some("cpp"),
        "rust" => Some("rust"),
        "java" => Some("java"),
        "kotlin" => Some("kotlin"),
        "swift" => Some("swift"),
        "ruby" => Some("ruby"),
        "php" => Some("php"),
        "c" => Some("c"),
        "scala" => Some("scala"),
        "elixir" => Some("elixir"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    // --- map_language_type tests ---

    #[test]
    fn test_map_typescript() {
        assert_eq!(
            map_language_type(LanguageType::TypeScript),
            Language::TypeScript
        );
        assert_eq!(map_language_type(LanguageType::Tsx), Language::TypeScript);
    }

    #[test]
    fn test_map_javascript() {
        assert_eq!(
            map_language_type(LanguageType::JavaScript),
            Language::JavaScript
        );
        assert_eq!(map_language_type(LanguageType::Jsx), Language::JavaScript);
    }

    #[test]
    fn test_map_python() {
        assert_eq!(map_language_type(LanguageType::Python), Language::Python);
    }

    #[test]
    fn test_map_rust() {
        assert_eq!(map_language_type(LanguageType::Rust), Language::Rust);
    }

    #[test]
    fn test_map_go() {
        assert_eq!(map_language_type(LanguageType::Go), Language::Go);
    }

    #[test]
    fn test_map_java() {
        assert_eq!(map_language_type(LanguageType::Java), Language::Java);
    }

    #[test]
    fn test_map_kotlin() {
        assert_eq!(map_language_type(LanguageType::Kotlin), Language::Kotlin);
    }

    #[test]
    fn test_map_swift() {
        assert_eq!(map_language_type(LanguageType::Swift), Language::Swift);
    }

    #[test]
    fn test_map_ruby() {
        assert_eq!(map_language_type(LanguageType::Ruby), Language::Ruby);
    }

    #[test]
    fn test_map_php() {
        assert_eq!(map_language_type(LanguageType::Php), Language::Php);
    }

    #[test]
    fn test_map_c() {
        assert_eq!(map_language_type(LanguageType::C), Language::C);
        assert_eq!(map_language_type(LanguageType::CHeader), Language::C);
    }

    #[test]
    fn test_map_cpp() {
        assert_eq!(map_language_type(LanguageType::Cpp), Language::Cpp);
        assert_eq!(map_language_type(LanguageType::CppHeader), Language::Cpp);
    }

    #[test]
    fn test_map_csharp() {
        assert_eq!(map_language_type(LanguageType::CSharp), Language::CSharp);
    }

    #[test]
    fn test_map_scala() {
        assert_eq!(map_language_type(LanguageType::Scala), Language::Scala);
    }

    #[test]
    fn test_map_elixir() {
        assert_eq!(map_language_type(LanguageType::Elixir), Language::Elixir);
    }

    #[test]
    fn test_map_unknown_to_other() {
        assert_eq!(map_language_type(LanguageType::Haskell), Language::Other);
        assert_eq!(map_language_type(LanguageType::Lua), Language::Other);
        assert_eq!(map_language_type(LanguageType::Perl), Language::Other);
    }

    // --- normalize_language tests ---

    #[test]
    fn test_normalize_csharp_aliases() {
        assert_eq!(normalize_language("c#"), Some("csharp"));
        assert_eq!(normalize_language("C#"), Some("csharp"));
        assert_eq!(normalize_language("c_sharp"), Some("csharp"));
        assert_eq!(normalize_language("c-sharp"), Some("csharp"));
        assert_eq!(normalize_language("cs"), Some("csharp"));
        // Canonical form and case variants all normalize
        assert_eq!(normalize_language("csharp"), Some("csharp"));
        assert_eq!(normalize_language("CSharp"), Some("csharp"));
    }

    #[test]
    fn test_normalize_javascript_aliases() {
        assert_eq!(normalize_language("js"), Some("javascript"));
        assert_eq!(normalize_language("JS"), Some("javascript"));
        assert_eq!(normalize_language("jsx"), Some("javascript"));
        assert_eq!(normalize_language("JSX"), Some("javascript"));
    }

    #[test]
    fn test_normalize_typescript_aliases() {
        assert_eq!(normalize_language("ts"), Some("typescript"));
        assert_eq!(normalize_language("TS"), Some("typescript"));
        assert_eq!(normalize_language("tsx"), Some("typescript"));
        assert_eq!(normalize_language("TSX"), Some("typescript"));
    }

    #[test]
    fn test_normalize_python_alias() {
        assert_eq!(normalize_language("py"), Some("python"));
        assert_eq!(normalize_language("PY"), Some("python"));
    }

    #[test]
    fn test_normalize_go_alias() {
        assert_eq!(normalize_language("golang"), Some("go"));
        assert_eq!(normalize_language("Golang"), Some("go"));
    }

    #[test]
    fn test_normalize_cpp_aliases() {
        assert_eq!(normalize_language("c++"), Some("cpp"));
        assert_eq!(normalize_language("C++"), Some("cpp"));
        assert_eq!(normalize_language("cxx"), Some("cpp"));
        assert_eq!(normalize_language("CXX"), Some("cpp"));
    }

    #[test]
    fn test_normalize_canonical_returns_self() {
        assert_eq!(normalize_language("rust"), Some("rust"));
        assert_eq!(normalize_language("python"), Some("python"));
        assert_eq!(normalize_language("go"), Some("go"));
        assert_eq!(normalize_language("java"), Some("java"));
        assert_eq!(normalize_language("typescript"), Some("typescript"));
        assert_eq!(normalize_language("javascript"), Some("javascript"));
        assert_eq!(normalize_language("cpp"), Some("cpp"));
        assert_eq!(normalize_language("c"), Some("c"));
        assert_eq!(normalize_language("kotlin"), Some("kotlin"));
        assert_eq!(normalize_language("swift"), Some("swift"));
        assert_eq!(normalize_language("ruby"), Some("ruby"));
        assert_eq!(normalize_language("php"), Some("php"));
        assert_eq!(normalize_language("scala"), Some("scala"));
        assert_eq!(normalize_language("elixir"), Some("elixir"));
    }

    #[test]
    fn test_normalize_case_variants() {
        assert_eq!(normalize_language("Rust"), Some("rust"));
        assert_eq!(normalize_language("PYTHON"), Some("python"));
        assert_eq!(normalize_language("Go"), Some("go"));
        assert_eq!(normalize_language("Java"), Some("java"));
        assert_eq!(normalize_language("TypeScript"), Some("typescript"));
        assert_eq!(normalize_language("JavaScript"), Some("javascript"));
    }

    #[test]
    fn test_normalize_unknown_returns_none() {
        assert_eq!(normalize_language("haskell"), None);
        assert_eq!(normalize_language("zig"), None);
        assert_eq!(normalize_language(""), None);
    }

    // --- detect_languages tests ---

    #[test]
    fn test_detect_languages_empty_directory() {
        let dir = tempdir().unwrap();
        let result = detect_languages(dir.path());
        assert!(result.is_empty());
    }

    #[test]
    fn test_detect_languages_single_language() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();

        let result = detect_languages(dir.path());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, Language::Rust);
        assert!(result[0].1 > 0);
    }

    #[test]
    fn test_detect_languages_mixed_languages() {
        let dir = tempdir().unwrap();

        // Write more Rust code than Python to test ordering
        fs::write(
            dir.path().join("main.rs"),
            "fn main() {\n    let x = 1;\n    let y = 2;\n    let z = x + y;\n    println!(\"{z}\");\n}\n",
        )
        .unwrap();

        fs::write(dir.path().join("script.py"), "print(\"hello\")\n").unwrap();

        let result = detect_languages(dir.path());
        assert!(result.len() >= 2);

        // Rust should come first (more code)
        assert_eq!(result[0].0, Language::Rust);
        assert_eq!(result[1].0, Language::Python);
        assert!(result[0].1 > result[1].1);
    }

    #[test]
    fn test_detect_languages_loc_ordering() {
        let dir = tempdir().unwrap();

        // Python with many lines
        let python_code = (0..20)
            .map(|i| format!("x_{i} = {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(dir.path().join("big.py"), &python_code).unwrap();

        // Rust with fewer lines
        fs::write(
            dir.path().join("small.rs"),
            "fn main() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();

        let result = detect_languages(dir.path());
        assert!(result.len() >= 2);

        // Python should come first (more LOC)
        assert_eq!(result[0].0, Language::Python);
        assert_eq!(result[1].0, Language::Rust);
    }

    #[test]
    fn test_detect_languages_filters_data_markup() {
        let dir = tempdir().unwrap();

        fs::write(
            dir.path().join("main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();

        fs::write(dir.path().join("data.json"), "{\"key\": \"value\"}\n").unwrap();

        fs::write(dir.path().join("style.css"), "body { color: red; }\n").unwrap();

        fs::write(dir.path().join("readme.md"), "# Title\n\nSome text.\n").unwrap();

        let result = detect_languages(dir.path());
        // Only Rust should appear, not JSON/CSS/Markdown
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, Language::Rust);
    }

    #[test]
    fn test_detect_languages_aggregates_other() {
        let dir = tempdir().unwrap();

        // Haskell and Lua are both "Other"
        fs::write(
            dir.path().join("main.hs"),
            "main :: IO ()\nmain = putStrLn \"hello\"\n",
        )
        .unwrap();

        fs::write(
            dir.path().join("script.lua"),
            "print(\"hello\")\nprint(\"world\")\n",
        )
        .unwrap();

        let result = detect_languages(dir.path());

        // Both should be aggregated into a single Other entry
        let other_entries: Vec<_> = result
            .iter()
            .filter(|(l, _)| *l == Language::Other)
            .collect();
        assert_eq!(
            other_entries.len(),
            1,
            "Other languages should be aggregated into one entry"
        );
        // The aggregated LOC should be > 0
        assert!(other_entries[0].1 > 0);
    }

    #[test]
    fn test_detect_languages_skips_zero_code_files() {
        let dir = tempdir().unwrap();

        // A Python file with only comments (0 code lines)
        fs::write(
            dir.path().join("empty.py"),
            "# just a comment\n# another comment\n",
        )
        .unwrap();

        // A Rust file with actual code
        fs::write(
            dir.path().join("main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();

        let result = detect_languages(dir.path());
        // Only Rust should appear (Python had 0 code lines)
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, Language::Rust);
    }

    #[test]
    fn test_detect_languages_other_is_last() {
        let dir = tempdir().unwrap();

        // Some Rust code
        fs::write(
            dir.path().join("main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();

        // Some Haskell code (maps to Other)
        fs::write(
            dir.path().join("main.hs"),
            "main :: IO ()\nmain = putStrLn \"hello\"\n",
        )
        .unwrap();

        let result = detect_languages(dir.path());
        assert!(result.len() >= 2);

        // Other should be the last entry regardless of LOC
        let last = result.last().unwrap();
        assert_eq!(last.0, Language::Other);
    }
}
