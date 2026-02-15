pub const START_MARKER: &str = "<!-- managed:actual-start -->";
pub const END_MARKER: &str = "<!-- managed:actual-end -->";

/// Wraps content with managed section markers, including an ISO 8601 timestamp and version comment.
pub fn wrap_in_markers(content: &str, version: u32) -> String {
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    format!(
        "{START_MARKER}\n<!-- last-synced: {timestamp} -->\n<!-- version: {version} -->\n\n{content}\n\n{END_MARKER}"
    )
}

/// Returns true when both START and END markers are present in the correct order.
pub fn has_managed_section(content: &str) -> bool {
    if let (Some(s), Some(e)) = (content.find(START_MARKER), content.find(END_MARKER)) {
        s < e
    } else {
        false
    }
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
        let output = wrap_in_markers("hello", 1);
        assert!(
            output.contains(START_MARKER),
            "expected START_MARKER in: {}",
            output
        );
    }

    #[test]
    fn test_wrap_in_markers_contains_end_marker() {
        let output = wrap_in_markers("hello", 1);
        assert!(
            output.contains(END_MARKER),
            "expected END_MARKER in: {}",
            output
        );
    }

    #[test]
    fn test_wrap_in_markers_contains_version_comment() {
        let output = wrap_in_markers("hello", 1);
        assert!(
            output.contains("<!-- version: 1 -->"),
            "expected version comment in: {}",
            output
        );
    }

    #[test]
    fn test_wrap_in_markers_contains_timestamp() {
        let output = wrap_in_markers("hello", 1);
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
        chrono::DateTime::parse_from_rfc3339(timestamp)
            .unwrap_or_else(|e| panic!("invalid ISO 8601 timestamp '{}': {}", timestamp, e));
    }

    #[test]
    fn test_wrap_in_markers_contains_content() {
        let output = wrap_in_markers("my special content", 1);
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
        let output = wrap_in_markers("hello", 1);
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
        let content = wrap_in_markers("hello", 1);
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
        let wrapped = wrap_in_markers("inner content", 1);
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
        let wrapped = wrap_in_markers("managed part", 1);
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
        let wrapped = wrap_in_markers(original, 2);
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
        let output = wrap_in_markers("test", 1);
        let prefix = "<!-- last-synced: ";
        let start = output.find(prefix).unwrap() + prefix.len();
        let end = output[start..].find(" -->").unwrap() + start;
        let timestamp = &output[start..end];
        let parsed = chrono::DateTime::parse_from_rfc3339(timestamp);
        assert!(
            parsed.is_ok(),
            "timestamp '{}' is not valid ISO 8601: {:?}",
            timestamp,
            parsed.err()
        );
    }
}
