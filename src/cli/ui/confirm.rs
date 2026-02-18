use crate::analysis::confirm::ConfirmAction;
use crate::analysis::types::{Language, Project, RepoAnalysis};
use crate::cli::ui::terminal::TerminalIO;

use super::panel::Panel;
use super::theme;

/// Abbreviate common language names for compact display.
fn abbreviate_language(lang: &Language) -> &str {
    match lang {
        Language::TypeScript => "ts",
        Language::JavaScript => "js",
        Language::Python => "py",
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
        Language::Other(name) => name.as_str(),
    }
}

/// Format project metadata as compact inline string: `lang · framework · pm`.
///
/// Omits empty sections. Returns empty string if no metadata is present.
fn format_metadata(project: &Project) -> String {
    let mut parts = Vec::new();

    if !project.languages.is_empty() {
        let langs: Vec<&str> = project.languages.iter().map(abbreviate_language).collect();
        parts.push(langs.join(", "));
    }

    if !project.frameworks.is_empty() {
        let fws: Vec<&str> = project.frameworks.iter().map(|f| f.name.as_str()).collect();
        parts.push(fws.join(", "));
    }

    if let Some(pm) = &project.package_manager {
        parts.push(pm.clone());
    }

    parts.join(" · ")
}

/// Format the project summary for display inside a Panel.
///
/// This is separate from the prompt so it can be tested independently.
/// The `width` parameter controls the panel's rendered width.
pub fn format_project_summary(analysis: &RepoAnalysis, width: usize) -> String {
    let title = if analysis.is_monorepo {
        "Projects"
    } else {
        "Project"
    };

    let mut panel = Panel::titled(title);

    // Header line
    let diamond = theme::hint(&theme::DIAMOND);
    let header = if analysis.is_monorepo {
        format!(
            "{diamond} Monorepo detected \u{2014} {} projects",
            analysis.projects.len()
        )
    } else {
        format!("{diamond} Single project detected")
    };
    panel = panel.line("").line(&header).line("");

    // Project lines
    for project in &analysis.projects {
        let bullet = theme::heading(&theme::BULLET);
        let name = theme::heading(&project.name);
        let metadata = format_metadata(project);
        let meta_display = if metadata.is_empty() {
            String::new()
        } else {
            format!("  {}", theme::muted(&metadata))
        };

        panel = panel.line(&format!("{bullet} {name}{meta_display}"));

        let path = theme::muted(&project.path);
        panel = panel.line(&format!("  {path}"));
    }

    panel = panel.line("");
    panel.render(width)
}

/// Display detected projects and prompt for confirmation using the given terminal.
///
/// Writes the project summary via `term.write_line`, then uses `term.confirm`
/// (backed by `dialoguer::Confirm` in production) for a yes/no prompt.
/// Terminal width is detected automatically.
pub fn prompt_project_confirmation(
    analysis: &RepoAnalysis,
    term: &dyn TerminalIO,
) -> ConfirmAction {
    let width = console::Term::stdout()
        .size_checked()
        .map(|(_, cols)| cols as usize)
        .unwrap_or(80)
        .min(90);
    let summary = format_project_summary(analysis, width);
    term.write_line(&summary);

    match term.confirm("Accept project configuration?") {
        Ok(true) => ConfirmAction::Accept,
        Ok(false) | Err(_) => ConfirmAction::Reject,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::types::{Framework, FrameworkCategory, Language, Project};
    use crate::cli::ui::test_utils::MockTerminal;

    fn make_monorepo_analysis() -> RepoAnalysis {
        RepoAnalysis {
            is_monorepo: true,
            projects: vec![
                Project {
                    path: "apps/web".to_string(),
                    name: "Web App".to_string(),
                    languages: vec![Language::TypeScript, Language::JavaScript],
                    frameworks: vec![Framework {
                        name: "nextjs".to_string(),
                        category: FrameworkCategory::WebFrontend,
                    }],
                    package_manager: Some("npm".to_string()),
                    description: None,
                },
                Project {
                    path: "services/api".to_string(),
                    name: "API Service".to_string(),
                    languages: vec![Language::Rust],
                    frameworks: vec![
                        Framework {
                            name: "actix-web".to_string(),
                            category: FrameworkCategory::WebBackend,
                        },
                        Framework {
                            name: "diesel".to_string(),
                            category: FrameworkCategory::Data,
                        },
                    ],
                    package_manager: Some("cargo".to_string()),
                    description: Some("Backend API".to_string()),
                },
            ],
        }
    }

    fn make_single_project_analysis() -> RepoAnalysis {
        RepoAnalysis {
            is_monorepo: false,
            projects: vec![Project {
                path: ".".to_string(),
                name: "my-cli".to_string(),
                languages: vec![Language::Rust],
                frameworks: vec![Framework {
                    name: "clap".to_string(),
                    category: FrameworkCategory::Cli,
                }],
                package_manager: Some("cargo".to_string()),
                description: None,
            }],
        }
    }

    fn make_empty_analysis() -> RepoAnalysis {
        RepoAnalysis {
            is_monorepo: false,
            projects: vec![],
        }
    }

    /// Strip ANSI codes from rendered output for assertion.
    fn strip(rendered: &str) -> String {
        console::strip_ansi_codes(rendered).into_owned()
    }

    // ── format_project_summary tests ──

    #[test]
    fn format_summary_monorepo() {
        let analysis = make_monorepo_analysis();
        let output = format_project_summary(&analysis, 80);
        let plain = strip(&output);

        assert!(
            plain.contains("Monorepo detected"),
            "expected monorepo header in: {plain}"
        );
        assert!(
            plain.contains("2 projects"),
            "expected project count in: {plain}"
        );
        assert!(
            plain.contains("Web App"),
            "expected project name in: {plain}"
        );
        assert!(
            plain.contains("API Service"),
            "expected project name in: {plain}"
        );
        assert!(
            plain.contains("apps/web"),
            "expected project path in: {plain}"
        );
        assert!(
            plain.contains("services/api"),
            "expected project path in: {plain}"
        );
        // Languages are now abbreviated inline
        assert!(
            plain.contains("ts"),
            "expected abbreviated typescript in: {plain}"
        );
        assert!(
            plain.contains("js"),
            "expected abbreviated javascript in: {plain}"
        );
        assert!(plain.contains("rust"), "expected language in: {plain}");
        assert!(plain.contains("nextjs"), "expected framework in: {plain}");
        assert!(
            plain.contains("actix-web"),
            "expected framework in: {plain}"
        );
        assert!(plain.contains("diesel"), "expected framework in: {plain}");
        // Package managers now inline with · separator
        assert!(
            plain.contains("npm"),
            "expected package manager in: {plain}"
        );
        assert!(
            plain.contains("cargo"),
            "expected package manager in: {plain}"
        );
        // Panel box characters
        assert!(plain.contains('┌'), "expected panel top border in: {plain}");
        assert!(
            plain.contains('└'),
            "expected panel bottom border in: {plain}"
        );
        assert!(
            plain.contains("Projects"),
            "expected panel title in: {plain}"
        );
    }

    #[test]
    fn format_summary_single_project() {
        let analysis = make_single_project_analysis();
        let output = format_project_summary(&analysis, 80);
        let plain = strip(&output);

        assert!(
            plain.contains("Single project detected"),
            "expected single project header in: {plain}"
        );
        assert!(
            !plain.contains("Monorepo"),
            "should not contain monorepo in: {plain}"
        );
        assert!(
            plain.contains("my-cli"),
            "expected project name in: {plain}"
        );
        assert!(plain.contains("clap"), "expected framework in: {plain}");
        assert!(
            plain.contains("Project"),
            "expected panel title 'Project' in: {plain}"
        );
    }

    #[test]
    fn format_summary_empty_projects() {
        let analysis = make_empty_analysis();
        let output = format_project_summary(&analysis, 80);
        let plain = strip(&output);

        assert!(
            plain.contains("Single project detected"),
            "expected header even with no projects in: {plain}"
        );
    }

    #[test]
    fn format_summary_project_without_optional_fields() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            projects: vec![Project {
                path: ".".to_string(),
                name: "bare".to_string(),
                languages: vec![],
                frameworks: vec![],
                package_manager: None,
                description: None,
            }],
        };
        let output = format_project_summary(&analysis, 80);
        let plain = strip(&output);

        assert!(plain.contains("bare"), "expected project name in: {plain}");
        // New format doesn't use "Languages:" or "Frameworks:" labels
        assert!(
            !plain.contains("Languages:"),
            "should not show languages label in: {plain}"
        );
        assert!(
            !plain.contains("Frameworks:"),
            "should not show frameworks label in: {plain}"
        );
        assert!(
            !plain.contains("Package manager:"),
            "should not show package manager label in: {plain}"
        );
    }

    // ── format_metadata / abbreviate_language tests ──

    #[test]
    fn abbreviate_language_all_variants() {
        assert_eq!(abbreviate_language(&Language::TypeScript), "ts");
        assert_eq!(abbreviate_language(&Language::JavaScript), "js");
        assert_eq!(abbreviate_language(&Language::Python), "py");
        assert_eq!(abbreviate_language(&Language::Rust), "rust");
        assert_eq!(abbreviate_language(&Language::Go), "go");
        assert_eq!(abbreviate_language(&Language::Java), "java");
        assert_eq!(abbreviate_language(&Language::Kotlin), "kotlin");
        assert_eq!(abbreviate_language(&Language::Swift), "swift");
        assert_eq!(abbreviate_language(&Language::Ruby), "ruby");
        assert_eq!(abbreviate_language(&Language::Php), "php");
        assert_eq!(abbreviate_language(&Language::C), "c");
        assert_eq!(abbreviate_language(&Language::Cpp), "cpp");
        assert_eq!(abbreviate_language(&Language::CSharp), "csharp");
        assert_eq!(abbreviate_language(&Language::Scala), "scala");
        assert_eq!(abbreviate_language(&Language::Elixir), "elixir");
        assert_eq!(
            abbreviate_language(&Language::Other("haskell".to_string())),
            "haskell"
        );
        assert_eq!(
            abbreviate_language(&Language::Other("other".to_string())),
            "other"
        );
    }

    #[test]
    fn format_metadata_full() {
        let project = Project {
            path: ".".to_string(),
            name: "test".to_string(),
            languages: vec![Language::Rust],
            frameworks: vec![Framework {
                name: "actix-web".to_string(),
                category: FrameworkCategory::WebBackend,
            }],
            package_manager: Some("cargo".to_string()),
            description: None,
        };
        let meta = format_metadata(&project);
        assert_eq!(meta, "rust · actix-web · cargo");
    }

    #[test]
    fn format_metadata_no_frameworks_no_pm() {
        let project = Project {
            path: ".".to_string(),
            name: "test".to_string(),
            languages: vec![Language::TypeScript],
            frameworks: vec![],
            package_manager: None,
            description: None,
        };
        let meta = format_metadata(&project);
        assert_eq!(meta, "ts");
    }

    #[test]
    fn format_metadata_empty() {
        let project = Project {
            path: ".".to_string(),
            name: "test".to_string(),
            languages: vec![],
            frameworks: vec![],
            package_manager: None,
            description: None,
        };
        let meta = format_metadata(&project);
        assert!(meta.is_empty());
    }

    #[test]
    fn format_metadata_multiple_languages() {
        let project = Project {
            path: ".".to_string(),
            name: "test".to_string(),
            languages: vec![Language::TypeScript, Language::JavaScript],
            frameworks: vec![],
            package_manager: Some("npm".to_string()),
            description: None,
        };
        let meta = format_metadata(&project);
        assert_eq!(meta, "ts, js · npm");
    }

    #[test]
    fn format_metadata_only_pm() {
        let project = Project {
            path: ".".to_string(),
            name: "test".to_string(),
            languages: vec![],
            frameworks: vec![],
            package_manager: Some("npm".to_string()),
            description: None,
        };
        let meta = format_metadata(&project);
        assert_eq!(meta, "npm");
    }

    #[test]
    fn format_metadata_multiple_frameworks() {
        let project = Project {
            path: ".".to_string(),
            name: "test".to_string(),
            languages: vec![Language::Rust],
            frameworks: vec![
                Framework {
                    name: "actix-web".to_string(),
                    category: FrameworkCategory::WebBackend,
                },
                Framework {
                    name: "diesel".to_string(),
                    category: FrameworkCategory::Data,
                },
            ],
            package_manager: None,
            description: None,
        };
        let meta = format_metadata(&project);
        assert_eq!(meta, "rust · actix-web, diesel");
    }

    #[test]
    fn format_summary_panel_width_60() {
        let analysis = make_single_project_analysis();
        let output = format_project_summary(&analysis, 60);
        let plain = strip(&output);

        // All lines should be 60 chars wide (panel lines)
        for line in plain.lines() {
            assert_eq!(
                line.chars().count(),
                60,
                "line should be 60 chars: '{line}'"
            );
        }
    }

    // ── prompt_project_confirmation tests ──
    //
    // MockTerminal uses the default `confirm()` impl which reads from
    // `read_line` and parses y/yes → true, anything else → false.

    #[test]
    fn prompt_accept() {
        let analysis = make_single_project_analysis();
        let term = MockTerminal::new(vec!["y"]);
        let result = prompt_project_confirmation(&analysis, &term);
        assert_eq!(result, ConfirmAction::Accept);
    }

    #[test]
    fn prompt_accept_yes() {
        let analysis = make_single_project_analysis();
        let term = MockTerminal::new(vec!["yes"]);
        let result = prompt_project_confirmation(&analysis, &term);
        assert_eq!(result, ConfirmAction::Accept);
    }

    #[test]
    fn prompt_reject() {
        let analysis = make_single_project_analysis();
        let term = MockTerminal::new(vec!["n"]);
        let result = prompt_project_confirmation(&analysis, &term);
        assert_eq!(result, ConfirmAction::Reject);
    }

    #[test]
    fn prompt_reject_no() {
        let analysis = make_single_project_analysis();
        let term = MockTerminal::new(vec!["no"]);
        let result = prompt_project_confirmation(&analysis, &term);
        assert_eq!(result, ConfirmAction::Reject);
    }

    #[test]
    fn prompt_invalid_defaults_to_reject() {
        let analysis = make_single_project_analysis();
        // Unrecognized input defaults to false → Reject
        let term = MockTerminal::new(vec!["x"]);
        let result = prompt_project_confirmation(&analysis, &term);
        assert_eq!(result, ConfirmAction::Reject);
    }

    #[test]
    fn prompt_io_error_returns_reject() {
        let analysis = make_single_project_analysis();
        // Empty inputs causes read_line to return Err → Reject
        let term = MockTerminal::new(vec![]);
        let result = prompt_project_confirmation(&analysis, &term);
        assert_eq!(result, ConfirmAction::Reject);
    }

    #[test]
    fn prompt_writes_summary_to_output() {
        let analysis = make_single_project_analysis();
        let term = MockTerminal::new(vec!["y"]);
        prompt_project_confirmation(&analysis, &term);
        let output = term.output_text();
        assert!(
            output.contains("my-cli"),
            "expected project name in output: {output}"
        );
        assert!(
            output.contains("Single project detected"),
            "expected header in output: {output}"
        );
    }

    // ── MockTerminal trait coverage ──

    #[test]
    fn mock_terminal_select_files_returns_none() {
        let term = MockTerminal::new(vec![]).with_selection(None);
        let result = term.select_files("prompt", &["a".to_string()], &[true]);
        assert_eq!(result.unwrap(), None);
    }
}
