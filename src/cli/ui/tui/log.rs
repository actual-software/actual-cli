use std::collections::VecDeque;

/// Maximum number of lines to retain in the buffer.
const MAX_LINES: usize = 1000;

pub struct LogPane {
    buffer: VecDeque<String>,
    capacity: usize,
}

impl LogPane {
    pub fn new() -> Self {
        Self {
            buffer: VecDeque::new(),
            capacity: MAX_LINES,
        }
    }

    #[cfg(test)]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buffer: VecDeque::new(),
            capacity,
        }
    }

    /// Push a new line into the log buffer. Oldest lines are dropped when capacity is exceeded.
    pub fn push(&mut self, line: String) {
        if self.buffer.len() >= self.capacity {
            self.buffer.pop_front();
        }
        self.buffer.push_back(line);
    }

    /// Returns the number of lines currently in the buffer.
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Returns true if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Render a window of `height` lines from the buffer, each soft-wrapped to
    /// `width` visible chars.  Returns a Vec of strings (top to bottom).
    ///
    /// `scroll_offset` is the number of **display lines** scrolled **up** from
    /// the bottom. Display lines are the post-wrap expanded lines.
    /// - `0` → pinned to the bottom (auto-scroll during sync).
    /// - `n` → the bottom of the visible window is `n` display lines above the
    ///   last display line, letting the user review earlier output.
    ///
    /// The offset is clamped so it never scrolls past the first display line.
    pub fn render_to_string(
        &self,
        height: usize,
        width: usize,
        scroll_offset: usize,
    ) -> Vec<String> {
        let display_lines: Vec<String> = self
            .buffer
            .iter()
            .flat_map(|line| wrap_line(line, width))
            .collect();
        let total = display_lines.len();
        let clamped = scroll_offset.min(total.saturating_sub(1));
        let bottom = total.saturating_sub(clamped);
        let start = bottom.saturating_sub(height);
        display_lines.into_iter().skip(start).take(height).collect()
    }

    /// Maximum scroll offset: how many lines the user can scroll up before the
    /// first buffered line reaches the top of a `height`-row window.
    pub fn max_scroll(&self, height: usize) -> usize {
        self.buffer.len().saturating_sub(height)
    }

    /// Maximum scroll offset accounting for soft-wrapping at `width` columns.
    ///
    /// Sums `wrap_line(line, width).len()` for all buffer lines to get the
    /// total number of display lines, then returns
    /// `total_display_lines.saturating_sub(height)`.
    pub fn max_scroll_wrapped(&self, height: usize, width: usize) -> usize {
        let total: usize = self
            .buffer
            .iter()
            .map(|line| wrap_line(line, width).len())
            .sum();
        total.saturating_sub(height)
    }
}

impl Default for LogPane {
    fn default() -> Self {
        Self::new()
    }
}

/// Soft-wrap a line to `width` visible characters.
///
/// Returns a `Vec<String>` with one element if the line fits within `width`,
/// or multiple elements if it must be broken.
///
/// Breaking strategy:
/// 1. Try to break at word boundaries (spaces). A continuation line has any
///    leading space trimmed.
/// 2. If a single word is wider than `width`, fall back to a hard character-by-
///    character break for that word.
///
/// Uses `console::measure_text_width` for ANSI-safe width measurement so that
/// lines with ANSI escape sequences are measured by their *visible* width.
///
/// Edge case: `width == 0` returns `vec![line.to_string()]` unchanged.
pub fn wrap_line(line: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![line.to_string()];
    }

    // Strip ANSI codes for all width measurements; the stored lines should
    // already be plain text (renderer.rs strips at push time), but we handle
    // ANSI defensively here as well.
    let visible = console::measure_text_width(line);
    if visible <= width {
        return vec![line.to_string()];
    }

    // The line is wider than the pane — need to wrap.
    // Work on the ANSI-stripped version for simplicity (stored lines are plain).
    let plain = console::strip_ansi_codes(line);
    let mut result: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_width: usize = 0;

    // Split into space-separated tokens. We split by spaces and add words one
    // at a time. `split_inclusive(' ')` keeps the trailing space on each token
    // except the last. We process tokens and track whether to add a space
    // separator between words on the same line.
    //
    // Collect words (non-space content) by splitting on ASCII space.
    let words: Vec<&str> = plain.split(' ').collect();

    for word in &words {
        let word_width = console::measure_text_width(word);

        if current_width == 0 {
            // Starting a new display line.
            if word_width <= width {
                current.push_str(word);
                current_width = word_width;
            } else {
                // Word alone exceeds width — hard break it.
                hard_break_into(&mut result, word, width);
                if let Some(last) = result.pop() {
                    current = last;
                    current_width = console::measure_text_width(&current);
                }
            }
        } else {
            // We're mid-line. Adding " " + word would use current_width + 1 + word_width.
            let needed = current_width + 1 + word_width;
            if needed <= width {
                // Fits on current line (with space separator).
                current.push(' ');
                current_width += 1;
                current.push_str(word);
                current_width += word_width;
            } else if word_width <= width {
                // Word fits on a fresh line but not on the current one.
                result.push(current.clone());
                current = word.to_string();
                current_width = word_width;
            } else {
                // Word alone exceeds width — flush current line, then hard break.
                result.push(current.clone());
                current = String::new();
                current_width = 0;
                hard_break_into(&mut result, word, width);
                if let Some(last) = result.pop() {
                    current = last;
                    current_width = console::measure_text_width(&current);
                }
            }
        }
    }

    // Push whatever remains in the current line buffer.
    result.push(current);

    result
}

/// Break `text` into chunks of at most `width` visible characters and append
/// them to `out`. The last chunk may be shorter than `width`; it is pushed
/// even if empty so that the caller has something to continue building from.
fn hard_break_into(out: &mut Vec<String>, text: &str, width: usize) {
    let mut chunk = String::new();
    let mut chunk_width: usize = 0;

    for ch in text.chars() {
        let ch_width = console::measure_text_width(&ch.to_string());
        if chunk_width + ch_width > width {
            out.push(chunk);
            chunk = String::new();
            chunk_width = 0;
        }
        chunk.push(ch);
        chunk_width += ch_width;
    }
    // Always push the last chunk (even if empty) so the caller can continue.
    out.push(chunk);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_is_empty() {
        let pane = LogPane::new();
        assert!(pane.is_empty());
        assert_eq!(pane.len(), 0);
    }

    #[test]
    fn test_default_is_empty() {
        let pane = LogPane::default();
        assert!(pane.is_empty());
        assert_eq!(pane.len(), 0);
    }

    #[test]
    fn test_push_adds_lines() {
        let mut pane = LogPane::new();
        pane.push("line 1".to_string());
        pane.push("line 2".to_string());
        assert_eq!(pane.len(), 2);
        assert!(!pane.is_empty());
    }

    #[test]
    fn test_push_overflow_drops_oldest() {
        let mut pane = LogPane::with_capacity(2);
        pane.push("first".to_string());
        pane.push("second".to_string());
        pane.push("third".to_string());
        // Buffer should only hold 2 items, oldest ("first") dropped
        assert_eq!(pane.len(), 2);
        let rendered = pane.render_to_string(10, 100, 0);
        assert_eq!(rendered[0], "second");
        assert_eq!(rendered[1], "third");
    }

    #[test]
    fn test_render_fewer_lines_than_height() {
        let mut pane = LogPane::new();
        pane.push("line 1".to_string());
        pane.push("line 2".to_string());
        let result = pane.render_to_string(10, 100, 0);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], "line 1");
        assert_eq!(result[1], "line 2");
    }

    #[test]
    fn test_render_more_lines_than_height_auto_scroll() {
        let mut pane = LogPane::new();
        for i in 0..10 {
            pane.push(format!("line {i}"));
        }
        // Ask for only 3 lines — should get last 3
        let result = pane.render_to_string(3, 100, 0);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], "line 7");
        assert_eq!(result[1], "line 8");
        assert_eq!(result[2], "line 9");
    }

    // ── wrap_line tests ──

    #[test]
    fn test_wrap_line_short_line_unchanged() {
        let result = wrap_line("hello", 20);
        assert_eq!(result, vec!["hello"]);
    }

    #[test]
    fn test_wrap_line_exact_width_not_wrapped() {
        let result = wrap_line("hello", 5);
        assert_eq!(result, vec!["hello"]);
    }

    #[test]
    fn test_wrap_line_splits_at_word_boundary() {
        let result = wrap_line("hello world", 7);
        assert_eq!(result.len(), 2);
        for part in &result {
            let w = console::measure_text_width(part);
            assert!(w <= 7);
        }
        // Both words must be present somewhere
        let joined = result.join(" ");
        assert!(joined.contains("hello"));
        assert!(joined.contains("world"));
    }

    #[test]
    fn test_wrap_line_hard_breaks_very_long_word() {
        let word = "a".repeat(20);
        let result = wrap_line(&word, 5);
        // Should produce 4 chunks of 5 chars each
        assert_eq!(result.len(), 4);
        for part in &result {
            let w = console::measure_text_width(part);
            assert!(w <= 5);
        }
    }

    #[test]
    fn test_wrap_line_multiple_words_fit_on_same_line() {
        // "hi there world" at width=10: "hi there" (8) fits, "world" starts new line
        let result = wrap_line("hi there world", 10);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], "hi there");
        assert_eq!(result[1], "world");
    }

    #[test]
    fn test_wrap_line_hard_break_mid_line() {
        // "ab" fits at width=5, then "aaaaaaaaaa" (10 chars) doesn't fit
        // at all on current or new line — must hard-break
        let result = wrap_line("ab aaaaaaaaaa", 5);
        // "ab" → line 1; "aaaaa" → line 2; "aaaaa" → line 3
        assert_eq!(result.len(), 3);
        for part in &result {
            let w = console::measure_text_width(part);
            assert!(w <= 5);
        }
    }

    #[test]
    fn test_wrap_line_empty_line() {
        let result = wrap_line("", 10);
        assert_eq!(result, vec![""]);
    }

    #[test]
    fn test_wrap_line_width_zero() {
        let result = wrap_line("hello", 0);
        assert_eq!(result, vec!["hello"]);
    }

    // ── render_to_string with wrapping tests ──

    #[test]
    fn test_render_wraps_long_line() {
        let mut pane = LogPane::new();
        pane.push("hello world".to_string());
        let result = pane.render_to_string(10, 5, 0);
        // "hello world" at width=5 should produce 2 display lines
        assert_eq!(result.len(), 2);
        for part in &result {
            let w = console::measure_text_width(part);
            assert!(w <= 5);
        }
    }

    #[test]
    fn test_render_wrap_scroll_offset() {
        let mut pane = LogPane::new();
        // Push 5 lines that each wrap to 2 display lines at width=5 → 10 display lines total
        for i in 0..5 {
            pane.push(format!("line{i} end{i}"));
        }
        // With scroll_offset=2, bottom of visible window is 2 display lines above the last
        let result = pane.render_to_string(4, 5, 2);
        // Should return 4 lines starting from display line (10-2-4)=4
        assert_eq!(result.len(), 4);
    }

    #[test]
    fn test_render_no_wrap_when_fits() {
        let mut pane = LogPane::new();
        pane.push("hi".to_string());
        let result = pane.render_to_string(10, 100, 0);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "hi");
    }

    // ── max_scroll_wrapped tests ──

    #[test]
    fn test_max_scroll_wrapped_accounts_for_wrapping() {
        let mut pane = LogPane::new();
        // Push 3 lines that each wrap to 2 display lines at width=5 → 6 display lines
        pane.push("hello world".to_string()); // wraps to 2 at width=5
        pane.push("hello world".to_string());
        pane.push("hello world".to_string());
        let height = 3;
        let max = pane.max_scroll_wrapped(height, 5);
        // total_display=6, height=3 → max=3
        assert_eq!(max, 3);
    }

    #[test]
    fn test_max_scroll_wrapped_no_wrap_matches_old() {
        let mut pane = LogPane::new();
        pane.push("short".to_string());
        pane.push("line".to_string());
        pane.push("here".to_string());
        let height = 2;
        // With large width, no wrapping → max_scroll_wrapped == max_scroll
        assert_eq!(
            pane.max_scroll_wrapped(height, 1000),
            pane.max_scroll(height)
        );
    }

    // ── replaced truncation tests ──

    #[test]
    fn test_no_ellipsis_on_long_line() {
        // Previously tested truncation with ellipsis — now wrapping is used instead.
        let mut pane = LogPane::new();
        pane.push("hello world".to_string());
        let result = pane.render_to_string(10, 7, 0);
        // Must not contain ellipsis in any line
        for line in &result {
            assert!(!line.contains('…'), "unexpected ellipsis in: {:?}", line);
        }
        // Both halves of the original line must appear across the display lines
        let joined = result.join(" ");
        assert!(joined.contains("hello"), "missing 'hello' in: {:?}", result);
        assert!(joined.contains("world"), "missing 'world' in: {:?}", result);
    }

    #[test]
    fn test_truncate_line_no_truncation() {
        // Kept for regression: short line renders unchanged with large width.
        let mut pane = LogPane::new();
        pane.push("hello".to_string());
        let result = pane.render_to_string(10, 10, 0);
        assert_eq!(result[0], "hello");
    }

    #[test]
    fn test_truncate_line_exact_width() {
        let mut pane = LogPane::new();
        pane.push("hello".to_string());
        let result = pane.render_to_string(10, 5, 0);
        assert_eq!(result[0], "hello");
    }

    #[test]
    fn test_truncate_line_strips_ansi_for_measurement() {
        // A line with ANSI codes that is visually 5 chars wide but many bytes long
        let ansi_line = "\x1b[38;2;0;251;126mhello\x1b[0m";
        // Width=10: visually 5 chars — should NOT produce multiple lines
        let result = wrap_line(ansi_line, 10);
        assert_eq!(result.len(), 1);
        assert!(!result[0].contains('…'));

        // Width=3: visually 5 chars > 3 — should wrap/break (no ellipsis)
        let result2 = wrap_line(ansi_line, 3);
        assert!(result2.len() > 1 || console::measure_text_width(&result2[0]) <= 3);
        for part in &result2 {
            assert!(!part.contains('…'), "unexpected ellipsis in: {:?}", part);
        }
    }
}
