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
//! The brief is bounded twice: rules per decision, and total characters.

use crate::rules::types::{RuleDocument, RuleLevel};

/// One decision's documents, in the order the index ranked them.
pub struct BriefDecision<'a> {
    /// The decision's name, or the slug of a document that names none.
    pub heading: &'a str,
    pub documents: Vec<&'a RuleDocument>,
}

/// Default ceiling on a brief's size, in characters. The reference corpus
/// tokenizes at 2.62 characters per token, so this is about 1.5k tokens: above
/// the median top-5 brief (~1.0k) and the p90 (~1.4k) of the AK-769 set, and
/// well under the ~2.6k that two uncapped decisions cost.
pub const DEFAULT_MAX_CHARS: usize = 4000;

/// Room kept free for the note that names decisions left out entirely.
const OMISSION_NOTE_RESERVE: usize = 64;

/// Render decisions as a brief, or `None` when there is nothing to say: no
/// decisions, none of them stating an obligation, or a cap too small for any
/// rule to fit.
///
/// `rules_per_decision` caps each decision; `max_chars` caps the whole brief.
/// Rules are kept or dropped whole, in rank order, never cut mid-line. The
/// first decision that cannot place a rule ends the brief, so what is left out
/// is always the lowest-ranked tail, and a note says how many decisions it was.
///
/// The brief opens with a sentence saying what it is and which file it is
/// about. Hook context is meant to be project information, and a bare run of
/// `MUST` lines reads like a command arriving from nowhere — which is what an
/// injection looks like. The rule lines themselves keep the rules' own words.
pub fn render_brief(
    file: &str,
    decisions: &[BriefDecision<'_>],
    rules_per_decision: usize,
    max_chars: usize,
) -> Option<String> {
    let candidates: Vec<(&str, Vec<String>)> = decisions
        .iter()
        .filter_map(|decision| {
            let lines = decision_lines(decision);
            (!lines.is_empty()).then_some((decision.heading, lines))
        })
        .collect();

    let frame = format!("This repository's own rules that apply to `{file}`:");
    // Keeping room for the omission note can cost a decision that would fit
    // alone, so a brief that comes out empty is packed again without it.
    pack(&frame, &candidates, rules_per_decision, max_chars, true)
        .or_else(|| pack(&frame, &candidates, rules_per_decision, max_chars, false))
}

/// One packing pass. With `reserve_note`, room is kept for the omission note
/// whenever it is or may yet be owed; without, the note is added only if it
/// still fits, so the cap holds either way.
fn pack(
    frame: &str,
    candidates: &[(&str, Vec<String>)],
    rules_per_decision: usize,
    max_chars: usize,
    reserve_note: bool,
) -> Option<String> {
    let mut used = frame.chars().count();
    let mut blocks: Vec<String> = Vec::new();
    let mut omitted = 0usize;

    for (at, (heading, lines)) in candidates.iter().enumerate() {
        let later = candidates.len() - at - 1;
        // The note is owed if this is not the last decision.
        let reserve = if reserve_note && later > 0 {
            OMISSION_NOTE_RESERVE
        } else {
            0
        };
        let total = lines.len();
        let fit = (1..=total.min(rules_per_decision)).rev().find_map(|shown| {
            let block = decision_block(heading, &lines[..shown], total);
            // +2 for the blank line that joins blocks.
            (used + 2 + block.chars().count() + reserve <= max_chars).then_some(block)
        });
        match fit {
            Some(block) => {
                used += 2 + block.chars().count();
                blocks.push(block);
            }
            None => {
                omitted = candidates.len() - at;
                break;
            }
        }
    }

    if blocks.is_empty() {
        return None;
    }
    if omitted > 0 {
        let noun = if omitted == 1 {
            "decision"
        } else {
            "decisions"
        };
        let note = format!("({omitted} more {noun} not shown)");
        if used + 2 + note.chars().count() <= max_chars {
            blocks.push(note);
        }
    }
    Some(format!("{frame}\n\n{}", blocks.join("\n\n")))
}

fn decision_block(heading: &str, shown: &[String], total: usize) -> String {
    let mut block = format!("## {heading}\n{}", shown.join("\n"));
    if total > shown.len() {
        block.push_str(&format!("\n- ({} of {total} rules shown)", shown.len()));
    }
    block
}

/// `RuleLevel::as_str` is the serialized form (`MUST_NOT`); a brief is prose
/// the model reads, so it gets the written form.
fn level_word(level: RuleLevel) -> &'static str {
    match level {
        RuleLevel::MustNot => "MUST NOT",
        _ => "MUST",
    }
}

/// Every obligation of a decision as a line, identical statements merged
/// under the first document that stated them.
fn decision_lines(decision: &BriefDecision<'_>) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();

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
            lines.push(format!(
                "- [{slug}/{}] {} {}",
                rule.id,
                level_word(rule.level),
                rule.statement
            ));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use crate::rules::parse_rule_document;

    const FILE: &str = "services/auth/token.ts";

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
        let brief = render_brief(
            FILE,
            &[decision("Adopt RS256", vec![&a])],
            8,
            DEFAULT_MAX_CHARS,
        )
        .expect("a brief");

        assert!(brief.contains("\n\n## Adopt RS256\n"), "{brief}");
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
        let brief = render_brief(
            FILE,
            &[decision("Adopt RS256", vec![&a])],
            8,
            DEFAULT_MAX_CHARS,
        )
        .expect("a brief");

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
        let brief = render_brief(
            FILE,
            &[decision("Adopt RS256", vec![&a, &b])],
            8,
            DEFAULT_MAX_CHARS,
        )
        .expect("a brief");

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
        let brief = render_brief(
            FILE,
            &[decision("Adopt RS256", vec![&a, &b])],
            8,
            DEFAULT_MAX_CHARS,
        )
        .expect("a brief");

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
        let brief = render_brief(
            FILE,
            &[decision("Adopt RS256", vec![&a])],
            2,
            DEFAULT_MAX_CHARS,
        )
        .expect("a brief");

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
        let brief = render_brief(
            FILE,
            &[decision("Adopt RS256", vec![&a])],
            8,
            DEFAULT_MAX_CHARS,
        )
        .expect("a brief");

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
            FILE,
            &[
                decision("Adopt RS256", vec![&a]),
                decision("Pin Providers", vec![&b]),
            ],
            8,
            DEFAULT_MAX_CHARS,
        )
        .expect("a brief");

        assert!(brief.contains("## Adopt RS256"));
        assert!(brief.contains("\n\n## Pin Providers"), "{brief}");
    }

    /// Nothing to say is `None`, not an empty heading: the hook turns this
    /// into silence.
    #[test]
    fn test_nothing_to_say_is_none() {
        assert_eq!(render_brief(FILE, &[], 8, DEFAULT_MAX_CHARS), None);

        let advice_only = doc(
            "cross-cutting-signing-e410",
            "Adopt RS256: Token Signing",
            "- **R-A-001** SHOULD: rotate quarterly.",
        );
        assert_eq!(
            render_brief(
                FILE,
                &[decision("Adopt RS256", vec![&advice_only])],
                8,
                DEFAULT_MAX_CHARS
            ),
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
        assert_eq!(
            render_brief(
                FILE,
                &[decision("Adopt RS256", vec![&a])],
                0,
                DEFAULT_MAX_CHARS
            ),
            None
        );
    }

    /// The brief says what it is and which file it is about before any rule,
    /// so it reads as project information rather than a bare command.
    #[test]
    fn test_brief_opens_with_a_factual_frame() {
        let a = doc(
            "cross-cutting-signing-e410",
            "Adopt RS256: Token Signing",
            "- **R-A-001** MUST: sign with RS256.",
        );
        let brief = render_brief(
            FILE,
            &[decision("Adopt RS256", vec![&a])],
            8,
            DEFAULT_MAX_CHARS,
        )
        .expect("a brief");

        assert!(
            brief.starts_with(
                "This repository's own rules that apply to `services/auth/token.ts`:\n\n"
            ),
            "{brief}"
        );
    }

    /// The total cap drops whole rules, never half a line, and says so.
    #[test]
    fn test_total_cap_drops_whole_rules_and_discloses() {
        let a = doc(
            "cross-cutting-signing-e410",
            "Adopt RS256: Token Signing",
            "- **R-A-001** MUST: one.\n- **R-A-002** MUST: two.\n- **R-A-003** MUST: three.",
        );
        let full = render_brief(FILE, &[decision("Adopt RS256", vec![&a])], 8, 10_000).unwrap();
        let cap = full.chars().count() - 5;
        let brief = render_brief(FILE, &[decision("Adopt RS256", vec![&a])], 8, cap).unwrap();

        assert!(brief.chars().count() <= cap, "{brief}");
        assert!(brief.contains("R-A-001"), "{brief}");
        assert!(!brief.contains("R-A-003"), "{brief}");
        assert!(brief.contains("rules shown)"), "{brief}");
    }

    /// A decision that does not fit is named as left out, not dropped quietly.
    #[test]
    fn test_decisions_beyond_the_cap_are_disclosed() {
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
        let decisions = [
            decision("Adopt RS256", vec![&a]),
            decision("Pin Providers", vec![&b]),
        ];
        let both = render_brief(FILE, &decisions, 8, 10_000).unwrap();
        let brief = render_brief(FILE, &decisions, 8, both.chars().count() - 1).unwrap();

        assert!(brief.contains("## Adopt RS256"), "{brief}");
        assert!(!brief.contains("## Pin Providers"), "{brief}");
        assert!(brief.contains("(1 more decision not shown)"), "{brief}");
    }

    /// More than one decision left out is disclosed in the plural.
    #[test]
    fn test_several_decisions_beyond_the_cap_are_disclosed_in_the_plural() {
        let a = doc(
            "cross-cutting-signing-e410",
            "Adopt RS256: Token Signing",
            "- **R-A-001** MUST: sign with RS256.",
        );
        let b = doc(
            "cross-cutting-pinning-c3d4",
            "Pin Providers: Terraform",
            "- **R-B-001** MUST: pin every provider to an exact version in the lockfile.",
        );
        let c = doc(
            "cross-cutting-logging-f5a6",
            "Structured Logging: Services",
            "- **R-C-001** MUST: emit structured JSON logs with a correlation id attached.",
        );
        let decisions = [
            decision("Adopt RS256", vec![&a]),
            decision("Pin Providers", vec![&b]),
            decision("Structured Logging", vec![&c]),
        ];
        let first_only = render_brief(FILE, &decisions[..1], 8, 10_000).unwrap();
        let cap = first_only.chars().count() + OMISSION_NOTE_RESERVE;
        let brief = render_brief(FILE, &decisions, 8, cap).unwrap();

        assert!(brief.contains("## Adopt RS256"), "{brief}");
        assert!(brief.contains("(2 more decisions not shown)"), "{brief}");
    }

    /// A smaller decision never fills a gap left by a higher-ranked one that
    /// did not fit: what is left out is the tail, so the note's count is true.
    #[test]
    fn test_a_lower_decision_is_not_shown_past_a_higher_one_that_did_not_fit() {
        let long = doc(
            "cross-cutting-pinning-c3d4",
            "Pin Providers: Terraform",
            "- **R-B-001** MUST: pin every provider to an exact version in the lockfile and never use a floating range.",
        );
        let short = doc(
            "cross-cutting-signing-e410",
            "Adopt RS256: Token Signing",
            "- **R-A-001** MUST: sign.",
        );
        let decisions = [
            decision("Pin Providers", vec![&long]),
            decision("Adopt RS256", vec![&short]),
        ];
        let short_only = render_brief(FILE, &decisions[1..], 8, 10_000).unwrap();
        let cap = short_only.chars().count();

        assert_eq!(render_brief(FILE, &decisions, 8, cap), None);
    }

    /// A decision that fits the cap alone is shown even when a lower one
    /// does not fit, and the cap holds.
    #[test]
    fn test_a_fitting_decision_survives_a_lower_one_that_does_not_fit() {
        let a = doc(
            "cross-cutting-signing-e410",
            "Adopt RS256: Token Signing",
            "- **R-A-001** MUST: sign with RS256.",
        );
        let b = doc(
            "cross-cutting-pinning-c3d4",
            "Pin Providers: Terraform",
            "- **R-B-001** MUST: pin every provider to an exact version in the lockfile.",
        );
        let top_only = render_brief(FILE, &[decision("Adopt RS256", vec![&a])], 8, 10_000).unwrap();
        let cap = top_only.chars().count();
        let decisions = [
            decision("Adopt RS256", vec![&a]),
            decision("Pin Providers", vec![&b]),
        ];
        let brief = render_brief(FILE, &decisions, 8, cap).expect("the top decision fits");

        assert!(brief.contains("## Adopt RS256"), "{brief}");
        assert!(brief.chars().count() <= cap, "{brief}");
    }

    /// A cap too small for any rule is silence, like a cap of zero rules.
    #[test]
    fn test_a_cap_too_small_for_any_rule_says_nothing() {
        let a = doc(
            "cross-cutting-signing-e410",
            "Adopt RS256: Token Signing",
            "- **R-A-001** MUST: sign with RS256.",
        );
        assert_eq!(
            render_brief(FILE, &[decision("Adopt RS256", vec![&a])], 8, 10),
            None
        );
    }
}
