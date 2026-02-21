//! Snapshot testing helpers for comparing terminal screen content against stored snapshots.
//!
//! This module provides formatting functions and macros for use with
//! [insta](https://docs.rs/insta) snapshot testing.
//!
//! The format functions are always available. The `assert_tui_snapshot!` and
//! `assert_screen_snapshot!` macros require the `snapshot` feature to be enabled.

use crate::ScreenSnapshot;

/// Format a screen snapshot into canonical text for snapshot comparison.
///
/// Trims trailing whitespace per line and removes trailing empty lines.
/// Leading whitespace is preserved.
pub fn format_screen_snapshot(snap: &ScreenSnapshot) -> String {
    let (rows, _cols) = snap.size();
    let mut lines: Vec<String> = (0..rows)
        .map(|row| snap.row_text(row).trim_end().to_string())
        .collect();

    // Remove trailing empty lines
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }

    lines.join("\n")
}

/// Format a screen snapshot with a metadata header.
///
/// The header includes terminal size and cursor position:
/// ```text
/// --- screen 80x24 cursor (0,5) ---
/// hello
/// ```
pub fn format_screen_snapshot_with_metadata(snap: &ScreenSnapshot) -> String {
    let (rows, cols) = snap.size();
    let (cr, cc) = snap.cursor_position();
    let body = format_screen_snapshot(snap);
    format!("--- screen {cols}x{rows} cursor ({cr},{cc}) ---\n{body}")
}

/// Assert screen text from a [`TuiSession`](crate::TuiSession) matches a stored snapshot.
///
/// Requires the `snapshot` feature to be enabled.
///
/// # Variants
///
/// - Auto-named: `assert_tui_snapshot!(session)`
/// - Named: `assert_tui_snapshot!("name", session)`
/// - Inline: `assert_tui_snapshot!(session, @"expected text")`
#[cfg(feature = "snapshot")]
#[macro_export]
macro_rules! assert_tui_snapshot {
    ($session:expr) => {{
        let __snap = $session.get_screen();
        let __text = $crate::snapshot::format_screen_snapshot(&__snap);
        ::insta::assert_snapshot!(__text);
    }};
    ($name:expr, $session:expr) => {{
        let __snap = $session.get_screen();
        let __text = $crate::snapshot::format_screen_snapshot(&__snap);
        ::insta::assert_snapshot!($name, __text);
    }};
    ($session:expr, @$inline:literal) => {{
        let __snap = $session.get_screen();
        let __text = $crate::snapshot::format_screen_snapshot(&__snap);
        ::insta::assert_snapshot!(__text, @$inline);
    }};
}

/// Assert screen text from a [`ScreenSnapshot`] matches a stored snapshot.
///
/// Requires the `snapshot` feature to be enabled.
///
/// # Variants
///
/// - Auto-named: `assert_screen_snapshot!(snap)`
/// - Named: `assert_screen_snapshot!("name", snap)`
/// - Inline: `assert_screen_snapshot!(snap, @"expected text")`
#[cfg(feature = "snapshot")]
#[macro_export]
macro_rules! assert_screen_snapshot {
    ($snap:expr) => {{
        let __text = $crate::snapshot::format_screen_snapshot(&$snap);
        ::insta::assert_snapshot!(__text);
    }};
    ($name:expr, $snap:expr) => {{
        let __text = $crate::snapshot::format_screen_snapshot(&$snap);
        ::insta::assert_snapshot!($name, __text);
    }};
    ($snap:expr, @$inline:literal) => {{
        let __text = $crate::snapshot::format_screen_snapshot(&$snap);
        ::insta::assert_snapshot!(__text, @$inline);
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a ScreenSnapshot from a vt100 parser with given input.
    fn snap_from_bytes(rows: u16, cols: u16, input: &[u8]) -> ScreenSnapshot {
        let mut parser = vt100::Parser::new(rows, cols, 0);
        parser.process(input);
        ScreenSnapshot::new_for_test(parser.screen().clone(), 0)
    }

    #[test]
    fn test_format_screen_snapshot_basic() {
        let snap = snap_from_bytes(24, 80, b"hello world");
        let text = format_screen_snapshot(&snap);
        assert_eq!(text, "hello world");
    }

    #[test]
    fn test_format_trims_trailing_whitespace() {
        // Write text followed by spaces — row_text will include trailing spaces
        // from the VT100 screen; format_screen_snapshot should trim them.
        let snap = snap_from_bytes(24, 80, b"hello   ");
        let text = format_screen_snapshot(&snap);
        assert_eq!(text, "hello");
    }

    #[test]
    fn test_format_trims_trailing_empty_lines() {
        // 24-row screen with text only on first 2 lines should produce 2 lines.
        let snap = snap_from_bytes(24, 80, b"line one\r\nline two");
        let text = format_screen_snapshot(&snap);
        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "line one");
        assert_eq!(lines[1], "line two");
    }

    #[test]
    fn test_format_empty_screen() {
        let snap = snap_from_bytes(24, 80, b"");
        let text = format_screen_snapshot(&snap);
        assert!(text.is_empty(), "Empty screen should produce empty string");
    }

    #[test]
    fn test_format_multiline() {
        let snap = snap_from_bytes(24, 80, b"first\r\nsecond\r\nthird");
        let text = format_screen_snapshot(&snap);
        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "first");
        assert_eq!(lines[1], "second");
        assert_eq!(lines[2], "third");
    }

    #[test]
    fn test_format_with_metadata() {
        let snap = snap_from_bytes(24, 80, b"hello");
        let text = format_screen_snapshot_with_metadata(&snap);
        assert!(text.starts_with("--- screen 80x24 cursor (0,5) ---\n"));
        assert!(text.contains("hello"));
    }

    #[test]
    fn test_format_with_metadata_cursor_position() {
        // Move cursor to row 1, col 3 by writing text on two lines
        let snap = snap_from_bytes(24, 80, b"abc\r\ndef");
        let text = format_screen_snapshot_with_metadata(&snap);
        // Cursor should be at (1, 3) after writing "def" on the second line
        assert!(
            text.starts_with("--- screen 80x24 cursor (1,3) ---\n"),
            "Unexpected header: {text}"
        );
    }

    #[test]
    fn test_format_preserves_leading_spaces() {
        let snap = snap_from_bytes(24, 80, b"   indented");
        let text = format_screen_snapshot(&snap);
        assert_eq!(text, "   indented");
    }

    #[test]
    fn test_format_with_metadata_empty_screen() {
        let snap = snap_from_bytes(24, 80, b"");
        let text = format_screen_snapshot_with_metadata(&snap);
        // Should still have the header line even with empty body
        assert_eq!(text, "--- screen 80x24 cursor (0,0) ---\n");
    }

    #[test]
    fn test_format_single_line_no_trailing_newline() {
        let snap = snap_from_bytes(24, 80, b"only line");
        let text = format_screen_snapshot(&snap);
        assert!(!text.ends_with('\n'), "Should not end with newline");
        assert_eq!(text, "only line");
    }

    #[test]
    fn test_format_mixed_leading_trailing_whitespace() {
        // Leading spaces preserved, trailing spaces trimmed
        let snap = snap_from_bytes(24, 80, b"  hello  ");
        let text = format_screen_snapshot(&snap);
        assert_eq!(text, "  hello");
    }

    #[test]
    fn test_format_different_terminal_sizes() {
        // Small terminal
        let snap = snap_from_bytes(2, 10, b"tiny");
        let text = format_screen_snapshot(&snap);
        assert_eq!(text, "tiny");

        let meta = format_screen_snapshot_with_metadata(&snap);
        assert!(meta.starts_with("--- screen 10x2 cursor (0,4) ---\n"));
    }
}
