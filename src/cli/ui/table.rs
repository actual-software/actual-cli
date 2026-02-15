use comfy_table::{ContentArrangement, Table};

use crate::analysis::types::RepoAnalysis;

/// Render a `RepoAnalysis` as a formatted table string.
///
/// Columns: Project | Languages | Frameworks
/// - Project shows the project path
/// - Languages shows comma-separated language names
/// - Frameworks shows comma-separated framework names, or "\u{2014}" if none
pub fn render_analysis_table(analysis: &RepoAnalysis) -> String {
    let mut table = Table::new();
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec!["Project", "Languages", "Frameworks"]);

    for project in &analysis.projects {
        let languages = project
            .languages
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join(", ");

        let frameworks = if project.frameworks.is_empty() {
            "\u{2014}".to_string() // em-dash
        } else {
            project
                .frameworks
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };

        table.add_row(vec![&project.path, &languages, &frameworks]);
    }

    table.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::types::{Framework, FrameworkCategory, Language, Project};

    fn make_project(path: &str, languages: Vec<Language>, frameworks: Vec<Framework>) -> Project {
        Project {
            path: path.to_string(),
            name: path.to_string(),
            languages,
            frameworks,
            package_manager: None,
            description: None,
        }
    }

    #[test]
    fn render_table_three_projects() {
        let analysis = RepoAnalysis {
            is_monorepo: true,
            projects: vec![
                make_project(
                    "apps/web",
                    vec![Language::TypeScript],
                    vec![Framework {
                        name: "Next.js".to_string(),
                        category: FrameworkCategory::WebFrontend,
                    }],
                ),
                make_project(
                    "apps/api",
                    vec![Language::TypeScript],
                    vec![Framework {
                        name: "Fastify".to_string(),
                        category: FrameworkCategory::WebBackend,
                    }],
                ),
                make_project("packages/shared", vec![Language::TypeScript], vec![]),
            ],
        };

        let output = render_analysis_table(&analysis);
        assert!(
            output.contains("apps/web"),
            "expected apps/web in:\n{output}"
        );
        assert!(
            output.contains("apps/api"),
            "expected apps/api in:\n{output}"
        );
        assert!(
            output.contains("packages/shared"),
            "expected packages/shared in:\n{output}"
        );
    }

    #[test]
    fn render_table_no_frameworks_shows_em_dash() {
        let analysis = RepoAnalysis {
            is_monorepo: true,
            projects: vec![make_project(
                "libs/utils",
                vec![Language::TypeScript],
                vec![],
            )],
        };

        let output = render_analysis_table(&analysis);
        assert!(
            output.contains('\u{2014}'),
            "expected em-dash for no frameworks in:\n{output}"
        );
    }

    #[test]
    fn render_table_single_project() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            projects: vec![make_project(
                ".",
                vec![Language::Rust],
                vec![Framework {
                    name: "clap".to_string(),
                    category: FrameworkCategory::Cli,
                }],
            )],
        };

        let output = render_analysis_table(&analysis);
        assert!(output.contains('.'), "expected project path in:\n{output}");
        assert!(
            output.contains("rust"),
            "expected rust language in:\n{output}"
        );
        assert!(
            output.contains("clap"),
            "expected clap framework in:\n{output}"
        );
    }
}
