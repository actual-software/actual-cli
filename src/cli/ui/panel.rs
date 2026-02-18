//! Box-drawing panel helpers for consistent CLI output framing.
//!
//! The [`Panel`] builder produces bordered, box-drawn panels using Unicode
//! box-drawing characters. Panel rendering is a **pure function** — it does
//! not query the terminal. Callers determine width and pass it to
//! [`Panel::render`].
//!
//! # Width detection (call-site responsibility)
//!
//! ```ignore
//! let width = console::Term::stdout().size_checked()
//!     .map(|(_, cols)| cols as usize)
//!     .unwrap_or(80)
//!     .min(90);
//! let output = panel.render(width);
//! ```
//!
//! # NO_COLOR behaviour
//!
//! Box-drawing characters are plain Unicode and render identically regardless
//! of `NO_COLOR`. Only styled content inside panels is affected. Panel borders
//! use [`theme::border()`](super::theme::border) (dim), which respects
//! `NO_COLOR` automatically via `console::style()`.

use std::fmt::Write;

use super::theme;

/// Minimum panel width. Below this threshold, [`Panel::render`] degrades to
/// plain unframed text (content lines without borders).
const MIN_PANEL_WIDTH: usize = 40;

/// A row inside a [`Panel`].
enum PanelRow {
    /// Full-width content line.
    Line(String),
    /// Key-value pair with optional right-aligned annotation.
    KeyValue {
        key: String,
        value: String,
        annotation: Option<String>,
    },
    /// Horizontal separator: `├───┤`.
    Separator,
}

/// A box-drawn panel with optional title and mixed content rows.
///
/// Use the builder methods to assemble content, then call [`Panel::render`]
/// with the desired width.
pub struct Panel {
    title: Option<String>,
    rows: Vec<PanelRow>,
}

impl Panel {
    /// Create a new untitled panel.
    pub fn new() -> Self {
        Self {
            title: None,
            rows: Vec::new(),
        }
    }

    /// Create a new panel with a title embedded in the top border.
    pub fn titled(title: &str) -> Self {
        Self {
            title: Some(title.to_string()),
            rows: Vec::new(),
        }
    }

    /// Add a full-width content line.
    pub fn line(mut self, text: &str) -> Self {
        self.rows.push(PanelRow::Line(text.to_string()));
        self
    }

    /// Add a key-value row.
    pub fn kv(mut self, key: &str, value: &str) -> Self {
        self.rows.push(PanelRow::KeyValue {
            key: key.to_string(),
            value: value.to_string(),
            annotation: None,
        });
        self
    }

    /// Add a key-value row with a right-aligned annotation.
    pub fn kv_annotated(mut self, key: &str, value: &str, annotation: &str) -> Self {
        self.rows.push(PanelRow::KeyValue {
            key: key.to_string(),
            value: value.to_string(),
            annotation: Some(annotation.to_string()),
        });
        self
    }

    /// Add a horizontal separator (`├───┤`).
    pub fn separator(mut self) -> Self {
        self.rows.push(PanelRow::Separator);
        self
    }

    /// Add a footer line rendered above the bottom border.
    pub fn footer(mut self, text: &str) -> Self {
        self.rows.push(PanelRow::Line(text.to_string()));
        self
    }

    /// Render the panel as a string at the given `width` (in columns).
    ///
    /// If `width < MIN_PANEL_WIDTH` (40), the panel degrades to plain unframed
    /// text — just the content lines without borders.
    pub fn render(&self, width: usize) -> String {
        if width < MIN_PANEL_WIDTH {
            return self.render_plain();
        }

        let mut out = String::new();

        // Inner width is panel width minus the border + padding on each side:
        // "│ " (2) on the left and " │" (2) on the right = 4 total.
        let inner = width.saturating_sub(4);

        // ── Top border ──
        out.push_str(&self.render_top_border(width));
        out.push('\n');

        // ── Content rows ──
        for row in &self.rows {
            match row {
                PanelRow::Line(text) => {
                    let line = Self::fit_content(text, inner);
                    let _ = writeln!(
                        out,
                        "{} {}{} {}",
                        theme::border("│"),
                        line,
                        Self::padding(inner, Self::visual_width(text).min(inner)),
                        theme::border("│"),
                    );
                }
                PanelRow::KeyValue {
                    key,
                    value,
                    annotation,
                } => {
                    Self::render_kv(&mut out, key, value, annotation.as_deref(), inner);
                }
                PanelRow::Separator => {
                    let fill: String = "─".repeat(inner + 2);
                    let _ = writeln!(
                        out,
                        "{}{}{}",
                        theme::border("├"),
                        theme::border(fill),
                        theme::border("┤")
                    );
                }
            }
        }

        // ── Bottom border ──
        let bottom_fill: String = "─".repeat(width.saturating_sub(2));
        let _ = write!(
            out,
            "{}{}{}",
            theme::border("└"),
            theme::border(bottom_fill),
            theme::border("┘")
        );

        out
    }

    /// Render the top border, embedding the title if present.
    fn render_top_border(&self, width: usize) -> String {
        match &self.title {
            Some(title) => {
                // Format: ┌─ Title ─...─┐
                // Total: ┌(1) + ─ (2) + title + " "(1) + dashes + ┐(1) = width
                // So dashes = width - 5 - title_visual
                let title_visual = console::measure_text_width(title);
                let overhead = 5; // ┌(1) + "─ "(2) + " "(1) + ┐(1)

                let available_for_title = width.saturating_sub(overhead + 1); // need at least 1 dash

                if title_visual > available_for_title {
                    // Title is too long — truncate it.
                    let truncated = console::truncate_str(title, available_for_title, "…");
                    let truncated_width = console::measure_text_width(&truncated);
                    let dashes = width.saturating_sub(overhead + truncated_width);
                    let dash_str: String = "─".repeat(dashes);
                    format!(
                        "{}{}{}{}{}{}",
                        theme::border("┌"),
                        theme::border("─ "),
                        theme::heading(&*truncated),
                        theme::border(" "),
                        theme::border(dash_str),
                        theme::border("┐"),
                    )
                } else {
                    let dashes = width.saturating_sub(overhead + title_visual);
                    let dash_str: String = "─".repeat(dashes);
                    format!(
                        "{}{}{}{}{}{}",
                        theme::border("┌"),
                        theme::border("─ "),
                        theme::heading(title),
                        theme::border(" "),
                        theme::border(dash_str),
                        theme::border("┐"),
                    )
                }
            }
            None => {
                let fill: String = "─".repeat(width.saturating_sub(2));
                format!(
                    "{}{}{}",
                    theme::border("┌"),
                    theme::border(fill),
                    theme::border("┐")
                )
            }
        }
    }

    /// Render a key-value row, with optional right-aligned annotation.
    fn render_kv(out: &mut String, key: &str, value: &str, annotation: Option<&str>, inner: usize) {
        // Format: "key: value" or "key: value    annotation"
        let kv_text = format!("{key}: {value}");
        let kv_width = console::measure_text_width(&kv_text);

        match annotation {
            Some(ann) => {
                let ann_width = console::measure_text_width(ann);
                // We need: kv_text + at least 2 spaces + annotation
                let total_content = kv_width + 2 + ann_width;

                if total_content <= inner {
                    // Fits: pad between kv and annotation
                    let gap = inner - kv_width - ann_width;
                    let gap_str: String = " ".repeat(gap);
                    let _ = writeln!(
                        out,
                        "{} {}{}{} {}",
                        theme::border("│"),
                        kv_text,
                        gap_str,
                        ann,
                        theme::border("│"),
                    );
                } else {
                    // Doesn't fit: truncate the kv portion, drop annotation
                    let line = Self::fit_content(&kv_text, inner);
                    let vis = Self::visual_width(&kv_text).min(inner);
                    let _ = writeln!(
                        out,
                        "{} {}{} {}",
                        theme::border("│"),
                        line,
                        Self::padding(inner, vis),
                        theme::border("│"),
                    );
                }
            }
            None => {
                let line = Self::fit_content(&kv_text, inner);
                let vis = Self::visual_width(&kv_text).min(inner);
                let _ = writeln!(
                    out,
                    "{} {}{} {}",
                    theme::border("│"),
                    line,
                    Self::padding(inner, vis),
                    theme::border("│"),
                );
            }
        }
    }

    /// Fit content to the inner width, truncating with "…" if necessary.
    fn fit_content(text: &str, inner: usize) -> String {
        let vis = Self::visual_width(text);
        if vis > inner {
            console::truncate_str(text, inner, "…").into_owned()
        } else {
            text.to_string()
        }
    }

    /// Right-pad to fill the remaining inner width.
    fn padding(inner: usize, used: usize) -> String {
        let remaining = inner.saturating_sub(used);
        " ".repeat(remaining)
    }

    /// Measure the visual width of text, handling ANSI escape codes.
    fn visual_width(text: &str) -> usize {
        console::measure_text_width(text)
    }

    /// Plain-text fallback for narrow terminals (width < MIN_PANEL_WIDTH).
    fn render_plain(&self) -> String {
        let mut out = String::new();

        if let Some(title) = &self.title {
            let _ = writeln!(out, "{title}");
        }

        for row in &self.rows {
            match row {
                PanelRow::Line(text) => {
                    let _ = writeln!(out, "{text}");
                }
                PanelRow::KeyValue {
                    key,
                    value,
                    annotation,
                } => {
                    if let Some(ann) = annotation {
                        let _ = writeln!(out, "{key}: {value}  {ann}");
                    } else {
                        let _ = writeln!(out, "{key}: {value}");
                    }
                }
                PanelRow::Separator => {
                    let _ = writeln!(out, "---");
                }
            }
        }

        out
    }
}

impl Default for Panel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strip ANSI codes from rendered output for assertion.
    fn plain(rendered: &str) -> String {
        console::strip_ansi_codes(rendered).into_owned()
    }

    // ── 1. Empty panel ──

    #[test]
    fn empty_panel_renders_top_and_bottom_borders() {
        let rendered = Panel::new().render(80);
        let p = plain(&rendered);
        let lines: Vec<&str> = p.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "empty panel should have 2 lines (top + bottom)"
        );
        assert!(lines[0].starts_with('┌'), "top border should start with ┌");
        assert!(lines[0].ends_with('┐'), "top border should end with ┐");
        assert!(
            lines[1].starts_with('└'),
            "bottom border should start with └"
        );
        assert!(lines[1].ends_with('┘'), "bottom border should end with ┘");
        assert_eq!(
            lines[0].chars().count(),
            80,
            "top border should be 80 chars wide"
        );
        assert_eq!(
            lines[1].chars().count(),
            80,
            "bottom border should be 80 chars wide"
        );
    }

    // ── 2. Titled panel ──

    #[test]
    fn titled_panel_embeds_title_in_top_border() {
        let rendered = Panel::titled("Config").render(80);
        let p = plain(&rendered);
        let lines: Vec<&str> = p.lines().collect();
        assert!(
            lines[0].starts_with("┌─ Config "),
            "top border should start with '┌─ Config '"
        );
        assert!(lines[0].ends_with('┐'), "top border should end with ┐");
        assert_eq!(lines[0].chars().count(), 80);
    }

    // ── 3. Single line ──

    #[test]
    fn single_line_panel_renders_content_between_borders() {
        let rendered = Panel::new().line("hello").render(80);
        let p = plain(&rendered);
        let lines: Vec<&str> = p.lines().collect();
        assert_eq!(lines.len(), 3, "should have top + 1 content + bottom");
        assert!(
            lines[1].starts_with("│ "),
            "content line should start with '│ '"
        );
        assert!(
            lines[1].ends_with(" │"),
            "content line should end with ' │'"
        );
        assert!(lines[1].contains("hello"), "content should contain 'hello'");
        assert_eq!(lines[1].chars().count(), 80);
    }

    // ── 4. Key-value rows ──

    #[test]
    fn kv_rows_show_key_colon_value() {
        let rendered = Panel::new()
            .kv("Name", "actual-cli")
            .kv("Version", "1.0.0")
            .render(80);
        let p = plain(&rendered);
        assert!(
            p.contains("Name: actual-cli"),
            "should contain 'Name: actual-cli'"
        );
        assert!(
            p.contains("Version: 1.0.0"),
            "should contain 'Version: 1.0.0'"
        );

        // Verify borders on content lines
        let lines: Vec<&str> = p.lines().collect();
        for line in &lines[1..lines.len() - 1] {
            assert!(
                line.starts_with("│ "),
                "kv line should start with '│ ': {line}"
            );
            assert!(line.ends_with(" │"), "kv line should end with ' │': {line}");
            assert_eq!(line.chars().count(), 80);
        }
    }

    // ── 5. Key-value with annotation ──

    #[test]
    fn kv_annotated_shows_right_aligned_annotation() {
        let rendered = Panel::new()
            .kv_annotated("Status", "synced", "(3s ago)")
            .render(80);
        let p = plain(&rendered);
        let content_line = p.lines().nth(1).expect("should have content line");
        assert!(
            content_line.contains("Status: synced"),
            "should contain key-value: {content_line}"
        );
        assert!(
            content_line.contains("(3s ago)"),
            "should contain annotation: {content_line}"
        );
        // Annotation should be to the right of the value
        let kv_pos = content_line.find("Status: synced").unwrap();
        let ann_pos = content_line.find("(3s ago)").unwrap();
        assert!(
            ann_pos > kv_pos + "Status: synced".len(),
            "annotation should be right of value"
        );
    }

    // ── 6. Separator ──

    #[test]
    fn separator_renders_t_junctions() {
        let rendered = Panel::new()
            .line("above")
            .separator()
            .line("below")
            .render(80);
        let p = plain(&rendered);
        let lines: Vec<&str> = p.lines().collect();
        // Find the separator line (should be line index 2)
        let sep = lines[2];
        assert!(sep.starts_with('├'), "separator should start with ├: {sep}");
        assert!(sep.ends_with('┤'), "separator should end with ┤: {sep}");
        assert!(
            sep.chars().all(|c| c == '├' || c == '─' || c == '┤'),
            "separator should only contain ├, ─, ┤: {sep}"
        );
        assert_eq!(sep.chars().count(), 80);
    }

    // ── 7. Footer ──

    #[test]
    fn footer_renders_above_bottom_border() {
        let rendered = Panel::new()
            .line("content")
            .footer("Total: 5 items")
            .render(80);
        let p = plain(&rendered);
        let lines: Vec<&str> = p.lines().collect();
        // Footer is the second-to-last line (before bottom border)
        let footer_line = lines[lines.len() - 2];
        assert!(
            footer_line.contains("Total: 5 items"),
            "footer should contain text: {footer_line}"
        );
        assert!(
            footer_line.starts_with("│ "),
            "footer should have left border"
        );
        assert!(
            footer_line.ends_with(" │"),
            "footer should have right border"
        );
    }

    // ── 8. Width 80 vs 120 ──

    #[test]
    fn width_adjusts_border_and_padding() {
        let panel = Panel::new().line("test");

        let r80 = plain(&panel.render(80));
        let r120 = plain(&panel.render(120));

        let lines_80: Vec<&str> = r80.lines().collect();
        let lines_120: Vec<&str> = r120.lines().collect();

        // All lines in each rendering should match their target width
        for line in &lines_80 {
            assert_eq!(
                line.chars().count(),
                80,
                "width=80: line should be 80 chars: '{line}'"
            );
        }
        for line in &lines_120 {
            assert_eq!(
                line.chars().count(),
                120,
                "width=120: line should be 120 chars: '{line}'"
            );
        }
    }

    // ── 9. Width below MIN_PANEL_WIDTH ──

    #[test]
    fn narrow_width_degrades_to_plain_text() {
        let rendered = Panel::titled("Config")
            .line("hello")
            .kv("key", "val")
            .separator()
            .footer("done")
            .render(30);

        // Should NOT contain box-drawing characters
        assert!(!rendered.contains('┌'), "plain mode should not have ┌");
        assert!(!rendered.contains('│'), "plain mode should not have │");
        assert!(!rendered.contains('└'), "plain mode should not have └");

        // Should contain content
        assert!(rendered.contains("Config"), "should contain title");
        assert!(rendered.contains("hello"), "should contain line content");
        assert!(rendered.contains("key: val"), "should contain kv");
        assert!(rendered.contains("---"), "separator should degrade to ---");
        assert!(rendered.contains("done"), "should contain footer");
    }

    // ── 10. Styled content (ANSI-aware width) ──

    #[test]
    fn styled_content_does_not_break_alignment() {
        let styled = format!("{}", theme::success("OK"));
        let rendered = Panel::new().line(&styled).render(80);
        let p = plain(&rendered);
        let lines: Vec<&str> = p.lines().collect();

        // Content line should still be exactly 80 chars (ANSI codes stripped)
        assert_eq!(
            lines[1].chars().count(),
            80,
            "styled content line should be 80 chars after stripping ANSI"
        );
        assert!(lines[1].contains("OK"), "content should contain 'OK'");
    }

    // ── 11. Title truncation ──

    #[test]
    fn long_title_is_truncated() {
        let long_title = "This Is A Very Long Title That Should Be Truncated";
        let rendered = Panel::titled(long_title).render(40);
        let p = plain(&rendered);
        let lines: Vec<&str> = p.lines().collect();
        let top = lines[0];

        assert_eq!(
            top.chars().count(),
            40,
            "top border should be exactly 40 chars: '{top}'"
        );
        assert!(top.starts_with("┌─ "), "should start with ┌─ : '{top}'");
        assert!(top.ends_with('┐'), "should end with ┐: '{top}'");
        assert!(
            top.contains('…'),
            "truncated title should contain …: '{top}'"
        );
    }

    // ── 12. Content truncation ──

    #[test]
    fn long_content_is_truncated_with_ellipsis() {
        let long_text = "A".repeat(200);
        let rendered = Panel::new().line(&long_text).render(50);
        let p = plain(&rendered);
        let content_line = p.lines().nth(1).expect("should have content line");

        assert_eq!(
            content_line.chars().count(),
            50,
            "content line should be 50 chars: '{content_line}'"
        );
        assert!(
            content_line.contains('…'),
            "truncated content should contain …: '{content_line}'"
        );
    }

    // ── Additional coverage tests ──

    #[test]
    fn default_creates_empty_panel() {
        let panel = Panel::default();
        let rendered = panel.render(80);
        let p = plain(&rendered);
        let lines: Vec<&str> = p.lines().collect();
        assert_eq!(lines.len(), 2, "default panel should have 2 lines");
    }

    #[test]
    fn kv_annotated_overflow_drops_annotation() {
        // Use a very long key+value so annotation doesn't fit at width 40
        let long_value = "A".repeat(40);
        let rendered = Panel::new()
            .kv_annotated("Key", &long_value, "(annotation)")
            .render(40);
        let p = plain(&rendered);
        let content_line = p.lines().nth(1).expect("should have content line");

        // Line should be exactly 40 chars
        assert_eq!(
            content_line.chars().count(),
            40,
            "content line should be 40 chars: '{content_line}'"
        );
        // Annotation should not appear (dropped because it doesn't fit)
        assert!(
            !content_line.contains("(annotation)"),
            "annotation should be dropped when it doesn't fit: '{content_line}'"
        );
        // Content should be truncated
        assert!(
            content_line.contains('…'),
            "overflowing kv should be truncated: '{content_line}'"
        );
    }

    #[test]
    fn plain_text_without_title() {
        let rendered = Panel::new().line("hello").kv("key", "val").render(30);
        // Should not start with a title line
        assert!(
            rendered.starts_with("hello"),
            "plain text without title should start with first content line"
        );
        assert!(rendered.contains("key: val"), "should contain kv");
    }

    #[test]
    fn plain_text_kv_annotated() {
        let rendered = Panel::new()
            .kv_annotated("Status", "ok", "(fast)")
            .render(30);
        assert!(
            rendered.contains("Status: ok  (fast)"),
            "plain text kv_annotated should show annotation: '{rendered}'"
        );
    }

    #[test]
    fn plain_text_kv_without_annotation() {
        let rendered = Panel::new().kv("key", "val").render(30);
        assert!(
            rendered.contains("key: val"),
            "plain text kv should show key: val"
        );
        // Should not have extra spaces from annotation
        assert!(
            rendered.trim_end().ends_with("key: val"),
            "should end with just key: val"
        );
    }

    #[test]
    fn long_footer_is_truncated() {
        let long_footer = "F".repeat(200);
        let rendered = Panel::new().footer(&long_footer).render(50);
        let p = plain(&rendered);
        let footer_line = p.lines().nth(1).expect("should have footer line");
        assert_eq!(
            footer_line.chars().count(),
            50,
            "footer line should be 50 chars: '{footer_line}'"
        );
        assert!(
            footer_line.contains('…'),
            "long footer should be truncated: '{footer_line}'"
        );
    }

    #[test]
    fn kv_long_value_is_truncated() {
        let long_val = "V".repeat(200);
        let rendered = Panel::new().kv("Key", &long_val).render(50);
        let p = plain(&rendered);
        let content_line = p.lines().nth(1).expect("should have content line");
        assert_eq!(
            content_line.chars().count(),
            50,
            "kv line should be 50 chars: '{content_line}'"
        );
        assert!(
            content_line.contains('…'),
            "long kv should be truncated: '{content_line}'"
        );
    }

    // ── 13. Multiple mixed rows ──

    #[test]
    fn multiple_mixed_rows_render_correctly() {
        let rendered = Panel::titled("Summary")
            .line("Overview of results")
            .separator()
            .kv("Files", "12")
            .kv("Rules", "48")
            .kv_annotated("Status", "ok", "(3.2s)")
            .separator()
            .footer("Done in 3.2s")
            .render(60);
        let p = plain(&rendered);
        let lines: Vec<&str> = p.lines().collect();

        // Expected: top + line + sep + 3 kvs + sep + footer + bottom = 9 lines
        assert_eq!(lines.len(), 9, "expected 9 lines");

        // All lines should be exactly 60 chars
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(
                line.chars().count(),
                60,
                "line {i} should be 60 chars: '{line}'"
            );
        }

        // Verify content
        assert!(p.contains("Summary"), "should contain title");
        assert!(
            p.contains("Overview of results"),
            "should contain line content"
        );
        assert!(p.contains("Files: 12"), "should contain Files kv");
        assert!(p.contains("Rules: 48"), "should contain Rules kv");
        assert!(p.contains("Status: ok"), "should contain Status kv");
        assert!(p.contains("(3.2s)"), "should contain annotation");
        assert!(p.contains("Done in 3.2s"), "should contain footer");
    }
}
