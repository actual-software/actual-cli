//! Reader and parser for the rule documents under `.actual/rules/`.
//!
//! # Design
//!
//! Three layers, split so each one can be tested — and covered — on its own:
//!
//! * [`types`] is the data model and the only place that knows RFC-2119.
//! * [`parse`] turns one file's text into a [`RuleDocument`]. It performs no
//!   I/O, so it can be exercised entirely from string fixtures, and a caller
//!   holding rule text from elsewhere can use it directly.
//! * [`discover`] finds `.actual/rules/*.md` under an injected repository root
//!   and reads each file, isolating per-file failures.
//! * [`scope`] indexes the parsed set so a plan can be matched against it
//!   offline, which is the layer that replaces reading filenames by eye.

pub mod discover;
pub mod parse;
pub mod scope;
pub mod types;

pub use discover::{
    load_rule_set, parse_rule_sources, read_rule_sources, rules_dir, RuleSource, RuleSources,
    MAX_RULE_FILE_SIZE, RULES_DIR_NAME,
};
pub use parse::parse_rule_document;
pub use scope::{resolve, IndexSource, Match, Query, ResolvedIndex, ScopeIndex};
pub use types::{
    Rule, RuleDocument, RuleFileError, RuleIssue, RuleIssueKind, RuleLevel, RuleSetLoadReport,
    VerifyBlock,
};
