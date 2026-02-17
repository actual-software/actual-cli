use crate::analysis::confirm::ConfirmAction;
use crate::analysis::types::RepoAnalysis;
use crate::cli::ui::file_confirm::TerminalIO;
use console::style;
use std::fmt::Write as FmtWrite;

/// Build the styled prompt string shown to the user.
pub fn prompt_text() -> String {
    format!(
        "  Confirm project configuration — {} {} {} {} ",
        style("[a]").green().bold(),
        "accept",
        style("[r]").red().bold(),
        "reject"
    )
}

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

/// Build the "invalid input" hint shown when the user enters unrecognized text.
pub fn invalid_input_hint() -> String {
    format!(
        "  {} Please enter {} or {}",
        style("?").yellow(),
        style("[a]").green().bold(),
        style("[r]").red().bold()
    )
}

/// Parse a single input string into a [`ConfirmAction`], if valid.
///
/// Returns `None` for unrecognized input.
pub fn parse_confirm_input(input: &str) -> Option<ConfirmAction> {
    match input.trim().to_lowercase().as_str() {
        "a" | "accept" => Some(ConfirmAction::Accept),
        "r" | "reject" | "q" | "quit" => Some(ConfirmAction::Reject),
        _ => None,
    }
}

/// Display detected projects and prompt using the given terminal.
///
/// Writes the project summary via `term.write_line`, then loops reading
/// input via `term.read_line` until the user enters a valid response
/// (`a`/`accept` or `r`/`reject`/`q`/`quit`).
pub fn prompt_project_confirmation(
    analysis: &RepoAnalysis,
    term: &dyn TerminalIO,
) -> ConfirmAction {
    let summary = format_project_summary(analysis);
    term.write_line(&summary);

    let prompt = prompt_text();

    loop {
        match term.read_line(&prompt) {
            Ok(input) => match parse_confirm_input(&input) {
                Some(action) => return action,
                None => {
                    let hint = format!("\n{}\n", invalid_input_hint());
                    term.write_line(&hint);
                }
            },
            Err(_) => return ConfirmAction::Reject,
        }
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

    // ── prompt_text tests ──

    #[test]
    fn prompt_text_contains_accept_and_reject() {
        let text = prompt_text();
        assert!(
            text.contains("Confirm project configuration"),
            "expected label in: {text}"
        );
        assert!(text.contains("accept"), "expected 'accept' in: {text}");
        assert!(text.contains("reject"), "expected 'reject' in: {text}");
    }

    // ── invalid_input_hint tests ──

    #[test]
    fn invalid_input_hint_contains_options() {
        let hint = invalid_input_hint();
        assert!(hint.contains("Please enter"), "expected prompt in: {hint}");
    }

    // ── parse_confirm_input tests ──

    #[test]
    fn parse_accept_short() {
        assert_eq!(parse_confirm_input("a"), Some(ConfirmAction::Accept));
    }

    #[test]
    fn parse_accept_full() {
        assert_eq!(parse_confirm_input("accept"), Some(ConfirmAction::Accept));
    }

    #[test]
    fn parse_accept_case_insensitive() {
        assert_eq!(parse_confirm_input("Accept"), Some(ConfirmAction::Accept));
    }

    #[test]
    fn parse_reject_short() {
        assert_eq!(parse_confirm_input("r"), Some(ConfirmAction::Reject));
    }

    #[test]
    fn parse_reject_full() {
        assert_eq!(parse_confirm_input("reject"), Some(ConfirmAction::Reject));
    }

    #[test]
    fn parse_quit_short() {
        assert_eq!(parse_confirm_input("q"), Some(ConfirmAction::Reject));
    }

    #[test]
    fn parse_quit_full() {
        assert_eq!(parse_confirm_input("quit"), Some(ConfirmAction::Reject));
    }

    #[test]
    fn parse_whitespace_trimmed() {
        assert_eq!(parse_confirm_input("  a  "), Some(ConfirmAction::Accept));
    }

    #[test]
    fn parse_invalid_returns_none() {
        assert_eq!(parse_confirm_input("x"), None);
        assert_eq!(parse_confirm_input(""), None);
        assert_eq!(parse_confirm_input("yes"), None);
    }

    // ── prompt_project_confirmation tests ──

    #[test]
    fn prompt_accept_short() {
        let analysis = make_single_project_analysis();
        let term = MockTerminal::new(vec!["a"]);
        let result = prompt_project_confirmation(&analysis, &term);
        assert_eq!(result, ConfirmAction::Accept);
    }

    #[test]
    fn prompt_accept_full() {
        let analysis = make_single_project_analysis();
        let term = MockTerminal::new(vec!["accept"]);
        let result = prompt_project_confirmation(&analysis, &term);
        assert_eq!(result, ConfirmAction::Accept);
    }

    #[test]
    fn prompt_accept_case_insensitive() {
        let analysis = make_single_project_analysis();
        let term = MockTerminal::new(vec!["Accept"]);
        let result = prompt_project_confirmation(&analysis, &term);
        assert_eq!(result, ConfirmAction::Accept);
    }

    #[test]
    fn prompt_reject_short() {
        let analysis = make_single_project_analysis();
        let term = MockTerminal::new(vec!["r"]);
        let result = prompt_project_confirmation(&analysis, &term);
        assert_eq!(result, ConfirmAction::Reject);
    }

    #[test]
    fn prompt_reject_full() {
        let analysis = make_single_project_analysis();
        let term = MockTerminal::new(vec!["reject"]);
        let result = prompt_project_confirmation(&analysis, &term);
        assert_eq!(result, ConfirmAction::Reject);
    }

    #[test]
    fn prompt_quit_short() {
        let analysis = make_single_project_analysis();
        let term = MockTerminal::new(vec!["q"]);
        let result = prompt_project_confirmation(&analysis, &term);
        assert_eq!(result, ConfirmAction::Reject);
    }

    #[test]
    fn prompt_quit_full() {
        let analysis = make_single_project_analysis();
        let term = MockTerminal::new(vec!["quit"]);
        let result = prompt_project_confirmation(&analysis, &term);
        assert_eq!(result, ConfirmAction::Reject);
    }

    #[test]
    fn prompt_invalid_then_accept() {
        let analysis = make_single_project_analysis();
        let term = MockTerminal::new(vec!["x", "invalid", "a"]);
        let result = prompt_project_confirmation(&analysis, &term);
        assert_eq!(result, ConfirmAction::Accept);
        let output = term.output_text();
        assert!(
            output.contains("Please enter"),
            "expected hint in output: {output}"
        );
        // Prompt text should be re-displayed after invalid input
        assert!(
            output.contains("accept"),
            "expected prompt text in output: {output}"
        );
        assert!(
            output.contains("reject"),
            "expected prompt text in output: {output}"
        );
    }

    #[test]
    fn prompt_invalid_then_reject() {
        let analysis = make_single_project_analysis();
        let term = MockTerminal::new(vec!["", "nope", "r"]);
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
    fn prompt_whitespace_trimmed() {
        let analysis = make_single_project_analysis();
        let term = MockTerminal::new(vec!["  a  "]);
        let result = prompt_project_confirmation(&analysis, &term);
        assert_eq!(result, ConfirmAction::Accept);
    }

    #[test]
    fn prompt_sequence_exhausted_returns_reject() {
        let analysis = make_single_project_analysis();
        // First response is invalid, then inputs exhausted → Err → Reject
        let term = MockTerminal::new(vec!["x"]);
        let result = prompt_project_confirmation(&analysis, &term);
        assert_eq!(result, ConfirmAction::Reject);
    }

    #[test]
    fn prompt_writes_summary_to_output() {
        let analysis = make_single_project_analysis();
        let term = MockTerminal::new(vec!["a"]);
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
        // Prompt text should appear after the summary
        assert!(
            output.contains("accept"),
            "expected prompt text in output: {output}"
        );
        assert!(
            output.contains("reject"),
            "expected prompt text in output: {output}"
        );
    }
}
