use crate::analysis::types::{FrameworkCategory, Language, RepoAnalysis};
use crate::api::types::{MatchFramework, MatchOptions, MatchProject, MatchRequest};
use crate::config::types::Config;

/// Serialize a [`Language`] enum variant to its serde string representation.
///
/// Uses `serde_json::to_value` which, combined with `#[serde(rename_all = "lowercase")]`,
/// produces lowercase strings like `"typescript"`, `"rust"`, etc.
fn serialize_language(lang: &Language) -> String {
    serde_json::to_value(lang)
        .expect("Language serialization cannot fail")
        .as_str()
        .expect("Language serializes as string")
        .to_string()
}

/// Serialize a [`FrameworkCategory`] enum variant to its serde string representation.
///
/// Uses `serde_json::to_value` which, combined with `#[serde(rename_all = "kebab-case")]`,
/// produces kebab-case strings like `"web-frontend"`, `"web-backend"`, etc.
fn serialize_framework_category(category: &FrameworkCategory) -> String {
    serde_json::to_value(category)
        .expect("FrameworkCategory serialization cannot fail")
        .as_str()
        .expect("FrameworkCategory serializes as string")
        .to_string()
}

/// Convert a [`RepoAnalysis`] and [`Config`] into a [`MatchRequest`] suitable for the API.
///
/// Maps each analyzed project to a [`MatchProject`] with serialized language and framework
/// strings, and builds [`MatchOptions`] from the config's filtering fields.
pub fn build_match_request(analysis: &RepoAnalysis, config: &Config) -> MatchRequest {
    let projects = analysis
        .projects
        .iter()
        .map(|project| MatchProject {
            path: project.path.clone(),
            name: project.name.clone(),
            languages: project.languages.iter().map(serialize_language).collect(),
            frameworks: project
                .frameworks
                .iter()
                .map(|fw| MatchFramework {
                    name: fw.name.clone(),
                    category: serialize_framework_category(&fw.category),
                })
                .collect(),
        })
        .collect();

    let options = if config.include_categories.is_some()
        || config.exclude_categories.is_some()
        || config.include_general.is_some()
    {
        Some(MatchOptions {
            categories: config.include_categories.clone(),
            exclude_categories: config.exclude_categories.clone(),
            include_general: config.include_general,
            max_per_framework: None,
        })
    } else {
        None
    };

    MatchRequest { projects, options }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::types::{Framework, FrameworkCategory, Language, Project, RepoAnalysis};

    /// Helper: create a minimal project with the given languages and frameworks.
    fn make_project(
        path: &str,
        name: &str,
        languages: Vec<Language>,
        frameworks: Vec<Framework>,
    ) -> Project {
        Project {
            path: path.to_string(),
            name: name.to_string(),
            languages,
            frameworks,
            package_manager: None,
            description: None,
        }
    }

    #[test]
    fn test_monorepo_three_projects() {
        let analysis = RepoAnalysis {
            is_monorepo: true,
            projects: vec![
                make_project("apps/web", "web", vec![Language::TypeScript], vec![]),
                make_project("apps/api", "api", vec![Language::Rust], vec![]),
                make_project("libs/shared", "shared", vec![Language::JavaScript], vec![]),
            ],
        };

        let request = build_match_request(&analysis, &Config::default());
        assert_eq!(request.projects.len(), 3);
    }

    #[test]
    fn test_config_with_include_categories() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            projects: vec![make_project(".", "app", vec![Language::Rust], vec![])],
        };

        let config = Config {
            include_categories: Some(vec!["security".to_string(), "testing".to_string()]),
            ..Config::default()
        };

        let request = build_match_request(&analysis, &config);
        assert!(request.options.is_some());
        let options = request.options.unwrap();
        assert_eq!(
            options.categories,
            Some(vec!["security".to_string(), "testing".to_string()])
        );
    }

    #[test]
    fn test_default_config_options_none() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            projects: vec![make_project(".", "app", vec![Language::Rust], vec![])],
        };

        let request = build_match_request(&analysis, &Config::default());
        assert!(request.options.is_none());
    }

    #[test]
    fn test_language_typescript_serializes_correctly() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            projects: vec![make_project(
                ".",
                "app",
                vec![Language::TypeScript],
                vec![Framework {
                    name: "nextjs".to_string(),
                    category: FrameworkCategory::WebFrontend,
                }],
            )],
        };

        let request = build_match_request(&analysis, &Config::default());
        assert!(request.projects[0]
            .languages
            .contains(&"typescript".to_string()));
    }
}
