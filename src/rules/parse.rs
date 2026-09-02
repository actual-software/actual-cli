//! Line-based parser for a single `.actual/rules/*.md` document.
//!
//! # Design
//!
//! No markdown crate is used, and none is warranted: the governing structure is
//! six landmarks deep, and the repository already parses structured markdown
//! this way in [`crate::generation::markers`]. This module performs **no I/O**,
//! so it is exercised entirely from string fixtures and can be reused by a
//! caller that already holds rule text.
//!
//! Two ordering decisions carry the whole design:
//!
//! * **Fences are consumed before headings are recognized.** The `### Verify`
//!   blocks are shell scripts, and shell comments start with `# `. In the
//!   reference corpus those comments outnumber real `# <Title>` headings almost
//!   five to one, so a scanner that checks for headings first mis-reads every
//!   file it sees.
//! * **Levels are matched as a longest-keyword prefix**, in
//!   [`RuleLevel::split_prefix`], never by splitting on the first colon. The
//!   corpus contains `MUST NOT apply to: ...`, where the colon belongs to the
//!   statement.
//!
//! Failures are two-tier. Structural absence — an empty file, no `### Rules`,
//! nothing parseable under it — is fatal and the file is skipped. An
//! unterminated fence is fatal only when it leaves the document with no
//! parseable rules; otherwise it is a warning, the same as an unterminated
//! `<enforcement>` tag. Anything local to one bullet is a warning recorded on
//! the document, because losing one malformed rule is better than losing the
//! eight good ones beside it.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::rules::types::{
    Rule, RuleDocument, RuleFileError, RuleIssue, RuleIssueKind, RuleLevel, VerifyBlock,
};

/// Matches the bold rule-id span and returns it with the rest of the line.
///
/// The id is captured loosely — anything inside the `**` delimiters — because
/// real corpora contain `R-JWT-001`, `R-ACTIVITY-INPUT-001`, `R-46-006` and
/// `EXC-001`. A stricter pattern silently drops rules.
fn rule_id_regex() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\*\*\s*([^*\s][^*]*?)\s*\*\*\s*(.*)$")
            .expect("valid regex — this is a programmer error")
    });
    &RE
}

/// Which part of the document the scanner is currently inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    /// Before any recognized section — where the title and the applicability
    /// sentence live.
    Preamble,
    Rules,
    Verify,
    AcceptWhen,
    /// A recognized-but-uninteresting region. Everything here is dropped, which
    /// is how trailing sections and in-line asides are ignored without naming
    /// any of them.
    Ignored,
}

/// Parse one rule document.
///
/// Returns `Err` only for structural failures that make the file unusable; a
/// document that parsed with local problems is returned as `Ok` with those
/// problems in [`RuleDocument::warnings`].
pub fn parse_rule_document(path: &Path, text: &str) -> Result<RuleDocument, RuleFileError> {
    if text.trim().is_empty() {
        return Err(RuleFileError::new(
            path,
            RuleIssueKind::Empty,
            None,
            "file is empty",
        ));
    }

    let lines: Vec<&str> = text.lines().collect();
    let mut doc = RuleDocument::empty(path);
    let mut section = Section::Preamble;
    let mut saw_rules_heading = false;
    let mut i = 0usize;

    while i < lines.len() {
        let trimmed = lines[i].trim();
        let lineno = i + 1;

        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        // Fences first: their contents must never be able to move the scanner.
        // An unclosed fence is consumed through EOF, like `<enforcement>`, so
        // later headings cannot be mistaken for structure.
        if let Some(lang) = fence_lang(trimmed) {
            let (block, next) = capture_fence(&lines, i, lang, &mut doc);
            if section == Section::Verify {
                doc.verify.push(block);
            }
            i = next;
            continue;
        }

        if trimmed.starts_with("<enforcement>") {
            i = capture_enforcement(&lines, i, &mut doc);
            section = Section::Ignored;
            continue;
        }

        if let Some((level, heading)) = heading_of(trimmed) {
            section = match heading.trim_end_matches(':').to_ascii_lowercase().as_str() {
                "rules" => {
                    saw_rules_heading = true;
                    Section::Rules
                }
                "verify" => Section::Verify,
                "accept when" => Section::AcceptWhen,
                // An H1 with no other meaning is the document title; the
                // applicability sentence follows it.
                _ if level == 1 && doc.title.is_none() => {
                    doc.title = Some(heading.to_string());
                    Section::Preamble
                }
                _ => Section::Ignored,
            };
            i += 1;
            continue;
        }

        if is_bold_run_in(trimmed) {
            section = if is_accept_when(trimmed) {
                Section::AcceptWhen
            } else {
                // A bold aside such as `**In scope:**` introduces bullets that
                // are not rules. Leaving the section keeps them from being
                // reported as malformed.
                Section::Ignored
            };
            i += 1;
            continue;
        }

        match section {
            Section::Preamble => absorb_scope(trimmed, &mut doc),
            Section::Rules => absorb_rule(trimmed, lineno, &mut doc),
            Section::AcceptWhen => absorb_accept(trimmed, &mut doc),
            Section::Verify | Section::Ignored => {}
        }
        i += 1;
    }

    if doc.rules.is_empty() {
        // An unclosed fence that ate the Rules section is more specific than
        // "no heading" / "no rules", and keeps the opening-line location.
        if let Some(err) = unterminated_fence_error(path, &doc) {
            return Err(err);
        }
        if !saw_rules_heading {
            return Err(RuleFileError::new(
                path,
                RuleIssueKind::MissingRulesSection,
                None,
                "no `### Rules` section",
            ));
        }
        return Err(RuleFileError::new(
            path,
            RuleIssueKind::NoRules,
            None,
            "`### Rules` contained no parseable `**<id>** LEVEL: ...` rules",
        ));
    }
    Ok(doc)
}

// ── line classifiers ─────────────────────────────────────────────────────

/// `### Rules` becomes `Some((3, "Rules"))`. A space after the `#` run is
/// required, so `#hashtag` is not a heading.
fn heading_of(trimmed: &str) -> Option<(usize, &str)> {
    let hashes = trimmed.len() - trimmed.trim_start_matches('#').len();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    // `hashes` counts leading ASCII `#` bytes, so it is a char boundary.
    let rest = trimmed[hashes..].strip_prefix(' ')?;
    Some((hashes, rest.trim()))
}

/// ```` ```bash ```` becomes `Some(Some("bash"))`; a bare fence becomes
/// `Some(None)`.
fn fence_lang(trimmed: &str) -> Option<Option<String>> {
    let info = trimmed.strip_prefix("```")?.trim();
    Some((!info.is_empty()).then(|| info.to_string()))
}

/// True for a line that is entirely one bold span, such as `**Accept when:**`
/// or `**In scope:**`.
///
/// The "exactly one span" check matters: an unbulleted rule that happens to end
/// in bold — `**R-X-001** MUST: emphasize **this**` — also starts and ends with
/// `**`, and must not be mistaken for a run-in heading.
fn is_bold_run_in(trimmed: &str) -> bool {
    trimmed.len() >= 4
        && trimmed.starts_with("**")
        && trimmed.ends_with("**")
        && !trimmed[2..trimmed.len() - 2].contains('*')
}

/// True for `**Accept when:**` and its casing variants.
fn is_accept_when(trimmed: &str) -> bool {
    let inner = trimmed.trim_matches('*').trim();
    inner
        .strip_suffix(':')
        .unwrap_or(inner)
        .eq_ignore_ascii_case("accept when")
}

/// Strip a `-`, `*` or `+` list marker.
fn strip_bullet(trimmed: &str) -> Option<&str> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return Some(rest.trim_start());
        }
    }
    None
}

// ── block capture ────────────────────────────────────────────────────────

/// Capture a fenced block opened at `start`, returning it with the index of the
/// line after the closing fence. Reaching end of input is recorded as a
/// warning rather than discarding the document — the same policy as
/// [`capture_enforcement`]. The body through EOF is still captured so later
/// lines cannot be re-read as headings.
fn capture_fence(
    lines: &[&str],
    start: usize,
    lang: Option<String>,
    doc: &mut RuleDocument,
) -> (VerifyBlock, usize) {
    let mut end = start + 1;
    while end < lines.len() {
        if lines[end].trim().starts_with("```") {
            return (
                VerifyBlock {
                    lang,
                    body: lines[start + 1..end].join("\n"),
                    line: start + 1,
                },
                end + 1,
            );
        }
        end += 1;
    }
    doc.warnings.push(issue(
        RuleIssueKind::UnterminatedFence,
        start + 1,
        "code fence opened but never closed",
    ));
    (
        VerifyBlock {
            lang,
            body: lines[start + 1..end].join("\n"),
            line: start + 1,
        },
        end,
    )
}

/// Capture `<enforcement> ... </enforcement>` starting at `start`, returning
/// the index of the line after the closing tag. The opening and closing tags
/// may share a line. Reaching end of input is recorded as a warning rather than
/// discarding the document.
fn capture_enforcement(lines: &[&str], start: usize, doc: &mut RuleDocument) -> usize {
    const OPEN: &str = "<enforcement>";
    const CLOSE: &str = "</enforcement>";

    let mut body: Vec<&str> = Vec::new();
    let mut rest = &lines[start].trim()[OPEN.len()..];
    let mut i = start;

    loop {
        if let Some((before, _)) = rest.split_once(CLOSE) {
            body.push(before);
            doc.enforcement = Some(body.join("\n").trim().to_string());
            return i + 1;
        }
        body.push(rest);
        i += 1;
        if i >= lines.len() {
            doc.warnings.push(issue(
                RuleIssueKind::UnterminatedEnforcement,
                start + 1,
                "`<enforcement>` was never closed",
            ));
            doc.enforcement = Some(body.join("\n").trim().to_string());
            return i;
        }
        rest = lines[i];
    }
}

// ── section content ──────────────────────────────────────────────────────

fn absorb_scope(trimmed: &str, doc: &mut RuleDocument) {
    match &mut doc.scope {
        Some(existing) => {
            existing.push(' ');
            existing.push_str(trimmed);
        }
        None => doc.scope = Some(trimmed.to_string()),
    }
}

fn absorb_accept(trimmed: &str, doc: &mut RuleDocument) {
    if let Some(item) = strip_bullet(trimmed) {
        doc.accept_when.push(item.trim().to_string());
    }
}

fn absorb_rule(trimmed: &str, lineno: usize, doc: &mut RuleDocument) {
    // A bullet inside `### Rules` is meant to be a rule, so a bullet that is
    // not one is reported. A bare prose line is not, so it is dropped quietly.
    let (body, bulleted) = match strip_bullet(trimmed) {
        Some(body) => (body, true),
        None => (trimmed, false),
    };

    let Some(caps) = rule_id_regex().captures(body) else {
        if bulleted {
            doc.warnings.push(issue(
                RuleIssueKind::MalformedRule,
                lineno,
                "bullet is not a `**<id>** LEVEL: ...` rule",
            ));
        }
        return;
    };
    let id = caps[1].to_string();
    let tail = caps.get(2).map_or("", |m| m.as_str());

    let Some((level, statement)) = RuleLevel::split_prefix(tail) else {
        if bulleted {
            doc.warnings.push(issue(
                RuleIssueKind::UnknownLevel,
                lineno,
                format!("rule `{id}` has no recognized RFC-2119 level"),
            ));
        }
        return;
    };

    if statement.is_empty() {
        doc.warnings.push(issue(
            RuleIssueKind::EmptyStatement,
            lineno,
            format!("rule `{id}` has no statement"),
        ));
        return;
    }

    doc.rules.push(Rule {
        id,
        level,
        statement: statement.to_string(),
        line: lineno,
    });
}

fn issue(kind: RuleIssueKind, line: usize, detail: impl Into<String>) -> RuleIssue {
    RuleIssue {
        kind,
        line: Some(line),
        detail: detail.into(),
    }
}

/// Promote an unterminated-fence warning to a file error. Used only when the
/// scan produced no rules, so the more specific fence location is kept.
fn unterminated_fence_error(path: &Path, doc: &RuleDocument) -> Option<RuleFileError> {
    doc.warnings
        .iter()
        .find(|warning| warning.kind == RuleIssueKind::UnterminatedFence)
        .map(|warning| RuleFileError::new(path, warning.kind, warning.line, warning.detail.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: parse fixture text attributed to a throwaway path.
    fn parse(text: &str) -> Result<RuleDocument, RuleFileError> {
        parse_rule_document(Path::new("/x/rules/sample.md"), text)
    }

    /// Helper: parse text that is expected to succeed.
    fn parse_ok(text: &str) -> RuleDocument {
        parse(text).expect("document should parse")
    }

    /// Helper: the levels of every parsed rule, in document order.
    fn levels(doc: &RuleDocument) -> Vec<RuleLevel> {
        doc.rules.iter().map(|rule| rule.level).collect()
    }

    /// Helper: the ids of every parsed rule, in document order.
    fn ids(doc: &RuleDocument) -> Vec<&str> {
        doc.rules.iter().map(|rule| rule.id.as_str()).collect()
    }

    /// A real rule file, copied byte for byte from
    /// `.actual/rules/cross-cutting-access-tokens-include-e410.md` in the
    /// sprintreview corpus. Note the absence of a trailing newline, which is
    /// how every file in that corpus ends.
    const FIXTURE_REAL: &str = r##"# Adopt JWT with RS256 for OAuth Access Token Signing in Public APIs: Access Tokens Include

These rules are ALWAYS ACTIVE for all OAuth token issuance and verification code in the project, including modules handling public API authentication, token signing, and JWKS-based key management.

### Rules

- **R-JWT-001** MUST: Access tokens MUST include standard claims: issuer (iss), audience (aud), expiration (exp), and issued-at (iat) derived from environment configuration.
- **R-JWT-002** MUST: All OAuth access token signing operations use RS256 algorithm with keyid from JWKS.
- **R-JWT-003** MUST: Token verification retrieves public keys from JWKS by kid and validates standard claims (iss, aud, exp).
- **R-JWT-004** MUST: Cryptographic random values are generated using crypto.randomBytes with at least 16 bytes for jti claims and state tokens.
- **R-JWT-005** MUST: Use mutatePayload: false in jwt.sign() to prevent modification of the original claims object.
- **R-JWT-006** MUST: Configure OAUTH_TOKEN_ISSUER and OAUTH_TOKEN_AUDIENCE environment variables to match deployment environment and API scope.
- **R-JWT-007** MUST: Implement structured logging for token verification failures using @actual/logger with error context (message, code, name).
- **R-JWT-008** SHOULD: Set OAUTH_ACCESS_TOKEN_TTL_SECONDS based on security requirements (shorter for high-security, longer for reduced token refresh overhead).
- **R-JWT-009** MAY: Use HS256 (HMAC with SHA-256) symmetric signing only for short-lived state tokens in OAuth flows (e.g., GitHub OAuth state) where symmetric HMAC signing provides sufficient security.

### Verify

```bash
# Verify RS256 usage in OAuth token signing
grep -r "jwt\.sign.*RS256" apps/actual/lib/oauth/ apps/actual/lib/github/

# Verify environment configuration for issuer and audience
grep -r "process\.env\.OAUTH_TOKEN_ISSUER\|process\.env\.OAUTH_TOKEN_AUDIENCE" apps/actual/lib/oauth/

# Verify cryptographic random value generation
grep -r "randomBytes(16)" apps/actual/lib/oauth/ apps/actual/lib/github/
```

**Accept when:**
- All OAuth access token signing operations use RS256 algorithm with keyid from JWKS
- Token verification retrieves public keys from JWKS by kid and validates standard claims (iss, aud, exp)
- Cryptographic random values are generated using crypto.randomBytes with at least 16 bytes
- OAUTH_TOKEN_ISSUER and OAUTH_TOKEN_AUDIENCE environment variables are configured and validated at startup
- Token verification includes revocation checking with appropriate fail-open or fail-closed semantics
- Structured logging is implemented for token verification failures

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules. Pull requests using HS256 or other symmetric algorithms for public API tokens must be rejected with explanation. Hardcoded secrets or missing environment variable validation triggers security review. Token verification without revocation checking requires security team approval and documentation.
</enforcement>"##;

    // ── the real corpus shape ────────────────────────────────────────────

    #[test]
    fn test_parse_real_rule_file() {
        let doc = parse_ok(FIXTURE_REAL);

        assert_eq!(
            doc.title.as_deref(),
            Some(
                "Adopt JWT with RS256 for OAuth Access Token Signing in Public APIs: Access Tokens Include"
            )
        );
        assert!(doc
            .scope
            .as_deref()
            .unwrap()
            .starts_with("These rules are ALWAYS ACTIVE for all OAuth token issuance"));

        assert_eq!(
            ids(&doc),
            vec![
                "R-JWT-001",
                "R-JWT-002",
                "R-JWT-003",
                "R-JWT-004",
                "R-JWT-005",
                "R-JWT-006",
                "R-JWT-007",
                "R-JWT-008",
                "R-JWT-009",
            ]
        );
        assert_eq!(
            levels(&doc),
            vec![
                RuleLevel::Must,
                RuleLevel::Must,
                RuleLevel::Must,
                RuleLevel::Must,
                RuleLevel::Must,
                RuleLevel::Must,
                RuleLevel::Must,
                RuleLevel::Should,
                RuleLevel::May,
            ]
        );
        assert_eq!(
            doc.rules[1].statement,
            "All OAuth access token signing operations use RS256 algorithm with keyid from JWKS."
        );

        assert_eq!(doc.verify.len(), 1);
        assert_eq!(doc.verify[0].lang.as_deref(), Some("bash"));
        assert!(doc.verify[0].body.contains("grep -r \"jwt\\.sign.*RS256\""));
        // The shell comments inside the fence must not have been read as
        // headings, nor their text as content.
        assert!(doc.verify[0]
            .body
            .contains("# Verify RS256 usage in OAuth token signing"));

        assert_eq!(doc.accept_when.len(), 6);
        assert_eq!(
            doc.accept_when[0],
            "All OAuth access token signing operations use RS256 algorithm with keyid from JWKS"
        );

        assert!(doc
            .enforcement
            .as_deref()
            .unwrap()
            .starts_with("Claude Code MUST NOT skip or defer verification of these rules."));
        assert!(doc.warnings.is_empty());
    }

    /// A shell comment inside the verify fence looks exactly like an H1. If
    /// fences were not consumed first, this document would lose its rules.
    #[test]
    fn test_parse_hash_comment_inside_a_fence_is_inert() {
        let doc = parse_ok(
            "# Title\n\nScope sentence.\n\n### Rules\n\n- **R-A-001** MUST: keep going.\n\n### Verify\n\n```bash\n# Rules\n# Accept when:\ngrep -r foo .\n```\n\n**Accept when:**\n- it holds\n",
        );
        assert_eq!(ids(&doc), vec!["R-A-001"]);
        assert_eq!(doc.accept_when, vec!["it holds".to_string()]);
        assert_eq!(doc.verify[0].body, "# Rules\n# Accept when:\ngrep -r foo .");
    }

    // ── levels ───────────────────────────────────────────────────────────

    /// The acceptance criterion that levels are not flattened to all-MUST.
    #[test]
    fn test_parse_preserves_every_level_and_synonym() {
        let doc = parse_ok(
            "# T\n\n### Rules\n\n\
             - **R-A-001** MUST: a\n\
             - **R-A-002** MUST NOT: b\n\
             - **R-A-003** MUST_NOT: c\n\
             - **R-A-004** SHOULD: d\n\
             - **R-A-005** SHOULD NOT: e\n\
             - **R-A-006** MAY: f\n\
             - **R-A-007** REQUIRED: g\n\
             - **R-A-008** SHALL NOT: h\n\
             - **R-A-009** RECOMMENDED: i\n\
             - **R-A-010** NOT RECOMMENDED: j\n\
             - **R-A-011** OPTIONAL: k\n",
        );
        assert_eq!(
            levels(&doc),
            vec![
                RuleLevel::Must,
                RuleLevel::MustNot,
                RuleLevel::MustNot,
                RuleLevel::Should,
                RuleLevel::ShouldNot,
                RuleLevel::May,
                RuleLevel::Must,
                RuleLevel::MustNot,
                RuleLevel::Should,
                RuleLevel::ShouldNot,
                RuleLevel::May,
            ]
        );
        assert!(doc.warnings.is_empty());
    }

    /// The corpus contains `MUST NOT apply to: ...`, where the first colon
    /// belongs to the statement rather than to the level.
    #[test]
    fn test_parse_level_keyword_followed_by_prose_and_a_colon() {
        let doc = parse_ok(
            "# T\n\n### Rules\n\n- **R-GET-008** MUST NOT apply to: Internal configuration dictionaries.\n",
        );
        assert_eq!(doc.rules[0].level, RuleLevel::MustNot);
        assert_eq!(
            doc.rules[0].statement,
            "apply to: Internal configuration dictionaries."
        );
    }

    /// Ids are taken verbatim: the corpus is not uniformly `R-XXX-NNN`.
    #[test]
    fn test_parse_rule_ids_round_trip_verbatim() {
        let doc = parse_ok(
            "# T\n\n### Rules\n\n\
             - **R-ACTIVITY-INPUT-001** MUST: a\n\
             - **R-46-006** SHOULD: b\n\
             - **EXC-001** MAY: c\n\
             - **  R-SPACED-001  ** MUST: d\n",
        );
        assert_eq!(
            ids(&doc),
            vec![
                "R-ACTIVITY-INPUT-001",
                "R-46-006",
                "EXC-001",
                "R-SPACED-001"
            ]
        );
        assert_eq!(doc.rules[0].line, 5);
    }

    #[test]
    fn test_parse_accepts_star_and_plus_bullets_and_no_bullet() {
        let doc = parse_ok(
            "# T\n\n### Rules\n\n\
             * **R-A-001** MUST: a\n\
             + **R-A-002** SHOULD: b\n\
             **R-A-003** MAY: c\n",
        );
        assert_eq!(ids(&doc), vec!["R-A-001", "R-A-002", "R-A-003"]);
    }

    /// An unbulleted rule ending in a bold span still starts and ends with
    /// `**`, and must not be mistaken for a run-in heading.
    #[test]
    fn test_parse_unbulleted_rule_ending_in_bold_is_still_a_rule() {
        let doc = parse_ok("# T\n\n### Rules\n\n**R-A-001** MUST: emphasize **this**\n");
        assert_eq!(ids(&doc), vec!["R-A-001"]);
        assert_eq!(doc.rules[0].statement, "emphasize **this**");
    }

    // ── sections ─────────────────────────────────────────────────────────

    #[test]
    fn test_parse_multi_line_scope_paragraph_is_joined() {
        let doc =
            parse_ok("# T\n\nFirst line.\nSecond line.\n\n### Rules\n\n- **R-A-001** MAY: a\n");
        assert_eq!(doc.scope.as_deref(), Some("First line. Second line."));
    }

    #[test]
    fn test_parse_bare_fence_has_no_language_and_multiple_fences_are_kept() {
        let doc = parse_ok(
            "# T\n\n### Rules\n\n- **R-A-001** MAY: a\n\n### Verify\n\n```\nplain\n```\n\n```sh\necho hi\n```\n",
        );
        assert_eq!(doc.verify.len(), 2);
        assert_eq!(doc.verify[0].lang, None);
        assert_eq!(doc.verify[0].body, "plain");
        assert_eq!(doc.verify[0].line, 9);
        assert_eq!(doc.verify[1].lang.as_deref(), Some("sh"));
    }

    /// A fence outside `### Verify` is consumed so its contents cannot steer
    /// the scanner, but it is not collected as a verify block.
    #[test]
    fn test_parse_fence_outside_verify_is_consumed_but_dropped() {
        let doc = parse_ok("# T\n\n```bash\n### Rules\n```\n\n### Rules\n\n- **R-A-001** MAY: a\n");
        assert!(doc.verify.is_empty());
        assert_eq!(ids(&doc), vec!["R-A-001"]);
    }

    #[test]
    fn test_parse_prose_in_verify_and_after_enforcement_is_dropped() {
        let doc = parse_ok(
            "# T\n\n### Rules\n\n- **R-A-001** MAY: a\n\n### Verify\n\nSome prose before the fence.\n\n```bash\nls\n```\n\n<enforcement>\nEnforce.\n</enforcement>\n\nTrailing prose.\n",
        );
        assert_eq!(doc.verify.len(), 1);
        assert_eq!(doc.enforcement.as_deref(), Some("Enforce."));
        assert_eq!(doc.rules.len(), 1);
    }

    /// Unrecognized headings — `## Consequences`, `### Exceptions` — end the
    /// current section without being named anywhere in the parser.
    #[test]
    fn test_parse_unknown_headings_end_the_current_section() {
        let doc = parse_ok(
            "# T\n\n### Rules\n\n- **R-A-001** MUST: a\n\n### Exceptions\n\n- **EXC-001**: not a rule\n\n### Verify\n\n```bash\nls\n```\n\n**Accept when:**\n- ok\n\n## Consequences\n\n- not an acceptance criterion\n",
        );
        assert_eq!(ids(&doc), vec!["R-A-001"]);
        assert_eq!(doc.accept_when, vec!["ok".to_string()]);
        assert!(doc.warnings.is_empty());
    }

    /// A bold aside such as `**In scope:**` introduces bullets that are not
    /// rules; leaving the section keeps them from being reported.
    #[test]
    fn test_parse_bold_run_in_heading_ends_the_rules_section() {
        let doc = parse_ok(
            "# T\n\n### Rules\n\n- **R-A-001** MAY: a\n\n**In scope:**\n- request bodies\n- cache reads\n\n**Out of scope:**\n- internal config\n",
        );
        assert_eq!(ids(&doc), vec!["R-A-001"]);
        assert!(doc.warnings.is_empty());
    }

    #[test]
    fn test_parse_accept_when_ends_at_enforcement_and_at_a_heading() {
        let by_enforcement = parse_ok(
            "# T\n\n### Rules\n\n- **R-A-001** MAY: a\n\n**Accept when:**\n- one\n- two\n\n<enforcement>\nEnforce.\n</enforcement>\n",
        );
        assert_eq!(
            by_enforcement.accept_when,
            vec!["one".to_string(), "two".to_string()]
        );

        let by_heading = parse_ok(
            "# T\n\n### Rules\n\n- **R-A-001** MAY: a\n\n**Accept when:**\n- one\n\n## Consequences\n\n- three\n",
        );
        assert_eq!(by_heading.accept_when, vec!["one".to_string()]);
    }

    #[test]
    fn test_parse_prose_inside_accept_when_is_dropped() {
        let doc = parse_ok(
            "# T\n\n### Rules\n\n- **R-A-001** MAY: a\n\n**Accept when:**\nA prose line.\n- one\n",
        );
        assert_eq!(doc.accept_when, vec!["one".to_string()]);
    }

    #[test]
    fn test_parse_accept_when_casing_variants() {
        for label in ["**Accept when:**", "**Accept When**", "**ACCEPT WHEN:**"] {
            let text = format!("# T\n\n### Rules\n\n- **R-A-001** MAY: a\n\n{label}\n- one\n");
            assert_eq!(
                parse_ok(&text).accept_when,
                vec!["one".to_string()],
                "{label}"
            );
        }
    }

    #[test]
    fn test_parse_inline_enforcement_tag() {
        let doc = parse_ok(
            "# T\n\n### Rules\n\n- **R-A-001** MAY: a\n\n<enforcement>Do not skip.</enforcement>\n",
        );
        assert_eq!(doc.enforcement.as_deref(), Some("Do not skip."));
        assert!(doc.warnings.is_empty());
    }

    #[test]
    fn test_parse_multi_line_enforcement_keeps_line_breaks() {
        let doc = parse_ok(
            "# T\n\n### Rules\n\n- **R-A-001** MAY: a\n\n<enforcement>\nFirst.\nSecond.\n</enforcement>\n",
        );
        assert_eq!(doc.enforcement.as_deref(), Some("First.\nSecond."));
    }

    // ── formatting variance ──────────────────────────────────────────────

    #[test]
    fn test_parse_file_without_a_trailing_newline() {
        let doc = parse_ok(
            "# T\n\n### Rules\n\n- **R-A-001** MAY: a\n\n<enforcement>\nEnforce.\n</enforcement>",
        );
        assert_eq!(doc.enforcement.as_deref(), Some("Enforce."));
    }

    #[test]
    fn test_parse_crlf_line_endings() {
        let doc = parse_ok("# T\r\n\r\nScope.\r\n\r\n### Rules\r\n\r\n- **R-A-001** MUST: a\r\n");
        assert_eq!(doc.title.as_deref(), Some("T"));
        assert_eq!(doc.scope.as_deref(), Some("Scope."));
        assert_eq!(ids(&doc), vec!["R-A-001"]);
    }

    #[test]
    fn test_parse_blank_lines_between_bullets_do_not_end_the_section() {
        let doc = parse_ok(
            "# T\n\n### Rules\n\n- **R-A-001** MUST: a\n\n- **R-A-002** SHOULD: b\n\n- **R-A-003** MAY: c\n",
        );
        assert_eq!(ids(&doc), vec!["R-A-001", "R-A-002", "R-A-003"]);
    }

    #[test]
    fn test_parse_non_ascii_content_does_not_panic() {
        let doc = parse_ok(
            "# Título ✓\n\nRègles ✗ actives ⚠.\n\n### Rules\n\n- **R-Ä-001** MUST: échapper ✓ correctement 🎉\n",
        );
        assert_eq!(doc.title.as_deref(), Some("Título ✓"));
        assert_eq!(doc.scope.as_deref(), Some("Règles ✗ actives ⚠."));
        assert_eq!(doc.rules[0].statement, "échapper ✓ correctement 🎉");
    }

    #[test]
    fn test_parse_hash_run_without_a_space_is_not_a_heading() {
        // `#hashtag` and a seven-hash run are prose, not headings, so neither
        // can claim the title.
        let doc = parse_ok(
            "####### deep\n\n#hashtag\n\n# Real Title\n\n### Rules\n\n- **R-A-001** MAY: a\n",
        );
        assert_eq!(doc.title.as_deref(), Some("Real Title"));
        assert_eq!(doc.scope.as_deref(), Some("####### deep #hashtag"));
    }

    #[test]
    fn test_parse_second_h1_does_not_replace_the_title() {
        let doc = parse_ok("# First\n\n# Second\n\n### Rules\n\n- **R-A-001** MAY: a\n");
        assert_eq!(doc.title.as_deref(), Some("First"));
    }

    // ── malformed input ──────────────────────────────────────────────────

    #[test]
    fn test_parse_error_empty_file() {
        for text in ["", "   \n\n\t\n"] {
            let err = parse(text).unwrap_err();
            assert_eq!(err.issue.kind, RuleIssueKind::Empty);
            assert_eq!(err.issue.line, None);
        }
    }

    #[test]
    fn test_parse_error_missing_rules_section() {
        let err = parse("# T\n\nScope.\n\n### Verify\n\n```bash\nls\n```\n").unwrap_err();
        assert_eq!(err.issue.kind, RuleIssueKind::MissingRulesSection);
        assert!(err.to_string().contains("no `### Rules` section"));
    }

    #[test]
    fn test_parse_error_rules_section_with_nothing_parseable() {
        let err = parse("# T\n\n### Rules\n\n- just prose, no rule id\n").unwrap_err();
        assert_eq!(err.issue.kind, RuleIssueKind::NoRules);
    }

    #[test]
    fn test_parse_unterminated_verify_fence_warns_and_keeps_the_rules() {
        let doc = parse_ok(
            "# T\n\n### Rules\n\n- **R-A-001** MAY: a\n\n### Verify\n\n```bash\nls\n",
        );
        assert_eq!(ids(&doc), vec!["R-A-001"]);
        assert_eq!(doc.verify.len(), 1);
        assert_eq!(doc.verify[0].lang.as_deref(), Some("bash"));
        assert_eq!(doc.verify[0].body, "ls");
        assert_eq!(doc.warnings.len(), 1);
        assert_eq!(doc.warnings[0].kind, RuleIssueKind::UnterminatedFence);
        assert_eq!(doc.warnings[0].line, Some(9));
        assert!(doc.warnings[0]
            .detail
            .contains("code fence opened but never closed"));
    }

    /// An unclosed fence in the preamble swallows `### Rules`, so the file is
    /// still skipped — with the fence's opening line, not a vaguer missing-section
    /// error.
    #[test]
    fn test_parse_error_unterminated_fence_before_any_rules() {
        let err = parse("# T\n\n```bash\n### Rules\n- **R-A-001** MAY: a\n").unwrap_err();
        assert_eq!(err.issue.kind, RuleIssueKind::UnterminatedFence);
        assert_eq!(err.issue.line, Some(3));
    }

    /// An unclosed fence under `### Rules` that ate every bullet is still the
    /// fence, not `NoRules`.
    #[test]
    fn test_parse_error_unterminated_fence_inside_rules_with_nothing_parsed() {
        let err = parse("# T\n\n### Rules\n\n```\n- **R-A-001** MAY: a\n").unwrap_err();
        assert_eq!(err.issue.kind, RuleIssueKind::UnterminatedFence);
        assert_eq!(err.issue.line, Some(5));
    }

    #[test]
    fn test_parse_unknown_level_warns_and_skips_only_that_rule() {
        let doc = parse_ok(
            "# T\n\n### Rules\n\n- **R-DICT-007** EXCEPTION: permitted for protocol fields.\n- **R-DICT-008** MUST: keep this one.\n",
        );
        assert_eq!(ids(&doc), vec!["R-DICT-008"]);
        assert_eq!(doc.warnings.len(), 1);
        assert_eq!(doc.warnings[0].kind, RuleIssueKind::UnknownLevel);
        assert_eq!(doc.warnings[0].line, Some(5));
        assert!(doc.warnings[0].detail.contains("R-DICT-007"));
    }

    #[test]
    fn test_parse_malformed_bullet_warns_but_bare_prose_does_not() {
        let doc = parse_ok(
            "# T\n\n### Rules\n\n\
             - **R-A-001** MUST: a\n\
             - a bullet that is not a rule\n\
             A bare prose line.\n\
             **Note:** an unbulleted bold aside with trailing text.\n",
        );
        assert_eq!(ids(&doc), vec!["R-A-001"]);
        assert_eq!(doc.warnings.len(), 1);
        assert_eq!(doc.warnings[0].kind, RuleIssueKind::MalformedRule);
        assert_eq!(doc.warnings[0].line, Some(6));
    }

    #[test]
    fn test_parse_empty_statement_warns() {
        let doc = parse_ok("# T\n\n### Rules\n\n- **R-A-001** MUST:\n- **R-A-002** MAY: b\n");
        assert_eq!(ids(&doc), vec!["R-A-002"]);
        assert_eq!(doc.warnings.len(), 1);
        assert_eq!(doc.warnings[0].kind, RuleIssueKind::EmptyStatement);
    }

    #[test]
    fn test_parse_unterminated_enforcement_warns_and_keeps_the_document() {
        let doc =
            parse_ok("# T\n\n### Rules\n\n- **R-A-001** MAY: a\n\n<enforcement>\nNever closed.\n");
        assert_eq!(doc.enforcement.as_deref(), Some("Never closed."));
        assert_eq!(doc.warnings.len(), 1);
        assert_eq!(doc.warnings[0].kind, RuleIssueKind::UnterminatedEnforcement);
        assert_eq!(doc.warnings[0].line, Some(7));
        assert_eq!(doc.rules.len(), 1);
    }

    /// Forces the module's `LazyLock<Regex>` so an invalid pattern fails here
    /// rather than during a production scan.
    #[test]
    fn test_static_regexes_all_initialize_without_panic() {
        assert!(rule_id_regex().is_match("**R-A-001** MUST: a"));
    }
}
