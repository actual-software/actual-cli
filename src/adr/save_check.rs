//! The check the server runs before it saves a decision, run locally.
//!
//! A save normalizes the decision, checks its content, caps the size of its
//! Context File, and makes sure that file reads back to the same decision.
//! [`check_document`] runs those steps in the server's order and returns
//! every reason in the server's wording, so `actual adr` can report what a
//! save would refuse before it sends the request.
//!
//! The server's schema check is not ported: it refuses malformed JSON, which a
//! [`ContextFile`] cannot be.

use std::sync::LazyLock;

use regex::Regex;

use crate::adr::context_file::{
    self, js_trim, Consequences, ContextFile, ParsedContextFile, Policy, JS_SPACE_CLASS,
};

/// The longest Context File the server saves, in UTF-16 code units.
pub const MAX_CONTEXT_FILE_UNITS: usize = 102_400;

/// What [`check_markdown`] returns: the check's result plus the parser's
/// warnings, which do not stop a save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedMarkdown {
    /// The normalized decision, or every reason the server would refuse it.
    pub result: Result<ContextFile, Vec<String>>,
    /// The parser's warnings, such as the "Untitled" fallback; empty when the
    /// markdown did not parse.
    pub warnings: Vec<String>,
}

/// A run of whitespace holding a line break, which a list entry loses.
static LINE_BREAK_RUN: LazyLock<Regex> = LazyLock::new(|| {
    let s = JS_SPACE_CLASS;
    Regex::new(&format!(r"{s}*[\r\n]+{s}*")).expect("valid regex")
});

/// Whether `text` would run over more than one line. Only CR and LF count:
/// U+2028 passes here, and the read back then reports the policy instead.
fn has_line_break(text: &str) -> bool {
    text.contains(['\r', '\n'])
}

/// A list entry's own line breaks would split it into several entries in the
/// Context File, so each whitespace run holding one becomes a single space.
fn one_line(text: &str) -> String {
    js_trim(&LINE_BREAK_RUN.replace_all(text, " ")).to_string()
}

/// The entries kept to one line, without those left empty.
fn list_entries(entries: &[String]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| one_line(entry))
        .filter(|entry| !entry.is_empty())
        .collect()
}

/// The decision as the server stores it. Deciders pass through untouched and
/// statements are trimmed but not folded to one line; the content checks
/// refuse a statement that still has a line break.
fn normalize(doc: &ContextFile, as_draft: bool, today: &str) -> ContextFile {
    let lists = |consequences: &Consequences| {
        (
            list_entries(&consequences.positive),
            list_entries(&consequences.negative),
        )
    };
    let (positive, negative) = doc.consequences.as_ref().map(lists).unwrap_or_default();
    let has_consequences = !positive.is_empty() || !negative.is_empty();
    ContextFile {
        title: js_trim(&doc.title).to_string(),
        status: if as_draft { "proposed" } else { "accepted" }.to_string(),
        date: today.to_string(),
        deciders: doc.deciders.clone(),
        context: list_entries(&doc.context),
        policies: doc
            .policies
            .iter()
            .map(|policy| Policy {
                level: policy.level,
                statement: js_trim(&policy.statement).to_string(),
            })
            .collect(),
        rationale: list_entries(&doc.rationale),
        consequences: has_consequences.then_some(Consequences { positive, negative }),
        references: list_entries(&doc.references),
    }
}

/// Refusals for an empty or multi-line title, missing context, no policies,
/// and each empty or multi-line policy statement, in the server's order.
fn content_errors(doc: &ContextFile) -> Vec<String> {
    let mut errors = Vec::new();
    if doc.title.is_empty() {
        errors.push("Add a title.".to_string());
    } else if has_line_break(&doc.title) {
        errors.push("Keep the title on one line.".to_string());
    }
    if doc.context.is_empty() {
        errors.push("Add context: why the decision is needed.".to_string());
    }
    if doc.policies.is_empty() {
        errors.push("Add at least one policy.".to_string());
    }
    for (index, policy) in doc.policies.iter().enumerate() {
        let number = index + 1;
        if policy.statement.is_empty() {
            errors.push(format!("Policy {number} has no statement."));
        } else if has_line_break(&policy.statement) {
            errors.push(format!(
                "Policy {number} runs over more than one line; give each policy one statement."
            ));
        }
    }
    errors
}

/// The parser's result, its error turned into the save's list of reasons.
fn parse_for_save(markdown: &str) -> Result<ParsedContextFile, Vec<String>> {
    context_file::parse(markdown).map_err(|error| vec![error])
}

/// The parts of `saved` that `read_back` does not reproduce, in the server's
/// order. Absent consequences compare as two empty lists.
fn drift(saved: &ContextFile, read_back: &ContextFile) -> Vec<&'static str> {
    let consequences = |doc: &ContextFile| doc.consequences.clone().unwrap_or_default();
    [
        ("title", read_back.title == saved.title),
        ("context", read_back.context == saved.context),
        ("policies", read_back.policies == saved.policies),
        ("rationale", read_back.rationale == saved.rationale),
        (
            "consequences",
            consequences(read_back) == consequences(saved),
        ),
        ("references", read_back.references == saved.references),
    ]
    .into_iter()
    .filter(|(_, same)| !same)
    .map(|(part, _)| part)
    .collect()
}

/// The server's save check for `doc`: normalize it as the server does (title
/// trimmed, list entries trimmed with inner line breaks folded to spaces,
/// status `proposed` when `as_draft` else `accepted`, date set to `today`),
/// then run the content checks, the size cap and the round trip. Returns the
/// normalized document, or every reason in the server's order and exact wording.
///
/// `today` is the save's day in UTC, `YYYY-MM-DD`, which is how the server
/// stamps it.
pub fn check_document(
    doc: &ContextFile,
    as_draft: bool,
    today: &str,
) -> Result<ContextFile, Vec<String>> {
    let saved = normalize(doc, as_draft, today);
    let errors = content_errors(&saved);
    if !errors.is_empty() {
        return Err(errors);
    }

    // JavaScript's `length`, which the server's cap counts, is UTF-16 units.
    let markdown = context_file::render(&saved);
    if markdown.encode_utf16().count() > MAX_CONTEXT_FILE_UNITS {
        return Err(vec![format!(
            "The decision is too long to save: its Context File is over {} KB.",
            MAX_CONTEXT_FILE_UNITS / 1024
        )]);
    }

    // The read back cannot fail once the content checks pass (the file then
    // has a title and a policy line), but the server forwards the parser's
    // error if it does, and so does this.
    let read_back = parse_for_save(&markdown)?;
    let drift = drift(&saved, &read_back.doc);
    if drift.is_empty() {
        Ok(saved)
    } else {
        Err(drift
            .into_iter()
            .map(|part| {
                format!("The {part} would not read back the same from the decision's Context File.")
            })
            .collect())
    }
}

/// Parse `markdown` (what a create sends as `markdown`) and check it the way
/// the create route does: a parse error is the only reason when parsing
/// fails; otherwise `check_document`'s result. The parser's warnings ride along.
pub fn check_markdown(markdown: &str, as_draft: bool, today: &str) -> CheckedMarkdown {
    match parse_for_save(markdown) {
        Ok(parsed) => CheckedMarkdown {
            result: check_document(&parsed.doc, as_draft, today),
            warnings: parsed.warnings,
        },
        Err(errors) => CheckedMarkdown {
            result: Err(errors),
            warnings: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::types::RuleLevel;

    const TODAY: &str = "2026-10-28";

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn policy(level: RuleLevel, statement: &str) -> Policy {
        Policy {
            level,
            statement: statement.to_string(),
        }
    }

    /// Shaped like a decision a person wrote and a save sends.
    fn shaped_draft() -> ContextFile {
        ContextFile {
            title: "Read and write through the admin client".to_string(),
            status: "proposed".to_string(),
            date: "2026-10-01".to_string(),
            deciders: String::new(),
            context: strings(&[
                "Handlers reach the database from several places.",
                "RLS is on with no policies.",
            ]),
            policies: vec![
                policy(
                    RuleLevel::Must,
                    "API handlers MUST read and write through the admin client.",
                ),
                policy(
                    RuleLevel::ShouldNot,
                    "Browser code SHOULD NOT import the database client.",
                ),
            ],
            rationale: strings(&["One client keeps the access checks in one place."]),
            consequences: Some(Consequences {
                positive: strings(&["Fewer access paths."]),
                negative: strings(&["Every handler needs the admin client."]),
            }),
            references: Vec::new(),
        }
    }

    fn errors_of(doc: &ContextFile) -> Vec<String> {
        check_document(doc, false, TODAY).expect_err("the check fails")
    }

    fn saved(doc: &ContextFile, as_draft: bool) -> ContextFile {
        check_document(doc, as_draft, TODAY).expect("the check passes")
    }

    fn parse_error(markdown: &str) -> String {
        context_file::parse(markdown).expect_err("the markdown does not parse")
    }

    fn drift_error(part: &str) -> String {
        format!("The {part} would not read back the same from the decision's Context File.")
    }

    #[test]
    fn passes_a_shaped_draft_and_stamps_the_status_and_date() {
        let saved = saved(&shaped_draft(), false);
        assert_eq!(
            saved,
            ContextFile {
                status: "accepted".to_string(),
                date: TODAY.to_string(),
                ..shaped_draft()
            }
        );
        let markdown = context_file::render(&saved);
        assert!(markdown
            .contains("1. MUST: API handlers MUST read and write through the admin client."));
        assert!(
            markdown.contains("2. SHOULD_NOT: Browser code SHOULD NOT import the database client.")
        );
    }

    #[test]
    fn stores_a_draft_as_proposed() {
        let mut doc = shaped_draft();
        doc.status = "accepted".to_string();
        assert_eq!(saved(&doc, true).status, "proposed");
    }

    #[test]
    fn trims_and_folds_list_entries() {
        let doc = ContextFile {
            title: "  Padded title  ".to_string(),
            date: String::new(),
            deciders: " Kept as sent ".to_string(),
            context: strings(&[
                "  first point ",
                "",
                "   ",
                "second\n  point",
                "a\u{3000}\r\n\t b",
                "line\u{2028}separator",
                "\u{85}kept",
            ]),
            policies: vec![policy(RuleLevel::May, "  Cache keys per request.  ")],
            rationale: strings(&["line one\r\nline two"]),
            consequences: Some(Consequences {
                positive: strings(&[" "]),
                negative: Vec::new(),
            }),
            references: strings(&["\r\n"]),
            ..shaped_draft()
        };
        let saved = saved(&doc, false);
        assert_eq!(saved.title, "Padded title");
        assert_eq!(saved.deciders, " Kept as sent ");
        assert_eq!(
            saved.context,
            strings(&[
                "first point",
                "second point",
                "a b",
                "line\u{2028}separator",
                "\u{85}kept"
            ])
        );
        assert_eq!(
            saved.policies,
            vec![policy(RuleLevel::May, "Cache keys per request.")]
        );
        assert_eq!(saved.rationale, strings(&["line one line two"]));
        assert_eq!(saved.consequences, None);
        assert!(saved.references.is_empty());
    }

    #[test]
    fn keeps_consequences_with_one_list_left() {
        let mut doc = shaped_draft();
        doc.consequences = Some(Consequences {
            positive: Vec::new(),
            negative: strings(&[" Slower\nwrites "]),
        });
        assert_eq!(
            saved(&doc, false).consequences,
            Some(Consequences {
                positive: Vec::new(),
                negative: strings(&["Slower writes"]),
            })
        );
    }

    #[test]
    fn asks_for_a_title_context_and_a_policy() {
        let doc = ContextFile {
            title: "   ".to_string(),
            context: strings(&["  "]),
            policies: Vec::new(),
            ..shaped_draft()
        };
        assert_eq!(
            errors_of(&doc),
            [
                "Add a title.",
                "Add context: why the decision is needed.",
                "Add at least one policy."
            ]
        );
    }

    #[test]
    fn names_each_empty_or_multi_line_policy() {
        let doc = ContextFile {
            policies: vec![
                policy(RuleLevel::Must, "Fine."),
                policy(RuleLevel::Must, "   "),
                policy(RuleLevel::Should, "First half\nsecond half"),
                policy(RuleLevel::May, "Carriage\rreturn"),
            ],
            ..shaped_draft()
        };
        assert_eq!(
            errors_of(&doc),
            [
                "Policy 2 has no statement.",
                "Policy 3 runs over more than one line; give each policy one statement.",
                "Policy 4 runs over more than one line; give each policy one statement.",
            ]
        );
    }

    #[test]
    fn refuses_a_title_on_more_than_one_line() {
        let doc = ContextFile {
            title: "One\nTwo".to_string(),
            ..shaped_draft()
        };
        assert_eq!(errors_of(&doc), ["Keep the title on one line."]);
    }

    #[test]
    fn refuses_a_context_file_over_the_cap() {
        let doc = ContextFile {
            context: vec!["x".repeat(MAX_CONTEXT_FILE_UNITS)],
            ..shaped_draft()
        };
        assert_eq!(
            errors_of(&doc),
            ["The decision is too long to save: its Context File is over 100 KB."]
        );
    }

    #[test]
    fn counts_the_cap_in_utf16_units() {
        let base = ContextFile {
            context: strings(&["x"]),
            ..shaped_draft()
        };
        let overhead = context_file::render(&saved(&base, false)).len() - 1;
        let room = MAX_CONTEXT_FILE_UNITS - overhead;
        let with_entry = |entry: String| ContextFile {
            context: vec![entry],
            ..base.clone()
        };
        assert!(check_document(&with_entry("x".repeat(room)), false, TODAY).is_ok());
        assert!(check_document(&with_entry("x".repeat(room + 1)), false, TODAY).is_err());
        // An emoji is one char but two UTF-16 units, so this entry is over
        // the cap although it has fewer chars than `room`.
        let emoji = "\u{1F600}".repeat(room / 2 + 1);
        assert!(emoji.chars().count() < room);
        assert!(check_document(&with_entry(emoji), false, TODAY).is_err());
    }

    #[test]
    fn reports_a_policy_that_would_not_read_back() {
        // U+2028 is not a line break to the content check, but the parser's
        // statement stops at it.
        let doc = ContextFile {
            policies: vec![policy(RuleLevel::Must, "Use X\u{2028}for Y")],
            ..shaped_draft()
        };
        assert_eq!(errors_of(&doc), [drift_error("policies")]);
    }

    #[test]
    fn reports_each_part_a_deciders_line_would_change() {
        // Deciders are not folded to one line, so they can add sections the
        // decision does not have.
        let doc = ContextFile {
            deciders: "Team\n## Rationale\n- injected\n## Consequences\nPositive:\n- p\n\
                       ## References\n- r"
                .to_string(),
            rationale: Vec::new(),
            consequences: None,
            ..shaped_draft()
        };
        assert_eq!(
            errors_of(&doc),
            [
                drift_error("rationale"),
                drift_error("consequences"),
                drift_error("references")
            ]
        );
    }

    #[test]
    fn check_markdown_reports_a_parse_error_alone() {
        let checked = check_markdown("# T\n\n## Context\n\n- c\n", false, TODAY);
        assert_eq!(
            checked.result,
            Err(vec![parse_error("# T\n\n## Context\n\n- c\n")])
        );
        assert!(checked.warnings.is_empty());
        assert_eq!(
            check_markdown(" \n", true, TODAY).result,
            Err(strings(&["Paste some Context File markdown to import."]))
        );
    }

    #[test]
    fn check_markdown_carries_the_parser_warnings() {
        let checked = check_markdown("## Decision\n\n1. MUST: always do this.\n", false, TODAY);
        assert_eq!(
            checked.result,
            Err(strings(&["Add context: why the decision is needed."]))
        );
        assert_eq!(
            checked.warnings,
            [
                "No title found (expected a \"# Title\" heading); defaulted to \"Untitled\".",
                "No \"## Context\" section found; the draft will have an empty context.",
            ]
        );
    }

    #[test]
    fn check_markdown_saves_what_the_server_saves() {
        let markdown = "# Use Postgres for storage\n\nStatus: proposed\nDate: 2020-01-01\n\
            Deciders: Jane Doe\n\n## Context\n\n- We need one relational store.\n\n## Decision\n\n\
            1. MUST: Store relational data in Postgres.\n2. SHOULD_NOT: Add a second relational database.\n\n\
            ## Rationale\n\n- One engine to operate.\n";
        let checked = check_markdown(markdown, false, "2026-11-11");
        assert!(checked.warnings.is_empty());
        let saved = checked.result.expect("the check passes");
        assert_eq!(
            context_file::render(&saved),
            "# Use Postgres for storage\n\nStatus: accepted\nDate: 2026-11-11\nDeciders: Jane Doe\n\n\
             ## Context\n\n- We need one relational store.\n\n## Decision\n\n\
             1. MUST: Store relational data in Postgres.\n2. SHOULD_NOT: Add a second relational database.\n\n\
             ## Rationale\n\n- One engine to operate."
        );
    }
}
