use crate::analysis::confirm::ConfirmAction;
use crate::analysis::types::RepoAnalysis;
use crate::cli::ui::file_confirm::TerminalIO;
use console::style;
use std::fmt::Write as FmtWrite;

/// Format the project summary for display.
///
/// This is separate from the prompt so it can be tested independently.
pub fn format_project_summary(analysis: &RepoAnalysis) -> String {
    let mut output = String::new();

    let diamond = style("◆").cyan();
    if analysis.is_monorepo {
        let _ = writeln!(
            output,
            "\n  {diamond} Monorepo detected with {} project(s):\n",
            analysis.projects.len()
        );
    } else {
        let _ = writeln!(output, "\n  {diamond} Single project detected:\n");
    }

    for project in &analysis.projects {
        let bullet = style("▸").bold();
        let name = style(&project.name).bold();
        let path = style(&project.path).dim();
        let _ = writeln!(output, "  {bullet} {name}");
        let _ = writeln!(output, "    Path: {path}");

        if !project.languages.is_empty() {
            let langs: Vec<String> = project
                .languages
                .iter()
                .map(|l| format!("{l:?}").to_lowercase())
                .collect();
            let joined = langs.join(", ");
            let _ = writeln!(output, "    Languages: {joined}");
        }

        if !project.frameworks.is_empty() {
            let fws: Vec<String> = project.frameworks.iter().map(|f| f.name.clone()).collect();
            let joined = fws.join(", ");
            let _ = writeln!(output, "    Frameworks: {joined}");
        }

        if let Some(pm) = &project.package_manager {
            let _ = writeln!(output, "    Package manager: {pm}");
        }

        let _ = writeln!(output);
    }

    output
}

/// Display detected projects and prompt for confirmation using the given terminal.
///
/// Writes the project summary via `term.write_line`, then uses `term.confirm`
/// (backed by `dialoguer::Confirm` in production) for a yes/no prompt.
pub fn prompt_project_confirmation(
    analysis: &RepoAnalysis,
    term: &dyn TerminalIO,
) -> ConfirmAction {
    let summary = format_project_summary(analysis);
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
    use crate::error::ActualError;
    use std::sync::Mutex;

    /// A mock terminal for testing that records output and returns
    /// predetermined inputs in sequence.
    ///
    /// Uses the default `confirm()` implementation from `TerminalIO`,
    /// which reads from `read_line` and parses y/n.
    struct MockTerminal {
        inputs: Mutex<Vec<String>>,
        output: Mutex<Vec<String>>,
    }

    impl MockTerminal {
        fn new(inputs: Vec<&str>) -> Self {
            Self {
                inputs: Mutex::new(inputs.into_iter().map(|s| s.to_string()).collect()),
                output: Mutex::new(Vec::new()),
            }
        }

        fn output_text(&self) -> String {
            self.output.lock().unwrap().join("\n")
        }
    }

    impl TerminalIO for MockTerminal {
        fn read_line(&self, prompt: &str) -> Result<String, ActualError> {
            self.output.lock().unwrap().push(prompt.to_string());
            let mut inputs = self.inputs.lock().unwrap();
            if inputs.is_empty() {
                return Err(ActualError::UserCancelled);
            }
            Ok(inputs.remove(0))
        }

        fn write_line(&self, text: &str) {
            self.output.lock().unwrap().push(text.to_string());
        }

        fn select_files(
            &self,
            _prompt: &str,
            _items: &[String],
            _defaults: &[bool],
        ) -> Result<Option<Vec<usize>>, ActualError> {
            Ok(None)
        }
    }

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

    // ── format_project_summary tests ──

    #[test]
    fn format_summary_monorepo() {
        let analysis = make_monorepo_analysis();
        let output = format_project_summary(&analysis);

        assert!(
            output.contains("Monorepo detected with 2 project(s)"),
            "expected monorepo header in: {output}"
        );
        assert!(
            output.contains("Web App"),
            "expected project name in: {output}"
        );
        assert!(
            output.contains("API Service"),
            "expected project name in: {output}"
        );
        assert!(
            output.contains("apps/web"),
            "expected project path in: {output}"
        );
        assert!(
            output.contains("services/api"),
            "expected project path in: {output}"
        );
        assert!(
            output.contains("typescript"),
            "expected language in: {output}"
        );
        assert!(
            output.contains("javascript"),
            "expected language in: {output}"
        );
        assert!(output.contains("rust"), "expected language in: {output}");
        assert!(output.contains("nextjs"), "expected framework in: {output}");
        assert!(
            output.contains("actix-web"),
            "expected framework in: {output}"
        );
        assert!(output.contains("diesel"), "expected framework in: {output}");
        assert!(
            output.contains("Package manager: npm"),
            "expected package manager in: {output}"
        );
        assert!(
            output.contains("Package manager: cargo"),
            "expected package manager in: {output}"
        );
    }

    #[test]
    fn format_summary_single_project() {
        let analysis = make_single_project_analysis();
        let output = format_project_summary(&analysis);

        assert!(
            output.contains("Single project detected"),
            "expected single project header in: {output}"
        );
        assert!(
            !output.contains("Monorepo"),
            "should not contain monorepo in: {output}"
        );
        assert!(
            output.contains("my-cli"),
            "expected project name in: {output}"
        );
        assert!(output.contains("clap"), "expected framework in: {output}");
    }

    #[test]
    fn format_summary_empty_projects() {
        let analysis = make_empty_analysis();
        let output = format_project_summary(&analysis);

        assert!(
            output.contains("Single project detected"),
            "expected header even with no projects in: {output}"
        );
        // No project blocks should be present
        assert!(
            !output.contains("Path:"),
            "should not contain project paths in: {output}"
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
        let output = format_project_summary(&analysis);

        assert!(
            output.contains("bare"),
            "expected project name in: {output}"
        );
        assert!(
            !output.contains("Languages:"),
            "should not show languages when empty in: {output}"
        );
        assert!(
            !output.contains("Frameworks:"),
            "should not show frameworks when empty in: {output}"
        );
        assert!(
            !output.contains("Package manager:"),
            "should not show package manager when None in: {output}"
        );
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
}
