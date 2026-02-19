use super::obfuscation::{cursor_rules_prompt_decoded, tailoring_prompt_decoded};
use crate::error::ActualError;

/// Returns a tailoring prompt with the given repository context, existing output file
/// paths, and ADR JSON array interpolated into the template.
///
/// The static instruction text is stored obfuscated in the binary (see `build.rs` and
/// `src/claude/obfuscation.rs`). It is decoded at runtime and then formatted with the
/// provided arguments.
///
/// The `format` parameter controls which filename (e.g. `CLAUDE.md`, `AGENTS.md`, or
/// `.cursor/rules/actual-policies.mdc`) is referenced inside the prompt so the LLM
/// targets the correct output file and uses the correct file structure.
///
/// Returns `Err(ActualError::InternalError)` if the obfuscated constant is malformed
/// (e.g. due to a key/build-artifact mismatch).
pub fn tailoring_prompt(
    projects_json: &str,
    existing_output_paths: &str,
    adr_json_array: &str,
    format: &crate::generation::OutputFormat,
) -> Result<String, ActualError> {
    use crate::generation::OutputFormat;
    match format {
        OutputFormat::CursorRules => {
            cursor_rules_tailoring_prompt(projects_json, existing_output_paths, adr_json_array)
        }
        _ => {
            standard_tailoring_prompt(projects_json, existing_output_paths, adr_json_array, format)
        }
    }
}

/// Standard tailoring prompt for `ClaudeMd` and `AgentsMd` formats.
fn standard_tailoring_prompt(
    projects_json: &str,
    existing_output_paths: &str,
    adr_json_array: &str,
    format: &crate::generation::OutputFormat,
) -> Result<String, ActualError> {
    let filename = format.filename();
    tailoring_prompt_decoded(
        projects_json,
        existing_output_paths,
        adr_json_array,
        filename,
    )
}

/// Tailoring prompt for the `CursorRules` format.
///
/// Instructs the LLM to generate `.cursor/rules/actual-policies.mdc` files with
/// YAML frontmatter (`alwaysApply: true`) followed by markdown content.
fn cursor_rules_tailoring_prompt(
    projects_json: &str,
    existing_output_paths: &str,
    adr_json_array: &str,
) -> Result<String, ActualError> {
    use crate::generation::format::CURSOR_RULES_PATH;
    cursor_rules_prompt_decoded(
        projects_json,
        existing_output_paths,
        adr_json_array,
        CURSOR_RULES_PATH,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::OutputFormat;

    #[test]
    fn tailoring_prompt_interpolates_all_sections() {
        let projects = r#"{"projects": [{"name": "web"}]}"#;
        let paths = "CLAUDE.md\napps/web/CLAUDE.md";
        let adrs = r#"[{"id": "adr-001", "title": "Use TypeScript"}]"#;

        let prompt = tailoring_prompt(projects, paths, adrs, &OutputFormat::ClaudeMd)
            .expect("decode must succeed for valid obfuscated constant");

        assert!(
            prompt.contains(projects),
            "tailoring prompt must contain projects_json"
        );
        assert!(
            prompt.contains(paths),
            "tailoring prompt must contain existing_output_paths"
        );
        assert!(
            prompt.contains(adrs),
            "tailoring prompt must contain adr_json_array"
        );
    }

    #[test]
    fn tailoring_prompt_contains_tailor_each_adr() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::ClaudeMd)
            .expect("decode must succeed for valid obfuscated constant");
        assert!(
            prompt.contains("Tailor each ADR"),
            "tailoring prompt must contain 'Tailor each ADR'"
        );
    }

    #[test]
    fn tailoring_prompt_contains_skip_inapplicable() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::ClaudeMd)
            .expect("decode must succeed for valid obfuscated constant");
        assert!(
            prompt.contains("Skip inapplicable"),
            "tailoring prompt must contain 'Skip inapplicable'"
        );
    }

    #[test]
    fn tailoring_prompt_contains_decide_file_layout() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::ClaudeMd)
            .expect("decode must succeed for valid obfuscated constant");
        assert!(
            prompt.contains("Decide file layout"),
            "tailoring prompt must contain 'Decide file layout'"
        );
    }

    #[test]
    fn tailoring_prompt_contains_format_as_markdown() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::ClaudeMd)
            .expect("decode must succeed for valid obfuscated constant");
        assert!(
            prompt.contains("Format as markdown"),
            "tailoring prompt must contain 'Format as markdown'"
        );
    }

    #[test]
    fn tailoring_prompt_does_not_panic_on_empty_inputs() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::ClaudeMd)
            .expect("decode must succeed for valid obfuscated constant");
        assert!(
            !prompt.is_empty(),
            "tailoring prompt with empty inputs must not be empty"
        );
    }

    #[test]
    fn tailoring_prompt_claude_md_references_claude_md() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::ClaudeMd)
            .expect("decode must succeed for valid obfuscated constant");
        assert!(
            prompt.contains("CLAUDE.md"),
            "claude-md format prompt must reference CLAUDE.md"
        );
        assert!(
            !prompt.contains("AGENTS.md"),
            "claude-md format prompt must not reference AGENTS.md"
        );
    }

    #[test]
    fn tailoring_prompt_agents_md_references_agents_md() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::AgentsMd)
            .expect("decode must succeed for valid obfuscated constant");
        assert!(
            prompt.contains("AGENTS.md"),
            "agents-md format prompt must reference AGENTS.md"
        );
        assert!(
            !prompt.contains("CLAUDE.md"),
            "agents-md format prompt must not reference CLAUDE.md"
        );
    }

    // ── CursorRules prompt tests ──

    #[test]
    fn tailoring_prompt_cursor_rules_references_mdc_path() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::CursorRules)
            .expect("decode must succeed for valid obfuscated constant");
        assert!(
            prompt.contains(".cursor/rules/actual-policies.mdc"),
            "cursor-rules prompt must reference .cursor/rules/actual-policies.mdc, got: {prompt}"
        );
    }

    #[test]
    fn tailoring_prompt_cursor_rules_does_not_reference_claude_md() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::CursorRules)
            .expect("decode must succeed for valid obfuscated constant");
        assert!(
            !prompt.contains("CLAUDE.md"),
            "cursor-rules prompt must not reference CLAUDE.md"
        );
    }

    #[test]
    fn tailoring_prompt_cursor_rules_does_not_reference_agents_md() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::CursorRules)
            .expect("decode must succeed for valid obfuscated constant");
        assert!(
            !prompt.contains("AGENTS.md"),
            "cursor-rules prompt must not reference AGENTS.md"
        );
    }

    #[test]
    fn tailoring_prompt_cursor_rules_contains_tailor_each_adr() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::CursorRules)
            .expect("decode must succeed for valid obfuscated constant");
        assert!(
            prompt.contains("Tailor each ADR"),
            "cursor-rules prompt must contain 'Tailor each ADR'"
        );
    }

    #[test]
    fn tailoring_prompt_cursor_rules_interpolates_projects() {
        let projects = r#"{"projects": [{"name": "app"}]}"#;
        let prompt = tailoring_prompt(projects, "", "", &OutputFormat::CursorRules)
            .expect("decode must succeed for valid obfuscated constant");
        assert!(
            prompt.contains(projects),
            "cursor-rules prompt must contain projects_json"
        );
    }

    #[test]
    fn tailoring_prompt_cursor_rules_is_not_empty() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::CursorRules)
            .expect("decode must succeed for valid obfuscated constant");
        assert!(!prompt.is_empty(), "cursor-rules prompt must not be empty");
    }
}
