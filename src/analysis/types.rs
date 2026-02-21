use serde::de::Deserializer;
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

/// The workspace system that manages a monorepo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceType {
    Pnpm,
    Npm,
    Yarn,
    Lerna,
    Nx,
    Cargo,
    Go,
}

impl std::fmt::Display for WorkspaceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pnpm => write!(f, "pnpm"),
            Self::Npm => write!(f, "npm"),
            Self::Yarn => write!(f, "yarn"),
            Self::Lerna => write!(f, "lerna"),
            Self::Nx => write!(f, "nx"),
            Self::Cargo => write!(f, "cargo"),
            Self::Go => write!(f, "go"),
        }
    }
}

/// Top-level result of repository analysis.
#[derive(Debug, Serialize, Deserialize)]
pub struct RepoAnalysis {
    pub is_monorepo: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_type: Option<WorkspaceType>,
    pub projects: Vec<Project>,
}

/// A single project detected within the repository.
#[derive(Debug, Serialize, Deserialize)]
pub struct Project {
    pub path: String,
    pub name: String,
    pub languages: Vec<Language>,
    pub frameworks: Vec<Framework>,
    pub package_manager: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub dep_count: usize,
    #[serde(default)]
    pub dev_dep_count: usize,
}

/// Programming language detected in a project.
///
/// Deserialization is case-insensitive: `"Rust"`, `"rust"`, and `"RUST"` all
/// map to [`Language::Rust`]. Unknown strings map to [`Language::Other`] with
/// the original name preserved (e.g. `"Haskell"` → `Language::Other("haskell")`).
/// Serialization always produces lowercase (e.g. `"typescript"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Language {
    TypeScript,
    JavaScript,
    Python,
    Rust,
    Go,
    Java,
    Kotlin,
    Swift,
    Ruby,
    Php,
    C,
    Cpp,
    CSharp,
    Scala,
    Elixir,
    Other(String),
}

impl Language {
    /// Lowercase string representation used for both serialization and display.
    fn as_str(&self) -> &str {
        match self {
            Language::TypeScript => "typescript",
            Language::JavaScript => "javascript",
            Language::Python => "python",
            Language::Rust => "rust",
            Language::Go => "go",
            Language::Java => "java",
            Language::Kotlin => "kotlin",
            Language::Swift => "swift",
            Language::Ruby => "ruby",
            Language::Php => "php",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::CSharp => "csharp",
            Language::Scala => "scala",
            Language::Elixir => "elixir",
            Language::Other(s) => s.as_str(),
        }
    }

    /// Parse a string into a `Language` using case-insensitive matching.
    ///
    /// Handles the same aliases as [`normalize_language`](crate::analysis::static_analyzer::languages::normalize_language)
    /// to ensure consistent behavior across deserialization and normalization.
    /// Unknown strings are preserved (lowercased) in [`Language::Other`].
    fn from_str_insensitive(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "typescript" | "ts" | "tsx" => Language::TypeScript,
            "javascript" | "js" | "jsx" => Language::JavaScript,
            "python" | "py" => Language::Python,
            "rust" => Language::Rust,
            "go" | "golang" => Language::Go,
            "java" => Language::Java,
            "kotlin" => Language::Kotlin,
            "swift" => Language::Swift,
            "ruby" => Language::Ruby,
            "php" => Language::Php,
            "c" => Language::C,
            "cpp" | "c++" | "cxx" => Language::Cpp,
            "csharp" | "c#" | "c_sharp" | "c-sharp" | "cs" => Language::CSharp,
            "scala" => Language::Scala,
            "elixir" => Language::Elixir,
            _ => Language::Other(s.to_lowercase()),
        }
    }
}

impl Serialize for Language {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Language {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Language::from_str_insensitive(&s))
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A framework detected in a project.
#[derive(Debug, Serialize, Deserialize)]
pub struct Framework {
    pub name: String,
    pub category: FrameworkCategory,
}

/// Category classification for a framework.
///
/// Deserialization is case-insensitive and normalizes separators: `"web-frontend"`,
/// `"Web Frontend"`, `"WEB_FRONTEND"` all map to [`FrameworkCategory::WebFrontend`].
/// Unknown strings are preserved via [`FrameworkCategory::Other`].
/// Serialization produces kebab-case for known variants (e.g. `"web-frontend"`)
/// and the raw string for `Other`.
#[derive(Debug, Clone, PartialEq)]
pub enum FrameworkCategory {
    WebFrontend,
    WebBackend,
    Mobile,
    Desktop,
    Cli,
    Library,
    Data,
    Ml,
    Devops,
    Testing,
    BuildSystem,
    Other(String),
}

impl FrameworkCategory {
    /// Kebab-case string for known variants, used for serialization.
    pub fn as_str(&self) -> &str {
        match self {
            FrameworkCategory::WebFrontend => "web-frontend",
            FrameworkCategory::WebBackend => "web-backend",
            FrameworkCategory::Mobile => "mobile",
            FrameworkCategory::Desktop => "desktop",
            FrameworkCategory::Cli => "cli",
            FrameworkCategory::Library => "library",
            FrameworkCategory::Data => "data",
            FrameworkCategory::Ml => "ml",
            FrameworkCategory::Devops => "devops",
            FrameworkCategory::Testing => "testing",
            FrameworkCategory::BuildSystem => "build-system",
            FrameworkCategory::Other(s) => s.as_str(),
        }
    }

    /// Normalize input (lowercase, replace spaces/underscores with hyphens)
    /// and match against known variants. Falls back to `Other`.
    pub fn from_str_insensitive(s: &str) -> Self {
        let normalized: String = s
            .to_lowercase()
            .chars()
            .map(|c| if c == ' ' || c == '_' { '-' } else { c })
            .collect();

        match normalized.as_str() {
            "web-frontend" | "webfrontend" => FrameworkCategory::WebFrontend,
            "web-backend" | "webbackend" => FrameworkCategory::WebBackend,
            "mobile" => FrameworkCategory::Mobile,
            "desktop" => FrameworkCategory::Desktop,
            "cli" => FrameworkCategory::Cli,
            "library" => FrameworkCategory::Library,
            "data" => FrameworkCategory::Data,
            "ml" => FrameworkCategory::Ml,
            "devops" => FrameworkCategory::Devops,
            "testing" => FrameworkCategory::Testing,
            "build-system" | "buildsystem" => FrameworkCategory::BuildSystem,
            _ => FrameworkCategory::Other(normalized),
        }
    }
}

impl Serialize for FrameworkCategory {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for FrameworkCategory {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(FrameworkCategory::from_str_insensitive(&s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_sample_json() {
        let json = r#"{
            "is_monorepo": true,
            "projects": [{
                "path": "apps/web",
                "name": "Web",
                "languages": ["typescript"],
                "frameworks": [{"name": "nextjs", "category": "web-frontend"}]
            }]
        }"#;

        let analysis: RepoAnalysis = serde_json::from_str(json).unwrap();
        assert!(analysis.is_monorepo);
        assert_eq!(analysis.projects.len(), 1);

        let project = &analysis.projects[0];
        assert_eq!(project.path, "apps/web");
        assert_eq!(project.name, "Web");
        assert_eq!(project.languages, vec![Language::TypeScript]);
        assert_eq!(project.frameworks.len(), 1);
        assert_eq!(project.frameworks[0].name, "nextjs");
        assert_eq!(
            project.frameworks[0].category,
            FrameworkCategory::WebFrontend
        );
        assert!(project.package_manager.is_none());
        assert!(project.description.is_none());
    }

    #[test]
    fn deserialize_title_case_from_claude() {
        // Real Claude returns title-case languages and free-form category strings
        let json = r#"{
            "is_monorepo": false,
            "projects": [{
                "path": ".",
                "name": "my-app",
                "languages": ["Rust", "TypeScript"],
                "frameworks": [
                    {"name": "cargo", "category": "Build System"},
                    {"name": "nextjs", "category": "Web Frontend"}
                ]
            }]
        }"#;

        let analysis: RepoAnalysis = serde_json::from_str(json).unwrap();
        let project = &analysis.projects[0];
        assert_eq!(
            project.languages,
            vec![Language::Rust, Language::TypeScript]
        );
        assert_eq!(
            project.frameworks[0].category,
            FrameworkCategory::BuildSystem
        );
        assert_eq!(
            project.frameworks[1].category,
            FrameworkCategory::WebFrontend
        );
    }

    #[test]
    fn serialize_deserialize_round_trip() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![Project {
                path: "src".to_string(),
                name: "my-app".to_string(),
                languages: vec![Language::Rust, Language::Python],
                frameworks: vec![
                    Framework {
                        name: "actix-web".to_string(),
                        category: FrameworkCategory::WebBackend,
                    },
                    Framework {
                        name: "clap".to_string(),
                        category: FrameworkCategory::Cli,
                    },
                ],
                package_manager: Some("cargo".to_string()),
                description: Some("A CLI tool".to_string()),
                dep_count: 5,
                dev_dep_count: 3,
            }],
        };

        let json = serde_json::to_string(&analysis).unwrap();
        let deserialized: RepoAnalysis = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.is_monorepo, analysis.is_monorepo);
        assert_eq!(deserialized.projects.len(), analysis.projects.len());

        let original = &analysis.projects[0];
        let restored = &deserialized.projects[0];
        assert_eq!(restored.path, original.path);
        assert_eq!(restored.name, original.name);
        assert_eq!(restored.languages, original.languages);
        assert_eq!(restored.frameworks.len(), original.frameworks.len());
        assert_eq!(restored.frameworks[0].name, original.frameworks[0].name);
        assert_eq!(
            restored.frameworks[0].category,
            original.frameworks[0].category
        );
        assert_eq!(restored.frameworks[1].name, original.frameworks[1].name);
        assert_eq!(
            restored.frameworks[1].category,
            original.frameworks[1].category
        );
        assert_eq!(restored.package_manager, original.package_manager);
        assert_eq!(restored.description, original.description);
    }

    #[test]
    fn language_display_matches_serde() {
        // Verify Display output matches serde serialization (without quotes)
        let all_languages = vec![
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
            Language::Rust,
            Language::Go,
            Language::Java,
            Language::Kotlin,
            Language::Swift,
            Language::Ruby,
            Language::Php,
            Language::C,
            Language::Cpp,
            Language::CSharp,
            Language::Scala,
            Language::Elixir,
            Language::Other("haskell".to_string()),
        ];
        for lang in all_languages {
            let display = lang.to_string();
            let serde = serde_json::to_string(&lang).unwrap();
            assert_eq!(
                format!("\"{display}\""),
                serde,
                "Display and serde should match for {lang}"
            );
        }
    }

    #[test]
    fn language_serializes_as_lowercase() {
        let serialized = serde_json::to_string(&Language::TypeScript).unwrap();
        assert_eq!(serialized, "\"typescript\"");
    }

    #[test]
    fn language_deserializes_case_insensitive() {
        // Lowercase (existing behavior)
        let lang: Language = serde_json::from_str("\"rust\"").unwrap();
        assert_eq!(lang, Language::Rust);

        // Title case (Claude's actual output)
        let lang: Language = serde_json::from_str("\"Rust\"").unwrap();
        assert_eq!(lang, Language::Rust);

        let lang: Language = serde_json::from_str("\"TypeScript\"").unwrap();
        assert_eq!(lang, Language::TypeScript);

        // Uppercase
        let lang: Language = serde_json::from_str("\"RUST\"").unwrap();
        assert_eq!(lang, Language::Rust);

        let lang: Language = serde_json::from_str("\"TYPESCRIPT\"").unwrap();
        assert_eq!(lang, Language::TypeScript);

        // Alternate representations
        let lang: Language = serde_json::from_str("\"C++\"").unwrap();
        assert_eq!(lang, Language::Cpp);

        let lang: Language = serde_json::from_str("\"C#\"").unwrap();
        assert_eq!(lang, Language::CSharp);
    }

    #[test]
    fn language_deserializes_common_aliases() {
        // These aliases match normalize_language in languages.rs
        let lang: Language = serde_json::from_str("\"ts\"").unwrap();
        assert_eq!(lang, Language::TypeScript);
        let lang: Language = serde_json::from_str("\"tsx\"").unwrap();
        assert_eq!(lang, Language::TypeScript);
        let lang: Language = serde_json::from_str("\"js\"").unwrap();
        assert_eq!(lang, Language::JavaScript);
        let lang: Language = serde_json::from_str("\"jsx\"").unwrap();
        assert_eq!(lang, Language::JavaScript);
        let lang: Language = serde_json::from_str("\"py\"").unwrap();
        assert_eq!(lang, Language::Python);
        let lang: Language = serde_json::from_str("\"golang\"").unwrap();
        assert_eq!(lang, Language::Go);
        let lang: Language = serde_json::from_str("\"cxx\"").unwrap();
        assert_eq!(lang, Language::Cpp);
        let lang: Language = serde_json::from_str("\"cs\"").unwrap();
        assert_eq!(lang, Language::CSharp);
        let lang: Language = serde_json::from_str("\"c_sharp\"").unwrap();
        assert_eq!(lang, Language::CSharp);
        let lang: Language = serde_json::from_str("\"c-sharp\"").unwrap();
        assert_eq!(lang, Language::CSharp);
    }

    #[test]
    fn language_unknown_maps_to_other() {
        let lang: Language = serde_json::from_str("\"haskell\"").unwrap();
        assert_eq!(lang, Language::Other("haskell".to_string()));

        let lang: Language = serde_json::from_str("\"Zig\"").unwrap();
        assert_eq!(lang, Language::Other("zig".to_string()));
    }

    #[test]
    fn language_other_preserves_name() {
        // Unknown language names are preserved (lowercased) in Language::Other
        let lang: Language = serde_json::from_str("\"Haskell\"").unwrap();
        assert_eq!(lang, Language::Other("haskell".to_string()));
        assert_eq!(lang.to_string(), "haskell");

        let lang: Language = serde_json::from_str("\"Elixir\"").unwrap();
        // Elixir IS a known language, should not be Other
        assert_eq!(lang, Language::Elixir);
    }

    #[test]
    fn language_other_display_shows_name() {
        // Display should show the actual language name, not "other"
        let lang = Language::Other("haskell".to_string());
        assert_eq!(lang.to_string(), "haskell");

        let lang = Language::Other("zig".to_string());
        assert_eq!(lang.to_string(), "zig");
    }

    #[test]
    fn language_other_round_trip() {
        // Language::Other(String) should round-trip correctly
        let lang = Language::Other("haskell".to_string());
        let json = serde_json::to_string(&lang).unwrap();
        assert_eq!(json, "\"haskell\"");
        let deserialized: Language = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, lang);
    }

    #[test]
    fn framework_category_serializes_as_kebab_case() {
        let serialized = serde_json::to_string(&FrameworkCategory::WebFrontend).unwrap();
        assert_eq!(serialized, "\"web-frontend\"");

        let serialized = serde_json::to_string(&FrameworkCategory::BuildSystem).unwrap();
        assert_eq!(serialized, "\"build-system\"");
    }

    #[test]
    fn framework_category_deserializes_case_insensitive() {
        // Kebab-case (existing behavior)
        let cat: FrameworkCategory = serde_json::from_str("\"web-frontend\"").unwrap();
        assert_eq!(cat, FrameworkCategory::WebFrontend);

        // Title case with space (Claude's actual output)
        let cat: FrameworkCategory = serde_json::from_str("\"Web Frontend\"").unwrap();
        assert_eq!(cat, FrameworkCategory::WebFrontend);

        // Uppercase
        let cat: FrameworkCategory = serde_json::from_str("\"WEB-FRONTEND\"").unwrap();
        assert_eq!(cat, FrameworkCategory::WebFrontend);

        // Underscore separator
        let cat: FrameworkCategory = serde_json::from_str("\"web_frontend\"").unwrap();
        assert_eq!(cat, FrameworkCategory::WebFrontend);

        // No separator (PascalCase normalized)
        let cat: FrameworkCategory = serde_json::from_str("\"webfrontend\"").unwrap();
        assert_eq!(cat, FrameworkCategory::WebFrontend);

        // Build System variants
        let cat: FrameworkCategory = serde_json::from_str("\"Build System\"").unwrap();
        assert_eq!(cat, FrameworkCategory::BuildSystem);

        let cat: FrameworkCategory = serde_json::from_str("\"build-system\"").unwrap();
        assert_eq!(cat, FrameworkCategory::BuildSystem);

        let cat: FrameworkCategory = serde_json::from_str("\"buildsystem\"").unwrap();
        assert_eq!(cat, FrameworkCategory::BuildSystem);
    }

    #[test]
    fn framework_category_unknown_maps_to_other() {
        // Unknown inputs are normalized (lowercased, spaces/underscores to hyphens)
        let cat: FrameworkCategory = serde_json::from_str("\"Embedded\"").unwrap();
        assert_eq!(cat, FrameworkCategory::Other("embedded".to_string()));

        let cat: FrameworkCategory = serde_json::from_str("\"game-engine\"").unwrap();
        assert_eq!(cat, FrameworkCategory::Other("game-engine".to_string()));

        let cat: FrameworkCategory = serde_json::from_str("\"Game Engine\"").unwrap();
        assert_eq!(cat, FrameworkCategory::Other("game-engine".to_string()));
    }

    #[test]
    fn framework_category_other_serializes_normalized() {
        let cat = FrameworkCategory::Other("embedded".to_string());
        let serialized = serde_json::to_string(&cat).unwrap();
        assert_eq!(serialized, "\"embedded\"");
    }

    #[test]
    fn framework_category_other_round_trip() {
        // Round-trips are stable: deserialize -> serialize -> deserialize = same value
        let cat = FrameworkCategory::Other("game-engine".to_string());
        let json = serde_json::to_string(&cat).unwrap();
        let deserialized: FrameworkCategory = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, cat);

        // Even non-normalized input stabilizes after one round-trip
        let cat: FrameworkCategory = serde_json::from_str("\"Game Engine\"").unwrap();
        let json = serde_json::to_string(&cat).unwrap();
        let deserialized: FrameworkCategory = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, cat);
    }

    #[test]
    fn language_round_trip() {
        let all_languages = vec![
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
            Language::Rust,
            Language::Go,
            Language::Java,
            Language::Kotlin,
            Language::Swift,
            Language::Ruby,
            Language::Php,
            Language::C,
            Language::Cpp,
            Language::CSharp,
            Language::Scala,
            Language::Elixir,
            Language::Other("haskell".to_string()),
        ];
        for lang in &all_languages {
            let json = serde_json::to_string(lang).unwrap();
            let deserialized: Language = serde_json::from_str(&json).unwrap();
            assert_eq!(&deserialized, lang, "Round-trip failed for {lang}");
        }
    }

    #[test]
    fn framework_category_round_trip() {
        let all_categories = vec![
            FrameworkCategory::WebFrontend,
            FrameworkCategory::WebBackend,
            FrameworkCategory::Mobile,
            FrameworkCategory::Desktop,
            FrameworkCategory::Cli,
            FrameworkCategory::Library,
            FrameworkCategory::Data,
            FrameworkCategory::Ml,
            FrameworkCategory::Devops,
            FrameworkCategory::Testing,
            FrameworkCategory::BuildSystem,
        ];
        for cat in &all_categories {
            let json = serde_json::to_string(cat).unwrap();
            let deserialized: FrameworkCategory = serde_json::from_str(&json).unwrap();
            assert_eq!(&deserialized, cat, "Round-trip failed for {json}");
        }
    }

    // ── dep_count / dev_dep_count backward compat tests ──

    #[test]
    fn deserialize_project_without_dep_counts_defaults_to_zero() {
        // Simulates cached data from before dep_count fields existed.
        let json = r#"{
            "path": ".",
            "name": "legacy",
            "languages": ["rust"],
            "frameworks": []
        }"#;
        let project: Project = serde_json::from_str(json).unwrap();
        assert_eq!(project.dep_count, 0);
        assert_eq!(project.dev_dep_count, 0);
    }

    #[test]
    fn deserialize_project_with_dep_counts() {
        let json = r#"{
            "path": ".",
            "name": "modern",
            "languages": ["rust"],
            "frameworks": [],
            "dep_count": 15,
            "dev_dep_count": 7
        }"#;
        let project: Project = serde_json::from_str(json).unwrap();
        assert_eq!(project.dep_count, 15);
        assert_eq!(project.dev_dep_count, 7);
    }

    #[test]
    fn dep_counts_round_trip() {
        let project = Project {
            path: ".".to_string(),
            name: "test".to_string(),
            languages: vec![Language::Rust],
            frameworks: vec![],
            package_manager: None,
            description: None,
            dep_count: 42,
            dev_dep_count: 13,
        };
        let json = serde_json::to_string(&project).unwrap();
        let deserialized: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.dep_count, 42);
        assert_eq!(deserialized.dev_dep_count, 13);
    }

    // -- WorkspaceType tests --

    #[test]
    fn workspace_type_display() {
        assert_eq!(WorkspaceType::Pnpm.to_string(), "pnpm");
        assert_eq!(WorkspaceType::Npm.to_string(), "npm");
        assert_eq!(WorkspaceType::Yarn.to_string(), "yarn");
        assert_eq!(WorkspaceType::Lerna.to_string(), "lerna");
        assert_eq!(WorkspaceType::Nx.to_string(), "nx");
        assert_eq!(WorkspaceType::Cargo.to_string(), "cargo");
        assert_eq!(WorkspaceType::Go.to_string(), "go");
    }

    #[test]
    fn workspace_type_serde_round_trip() {
        let all = vec![
            WorkspaceType::Pnpm,
            WorkspaceType::Npm,
            WorkspaceType::Yarn,
            WorkspaceType::Lerna,
            WorkspaceType::Nx,
            WorkspaceType::Cargo,
            WorkspaceType::Go,
        ];
        for wt in &all {
            let json = serde_json::to_string(wt).unwrap();
            let deserialized: WorkspaceType = serde_json::from_str(&json).unwrap();
            assert_eq!(&deserialized, wt, "Round-trip failed for {json}");
        }
    }

    #[test]
    fn workspace_type_display_matches_serde() {
        let all = vec![
            WorkspaceType::Pnpm,
            WorkspaceType::Npm,
            WorkspaceType::Yarn,
            WorkspaceType::Lerna,
            WorkspaceType::Nx,
            WorkspaceType::Cargo,
            WorkspaceType::Go,
        ];
        for wt in all {
            let display = wt.to_string();
            let serde = serde_json::to_string(&wt).unwrap();
            let serde_unquoted = serde.trim_matches('"');
            assert_eq!(
                display, serde_unquoted,
                "Display and serde mismatch for {display}"
            );
        }
    }

    #[test]
    fn workspace_type_none_omitted_from_json() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![],
        };
        let json = serde_json::to_string(&analysis).unwrap();
        assert!(
            !json.contains("workspace_type"),
            "workspace_type: None should be omitted from JSON: {json}"
        );
    }

    #[test]
    fn workspace_type_some_included_in_json() {
        let analysis = RepoAnalysis {
            is_monorepo: true,
            workspace_type: Some(WorkspaceType::Pnpm),
            projects: vec![],
        };
        let json = serde_json::to_string(&analysis).unwrap();
        assert!(
            json.contains("\"workspace_type\":\"pnpm\""),
            "workspace_type: Some(Pnpm) should be in JSON: {json}"
        );
    }

    #[test]
    fn workspace_type_defaults_to_none_on_missing_key() {
        // Simulates deserializing cached data that predates the workspace_type field
        let json = r#"{"is_monorepo":false,"projects":[]}"#;
        let analysis: RepoAnalysis = serde_json::from_str(json).unwrap();
        assert_eq!(analysis.workspace_type, None);
    }
}
