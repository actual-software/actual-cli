//! Typed model for the rule documents under `.actual/rules/`.
//!
//! # Design
//!
//! A rule document is markdown that a coding agent is expected to obey. The
//! governing structure is small — a title, a prose applicability sentence, a
//! list of RFC-2119 rule statements, a verification block, acceptance criteria,
//! and an enforcement note — so it is modelled as plain data with public fields,
//! the same shape [`crate::api::types::Adr`] uses.
//!
//! Two decisions here are load-bearing:
//!
//! * [`RuleLevel`] is the *only* place that knows RFC-2119 vocabulary. Its
//!   keyword table is ordered longest-first so `MUST NOT` can never be
//!   shortened to `MUST` — the failure mode that silently flattens a whole
//!   corpus to all-MUST.
//! * A document distinguishes *fatal* problems (returned as a
//!   [`RuleFileError`], the file is skipped) from *non-fatal* ones (recorded in
//!   [`RuleDocument::warnings`], the document is still usable). Losing one
//!   malformed bullet is better than losing the eight good rules beside it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// An RFC-2119 normative level, normalized to five canonical spellings.
///
/// The serde representation is `SCREAMING_SNAKE_CASE`, so the normalization
/// contract (`MUST NOT` becomes `MUST_NOT`) and the wire contract are the same
/// thing and can be pinned by a single assertion.
///
/// Declaration order is strength order — `Must` is the strongest — so the
/// derived [`Ord`] sorts strongest-first for downstream ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RuleLevel {
    Must,
    MustNot,
    Should,
    ShouldNot,
    May,
}

/// RFC-2119 keywords and their synonyms, **ordered longest-first**.
///
/// The ordering is the whole defence against `MUST NOT` being read as `MUST`:
/// [`RuleLevel::split_prefix`] returns the first entry that matches, so a
/// longer phrase always wins over the shorter phrase it starts with. Adding a
/// keyword means inserting it in length order, not appending it.
const LEVEL_KEYWORDS: &[(&str, RuleLevel)] = &[
    ("NOT RECOMMENDED", RuleLevel::ShouldNot),
    ("RECOMMENDED", RuleLevel::Should),
    ("SHOULD NOT", RuleLevel::ShouldNot),
    ("SHOULD_NOT", RuleLevel::ShouldNot),
    ("SHALL NOT", RuleLevel::MustNot),
    ("SHALL_NOT", RuleLevel::MustNot),
    ("MUST NOT", RuleLevel::MustNot),
    ("MUST_NOT", RuleLevel::MustNot),
    ("OPTIONAL", RuleLevel::May),
    ("REQUIRED", RuleLevel::Must),
    ("SHOULD", RuleLevel::Should),
    ("SHALL", RuleLevel::Must),
    ("MUST", RuleLevel::Must),
    ("MAY", RuleLevel::May),
];

impl RuleLevel {
    /// The canonical spelling: `MUST`, `MUST_NOT`, `SHOULD`, `SHOULD_NOT`, `MAY`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Must => "MUST",
            Self::MustNot => "MUST_NOT",
            Self::Should => "SHOULD",
            Self::ShouldNot => "SHOULD_NOT",
            Self::May => "MAY",
        }
    }

    /// Parse a token that is expected to be a level keyword and nothing else.
    ///
    /// Applies the same synonym folding as the MkII TypeScript parser:
    /// `REQUIRED`/`SHALL` become `MUST`, `SHALL NOT` becomes `MUST_NOT`,
    /// `RECOMMENDED` becomes `SHOULD`, `NOT RECOMMENDED` becomes `SHOULD_NOT`,
    /// and `OPTIONAL` becomes `MAY`.
    pub fn parse(token: &str) -> Option<Self> {
        match Self::split_prefix(token) {
            Some((level, "")) => Some(level),
            _ => None,
        }
    }

    /// Match a level keyword at the start of `tail` and return it with the text
    /// that follows.
    ///
    /// The keyword must be a whole token — it has to be followed by end of
    /// input, a colon, or whitespace — so `MAYBE` does not match `MAY`. A
    /// single separating colon is consumed, and the remainder is trimmed.
    ///
    /// This is a prefix match rather than a "split on the first colon" because
    /// the corpus contains bullets like `MUST NOT apply to: ...`, where the
    /// colon belongs to the statement rather than to the level.
    pub fn split_prefix(tail: &str) -> Option<(Self, &str)> {
        let tail = tail.trim_start();
        for (keyword, level) in LEVEL_KEYWORDS {
            let width = keyword.len();
            if tail.len() < width {
                continue;
            }
            // A case-insensitive ASCII match can only succeed when the first
            // `width` bytes are themselves ASCII, so `width` is guaranteed to be
            // a char boundary and the slicing below cannot panic.
            if !tail.as_bytes()[..width].eq_ignore_ascii_case(keyword.as_bytes()) {
                continue;
            }
            let rest = &tail[width..];
            if !(rest.is_empty() || rest.starts_with(':') || rest.starts_with(char::is_whitespace))
            {
                continue;
            }
            let rest = rest.trim_start();
            let rest = rest.strip_prefix(':').unwrap_or(rest);
            return Some((*level, rest.trim()));
        }
        None
    }
}

/// One `**R-XXX-NNN** LEVEL: statement` rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// The rule id exactly as written between the `**` delimiters.
    ///
    /// Deliberately unvalidated: the corpus contains `R-JWT-001`,
    /// `R-ACTIVITY-INPUT-001`, `R-46-006` and `EXC-001`, so any pattern
    /// stricter than "whatever was in the bold span" would silently drop rules.
    pub id: String,
    pub level: RuleLevel,
    /// The statement text, trimmed.
    pub statement: String,
    /// 1-based line in the source file where this rule was found.
    pub line: usize,
}

/// One fenced block captured from the `### Verify` section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyBlock {
    /// The info string after the opening fence (`bash`), or `None` for a bare fence.
    pub lang: Option<String>,
    /// Fence contents, verbatim, without a trailing newline.
    pub body: String,
    /// 1-based line of the opening fence.
    pub line: usize,
}

/// A single parsed `.actual/rules/*.md` file.
///
/// Every field except `rules` is optional. Real rule files omit sections
/// freely, and a missing section is not a parse failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleDocument {
    /// The file this document was read from.
    pub source_path: PathBuf,
    /// The `# <Title>` heading.
    pub title: Option<String>,
    /// The prose applicability sentence that follows the title.
    pub scope: Option<String>,
    pub rules: Vec<Rule>,
    /// Fenced blocks under `### Verify`, in document order.
    pub verify: Vec<VerifyBlock>,
    /// Bullets under `**Accept when:**`.
    pub accept_when: Vec<String>,
    /// Text between `<enforcement>` and `</enforcement>`.
    pub enforcement: Option<String>,
    /// Non-fatal problems: a dropped bullet, an unrecognized level keyword.
    /// The document is still usable.
    pub warnings: Vec<RuleIssue>,
}

impl RuleDocument {
    /// An empty document attributed to `path`.
    pub fn empty(path: &Path) -> Self {
        Self {
            source_path: path.to_path_buf(),
            title: None,
            scope: None,
            rules: Vec::new(),
            verify: Vec::new(),
            accept_when: Vec::new(),
            enforcement: None,
            warnings: Vec::new(),
        }
    }

    /// The file stem — `<topic>-<aspect-slug>-<hash>` — which is the document's
    /// stable identity for the downstream scope index.
    pub fn slug(&self) -> Option<&str> {
        self.source_path.file_stem().and_then(|stem| stem.to_str())
    }
}

/// What went wrong, structurally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleIssueKind {
    // ── file level, raised while reading ──
    Io,
    TooLarge,
    NotUtf8,
    // ── fatal parse failures, the file is skipped ──
    Empty,
    MissingRulesSection,
    NoRules,
    // ── non-fatal, recorded on the document ──
    // Promoted to a file error when the scan is left with no parseable rules.
    UnterminatedFence,
    UnknownLevel,
    MalformedRule,
    EmptyStatement,
    UnterminatedEnforcement,
}

/// A structured problem, located at a line where that is meaningful.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleIssue {
    pub kind: RuleIssueKind,
    /// 1-based source line, when the problem is localizable.
    pub line: Option<usize>,
    /// Human-readable detail. I/O failures render their `std::io::Error` into
    /// this string rather than storing it, which keeps `RuleIssue` comparable
    /// so tests can assert on whole issues.
    pub detail: String,
}

impl std::fmt::Display for RuleIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(line) => write!(f, "line {line}: {}", self.detail),
            None => write!(f, "{}", self.detail),
        }
    }
}

/// A per-file failure. One file failing never affects any other file.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{}: {issue}", path.display())]
pub struct RuleFileError {
    pub path: PathBuf,
    pub issue: RuleIssue,
}

impl RuleFileError {
    pub fn new(
        path: &Path,
        kind: RuleIssueKind,
        line: Option<usize>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            path: path.to_path_buf(),
            issue: RuleIssue {
                kind,
                line,
                detail: detail.into(),
            },
        }
    }
}

/// The result of loading a whole `.actual/rules/` directory: what parsed, and
/// what did not. Both lists are ordered by path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleSetLoadReport {
    /// The directory that was scanned. It need not exist.
    pub rules_dir: PathBuf,
    pub documents: Vec<RuleDocument>,
    pub errors: Vec<RuleFileError>,
}

impl RuleSetLoadReport {
    /// Total rules across all successfully parsed documents.
    pub fn rule_count(&self) -> usize {
        self.documents.iter().map(|doc| doc.rules.len()).sum()
    }

    /// Total non-fatal warnings across all successfully parsed documents.
    pub fn warning_count(&self) -> usize {
        self.documents.iter().map(|doc| doc.warnings.len()).sum()
    }

    /// True when nothing parsed and nothing failed — no rules directory, or an
    /// empty one.
    pub fn is_empty(&self) -> bool {
        self.documents.is_empty() && self.errors.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_LEVELS: &[RuleLevel] = &[
        RuleLevel::Must,
        RuleLevel::MustNot,
        RuleLevel::Should,
        RuleLevel::ShouldNot,
        RuleLevel::May,
    ];

    /// Helper: a rule with the given id and level.
    fn make_rule(id: &str, level: RuleLevel) -> Rule {
        Rule {
            id: id.to_string(),
            level,
            statement: "a statement".to_string(),
            line: 7,
        }
    }

    /// Helper: a document at `path` carrying `rules` rules and `warnings` warnings.
    fn make_doc(path: &str, rules: usize, warnings: usize) -> RuleDocument {
        let mut doc = RuleDocument::empty(Path::new(path));
        for i in 0..rules {
            doc.rules
                .push(make_rule(&format!("R-X-{i:03}"), RuleLevel::Must));
        }
        for _ in 0..warnings {
            doc.warnings.push(RuleIssue {
                kind: RuleIssueKind::UnknownLevel,
                line: Some(1),
                detail: "unknown".to_string(),
            });
        }
        doc
    }

    // ── RuleLevel ────────────────────────────────────────────────────────

    #[test]
    fn test_rule_level_parse_canonical_spellings() {
        assert_eq!(RuleLevel::parse("MUST"), Some(RuleLevel::Must));
        assert_eq!(RuleLevel::parse("MUST NOT"), Some(RuleLevel::MustNot));
        assert_eq!(RuleLevel::parse("MUST_NOT"), Some(RuleLevel::MustNot));
        assert_eq!(RuleLevel::parse("SHOULD"), Some(RuleLevel::Should));
        assert_eq!(RuleLevel::parse("SHOULD NOT"), Some(RuleLevel::ShouldNot));
        assert_eq!(RuleLevel::parse("SHOULD_NOT"), Some(RuleLevel::ShouldNot));
        assert_eq!(RuleLevel::parse("MAY"), Some(RuleLevel::May));
    }

    #[test]
    fn test_rule_level_parse_rfc2119_synonyms() {
        assert_eq!(RuleLevel::parse("REQUIRED"), Some(RuleLevel::Must));
        assert_eq!(RuleLevel::parse("SHALL"), Some(RuleLevel::Must));
        assert_eq!(RuleLevel::parse("SHALL NOT"), Some(RuleLevel::MustNot));
        assert_eq!(RuleLevel::parse("SHALL_NOT"), Some(RuleLevel::MustNot));
        assert_eq!(RuleLevel::parse("RECOMMENDED"), Some(RuleLevel::Should));
        assert_eq!(
            RuleLevel::parse("NOT RECOMMENDED"),
            Some(RuleLevel::ShouldNot)
        );
        assert_eq!(RuleLevel::parse("OPTIONAL"), Some(RuleLevel::May));
    }

    #[test]
    fn test_rule_level_parse_is_case_insensitive_and_trims() {
        assert_eq!(RuleLevel::parse("must"), Some(RuleLevel::Must));
        assert_eq!(RuleLevel::parse("Must Not"), Some(RuleLevel::MustNot));
        assert_eq!(RuleLevel::parse("  may"), Some(RuleLevel::May));
    }

    #[test]
    fn test_rule_level_parse_rejects_unknown() {
        assert_eq!(RuleLevel::parse(""), None);
        assert_eq!(RuleLevel::parse("EXCEPTION"), None);
        assert_eq!(RuleLevel::parse("Exception"), None);
        assert_eq!(RuleLevel::parse("MUSTNT"), None);
        // A keyword followed by extra words is not a bare level token.
        assert_eq!(RuleLevel::parse("MUST NOT NOT"), None);
    }

    #[test]
    fn test_rule_level_as_str_round_trips() {
        for level in ALL_LEVELS {
            assert_eq!(RuleLevel::parse(level.as_str()), Some(*level));
        }
        assert_eq!(RuleLevel::MustNot.as_str(), "MUST_NOT");
        assert_eq!(RuleLevel::ShouldNot.as_str(), "SHOULD_NOT");
    }

    #[test]
    fn test_rule_level_split_prefix_consumes_colon_and_trims() {
        assert_eq!(
            RuleLevel::split_prefix("MUST: do the thing  "),
            Some((RuleLevel::Must, "do the thing"))
        );
        assert_eq!(
            RuleLevel::split_prefix("SHOULD do the thing"),
            Some((RuleLevel::Should, "do the thing"))
        );
        assert_eq!(RuleLevel::split_prefix("MAY"), Some((RuleLevel::May, "")));
    }

    /// The regression that matters most: `MUST NOT` must never be shortened to
    /// `MUST`, in any of its spellings. Getting this wrong flattens a whole
    /// corpus to all-MUST while every individual rule still looks parsed.
    #[test]
    fn test_rule_level_split_prefix_must_not_is_not_must() {
        for raw in ["MUST NOT: never", "MUST_NOT: never", "must not: never"] {
            let (level, statement) = RuleLevel::split_prefix(raw).unwrap();
            assert_eq!(level, RuleLevel::MustNot, "input: {raw}");
            assert_eq!(statement, "never");
        }
        for raw in ["SHOULD NOT: avoid", "SHOULD_NOT: avoid"] {
            let (level, statement) = RuleLevel::split_prefix(raw).unwrap();
            assert_eq!(level, RuleLevel::ShouldNot, "input: {raw}");
            assert_eq!(statement, "avoid");
        }
    }

    /// The corpus contains `MUST NOT apply to: Internal configuration ...`,
    /// where the first colon belongs to the statement, not the level.
    #[test]
    fn test_rule_level_split_prefix_keyword_followed_by_prose_and_colon() {
        assert_eq!(
            RuleLevel::split_prefix("MUST NOT apply to: Internal configuration"),
            Some((RuleLevel::MustNot, "apply to: Internal configuration"))
        );
    }

    #[test]
    fn test_rule_level_split_prefix_requires_whole_token() {
        // `MAYBE` starts with `MAY` but is not a level.
        assert_eq!(RuleLevel::split_prefix("MAYBE: perhaps"), None);
        assert_eq!(RuleLevel::split_prefix("MUSTARD: yellow"), None);
        // Shorter than the shortest keyword.
        assert_eq!(RuleLevel::split_prefix("A"), None);
        assert_eq!(RuleLevel::split_prefix(""), None);
        // A multi-byte character where a keyword's bytes would end.
        assert_eq!(RuleLevel::split_prefix("MA✓ something"), None);
    }

    #[test]
    fn test_rule_level_ord_is_strength_order() {
        assert!(RuleLevel::Must < RuleLevel::MustNot);
        assert!(RuleLevel::MustNot < RuleLevel::Should);
        assert!(RuleLevel::Should < RuleLevel::ShouldNot);
        assert!(RuleLevel::ShouldNot < RuleLevel::May);
    }

    #[test]
    fn test_rule_level_serde_uses_screaming_snake_case() {
        assert_eq!(
            serde_json::to_string(&RuleLevel::MustNot).unwrap(),
            "\"MUST_NOT\""
        );
        for level in ALL_LEVELS {
            let json = serde_json::to_string(level).unwrap();
            assert_eq!(json, format!("\"{}\"", level.as_str()));
            let back: RuleLevel = serde_json::from_str(&json).unwrap();
            assert_eq!(back, *level);
        }
    }

    #[test]
    fn test_rule_level_is_hashable_and_clonable() {
        let mut seen = std::collections::HashSet::new();
        for level in ALL_LEVELS {
            assert!(seen.insert(*level));
        }
        let copy = RuleLevel::Must;
        assert_eq!(format!("{copy:?}"), "Must");
    }

    // ── RuleDocument ─────────────────────────────────────────────────────

    #[test]
    fn test_rule_document_empty_has_only_the_path() {
        let doc = RuleDocument::empty(Path::new("/tmp/a.md"));
        assert_eq!(doc.source_path, PathBuf::from("/tmp/a.md"));
        assert!(doc.title.is_none());
        assert!(doc.scope.is_none());
        assert!(doc.rules.is_empty());
        assert!(doc.verify.is_empty());
        assert!(doc.accept_when.is_empty());
        assert!(doc.enforcement.is_none());
        assert!(doc.warnings.is_empty());
    }

    #[test]
    fn test_rule_document_slug_is_the_file_stem() {
        let doc = RuleDocument::empty(Path::new("/x/cross-cutting-tokens-e410.md"));
        assert_eq!(doc.slug(), Some("cross-cutting-tokens-e410"));
    }

    #[test]
    fn test_rule_document_slug_is_none_without_a_stem() {
        let doc = RuleDocument::empty(Path::new(".."));
        assert_eq!(doc.slug(), None);
    }

    #[test]
    fn test_rule_document_serde_round_trip() {
        let mut doc = make_doc("/x/a.md", 1, 1);
        doc.title = Some("A title".to_string());
        doc.scope = Some("These rules are ALWAYS ACTIVE.".to_string());
        doc.verify.push(VerifyBlock {
            lang: Some("bash".to_string()),
            body: "grep -r foo .".to_string(),
            line: 12,
        });
        doc.accept_when.push("it works".to_string());
        doc.enforcement = Some("Claude Code MUST verify.".to_string());

        let json = serde_json::to_string(&doc).unwrap();
        let back: RuleDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(back, doc);
        assert_eq!(back.clone(), doc);
        assert!(format!("{doc:?}").contains("A title"));
    }

    // ── issues and errors ────────────────────────────────────────────────

    #[test]
    fn test_rule_issue_display_with_and_without_a_line() {
        let located = RuleIssue {
            kind: RuleIssueKind::UnknownLevel,
            line: Some(4),
            detail: "unrecognized level `EXCEPTION`".to_string(),
        };
        assert_eq!(
            located.to_string(),
            "line 4: unrecognized level `EXCEPTION`"
        );

        let unlocated = RuleIssue {
            kind: RuleIssueKind::NoRules,
            line: None,
            detail: "no rules".to_string(),
        };
        assert_eq!(unlocated.to_string(), "no rules");
    }

    #[test]
    fn test_rule_issue_kind_serde_is_snake_case() {
        assert_eq!(
            serde_json::to_string(&RuleIssueKind::MissingRulesSection).unwrap(),
            "\"missing_rules_section\""
        );
        let back: RuleIssueKind = serde_json::from_str("\"unterminated_fence\"").unwrap();
        assert_eq!(back, RuleIssueKind::UnterminatedFence);
    }

    #[test]
    fn test_rule_file_error_display_includes_the_path() {
        let err = RuleFileError::new(
            Path::new("/x/bad.md"),
            RuleIssueKind::Empty,
            None,
            "file is empty",
        );
        assert_eq!(err.to_string(), "/x/bad.md: file is empty");
        assert_eq!(err.issue.kind, RuleIssueKind::Empty);
        assert_eq!(err.clone(), err);

        let located = RuleFileError::new(
            Path::new("/x/bad.md"),
            RuleIssueKind::UnterminatedFence,
            Some(9),
            "fence never closed",
        );
        assert_eq!(located.to_string(), "/x/bad.md: line 9: fence never closed");
    }

    // ── RuleSetLoadReport ────────────────────────────────────────────────

    #[test]
    fn test_report_counts_rules_and_warnings_across_documents() {
        let report = RuleSetLoadReport {
            rules_dir: PathBuf::from("/x/.actual/rules"),
            documents: vec![make_doc("/x/a.md", 3, 1), make_doc("/x/b.md", 2, 0)],
            errors: Vec::new(),
        };
        assert_eq!(report.rule_count(), 5);
        assert_eq!(report.warning_count(), 1);
        assert!(!report.is_empty());
    }

    #[test]
    fn test_report_is_empty_only_without_documents_or_errors() {
        let empty = RuleSetLoadReport::default();
        assert!(empty.is_empty());
        assert_eq!(empty.rule_count(), 0);
        assert_eq!(empty.warning_count(), 0);

        let only_errors = RuleSetLoadReport {
            rules_dir: PathBuf::from("/x"),
            documents: Vec::new(),
            errors: vec![RuleFileError::new(
                Path::new("/x/bad.md"),
                RuleIssueKind::NotUtf8,
                None,
                "not utf-8",
            )],
        };
        assert!(!only_errors.is_empty());
        assert_eq!(only_errors.clone(), only_errors);
        assert!(format!("{only_errors:?}").contains("bad.md"));
    }
}
