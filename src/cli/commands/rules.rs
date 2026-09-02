//! `actual rules` — inspect the rule documents under `.actual/rules/`.
//!
//! # Design
//!
//! This is a reporting surface, not a gate: `rules ls` exits 0 even when some
//! files failed to parse, and prints those failures. Deciding that a
//! governance run should *fail* on an unparseable rule file is a separate
//! decision, made where governance is enforced rather than where it is listed.
//!
//! Rendering is split into pure functions taking the report and a width, the
//! way [`crate::cli::commands::runners`] does, so the output can be asserted on
//! without a terminal.

use serde::Serialize;

use crate::cli::args::{RulesAction, RulesArgs, RulesLsArgs};
use crate::cli::ui::panel::Panel;
use crate::cli::ui::term_size;
use crate::error::ActualError;
use crate::rules::{load_rule_set, RuleDocument, RuleIssue, RuleLevel, RuleSetLoadReport};

pub fn exec(args: &RulesArgs) -> Result<(), ActualError> {
    match &args.action {
        RulesAction::Ls(ls) => exec_ls(ls),
    }
}

fn exec_ls(args: &RulesLsArgs) -> Result<(), ActualError> {
    let root = args
        .path
        .clone()
        .unwrap_or_else(crate::cli::commands::sync::resolve_cwd);
    let report = load_rule_set(&root)?;

    if args.json {
        println!("{}", render_json(&report));
    } else {
        println!("{}", render_panel(&report, term_size::terminal_width()));
    }
    Ok(())
}

/// `MUST 7 · SHOULD 1 · MAY 1`, listing only the levels the document uses so a
/// mostly-MUST corpus does not print four zeroes on every row.
fn level_histogram(doc: &RuleDocument) -> String {
    const ORDER: &[RuleLevel] = &[
        RuleLevel::Must,
        RuleLevel::MustNot,
        RuleLevel::Should,
        RuleLevel::ShouldNot,
        RuleLevel::May,
    ];
    ORDER
        .iter()
        .filter_map(|level| {
            let count = doc.rules.iter().filter(|rule| rule.level == *level).count();
            (count > 0).then(|| format!("{} {count}", level.as_str()))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn render_panel(report: &RuleSetLoadReport, width: usize) -> String {
    let mut panel = Panel::titled("Rules");
    panel = panel.line(&report.rules_dir.display().to_string());

    if report.is_empty() {
        panel = panel.separator().line("No rule documents found.");
        return panel.render(width);
    }

    if !report.documents.is_empty() {
        panel = panel.separator();
        for doc in &report.documents {
            let name = doc.slug().unwrap_or("<unnamed>");
            panel = panel.kv(name, &level_histogram(doc));
        }
    }

    if !report.errors.is_empty() {
        panel = panel.separator().line("Failed to parse:");
        for error in &report.errors {
            panel = panel.line(&format!("  {error}"));
        }
    }

    panel = panel.separator().line(&format!(
        "{} documents · {} rules · {} warnings · {} errors",
        report.documents.len(),
        report.rule_count(),
        report.warning_count(),
        report.errors.len(),
    ));
    panel.render(width)
}

/// Serializable view of a load report.
///
/// [`RuleSetLoadReport`] itself is deliberately not `Serialize` — it carries an
/// absolute machine path — so the JSON shape is declared here, where it is a
/// user-facing contract rather than an accident of the data model.
#[derive(Serialize)]
struct JsonReport<'a> {
    rules_dir: String,
    summary: JsonSummary,
    documents: &'a [RuleDocument],
    errors: Vec<JsonError<'a>>,
}

#[derive(Serialize)]
struct JsonSummary {
    documents: usize,
    rules: usize,
    warnings: usize,
    errors: usize,
}

#[derive(Serialize)]
struct JsonError<'a> {
    path: String,
    issue: &'a RuleIssue,
}

fn render_json(report: &RuleSetLoadReport) -> String {
    let payload = JsonReport {
        rules_dir: report.rules_dir.display().to_string(),
        summary: JsonSummary {
            documents: report.documents.len(),
            rules: report.rule_count(),
            warnings: report.warning_count(),
            errors: report.errors.len(),
        },
        documents: &report.documents,
        errors: report
            .errors
            .iter()
            .map(|error| JsonError {
                path: error.path.display().to_string(),
                issue: &error.issue,
            })
            .collect(),
    };
    serde_json::to_string_pretty(&payload)
        .expect("rule report is serializable — this is a programmer error")
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;

    use tempfile::{tempdir, TempDir};

    const DOC: &str = "# Alpha\n\nScope.\n\n### Rules\n\n- **R-A-001** MUST: a\n- **R-A-002** MAY: b\n- **R-A-003** EXCEPTION: c\n";
    const BAD: &str = "no rules section here\n";

    /// Helper: a repository root whose `.actual/rules/` holds `files`.
    fn seed(files: &[(&str, &str)]) -> TempDir {
        let root = tempdir().unwrap();
        let dir = crate::rules::rules_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        for (name, contents) in files {
            std::fs::write(dir.join(name), contents).unwrap();
        }
        root
    }

    /// Helper: `rules ls` arguments for `root`.
    fn ls_args(root: &Path, json: bool) -> RulesArgs {
        RulesArgs {
            action: RulesAction::Ls(RulesLsArgs {
                path: Some(root.to_path_buf()),
                json,
            }),
        }
    }

    #[test]
    fn test_level_histogram_lists_only_used_levels() {
        let root = seed(&[("a.md", DOC)]);
        let report = load_rule_set(root.path()).unwrap();
        assert_eq!(level_histogram(&report.documents[0]), "MUST 1 · MAY 1");
    }

    #[test]
    fn test_render_panel_lists_documents_and_a_summary() {
        let root = seed(&[("a.md", DOC)]);
        let report = load_rule_set(root.path()).unwrap();
        let out = render_panel(&report, 100);

        assert!(out.contains("Rules"));
        assert!(out.contains(".actual/rules"));
        assert!(out.contains("MUST 1 · MAY 1"));
        assert!(out.contains("1 documents · 2 rules · 1 warnings · 0 errors"));
    }

    #[test]
    fn test_render_panel_lists_failures() {
        let root = seed(&[("a.md", DOC), ("b.md", BAD)]);
        let report = load_rule_set(root.path()).unwrap();
        let out = render_panel(&report, 120);

        assert!(out.contains("Failed to parse:"));
        assert!(out.contains("no `### Rules` section"));
        assert!(out.contains("1 documents · 2 rules · 1 warnings · 1 errors"));
    }

    #[test]
    fn test_render_panel_for_an_empty_rule_set() {
        let root = tempdir().unwrap();
        let report = load_rule_set(root.path()).unwrap();
        let out = render_panel(&report, 80);
        assert!(out.contains("No rule documents found."));
    }

    /// A report holding only failures still renders the failure list, without a
    /// document section.
    #[test]
    fn test_render_panel_with_errors_only() {
        let root = seed(&[("b.md", BAD)]);
        let report = load_rule_set(root.path()).unwrap();
        let out = render_panel(&report, 120);
        assert!(out.contains("Failed to parse:"));
        assert!(out.contains("0 documents · 0 rules · 0 warnings · 1 errors"));
    }

    #[test]
    fn test_render_json_carries_summary_documents_and_errors() {
        let root = seed(&[("a.md", DOC), ("b.md", BAD)]);
        let report = load_rule_set(root.path()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&render_json(&report)).unwrap();

        assert_eq!(value["summary"]["documents"], 1);
        assert_eq!(value["summary"]["rules"], 2);
        assert_eq!(value["summary"]["warnings"], 1);
        assert_eq!(value["summary"]["errors"], 1);
        assert_eq!(value["documents"][0]["rules"][0]["id"], "R-A-001");
        assert_eq!(value["documents"][0]["rules"][0]["level"], "MUST");
        assert_eq!(value["errors"][0]["issue"]["kind"], "missing_rules_section");
        assert!(value["rules_dir"]
            .as_str()
            .unwrap()
            .ends_with(".actual/rules"));
    }

    #[test]
    fn test_exec_ls_renders_a_panel() {
        let root = seed(&[("a.md", DOC)]);
        assert!(exec(&ls_args(root.path(), false)).is_ok());
    }

    #[test]
    fn test_exec_ls_renders_json() {
        let root = seed(&[("a.md", DOC)]);
        assert!(exec(&ls_args(root.path(), true)).is_ok());
    }

    #[test]
    fn test_exec_ls_propagates_a_load_failure() {
        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".actual")).unwrap();
        std::fs::write(crate::rules::rules_dir(root.path()), "not a directory").unwrap();

        let err = exec(&ls_args(root.path(), false)).unwrap_err();
        assert!(matches!(err, ActualError::IoError(_)));
    }
}
