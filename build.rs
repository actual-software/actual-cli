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
The following repository context has been pre-bundled for you. Only use \
Read/Glob/Grep tools for files NOT included here.
{4}

## Existing {0} Files
The following {0} files already exist in the repo (read them if you need to \
understand the current state, but only generate content for the managed section):
{2}

## ADRs to Tailor

{3}

## Instructions

1. **Tailor each ADR**: Reference actual paths, files, and patterns from the \
pre-bundled context. Identify conflicts with existing code. Merge duplicates.

2. **Skip inapplicable ADRs**: If an ADR doesn't apply (e.g., it recommends Tailwind \
   but the project uses styled-components), mark it as skipped with a reason.

3. **Decide file layout**: Generate a root {0} for general/cross-cutting rules. \
   Generate subdirectory {0} files (e.g., apps/web/{0}) for projects \
   that have project-specific ADRs. Only create subdirectory files when there are \
   ADRs specific to that project -- don't create empty or redundant files.

4. **Format as markdown**: Content goes inside the managed section markers. \
Organize with headings, bullet points, and inline code references.

5. **Preserve intent**: Don't change the fundamental decision -- only make it more \
   specific and actionable for this codebase.

6. **One section per ADR**: Return one section per ADR in the sections array. \
Each section must have exactly one adr_id and its content. Do not combine \
multiple ADRs into a single section.

Return ONLY a JSON object matching the provided schema. No commentary outside the JSON.";

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
The following repository context has been pre-bundled for you. Only use \
Read/Glob/Grep tools for files NOT included here.
{4}

## Existing Cursor Rule Files
The following {0} files already exist in the repo (read them if you need to \
understand the current state, but only generate content for the managed section):
{2}

## ADRs to Tailor

{3}

## Instructions

1. **Tailor each ADR**: Reference actual paths, files, and patterns from the \
pre-bundled context. Identify conflicts with existing code. Merge duplicates.

2. **Skip inapplicable ADRs**: If an ADR doesn't apply (e.g., it recommends Tailwind \
   but the project uses styled-components), mark it as skipped with a reason.

3. **Decide file layout**: Generate a root `{0}` for \
   general/cross-cutting rules. Generate subdirectory files (e.g., \
   `apps/web/{0}`) for projects that have project-specific ADRs. \
   Only create subdirectory files when there are ADRs specific to that project -- \
   don't create empty or redundant files.

4. **Format as Cursor MDC**: Each file path must end with `{0}`. \
Content goes inside managed section markers (YAML frontmatter is added \
automatically). Organize with headings, bullet points, and inline code references.

5. **Preserve intent**: Don't change the fundamental decision -- only make it more \
   specific and actionable for this codebase.

6. **One section per ADR**: Return one section per ADR in the sections array. \
Each section must have exactly one adr_id and its content. Do not combine \
multiple ADRs into a single section.

Return ONLY a JSON object matching the provided schema. No commentary outside the JSON.";

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
            "description": "Relative path from repo root (e.g. 'CLAUDE.md', 'apps/web/CLAUDE.md')"
          },
          "sections": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "adr_id": {
                  "type": "string",
                  "description": "UUID of the ADR this section is for. Each ADR MUST have exactly one section."
                },
                "content": {
                  "type": "string",
                  "description": "Terse directive-style markdown for this ADR. 1-2 sentences per policy. Reference actual repo paths. No preamble."
                }
              },
              "required": ["adr_id", "content"]
            },
            "description": "One section per ADR. Do NOT combine multiple ADRs into one section."
          },
          "reasoning": {
            "type": "string",
            "description": "One-sentence label, max 15 words (e.g. 'Cross-cutting Rust style rules for the workspace')"
          }
        },
        "required": ["path", "sections", "reasoning"]
      }
    },
    "skipped_adrs": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "id": { "type": "string" },
          "reason": { "type": "string", "description": "Short reason, max 8 words (e.g. 'no mobile project detected')" }
        },
        "required": ["id", "reason"]
      },
      "description": "ADRs that were not applicable to this repo"
    }
  },
  "required": ["files", "skipped_adrs"]
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

    generate_embedded_file_arrays(&out_dir);
}

fn generate_embedded_file_arrays(out_dir: &str) {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let mut content = String::from("// Auto-generated by build.rs — do not edit\n\n");

    // Tree-sitter query files
    let ts_dir = format!("{manifest_dir}/src/analysis/detectors/tree_sitter_queries");
    let mut ts_files: Vec<_> = std::fs::read_dir(&ts_dir)
        .expect("tree_sitter_queries dir must exist")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x == "scm")
                .unwrap_or(false)
        })
        .collect();
    ts_files.sort_by_key(|e| e.file_name());

    content.push_str("pub(crate) static TREE_SITTER_QUERY_FILES: &[(&str, &str)] = &[\n");
    for entry in &ts_files {
        let abs = entry.path().to_string_lossy().into_owned();
        let name = entry.file_name().to_string_lossy().into_owned();
        println!("cargo:rerun-if-changed={abs}");
        content.push_str(&format!("    ({name:?}, include_str!({abs:?})),\n"));
    }
    content.push_str("];\n\n");

    // Semgrep rule files (recursive walk)
    let sg_root = format!("{manifest_dir}/src/analysis/detectors/semgrep_rules");
    let mut sg_files: Vec<(String, String)> = Vec::new(); // (relative_key, abs_path)
    collect_yml_files(&sg_root, &sg_root, &mut sg_files);
    sg_files.sort_by_key(|(k, _)| k.clone());

    content.push_str("pub(crate) static SEMGREP_RULE_FILES: &[(&str, &[u8])] = &[\n");
    for (rel_key, abs) in &sg_files {
        println!("cargo:rerun-if-changed={abs}");
        content.push_str(&format!("    ({rel_key:?}, include_bytes!({abs:?})),\n"));
    }
    content.push_str("];\n");

    let dest = format!("{out_dir}/embedded_file_arrays.rs");
    std::fs::write(&dest, content).expect("write embedded_file_arrays.rs");
}

fn collect_yml_files(root: &str, current: &str, out: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_yml_files(root, &path.to_string_lossy(), out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("yml") {
            let abs = path.to_string_lossy().into_owned();
            // relative key: strip root prefix and leading slash
            let rel = abs
                .strip_prefix(root)
                .unwrap_or(&abs)
                .trim_start_matches('/')
                .to_string();
            out.push((rel, abs));
        }
    }
}
