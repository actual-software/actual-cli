/// Returns a static analysis prompt that instructs Claude Code to analyze
/// a repository's structure, languages, and frameworks.
pub fn analysis_prompt() -> String {
    "\
Analyze this repository's structure, programming languages, and frameworks.

Instructions:
- First, check the root directory for monorepo indicators (workspace files, lerna.json, \
nx.json, turbo.json, pnpm-workspace.yaml, go.work, Cargo workspace).
- If this is a monorepo, identify each sub-project by looking in common directories \
(apps/, packages/, services/, libs/, modules/).
- For each project (or the root if it's a single project), identify:
  - The primary programming language(s) by examining package manifests and source files
  - Major frameworks by looking at dependencies and configuration files
  - The package manager used
- Use the exact enum values specified in the JSON schema for languages, framework \
categories, etc.
- For framework names, use lowercase with hyphens (e.g., \"nextjs\", \"fastapi\", \
\"ruby-on-rails\", \"express\", \"react\", \"vue\", \"angular\", \"django\", \"flask\", \
\"spring-boot\", \"gin\", \"actix-web\").
- Be thorough but focus on major/primary frameworks, not every utility library."
        .to_string()
}

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

6. **ADR ID tracking**: The `adr_ids` field in your response must ONLY contain IDs from
   the ADRs provided in the \"ADRs to Tailor\" section above. Do NOT copy or reference
   ADR IDs from existing CLAUDE.md metadata comments (like `<!-- adr-ids: ... -->`).

Return your response as a JSON object matching the provided schema."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analysis_prompt_contains_monorepo_indicators() {
        let prompt = analysis_prompt();
        assert!(
            prompt.contains("monorepo indicators"),
            "analysis prompt must contain 'monorepo indicators'"
        );
    }

    #[test]
    fn analysis_prompt_contains_workspace_files() {
        let prompt = analysis_prompt();
        assert!(
            prompt.contains("workspace files"),
            "analysis prompt must contain 'workspace files'"
        );
    }

    #[test]
    fn analysis_prompt_contains_json_schema() {
        let prompt = analysis_prompt();
        assert!(
            prompt.contains("JSON schema"),
            "analysis prompt must contain 'JSON schema'"
        );
    }

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

    #[test]
    fn tailoring_prompt_contains_adr_id_tracking_instruction() {
        let prompt = tailoring_prompt("", "", "");
        assert!(
            prompt.contains("ADR ID tracking"),
            "tailoring prompt must contain 'ADR ID tracking' instruction"
        );
        assert!(
            prompt.contains("adr_ids"),
            "tailoring prompt must reference 'adr_ids' field"
        );
    }
}
