use crate::analysis::confirm::ConfirmAction;
use crate::analysis::types::{Language, LanguageStat, Project, RepoAnalysis};
use crate::cli::ui::term_size;
use crate::cli::ui::terminal::TerminalIO;
use crate::error::ActualError;

use super::theme;

/// Format a number with comma thousand separators (ASCII-only).
fn format_number(n: usize) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

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

/// Format a [`LanguageStat`] as abbreviated name with optional LOC count.
fn format_language_stat(ls: &LanguageStat) -> String {
    let abbr = abbreviate_language(&ls.language);
    if ls.loc > 0 {
        format!("{} ({} loc)", abbr, format_number(ls.loc))
    } else {
        abbr.to_string()
    }
}

/// Returns indented multi-line metadata detail lines for a project.
/// Returns empty vec if no metadata is present.
fn format_metadata_lines(project: &Project) -> Vec<String> {
    let mut lines = Vec::new();

    if !project.languages.is_empty() {
        let langs: Vec<String> = project.languages.iter().map(format_language_stat).collect();
        lines.push(format!(
            "    {}  {}",
            theme::muted("Languages:"),
            langs.join(", ")
        ));
    }

    if !project.frameworks.is_empty() {
        let frameworks: Vec<String> = project
            .frameworks
            .iter()
            .map(|f| console::strip_ansi_codes(f.name.as_str()).into_owned())
            .collect();
        lines.push(format!(
            "    {} {}",
            theme::muted("Frameworks:"),
            frameworks.join(", ")
        ));
    }

    if let Some(pm) = &project.package_manager {
        let value = console::strip_ansi_codes(pm).into_owned();
        lines.push(format!("    {}    {}", theme::muted("Manager:"), value));
    }

    let total_deps = project.dep_count + project.dev_dep_count;
    if total_deps > 0 {
        lines.push(format!(
            "    {}       {} prod, {} dev",
            theme::muted("Deps:"),
            project.dep_count,
            project.dev_dep_count,
        ));
    }

    lines
}

/// Label column width — "Frameworks:" is the longest at 11 chars.
const LABEL_WIDTH: usize = 11;

/// Format the project summary as clean ASCII text (no ANSI codes, no Unicode).
///
/// Used by the TUI renderer where ratatui handles all styling. Output uses
/// ASCII-only characters and right-aligns labels so values start at the same column.
pub fn format_project_summary_plain(analysis: &RepoAnalysis) -> String {
    let header = if analysis.is_monorepo {
        match &analysis.workspace_type {
            Some(wt) => format!(
                "* {wt} monorepo detected -- {} projects",
                analysis.projects.len()
            ),
            None => format!(
                "* Monorepo detected -- {} projects",
                analysis.projects.len()
            ),
        }
    } else {
        "* Single project detected".to_string()
    };

    let mut lines = vec![header];

    for project in &analysis.projects {
        let safe_name = console::strip_ansi_codes(&project.name);

        let name_line = if analysis.is_monorepo {
            let safe_path = console::strip_ansi_codes(&project.path);
            format!("  {safe_name}  ({safe_path})")
        } else {
            format!("  {safe_name}")
        };
        lines.push(name_line);

        lines.extend(format_metadata_lines_plain(project));
    }

    lines.join("\n")
}

/// Returns indented multi-line metadata detail lines for a project (ASCII-only).
/// Right-aligns labels so all colons and values align vertically.
fn format_metadata_lines_plain(project: &Project) -> Vec<String> {
    let mut lines = Vec::new();

    if !project.languages.is_empty() {
        let langs: Vec<String> = project.languages.iter().map(format_language_stat).collect();
        lines.push(format!(
            "    {:>width$}  {}",
            "Languages:",
            langs.join(", "),
            width = LABEL_WIDTH,
        ));
    }

    if !project.frameworks.is_empty() {
        let frameworks: Vec<String> = project
            .frameworks
            .iter()
            .map(|f| console::strip_ansi_codes(f.name.as_str()).into_owned())
            .collect();
        lines.push(format!(
            "    {:>width$}  {}",
            "Frameworks:",
            frameworks.join(", "),
            width = LABEL_WIDTH,
        ));
    }

    if let Some(pm) = &project.package_manager {
        let value = console::strip_ansi_codes(pm).into_owned();
        lines.push(format!(
            "    {:>width$}  {}",
            "Manager:",
            value,
            width = LABEL_WIDTH,
        ));
    }

    let total_deps = project.dep_count + project.dev_dep_count;
    if total_deps > 0 {
        lines.push(format!(
            "    {:>width$}  {} prod, {} dev",
            "Deps:",
            project.dep_count,
            project.dev_dep_count,
            width = LABEL_WIDTH,
        ));
    }

    lines
}

/// Format the project summary as clean multi-line labeled text.
///
/// This is separate from the prompt so it can be tested independently.
/// The `width` parameter is retained for API compatibility but is no longer
/// used to render a panel — the output is plain text.
pub fn format_project_summary(analysis: &RepoAnalysis, _width: usize) -> String {
    let diamond = theme::hint(&theme::DIAMOND);

    let header = if analysis.is_monorepo {
        match &analysis.workspace_type {
            Some(wt) => format!(
                "{diamond} {wt} monorepo detected \u{2014} {} projects",
                analysis.projects.len()
            ),
            None => format!(
                "{diamond} Monorepo detected \u{2014} {} projects",
                analysis.projects.len()
            ),
        }
    } else {
        format!("{diamond} Single project detected")
    };

    let mut lines = vec![header];

    for project in &analysis.projects {
        let safe_name = console::strip_ansi_codes(&project.name);
        let name = theme::heading(&safe_name);

        let name_line = if analysis.is_monorepo {
            let safe_path = console::strip_ansi_codes(&project.path);
            let path = theme::muted(&safe_path);
            format!("  {name}  ({path})")
        } else {
            format!("  {name}")
        };
        lines.push(name_line);

        lines.extend(format_metadata_lines(project));
    }

    lines.join("\n")
}

/// Display detected projects and prompt for confirmation using the given terminal.
///
/// Writes the project summary via `term.write_line`, then uses `term.confirm`
/// (backed by `dialoguer::Confirm` in production) for a yes/no prompt.
/// Terminal width is detected automatically.
pub fn prompt_project_confirmation(
    analysis: &RepoAnalysis,
    term: &dyn TerminalIO,
) -> Result<ConfirmAction, ActualError> {
    let width = term_size::terminal_width();
    let summary = format_project_summary(analysis, width);
    term.write_line(&summary);

    match term.confirm("Accept project configuration?") {
        Ok(true) => Ok(ConfirmAction::Accept),
        Ok(false) => Ok(ConfirmAction::Reject),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::types::{Framework, FrameworkCategory, Language, LanguageStat, Project};
    use crate::cli::ui::test_utils::MockTerminal;

    fn make_monorepo_analysis() -> RepoAnalysis {
        RepoAnalysis {
            is_monorepo: true,
            workspace_type: Some(crate::analysis::types::WorkspaceType::Pnpm),
            projects: vec![
                Project {
                    path: "apps/web".to_string(),
                    name: "Web App".to_string(),
                    languages: vec![
                        LanguageStat {
                            language: Language::TypeScript,
                            loc: 0,
                        },
                        LanguageStat {
                            language: Language::JavaScript,
                            loc: 0,
                        },
                    ],
                    frameworks: vec![Framework {
                        name: "nextjs".to_string(),
                        category: FrameworkCategory::WebFrontend,
                    }],
                    package_manager: Some("npm".to_string()),
                    description: None,
                    dep_count: 0,
                    dev_dep_count: 0,
                },
                Project {
                    path: "services/api".to_string(),
                    name: "API Service".to_string(),
                    languages: vec![LanguageStat {
                        language: Language::Rust,
                        loc: 0,
                    }],
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
                    dep_count: 0,
                    dev_dep_count: 0,
                },
            ],
        }
    }

    fn make_single_project_analysis() -> RepoAnalysis {
        RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![Project {
                path: ".".to_string(),
                name: "my-cli".to_string(),
                languages: vec![LanguageStat {
                    language: Language::Rust,
                    loc: 0,
                }],
                frameworks: vec![Framework {
                    name: "clap".to_string(),
                    category: FrameworkCategory::Cli,
                }],
                package_manager: Some("cargo".to_string()),
                description: None,
                dep_count: 0,
                dev_dep_count: 0,
            }],
        }
    }

    fn make_empty_analysis() -> RepoAnalysis {
        RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
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
            plain.contains("pnpm monorepo detected"),
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
        // Package managers shown under Manager: label
        assert!(
            plain.contains("npm"),
            "expected package manager in: {plain}"
        );
        assert!(
            plain.contains("cargo"),
            "expected package manager in: {plain}"
        );
        // New labeled layout uses Frameworks: and Manager: labels
        assert!(
            plain.contains("Frameworks:"),
            "expected Frameworks: label in: {plain}"
        );
        assert!(
            plain.contains("Manager:"),
            "expected Manager: label in: {plain}"
        );
        // No old dot-separated inline format
        assert!(
            !plain.contains(" · "),
            "should not contain · separator in: {plain}"
        );
        // No panel box characters — output is plain text
        assert!(
            !plain.contains('┌'),
            "should not contain panel top border in: {plain}"
        );
        assert!(
            !plain.contains('└'),
            "should not contain panel bottom border in: {plain}"
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
        // New labeled layout
        assert!(
            plain.contains("Frameworks:"),
            "expected Frameworks: label in: {plain}"
        );
        assert!(
            plain.contains("Manager:"),
            "expected Manager: label in: {plain}"
        );
        // No old dot-separated inline format
        assert!(
            !plain.contains(" · "),
            "should not contain · separator in: {plain}"
        );
        // No panel box characters — output is plain text
        assert!(
            !plain.contains('┌'),
            "should not contain panel borders in: {plain}"
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
    fn format_summary_monorepo_without_workspace_type() {
        // Covers the None branch: "Monorepo detected" without workspace prefix
        let analysis = RepoAnalysis {
            is_monorepo: true,
            workspace_type: None,
            projects: vec![Project {
                path: "pkg/a".to_string(),
                name: "a".to_string(),
                languages: vec![],
                frameworks: vec![],
                package_manager: None,
                description: None,
                dep_count: 0,
                dev_dep_count: 0,
            }],
        };
        let themed = strip(&format_project_summary(&analysis, 80));
        assert!(
            themed.contains("Monorepo detected"),
            "expected 'Monorepo detected' in themed output: {themed}"
        );
        assert!(
            !themed.contains("pnpm"),
            "should not contain workspace type: {themed}"
        );
        let plain = format_project_summary_plain(&analysis);
        assert!(
            plain.contains("* Monorepo detected"),
            "expected '* Monorepo detected' in plain output: {plain}"
        );
        assert!(
            !plain.contains("pnpm"),
            "should not contain workspace type: {plain}"
        );
    }

    #[test]
    fn format_summary_project_without_optional_fields() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![Project {
                path: ".".to_string(),
                name: "bare".to_string(),
                languages: vec![],
                frameworks: vec![],
                package_manager: None,
                description: None,
                dep_count: 0,
                dev_dep_count: 0,
            }],
        };
        let output = format_project_summary(&analysis, 80);
        let plain = strip(&output);

        assert!(plain.contains("bare"), "expected project name in: {plain}");
        // Labels are omitted when the corresponding field is empty
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

    // ── format_metadata_lines / abbreviate_language tests ──

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
            languages: vec![LanguageStat {
                language: Language::Rust,
                loc: 0,
            }],
            frameworks: vec![Framework {
                name: "actix-web".to_string(),
                category: FrameworkCategory::WebBackend,
            }],
            package_manager: Some("cargo".to_string()),
            description: None,
            dep_count: 0,
            dev_dep_count: 0,
        };
        let lines = format_metadata_lines(&project);
        assert_eq!(lines.len(), 3, "expected 3 lines, got: {lines:?}");
        let plain0 = strip(&lines[0]);
        let plain1 = strip(&lines[1]);
        let plain2 = strip(&lines[2]);
        assert!(
            plain0.contains("Languages:") && plain0.contains("rust"),
            "line[0] should contain Languages: and rust, got: {plain0}"
        );
        assert!(
            plain1.contains("Frameworks:") && plain1.contains("actix-web"),
            "line[1] should contain Frameworks: and actix-web, got: {plain1}"
        );
        assert!(
            plain2.contains("Manager:") && plain2.contains("cargo"),
            "line[2] should contain Manager: and cargo, got: {plain2}"
        );
    }

    #[test]
    fn format_metadata_no_frameworks_no_pm() {
        let project = Project {
            path: ".".to_string(),
            name: "test".to_string(),
            languages: vec![LanguageStat {
                language: Language::TypeScript,
                loc: 0,
            }],
            frameworks: vec![],
            package_manager: None,
            description: None,
            dep_count: 0,
            dev_dep_count: 0,
        };
        let lines = format_metadata_lines(&project);
        assert_eq!(lines.len(), 1, "expected 1 line, got: {lines:?}");
        let plain = strip(&lines[0]);
        assert!(
            plain.contains("Languages:") && plain.contains("ts"),
            "line should contain Languages: and ts, got: {plain}"
        );
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
            dep_count: 0,
            dev_dep_count: 0,
        };
        let lines = format_metadata_lines(&project);
        assert!(lines.is_empty(), "expected empty vec, got: {lines:?}");
    }

    #[test]
    fn format_metadata_multiple_languages() {
        let project = Project {
            path: ".".to_string(),
            name: "test".to_string(),
            languages: vec![
                LanguageStat {
                    language: Language::TypeScript,
                    loc: 0,
                },
                LanguageStat {
                    language: Language::JavaScript,
                    loc: 0,
                },
            ],
            frameworks: vec![],
            package_manager: Some("npm".to_string()),
            description: None,
            dep_count: 0,
            dev_dep_count: 0,
        };
        let lines = format_metadata_lines(&project);
        assert_eq!(lines.len(), 2, "expected 2 lines, got: {lines:?}");
        let plain0 = strip(&lines[0]);
        let plain1 = strip(&lines[1]);
        assert!(
            plain0.contains("Languages:") && plain0.contains("ts") && plain0.contains("js"),
            "line[0] should contain Languages: with ts and js, got: {plain0}"
        );
        assert!(
            plain1.contains("Manager:") && plain1.contains("npm"),
            "line[1] should contain Manager: and npm, got: {plain1}"
        );
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
            dep_count: 0,
            dev_dep_count: 0,
        };
        let lines = format_metadata_lines(&project);
        assert_eq!(lines.len(), 1, "expected 1 line, got: {lines:?}");
        let plain = strip(&lines[0]);
        assert!(
            plain.contains("Manager:") && plain.contains("npm"),
            "line should contain Manager: and npm, got: {plain}"
        );
    }

    #[test]
    fn format_metadata_multiple_frameworks() {
        let project = Project {
            path: ".".to_string(),
            name: "test".to_string(),
            languages: vec![LanguageStat {
                language: Language::Rust,
                loc: 0,
            }],
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
            dep_count: 0,
            dev_dep_count: 0,
        };
        let lines = format_metadata_lines(&project);
        assert_eq!(lines.len(), 2, "expected 2 lines, got: {lines:?}");
        let plain0 = strip(&lines[0]);
        let plain1 = strip(&lines[1]);
        assert!(
            plain0.contains("Languages:") && plain0.contains("rust"),
            "line[0] should contain Languages: and rust, got: {plain0}"
        );
        assert!(
            plain1.contains("Frameworks:")
                && plain1.contains("actix-web")
                && plain1.contains("diesel"),
            "line[1] should contain Frameworks: with actix-web and diesel, got: {plain1}"
        );
    }

    #[test]
    fn format_summary_width_ignored_no_panel() {
        let analysis = make_single_project_analysis();
        // Width parameter is accepted but no longer affects output — no panel is rendered
        let output = format_project_summary(&analysis, 60);
        let plain = strip(&output);

        // Content is still present
        assert!(
            plain.contains("Single project detected"),
            "expected header in: {plain}"
        );
        assert!(
            plain.contains("my-cli"),
            "expected project name in: {plain}"
        );
        // No panel box-drawing characters
        assert!(
            !plain.contains('┌'),
            "should not contain panel top border in: {plain}"
        );
        assert!(
            !plain.contains('└'),
            "should not contain panel bottom border in: {plain}"
        );
        assert!(
            !plain.contains('│'),
            "should not contain panel side border in: {plain}"
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
        let result = prompt_project_confirmation(&analysis, &term).unwrap();
        assert_eq!(result, ConfirmAction::Accept);
    }

    #[test]
    fn prompt_accept_yes() {
        let analysis = make_single_project_analysis();
        let term = MockTerminal::new(vec!["yes"]);
        let result = prompt_project_confirmation(&analysis, &term).unwrap();
        assert_eq!(result, ConfirmAction::Accept);
    }

    #[test]
    fn prompt_reject() {
        let analysis = make_single_project_analysis();
        let term = MockTerminal::new(vec!["n"]);
        let result = prompt_project_confirmation(&analysis, &term).unwrap();
        assert_eq!(result, ConfirmAction::Reject);
    }

    #[test]
    fn prompt_reject_no() {
        let analysis = make_single_project_analysis();
        let term = MockTerminal::new(vec!["no"]);
        let result = prompt_project_confirmation(&analysis, &term).unwrap();
        assert_eq!(result, ConfirmAction::Reject);
    }

    #[test]
    fn prompt_invalid_defaults_to_reject() {
        let analysis = make_single_project_analysis();
        // Unrecognized input defaults to false → Reject
        let term = MockTerminal::new(vec!["x"]);
        let result = prompt_project_confirmation(&analysis, &term).unwrap();
        assert_eq!(result, ConfirmAction::Reject);
    }

    #[test]
    fn prompt_io_error_propagates_error() {
        let analysis = make_single_project_analysis();
        // Empty inputs causes read_line to return Err → propagated as Err
        let term = MockTerminal::new(vec![]);
        let result = prompt_project_confirmation(&analysis, &term);
        assert!(
            result.is_err(),
            "expected Err when terminal confirm fails, got: {result:?}"
        );
    }

    #[test]
    fn prompt_writes_summary_to_output() {
        let analysis = make_single_project_analysis();
        let term = MockTerminal::new(vec!["y"]);
        prompt_project_confirmation(&analysis, &term).unwrap();
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

    // ── ANSI injection prevention tests ──

    #[test]
    fn format_summary_strips_ansi_from_project_name() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![Project {
                path: ".".to_string(),
                name: "\x1b[31mINJECTED\x1b[0m".to_string(),
                languages: vec![],
                frameworks: vec![],
                package_manager: None,
                description: None,
                dep_count: 0,
                dev_dep_count: 0,
            }],
        };
        let output = format_project_summary(&analysis, 80);
        assert!(
            !output.contains('\x1b'),
            "output must not contain raw ANSI escape codes from injected project name: {output:?}"
        );
        // The text content should still be present
        let plain = strip(&output);
        assert!(
            plain.contains("INJECTED"),
            "plain text 'INJECTED' should still appear: {plain}"
        );
    }

    #[test]
    fn format_summary_strips_ansi_from_project_path() {
        // Use a monorepo so the path is shown in the output (single-project format omits the path)
        let analysis = RepoAnalysis {
            is_monorepo: true,
            workspace_type: None,
            projects: vec![Project {
                path: "\x1b[32mPATH_INJECT\x1b[0m".to_string(),
                name: "safe-name".to_string(),
                languages: vec![],
                frameworks: vec![],
                package_manager: None,
                description: None,
                dep_count: 0,
                dev_dep_count: 0,
            }],
        };
        let output = format_project_summary(&analysis, 80);
        assert!(
            !output.contains('\x1b'),
            "output must not contain raw ANSI escape codes from injected project path: {output:?}"
        );
        let plain = strip(&output);
        assert!(
            plain.contains("PATH_INJECT"),
            "plain text 'PATH_INJECT' should still appear: {plain}"
        );
    }

    #[test]
    fn format_metadata_strips_ansi_from_framework_name() {
        let project = Project {
            path: ".".to_string(),
            name: "test".to_string(),
            languages: vec![],
            frameworks: vec![Framework {
                name: "\x1b[31mMALICIOUS\x1b[0m".to_string(),
                category: FrameworkCategory::WebFrontend,
            }],
            package_manager: None,
            description: None,
            dep_count: 0,
            dev_dep_count: 0,
        };
        let lines = format_metadata_lines(&project);
        let joined = lines.join("\n");
        assert!(
            !joined.contains('\x1b'),
            "metadata must not contain raw ANSI codes from injected framework name: {joined:?}"
        );
        assert!(
            joined.contains("MALICIOUS"),
            "text content should still be present: {joined}"
        );
    }

    #[test]
    fn format_metadata_strips_ansi_from_package_manager() {
        let project = Project {
            path: ".".to_string(),
            name: "test".to_string(),
            languages: vec![],
            frameworks: vec![],
            package_manager: Some("\x1b[33mINJECT_PM\x1b[0m".to_string()),
            description: None,
            dep_count: 0,
            dev_dep_count: 0,
        };
        let lines = format_metadata_lines(&project);
        let joined = lines.join("\n");
        assert!(
            !joined.contains('\x1b'),
            "metadata must not contain raw ANSI codes from injected package manager: {joined:?}"
        );
        assert!(
            joined.contains("INJECT_PM"),
            "text content should still be present: {joined}"
        );
    }

    // ── format_project_summary_plain tests ──

    #[test]
    fn format_summary_plain_single_project() {
        let analysis = make_single_project_analysis();
        let output = format_project_summary_plain(&analysis);
        assert!(output.contains("* Single project detected"));
        assert!(output.contains("my-cli"));
        assert!(output.contains("Frameworks:"));
        assert!(output.contains("Manager:"));
        // ASCII-only: no chars > 0x7F
        assert!(output.is_ascii(), "output must be ASCII-only: {output:?}");
        // No ANSI codes
        assert!(!output.contains('\x1b'));
    }

    #[test]
    fn format_summary_plain_monorepo() {
        let analysis = make_monorepo_analysis();
        let output = format_project_summary_plain(&analysis);
        assert!(output.contains("* pnpm monorepo detected -- 2 projects"));
        assert!(output.contains("Web App"));
        assert!(output.contains("API Service"));
        assert!(output.contains("apps/web"));
        assert!(output.contains("services/api"));
        // ASCII-only
        assert!(output.is_ascii(), "output must be ASCII-only: {output:?}");
        assert!(!output.contains('\x1b'));
    }

    #[test]
    fn format_summary_plain_empty_projects() {
        let analysis = make_empty_analysis();
        let output = format_project_summary_plain(&analysis);
        assert!(output.contains("* Single project detected"));
    }

    #[test]
    fn format_summary_plain_right_aligned_labels() {
        let analysis = make_single_project_analysis();
        let output = format_project_summary_plain(&analysis);
        // Find colon positions in metadata lines
        let colon_positions: Vec<usize> = output
            .lines()
            .filter(|l| {
                let trimmed = l.trim_start();
                trimmed.starts_with("Languages:")
                    || trimmed.starts_with("Frameworks:")
                    || trimmed.starts_with("Manager:")
            })
            .map(|l| l.find(':').unwrap())
            .collect();
        assert!(!colon_positions.is_empty(), "expected metadata lines");
        let first = colon_positions[0];
        for (i, pos) in colon_positions.iter().enumerate() {
            assert_eq!(
                *pos, first,
                "colon on line {i} at column {pos}, expected {first}"
            );
        }
    }

    #[test]
    fn format_summary_plain_no_optional_fields() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![Project {
                path: ".".to_string(),
                name: "bare".to_string(),
                languages: vec![],
                frameworks: vec![],
                package_manager: None,
                description: None,
                dep_count: 0,
                dev_dep_count: 0,
            }],
        };
        let output = format_project_summary_plain(&analysis);
        assert!(output.contains("bare"));
        assert!(!output.contains("Languages:"));
        assert!(!output.contains("Frameworks:"));
        assert!(!output.contains("Manager:"));
    }

    #[test]
    fn format_summary_plain_strips_ansi_injection() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![Project {
                path: ".".to_string(),
                name: "\x1b[31mINJECTED\x1b[0m".to_string(),
                languages: vec![],
                frameworks: vec![],
                package_manager: None,
                description: None,
                dep_count: 0,
                dev_dep_count: 0,
            }],
        };
        let output = format_project_summary_plain(&analysis);
        assert!(!output.contains('\x1b'));
        assert!(output.contains("INJECTED"));
    }

    #[test]
    fn format_metadata_plain_full() {
        let project = Project {
            path: ".".to_string(),
            name: "test".to_string(),
            languages: vec![LanguageStat {
                language: Language::Rust,
                loc: 0,
            }],
            frameworks: vec![Framework {
                name: "actix-web".to_string(),
                category: FrameworkCategory::WebBackend,
            }],
            package_manager: Some("cargo".to_string()),
            description: None,
            dep_count: 0,
            dev_dep_count: 0,
        };
        let lines = format_metadata_lines_plain(&project);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("Languages:") && lines[0].contains("rust"));
        assert!(lines[1].contains("Frameworks:") && lines[1].contains("actix-web"));
        assert!(lines[2].contains("Manager:") && lines[2].contains("cargo"));
    }

    // ── Deps display tests ──

    #[test]
    fn format_metadata_shows_deps_when_nonzero() {
        let project = Project {
            path: ".".to_string(),
            name: "test".to_string(),
            languages: vec![LanguageStat {
                language: Language::Rust,
                loc: 0,
            }],
            frameworks: vec![],
            package_manager: None,
            description: None,
            dep_count: 12,
            dev_dep_count: 8,
        };
        let lines = format_metadata_lines(&project);
        assert_eq!(lines.len(), 2, "expected Languages + Deps, got: {lines:?}");
        let plain = strip(&lines[1]);
        assert!(
            plain.contains("Deps:") && plain.contains("12 prod") && plain.contains("8 dev"),
            "expected Deps: line with counts, got: {plain}"
        );
    }

    #[test]
    fn format_metadata_hides_deps_when_zero() {
        let project = Project {
            path: ".".to_string(),
            name: "test".to_string(),
            languages: vec![LanguageStat {
                language: Language::Rust,
                loc: 0,
            }],
            frameworks: vec![],
            package_manager: None,
            description: None,
            dep_count: 0,
            dev_dep_count: 0,
        };
        let lines = format_metadata_lines(&project);
        assert_eq!(
            lines.len(),
            1,
            "expected only Languages (no Deps), got: {lines:?}"
        );
        let joined = lines.join("\n");
        assert!(
            !joined.contains("Deps:"),
            "Deps: should not appear when counts are zero, got: {joined}"
        );
    }

    #[test]
    fn format_metadata_plain_shows_deps_when_nonzero() {
        let project = Project {
            path: ".".to_string(),
            name: "test".to_string(),
            languages: vec![LanguageStat {
                language: Language::Rust,
                loc: 0,
            }],
            frameworks: vec![],
            package_manager: None,
            description: None,
            dep_count: 5,
            dev_dep_count: 3,
        };
        let lines = format_metadata_lines_plain(&project);
        assert_eq!(lines.len(), 2, "expected Languages + Deps, got: {lines:?}");
        let deps_line = &lines[1];
        assert!(
            deps_line.contains("Deps:")
                && deps_line.contains("5 prod")
                && deps_line.contains("3 dev"),
            "expected Deps: line with counts, got: {deps_line}"
        );
    }

    #[test]
    fn format_metadata_plain_hides_deps_when_zero() {
        let project = Project {
            path: ".".to_string(),
            name: "test".to_string(),
            languages: vec![LanguageStat {
                language: Language::Rust,
                loc: 0,
            }],
            frameworks: vec![],
            package_manager: None,
            description: None,
            dep_count: 0,
            dev_dep_count: 0,
        };
        let lines = format_metadata_lines_plain(&project);
        assert_eq!(
            lines.len(),
            1,
            "expected only Languages (no Deps), got: {lines:?}"
        );
        let joined = lines.join("\n");
        assert!(
            !joined.contains("Deps:"),
            "Deps: should not appear when counts are zero, got: {joined}"
        );
    }

    #[test]
    fn format_metadata_plain_deps_right_aligned() {
        let project = Project {
            path: ".".to_string(),
            name: "test".to_string(),
            languages: vec![LanguageStat {
                language: Language::Rust,
                loc: 0,
            }],
            frameworks: vec![Framework {
                name: "actix-web".to_string(),
                category: FrameworkCategory::WebBackend,
            }],
            package_manager: Some("cargo".to_string()),
            description: None,
            dep_count: 10,
            dev_dep_count: 2,
        };
        let lines = format_metadata_lines_plain(&project);
        // With deps, we should have Languages, Packages, Manager, Deps = 4 lines
        assert_eq!(lines.len(), 4, "expected 4 lines, got: {lines:?}");
        // Verify all colons align
        let colon_positions: Vec<usize> = lines.iter().map(|l| l.find(':').unwrap()).collect();
        let first = colon_positions[0];
        for (i, pos) in colon_positions.iter().enumerate() {
            assert_eq!(
                *pos, first,
                "colon on line {i} at column {pos}, expected {first}"
            );
        }
    }

    // ── format_number tests ──

    #[test]
    fn format_number_zero() {
        assert_eq!(format_number(0), "0");
    }

    #[test]
    fn format_number_small() {
        assert_eq!(format_number(999), "999");
    }

    #[test]
    fn format_number_thousand() {
        assert_eq!(format_number(1000), "1,000");
    }

    #[test]
    fn format_number_large() {
        assert_eq!(format_number(1234567), "1,234,567");
    }

    #[test]
    fn format_number_exact_thousands() {
        assert_eq!(format_number(1000000), "1,000,000");
    }

    // ── format_language_stat tests ──

    #[test]
    fn format_language_stat_with_loc() {
        let ls = LanguageStat {
            language: Language::TypeScript,
            loc: 2340,
        };
        assert_eq!(format_language_stat(&ls), "ts (2,340 loc)");
    }

    #[test]
    fn format_language_stat_zero_loc() {
        let ls = LanguageStat {
            language: Language::Rust,
            loc: 0,
        };
        assert_eq!(format_language_stat(&ls), "rust");
    }

    // ── LOC display in plain formatter ──

    #[test]
    fn format_summary_plain_shows_loc() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![Project {
                path: ".".to_string(),
                name: "my-app".to_string(),
                languages: vec![
                    LanguageStat {
                        language: Language::TypeScript,
                        loc: 2340,
                    },
                    LanguageStat {
                        language: Language::JavaScript,
                        loc: 180,
                    },
                ],
                frameworks: vec![],
                package_manager: None,
                description: None,
                dep_count: 0,
                dev_dep_count: 0,
            }],
        };
        let output = format_project_summary_plain(&analysis);
        assert!(
            output.contains("ts (2,340 loc)"),
            "expected LOC for TypeScript in: {output}"
        );
        assert!(
            output.contains("js (180 loc)"),
            "expected LOC for JavaScript in: {output}"
        );
    }

    #[test]
    fn format_summary_plain_hides_zero_loc() {
        let analysis = RepoAnalysis {
            is_monorepo: false,
            workspace_type: None,
            projects: vec![Project {
                path: ".".to_string(),
                name: "my-app".to_string(),
                languages: vec![LanguageStat {
                    language: Language::Rust,
                    loc: 0,
                }],
                frameworks: vec![],
                package_manager: None,
                description: None,
                dep_count: 0,
                dev_dep_count: 0,
            }],
        };
        let output = format_project_summary_plain(&analysis);
        assert!(
            output.contains("rust") && !output.contains("(0 loc)"),
            "zero LOC should not show parenthetical in: {output}"
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
