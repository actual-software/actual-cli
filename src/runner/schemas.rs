use super::obfuscation::tailoring_schema_decoded;
use crate::error::ActualError;

/// Returns the JSON schema for the combined tailoring + formatting Claude Code invocation.
///
/// The schema text is stored obfuscated in the binary (see `build.rs` and
/// `src/claude/obfuscation.rs`). It is decoded at runtime on first call.
///
/// Passed to `claude -p --json-schema` during ADR tailoring.
/// Required top-level fields: `files`, `skipped_adrs`, `summary`.
///
/// See `docs/plan/05-adr-tailoring.md` for the full schema specification.
///
/// Returns `Err(ActualError::InternalError)` if the obfuscated constant is malformed.
pub fn tailoring_output_schema() -> Result<String, ActualError> {
    tailoring_schema_decoded()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tailoring_output_schema_is_valid_json() {
        let schema_str =
            tailoring_output_schema().expect("decode must succeed for valid obfuscated constant");
        let schema: serde_json::Value =
            serde_json::from_str(&schema_str).expect("TAILORING_OUTPUT_SCHEMA is not valid JSON");
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn tailoring_output_schema_has_required_fields() {
        let schema_str =
            tailoring_output_schema().expect("decode must succeed for valid obfuscated constant");
        let schema: serde_json::Value = serde_json::from_str(&schema_str).unwrap();
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
        // summary is intentionally optional (computed in Rust from files/skipped_adrs)
    }
}
