use std::collections::HashSet;

pub const START_MARKER: &str = "<!-- managed:actual-start -->";
pub const END_MARKER: &str = "<!-- managed:actual-end -->";

pub const ADR_SECTION_START_PREFIX: &str = "<!-- adr:";
pub const ADR_SECTION_START_SUFFIX: &str = " start -->";
pub const ADR_SECTION_END_PREFIX: &str = "<!-- adr:";
pub const ADR_SECTION_END_SUFFIX: &str = " end -->";

/// A single ADR's content section within a managed block.
#[derive(Debug, Clone, PartialEq)]
pub struct AdrSection {
    /// The ADR UUID
    pub id: String,
    /// The content between the start and end markers (trimmed of surrounding newlines)
    pub content: String,
}

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

/// Wrap a single ADR's content with per-ADR section markers.
pub fn wrap_adr_section(adr_id: &str, content: &str) -> String {
    format!(
        "{ADR_SECTION_START_PREFIX}{adr_id}{ADR_SECTION_START_SUFFIX}\n{content}\n{ADR_SECTION_END_PREFIX}{adr_id}{ADR_SECTION_END_SUFFIX}"
    )
}

/// Extract all per-ADR sections from content (inside or outside managed markers).
/// Returns sections in document order.
pub fn extract_adr_sections(content: &str) -> Vec<AdrSection> {
    let mut sections = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        if let Some(id) = parse_adr_section_start(trimmed) {
            let end_marker = format!("{ADR_SECTION_END_PREFIX}{id}{ADR_SECTION_END_SUFFIX}");
            // Search for the matching end marker
            let mut found_end = false;
            let mut content_lines = Vec::new();
            let start_line = i;
            i += 1;
            while i < lines.len() {
                if lines[i].trim() == end_marker {
                    found_end = true;
                    break;
                }
                content_lines.push(lines[i]);
                i += 1;
            }
            if found_end {
                let inner = content_lines.join("\n");
                // Trim one leading and one trailing newline if present
                let inner = inner.strip_prefix('\n').unwrap_or(&inner);
                let inner = inner.strip_suffix('\n').unwrap_or(inner);
                sections.push(AdrSection {
                    id,
                    content: inner.to_string(),
                });
            } else {
                // No matching end marker found — skip this start marker
                // Reset to just after the start line so we don't skip other sections
                i = start_line;
            }
        }
        i += 1;
    }
    sections
}

/// Strip per-ADR section markers from content, keeping the content between them.
pub fn strip_adr_section_markers(content: &str) -> String {
    content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !is_adr_section_marker(trimmed)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Check if a trimmed line is a valid ADR section start or end marker.
///
/// Delegates to the strict parsers to ensure the ID between the prefix and
/// suffix is non-empty, avoiding false positives on malformed markers like
/// `<!-- adr: start -->` or `<!-- adr: end -->`.
fn is_adr_section_marker(trimmed: &str) -> bool {
    parse_adr_section_start(trimmed).is_some() || parse_adr_section_end(trimmed).is_some()
}

/// Parse an ADR section start marker and return the ADR ID if valid.
fn parse_adr_section_start(trimmed: &str) -> Option<String> {
    let rest = trimmed.strip_prefix(ADR_SECTION_START_PREFIX)?;
    let id = rest.strip_suffix(ADR_SECTION_START_SUFFIX)?;
    if id.is_empty() {
        return None;
    }
    Some(id.to_string())
}

/// Parse an ADR section end marker and return the ADR ID if valid.
fn parse_adr_section_end(trimmed: &str) -> Option<String> {
    let rest = trimmed.strip_prefix(ADR_SECTION_END_PREFIX)?;
    let id = rest.strip_suffix(ADR_SECTION_END_SUFFIX)?;
    if id.is_empty() {
        return None;
    }
    Some(id.to_string())
}

/// Parse `<!-- adr-ids: ... -->` from the given content string and return the IDs as a Vec.
/// Returns empty vec if no adr-ids comment is found. Trims whitespace from each ID.
pub fn extract_adr_ids(content: &str) -> Vec<String> {
    // First try per-ADR section markers
    let sections = extract_adr_sections(content);
    if !sections.is_empty() {
        return sections.into_iter().map(|s| s.id).collect();
    }
    // Fall back to flat adr-ids comment (backward compat)
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

/// Returns the byte positions `(start_idx, end_idx)` of the START and END markers in `content`,
/// or `None` if either marker is absent or they appear in the wrong order (end before start).
///
/// `start_idx` is the byte offset of the first character of `START_MARKER`.
/// `end_idx` is the byte offset of the first character of `END_MARKER`.
pub fn find_managed_section_bounds(content: &str) -> Option<(usize, usize)> {
    let start_idx = content.find(START_MARKER)?;
    let end_idx = content.find(END_MARKER)?;
    if start_idx < end_idx {
        Some((start_idx, end_idx))
    } else {
        None
    }
}

/// Returns true when both START and END markers are present in the correct order.
pub fn has_managed_section(content: &str) -> bool {
    find_managed_section_bounds(content).is_some()
}

/// Extracts the version number from a `<!-- version: N -->` comment in the content.
/// Returns `Some(N)` if found, `None` otherwise.
pub fn extract_version(content: &str) -> Option<u32> {
    let managed = extract_managed_content(content).unwrap_or(content);
    let prefix = "<!-- version: ";
    let start = managed.find(prefix)? + prefix.len();
    let rest = &managed[start..];
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
    let managed = extract_managed_content(content).unwrap_or(content);
    let prefix = "<!-- last-synced: ";
    let start = managed.find(prefix)? + prefix.len();
    let rest = &managed[start..];
    let end = rest.find(" -->")?;
    Some(&rest[..end])
}

/// Strips managed section metadata comments from content.
///
/// Removes `<!-- adr-ids: ... -->`, `<!-- last-synced: ... -->`, `<!-- version: ... -->`,
/// `<!-- managed:actual-start -->`, and `<!-- managed:actual-end -->` lines so that
/// the tailoring LLM only sees the human-readable content without tracking metadata
/// that could cause ADR ID hallucination.
pub fn strip_managed_metadata(content: &str) -> String {
    content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !(trimmed.starts_with("<!-- adr-ids:")
                || trimmed.starts_with("<!-- last-synced:")
                || trimmed.starts_with("<!-- version:")
                || trimmed == START_MARKER
                || trimmed == END_MARKER
                || is_adr_section_marker(trimmed))
        })
        .collect::<Vec<_>>()
        .join("\n")
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

    // --- find_managed_section_bounds tests ---

    #[test]
    fn test_find_managed_section_bounds_returns_correct_positions() {
        let content = wrap_in_markers("hello", 1, &[]);
        let (start_idx, end_idx) =
            find_managed_section_bounds(&content).expect("should find bounds in wrapped content");
        assert_eq!(
            &content[start_idx..start_idx + START_MARKER.len()],
            START_MARKER
        );
        assert_eq!(&content[end_idx..end_idx + END_MARKER.len()], END_MARKER);
        assert!(start_idx < end_idx, "start must be before end");
    }

    #[test]
    fn test_find_managed_section_bounds_returns_none_for_plain_text() {
        assert_eq!(find_managed_section_bounds("just plain text"), None);
    }

    #[test]
    fn test_find_managed_section_bounds_returns_none_for_reversed_markers() {
        let content = format!("{END_MARKER}\nsome content\n{START_MARKER}");
        assert_eq!(
            find_managed_section_bounds(&content),
            None,
            "expected None when markers are reversed"
        );
    }

    #[test]
    fn test_find_managed_section_bounds_returns_none_only_start() {
        let content = format!("{START_MARKER}\nsome content");
        assert_eq!(find_managed_section_bounds(&content), None);
    }

    #[test]
    fn test_find_managed_section_bounds_returns_none_only_end() {
        let content = format!("some content\n{END_MARKER}");
        assert_eq!(find_managed_section_bounds(&content), None);
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

    #[test]
    fn test_extract_version_malformed_value() {
        // Version comment present but with non-numeric value
        assert_eq!(extract_version("<!-- version: abc -->"), None);
    }

    #[test]
    fn test_extract_last_synced_absent() {
        assert_eq!(extract_last_synced("no timestamp here"), None);
    }

    #[test]
    fn test_next_version_no_version_in_content() {
        // Content exists but has no version comment
        assert_eq!(next_version(Some("no version here")), 1);
    }

    // --- strip_managed_metadata tests ---

    #[test]
    fn test_strip_managed_metadata_removes_adr_ids() {
        let content = "# Rules\n<!-- adr-ids: abc-123,def-456 -->\nSome content";
        let result = strip_managed_metadata(content);
        assert!(
            !result.contains("adr-ids"),
            "should strip adr-ids: {result}"
        );
        assert!(
            result.contains("# Rules"),
            "should preserve heading: {result}"
        );
        assert!(
            result.contains("Some content"),
            "should preserve content: {result}"
        );
    }

    #[test]
    fn test_strip_managed_metadata_removes_last_synced() {
        let content = "# Rules\n<!-- last-synced: 2024-01-01T00:00:00Z -->\nSome content";
        let result = strip_managed_metadata(content);
        assert!(
            !result.contains("last-synced"),
            "should strip last-synced: {result}"
        );
        assert!(
            result.contains("Some content"),
            "should preserve content: {result}"
        );
    }

    #[test]
    fn test_strip_managed_metadata_removes_version() {
        let content = "# Rules\n<!-- version: 3 -->\nSome content";
        let result = strip_managed_metadata(content);
        assert!(
            !result.contains("version:"),
            "should strip version: {result}"
        );
        assert!(
            result.contains("Some content"),
            "should preserve content: {result}"
        );
    }

    #[test]
    fn test_strip_managed_metadata_removes_markers() {
        let content = format!(
            "# Before\n{}\nManaged content\n{}\n# After",
            START_MARKER, END_MARKER
        );
        let result = strip_managed_metadata(&content);
        assert!(
            !result.contains("managed:actual"),
            "should strip markers: {result}"
        );
        assert!(
            result.contains("Managed content"),
            "should preserve inner content: {result}"
        );
        assert!(
            result.contains("# Before"),
            "should preserve before: {result}"
        );
        assert!(
            result.contains("# After"),
            "should preserve after: {result}"
        );
    }

    #[test]
    fn test_strip_managed_metadata_preserves_non_metadata() {
        let content = "# My Rules\n\n- Do this\n- Do that\n\n## Details\n\nMore info here.";
        let result = strip_managed_metadata(content);
        assert_eq!(
            result, content,
            "non-metadata content should pass through unchanged"
        );
    }

    #[test]
    fn test_strip_managed_metadata_empty_input() {
        let result = strip_managed_metadata("");
        assert_eq!(result, "", "empty input should return empty string");
    }

    #[test]
    fn test_strip_managed_metadata_full_managed_section() {
        let content = wrap_in_markers("Real content here", 2, &["id-1".into(), "id-2".into()]);
        let result = strip_managed_metadata(&content);
        assert!(
            !result.contains("adr-ids"),
            "should strip adr-ids: {result}"
        );
        assert!(
            !result.contains("last-synced"),
            "should strip last-synced: {result}"
        );
        assert!(
            !result.contains("version:"),
            "should strip version: {result}"
        );
        assert!(
            !result.contains("managed:actual"),
            "should strip markers: {result}"
        );
        assert!(
            result.contains("Real content here"),
            "should preserve real content: {result}"
        );
    }

    // --- extract_version / extract_last_synced scoped to managed section ---

    #[test]
    fn test_extract_version_ignores_user_content_outside_managed_section() {
        let managed = wrap_in_markers("managed content", 3, &[]);
        let full = format!("# My Custom Rules\n\n<!-- version: 99 -->\n\n{managed}\n\n## Footer\n");
        assert_eq!(extract_version(&full), Some(3));
    }

    #[test]
    fn test_extract_last_synced_ignores_user_content_outside_managed_section() {
        let managed = wrap_in_markers("managed content", 1, &[]);
        let full =
            format!("# My Notes\n\n<!-- last-synced: 1999-01-01T00:00:00Z -->\n\n{managed}\n");
        let ts = extract_last_synced(&full).expect("should find timestamp");
        assert_ne!(ts, "1999-01-01T00:00:00Z");
        // Verify the returned timestamp is actually from the managed section
        chrono::DateTime::parse_from_rfc3339(ts)
            .expect("should be a valid RFC 3339 timestamp from the managed section");
    }

    #[test]
    fn test_extract_version_ignores_version_after_managed_section() {
        let managed = wrap_in_markers("managed content", 5, &[]);
        let full = format!("{managed}\n\n<!-- version: 42 -->\n");
        assert_eq!(extract_version(&full), Some(5));
    }

    #[test]
    fn test_extract_version_falls_back_when_no_managed_section() {
        assert_eq!(extract_version("<!-- version: 7 -->"), Some(7));
    }

    #[test]
    fn test_extract_last_synced_falls_back_when_no_managed_section() {
        assert_eq!(
            extract_last_synced("<!-- last-synced: 2025-06-01T00:00:00Z -->"),
            Some("2025-06-01T00:00:00Z")
        );
    }

    // --- per-ADR section marker tests ---

    #[test]
    fn test_adr_section_constants() {
        assert_eq!(ADR_SECTION_START_PREFIX, "<!-- adr:");
        assert_eq!(ADR_SECTION_START_SUFFIX, " start -->");
        assert_eq!(ADR_SECTION_END_PREFIX, "<!-- adr:");
        assert_eq!(ADR_SECTION_END_SUFFIX, " end -->");
    }

    #[test]
    fn test_wrap_adr_section_basic() {
        let result = wrap_adr_section("abc-123", "Some content");
        assert!(result.contains("<!-- adr:abc-123 start -->"));
        assert!(result.contains("<!-- adr:abc-123 end -->"));
        assert!(result.contains("Some content"));
    }

    #[test]
    fn test_wrap_adr_section_multiline() {
        let content = "Line 1\nLine 2\nLine 3";
        let result = wrap_adr_section("multi", content);
        assert_eq!(
            result,
            "<!-- adr:multi start -->\nLine 1\nLine 2\nLine 3\n<!-- adr:multi end -->"
        );
    }

    #[test]
    fn test_wrap_adr_section_empty_content() {
        let result = wrap_adr_section("empty", "");
        assert_eq!(result, "<!-- adr:empty start -->\n\n<!-- adr:empty end -->");
    }

    #[test]
    fn test_extract_adr_sections_multiple() {
        let content = format!(
            "{}\n{}\n{}",
            wrap_adr_section("adr-1", "Content one"),
            wrap_adr_section("adr-2", "Content two"),
            wrap_adr_section("adr-3", "Content three"),
        );
        let sections = extract_adr_sections(&content);
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].id, "adr-1");
        assert_eq!(sections[0].content, "Content one");
        assert_eq!(sections[1].id, "adr-2");
        assert_eq!(sections[1].content, "Content two");
        assert_eq!(sections[2].id, "adr-3");
        assert_eq!(sections[2].content, "Content three");
    }

    #[test]
    fn test_extract_adr_sections_single() {
        let content = wrap_adr_section("only-one", "The content");
        let sections = extract_adr_sections(&content);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].id, "only-one");
        assert_eq!(sections[0].content, "The content");
    }

    #[test]
    fn test_extract_adr_sections_empty() {
        let sections = extract_adr_sections("no sections here");
        assert!(sections.is_empty());
    }

    #[test]
    fn test_extract_adr_sections_empty_id_ignored() {
        // <!-- adr: start --> has an empty id and should be ignored
        let content = "<!-- adr: start -->\nSome content\n<!-- adr: end -->";
        let sections = extract_adr_sections(content);
        assert!(sections.is_empty(), "empty id should be ignored");
    }

    #[test]
    fn test_extract_adr_sections_malformed_no_end() {
        let content = "<!-- adr:orphan start -->\nSome content\nNo end marker";
        let sections = extract_adr_sections(content);
        assert!(
            sections.is_empty(),
            "should skip sections without matching end marker"
        );
    }

    #[test]
    fn test_extract_adr_sections_roundtrip() {
        let original_content = "Hello world\nSecond line";
        let wrapped = wrap_adr_section("roundtrip-id", original_content);
        let sections = extract_adr_sections(&wrapped);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].id, "roundtrip-id");
        assert_eq!(sections[0].content, original_content);
    }

    #[test]
    fn test_extract_adr_ids_from_section_markers() {
        let content = format!(
            "{}\n{}",
            wrap_adr_section("id-alpha", "Alpha content"),
            wrap_adr_section("id-beta", "Beta content"),
        );
        let ids = extract_adr_ids(&content);
        assert_eq!(ids, vec!["id-alpha", "id-beta"]);
    }

    #[test]
    fn test_extract_adr_ids_fallback_flat() {
        // No section markers, only flat adr-ids comment
        let content = "<!-- adr-ids: flat-1,flat-2 -->";
        let ids = extract_adr_ids(content);
        assert_eq!(ids, vec!["flat-1", "flat-2"]);
    }

    #[test]
    fn test_strip_adr_section_markers_basic() {
        let content = "<!-- adr:abc start -->\nContent here\n<!-- adr:abc end -->";
        let result = strip_adr_section_markers(content);
        assert_eq!(result, "Content here");
        assert!(!result.contains("<!-- adr:"));
    }

    #[test]
    fn test_strip_adr_section_markers_multiple() {
        let content = format!(
            "{}\n{}\n{}",
            wrap_adr_section("a", "Content A"),
            wrap_adr_section("b", "Content B"),
            wrap_adr_section("c", "Content C"),
        );
        let result = strip_adr_section_markers(&content);
        assert!(result.contains("Content A"));
        assert!(result.contains("Content B"));
        assert!(result.contains("Content C"));
        assert!(!result.contains("<!-- adr:"));
    }

    #[test]
    fn test_strip_managed_metadata_strips_adr_section_markers() {
        let inner = format!(
            "<!-- adr-ids: x,y -->\n{}\n{}",
            wrap_adr_section("x", "X content"),
            wrap_adr_section("y", "Y content"),
        );
        let content = wrap_in_markers(&inner, 1, &["x".into(), "y".into()]);
        let result = strip_managed_metadata(&content);
        assert!(
            !result.contains("<!-- adr:"),
            "should strip adr section markers: {result}"
        );
        assert!(
            !result.contains("adr-ids"),
            "should strip adr-ids: {result}"
        );
        assert!(
            result.contains("X content"),
            "should preserve content: {result}"
        );
        assert!(
            result.contains("Y content"),
            "should preserve content: {result}"
        );
    }

    // --- is_adr_section_marker strict validation tests ---

    #[test]
    fn test_is_adr_section_marker_accepts_valid_start() {
        assert!(is_adr_section_marker("<!-- adr:abc-123 start -->"));
    }

    #[test]
    fn test_is_adr_section_marker_accepts_valid_end() {
        assert!(is_adr_section_marker("<!-- adr:abc-123 end -->"));
    }

    #[test]
    fn test_is_adr_section_marker_rejects_empty_id_start() {
        assert!(
            !is_adr_section_marker("<!-- adr: start -->"),
            "empty ID start marker should be rejected"
        );
    }

    #[test]
    fn test_is_adr_section_marker_rejects_empty_id_end() {
        assert!(
            !is_adr_section_marker("<!-- adr: end -->"),
            "empty ID end marker should be rejected"
        );
    }

    #[test]
    fn test_is_adr_section_marker_rejects_plain_text() {
        assert!(!is_adr_section_marker("just some text"));
    }

    #[test]
    fn test_is_adr_section_marker_rejects_partial_prefix() {
        assert!(!is_adr_section_marker("<!-- adr:abc"));
    }

    #[test]
    fn test_is_adr_section_marker_rejects_wrong_suffix() {
        assert!(!is_adr_section_marker("<!-- adr:abc foo -->"));
    }

    // --- parse_adr_section_end tests ---

    #[test]
    fn test_parse_adr_section_end_valid() {
        assert_eq!(
            parse_adr_section_end("<!-- adr:my-id end -->"),
            Some("my-id".to_string())
        );
    }

    #[test]
    fn test_parse_adr_section_end_empty_id() {
        assert_eq!(
            parse_adr_section_end("<!-- adr: end -->"),
            None,
            "empty ID should return None"
        );
    }

    #[test]
    fn test_parse_adr_section_end_malformed_no_suffix() {
        assert_eq!(
            parse_adr_section_end("<!-- adr:my-id"),
            None,
            "missing suffix should return None"
        );
    }

    #[test]
    fn test_parse_adr_section_end_wrong_prefix() {
        assert_eq!(
            parse_adr_section_end("<!-- wrong:my-id end -->"),
            None,
            "wrong prefix should return None"
        );
    }

    #[test]
    fn test_parse_adr_section_end_start_suffix() {
        assert_eq!(
            parse_adr_section_end("<!-- adr:my-id start -->"),
            None,
            "start suffix should not match end parser"
        );
    }

    // --- strip_adr_section_markers preserves empty-ID markers ---

    #[test]
    fn test_strip_adr_section_markers_preserves_empty_id_markers() {
        let content =
            "<!-- adr: start -->\nUser content here\n<!-- adr: end -->\n<!-- adr:real-id start -->\nManaged content\n<!-- adr:real-id end -->";
        let result = strip_adr_section_markers(content);
        assert!(
            result.contains("<!-- adr: start -->"),
            "empty-ID start marker should be preserved: {result}"
        );
        assert!(
            result.contains("<!-- adr: end -->"),
            "empty-ID end marker should be preserved: {result}"
        );
        assert!(
            result.contains("User content here"),
            "user content should be preserved: {result}"
        );
        assert!(
            !result.contains("<!-- adr:real-id start -->"),
            "valid start marker should be stripped: {result}"
        );
        assert!(
            !result.contains("<!-- adr:real-id end -->"),
            "valid end marker should be stripped: {result}"
        );
    }

    // --- strip_managed_metadata preserves empty-ID markers ---

    #[test]
    fn test_strip_managed_metadata_preserves_empty_id_markers() {
        let content = "# Title\n<!-- adr: start -->\nUser note\n<!-- adr: end -->\nMore content";
        let result = strip_managed_metadata(content);
        assert!(
            result.contains("<!-- adr: start -->"),
            "empty-ID start marker should be preserved: {result}"
        );
        assert!(
            result.contains("<!-- adr: end -->"),
            "empty-ID end marker should be preserved: {result}"
        );
        assert!(
            result.contains("User note"),
            "user content should be preserved: {result}"
        );
    }
}
