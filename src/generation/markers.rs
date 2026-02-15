use std::collections::HashSet;

pub const START_MARKER: &str = "<!-- managed:actual-start -->";
pub const END_MARKER: &str = "<!-- managed:actual-end -->";

/// Wraps content with managed section markers, including an ISO 8601 timestamp, version comment,
/// and optional ADR IDs metadata.
pub fn wrap_in_markers(content: &str, version: u32, adr_ids: &[String]) -> String {
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let adr_line = if adr_ids.is_empty() {
        String::new()
    } else {
        format!("\n<!-- adr-ids: {} -->", adr_ids.join(","))
    };
    format!(
        "{START_MARKER}\n<!-- last-synced: {timestamp} -->\n<!-- version: {version} -->{adr_line}\n\n{content}\n\n{END_MARKER}"
    )
}

/// Parse `<!-- adr-ids: ... -->` from the given content string and return the IDs as a Vec.
/// Returns empty vec if no adr-ids comment is found. Trims whitespace from each ID.
pub fn extract_adr_ids(content: &str) -> Vec<String> {
    let prefix = "<!-- adr-ids: ";
    let suffix = " -->";
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            if let Some(ids_str) = rest.strip_suffix(suffix) {
                return ids_str
                    .split(',')
                    .map(|id| id.trim().to_string())
                    .filter(|id| !id.is_empty())
                    .collect();
            }
        }
    }
    Vec::new()
}

/// Result of comparing old vs new ADR IDs during re-sync.
#[derive(Debug, Clone, PartialEq)]
pub struct ChangeDetection {
    /// ADR IDs present in new set but not in old (newly added)
    pub new_ids: Vec<String>,
    /// ADR IDs present in both old and new (updated/retained)
    pub updated_ids: Vec<String>,
    /// ADR IDs present in old but not in new (removed)
    pub removed_ids: Vec<String>,
}

/// Detect changes between existing CLAUDE.md content and new ADR IDs.
///
/// `old_content` is the full existing CLAUDE.md file content, or None for new files.
/// `new_adr_ids` are the ADR IDs from the new tailoring output.
pub fn detect_changes(old_content: Option<&str>, new_adr_ids: &[String]) -> ChangeDetection {
    let old_ids = old_content
        .and_then(extract_managed_content)
        .map(extract_adr_ids)
        .unwrap_or_default();

    let old_set: HashSet<&str> = old_ids.iter().map(|s| s.as_str()).collect();
    let new_set: HashSet<&str> = new_adr_ids.iter().map(|s| s.as_str()).collect();

    // Preserve order from new_adr_ids for new_ids and updated_ids
    let new_ids: Vec<String> = new_adr_ids
        .iter()
        .filter(|id| !old_set.contains(id.as_str()))
        .cloned()
        .collect();

    let updated_ids: Vec<String> = new_adr_ids
        .iter()
        .filter(|id| old_set.contains(id.as_str()))
        .cloned()
        .collect();

    // Preserve order from old for removed_ids
    let removed_ids: Vec<String> = old_ids
        .iter()
        .filter(|id| !new_set.contains(id.as_str()))
        .cloned()
        .collect();

    ChangeDetection {
        new_ids,
        updated_ids,
        removed_ids,
    }
}

/// Returns true when both START and END markers are present in the correct order.
pub fn has_managed_section(content: &str) -> bool {
    if let (Some(s), Some(e)) = (content.find(START_MARKER), content.find(END_MARKER)) {
        s < e
    } else {
        false
    }
}

/// Extracts the version number from a `<!-- version: N -->` comment in the content.
/// Returns `Some(N)` if found, `None` otherwise.
pub fn extract_version(content: &str) -> Option<u32> {
    let prefix = "<!-- version: ";
    let start = content.find(prefix)? + prefix.len();
    let rest = &content[start..];
    let end = rest.find(" -->")?;
    rest[..end].parse().ok()
}

/// Returns the next version number to use for a managed section.
/// If `existing_content` contains a version comment, returns version + 1.
/// If `existing_content` is `Some` but has no version, or is `None`, returns 1.
pub fn next_version(existing_content: Option<&str>) -> u32 {
    match existing_content.and_then(extract_version) {
        Some(v) => v + 1,
        None => 1,
    }
}

/// Extracts the timestamp string from a `<!-- last-synced: TIMESTAMP -->` comment.
/// Returns a zero-copy `&str` of the timestamp, or `None` if not found.
pub fn extract_last_synced(content: &str) -> Option<&str> {
    let prefix = "<!-- last-synced: ";
    let start = content.find(prefix)? + prefix.len();
    let rest = &content[start..];
    let end = rest.find(" -->")?;
    Some(&rest[..end])
}

/// Returns the content between the START and END markers, or None if markers are absent.
pub fn extract_managed_content(content: &str) -> Option<&str> {
    let start_idx = content.find(START_MARKER)?;
    let end_idx = content.find(END_MARKER)?;
    if start_idx >= end_idx {
        return None;
    }
    let inner_start = start_idx + START_MARKER.len();
    let inner = &content[inner_start..end_idx];
    // Skip the first newline after the start marker line
    let inner = inner.strip_prefix('\n').unwrap_or(inner);
    // Strip the trailing newline before the end marker
    let inner = inner.strip_suffix('\n').unwrap_or(inner);
    Some(inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrap_in_markers_contains_start_marker() {
        let output = wrap_in_markers("hello", 1, &[]);
        assert!(
            output.contains(START_MARKER),
            "expected START_MARKER in: {}",
            output
        );
    }

    #[test]
    fn test_wrap_in_markers_contains_end_marker() {
        let output = wrap_in_markers("hello", 1, &[]);
        assert!(
            output.contains(END_MARKER),
            "expected END_MARKER in: {}",
            output
        );
    }

    #[test]
    fn test_wrap_in_markers_contains_version_comment() {
        let output = wrap_in_markers("hello", 1, &[]);
        assert!(
            output.contains("<!-- version: 1 -->"),
            "expected version comment in: {}",
            output
        );
    }

    #[test]
    fn test_wrap_in_markers_contains_timestamp() {
        let output = wrap_in_markers("hello", 1, &[]);
        assert!(
            output.contains("<!-- last-synced: "),
            "expected last-synced comment in: {}",
            output
        );
        // Extract and validate the timestamp
        let prefix = "<!-- last-synced: ";
        let start = output.find(prefix).unwrap() + prefix.len();
        let end = output[start..].find(" -->").unwrap() + start;
        let timestamp = &output[start..end];
        // Validates that timestamp is valid ISO 8601 - panics if not
        let _ = chrono::DateTime::parse_from_rfc3339(timestamp).unwrap();
    }

    #[test]
    fn test_wrap_in_markers_contains_content() {
        let output = wrap_in_markers("my special content", 1, &[]);
        let start_pos = output.find(START_MARKER).unwrap();
        let end_pos = output.find(END_MARKER).unwrap();
        let between = &output[start_pos..end_pos];
        assert!(
            between.contains("my special content"),
            "expected content between markers in: {}",
            output
        );
    }

    #[test]
    fn test_wrap_in_markers_start_before_end() {
        let output = wrap_in_markers("hello", 1, &[]);
        let start_pos = output.find(START_MARKER).unwrap();
        let end_pos = output.find(END_MARKER).unwrap();
        assert!(
            start_pos < end_pos,
            "expected START before END, start={} end={}",
            start_pos,
            end_pos
        );
    }

    #[test]
    fn test_has_managed_section_true() {
        let content = wrap_in_markers("hello", 1, &[]);
        assert!(
            has_managed_section(&content),
            "expected true for wrapped content"
        );
    }

    #[test]
    fn test_has_managed_section_false_no_markers() {
        assert!(
            !has_managed_section("just some plain text"),
            "expected false for plain text"
        );
    }

    #[test]
    fn test_has_managed_section_false_only_start() {
        let content = format!("{}\nsome content", START_MARKER);
        assert!(
            !has_managed_section(&content),
            "expected false with only start marker"
        );
    }

    #[test]
    fn test_has_managed_section_false_only_end() {
        let content = format!("some content\n{}", END_MARKER);
        assert!(
            !has_managed_section(&content),
            "expected false with only end marker"
        );
    }

    #[test]
    fn test_extract_managed_content_returns_content() {
        let wrapped = wrap_in_markers("inner content", 1, &[]);
        let extracted = extract_managed_content(&wrapped);
        assert!(extracted.is_some(), "expected Some, got None");
        assert!(
            extracted.unwrap().contains("inner content"),
            "expected inner content in extracted: {:?}",
            extracted
        );
    }

    #[test]
    fn test_extract_managed_content_returns_none_without_markers() {
        assert_eq!(
            extract_managed_content("just some text"),
            None,
            "expected None for plain text"
        );
    }

    #[test]
    fn test_extract_managed_content_returns_none_only_start() {
        let content = format!("{}\nsome content", START_MARKER);
        assert_eq!(
            extract_managed_content(&content),
            None,
            "expected None with only start marker"
        );
    }

    #[test]
    fn test_extract_managed_content_preserves_surrounding_content() {
        let wrapped = wrap_in_markers("managed part", 1, &[]);
        let full = format!("before\n{}\nafter", wrapped);
        let extracted = extract_managed_content(&full).unwrap();
        assert!(
            !extracted.contains("before"),
            "extracted should not contain 'before': {:?}",
            extracted
        );
        assert!(
            !extracted.contains("after"),
            "extracted should not contain 'after': {:?}",
            extracted
        );
        assert!(
            extracted.contains("managed part"),
            "extracted should contain 'managed part': {:?}",
            extracted
        );
    }

    #[test]
    fn test_roundtrip_wrap_then_extract() {
        let original = "line one\nline two\nline three";
        let wrapped = wrap_in_markers(original, 2, &[]);
        let extracted = extract_managed_content(&wrapped).expect("should extract content");
        assert!(
            extracted.contains(original),
            "expected original content in extracted: {:?}",
            extracted
        );
    }

    #[test]
    fn test_extract_managed_content_returns_none_only_end() {
        let content = format!("some content\n{END_MARKER}");
        assert_eq!(
            extract_managed_content(&content),
            None,
            "expected None with only end marker"
        );
    }

    #[test]
    fn test_extract_managed_content_returns_none_reversed_markers() {
        // End marker appears before start marker
        let content = format!("{END_MARKER}\nsome content\n{START_MARKER}");
        assert_eq!(
            extract_managed_content(&content),
            None,
            "expected None when markers are reversed"
        );
    }

    #[test]
    fn test_extract_version_from_comment() {
        assert_eq!(extract_version("<!-- version: 3 -->"), Some(3));
    }

    #[test]
    fn test_extract_version_from_managed_section() {
        let wrapped = wrap_in_markers("content", 5, &[]);
        assert_eq!(extract_version(&wrapped), Some(5));
    }

    #[test]
    fn test_extract_version_none_when_absent() {
        assert_eq!(extract_version("no version here"), None);
    }

    #[test]
    fn test_next_version_increments() {
        let content = wrap_in_markers("some content", 3, &[]);
        assert_eq!(next_version(Some(&content)), 4);
    }

    #[test]
    fn test_next_version_none_returns_1() {
        assert_eq!(next_version(None), 1);
    }

    #[test]
    fn test_extract_last_synced_returns_timestamp() {
        let wrapped = wrap_in_markers("content", 1, &[]);
        let timestamp = extract_last_synced(&wrapped).expect("should find timestamp");
        // Verify it parses as a valid RFC 3339 / ISO 8601 timestamp
        chrono::DateTime::parse_from_rfc3339(timestamp)
            .expect("timestamp should be valid RFC 3339");
    }

    #[test]
    fn test_extract_managed_content_no_newlines_around_content() {
        // Markers directly adjacent to content without newlines
        let content = format!("{START_MARKER}content{END_MARKER}");
        let extracted = extract_managed_content(&content);
        assert_eq!(
            extracted,
            Some("content"),
            "expected 'content' when no newlines around it"
        );
    }

    #[test]
    fn test_has_managed_section_false_reversed_markers() {
        let content = format!("{END_MARKER}\nsome content\n{START_MARKER}");
        assert!(
            !has_managed_section(&content),
            "expected false when markers are reversed"
        );
    }

    #[test]
    fn test_timestamp_is_valid_iso8601() {
        let output = wrap_in_markers("test", 1, &[]);
        let prefix = "<!-- last-synced: ";
        let start = output.find(prefix).unwrap() + prefix.len();
        let end = output[start..].find(" -->").unwrap() + start;
        let timestamp = &output[start..end];
        // Validates that timestamp is valid ISO 8601 - panics if not
        let _ = chrono::DateTime::parse_from_rfc3339(timestamp).unwrap();
    }

    // --- extract_adr_ids tests ---

    #[test]
    fn test_extract_adr_ids_basic() {
        let content = "<!-- adr-ids: abc,def -->";
        assert_eq!(extract_adr_ids(content), vec!["abc", "def"]);
    }

    #[test]
    fn test_extract_adr_ids_empty() {
        let content = "no adr-ids comment here";
        assert!(extract_adr_ids(content).is_empty());
    }

    #[test]
    fn test_extract_adr_ids_single() {
        let content = "<!-- adr-ids: only-one -->";
        assert_eq!(extract_adr_ids(content), vec!["only-one"]);
    }

    #[test]
    fn test_extract_adr_ids_with_whitespace() {
        let content = "<!-- adr-ids: abc , def -->";
        assert_eq!(extract_adr_ids(content), vec!["abc", "def"]);
    }

    #[test]
    fn test_extract_adr_ids_trailing_comma_filters_empty() {
        let content = "<!-- adr-ids: abc,, def, -->";
        assert_eq!(extract_adr_ids(content), vec!["abc", "def"]);
    }

    #[test]
    fn test_extract_adr_ids_malformed_no_suffix() {
        // Has the prefix but no closing " -->"
        let content = "<!-- adr-ids: abc,def";
        assert!(extract_adr_ids(content).is_empty());
    }

    // --- wrap_in_markers with adr_ids tests ---

    #[test]
    fn test_wrap_in_markers_contains_adr_ids() {
        let output = wrap_in_markers("content", 1, &["abc".into(), "def".into()]);
        assert!(
            output.contains("<!-- adr-ids: abc,def -->"),
            "expected adr-ids comment in: {}",
            output
        );
    }

    #[test]
    fn test_wrap_in_markers_empty_adr_ids_no_comment() {
        let output = wrap_in_markers("content", 1, &[]);
        assert!(
            !output.contains("adr-ids"),
            "expected no adr-ids comment in: {}",
            output
        );
    }

    // --- detect_changes tests ---

    #[test]
    fn test_detect_changes_new_file() {
        let new_ids: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        let result = detect_changes(None, &new_ids);
        assert_eq!(
            result,
            ChangeDetection {
                new_ids: vec!["a".into(), "b".into(), "c".into()],
                updated_ids: vec![],
                removed_ids: vec![],
            }
        );
    }

    #[test]
    fn test_detect_changes_same_ids() {
        let content = wrap_in_markers("stuff", 1, &["a".into(), "b".into()]);
        let new_ids: Vec<String> = vec!["a".into(), "b".into()];
        let result = detect_changes(Some(&content), &new_ids);
        assert_eq!(
            result,
            ChangeDetection {
                new_ids: vec![],
                updated_ids: vec!["a".into(), "b".into()],
                removed_ids: vec![],
            }
        );
    }

    #[test]
    fn test_detect_changes_mixed_overlap() {
        let content = wrap_in_markers("stuff", 1, &["a".into(), "b".into(), "c".into()]);
        let new_ids: Vec<String> = vec!["b".into(), "d".into()];
        let result = detect_changes(Some(&content), &new_ids);
        assert_eq!(
            result,
            ChangeDetection {
                new_ids: vec!["d".into()],
                updated_ids: vec!["b".into()],
                removed_ids: vec!["a".into(), "c".into()],
            }
        );
    }

    #[test]
    fn test_detect_changes_all_removed() {
        let content = wrap_in_markers("stuff", 1, &["a".into(), "b".into()]);
        let new_ids: Vec<String> = vec![];
        let result = detect_changes(Some(&content), &new_ids);
        assert_eq!(
            result,
            ChangeDetection {
                new_ids: vec![],
                updated_ids: vec![],
                removed_ids: vec!["a".into(), "b".into()],
            }
        );
    }

    #[test]
    fn test_detect_changes_no_managed_section() {
        let new_ids: Vec<String> = vec!["a".into()];
        let result = detect_changes(Some("plain text with no markers"), &new_ids);
        assert_eq!(
            result,
            ChangeDetection {
                new_ids: vec!["a".into()],
                updated_ids: vec![],
                removed_ids: vec![],
            }
        );
    }

    // --- roundtrip test ---

    #[test]
    fn test_roundtrip_adr_ids() {
        let ids: Vec<String> = vec!["alpha".into(), "beta".into(), "gamma".into()];
        let wrapped = wrap_in_markers("some content", 3, &ids);
        let managed = extract_managed_content(&wrapped).expect("should have managed section");
        let recovered = extract_adr_ids(managed);
        assert_eq!(recovered, ids);
    }
}
