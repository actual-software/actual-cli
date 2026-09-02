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

pub mod discover;
pub mod parse;
pub mod types;

pub use discover::{load_rule_set, rules_dir, MAX_RULE_FILE_SIZE, RULES_DIR_NAME};
pub use parse::parse_rule_document;
pub use types::{
    Rule, RuleDocument, RuleFileError, RuleIssue, RuleIssueKind, RuleLevel, RuleSetLoadReport,
    VerifyBlock,
};
