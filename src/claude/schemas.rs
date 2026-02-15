/// JSON schema for the repo analysis Claude Code invocation.
///
/// Passed to `claude -p --json-schema` during repository analysis.
/// Required top-level fields: `is_monorepo`, `projects`.
///
/// See `docs/plan/03-repo-analysis.md` for the full schema specification.
pub const REPO_ANALYSIS_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "is_monorepo": {
      "type": "boolean",
      "description": "Whether this repository contains multiple distinct projects"
    },
    "projects": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "path": {
            "type": "string",
            "description": "Relative path from repo root (use '.' for root-level project)"
          },
          "name": {
            "type": "string",
            "description": "Human-readable project name"
          },
          "languages": {
            "type": "array",
            "items": {
              "type": "string",
              "enum": ["typescript", "javascript", "python", "rust", "go", "java", "kotlin", "swift", "ruby", "php", "c", "cpp", "csharp", "scala", "elixir", "other"]
            }
          },
          "frameworks": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "name": {
                  "type": "string",
                  "description": "Framework name (e.g., 'nextjs', 'fastapi', 'rails')"
                },
                "category": {
                  "type": "string",
                  "enum": ["web-frontend", "web-backend", "mobile", "desktop", "cli", "library", "data", "ml", "devops", "testing"]
                }
              },
              "required": ["name", "category"]
            }
          },
          "package_manager": {
            "type": "string",
            "description": "Package manager used (npm, pnpm, yarn, cargo, pip, poetry, etc.)"
          },
          "description": {
            "type": "string",
            "description": "Brief description of what this project does"
          }
        },
        "required": ["path", "name", "languages", "frameworks"]
      }
    }
  },
  "required": ["is_monorepo", "projects"]
}"#;

/// JSON schema for the combined tailoring + formatting Claude Code invocation.
///
/// Passed to `claude -p --json-schema` during ADR tailoring.
/// Required top-level fields: `files`, `skipped_adrs`, `summary`.
///
/// See `docs/plan/05-adr-tailoring.md` for the full schema specification.
pub const TAILORING_OUTPUT_SCHEMA: &str = r#"{
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_analysis_schema_is_valid_json() {
        let schema: serde_json::Value = serde_json::from_str(REPO_ANALYSIS_SCHEMA)
            .expect("REPO_ANALYSIS_SCHEMA is not valid JSON");
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn tailoring_output_schema_is_valid_json() {
        let schema: serde_json::Value = serde_json::from_str(TAILORING_OUTPUT_SCHEMA)
            .expect("TAILORING_OUTPUT_SCHEMA is not valid JSON");
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn repo_analysis_schema_has_required_fields() {
        let schema: serde_json::Value = serde_json::from_str(REPO_ANALYSIS_SCHEMA).unwrap();
        let required = schema["required"]
            .as_array()
            .expect("required field should be an array");
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(
            required_strs.contains(&"is_monorepo"),
            "required should contain 'is_monorepo'"
        );
        assert!(
            required_strs.contains(&"projects"),
            "required should contain 'projects'"
        );
    }

    #[test]
    fn tailoring_output_schema_has_required_fields() {
        let schema: serde_json::Value = serde_json::from_str(TAILORING_OUTPUT_SCHEMA).unwrap();
        let required = schema["required"]
            .as_array()
            .expect("required field should be an array");
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(
            required_strs.contains(&"files"),
            "required should contain 'files'"
        );
        assert!(
            required_strs.contains(&"skipped_adrs"),
            "required should contain 'skipped_adrs'"
        );
        assert!(
            required_strs.contains(&"summary"),
            "required should contain 'summary'"
        );
    }
}
