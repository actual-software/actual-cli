/// Returns a tailoring prompt with the given repository context, existing output file
/// paths, and ADR JSON array interpolated into the template.
///
/// The `format` parameter controls which filename (e.g. `CLAUDE.md`, `AGENTS.md`, or
/// `.cursor/rules/actual-policies.mdc`) is referenced inside the prompt so the LLM
/// targets the correct output file and uses the correct file structure.
pub fn tailoring_prompt(
    projects_json: &str,
    existing_output_paths: &str,
    adr_json_array: &str,
    format: &crate::generation::OutputFormat,
) -> String {
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
) -> String {
    let filename = format.filename();
    format!(
        "\
You are tailoring Architecture Decision Records (ADRs) for a specific codebase
and generating {filename} file content.

## Repository Context
{projects_json}

## Existing {filename} Files
The following {filename} files already exist in the repo (read them if you need to
understand the current state, but only generate content for the managed section):
{existing_output_paths}

## ADRs to Tailor

{adr_json_array}

## Instructions

1. **Tailor each ADR**: Examine the repository to verify applicability. Reference
   actual paths, files, and patterns. Identify conflicts with existing code. Merge
   duplicates. Keep policies concise (1-2 sentences each).

2. **Skip inapplicable ADRs**: If an ADR doesn't apply (e.g., it recommends Tailwind
   but the project uses styled-components), mark it as skipped with a reason.

3. **Decide file layout**: Generate a root {filename} for general/cross-cutting rules.
   Generate subdirectory {filename} files (e.g., apps/web/{filename}) for projects
   that have project-specific ADRs. Only create subdirectory files when there are
   ADRs specific to that project -- don't create empty or redundant files.

4. **Format as markdown**: Generate the content that goes inside the managed section
   markers. Use terse, actionable directive-style instructions optimized for Claude's
   consumption. Organize content naturally -- use headings, bullet points, and inline
   code references as appropriate.

5. **Preserve intent**: Don't change the fundamental decision -- only make it more
   specific and actionable for this codebase.

Return your response as a JSON object matching the provided schema."
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
) -> String {
    use crate::generation::format::CURSOR_RULES_PATH;
    format!(
        "\
You are tailoring Architecture Decision Records (ADRs) for a specific codebase
and generating Cursor IDE rule files ({CURSOR_RULES_PATH}).

## Repository Context
{projects_json}

## Existing Cursor Rule Files
The following {CURSOR_RULES_PATH} files already exist in the repo (read them if you
need to understand the current state, but only generate content for the managed section):
{existing_output_paths}

## ADRs to Tailor

{adr_json_array}

## Instructions

1. **Tailor each ADR**: Examine the repository to verify applicability. Reference
   actual paths, files, and patterns. Identify conflicts with existing code. Merge
   duplicates. Keep policies concise (1-2 sentences each).

2. **Skip inapplicable ADRs**: If an ADR doesn't apply (e.g., it recommends Tailwind
   but the project uses styled-components), mark it as skipped with a reason.

3. **Decide file layout**: Generate a root `{CURSOR_RULES_PATH}` for
   general/cross-cutting rules. Generate subdirectory files (e.g.,
   `apps/web/{CURSOR_RULES_PATH}`) for projects that have project-specific ADRs.
   Only create subdirectory files when there are ADRs specific to that project --
   don't create empty or redundant files.

4. **Format as Cursor MDC**: Each file path must end with `{CURSOR_RULES_PATH}`.
   Generate only the content that goes inside the managed section markers (do NOT
   include the YAML frontmatter -- that is added automatically). Use terse,
   actionable directive-style instructions. Organize content naturally -- use
   headings, bullet points, and inline code references as appropriate.

5. **Preserve intent**: Don't change the fundamental decision -- only make it more
   specific and actionable for this codebase.

Return your response as a JSON object matching the provided schema."
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

        let prompt = tailoring_prompt(projects, paths, adrs, &OutputFormat::ClaudeMd);

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
        let prompt = tailoring_prompt("", "", "", &OutputFormat::ClaudeMd);
        assert!(
            prompt.contains("Tailor each ADR"),
            "tailoring prompt must contain 'Tailor each ADR'"
        );
    }

    #[test]
    fn tailoring_prompt_contains_skip_inapplicable() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::ClaudeMd);
        assert!(
            prompt.contains("Skip inapplicable"),
            "tailoring prompt must contain 'Skip inapplicable'"
        );
    }

    #[test]
    fn tailoring_prompt_contains_decide_file_layout() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::ClaudeMd);
        assert!(
            prompt.contains("Decide file layout"),
            "tailoring prompt must contain 'Decide file layout'"
        );
    }

    #[test]
    fn tailoring_prompt_contains_format_as_markdown() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::ClaudeMd);
        assert!(
            prompt.contains("Format as markdown"),
            "tailoring prompt must contain 'Format as markdown'"
        );
    }

    #[test]
    fn tailoring_prompt_does_not_panic_on_empty_inputs() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::ClaudeMd);
        assert!(
            !prompt.is_empty(),
            "tailoring prompt with empty inputs must not be empty"
        );
    }

    #[test]
    fn tailoring_prompt_claude_md_references_claude_md() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::ClaudeMd);
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
        let prompt = tailoring_prompt("", "", "", &OutputFormat::AgentsMd);
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
        let prompt = tailoring_prompt("", "", "", &OutputFormat::CursorRules);
        assert!(
            prompt.contains(".cursor/rules/actual-policies.mdc"),
            "cursor-rules prompt must reference .cursor/rules/actual-policies.mdc, got: {prompt}"
        );
    }

    #[test]
    fn tailoring_prompt_cursor_rules_does_not_reference_claude_md() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::CursorRules);
        assert!(
            !prompt.contains("CLAUDE.md"),
            "cursor-rules prompt must not reference CLAUDE.md"
        );
    }

    #[test]
    fn tailoring_prompt_cursor_rules_does_not_reference_agents_md() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::CursorRules);
        assert!(
            !prompt.contains("AGENTS.md"),
            "cursor-rules prompt must not reference AGENTS.md"
        );
    }

    #[test]
    fn tailoring_prompt_cursor_rules_contains_tailor_each_adr() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::CursorRules);
        assert!(
            prompt.contains("Tailor each ADR"),
            "cursor-rules prompt must contain 'Tailor each ADR'"
        );
    }

    #[test]
    fn tailoring_prompt_cursor_rules_interpolates_projects() {
        let projects = r#"{"projects": [{"name": "app"}]}"#;
        let prompt = tailoring_prompt(projects, "", "", &OutputFormat::CursorRules);
        assert!(
            prompt.contains(projects),
            "cursor-rules prompt must contain projects_json"
        );
    }

    #[test]
    fn tailoring_prompt_cursor_rules_is_not_empty() {
        let prompt = tailoring_prompt("", "", "", &OutputFormat::CursorRules);
        assert!(!prompt.is_empty(), "cursor-rules prompt must not be empty");
    }
}
