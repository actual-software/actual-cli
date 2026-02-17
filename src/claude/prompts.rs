/// Returns a tailoring prompt with the given repository context, existing CLAUDE.md
/// paths, and ADR JSON array interpolated into the template.
pub fn tailoring_prompt(
    projects_json: &str,
    existing_claude_md_paths: &str,
    adr_json_array: &str,
) -> String {
    format!(
        "\
You are tailoring Architecture Decision Records (ADRs) for a specific codebase
and generating CLAUDE.md file content.

## Repository Context
{projects_json}

## Existing CLAUDE.md Files
The following CLAUDE.md files already exist in the repo (read them if you need to
understand the current state, but only generate content for the managed section):
{existing_claude_md_paths}

## ADRs to Tailor

{adr_json_array}

## Instructions

1. **Tailor each ADR**: Examine the repository to verify applicability. Reference
   actual paths, files, and patterns. Identify conflicts with existing code. Merge
   duplicates. Keep policies concise (1-2 sentences each).

2. **Skip inapplicable ADRs**: If an ADR doesn't apply (e.g., it recommends Tailwind
   but the project uses styled-components), mark it as skipped with a reason.

3. **Decide file layout**: Generate a root CLAUDE.md for general/cross-cutting rules.
   Generate subdirectory CLAUDE.md files (e.g., apps/web/CLAUDE.md) for projects
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

    #[test]
    fn tailoring_prompt_interpolates_all_sections() {
        let projects = r#"{"projects": [{"name": "web"}]}"#;
        let paths = "CLAUDE.md\napps/web/CLAUDE.md";
        let adrs = r#"[{"id": "adr-001", "title": "Use TypeScript"}]"#;

        let prompt = tailoring_prompt(projects, paths, adrs);

        assert!(
            prompt.contains(projects),
            "tailoring prompt must contain projects_json"
        );
        assert!(
            prompt.contains(paths),
            "tailoring prompt must contain existing_claude_md_paths"
        );
        assert!(
            prompt.contains(adrs),
            "tailoring prompt must contain adr_json_array"
        );
    }

    #[test]
    fn tailoring_prompt_contains_tailor_each_adr() {
        let prompt = tailoring_prompt("", "", "");
        assert!(
            prompt.contains("Tailor each ADR"),
            "tailoring prompt must contain 'Tailor each ADR'"
        );
    }

    #[test]
    fn tailoring_prompt_contains_skip_inapplicable() {
        let prompt = tailoring_prompt("", "", "");
        assert!(
            prompt.contains("Skip inapplicable"),
            "tailoring prompt must contain 'Skip inapplicable'"
        );
    }

    #[test]
    fn tailoring_prompt_contains_decide_file_layout() {
        let prompt = tailoring_prompt("", "", "");
        assert!(
            prompt.contains("Decide file layout"),
            "tailoring prompt must contain 'Decide file layout'"
        );
    }

    #[test]
    fn tailoring_prompt_contains_format_as_markdown() {
        let prompt = tailoring_prompt("", "", "");
        assert!(
            prompt.contains("Format as markdown"),
            "tailoring prompt must contain 'Format as markdown'"
        );
    }

    #[test]
    fn tailoring_prompt_does_not_panic_on_empty_inputs() {
        let prompt = tailoring_prompt("", "", "");
        assert!(
            !prompt.is_empty(),
            "tailoring prompt with empty inputs must not be empty"
        );
    }
}
