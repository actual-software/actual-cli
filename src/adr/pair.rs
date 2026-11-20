//! The text of a decision's two files, as `actual adr new` and
//! `actual adr normalize` write them: the decision doc (a header, the Context
//! File, then any sections carried over from the file it was normalized
//! from) and the rule file (the same header with its source record, then the
//! rules).

use crate::adr::context_file::{self, ContextFile};
use crate::adr::header::{description_for, render_header, AdrHeader, REPOSITORY_ROOT};
use crate::adr::provenance::{digest, Source};
use crate::adr::rule_body::{render_rule_body, rule_id_prefix, scope_line};
use crate::rules::types::RuleLevel;

/// A level as rule files spell it: `MUST NOT`, not `MUST_NOT`.
pub fn rule_spelling(level: RuleLevel) -> &'static str {
    match level {
        RuleLevel::Must => "MUST",
        RuleLevel::MustNot => "MUST NOT",
        RuleLevel::Should => "SHOULD",
        RuleLevel::ShouldNot => "SHOULD NOT",
        RuleLevel::May => "MAY",
    }
}

/// What both files say about a decision.
#[derive(Debug, Clone, Copy)]
pub struct Decision<'a> {
    /// The checked decision.
    pub doc: &'a ContextFile,
    pub adr_id: &'a str,
    /// The header status, such as `accepted` or `draft`.
    pub status: &'a str,
    /// Globs from the repository root; `!` excludes. Empty for the whole
    /// repository.
    pub scope: &'a [String],
}

impl Decision<'_> {
    fn header(&self, file_name: &str, source: Option<Source>) -> String {
        render_header(&AdrHeader {
            name: file_name.trim_end_matches(".md").to_string(),
            description: description_for(&self.doc.title),
            adr_id: self.adr_id.to_string(),
            status: self.status.to_string(),
            scope_root: REPOSITORY_ROOT.to_string(),
            scope_paths: self.scope.to_vec(),
            source,
        })
    }

    /// The [`digest`] a rule file records for this decision.
    pub fn digest(&self) -> String {
        digest(self.doc, self.status, REPOSITORY_ROOT, self.scope)
    }

    /// The decision doc named `file_name`: the header, the Context File, and
    /// each `(heading, body)` of `carried` as its own section after it.
    pub fn doc_text(&self, file_name: &str, carried: &[(String, String)]) -> String {
        let mut text = format!(
            "{}\n{}",
            self.header(file_name, None),
            context_file::render(self.doc)
        );
        for (heading, body) in carried {
            text.push_str(&format!("\n\n## {heading}"));
            if !body.is_empty() {
                text.push_str(&format!("\n\n{body}"));
            }
        }
        text.push('\n');
        text
    }

    /// The rule file named `file_name`, recording that it was built from the
    /// decision doc at `doc_path` (relative to the repository root).
    pub fn rule_text(&self, file_name: &str, doc_path: &str) -> String {
        let rules: Vec<(&str, &str)> = self
            .doc
            .policies
            .iter()
            .map(|policy| (rule_spelling(policy.level), policy.statement.as_str()))
            .collect();
        let body = render_rule_body(
            &self.doc.title,
            &scope_line(REPOSITORY_ROOT, self.scope),
            &rule_id_prefix(self.adr_id),
            &rules,
        );
        let source = Source::local(doc_path, self.digest());
        format!("{}\n{body}", self.header(file_name, Some(source)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adr::context_file::Policy;

    fn doc() -> ContextFile {
        ContextFile {
            title: "Use Postgres".to_string(),
            status: "accepted".to_string(),
            date: "2026-11-19".to_string(),
            context: vec!["One store.".to_string()],
            policies: vec![Policy {
                level: RuleLevel::MustNot,
                statement: "Open SQLite files.".to_string(),
            }],
            ..ContextFile::default()
        }
    }

    #[test]
    fn doc_text_carries_sections_after_the_context_file() {
        let doc = doc();
        let scope = vec!["src/**".to_string()];
        let decision = Decision {
            doc: &doc,
            adr_id: "0b6f2a1e-1111-2222-3333-444455556666",
            status: "accepted",
            scope: &scope,
        };
        let text = decision.doc_text(
            "0007-use-postgres.md",
            &[
                ("Considered Options".to_string(), "- MySQL".to_string()),
                ("Empty".to_string(), String::new()),
            ],
        );
        assert!(text.starts_with("---\nversion: 2\nname: 0007-use-postgres\n"));
        assert!(!text.contains("source:"));
        assert!(text.contains("\n# Use Postgres\n"));
        assert!(text.ends_with("## Considered Options\n\n- MySQL\n\n## Empty\n"));
    }

    #[test]
    fn rule_text_records_its_source() {
        let doc = doc();
        let decision = Decision {
            doc: &doc,
            adr_id: "0b6f2a1e-1111-2222-3333-444455556666",
            status: "draft",
            scope: &[],
        };
        let text = decision.rule_text("cross-cutting-use-postgres-0b6f.md", "docs/adr/a.md");
        assert!(text.contains("    source:\n      origin: local\n      path: docs/adr/a.md\n"));
        assert!(text.contains(&decision.digest()));
        assert!(text.contains("**R-0B6F-001** MUST NOT: Open SQLite files."));
    }

    #[test]
    fn rule_spelling_uses_spaces() {
        let spelled: Vec<&str> = [
            RuleLevel::Must,
            RuleLevel::MustNot,
            RuleLevel::Should,
            RuleLevel::ShouldNot,
            RuleLevel::May,
        ]
        .into_iter()
        .map(rule_spelling)
        .collect();
        assert_eq!(spelled, ["MUST", "MUST NOT", "SHOULD", "SHOULD NOT", "MAY"]);
    }
}
