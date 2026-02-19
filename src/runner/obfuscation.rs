use crate::error::ActualError;

/// XOR key mirroring the one in build.rs.
/// Non-printable bytes intentionally — they will not appear in `strings` output.
const KEY: &[u8] = &[
    0x17, 0x8F, 0x09, 0xAB, 0xDC, 0xE2, 0xF4, 0xD1, 0x0E, 0x93, 0xE1, 0xBF, 0xAA, 0x88, 0xCF, 0x1D,
    0xC7, 0xD6, 0x9E, 0x9A, 0xF2, 0xF5, 0x8B, 0xD4, 0xC7, 0xEC, 0x1C, 0x85, 0xEB, 0xB0, 0xA9, 0xFE,
];

// Pull in the compile-time generated constants.
include!(concat!(env!("OUT_DIR"), "/obfuscated_constants.rs"));

/// Decode an XOR-obfuscated byte slice back to a UTF-8 string.
///
/// Returns `Err(ActualError::InternalError)` if the decoded bytes are not valid UTF-8,
/// which indicates a build/key mismatch (e.g. stale build artifact).
fn decode(data: &[u8]) -> Result<String, ActualError> {
    let decoded: Vec<u8> = data
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ KEY[i % KEY.len()])
        .collect();
    String::from_utf8(decoded)
        .map_err(|_| ActualError::InternalError("obfuscated constant malformed".to_string()))
}

/// Returns the tailoring prompt template with runtime values interpolated.
///
/// The static instruction text is stored obfuscated in the binary; it is decoded
/// here at runtime and then formatted with the provided arguments.
pub fn tailoring_prompt_decoded(
    projects_json: &str,
    existing_output_paths: &str,
    adr_json_array: &str,
    filename: &str,
) -> Result<String, ActualError> {
    let template = decode(PROMPT_OBFUSCATED)?;
    // Positional placeholders: {0}=filename, {1}=projects_json,
    // {2}=existing_output_paths, {3}=adr_json_array
    Ok(template
        .replace("{0}", filename)
        .replace("{1}", projects_json)
        .replace("{2}", existing_output_paths)
        .replace("{3}", adr_json_array))
}

/// Returns the CursorRules tailoring prompt template with runtime values interpolated.
///
/// The static instruction text is stored obfuscated in the binary; it is decoded
/// here at runtime and then formatted with the provided arguments.
pub fn cursor_rules_prompt_decoded(
    projects_json: &str,
    existing_output_paths: &str,
    adr_json_array: &str,
    cursor_rules_path: &str,
) -> Result<String, ActualError> {
    let template = decode(CURSOR_RULES_PROMPT_OBFUSCATED)?;
    // Positional placeholders: {0}=cursor_rules_path, {1}=projects_json,
    // {2}=existing_output_paths, {3}=adr_json_array
    Ok(template
        .replace("{0}", cursor_rules_path)
        .replace("{1}", projects_json)
        .replace("{2}", existing_output_paths)
        .replace("{3}", adr_json_array))
}

/// Returns the tailoring output JSON schema string, decoded from its obfuscated
/// compile-time representation.
pub fn tailoring_schema_decoded() -> Result<String, ActualError> {
    decode(SCHEMA_OBFUSCATED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_roundtrips_prompt() {
        let prompt = tailoring_prompt_decoded(r#"{"projects":[]}"#, "CLAUDE.md", "[]", "CLAUDE.md")
            .expect("decode must succeed for valid obfuscated constant");
        assert!(
            prompt.contains("Tailor each ADR"),
            "decoded prompt must contain instruction text"
        );
        assert!(
            prompt.contains("CLAUDE.md"),
            "decoded prompt must contain interpolated filename"
        );
        assert!(
            prompt.contains(r#"{"projects":[]}"#),
            "decoded prompt must contain projects_json"
        );
    }

    #[test]
    fn decode_roundtrips_cursor_rules_prompt() {
        let path = ".cursor/rules/actual-policies.mdc";
        let prompt = cursor_rules_prompt_decoded(r#"{"projects":[]}"#, "", "[]", path)
            .expect("decode must succeed for valid obfuscated constant");
        assert!(
            prompt.contains("Tailor each ADR"),
            "decoded cursor prompt must contain instruction text"
        );
        assert!(
            prompt.contains(path),
            "decoded cursor prompt must contain cursor_rules_path"
        );
    }

    #[test]
    fn decode_roundtrips_schema() {
        let schema =
            tailoring_schema_decoded().expect("decode must succeed for valid obfuscated constant");
        let value: serde_json::Value =
            serde_json::from_str(&schema).expect("decoded schema must be valid JSON");
        assert_eq!(value["type"], "object");
        let required = value["required"].as_array().unwrap();
        let fields: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(fields.contains(&"files"));
        assert!(fields.contains(&"skipped_adrs"));
        assert!(fields.contains(&"summary"));
    }

    #[test]
    fn key_is_not_printable_ascii() {
        // Ensure every byte of the key is outside the printable ASCII range
        // so it won't surface in `strings` output.
        for &b in KEY {
            assert!(
                b < 0x20 || b > 0x7E,
                "key byte 0x{b:02X} is printable ASCII — change it"
            );
        }
    }

    #[test]
    fn obfuscated_bytes_differ_from_plaintext() {
        // Verify encoding actually changed the bytes (not a no-op key).
        let plaintext = b"You are tailoring";
        let encoded = PROMPT_OBFUSCATED;
        assert_ne!(
            &encoded[..plaintext.len()],
            plaintext,
            "obfuscated bytes must differ from plaintext"
        );
    }

    #[test]
    fn prompt_filename_substituted_for_agents_md() {
        let prompt = tailoring_prompt_decoded("", "", "", "AGENTS.md")
            .expect("decode must succeed for valid obfuscated constant");
        assert!(prompt.contains("AGENTS.md"));
        assert!(!prompt.contains("CLAUDE.md"));
    }

    #[test]
    fn decode_returns_err_for_invalid_utf8() {
        // XOR 0xFF and 0xFE with the key — produce bytes that after decoding
        // are not valid UTF-8 regardless of the key, by constructing a known
        // invalid sequence. We XOR with the key bytes so that after decode()
        // re-XORs them we get the raw bytes 0xFF and 0xFE, which are never
        // valid UTF-8.
        let raw_invalid: Vec<u8> = vec![0xFF, 0xFE];
        let obfuscated: Vec<u8> = raw_invalid
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ KEY[i % KEY.len()])
            .collect();
        let result = decode(&obfuscated);
        assert!(
            matches!(result, Err(ActualError::InternalError(_))),
            "decode() must return Err(InternalError) for invalid UTF-8, got: {result:?}"
        );
    }
}
