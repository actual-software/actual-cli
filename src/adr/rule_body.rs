//! The body of the `.actual/rules/` file this CLI writes for a decision.
//!
//! Actual AI's repository sync has a model write that body, so it cannot be
//! reproduced locally; this is the CLI's own fixed template for it. Its
//! `Scope:` line is the one the sync writes in code, so a re-sync keeps it.
//!
//! The template is shaped for the rule-file parser: a `# <title>` heading, the
//! scope sentence, `### Rules`, and one `- **<id>** LEVEL: statement` bullet per
//! rule. Line breaks inside the title, scope line, level or a statement are
//! folded to spaces, because a stray line could open a code fence or a heading
//! and hide the rules after it.

use crate::adr::header::{normalize_scope_root, REPOSITORY_ROOT};

/// Include globs the sync reads as "the whole repository".
const MATCH_ALL_PATTERNS: [&str; 3] = ["**/*", "**", "/"];

/// The sync's `Scope:` sentence for a decision scope: `Scope: applies to the whole repository.` for root `.` with no paths, else the "applies to `…`; excludes `…`." form, with paths prefixed by a non-`.` root as the sync does.
///
/// As in the sync, a root with no include paths stands for `<root>/**` (the
/// whole repository for `.`), the include list reads "the whole repository"
/// when it holds only match-all globs (`**/*`, `**`, `/`), and excludes keep
/// their `!`.
pub fn scope_line(root: &str, paths: &[String]) -> String {
    let root = normalize_scope_root(root);
    let to_repo = |glob: &str| {
        if root == REPOSITORY_ROOT {
            glob.to_string()
        } else if glob == "**" {
            format!("{root}/**")
        } else {
            format!("{root}/{glob}")
        }
    };
    let mut include: Vec<String> = paths
        .iter()
        .filter(|path| !path.starts_with('!'))
        .map(|path| to_repo(path))
        .collect();
    let exclude: Vec<String> = paths
        .iter()
        .filter_map(|path| path.strip_prefix('!'))
        .map(|path| format!("!{}", to_repo(path)))
        .collect();
    if include.is_empty() {
        include.push(to_repo(if root == REPOSITORY_ROOT {
            "**/*"
        } else {
            "**"
        }));
    }
    let applies = if include
        .iter()
        .all(|glob| MATCH_ALL_PATTERNS.contains(&glob.as_str()))
    {
        "applies to the whole repository".to_string()
    } else {
        format!("applies to {}", code_list(&include))
    };
    let excludes = if exclude.is_empty() {
        String::new()
    } else {
        format!("; excludes {}", code_list(&exclude))
    };
    format!("Scope: {applies}{excludes}.")
}

/// Rule IDs for a decision: `R-` and the first four hex digits of its id, uppercased (`R-0B6F`).
pub fn rule_id_prefix(adr_id: &str) -> String {
    let digits: String = adr_id
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(4)
        .collect();
    format!("R-{}", digits.to_ascii_uppercase())
}

/// The rule file body: `# <title>`, a blank line, the scope line, a blank line, `### Rules`, a blank line, then one `- **<prefix>-NNN** <LEVEL>: <statement>` bullet per rule numbered from 001, ending with a newline. `rules` holds each rule's level as a rule file spells it (`MUST`, `MUST NOT`, `SHOULD`, `SHOULD NOT`, `MAY`) and its statement.
///
/// The rule-file parser reads every rule of this body when the title and each
/// statement have text, each level is one of those keywords, and `scope_line`
/// comes from [`scope_line`]; it drops a rule with an empty statement or an
/// unknown level.
pub fn render_rule_body(
    title: &str,
    scope_line: &str,
    rule_id_prefix: &str,
    rules: &[(&str, &str)],
) -> String {
    let mut body = format!(
        "# {}\n\n{}\n\n### Rules\n\n",
        one_line(title),
        one_line(scope_line)
    );
    for (number, (level, statement)) in rules.iter().enumerate() {
        body.push_str(&format!(
            "- **{}-{:03}** {}: {}\n",
            one_line(rule_id_prefix),
            number + 1,
            one_line(level),
            one_line(statement)
        ));
    }
    body
}

/// Backtick each glob and join them with `, `.
fn code_list(globs: &[String]) -> String {
    globs
        .iter()
        .map(|glob| format!("`{glob}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `text` on one line: each line trimmed, blank lines dropped, the rest joined
/// with single spaces.
fn one_line(text: &str) -> String {
    text.split(['\r', '\n'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adr::header::{render_header, AdrHeader};

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn scope_line_whole_repository() {
        assert_eq!(
            scope_line(".", &[]),
            "Scope: applies to the whole repository."
        );
        assert_eq!(
            scope_line("", &strings(&["**"])),
            "Scope: applies to the whole repository."
        );
        assert_eq!(
            scope_line("./", &strings(&["**/*", "/"])),
            "Scope: applies to the whole repository."
        );
        assert_eq!(
            scope_line(".", &strings(&["!**/*.test.ts"])),
            "Scope: applies to the whole repository; excludes `!**/*.test.ts`."
        );
    }

    #[test]
    fn scope_line_prefixes_a_root() {
        // The real example's line.
        assert_eq!(
            scope_line(
                "backend",
                &strings(&["src/activities/**", "src/utils/**", "src/workers/**"])
            ),
            "Scope: applies to `backend/src/activities/**`, `backend/src/utils/**`, \
             `backend/src/workers/**`."
        );
        assert_eq!(
            scope_line("services/api/", &[]),
            "Scope: applies to `services/api/**`."
        );
        assert_eq!(
            scope_line("services/api", &strings(&["**", "!**/*.test.ts"])),
            "Scope: applies to `services/api/**`; excludes `!services/api/**/*.test.ts`."
        );
        assert_eq!(
            scope_line(".", &strings(&["src/**", "!src/gen/**", "!**/*.snap"])),
            "Scope: applies to `src/**`; excludes `!src/gen/**`, `!**/*.snap`."
        );
        assert_eq!(
            scope_line("caf\u{e9}", &strings(&["d\u{e9}j\u{e0}/**"])),
            "Scope: applies to `caf\u{e9}/d\u{e9}j\u{e0}/**`."
        );
    }

    #[test]
    fn rule_id_prefix_takes_four_hex_digits() {
        assert_eq!(
            rule_id_prefix("0b6f2a1e-1111-2222-3333-444455556666"),
            "R-0B6F"
        );
        assert_eq!(rule_id_prefix("fdbbcb01-e293"), "R-FDBB");
        assert_eq!(rule_id_prefix("\u{e9}-ab-zz-c9d"), "R-ABC9");
        assert_eq!(rule_id_prefix("xyz"), "R-");
    }

    #[test]
    fn renders_the_minimal_rule_file() {
        let id = "0b6f2a1e-1111-2222-3333-444455556666";
        let header = render_header(&AdrHeader {
            name: "cross-cutting-use-postgres-storage-0b6f".to_string(),
            description: "Use Postgres for storage".to_string(),
            adr_id: id.to_string(),
            status: "created-from-user".to_string(),
            scope_root: ".".to_string(),
            scope_paths: Vec::new(),
        });
        let body = render_rule_body(
            "Use Postgres for storage",
            &scope_line(".", &[]),
            &rule_id_prefix(id),
            &[("MUST", "Store relational data in Postgres.")],
        );
        assert_eq!(
            format!("{header}\n{body}"),
            "---\nversion: 2\nname: cross-cutting-use-postgres-storage-0b6f\n\
             description: Use Postgres for storage\nmetadata:\n  actual:\n    \
             adr_id: 0b6f2a1e-1111-2222-3333-444455556666\n    status: created-from-user\n  \
             scope:\n    root: .\n---\n\n# Use Postgres for storage\n\n\
             Scope: applies to the whole repository.\n\n### Rules\n\n\
             - **R-0B6F-001** MUST: Store relational data in Postgres.\n"
        );
    }

    #[test]
    fn numbers_every_rule_and_spells_each_level() {
        let body = render_rule_body(
            "Caf\u{e9}: cr\u{e8}me \u{1F389}",
            "Scope: applies to `src/**`.",
            "R-ABCD",
            &[
                ("MUST", "Do one."),
                ("MUST NOT", "Do two: never."),
                ("SHOULD", "Prefer **this** one."),
                ("SHOULD NOT", "NOT a level word."),
                ("MAY", "Na\u{ef}ve r\u{e9}sum\u{e9}."),
            ],
        );
        assert_eq!(
            body,
            "# Caf\u{e9}: cr\u{e8}me \u{1F389}\n\nScope: applies to `src/**`.\n\n### Rules\n\n\
             - **R-ABCD-001** MUST: Do one.\n\
             - **R-ABCD-002** MUST NOT: Do two: never.\n\
             - **R-ABCD-003** SHOULD: Prefer **this** one.\n\
             - **R-ABCD-004** SHOULD NOT: NOT a level word.\n\
             - **R-ABCD-005** MAY: Na\u{ef}ve r\u{e9}sum\u{e9}.\n"
        );
        let many: Vec<(&str, &str)> = (0..1000).map(|_| ("MAY", "x")).collect();
        let body = render_rule_body("T", "Scope: x.", "R-0000", &many);
        assert!(body.contains("- **R-0000-999** MAY: x\n- **R-0000-1000** MAY: x\n"));
    }

    #[test]
    fn line_breaks_are_folded() {
        let body = render_rule_body(
            "Two\nline title",
            "Scope: a\r\n\r\nb.",
            "R-1\n",
            &[("MUST\n", "First line\n```\n## heading\n  indented  ")],
        );
        assert_eq!(
            body,
            "# Two line title\n\nScope: a b.\n\n### Rules\n\n\
             - **R-1-001** MUST: First line ``` ## heading indented\n"
        );
        assert_eq!(
            render_rule_body("T", "S", "R-X", &[]),
            "# T\n\nS\n\n### Rules\n\n"
        );
    }
}
