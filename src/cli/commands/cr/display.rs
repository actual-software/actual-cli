//! Output rendering for `actual cr` — signal matrix, verbose findings, and JSON.

use crate::cli::ui::panel::Panel;
use crate::cli::ui::term_size::terminal_width;

use super::types::{AggregatedResult, Finding, LensReview, Signal};

/// Maximum panel width for CR output.
const MAX_PANEL_WIDTH: usize = 90;

/// Render the signal matrix panel for terminal output.
pub fn render_signal_matrix(result: &AggregatedResult) -> String {
    let width = terminal_width().min(MAX_PANEL_WIDTH);

    let mut panel = Panel::titled("Code Review");

    // Header: branch info
    panel = panel.kv("Base", &result.metadata.base_branch);
    panel = panel.kv(
        "Head",
        &result.metadata.head_sha[..8.min(result.metadata.head_sha.len())],
    );
    panel = panel.separator();

    // Signal matrix — one row per lens
    for review in &result.per_lens {
        panel = panel.kv_annotated(
            review.lens.display_name(),
            review.signal.icon(),
            &review.rationale,
        );
    }

    // Overall signal + decision
    panel = panel.separator();
    panel = panel.kv(
        "Overall",
        &format!(
            "{} — {}",
            result.overall_signal.display(),
            result.decision.display()
        ),
    );

    // Cross-lens synthesis summary
    if let Some(synthesis) = &result.cross_lens_synthesis {
        panel = panel.separator();
        if !synthesis.convergent.is_empty() {
            panel = panel.line(&format!("Convergent: {}", synthesis.convergent[0].summary));
        }
        if !synthesis.divergent.is_empty() {
            panel = panel.line(&format!("Divergent: {}", synthesis.divergent[0].summary));
        }
        if !synthesis.surprising.is_empty() {
            panel = panel.line(&format!("Surprising: {}", synthesis.surprising[0].summary));
        }
    }

    panel.render(width)
}

/// Render verbose findings — one panel per Yellow/Red finding.
pub fn render_verbose_findings(result: &AggregatedResult) -> String {
    let width = terminal_width().min(MAX_PANEL_WIDTH);
    let mut out = String::new();

    for review in &result.per_lens {
        for finding in &review.findings {
            if finding.severity == Signal::Green || finding.severity == Signal::Gray {
                continue;
            }
            out.push('\n');
            out.push_str(&render_finding_panel(review, finding, width));
        }
    }

    out
}

/// Render a single finding as a titled panel.
fn render_finding_panel(review: &LensReview, finding: &Finding, width: usize) -> String {
    let title = format!(
        "{} — {}",
        review.lens.display_name(),
        review.signal.display(),
    );
    let mut panel = Panel::titled(&title);

    panel = panel.kv("Finding", &finding.summary);
    panel = panel.kv("Severity", finding.severity.display());

    if !finding.affected_files.is_empty() {
        panel = panel.kv("Files", &finding.affected_files.join(", "));
    }
    if !finding.related_adrs.is_empty() {
        panel = panel.kv("Related ADRs", &finding.related_adrs.join(", "));
    }

    panel = panel.separator();
    panel = panel.line("Evidence:");
    panel = panel.line(&finding.details);

    panel = panel.separator();
    panel = panel.line("Suggestion:");
    panel = panel.line(&finding.suggestion);

    panel.render(width)
}

/// Render the full result as JSON to stdout.
pub fn render_json(result: &AggregatedResult) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(result)
}

/// Print the CR output to stdout/stderr based on args.
pub fn print_review(result: &AggregatedResult, json: bool, verbose: bool) {
    if json {
        match render_json(result) {
            Ok(json_str) => println!("{json_str}"),
            Err(e) => eprintln!("Error serializing review result: {e}"),
        }
    } else {
        eprintln!("{}", render_signal_matrix(result));
        if verbose {
            eprintln!("{}", render_verbose_findings(result));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::commands::cr::types::*;

    fn make_test_result() -> AggregatedResult {
        AggregatedResult {
            per_lens: vec![
                LensReview {
                    lens: ReviewerLens::PrincipalEngineer,
                    signal: Signal::Green,
                    rationale: "No architectural concerns".to_string(),
                    findings: vec![],
                },
                LensReview {
                    lens: ReviewerLens::SecurityEngineer,
                    signal: Signal::Yellow,
                    rationale: "Missing input validation".to_string(),
                    findings: vec![Finding {
                        severity: Signal::Yellow,
                        related_adrs: vec!["adr-001".to_string()],
                        affected_files: vec!["src/api/login.rs".to_string()],
                        summary: "No input sanitization".to_string(),
                        details: "Username passed directly to DB query".to_string(),
                        suggestion: "Add input validation".to_string(),
                    }],
                },
            ],
            overall_signal: Signal::Yellow,
            decision: Decision::ProceedWithMitigation,
            cross_lens_synthesis: Some(CrossLensSynthesis {
                convergent: vec![SynthesisFinding {
                    lenses: vec![ReviewerLens::SecurityEngineer, ReviewerLens::TestEngineer],
                    summary: "Both flag missing validation".to_string(),
                    action: "Add input validation".to_string(),
                }],
                divergent: vec![],
                surprising: vec![],
                most_relevant: vec![],
            }),
            metadata: ReviewMetadata {
                timestamp_utc: "2026-03-09T00:00:00Z".to_string(),
                head_sha: "abc12345def".to_string(),
                base_branch: "main".to_string(),
                model: "sonnet".to_string(),
                runner: "claude-cli".to_string(),
            },
        }
    }

    #[test]
    fn signal_matrix_contains_lens_names() {
        let result = make_test_result();
        let output = render_signal_matrix(&result);
        let plain = console::strip_ansi_codes(&output);
        assert!(plain.contains("Principal Engineer"));
        assert!(plain.contains("Security Engineer"));
    }

    #[test]
    fn signal_matrix_contains_overall() {
        let result = make_test_result();
        let output = render_signal_matrix(&result);
        let plain = console::strip_ansi_codes(&output);
        assert!(plain.contains("Yellow"));
        assert!(plain.contains("Proceed With Mitigation"));
    }

    #[test]
    fn signal_matrix_contains_synthesis() {
        let result = make_test_result();
        let output = render_signal_matrix(&result);
        let plain = console::strip_ansi_codes(&output);
        assert!(plain.contains("Convergent:"));
        assert!(plain.contains("Both flag missing validation"));
    }

    #[test]
    fn verbose_findings_renders_yellow_red_only() {
        let result = make_test_result();
        let output = render_verbose_findings(&result);
        let plain = console::strip_ansi_codes(&output);
        // The yellow finding should appear
        assert!(plain.contains("No input sanitization"));
        assert!(plain.contains("Evidence:"));
        assert!(plain.contains("Suggestion:"));
    }

    #[test]
    fn verbose_findings_empty_when_all_green() {
        let result = AggregatedResult {
            per_lens: vec![LensReview {
                lens: ReviewerLens::PrincipalEngineer,
                signal: Signal::Green,
                rationale: "OK".to_string(),
                findings: vec![],
            }],
            overall_signal: Signal::Green,
            decision: Decision::Proceed,
            cross_lens_synthesis: None,
            metadata: ReviewMetadata {
                timestamp_utc: "2026-03-09T00:00:00Z".to_string(),
                head_sha: "abc12345".to_string(),
                base_branch: "main".to_string(),
                model: "sonnet".to_string(),
                runner: "claude-cli".to_string(),
            },
        };
        let output = render_verbose_findings(&result);
        assert!(output.is_empty());
    }

    #[test]
    fn json_output_is_valid() {
        let result = make_test_result();
        let json_str = render_json(&result).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed["overall_signal"], "yellow");
        assert_eq!(parsed["decision"], "proceed_with_mitigation");
    }

    #[test]
    fn finding_panel_includes_files_and_adrs() {
        let review = LensReview {
            lens: ReviewerLens::SecurityEngineer,
            signal: Signal::Yellow,
            rationale: "test".to_string(),
            findings: vec![],
        };
        let finding = Finding {
            severity: Signal::Yellow,
            related_adrs: vec!["adr-001".to_string()],
            affected_files: vec!["src/lib.rs".to_string()],
            summary: "test finding".to_string(),
            details: "evidence here".to_string(),
            suggestion: "fix it".to_string(),
        };
        let output = render_finding_panel(&review, &finding, 80);
        let plain = console::strip_ansi_codes(&output);
        assert!(plain.contains("Files: src/lib.rs"));
        assert!(plain.contains("Related ADRs: adr-001"));
    }

    #[test]
    fn signal_icon_values() {
        assert_eq!(Signal::Red.icon(), "RED");
        assert_eq!(Signal::Yellow.icon(), "YLW");
        assert_eq!(Signal::Green.icon(), "GRN");
        assert_eq!(Signal::Gray.icon(), "N/A");
    }

    #[test]
    fn decision_display_values() {
        assert_eq!(Decision::Proceed.display(), "Proceed");
        assert_eq!(
            Decision::ProceedWithMitigation.display(),
            "Proceed With Mitigation"
        );
        assert_eq!(Decision::BlockOrEscalate.display(), "Block or Escalate");
    }

    #[test]
    fn decision_from_signal() {
        assert_eq!(Decision::from_signal(Signal::Green), Decision::Proceed);
        assert_eq!(Decision::from_signal(Signal::Gray), Decision::Proceed);
        assert_eq!(
            Decision::from_signal(Signal::Yellow),
            Decision::ProceedWithMitigation
        );
        assert_eq!(
            Decision::from_signal(Signal::Red),
            Decision::BlockOrEscalate
        );
    }

    #[test]
    fn reviewer_lens_display_names() {
        assert_eq!(
            ReviewerLens::PrincipalEngineer.display_name(),
            "Principal Engineer"
        );
        assert_eq!(ReviewerLens::SreEngineer.display_name(), "SRE Engineer");
        assert_eq!(ReviewerLens::OssMaintainer.display_name(), "OSS Maintainer");
    }

    #[test]
    fn reviewer_lens_all_returns_eight() {
        assert_eq!(ReviewerLens::all().len(), 8);
    }
}
