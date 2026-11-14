//! The Context File: the markdown form of a decision.
//!
//! The server shows this file under a decision's Context File tab, writes it to
//! `docs/adr/`, and reads it back when a decision is created from markdown.
//! [`parse`] reads a file the way the server's parser does, and [`render`]
//! writes the file the server's serializer writes, byte for byte.
//!
//! # Design
//!
//! Both halves copy the server's quirks on purpose, because a local check is
//! only worth running when it agrees with the save. Three of them surprise a
//! reader:
//!
//! * The policy pattern is not anchored, so the first RFC-2119 keyword
//!   anywhere on a line wins: `1. Services MUST use X` reads as `MUST: use X`.
//! * Text is trimmed, split and matched with JavaScript's rules. JavaScript
//!   whitespace includes U+FEFF but not U+0085, the reverse of
//!   [`char::is_whitespace`], and a JavaScript `.` also stops at U+2028 and
//!   U+2029. The helpers and patterns below follow the JavaScript rules.
//! * Only the title, the `Date:` and `Deciders:` lines, and the Context,
//!   Decision (or Policy Block), Rationale, Consequences and References
//!   sections are read. Any other section is dropped on the server, so
//!   [`ContextFile`] has no field for it.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::rules::types::RuleLevel;

/// One policy of a decision: a level and a one-line statement.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Policy {
    /// The RFC-2119 level, written `MUST_NOT` (underscore) in the file and on
    /// the wire.
    pub level: RuleLevel,
    /// What the policy requires, on one line.
    pub statement: String,
}

/// The `## Consequences` section: a `Positive:` list and a `Negative:` list.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Consequences {
    /// The bullets under `Positive:`.
    pub positive: Vec<String>,
    /// The bullets under `Negative:`.
    pub negative: Vec<String>,
}

/// A decision as a Context File carries it: the fields the server's parser
/// reads and its serializer writes.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ContextFile {
    /// The `# Title` heading.
    pub title: String,
    /// The `Status:` line, such as `accepted` or `proposed`. The server's
    /// parser ignores the line, so [`parse`] always returns `proposed`; a save
    /// sets the status from its draft choice.
    pub status: String,
    /// The `Date:` line, `YYYY-MM-DD` once saved. Empty when the file has
    /// none: the server fills in the day of the import there, and a save
    /// stamps the day anyway.
    pub date: String,
    /// The `Deciders:` line, free text and possibly empty.
    pub deciders: String,
    /// The `## Context` bullets: why the decision is needed.
    pub context: Vec<String>,
    /// The `## Decision` lines, `1. MUST: ...`, in order.
    pub policies: Vec<Policy>,
    /// The `## Rationale` bullets.
    pub rationale: Vec<String>,
    /// The `## Consequences` section. `None` writes no section, while
    /// `Some` with two empty lists still writes the heading and both labels.
    pub consequences: Option<Consequences>,
    /// The `## References` bullets.
    pub references: Vec<String>,
}

/// What [`parse`] returns: the decision plus the parser's non-fatal warnings
/// (for example the "Untitled" fallback when there is no `# Title`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedContextFile {
    /// The decision the file describes.
    pub doc: ContextFile,
    /// Problems the server reports without refusing the file, in its order.
    pub warnings: Vec<String>,
}

const EMPTY_MARKDOWN: &str = "Paste some Context File markdown to import.";
const NO_POLICIES: &str = "No policies found. Expect rule lines like \"1. MUST: …\" under \"## Decision\" (or \"- MUST …\" under \"## Policy Block\").";
const NO_TITLE: &str =
    "No title found (expected a \"# Title\" heading); defaulted to \"Untitled\".";
const NO_CONTEXT: &str = "No \"## Context\" section found; the draft will have an empty context.";

/// JavaScript's `\s` as a regex class. Rust's `\s` adds U+0085 and lacks
/// U+FEFF.
pub(crate) const JS_SPACE_CLASS: &str =
    r"[\t\n\x0B\x0C\r \xA0\u1680\u2000-\u200A\u2028\u2029\u202F\u205F\u3000\uFEFF]";

/// A policy line: an optional `**R-XXX-000**` id, an RFC-2119 keyword, then
/// the statement after a `:` (`1. MUST: ...`) or after whitespace
/// (`- MUST ...`). Two-word keywords take a space or an underscore, and come
/// before their one-word prefixes so the longer one wins.
///
/// The server's pattern is case-insensitive without Unicode mode, so its case
/// folding and its word boundaries are ASCII-only (`(?i-u)`, `(?-u:\b)`): with
/// Unicode folding, U+017F (long s) would match `s`. Its `.` stops at every
/// JavaScript line terminator, not just `\n`.
static RULE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    let s = JS_SPACE_CLASS;
    Regex::new(&format!(
        r"(?:\*\*(?i-u:R-[A-Z]+-[0-9]+)\*\*{s}+)?(?-u:\b)(?<keyword>(?i-u:MUST[ _]NOT|MUST|SHALL[ _]NOT|SHALL|SHOULD[ _]NOT|SHOULD|NOT[ _]RECOMMENDED|RECOMMENDED|REQUIRED|MAY|OPTIONAL))(?-u:\b)(?::|{s}){s}*(?<statement>[^\n\r\u2028\u2029]+)"
    ))
    .expect("valid regex")
});

/// Whether JavaScript counts `c` as whitespace (`\s`, and what `trim` strips):
/// its WhiteSpace and LineTerminator characters.
pub(crate) fn is_js_whitespace(c: char) -> bool {
    const SPACES: &[char] = &[
        '\t', '\n', '\u{B}', '\u{C}', '\r', ' ', '\u{A0}', '\u{1680}', '\u{2028}', '\u{2029}',
        '\u{202F}', '\u{205F}', '\u{3000}', '\u{FEFF}',
    ];
    SPACES.contains(&c) || ('\u{2000}'..='\u{200A}').contains(&c)
}

/// JavaScript's `String.prototype.trim`.
pub(crate) fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// The server's line split (`/\r?\n/`): each `\n` ends a line and takes one
/// `\r` before it along, and a trailing newline leaves an empty last line.
fn split_lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
}

/// The text after the `##` of a `## ` heading line. The third character must
/// be a space or a tab, so `### Rules` is body text of the section around it.
fn h2_heading(line: &str) -> Option<&str> {
    line.strip_prefix("##")
        .filter(|rest| rest.starts_with([' ', '\t']))
}

/// Each section's trimmed body by its exact, case-sensitive heading text. A
/// repeated heading replaces the earlier one, and a heading with no text is
/// body text (though it still ends the preamble).
fn split_sections(markdown: &str) -> HashMap<&str, String> {
    let mut sections = HashMap::new();
    let mut current: Option<(&str, Vec<&str>)> = None;
    for line in split_lines(markdown) {
        let title = h2_heading(line)
            .map(js_trim)
            .filter(|title| !title.is_empty());
        if let Some(title) = title {
            if let Some((key, body)) = current.replace((title, Vec::new())) {
                sections.insert(key, js_trim(&body.join("\n")).to_string());
            }
        } else if let Some((_, body)) = current.as_mut() {
            body.push(line);
        }
    }
    if let Some((key, body)) = current {
        sections.insert(key, js_trim(&body.join("\n")).to_string());
    }
    sections
}

/// The body of the first heading in `keys` that is present: the Context File
/// name, then the legacy numbered name (`## 3. Decision`).
fn section<'a>(sections: &'a HashMap<&str, String>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| sections.get(*key))
        .map(String::as_str)
}

/// The lines before the first `## ` heading: the title, `Date:` and
/// `Deciders:` live here.
fn preamble_of(markdown: &str) -> String {
    split_lines(markdown)
        .take_while(|line| h2_heading(line).is_none())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The first `#` (not `##`) line of the preamble with text after the `#`.
fn h1_title(preamble: &str) -> Option<&str> {
    split_lines(preamble)
        .filter_map(|line| line.strip_prefix('#'))
        .filter(|rest| !rest.starts_with('#'))
        .map(js_trim)
        .find(|title| !title.is_empty())
}

/// The value of the first preamble line that starts with `label`, compared
/// ASCII case-insensitively. That first line decides: an empty value there is
/// "absent" even when a later line has one.
fn preamble_field<'a>(preamble: &'a str, label: &str) -> Option<&'a str> {
    split_lines(preamble)
        .map(js_trim)
        .find_map(|line| {
            let head = line.get(..label.len())?;
            head.eq_ignore_ascii_case(label)
                .then(|| js_trim(&line[label.len()..]))
        })
        .filter(|value| !value.is_empty())
}

/// An attribute of a legacy `<adr_meta ... />` tag, found the way the server
/// finds it: the attributes run from the first space after `<adr_meta` to the
/// first `/>`, and the value is whatever follows the first `key="`, so
/// `title` also finds the end of `subtitle="`.
fn meta_attr<'a>(markdown: &'a str, key: &str) -> Option<&'a str> {
    let tag = &markdown[markdown.find("<adr_meta")?..];
    let close = tag.find("/>")?;
    let space = tag[..close].find(' ')?;
    let attrs = js_trim(&tag[space + 1..close]);
    let needle = format!("{key}=\"");
    let start = attrs.find(&needle)? + needle.len();
    let len = attrs[start..].find('"')?;
    Some(&attrs[start..start + len])
}

/// The items of a list section. A `- ` line is an item without its marker,
/// and so is any other non-empty line except one starting with `#` or `---`:
/// a wrapped paragraph becomes several items and `* x` stays `* x`.
fn list_items(section: Option<&str>) -> Vec<String> {
    let mut items = Vec::new();
    for line in split_lines(section.unwrap_or_default()).map(js_trim) {
        if let Some(item) = line.strip_prefix("- ") {
            items.push(js_trim(item).to_string());
        } else if !line.is_empty() && !line.starts_with('#') && !line.starts_with("---") {
            items.push(line.to_string());
        }
    }
    items
}

/// Every line of a section that matches `RULE_PATTERN` with a non-empty
/// statement, in order; other lines are skipped.
fn policies_in(section: Option<&str>) -> Vec<Policy> {
    split_lines(section.unwrap_or_default())
        .filter_map(|line| {
            let found = RULE_PATTERN.captures(line)?;
            let keyword = found.name("keyword")?.as_str().replace('_', " ");
            let level = RuleLevel::parse(&keyword)?;
            let statement = js_trim(found.name("statement")?.as_str());
            (!statement.is_empty()).then(|| Policy {
                level,
                statement: statement.to_string(),
            })
        })
        .collect()
}

/// The Consequences lists. A line that starts with `Positive` or `Negative`
/// once its `*`s are removed (`Positive:`, `**Negative:**`) picks the list,
/// and `- ` lines join the list picked last. `None` when both are empty.
fn consequences_in(section: Option<&str>) -> Option<Consequences> {
    let mut found = Consequences::default();
    let mut into_positive = None;
    for line in split_lines(section.unwrap_or_default()).map(js_trim) {
        let label = line.replace('*', "");
        let label = js_trim(&label);
        if label.starts_with("Positive") {
            into_positive = Some(true);
        } else if label.starts_with("Negative") {
            into_positive = Some(false);
        } else if let (Some(item), Some(positive)) = (line.strip_prefix("- "), into_positive) {
            let list = if positive {
                &mut found.positive
            } else {
                &mut found.negative
            };
            list.push(js_trim(item).to_string());
        }
    }
    (!found.positive.is_empty() || !found.negative.is_empty()).then_some(found)
}

/// Read Context File markdown the way the server does. `Err` carries the
/// server's own message (for example the "No policies found..." text).
pub fn parse(markdown: &str) -> Result<ParsedContextFile, String> {
    if js_trim(markdown).is_empty() {
        return Err(EMPTY_MARKDOWN.to_string());
    }
    let sections = split_sections(markdown);
    let preamble = preamble_of(markdown);

    let mut policies = policies_in(section(&sections, &["Decision", "3. Decision"]));
    if policies.is_empty() {
        policies = policies_in(section(&sections, &["Policy Block", "4. Policy Block"]));
    }
    if policies.is_empty() {
        return Err(NO_POLICIES.to_string());
    }

    let mut warnings = Vec::new();
    let title = h1_title(&preamble)
        .or_else(|| meta_attr(markdown, "title").map(js_trim))
        .filter(|title| !title.is_empty())
        .map_or_else(
            || {
                warnings.push(NO_TITLE.to_string());
                "Untitled".to_string()
            },
            str::to_string,
        );
    let context = list_items(section(&sections, &["Context", "1. Context"]));
    if context.is_empty() {
        warnings.push(NO_CONTEXT.to_string());
    }
    let header_value = |label: &str, key: &str| {
        preamble_field(&preamble, label)
            .or_else(|| meta_attr(markdown, key))
            .map_or_else(String::new, |value| js_trim(value).to_string())
    };

    let doc = ContextFile {
        title,
        status: "proposed".to_string(),
        date: header_value("Date:", "date"),
        deciders: header_value("Deciders:", "deciders"),
        context,
        policies,
        rationale: list_items(section(&sections, &["Rationale", "5. Rationale"])),
        consequences: consequences_in(section(&sections, &["Consequences", "6. Consequences"])),
        references: list_items(section(&sections, &["References", "13. References"])),
    };
    Ok(ParsedContextFile { doc, warnings })
}

/// The heading and `- ` bullets of a list section; nothing when it is empty.
fn bullet_section(heading: &str, items: &[String]) -> Vec<String> {
    if items.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![heading.to_string(), String::new()];
    lines.extend(items.iter().map(|item| format!("- {item}")));
    lines
}

/// Write the Context File the server writes for `doc`, byte for byte
/// (including its trimming, so no trailing newline).
pub fn render(doc: &ContextFile) -> String {
    let mut lines = vec![
        format!("# {}", doc.title),
        String::new(),
        format!("Status: {}", doc.status),
        format!("Date: {}", doc.date),
        format!("Deciders: {}", doc.deciders),
    ];
    // A list section's blank separator line is written even when the list is
    // empty, so an empty section leaves an extra blank line behind.
    lines.push(String::new());
    lines.extend(bullet_section("## Context", &doc.context));
    if !doc.policies.is_empty() {
        lines.extend([String::new(), "## Decision".to_string(), String::new()]);
        lines.extend(doc.policies.iter().enumerate().map(|(index, policy)| {
            format!(
                "{}. {}: {}",
                index + 1,
                policy.level.as_str(),
                policy.statement
            )
        }));
    }
    lines.push(String::new());
    lines.extend(bullet_section("## Rationale", &doc.rationale));
    if let Some(consequences) = &doc.consequences {
        lines.extend(["", "## Consequences", "", "Positive:"].map(String::from));
        lines.extend(consequences.positive.iter().map(|item| format!("- {item}")));
        lines.extend(["", "Negative:"].map(String::from));
        lines.extend(consequences.negative.iter().map(|item| format!("- {item}")));
    }
    // The separator of the server's Implementation Notes section, which a
    // Context File read here never has, then the References separator.
    lines.extend([String::new(), String::new()]);
    lines.extend(bullet_section("## References", &doc.references));
    js_trim(&lines.join("\n")).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Context File as the server's serializer writes one, with a trailing
    /// newline as an editor leaves it.
    const CONTEXT_FILE_MD: &str = "# Use API key authentication

Status: proposed
Date: 2026-07-29
Deciders: Platform

## Context

- Users need to authenticate requests to the HTTP API.
- The package ships no auth of its own.

## Decision

1. MUST: clients pass an API key via the `Authorization: Bearer` header.
2. SHOULD: rotate API keys on a 90-day cadence.
3. MUST_NOT: log API keys in plaintext.
4. MAY: cache validated keys for the request lifetime.

## Rationale

- Bearer headers are the ecosystem convention.

## Consequences

Positive:
- Simple, ubiquitous client integration.

Negative:
- Requires secure key storage.

## References

- https://example.com/auth-spec
";

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn policy(level: RuleLevel, statement: &str) -> Policy {
        Policy {
            level,
            statement: statement.to_string(),
        }
    }

    fn parsed(markdown: &str) -> ParsedContextFile {
        parse(markdown).expect("the markdown parses")
    }

    fn levels(markdown: &str) -> Vec<RuleLevel> {
        parsed(markdown)
            .doc
            .policies
            .iter()
            .map(|policy| policy.level)
            .collect()
    }

    fn api_key_decision() -> ContextFile {
        ContextFile {
            title: "Use API key authentication".to_string(),
            status: "proposed".to_string(),
            date: "2026-07-29".to_string(),
            deciders: "Platform".to_string(),
            context: strings(&[
                "Users need to authenticate requests to the HTTP API.",
                "The package ships no auth of its own.",
            ]),
            policies: vec![
                policy(
                    RuleLevel::Must,
                    "clients pass an API key via the `Authorization: Bearer` header.",
                ),
                policy(RuleLevel::Should, "rotate API keys on a 90-day cadence."),
                policy(RuleLevel::MustNot, "log API keys in plaintext."),
                policy(
                    RuleLevel::May,
                    "cache validated keys for the request lifetime.",
                ),
            ],
            rationale: strings(&["Bearer headers are the ecosystem convention."]),
            consequences: Some(Consequences {
                positive: strings(&["Simple, ubiquitous client integration."]),
                negative: strings(&["Requires secure key storage."]),
            }),
            references: strings(&["https://example.com/auth-spec"]),
        }
    }

    #[test]
    fn js_trim_strips_javascript_whitespace_only() {
        assert_eq!(js_trim("\u{FEFF}\u{A0} a b \u{3000}\u{2028}"), "a b");
        assert_eq!(js_trim("\u{85}a\u{85}"), "\u{85}a\u{85}");
        assert!(is_js_whitespace('\u{2005}'));
        assert!(!is_js_whitespace('\u{200B}'));
    }

    #[test]
    fn split_lines_matches_the_server_split() {
        let lines: Vec<&str> = split_lines("a\r\nb\r\r\nc\rd\n").collect();
        assert_eq!(lines, ["a", "b\r", "c\rd", ""]);
    }

    #[test]
    fn parses_the_context_file_format() {
        let parsed = parsed(CONTEXT_FILE_MD);
        assert_eq!(parsed.doc, api_key_decision());
        assert!(parsed.warnings.is_empty());
    }

    #[test]
    fn reads_the_decision_block_the_serializer_writes() {
        let markdown = "# Round trip\n\nStatus: proposed\nDate: 2026-07-29\nDeciders: Team\n\n\
            ## Context\n\n- context one\n- context two\n\n## Decision\n\n\
            1. MUST: do a\n2. MUST_NOT: do b\n3. SHOULD: do c\n4. SHOULD_NOT: do d\n5. MAY: do e\n";
        let doc = parsed(markdown).doc;
        assert_eq!(doc.title, "Round trip");
        assert_eq!(doc.context, strings(&["context one", "context two"]));
        assert_eq!(
            doc.policies,
            vec![
                policy(RuleLevel::Must, "do a"),
                policy(RuleLevel::MustNot, "do b"),
                policy(RuleLevel::Should, "do c"),
                policy(RuleLevel::ShouldNot, "do d"),
                policy(RuleLevel::May, "do e"),
            ]
        );
    }

    #[test]
    fn folds_keyword_synonyms_onto_the_five_levels() {
        let markdown = "# Synonyms\n\n## Decision\n\n1. REQUIRED: a.\n2. RECOMMENDED: b.\n\
            3. NOT RECOMMENDED: c.\n4. OPTIONAL: d.\n5. SHALL NOT: e.\n6. NOT_RECOMMENDED: f.\n\
            7. shall_not: g.\n8. Must Not: h.\n9. shall: i.\n";
        assert_eq!(
            levels(markdown),
            [
                RuleLevel::Must,
                RuleLevel::Should,
                RuleLevel::ShouldNot,
                RuleLevel::May,
                RuleLevel::MustNot,
                RuleLevel::ShouldNot,
                RuleLevel::MustNot,
                RuleLevel::MustNot,
                RuleLevel::Must,
            ]
        );
    }

    #[test]
    fn falls_back_to_the_policy_block() {
        let markdown =
            "# Policy block only\n\n## Context\n\n- Some context.\n\n## Policy Block\n\n\
            - MUST do the thing.\n- SHOULD prefer the other thing.\n";
        let doc = parsed(markdown).doc;
        assert_eq!(
            doc.policies,
            vec![
                policy(RuleLevel::Must, "do the thing."),
                policy(RuleLevel::Should, "prefer the other thing."),
            ]
        );
        // A Decision section without policy lines also falls back, and the
        // numbered legacy heading counts.
        let markdown = "# T\n## Decision\nWe decided.\n## 4. Policy Block\n- MAY skip it\n";
        assert_eq!(levels(markdown), [RuleLevel::May]);
    }

    #[test]
    fn prefers_the_named_section_over_the_numbered_one() {
        // `## Decision` is present, so `## 3. Decision` is never read, even
        // though only the numbered one has policies.
        let markdown = "# T\n## Decision\nNone yet.\n## 3. Decision\n1. MUST: x\n";
        assert_eq!(parse(markdown), Err(NO_POLICIES.to_string()));
    }

    #[test]
    fn accepts_the_legacy_numbered_template() {
        let markdown = "<adr_meta title=\"Legacy MkII\" status=\"accepted\" date=\" 2026-01-01 \" deciders=\"Arch\"/>\n\n\
            ## 1. Context\n- Legacy context.\n\n## 3. Decision\n\
            - **R-X-001** MUST: keep parsing old skill output.\n\
            - **R-X-002** SHOULD: prefer the new Context File format.\n\n\
            ## 5. Rationale\n- Old output exists.\n\n## 6. Consequences\n**Positive:**\n- Both import.\n\
            **Negative:**\n- Two shapes.\n\n## 13. References\n- docs/adr/0001.md\n";
        let parsed = parsed(markdown);
        assert_eq!(
            parsed.doc,
            ContextFile {
                title: "Legacy MkII".to_string(),
                status: "proposed".to_string(),
                date: "2026-01-01".to_string(),
                deciders: "Arch".to_string(),
                context: strings(&["Legacy context."]),
                policies: vec![
                    policy(RuleLevel::Must, "keep parsing old skill output."),
                    policy(RuleLevel::Should, "prefer the new Context File format."),
                ],
                rationale: strings(&["Old output exists."]),
                consequences: Some(Consequences {
                    positive: strings(&["Both import."]),
                    negative: strings(&["Two shapes."]),
                }),
                references: strings(&["docs/adr/0001.md"]),
            }
        );
        assert!(parsed.warnings.is_empty());
    }

    #[test]
    fn defaults_a_missing_title_to_untitled() {
        let parsed = parsed("## Decision\n\n1. MUST: always do this.\n");
        assert_eq!(parsed.doc.title, "Untitled");
        assert_eq!(parsed.doc.date, "");
        assert_eq!(parsed.doc.deciders, "");
        assert_eq!(parsed.warnings, [NO_TITLE, NO_CONTEXT]);
    }

    #[test]
    fn refuses_a_file_without_policies() {
        let error = parse("# No rules\n\n## Context\n- Just context, no decision rules.\n");
        assert_eq!(
            error,
            Err(
                "No policies found. Expect rule lines like \"1. MUST: \u{2026}\" under \
                 \"## Decision\" (or \"- MUST \u{2026}\" under \"## Policy Block\")."
                    .to_string()
            )
        );
    }

    #[test]
    fn refuses_empty_input() {
        let empty = Err("Paste some Context File markdown to import.".to_string());
        assert_eq!(parse(""), empty);
        assert_eq!(parse("   "), empty);
        assert_eq!(parse("\u{FEFF}\r\n\t"), empty);
        // JavaScript does not count U+0085 as whitespace.
        assert_eq!(parse("\u{85}"), Err(NO_POLICIES.to_string()));
    }

    #[test]
    fn reads_pathological_whitespace() {
        let markdown = format!(
            "<adr_meta title=\"Legacy MkII\" status=\"proposed\" date=\"2026-01-01\"{}/>\n\n\
             # Title{}with tabs\n\n## 1. Context\n- Legacy context.\n\n## 3. Decision\n\
             - **R-X-001** MUST: keep parsing old skill output.\n",
            " ".repeat(5000),
            "\t".repeat(5000)
        );
        // The `# ` heading wins over the tag's title.
        assert!(parsed(&markdown).doc.title.starts_with("Title\t"));
    }

    #[test]
    fn ignores_a_yaml_header() {
        let header = "---\nname: 3798cbc3-use-api-key-authentication\ndescription: |-\n  # Fake title\n  \
            Clients authenticate with API keys.\npaths:\n  - src/auth/**\nmetadata:\n  actual:\n    \
            schema: 1\n---\n";
        let with_header = parsed(&format!("{header}{CONTEXT_FILE_MD}"));
        assert_eq!(with_header.doc, api_key_decision());
    }

    #[test]
    fn reads_the_first_keyword_anywhere_on_a_line() {
        let markdown = "# T\n## Decision\n1. Services MUST use X\n2. You may cache keys\n\
            - MAYBE later\nMUST_NOTE: not a keyword\n3. MUST:   \n4. MU\u{17F}T: long s\n\
            5. **r-ab-1** should_not: lower-case id\nno keyword here\n";
        assert_eq!(
            parsed(markdown).doc.policies,
            vec![
                policy(RuleLevel::Must, "use X"),
                policy(RuleLevel::May, "cache keys"),
                policy(RuleLevel::ShouldNot, "lower-case id"),
            ]
        );
    }

    #[test]
    fn splits_statements_with_javascript_rules() {
        let markdown =
            "# T\n## Decision\n1. MUST\u{A0}nbsp\n2. MUST\u{FEFF}bom\n3. MUST\u{85}nel\n\
            4. MUST: cut\u{2028}here\n5. SHOULD: keep\u{85}\n6. MAY: stop\rthere\n";
        assert_eq!(
            parsed(markdown).doc.policies,
            vec![
                policy(RuleLevel::Must, "nbsp"),
                policy(RuleLevel::Must, "bom"),
                policy(RuleLevel::Must, "cut"),
                policy(RuleLevel::Should, "keep\u{85}"),
                policy(RuleLevel::May, "stop"),
            ]
        );
    }

    #[test]
    fn splits_sections_like_the_server() {
        // `## ` with no text ends the preamble (so the Date line is not read)
        // without opening a section. Headings are exact and case-sensitive,
        // the last of a repeated heading wins, and `### ` and `##x` are body
        // text.
        let markdown = "# T\n## \nDate: 2026-01-01\n## context\n- lower\n##\tContext\n- first\n\
            ## Context \n- second\n## \n### Rules\n## Decision\n### Rules\n1. MUST: a\n##Decision\n2. MAY: b";
        let parsed = parsed(markdown);
        assert_eq!(parsed.doc.title, "T");
        assert_eq!(parsed.doc.date, "");
        assert_eq!(parsed.doc.context, strings(&["second"]));
        assert_eq!(
            parsed.doc.policies,
            vec![policy(RuleLevel::Must, "a"), policy(RuleLevel::May, "b")]
        );
        assert!(parsed.warnings.is_empty());
    }

    #[test]
    fn reads_list_sections_like_the_server() {
        let markdown = "# T\nnot in a section\n## Decision\n1. MUST: x\n## Context\n\n- a\n* b\n\
            wrapped paragraph\nline two\n---\n# heading\n-\n\n  - indented  \n## Rationale\n- r\n\
            ## References\n\n";
        let doc = parsed(markdown).doc;
        assert_eq!(
            doc.context,
            strings(&["a", "* b", "wrapped paragraph", "line two", "-", "indented"])
        );
        assert_eq!(doc.rationale, strings(&["r"]));
        assert!(doc.references.is_empty());
    }

    #[test]
    fn reads_consequences_like_the_server() {
        let markdown = "# T\n## Decision\n1. MUST: x\n## Consequences\n- orphan\nPositive:\n- p1\n\
            plain line\n**Negative:**\n- n1\n*Positive* again\n- p2\n";
        assert_eq!(
            parsed(markdown).doc.consequences,
            Some(Consequences {
                positive: strings(&["p1", "p2"]),
                negative: strings(&["n1"]),
            })
        );
        let only_labels = "# T\n## Decision\n1. MUST: x\n## Consequences\nPositive:\n\nNegative:\n";
        assert_eq!(parsed(only_labels).doc.consequences, None);
        let only_negative = "# T\n## Decision\n1. MUST: x\n## 6. Consequences\nNegatives\n- n\n";
        assert_eq!(
            parsed(only_negative).doc.consequences,
            Some(Consequences {
                positive: Vec::new(),
                negative: strings(&["n"]),
            })
        );
    }

    #[test]
    fn reads_date_and_deciders_like_the_server() {
        // Labels match case-insensitively on a trimmed line; Status is
        // ignored; the first Deciders line decides even when it is empty, so
        // the tag's value applies.
        let markdown = "# T\nStatus: accepted\n  date:  2026-03-04  \nDeciders:\nDeciders: Late\n\
            <adr_meta deciders=\" Meta \" date=\"2020-02-02\" />\n## Decision\n1. MUST: x\n";
        let doc = parsed(markdown).doc;
        assert_eq!(doc.status, "proposed");
        assert_eq!(doc.date, "2026-03-04");
        assert_eq!(doc.deciders, "Meta");
    }

    #[test]
    fn reads_legacy_tag_attributes_like_the_server() {
        let title = |preamble: &str| {
            parsed(&format!("{preamble}\n## Decision\n1. MUST: x"))
                .doc
                .title
        };
        assert_eq!(title(""), "Untitled");
        assert_eq!(title("<adr_meta title=\"No close\""), "Untitled");
        assert_eq!(title("<adr_meta/> title=\"A\" "), "Untitled");
        assert_eq!(title("<adr_meta date=\"2026-01-01\" />"), "Untitled");
        assert_eq!(title("<adr_meta title=\"unterminated />"), "Untitled");
        assert_eq!(title("<adr_meta title=\"  \" />"), "Untitled");
        assert_eq!(title("<adr_meta subtitle=\"Sub\" title=\"Main\" />"), "Sub");
        assert_eq!(title("#\n#\t \n<adr_meta title=\" Meta \"/>"), "Meta");
        assert_eq!(title("# Heading\n<adr_meta title=\"Meta\"/>"), "Heading");
        assert_eq!(title("#Tight"), "Tight");
        assert_eq!(title("  # Indented\n#\tTabbed"), "Tabbed");
    }

    #[test]
    fn renders_the_server_layout() {
        let mut doc = api_key_decision();
        doc.status = "accepted".to_string();
        doc.policies.truncate(3);
        let expected = "# Use API key authentication\n\nStatus: accepted\nDate: 2026-07-29\n\
            Deciders: Platform\n\n## Context\n\n- Users need to authenticate requests to the HTTP API.\n\
            - The package ships no auth of its own.\n\n## Decision\n\n\
            1. MUST: clients pass an API key via the `Authorization: Bearer` header.\n\
            2. SHOULD: rotate API keys on a 90-day cadence.\n3. MUST_NOT: log API keys in plaintext.\n\n\
            ## Rationale\n\n- Bearer headers are the ecosystem convention.\n\n## Consequences\n\n\
            Positive:\n- Simple, ubiquitous client integration.\n\nNegative:\n- Requires secure key storage.\n\n\n\
            ## References\n\n- https://example.com/auth-spec";
        assert_eq!(render(&doc), expected);
    }

    #[test]
    fn renders_empty_sections_as_the_server_does() {
        // The final trim takes the space after an empty `Deciders:` along.
        assert_eq!(
            render(&ContextFile::default()),
            "# \n\nStatus: \nDate: \nDeciders:"
        );
        let doc = ContextFile {
            title: "T".to_string(),
            status: "proposed".to_string(),
            date: "2026-01-01".to_string(),
            consequences: Some(Consequences::default()),
            ..ContextFile::default()
        };
        assert_eq!(
            render(&doc),
            "# T\n\nStatus: proposed\nDate: 2026-01-01\nDeciders: \n\n\n\n## Consequences\n\nPositive:\n\nNegative:"
        );
        let doc = ContextFile {
            deciders: "D".to_string(),
            policies: vec![policy(RuleLevel::May, "s")],
            consequences: None,
            references: strings(&["r"]),
            ..doc
        };
        assert_eq!(
            render(&doc),
            "# T\n\nStatus: proposed\nDate: 2026-01-01\nDeciders: D\n\n\n## Decision\n\n1. MAY: s\n\n\n\n## References\n\n- r"
        );
    }

    #[test]
    fn reads_back_what_it_renders() {
        let mut minimal = ContextFile {
            title: "Minimal".to_string(),
            status: "proposed".to_string(),
            date: "2026-11-11".to_string(),
            policies: vec![policy(RuleLevel::ShouldNot, "Add a second database.")],
            ..ContextFile::default()
        };
        let mut docs = vec![api_key_decision(), minimal.clone()];
        minimal.context = strings(&["- nested marker", "#hash", "* star"]);
        minimal.rationale = strings(&["Positive: not a label here"]);
        minimal.consequences = Some(Consequences {
            positive: Vec::new(),
            negative: strings(&["**", "Negative"]),
        });
        minimal.references = strings(&["\u{85}kept"]);
        docs.push(minimal);
        for doc in docs {
            assert_eq!(parsed(&render(&doc)).doc, doc);
        }
    }

    #[test]
    fn policy_uses_the_wire_spelling() {
        let policy = policy(RuleLevel::MustNot, "Log keys.");
        let json = serde_json::to_string(&policy).expect("serializes");
        assert_eq!(json, r#"{"level":"MUST_NOT","statement":"Log keys."}"#);
        assert_eq!(
            serde_json::from_str::<Policy>(&json).expect("deserializes"),
            policy
        );
        let doc = serde_json::to_value(ContextFile::default()).expect("serializes");
        assert_eq!(doc["consequences"], serde_json::Value::Null);
        assert_eq!(doc["policies"], serde_json::json!([]));
    }
}
