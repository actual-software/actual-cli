/// Returns a tailoring prompt with the given repository context, existing output file
/// paths, and ADR JSON array interpolated into the template.
///
/// The `format` parameter controls which filename (e.g. `CLAUDE.md` or `AGENTS.md`) is
/// referenced inside the prompt so the LLM targets the correct output file.
pub fn tailoring_prompt(
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
}
