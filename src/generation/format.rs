use serde::{Deserialize, Serialize};

/// The output file format for generated policy content.
///
/// Controls the filename written to disk (`CLAUDE.md` vs `AGENTS.md`) and
/// how the discovery, validation, and prompt steps refer to that file.  The
/// markers themselves (`<!-- managed:actual-start -->` / `<!-- managed:actual-end -->`)
/// are identical for both formats — HTML comments are invisible in rendered
/// markdown and do not interfere with Codex CLI parsing.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat {
    /// Generate `CLAUDE.md` (default).
    #[default]
    ClaudeMd,
    /// Generate `AGENTS.md` (for Codex CLI and other tools that read AGENTS.md).
    AgentsMd,
}

impl OutputFormat {
    /// The filename this format writes to.
    pub fn filename(&self) -> &str {
        match self {
            OutputFormat::ClaudeMd => "CLAUDE.md",
            OutputFormat::AgentsMd => "AGENTS.md",
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

    /// The header prepended to new root-level files.
    pub fn root_header(&self) -> Option<&str> {
        Some("# Project Guidelines")
    }
}

impl clap::ValueEnum for OutputFormat {
    fn value_variants<'a>() -> &'a [Self] {
        &[OutputFormat::ClaudeMd, OutputFormat::AgentsMd]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(match self {
            OutputFormat::ClaudeMd => clap::builder::PossibleValue::new("claude-md"),
            OutputFormat::AgentsMd => clap::builder::PossibleValue::new("agents-md"),
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
    fn test_start_marker_same_for_both() {
        assert_eq!(OutputFormat::ClaudeMd.start_marker(), START_MARKER);
        assert_eq!(OutputFormat::AgentsMd.start_marker(), START_MARKER);
    }

    #[test]
    fn test_end_marker_same_for_both() {
        assert_eq!(OutputFormat::ClaudeMd.end_marker(), END_MARKER);
        assert_eq!(OutputFormat::AgentsMd.end_marker(), END_MARKER);
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
    fn test_default_is_claude_md() {
        assert_eq!(OutputFormat::default(), OutputFormat::ClaudeMd);
    }

    #[test]
    fn test_root_header_present() {
        assert_eq!(
            OutputFormat::ClaudeMd.root_header(),
            Some("# Project Guidelines")
        );
        assert_eq!(
            OutputFormat::AgentsMd.root_header(),
            Some("# Project Guidelines")
        );
    }
}
