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

    /// Render a window of `height` lines from the buffer, each truncated to
    /// `width` visible chars.  Returns a Vec of strings (top to bottom).
    ///
    /// `scroll_offset` is the number of lines scrolled **up** from the bottom.
    /// - `0` → pinned to the bottom (auto-scroll during sync).
    /// - `n` → the bottom of the visible window is `n` lines above the last
    ///   line, letting the user review earlier output.
    ///
    /// The offset is clamped so it never scrolls past the first line.
    pub fn render_to_string(
        &self,
        height: usize,
        width: usize,
        scroll_offset: usize,
    ) -> Vec<String> {
        let total = self.buffer.len();
        let clamped = scroll_offset.min(total.saturating_sub(1));
        let bottom = total.saturating_sub(clamped);
        let start = bottom.saturating_sub(height);
        self.buffer
            .iter()
            .skip(start)
            .take(height)
            .map(|line| truncate_line(line, width))
            .collect()
    }

    /// Maximum scroll offset: how many lines the user can scroll up before the
    /// first buffered line reaches the top of a `height`-row window.
    pub fn max_scroll(&self, height: usize) -> usize {
        self.buffer.len().saturating_sub(height)
    }
}

impl Default for LogPane {
    fn default() -> Self {
        Self::new()
    }
}

/// Truncate a line to `width` visible characters, appending `…` if truncated.
///
/// Uses `console::measure_text_width` to measure visible width (ignoring ANSI
/// escape codes) so that lines containing ANSI sequences are measured correctly.
/// If truncation is needed the ANSI codes are stripped from the output so that
/// the truncated result is always plain text.
fn truncate_line(line: &str, width: usize) -> String {
    let visible = console::measure_text_width(line);
    if visible <= width {
        line.to_string()
    } else {
        // Strip ANSI codes, then take the first (width - 1) visible chars and
        // append an ellipsis so the result fits within `width` columns.
        let stripped = console::strip_ansi_codes(line);
        let truncated: String = stripped.chars().take(width.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
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

    #[test]
    fn test_truncate_line_no_truncation() {
        let result = truncate_line("hello", 10);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_line_exact_width() {
        let result = truncate_line("hello", 5);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_line_truncated() {
        let result = truncate_line("hello world", 7);
        // takes 6 chars + ellipsis
        assert_eq!(result, "hello …");
    }

    #[test]
    fn test_render_with_truncation() {
        let mut pane = LogPane::new();
        pane.push("hello world".to_string());
        let result = pane.render_to_string(5, 7, 0);
        assert_eq!(result[0], "hello …");
    }

    #[test]
    fn test_truncate_line_strips_ansi_for_measurement() {
        // A line with ANSI codes that is visually 5 chars wide but many bytes long
        let ansi_line = "\x1b[38;2;0;251;126mhello\x1b[0m";
        let result = truncate_line(ansi_line, 10); // 10 wide — should NOT truncate
                                                   // Should return the original (fits within width)
        assert!(!result.ends_with('…'));

        let result2 = truncate_line(ansi_line, 3); // 3 wide — should truncate
        assert!(result2.ends_with('…'));
        assert!(!result2.contains('\x1b')); // no ANSI in truncated output
    }
}
