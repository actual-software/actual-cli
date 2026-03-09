use std::path::Path;

use crate::cli::commands::cr::types::AdrContext;
use crate::cli::commands::find_output_files;
use crate::error::ActualError;
use crate::generation::markers;
use crate::generation::OutputFormat;

/// Read ADR-managed content from the project's output files (CLAUDE.md, AGENTS.md, etc.).
///
/// Searches for output files across all supported formats, extracts managed
/// sections and per-ADR content, and strips metadata so the LLM only sees
/// policy text.
///
/// If no ADR content is found, warns the user and returns an empty context
/// rather than failing — the review lenses can still analyze the diff without
/// ADR grounding.
pub fn read_adr_context(root_dir: &Path) -> Result<AdrContext, ActualError> {
    // Search all formats for output files
    let formats = [
        OutputFormat::ClaudeMd,
        OutputFormat::AgentsMd,
        OutputFormat::CursorRules,
    ];

    for format in &formats {
        let files = find_output_files(root_dir, format);
        for file_path in &files {
            let content = match std::fs::read_to_string(file_path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("failed to read {}: {e}", file_path.display());
                    continue;
                }
            };

            if let Some(managed) = markers::extract_managed_content(&content) {
                let adr_sections = markers::extract_adr_sections(managed);
                let policies = markers::strip_managed_metadata(managed);

                return Ok(AdrContext {
                    policies,
                    adr_sections,
                    source_file: file_path.clone(),
                });
            }
        }
    }

    // No managed sections found in any output file
    eprintln!(
        "warning: No ADR policies found. Run `actual adr-bot` first for policy-aware review."
    );

    Ok(AdrContext {
        policies: String::new(),
        adr_sections: Vec::new(),
        source_file: root_dir.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn test_read_adr_context_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = read_adr_context(tmp.path()).unwrap();
        assert!(ctx.policies.is_empty());
        assert!(ctx.adr_sections.is_empty());
    }

    #[test]
    fn test_read_adr_context_with_managed_content() {
        let tmp = tempfile::tempdir().unwrap();
        let claude_md = tmp.path().join("CLAUDE.md");
        fs::write(
            &claude_md,
            "\
# Project Rules

Some preamble.

<!-- managed:actual-start -->
<!-- adr-ids: abc-123 -->
<!-- adr:abc-123 start -->
## Security Policy

Always validate input.
<!-- adr:abc-123 end -->
<!-- managed:actual-end -->

# Manual Section
",
        )
        .unwrap();

        let ctx = read_adr_context(tmp.path()).unwrap();
        assert!(!ctx.policies.is_empty());
        assert!(ctx.policies.contains("Always validate input"));
        // Metadata should be stripped
        assert!(!ctx.policies.contains("adr-ids:"));
        assert!(!ctx.policies.contains("managed:actual"));
        assert_eq!(ctx.adr_sections.len(), 1);
        assert_eq!(ctx.adr_sections[0].id, "abc-123");
        assert_eq!(ctx.source_file, claude_md);
    }

    #[test]
    fn test_read_adr_context_no_managed_section() {
        let tmp = tempfile::tempdir().unwrap();
        let claude_md = tmp.path().join("CLAUDE.md");
        fs::write(
            &claude_md,
            "# Just a plain file\nNo managed sections here.\n",
        )
        .unwrap();

        let ctx = read_adr_context(tmp.path()).unwrap();
        assert!(ctx.policies.is_empty());
        assert!(ctx.adr_sections.is_empty());
    }
}
