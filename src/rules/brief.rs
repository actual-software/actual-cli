//! Rendering a selection as a brief an agent reads before it edits.
//!
//! # Design
//!
//! A brief is spent context: every line competes with the work the agent is
//! actually doing, which is the inflation this epic exists to avoid. So it
//! carries obligations and nothing else — one heading per decision, then its
//! MUST rules. `SHOULD` and `MAY` are left out deliberately: they are advice,
//! and advice is what a capable agent already has.
//!
//! Two properties of a generated rule set shape the format.
//!
//! **Sibling documents repeat themselves.** Documents of one decision overlap
//! heavily, so identical statements are stated once, under the decision that
//! owns them.
//!
//! **Rule ids are not unique.** On the reference corpus 314 of 694 ids appear
//! in more than one document — `R-CACHE-001` means six different things — so
//! every line is keyed by document *and* id. An id alone would name six rules
//! at once, which is worse than useless in a brief the agent may quote back.
//!
//! Truncation is disclosed rather than silent: a brief showing four of eleven
//! rules says so, because an agent told nothing about the remainder would
//! reasonably read the four as the whole obligation.
//!
//! This is the minimal renderer the hook needs; the caps and wording are
//! AK-790's subject.

use crate::rules::types::{RuleDocument, RuleLevel};

/// One decision's documents, in the order the index ranked them.
pub struct BriefDecision<'a> {
    /// The decision's name, or the slug of a document that names none.
    pub heading: &'a str,
    pub documents: Vec<&'a RuleDocument>,
}

/// Render decisions as a brief, or `None` when there is nothing to say: no
/// decisions, or none of them stating an obligation.
pub fn render_brief(decisions: &[BriefDecision<'_>], rules_per_decision: usize) -> Option<String> {
    let blocks: Vec<String> = decisions
        .iter()
        .filter_map(|decision| render_decision(decision, rules_per_decision))
        .collect();

    (!blocks.is_empty()).then(|| blocks.join("\n\n"))
}

/// `RuleLevel::as_str` is the serialized form (`MUST_NOT`); a brief is prose
/// the model reads, so it gets the written form.
fn level_word(level: RuleLevel) -> &'static str {
    match level {
        RuleLevel::MustNot => "MUST NOT",
        _ => "MUST",
    }
}

fn render_decision(decision: &BriefDecision<'_>, rules_per_decision: usize) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    let mut available = 0usize;

    for document in &decision.documents {
        let slug = document.slug().unwrap_or("<unnamed>");
        for rule in &document.rules {
            if !matches!(rule.level, RuleLevel::Must | RuleLevel::MustNot) {
                continue;
            }
            if seen.contains(&rule.statement.as_str()) {
                continue;
            }
            seen.push(&rule.statement);
            available += 1;
            if lines.len() < rules_per_decision {
                lines.push(format!(
                    "- [{slug}/{}] {} {}",
                    rule.id,
                    level_word(rule.level),
                    rule.statement
                ));
            }
        }
    }

    if lines.is_empty() {
        return None;
    }
    let mut block = format!("## {}\n{}", decision.heading, lines.join("\n"));
    if available > lines.len() {
        block.push_str(&format!("\n- ({} of {available} rules shown)", lines.len()));
    }
    Some(block)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use crate::rules::parse_rule_document;

    fn doc(slug: &str, title: &str, rules: &str) -> RuleDocument {
        let text = format!(
            "# {title}\n\nThese rules are ALWAYS ACTIVE everywhere.\n\n### Rules\n\n{rules}\n"
        );
        parse_rule_document(
            &PathBuf::from(format!("/repo/.actual/rules/{slug}.md")),
            &text,
        )
        .expect("fixture parses")
    }

    fn decision<'a>(heading: &'a str, documents: Vec<&'a RuleDocument>) -> BriefDecision<'a> {
        BriefDecision { heading, documents }
    }

    #[test]
    fn test_brief_carries_obligations_under_the_decision() {
        let a = doc(
            "cross-cutting-signing-e410",
            "Adopt RS256: Token Signing",
            "- **R-A-001** MUST: sign with RS256.\n- **R-A-002** MUST NOT: sign with HS256.",
        );
        let brief = render_brief(&[decision("Adopt RS256", vec![&a])], 8).expect("a brief");

        assert!(brief.starts_with("## Adopt RS256\n"), "{brief}");
        assert!(brief.contains("- [cross-cutting-signing-e410/R-A-001] MUST sign with RS256."));
        assert!(brief.contains("[cross-cutting-signing-e410/R-A-002] MUST NOT sign with HS256."));
    }

    /// Advice is left out: a brief states obligations only.
    #[test]
    fn test_brief_leaves_out_should_and_may() {
        let a = doc(
            "cross-cutting-signing-e410",
            "Adopt RS256: Token Signing",
            "- **R-A-001** MUST: sign with RS256.\n- **R-A-002** SHOULD: rotate quarterly.\n- **R-A-003** MAY: cache the key.",
        );
        let brief = render_brief(&[decision("Adopt RS256", vec![&a])], 8).expect("a brief");

        assert!(brief.contains("R-A-001"));
        assert!(!brief.contains("R-A-002"), "{brief}");
        assert!(!brief.contains("R-A-003"), "{brief}");
    }

    /// Siblings restate the same obligation; the agent needs it once. The
    /// line keeps the document that stated it first.
    #[test]
    fn test_identical_statements_across_siblings_are_stated_once() {
        let a = doc(
            "cross-cutting-signing-e410",
            "Adopt RS256: Token Signing",
            "- **R-A-001** MUST: sign with RS256.",
        );
        let b = doc(
            "cross-cutting-verification-a1b2",
            "Adopt RS256: Token Verification",
            "- **R-B-001** MUST: sign with RS256.\n- **R-B-002** MUST: check revocation.",
        );
        let brief = render_brief(&[decision("Adopt RS256", vec![&a, &b])], 8).expect("a brief");

        assert_eq!(brief.matches("sign with RS256.").count(), 1, "{brief}");
        assert!(
            brief.contains("cross-cutting-signing-e410/R-A-001"),
            "{brief}"
        );
        assert!(
            brief.contains("cross-cutting-verification-a1b2/R-B-002"),
            "{brief}"
        );
    }

    /// Rule ids repeat across documents, so a line names both. Two documents
    /// carrying `R-A-001` for different rules must stay distinguishable.
    #[test]
    fn test_lines_are_keyed_by_document_and_rule_id() {
        let a = doc(
            "cross-cutting-signing-e410",
            "Adopt RS256: Token Signing",
            "- **R-A-001** MUST: sign with RS256.",
        );
        let b = doc(
            "cross-cutting-caching-a1b2",
            "Adopt RS256: Key Caching",
            "- **R-A-001** MUST: cache public keys for an hour.",
        );
        let brief = render_brief(&[decision("Adopt RS256", vec![&a, &b])], 8).expect("a brief");

        assert!(
            brief.contains("[cross-cutting-signing-e410/R-A-001]"),
            "{brief}"
        );
        assert!(
            brief.contains("[cross-cutting-caching-a1b2/R-A-001]"),
            "{brief}"
        );
    }

    /// A truncated brief says what it left out.
    #[test]
    fn test_truncation_is_disclosed() {
        let a = doc(
            "cross-cutting-signing-e410",
            "Adopt RS256: Token Signing",
            "- **R-A-001** MUST: one.\n- **R-A-002** MUST: two.\n- **R-A-003** MUST: three.",
        );
        let brief = render_brief(&[decision("Adopt RS256", vec![&a])], 2).expect("a brief");

        assert!(brief.contains("R-A-001"));
        assert!(brief.contains("R-A-002"));
        assert!(!brief.contains("R-A-003"), "{brief}");
        assert!(brief.contains("(2 of 3 rules shown)"), "{brief}");
    }

    /// An untruncated brief does not carry the disclosure.
    #[test]
    fn test_untruncated_brief_says_nothing_about_counts() {
        let a = doc(
            "cross-cutting-signing-e410",
            "Adopt RS256: Token Signing",
            "- **R-A-001** MUST: one.",
        );
        let brief = render_brief(&[decision("Adopt RS256", vec![&a])], 8).expect("a brief");

        assert!(!brief.contains("rules shown"), "{brief}");
    }

    /// Several decisions are separated, each under its own heading.
    #[test]
    fn test_decisions_are_separate_blocks() {
        let a = doc(
            "cross-cutting-signing-e410",
            "Adopt RS256: Token Signing",
            "- **R-A-001** MUST: sign with RS256.",
        );
        let b = doc(
            "cross-cutting-pinning-c3d4",
            "Pin Providers: Terraform",
            "- **R-B-001** MUST: pin providers.",
        );
        let brief = render_brief(
            &[
                decision("Adopt RS256", vec![&a]),
                decision("Pin Providers", vec![&b]),
            ],
            8,
        )
        .expect("a brief");

        assert!(brief.contains("## Adopt RS256"));
        assert!(brief.contains("\n\n## Pin Providers"), "{brief}");
    }

    /// Nothing to say is `None`, not an empty heading: the hook turns this
    /// into silence.
    #[test]
    fn test_nothing_to_say_is_none() {
        assert_eq!(render_brief(&[], 8), None);

        let advice_only = doc(
            "cross-cutting-signing-e410",
            "Adopt RS256: Token Signing",
            "- **R-A-001** SHOULD: rotate quarterly.",
        );
        assert_eq!(
            render_brief(&[decision("Adopt RS256", vec![&advice_only])], 8),
            None
        );
    }

    /// A cap of zero is the same as nothing to say, rather than a heading
    /// with no rules under it.
    #[test]
    fn test_a_cap_of_zero_says_nothing() {
        let a = doc(
            "cross-cutting-signing-e410",
            "Adopt RS256: Token Signing",
            "- **R-A-001** MUST: sign with RS256.",
        );
        assert_eq!(render_brief(&[decision("Adopt RS256", vec![&a])], 0), None);
    }
}
