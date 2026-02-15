use serde::{Deserialize, Serialize};

/// Top-level result of repository analysis.
#[derive(Debug, Serialize, Deserialize)]
pub struct RepoAnalysis {
    pub is_monorepo: bool,
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
}

/// Programming language detected in a project.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
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
    Other,
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
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
            Language::Other => "other",
        };
        write!(f, "{s}")
    }
}

/// A framework detected in a project.
#[derive(Debug, Serialize, Deserialize)]
pub struct Framework {
    pub name: String,
    pub category: FrameworkCategory,
}

/// Category classification for a framework.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "kebab-case")]
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
    fn serialize_deserialize_round_trip() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
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
            Language::Other,
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
    fn framework_category_serializes_as_kebab_case() {
        let serialized = serde_json::to_string(&FrameworkCategory::WebFrontend).unwrap();
        assert_eq!(serialized, "\"web-frontend\"");
    }
}
