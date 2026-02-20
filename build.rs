use std::env;
use std::fs;
use std::path::Path;

/// XOR key used to obfuscate sensitive string constants at compile time.
/// Non-printable bytes so they don't appear in `strings` output.
/// IMPORTANT: must stay in sync with src/runner/obfuscation.rs KEY constant.
const KEY: &[u8] = &[
    0x17, 0x8F, 0x09, 0xAB, 0xDC, 0xE2, 0xF4, 0xD1, 0x0E, 0x93, 0xE1, 0xBF, 0xAA, 0x88, 0xCF, 0x1D,
    0xC7, 0xD6, 0x9E, 0x9A, 0xF2, 0xF5, 0x8B, 0xD4, 0xC7, 0xEC, 0x1C, 0x85, 0xEB, 0xB0, 0xA9, 0xFE,
];

fn xor_encode(input: &[u8]) -> Vec<u8> {
    input
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ KEY[i % KEY.len()])
        .collect()
}

fn bytes_literal(data: &[u8]) -> String {
    let hex: Vec<String> = data.iter().map(|b| format!("0x{b:02X}")).collect();
    format!("&[{}]", hex.join(", "))
}

fn main() {
    // Tell Cargo to re-run this script if source files change.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/runner/prompts.rs");
    println!("cargo:rerun-if-changed=src/runner/schemas.rs");

    // --- Standard prompt template (ClaudeMd / AgentsMd) ---
    // Uses {0} .. {4} positional placeholders so it survives format!() at runtime:
    //   {0} = filename  (appears multiple times)
    //   {1} = projects_json
    //   {2} = existing_output_paths
    //   {3} = adr_json_array
    //   {4} = bundled_context
    let prompt_template = "\
You are tailoring Architecture Decision Records (ADRs) for a specific codebase \
and generating {0} file content.

## Repository Context
{1}

## Pre-bundled Repository Context
The following repository context has been pre-bundled for you. Use it as your \
primary source of truth — only use Read/Glob/Grep tools when you need information \
about a specific file NOT already included here.
{4}

## Existing {0} Files
The following {0} files already exist in the repo (read them if you need to \
understand the current state, but only generate content for the managed section):
{2}

## ADRs to Tailor

{3}

## Instructions

1. **Tailor each ADR**: Examine the repository to verify applicability. Reference \
   actual paths, files, and patterns. Identify conflicts with existing code. Merge \
   duplicates. Keep policies concise (1-2 sentences each).

2. **Skip inapplicable ADRs**: If an ADR doesn't apply (e.g., it recommends Tailwind \
   but the project uses styled-components), mark it as skipped with a reason.

3. **Decide file layout**: Generate a root {0} for general/cross-cutting rules. \
   Generate subdirectory {0} files (e.g., apps/web/{0}) for projects \
   that have project-specific ADRs. Only create subdirectory files when there are \
   ADRs specific to that project -- don't create empty or redundant files.

4. **Format as markdown**: Generate the content that goes inside the managed section \
   markers. Use terse, actionable directive-style instructions optimized for Claude's \
   consumption. Organize content naturally -- use headings, bullet points, and inline \
   code references as appropriate.

5. **Preserve intent**: Don't change the fundamental decision -- only make it more \
   specific and actionable for this codebase.

Return your response as a JSON object matching the provided schema.";

    // --- CursorRules prompt template ---
    // Uses {0} = cursor_rules_path (e.g. ".cursor/rules/actual-policies.mdc"),
    //      {1} = projects_json,
    //      {2} = existing_output_paths,
    //      {3} = adr_json_array,
    //      {4} = bundled_context.
    let cursor_rules_template = "\
You are tailoring Architecture Decision Records (ADRs) for a specific codebase \
and generating Cursor IDE rule files ({0}).

## Repository Context
{1}

## Pre-bundled Repository Context
The following repository context has been pre-bundled for you. Use it as your \
primary source of truth — only use Read/Glob/Grep tools when you need information \
about a specific file NOT already included here.
{4}

## Existing Cursor Rule Files
The following {0} files already exist in the repo (read them if you need to \
understand the current state, but only generate content for the managed section):
{2}

## ADRs to Tailor

{3}

## Instructions

1. **Tailor each ADR**: Examine the repository to verify applicability. Reference \
   actual paths, files, and patterns. Identify conflicts with existing code. Merge \
   duplicates. Keep policies concise (1-2 sentences each).

2. **Skip inapplicable ADRs**: If an ADR doesn't apply (e.g., it recommends Tailwind \
   but the project uses styled-components), mark it as skipped with a reason.

3. **Decide file layout**: Generate a root `{0}` for \
   general/cross-cutting rules. Generate subdirectory files (e.g., \
   `apps/web/{0}`) for projects that have project-specific ADRs. \
   Only create subdirectory files when there are ADRs specific to that project -- \
   don't create empty or redundant files.

4. **Format as Cursor MDC**: Each file path must end with `{0}`. \
   Generate only the content that goes inside the managed section markers (do NOT \
   include the YAML frontmatter -- that is added automatically). Use terse, \
   actionable directive-style instructions. Organize content naturally -- use \
   headings, bullet points, and inline code references as appropriate.

5. **Preserve intent**: Don't change the fundamental decision -- only make it more \
   specific and actionable for this codebase.

Return your response as a JSON object matching the provided schema.";

    // --- Output schema ---
    let schema = r#"{
  "type": "object",
  "properties": {
    "files": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "path": {
            "type": "string",
            "description": "File path relative to repo root (e.g., 'CLAUDE.md' or 'apps/web/CLAUDE.md')"
          },
          "content": {
            "type": "string",
            "description": "Rendered markdown content for inside the managed section markers"
          },
          "reasoning": {
            "type": "string",
            "description": "Brief explanation of what rules this file contains and why"
          },
          "adr_ids": {
            "type": "array",
            "items": { "type": "string" },
            "description": "UUIDs of the ADRs included in this file"
          }
        },
        "required": ["path", "content", "reasoning", "adr_ids"]
      }
    },
    "skipped_adrs": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "id": { "type": "string" },
          "reason": { "type": "string" }
        },
        "required": ["id", "reason"]
      },
      "description": "ADRs that were not applicable to this repo"
    },
    "summary": {
      "type": "object",
      "properties": {
        "total_input": { "type": "integer" },
        "applicable": { "type": "integer" },
        "not_applicable": { "type": "integer" },
        "files_generated": { "type": "integer" }
      }
    }
  },
  "required": ["files", "skipped_adrs", "summary"]
}"#;

    let encoded_prompt = xor_encode(prompt_template.as_bytes());
    let encoded_cursor = xor_encode(cursor_rules_template.as_bytes());
    let encoded_schema = xor_encode(schema.as_bytes());

    let out_dir = env::var("OUT_DIR").unwrap();
    let dest = Path::new(&out_dir).join("obfuscated_constants.rs");

    let content = format!(
        "// @generated by build.rs — do not edit\n\
         pub const PROMPT_OBFUSCATED: &[u8] = {prompt};\n\
         pub const CURSOR_RULES_PROMPT_OBFUSCATED: &[u8] = {cursor};\n\
         pub const SCHEMA_OBFUSCATED: &[u8] = {schema};\n",
        prompt = bytes_literal(&encoded_prompt),
        cursor = bytes_literal(&encoded_cursor),
        schema = bytes_literal(&encoded_schema),
    );

    fs::write(&dest, content).unwrap();
}
