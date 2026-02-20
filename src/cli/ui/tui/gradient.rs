use ratatui::{buffer::Buffer, layout::Rect, style::Style};

const GRADIENT_START: (u8, u8, u8) = (0x00, 0xFB, 0x7E);
const GRADIENT_END: (u8, u8, u8) = (0x17, 0x9C, 0xA9);

fn lerp(a: u8, b: u8, t: f64) -> u8 {
    (f64::from(a) + (f64::from(b) - f64::from(a)) * t).round() as u8
}

/// Returns the gradient RGB color for a vertical position `y_offset` within
/// a box of `height` rows. The gradient runs top→bottom.
fn gradient_rgb(y_offset: u16, height: u16) -> (u8, u8, u8) {
    let t = if height <= 1 {
        0.0
    } else {
        f64::from(y_offset) / f64::from(height - 1)
    };
    (
        lerp(GRADIENT_START.0, GRADIENT_END.0, t),
        lerp(GRADIENT_START.1, GRADIENT_END.1, t),
        lerp(GRADIENT_START.2, GRADIENT_END.2, t),
    )
}

/// Paint the border cells of a `Borders::ALL` box in the given `area` with the
/// green→teal vertical gradient.
///
/// Call this **after** `frame.render_widget()` so the border glyphs are already
/// written to the buffer. Only border-row/column cells are touched; interior
/// content cells are unmodified.
///
/// If the area is too small to have a border, this is a no-op.
pub fn paint_gradient_border(buf: &mut Buffer, area: Rect) {
    if area.width < 2 || area.height < 2 {
        return;
    }
    let x0 = area.x;
    let x1 = area.x + area.width - 1;
    let y0 = area.y;
    let y1 = area.y + area.height - 1;
    let h = area.height;

    for y in y0..=y1 {
        let (r, g, b) = gradient_rgb(y - y0, h);
        let color = ratatui::style::Color::Rgb(r, g, b);
        let style = Style::default().fg(color);

        if y == y0 || y == y1 {
            // Top or bottom border row — paint entire width
            for x in x0..=x1 {
                buf[(x, y)].set_style(style);
            }
        } else {
            // Middle rows — only the left and right border columns
            buf[(x0, y)].set_style(style);
            buf[(x1, y)].set_style(style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn make_buf(width: u16, height: u16) -> Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.current_buffer_mut().clone()
    }

    fn is_rgb(color: Option<ratatui::style::Color>) -> bool {
        matches!(color, Some(ratatui::style::Color::Rgb(_, _, _)))
    }

    #[test]
    fn test_lerp_endpoints() {
        assert_eq!(lerp(0, 255, 0.0), 0);
        assert_eq!(lerp(0, 255, 1.0), 255);
    }

    #[test]
    fn test_lerp_midpoint() {
        // lerp(0, 100, 0.5) = 50
        assert_eq!(lerp(0, 100, 0.5), 50);
    }

    #[test]
    fn test_gradient_rgb_start() {
        let (r, g, b) = gradient_rgb(0, 10);
        assert_eq!((r, g, b), GRADIENT_START);
    }

    #[test]
    fn test_gradient_rgb_end() {
        let (r, g, b) = gradient_rgb(9, 10);
        assert_eq!((r, g, b), GRADIENT_END);
    }

    #[test]
    fn test_gradient_rgb_single_row() {
        // height <= 1 → t = 0.0 → returns GRADIENT_START
        let (r, g, b) = gradient_rgb(0, 1);
        assert_eq!((r, g, b), GRADIENT_START);
    }

    #[test]
    fn test_paint_gradient_border_too_small() {
        // Areas smaller than 2×2 should be a no-op (no panic)
        let mut buf = make_buf(10, 10);
        paint_gradient_border(&mut buf, Rect::new(0, 0, 1, 5));
        paint_gradient_border(&mut buf, Rect::new(0, 0, 5, 1));
        paint_gradient_border(&mut buf, Rect::new(0, 0, 0, 0));
    }

    #[test]
    fn test_paint_gradient_border_colors_top_row() {
        let mut buf = make_buf(10, 5);
        let area = Rect::new(0, 0, 10, 5);
        paint_gradient_border(&mut buf, area);

        // Top row should have GRADIENT_START color on all cells
        let expected_color =
            ratatui::style::Color::Rgb(GRADIENT_START.0, GRADIENT_START.1, GRADIENT_START.2);
        for x in 0..10u16 {
            assert_eq!(
                buf[(x, 0u16)].style().fg,
                Some(expected_color),
                "top row cell ({x}, 0) should have gradient start color"
            );
        }
    }

    #[test]
    fn test_paint_gradient_border_colors_bottom_row() {
        let mut buf = make_buf(10, 5);
        let area = Rect::new(0, 0, 10, 5);
        paint_gradient_border(&mut buf, area);

        // Bottom row should have GRADIENT_END color on all cells
        let expected_color =
            ratatui::style::Color::Rgb(GRADIENT_END.0, GRADIENT_END.1, GRADIENT_END.2);
        for x in 0..10u16 {
            assert_eq!(
                buf[(x, 4u16)].style().fg,
                Some(expected_color),
                "bottom row cell ({x}, 4) should have gradient end color"
            );
        }
    }

    #[test]
    fn test_paint_gradient_border_middle_rows_only_sides() {
        let mut buf = make_buf(10, 5);
        let area = Rect::new(0, 0, 10, 5);
        paint_gradient_border(&mut buf, area);

        // Middle rows (y=1,2,3): only x=0 and x=9 should have an RGB gradient color;
        // interior cells (x=1..8) should NOT have an RGB color set by gradient.
        for y in 1..=3u16 {
            assert!(
                is_rgb(buf[(0u16, y)].style().fg),
                "left border at y={y} should have an Rgb gradient color"
            );
            assert!(
                is_rgb(buf[(9u16, y)].style().fg),
                "right border at y={y} should have an Rgb gradient color"
            );

            for x in 1u16..9u16 {
                assert!(
                    !is_rgb(buf[(x, y)].style().fg),
                    "interior cell ({x}, {y}) should not be painted with Rgb by gradient"
                );
            }
        }
    }

    #[test]
    fn test_paint_gradient_border_offset_area() {
        // Test that an area not starting at (0,0) is handled correctly
        let mut buf = make_buf(20, 10);
        let area = Rect::new(5, 3, 10, 4);
        paint_gradient_border(&mut buf, area);

        // Top row of area (y=3) — all cells x=5..14 should have gradient start
        let expected_top =
            ratatui::style::Color::Rgb(GRADIENT_START.0, GRADIENT_START.1, GRADIENT_START.2);
        for x in 5..15u16 {
            assert_eq!(
                buf[(x, 3u16)].style().fg,
                Some(expected_top),
                "top border cell ({x}, 3) should have gradient start"
            );
        }

        // Bottom row of area (y=6) — all cells should have gradient end
        let expected_bottom =
            ratatui::style::Color::Rgb(GRADIENT_END.0, GRADIENT_END.1, GRADIENT_END.2);
        for x in 5..15u16 {
            assert_eq!(
                buf[(x, 6u16)].style().fg,
                Some(expected_bottom),
                "bottom border cell ({x}, 6) should have gradient end"
            );
        }
    }
}
