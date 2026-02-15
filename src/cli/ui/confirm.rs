use crate::analysis::confirm::ConfirmAction;
use crate::analysis::types::RepoAnalysis;
use console::style;
use std::fmt::Write as FmtWrite;
use std::io::{self, Write as IoWrite};

/// Trait for reading user input, enabling test injection.
pub trait InputReader {
    fn read_line(&self) -> io::Result<String>;
}

/// Build the styled prompt string shown to the user.
pub fn prompt_text() -> String {
    format!(
        "  {} {} {} {}",
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

    if analysis.is_monorepo {
        writeln!(
            output,
            "\n  {} Monorepo detected with {} project(s):\n",
            style("◆").cyan(),
            analysis.projects.len()
        )
        .unwrap();
    } else {
        writeln!(
            output,
            "\n  {} Single project detected:\n",
            style("◆").cyan()
        )
        .unwrap();
    }

    for project in &analysis.projects {
        writeln!(
            output,
            "  {} {}",
            style("▸").bold(),
            style(&project.name).bold()
        )
        .unwrap();
        writeln!(output, "    Path: {}", style(&project.path).dim()).unwrap();

        if !project.languages.is_empty() {
            let langs: Vec<String> = project
                .languages
                .iter()
                .map(|l| format!("{l:?}").to_lowercase())
                .collect();
            writeln!(output, "    Languages: {}", langs.join(", ")).unwrap();
        }

        if !project.frameworks.is_empty() {
            let fws: Vec<String> = project.frameworks.iter().map(|f| f.name.clone()).collect();
            writeln!(output, "    Frameworks: {}", fws.join(", ")).unwrap();
        }

        if let Some(pm) = &project.package_manager {
            writeln!(output, "    Package manager: {pm}").unwrap();
        }

        writeln!(output).unwrap();
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

/// Display detected projects and prompt with an injectable reader.
///
/// Prints the project summary to stderr, then loops reading input from `reader`
/// until the user enters a valid response (`a`/`accept` or `r`/`reject`/`q`/`quit`).
pub fn prompt_project_confirmation(
    analysis: &RepoAnalysis,
    reader: &dyn InputReader,
) -> ConfirmAction {
    let _ = write!(io::stderr(), "{}", format_project_summary(analysis));

    loop {
        match reader.read_line() {
            Ok(input) => match parse_confirm_input(&input) {
                Some(action) => return action,
                None => {
                    let _ = writeln!(io::stderr(), "{}", invalid_input_hint());
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A mock reader that returns a fixed string.
    struct MockReader {
        response: String,
    }

    impl MockReader {
        fn new(response: &str) -> Self {
            Self {
                response: response.to_string(),
            }
        }
    }

    impl InputReader for MockReader {
        fn read_line(&self) -> io::Result<String> {
            Ok(self.response.clone())
        }
    }

    /// A mock reader that returns different responses on successive calls.
    struct SequenceReader {
        responses: Vec<String>,
        index: AtomicUsize,
    }

    impl SequenceReader {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                responses: responses.into_iter().map(String::from).collect(),
                index: AtomicUsize::new(0),
            }
        }
    }

    impl InputReader for SequenceReader {
        fn read_line(&self) -> io::Result<String> {
            let i = self.index.fetch_add(1, Ordering::SeqCst);
            if i < self.responses.len() {
                Ok(self.responses[i].clone())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "no more input",
                ))
            }
        }
    }

    /// A mock reader that always returns an IO error.
    struct ErrorReader;

    impl InputReader for ErrorReader {
        fn read_line(&self) -> io::Result<String> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "pipe broken"))
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
        let reader = MockReader::new("a");
        let result = prompt_project_confirmation(&analysis, &reader);
        assert_eq!(result, ConfirmAction::Accept);
    }

    #[test]
    fn prompt_accept_full() {
        let analysis = make_single_project_analysis();
        let reader = MockReader::new("accept");
        let result = prompt_project_confirmation(&analysis, &reader);
        assert_eq!(result, ConfirmAction::Accept);
    }

    #[test]
    fn prompt_accept_case_insensitive() {
        let analysis = make_single_project_analysis();
        let reader = MockReader::new("Accept");
        let result = prompt_project_confirmation(&analysis, &reader);
        assert_eq!(result, ConfirmAction::Accept);
    }

    #[test]
    fn prompt_reject_short() {
        let analysis = make_single_project_analysis();
        let reader = MockReader::new("r");
        let result = prompt_project_confirmation(&analysis, &reader);
        assert_eq!(result, ConfirmAction::Reject);
    }

    #[test]
    fn prompt_reject_full() {
        let analysis = make_single_project_analysis();
        let reader = MockReader::new("reject");
        let result = prompt_project_confirmation(&analysis, &reader);
        assert_eq!(result, ConfirmAction::Reject);
    }

    #[test]
    fn prompt_quit_short() {
        let analysis = make_single_project_analysis();
        let reader = MockReader::new("q");
        let result = prompt_project_confirmation(&analysis, &reader);
        assert_eq!(result, ConfirmAction::Reject);
    }

    #[test]
    fn prompt_quit_full() {
        let analysis = make_single_project_analysis();
        let reader = MockReader::new("quit");
        let result = prompt_project_confirmation(&analysis, &reader);
        assert_eq!(result, ConfirmAction::Reject);
    }

    #[test]
    fn prompt_invalid_then_accept() {
        let analysis = make_single_project_analysis();
        let reader = SequenceReader::new(vec!["x", "invalid", "a"]);
        let result = prompt_project_confirmation(&analysis, &reader);
        assert_eq!(result, ConfirmAction::Accept);
    }

    #[test]
    fn prompt_invalid_then_reject() {
        let analysis = make_single_project_analysis();
        let reader = SequenceReader::new(vec!["", "nope", "r"]);
        let result = prompt_project_confirmation(&analysis, &reader);
        assert_eq!(result, ConfirmAction::Reject);
    }

    #[test]
    fn prompt_io_error_returns_reject() {
        let analysis = make_single_project_analysis();
        let reader = ErrorReader;
        let result = prompt_project_confirmation(&analysis, &reader);
        assert_eq!(result, ConfirmAction::Reject);
    }

    #[test]
    fn prompt_whitespace_trimmed() {
        let analysis = make_single_project_analysis();
        let reader = MockReader::new("  a  ");
        let result = prompt_project_confirmation(&analysis, &reader);
        assert_eq!(result, ConfirmAction::Accept);
    }
}
