/// JSON schema for per-lens review output, passed to LLM via `run_raw_json()`.
pub const LENS_REVIEW_SCHEMA: &str = r#"{
  "type": "object",
  "required": ["lens", "signal", "rationale", "findings"],
  "properties": {
    "lens": {
      "type": "string",
      "enum": ["principal_engineer", "program_manager", "product_manager",
               "security_engineer", "pragmatic_engineer", "oss_maintainer",
               "test_engineer", "sre_engineer"]
    },
    "signal": {
      "type": "string",
      "enum": ["red", "yellow", "green", "gray"]
    },
    "rationale": {
      "type": "string",
      "description": "One-sentence summary of why this signal was chosen"
    },
    "findings": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["severity", "summary", "details", "suggestion"],
        "properties": {
          "severity": {
            "type": "string",
            "enum": ["red", "yellow", "green", "gray"]
          },
          "related_adrs": {
            "type": "array",
            "items": { "type": "string" },
            "default": []
          },
          "affected_files": {
            "type": "array",
            "items": { "type": "string" },
            "default": []
          },
          "summary": {
            "type": "string",
            "description": "One-line finding title"
          },
          "details": {
            "type": "string",
            "description": "Evidence from the diff supporting this finding"
          },
          "suggestion": {
            "type": "string",
            "description": "Concrete fix or mitigation action"
          }
        }
      }
    }
  }
}"#;

/// JSON schema for cross-lens synthesis output, passed to LLM via `run_raw_json()`.
pub const CROSS_LENS_SYNTHESIS_SCHEMA: &str = r#"{
  "type": "object",
  "required": ["convergent", "divergent", "surprising", "most_relevant"],
  "properties": {
    "convergent": {
      "type": "array",
      "description": "Risks flagged by 3+ lenses",
      "items": {
        "type": "object",
        "required": ["lenses", "summary", "action"],
        "properties": {
          "lenses": { "type": "array", "items": { "type": "string" } },
          "summary": { "type": "string" },
          "action": { "type": "string" }
        }
      }
    },
    "divergent": {
      "type": "array",
      "description": "Where lenses disagree",
      "items": {
        "type": "object",
        "required": ["lenses", "summary", "action"],
        "properties": {
          "lenses": { "type": "array", "items": { "type": "string" } },
          "summary": { "type": "string" },
          "action": { "type": "string" }
        }
      }
    },
    "surprising": {
      "type": "array",
      "description": "Unique insights from a single lens",
      "items": {
        "type": "object",
        "required": ["lenses", "summary", "action"],
        "properties": {
          "lenses": { "type": "array", "items": { "type": "string" } },
          "summary": { "type": "string" },
          "action": { "type": "string" }
        }
      }
    },
    "most_relevant": {
      "type": "array",
      "description": "Top 3 findings most applicable to the changes",
      "maxItems": 3,
      "items": {
        "type": "object",
        "required": ["lenses", "summary", "action"],
        "properties": {
          "lenses": { "type": "array", "items": { "type": "string" } },
          "summary": { "type": "string" },
          "action": { "type": "string" }
        }
      }
    }
  }
}"#;
