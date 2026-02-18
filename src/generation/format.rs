use serde::{Deserialize, Serialize};

/// The YAML frontmatter header prepended to new root-level `.cursor/rules/actual-policies.mdc` files.
///
/// Cursor reads this to determine the rule type.  `alwaysApply: true` means the
/// rule is included in every chat session — the closest semantic equivalent to
/// CLAUDE.md / AGENTS.md (always-present context).
pub const CURSOR_RULES_FRONTMATTER: &str = "\
---
description: actual.ai ADR policies for this project
alwaysApply: true
---";

/// The filename (basename only) for the Cursor rules output file.
pub const CURSOR_RULES_FILENAME: &str = "actual-policies.mdc";

/// The parent directory path (relative to project root) where Cursor rules live.
pub const CURSOR_RULES_DIR: &str = ".cursor/rules";

/// The full relative path for the Cursor rules output file.
pub const CURSOR_RULES_PATH: &str = ".cursor/rules/actual-policies.mdc";

/// The output file format for generated policy content.
///
/// Controls the filename written to disk (`CLAUDE.md`, `AGENTS.md`, or
/// `.cursor/rules/actual-policies.mdc`) and how the discovery, validation, and
/// prompt steps refer to that file.  The managed-section markers
/// (`<!-- managed:actual-start -->` / `<!-- managed:actual-end -->`) are
/// identical for all formats — HTML comments are invisible in rendered markdown
/// and do not interfere with any of the supported tools.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat {
    /// Generate `CLAUDE.md` (default).
    #[default]
    ClaudeMd,
    /// Generate `AGENTS.md` (for Codex CLI and other tools that read AGENTS.md).
    AgentsMd,
    /// Generate `.cursor/rules/actual-policies.mdc` (for Cursor IDE).
    ///
    /// The file uses YAML frontmatter (`alwaysApply: true`) followed by
    /// markdown content inside the standard managed-section markers.
    CursorRules,
}

impl OutputFormat {
    /// The filename (basename) this format writes to.
    ///
    /// For `CursorRules` this returns the full relative path
    /// `.cursor/rules/actual-policies.mdc` so callers that compare against the
    /// path suffix work correctly.
    pub fn filename(&self) -> &str {
        match self {
            OutputFormat::ClaudeMd => "CLAUDE.md",
            OutputFormat::AgentsMd => "AGENTS.md",
            OutputFormat::CursorRules => CURSOR_RULES_PATH,
        }
    }

    /// The managed-section start marker (identical for all formats).
    pub fn start_marker(&self) -> &str {
        crate::generation::markers::START_MARKER
    }

    /// The managed-section end marker (identical for all formats).
    pub fn end_marker(&self) -> &str {
        crate::generation::markers::END_MARKER
    }

    /// The header prepended to new root-level files, or `None` if no header
    /// should be added.
    ///
    /// - `ClaudeMd` / `AgentsMd`: `# Project Guidelines`
    /// - `CursorRules`: YAML frontmatter block (`---\nalwaysApply: true\n---`)
    pub fn root_header(&self) -> Option<&str> {
        match self {
            OutputFormat::ClaudeMd | OutputFormat::AgentsMd => Some("# Project Guidelines"),
            OutputFormat::CursorRules => Some(CURSOR_RULES_FRONTMATTER),
        }
    }
}

impl clap::ValueEnum for OutputFormat {
    fn value_variants<'a>() -> &'a [Self] {
        &[
            OutputFormat::ClaudeMd,
            OutputFormat::AgentsMd,
            OutputFormat::CursorRules,
        ]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(match self {
            OutputFormat::ClaudeMd => clap::builder::PossibleValue::new("claude-md"),
            OutputFormat::AgentsMd => clap::builder::PossibleValue::new("agents-md"),
            OutputFormat::CursorRules => clap::builder::PossibleValue::new("cursor-rules"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::markers::{END_MARKER, START_MARKER};

    #[test]
    fn test_claude_md_filename() {
        assert_eq!(OutputFormat::ClaudeMd.filename(), "CLAUDE.md");
    }

    #[test]
    fn test_agents_md_filename() {
        assert_eq!(OutputFormat::AgentsMd.filename(), "AGENTS.md");
    }

    #[test]
    fn test_cursor_rules_filename() {
        assert_eq!(
            OutputFormat::CursorRules.filename(),
            ".cursor/rules/actual-policies.mdc"
        );
    }

    #[test]
    fn test_cursor_rules_filename_matches_constant() {
        assert_eq!(OutputFormat::CursorRules.filename(), CURSOR_RULES_PATH);
    }

    #[test]
    fn test_start_marker_same_for_all() {
        assert_eq!(OutputFormat::ClaudeMd.start_marker(), START_MARKER);
        assert_eq!(OutputFormat::AgentsMd.start_marker(), START_MARKER);
        assert_eq!(OutputFormat::CursorRules.start_marker(), START_MARKER);
    }

    #[test]
    fn test_end_marker_same_for_all() {
        assert_eq!(OutputFormat::ClaudeMd.end_marker(), END_MARKER);
        assert_eq!(OutputFormat::AgentsMd.end_marker(), END_MARKER);
        assert_eq!(OutputFormat::CursorRules.end_marker(), END_MARKER);
    }

    #[test]
    fn test_serde_claude_md_roundtrip() {
        let json = serde_json::to_string(&OutputFormat::ClaudeMd).unwrap();
        assert_eq!(json, "\"claude-md\"");
        let back: OutputFormat = serde_json::from_str(&json).unwrap();
        assert_eq!(back, OutputFormat::ClaudeMd);
    }

    #[test]
    fn test_serde_agents_md_roundtrip() {
        let json = serde_json::to_string(&OutputFormat::AgentsMd).unwrap();
        assert_eq!(json, "\"agents-md\"");
        let back: OutputFormat = serde_json::from_str(&json).unwrap();
        assert_eq!(back, OutputFormat::AgentsMd);
    }

    #[test]
    fn test_serde_cursor_rules_roundtrip() {
        let json = serde_json::to_string(&OutputFormat::CursorRules).unwrap();
        assert_eq!(json, "\"cursor-rules\"");
        let back: OutputFormat = serde_json::from_str(&json).unwrap();
        assert_eq!(back, OutputFormat::CursorRules);
    }

    #[test]
    fn test_default_is_claude_md() {
        assert_eq!(OutputFormat::default(), OutputFormat::ClaudeMd);
    }

    #[test]
    fn test_root_header_claude_md() {
        assert_eq!(
            OutputFormat::ClaudeMd.root_header(),
            Some("# Project Guidelines")
        );
    }

    #[test]
    fn test_root_header_agents_md() {
        assert_eq!(
            OutputFormat::AgentsMd.root_header(),
            Some("# Project Guidelines")
        );
    }

    #[test]
    fn test_root_header_cursor_rules_is_frontmatter() {
        let header = OutputFormat::CursorRules.root_header();
        assert!(header.is_some(), "CursorRules must have a root header");
        let h = header.unwrap();
        assert!(
            h.contains("alwaysApply: true"),
            "header must contain alwaysApply: true"
        );
        assert!(
            h.starts_with("---"),
            "header must start with YAML front-matter delimiter"
        );
    }

    #[test]
    fn test_cursor_rules_frontmatter_constant() {
        assert!(CURSOR_RULES_FRONTMATTER.contains("alwaysApply: true"));
        assert!(CURSOR_RULES_FRONTMATTER.contains("description:"));
        assert!(CURSOR_RULES_FRONTMATTER.starts_with("---"));
        assert!(CURSOR_RULES_FRONTMATTER.ends_with("---"));
    }

    #[test]
    fn test_cursor_rules_path_constant() {
        assert_eq!(CURSOR_RULES_PATH, ".cursor/rules/actual-policies.mdc");
    }

    #[test]
    fn test_cursor_rules_dir_constant() {
        assert_eq!(CURSOR_RULES_DIR, ".cursor/rules");
    }

    #[test]
    fn test_cursor_rules_filename_constant() {
        assert_eq!(CURSOR_RULES_FILENAME, "actual-policies.mdc");
    }
}
