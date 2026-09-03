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

use std::path::Path;

use serde::Serialize;

use crate::cli::args::{RulesAction, RulesArgs, RulesLsArgs};
use crate::cli::ui::panel::Panel;
use crate::cli::ui::term_size;
use crate::error::ActualError;
use crate::rules::{
    load_rule_set, Rule, RuleDocument, RuleIssue, RuleLevel, RuleSetLoadReport, VerifyBlock,
};

pub fn exec(args: &RulesArgs) -> Result<(), ActualError> {
    use crate::cli::commands::rules_scope;
    match &args.action {
        RulesAction::Ls(ls) => exec_ls(ls),
        RulesAction::Index(index) => rules_scope::exec_index(index),
        RulesAction::Select(select) => rules_scope::exec_select(select),
        RulesAction::Eval(eval) => rules_scope::exec_eval(eval),
    }
}

fn exec_ls(args: &RulesLsArgs) -> Result<(), ActualError> {
    let root = args
        .path
        .clone()
        .unwrap_or_else(crate::cli::commands::sync::resolve_cwd);
    let report = load_rule_set(&root)?;

    if args.json {
        println!("{}", render_json(&report, &root));
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

/// `1 document` / `2 documents`. Zero uses the plural, like English counts.
fn counted(n: usize, singular: &str) -> String {
    if n == 1 {
        format!("1 {singular}")
    } else {
        format!("{n} {singular}s")
    }
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

    if report.warning_count() > 0 {
        panel = panel.separator().line("Warnings:");
        for doc in &report.documents {
            let name = doc.slug().unwrap_or("<unnamed>");
            for warning in &doc.warnings {
                panel = panel.line(&format!("  {name}: {warning}"));
            }
        }
    }

    if !report.errors.is_empty() {
        panel = panel.separator().line("Failed to parse:");
        for error in &report.errors {
            panel = panel.line(&format!("  {error}"));
        }
    }

    panel = panel.separator().line(&format!(
        "{} · {} · {} · {}",
        counted(report.documents.len(), "document"),
        counted(report.rule_count(), "rule"),
        counted(report.warning_count(), "warning"),
        counted(report.errors.len(), "error"),
    ));
    panel.render(width)
}

/// Serializable view of a load report.
///
/// Neither [`RuleSetLoadReport`] nor [`RuleDocument`] is serialized straight to
/// the wire. Both carry the absolute on-disk path a document was read from, so
/// serializing them directly makes `--json` differ between machines for an
/// identical rule set. The shape is declared here instead, where it is a
/// user-facing contract rather than an accident of the data model.
///
/// `rules_dir` is the one deliberately machine-specific field: it records which
/// checkout was scanned, and is the anchor every relative path below resolves
/// against. Dropping it would make the relative paths uninterpretable.
#[derive(Serialize)]
struct JsonReport<'a> {
    rules_dir: String,
    summary: JsonSummary,
    documents: Vec<JsonDocument<'a>>,
    errors: Vec<JsonError<'a>>,
}

#[derive(Serialize)]
struct JsonSummary {
    documents: usize,
    rules: usize,
    warnings: usize,
    errors: usize,
}

/// One parsed document.
///
/// `path` is relative to the scanned root, so an identical rule set serializes
/// identically wherever it is checked out. `slug` is the stable identity the
/// scope index keys on, which the document type computes rather than storing
/// as a field of its own.
#[derive(Serialize)]
struct JsonDocument<'a> {
    path: String,
    slug: Option<&'a str>,
    title: Option<&'a str>,
    scope: Option<&'a str>,
    rules: &'a [Rule],
    verify: &'a [VerifyBlock],
    accept_when: &'a [String],
    enforcement: Option<&'a str>,
    warnings: &'a [RuleIssue],
}

#[derive(Serialize)]
struct JsonError<'a> {
    path: String,
    issue: &'a RuleIssue,
}

/// `path` expressed relative to `root`.
///
/// Falls back to the full path when `path` lies outside the scanned root. That
/// cannot happen for a document discovered under `root`, but an unstable path
/// is a better outcome than a panic if it ever does.
fn relative_to(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn render_json(report: &RuleSetLoadReport, root: &Path) -> String {
    let payload = JsonReport {
        rules_dir: report.rules_dir.display().to_string(),
        summary: JsonSummary {
            documents: report.documents.len(),
            rules: report.rule_count(),
            warnings: report.warning_count(),
            errors: report.errors.len(),
        },
        documents: report
            .documents
            .iter()
            .map(|doc| JsonDocument {
                path: relative_to(root, &doc.source_path),
                slug: doc.slug(),
                title: doc.title.as_deref(),
                scope: doc.scope.as_deref(),
                rules: &doc.rules,
                verify: &doc.verify,
                accept_when: &doc.accept_when,
                enforcement: doc.enforcement.as_deref(),
                warnings: &doc.warnings,
            })
            .collect(),
        errors: report
            .errors
            .iter()
            .map(|error| JsonError {
                path: relative_to(root, &error.path),
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
    fn test_counted_singular_only_for_one() {
        assert_eq!(counted(0, "document"), "0 documents");
        assert_eq!(counted(1, "document"), "1 document");
        assert_eq!(counted(2, "warning"), "2 warnings");
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
        assert!(out.contains("1 document · 2 rules · 1 warning · 0 errors"));
    }

    /// Dropped bullets (unknown levels, malformed rules) are listed by slug and
    /// rule id, not only counted in the summary.
    #[test]
    fn test_render_panel_lists_warnings() {
        let root = seed(&[("a.md", DOC)]);
        let report = load_rule_set(root.path()).unwrap();
        let out = render_panel(&report, 100);

        assert!(out.contains("Warnings:"));
        assert!(out.contains("a: line 9:"));
        assert!(out.contains("R-A-003"));
        assert!(out.contains("MUST 1 · MAY 1"));
    }

    #[test]
    fn test_render_panel_lists_failures() {
        let root = seed(&[("a.md", DOC), ("b.md", BAD)]);
        let report = load_rule_set(root.path()).unwrap();
        let out = render_panel(&report, 120);

        assert!(out.contains("Failed to parse:"));
        assert!(out.contains("no `### Rules` section"));
        assert!(out.contains("1 document · 2 rules · 1 warning · 1 error"));
        let warnings_at = out.find("Warnings:").expect("warnings section");
        let errors_at = out.find("Failed to parse:").expect("errors section");
        assert!(
            warnings_at < errors_at,
            "warnings should be listed before parse failures"
        );
    }

    #[test]
    fn test_render_panel_for_an_empty_rule_set() {
        let root = tempdir().unwrap();
        let report = load_rule_set(root.path()).unwrap();
        let out = render_panel(&report, 80);
        assert!(out.contains("No rule documents found."));
        assert!(!out.contains("Warnings:"));
    }

    /// A report holding only failures still renders the failure list, without a
    /// document section.
    #[test]
    fn test_render_panel_with_errors_only() {
        let root = seed(&[("b.md", BAD)]);
        let report = load_rule_set(root.path()).unwrap();
        let out = render_panel(&report, 120);
        assert!(out.contains("Failed to parse:"));
        assert!(out.contains("0 documents · 0 rules · 0 warnings · 1 error"));
        assert!(!out.contains("Warnings:"));
    }

    #[test]
    fn test_render_json_carries_summary_documents_and_errors() {
        let root = seed(&[("a.md", DOC), ("b.md", BAD)]);
        let report = load_rule_set(root.path()).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&render_json(&report, root.path())).unwrap();

        assert_eq!(value["summary"]["documents"], 1);
        assert_eq!(value["summary"]["rules"], 2);
        assert_eq!(value["summary"]["warnings"], 1);
        assert_eq!(value["summary"]["errors"], 1);
        assert_eq!(value["documents"][0]["title"], "Alpha");
        assert_eq!(value["documents"][0]["scope"], "Scope.");
        assert_eq!(value["documents"][0]["rules"][0]["id"], "R-A-001");
        assert_eq!(value["documents"][0]["rules"][0]["level"], "MUST");
        assert_eq!(
            value["documents"][0]["warnings"][0]["kind"],
            "unknown_level"
        );
        assert_eq!(value["errors"][0]["issue"]["kind"], "missing_rules_section");
        assert!(value["rules_dir"]
            .as_str()
            .unwrap()
            .ends_with(".actual/rules"));
    }

    /// The one field that names this machine is `rules_dir`. Every per-file
    /// path is relative to the scanned root, so the same rule set serializes
    /// byte-identically wherever it is checked out.
    #[test]
    fn test_render_json_paths_are_relative_to_the_scanned_root() {
        let root = seed(&[("a.md", DOC), ("b.md", BAD)]);
        let report = load_rule_set(root.path()).unwrap();
        let json = render_json(&report, root.path());
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(value["documents"][0]["path"], ".actual/rules/a.md");
        assert_eq!(value["documents"][0]["slug"], "a");
        assert_eq!(value["errors"][0]["path"], ".actual/rules/b.md");

        // The scanned root is the only place the temp directory may appear.
        let root_display = root.path().display().to_string();
        let occurrences = json.matches(root_display.as_str()).count();
        assert_eq!(occurrences, 1, "temp root leaked outside rules_dir: {json}");
    }

    /// A path outside the scanned root cannot arise from discovery, but it must
    /// degrade to the full path rather than panicking.
    #[test]
    fn test_relative_to_falls_back_to_the_full_path() {
        assert_eq!(
            relative_to(Path::new("/repo"), Path::new("/repo/.actual/rules/a.md")),
            ".actual/rules/a.md"
        );
        assert_eq!(
            relative_to(Path::new("/repo"), Path::new("/elsewhere/a.md")),
            "/elsewhere/a.md"
        );
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
