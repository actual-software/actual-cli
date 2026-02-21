use image::{Rgba, RgbaImage};
use std::path::Path;

use crate::TuiTestError;

use super::color::ColorScheme;
use super::font::FontAtlas;

/// Configuration for screen rendering.
pub struct RenderConfig {
    /// Font size in pixels.
    pub font_size: f32,
    /// Terminal color scheme.
    pub color_scheme: ColorScheme,
    /// Padding around the rendered image in pixels.
    pub padding: u32,
    /// Whether to render the cursor.
    pub show_cursor: bool,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            color_scheme: ColorScheme::default(),
            padding: 8,
            show_cursor: true,
        }
    }
}

/// Renders a `vt100::Screen` to a PNG image.
pub struct ScreenRenderer {
    config: RenderConfig,
    font_atlas: FontAtlas,
}

impl ScreenRenderer {
    /// Create a new renderer with default configuration.
    pub fn new() -> Self {
        Self::with_config(RenderConfig::default())
    }

    /// Create a new renderer with the given configuration.
    pub fn with_config(config: RenderConfig) -> Self {
        let font_atlas = FontAtlas::new(config.font_size);
        Self { config, font_atlas }
    }

    /// Return the cell width in pixels.
    pub fn cell_width(&self) -> u32 {
        self.font_atlas.cell_width
    }

    /// Return the cell height in pixels.
    pub fn cell_height(&self) -> u32 {
        self.font_atlas.cell_height
    }

    /// Render a `vt100::Screen` to an RGBA image.
    pub fn render(&mut self, screen: &vt100::Screen) -> RgbaImage {
        let (rows, cols) = screen.size();
        let cw = self.font_atlas.cell_width;
        let ch = self.font_atlas.cell_height;
        let ascent = self.font_atlas.ascent;
        let pad = self.config.padding;

        let img_w = pad * 2 + cols as u32 * cw;
        let img_h = pad * 2 + rows as u32 * ch;

        let bg = self.config.color_scheme.bg;
        let mut img = RgbaImage::from_pixel(img_w, img_h, Rgba([bg[0], bg[1], bg[2], 255]));

        for row in 0..rows {
            for col in 0..cols {
                if let Some(cell) = screen.cell(row, col) {
                    // Skip wide continuation cells
                    if cell.is_wide_continuation() {
                        continue;
                    }

                    let (fg_rgba, bg_rgba) = self.resolve_cell_colors(cell);

                    // Draw cell background
                    let cell_x = pad + col as u32 * cw;
                    let cell_y = pad + row as u32 * ch;
                    let cell_w = if cell.is_wide() { cw * 2 } else { cw };
                    Self::fill_rect(&mut img, cell_x, cell_y, cell_w, ch, bg_rgba);

                    // Render character
                    let contents = cell.contents();
                    if !contents.is_empty() {
                        for c in contents.chars() {
                            if c == ' ' || c == '\0' {
                                continue;
                            }
                            let glyph = self.font_atlas.rasterize(c, cell.bold());
                            let gx = cell_x as i32 + glyph.metrics.xmin;
                            let gy = cell_y as i32 + ascent.ceil() as i32
                                - glyph.metrics.height as i32
                                - glyph.metrics.ymin;
                            Self::composite_glyph(&mut img, glyph, gx, gy, fg_rgba);
                        }
                    }

                    // Underline
                    if cell.underline() {
                        let uy = cell_y + ascent.ceil() as u32 + 2;
                        Self::fill_rect(&mut img, cell_x, uy, cell_w, 1, fg_rgba);
                    }
                }
            }
        }

        // Cursor
        if self.config.show_cursor {
            let (cr, cc) = screen.cursor_position();
            let cx = pad + cc as u32 * cw;
            let cy = pad + cr as u32 * ch;
            // Draw cursor as a block with fg color of the cell
            if let Some(cell) = screen.cell(cr, cc) {
                let (fg, _bg) = self.resolve_cell_colors(cell);
                Self::fill_rect(&mut img, cx, cy, cw, ch, fg);
            }
        }

        img
    }

    /// Render to a PNG file.
    pub fn render_to_png(
        &mut self,
        screen: &vt100::Screen,
        path: impl AsRef<Path>,
    ) -> Result<(), TuiTestError> {
        let img = self.render(screen);
        img.save(path)
            .map_err(|e| TuiTestError::Render(e.to_string()))
    }

    /// Render to PNG bytes.
    pub fn render_to_png_bytes(&mut self, screen: &vt100::Screen) -> Result<Vec<u8>, TuiTestError> {
        let img = self.render(screen);
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png)
            .map_err(|e| TuiTestError::Render(e.to_string()))?;
        Ok(buf.into_inner())
    }

    fn resolve_cell_colors(&self, cell: &vt100::Cell) -> (Rgba<u8>, Rgba<u8>) {
        let scheme = &self.config.color_scheme;
        let mut fg = scheme.resolve(cell.fgcolor(), true);
        let mut bg = scheme.resolve(cell.bgcolor(), false);

        // Bold + indexed 0-7: promote to bright
        if cell.bold() {
            if let vt100::Color::Idx(idx @ 0..=7) = cell.fgcolor() {
                fg = scheme.idx_to_rgb(idx + 8);
            }
        }

        // Inverse: swap
        if cell.inverse() {
            std::mem::swap(&mut fg, &mut bg);
        }

        (
            Rgba([fg[0], fg[1], fg[2], 255]),
            Rgba([bg[0], bg[1], bg[2], 255]),
        )
    }

    fn fill_rect(img: &mut RgbaImage, x: u32, y: u32, w: u32, h: u32, color: Rgba<u8>) {
        for py in y..y.saturating_add(h).min(img.height()) {
            for px in x..x.saturating_add(w).min(img.width()) {
                img.put_pixel(px, py, color);
            }
        }
    }

    fn composite_glyph(
        img: &mut RgbaImage,
        glyph: &super::font::RasterizedGlyph,
        gx: i32,
        gy: i32,
        fg: Rgba<u8>,
    ) {
        for row in 0..glyph.metrics.height {
            for col in 0..glyph.metrics.width {
                let coverage = glyph.coverage[row * glyph.metrics.width + col];
                if coverage == 0 {
                    continue;
                }

                let px = gx + col as i32;
                let py = gy + row as i32;
                if px < 0 || py < 0 || px >= img.width() as i32 || py >= img.height() as i32 {
                    continue;
                }

                let pixel = img.get_pixel_mut(px as u32, py as u32);
                let alpha = coverage as u16;
                let inv_alpha = 255 - alpha;
                pixel[0] = ((fg[0] as u16 * alpha + pixel[0] as u16 * inv_alpha) / 255) as u8;
                pixel[1] = ((fg[1] as u16 * alpha + pixel[1] as u16 * inv_alpha) / 255) as u8;
                pixel[2] = ((fg[2] as u16 * alpha + pixel[2] as u16 * inv_alpha) / 255) as u8;
                pixel[3] = 255;
            }
        }
    }
}

impl Default for ScreenRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    /// Helper: create a vt100 parser, process bytes, return the screen.
    fn screen_from_bytes(rows: u16, cols: u16, data: &[u8]) -> vt100::Screen {
        let mut parser = vt100::Parser::new(rows, cols, 0);
        parser.process(data);
        parser.screen().clone()
    }

    #[test]
    fn test_render_correct_dimensions() {
        let mut renderer = ScreenRenderer::new();
        let screen = screen_from_bytes(24, 80, b"");
        let img = renderer.render(&screen);

        let cw = renderer.cell_width();
        let ch = renderer.cell_height();
        let pad = 8u32; // default padding

        let expected_w = pad * 2 + 80 * cw;
        let expected_h = pad * 2 + 24 * ch;
        assert_eq!(img.width(), expected_w);
        assert_eq!(img.height(), expected_h);
    }

    #[test]
    fn test_render_custom_size() {
        let config = RenderConfig {
            padding: 16,
            ..RenderConfig::default()
        };
        let mut renderer = ScreenRenderer::with_config(config);
        let screen = screen_from_bytes(10, 40, b"");
        let img = renderer.render(&screen);

        let cw = renderer.cell_width();
        let ch = renderer.cell_height();
        let expected_w = 16 * 2 + 40 * cw;
        let expected_h = 16 * 2 + 10 * ch;
        assert_eq!(img.width(), expected_w);
        assert_eq!(img.height(), expected_h);
    }

    #[test]
    fn test_render_to_png_bytes_valid_png() {
        let mut renderer = ScreenRenderer::new();
        let screen = screen_from_bytes(4, 20, b"Hello");
        let bytes = renderer.render_to_png_bytes(&screen).unwrap();

        // PNG magic bytes
        assert!(bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]));
        assert!(!bytes.is_empty());
    }

    #[test]
    fn test_render_to_png_file() {
        let mut renderer = ScreenRenderer::new();
        let screen = screen_from_bytes(4, 20, b"Test");
        let dir = std::env::temp_dir();
        let path = dir.join("tui_test_render_test.png");

        renderer.render_to_png(&screen, &path).unwrap();
        assert!(path.exists());

        // Clean up
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_determinism_same_input_same_output() {
        let screen = screen_from_bytes(4, 20, b"Deterministic");

        let mut r1 = ScreenRenderer::new();
        let bytes1 = r1.render_to_png_bytes(&screen).unwrap();
        let hash1 = Sha256::digest(&bytes1);

        let mut r2 = ScreenRenderer::new();
        let bytes2 = r2.render_to_png_bytes(&screen).unwrap();
        let hash2 = Sha256::digest(&bytes2);

        assert_eq!(
            hash1, hash2,
            "Same input should produce identical PNG output"
        );
    }

    #[test]
    fn test_determinism_multiple_renders() {
        let screen = screen_from_bytes(4, 20, b"Repeat");
        let mut renderer = ScreenRenderer::new();

        let bytes1 = renderer.render_to_png_bytes(&screen).unwrap();
        let bytes2 = renderer.render_to_png_bytes(&screen).unwrap();
        let bytes3 = renderer.render_to_png_bytes(&screen).unwrap();

        let hash1 = Sha256::digest(&bytes1);
        let hash2 = Sha256::digest(&bytes2);
        let hash3 = Sha256::digest(&bytes3);

        assert_eq!(hash1, hash2);
        assert_eq!(hash2, hash3);
    }

    #[test]
    fn test_colored_text_renders_differently() {
        // Red text vs default text should produce different images
        let screen_default = screen_from_bytes(4, 20, b"Color");
        let screen_red = screen_from_bytes(4, 20, b"\x1b[31mColor\x1b[0m");

        let mut renderer = ScreenRenderer::new();
        let bytes_default = renderer.render_to_png_bytes(&screen_default).unwrap();

        let mut renderer2 = ScreenRenderer::new();
        let bytes_red = renderer2.render_to_png_bytes(&screen_red).unwrap();

        assert_ne!(
            Sha256::digest(&bytes_default),
            Sha256::digest(&bytes_red),
            "Different colored text should produce different images"
        );
    }

    #[test]
    fn test_colored_text_pixel_verification() {
        // Render red text on default background
        let screen = screen_from_bytes(4, 20, b"\x1b[31mX\x1b[0m");
        let mut renderer = ScreenRenderer::new();
        let img = renderer.render(&screen);

        // Sample pixels in the cell area of character 'X' (row 0, col 0)
        let pad = 8u32;
        let cw = renderer.cell_width();
        let ch = renderer.cell_height();

        // The cell area for (0,0) is from (pad, pad) to (pad+cw, pad+ch)
        // Find any non-background pixel in this area that has red channel > green/blue
        let bg = [30u8, 30, 30];
        let mut found_red_pixel = false;
        for py in pad..pad + ch {
            for px in pad..pad + cw {
                let pixel = img.get_pixel(px, py);
                if pixel[0] != bg[0] || pixel[1] != bg[1] || pixel[2] != bg[2] {
                    // This is a rendered glyph pixel; it should be reddish
                    // Red color index 1 = [205, 49, 49]
                    // Due to alpha blending, the actual pixel will be a blend
                    if pixel[0] > pixel[1] && pixel[0] > pixel[2] {
                        found_red_pixel = true;
                    }
                }
            }
        }
        assert!(found_red_pixel, "Should find red-ish pixels for red text");
    }

    #[test]
    fn test_bold_text_renders_differently() {
        let screen_normal = screen_from_bytes(4, 20, b"Bold");
        let screen_bold = screen_from_bytes(4, 20, b"\x1b[1mBold\x1b[0m");

        let mut r1 = ScreenRenderer::new();
        let bytes_normal = r1.render_to_png_bytes(&screen_normal).unwrap();

        let mut r2 = ScreenRenderer::new();
        let bytes_bold = r2.render_to_png_bytes(&screen_bold).unwrap();

        assert_ne!(
            Sha256::digest(&bytes_normal),
            Sha256::digest(&bytes_bold),
            "Bold and normal text should produce different images"
        );
    }

    #[test]
    fn test_bold_promotes_ansi_colors() {
        // Bold + color index 1 (red) should promote to bright red (index 9)
        let screen = screen_from_bytes(4, 20, b"\x1b[1;31mX\x1b[0m");
        let mut renderer = ScreenRenderer::new();
        let img = renderer.render(&screen);

        let pad = 8u32;
        let cw = renderer.cell_width();
        let ch = renderer.cell_height();
        let bg = [30u8, 30, 30];

        // Look for bright-red pixels (241, 76, 76) instead of regular red (205, 49, 49)
        let mut found_bright_pixel = false;
        for py in pad..pad + ch {
            for px in pad..pad + cw {
                let pixel = img.get_pixel(px, py);
                if pixel[0] != bg[0] || pixel[1] != bg[1] || pixel[2] != bg[2] {
                    // Bright red has higher red channel than regular red after blending
                    if pixel[0] > 200 {
                        found_bright_pixel = true;
                    }
                }
            }
        }
        assert!(
            found_bright_pixel,
            "Bold + color 0-7 should promote to bright variant"
        );
    }

    #[test]
    fn test_inverse_text() {
        // Inverse should swap fg and bg
        let screen = screen_from_bytes(4, 20, b"\x1b[7mI\x1b[0m");
        let mut renderer = ScreenRenderer::new();
        let img = renderer.render(&screen);

        let pad = 8u32;
        let ch = renderer.cell_height();

        // With inverse, the cell background should be the default fg color [204, 204, 204]
        // and the text fg should be the default bg color [30, 30, 30]
        let expected_bg = [204u8, 204, 204]; // swapped from fg

        // Check a pixel in the cell area that should have the inverted background
        // Pick a corner of the cell where there's likely no glyph
        let corner_pixel = img.get_pixel(pad, pad + ch - 1);
        assert_eq!(
            [corner_pixel[0], corner_pixel[1], corner_pixel[2]],
            expected_bg,
            "Inverse cell background should be default fg color"
        );
    }

    #[test]
    fn test_underline_rendering() {
        // Underlined text should have a line below the baseline
        let screen_no_ul = screen_from_bytes(4, 20, b"U");
        let screen_ul = screen_from_bytes(4, 20, b"\x1b[4mU\x1b[0m");

        let mut r1 = ScreenRenderer::new();
        let bytes_no_ul = r1.render_to_png_bytes(&screen_no_ul).unwrap();

        let mut r2 = ScreenRenderer::new();
        let bytes_ul = r2.render_to_png_bytes(&screen_ul).unwrap();

        assert_ne!(
            Sha256::digest(&bytes_no_ul),
            Sha256::digest(&bytes_ul),
            "Underlined text should produce different image"
        );
    }

    #[test]
    fn test_no_cursor_config() {
        let screen = screen_from_bytes(4, 20, b"Test");

        let config_cursor = RenderConfig {
            show_cursor: true,
            ..RenderConfig::default()
        };
        let config_no_cursor = RenderConfig {
            show_cursor: false,
            ..RenderConfig::default()
        };

        let mut r1 = ScreenRenderer::with_config(config_cursor);
        let bytes1 = r1.render_to_png_bytes(&screen).unwrap();

        let mut r2 = ScreenRenderer::with_config(config_no_cursor);
        let bytes2 = r2.render_to_png_bytes(&screen).unwrap();

        assert_ne!(
            Sha256::digest(&bytes1),
            Sha256::digest(&bytes2),
            "Cursor on/off should produce different images"
        );
    }

    #[test]
    fn test_custom_font_size() {
        let config = RenderConfig {
            font_size: 20.0,
            ..RenderConfig::default()
        };
        let mut renderer = ScreenRenderer::with_config(config);
        let screen = screen_from_bytes(4, 10, b"Big");
        let img = renderer.render(&screen);

        // With bigger font, dimensions should be larger
        let cw = renderer.cell_width();
        let ch = renderer.cell_height();
        assert!(cw > 0);
        assert!(ch > 0);
        assert_eq!(img.width(), 8 * 2 + 10 * cw);
        assert_eq!(img.height(), 8 * 2 + 4 * ch);
    }

    #[test]
    fn test_custom_padding() {
        let config = RenderConfig {
            padding: 0,
            ..RenderConfig::default()
        };
        let mut renderer = ScreenRenderer::with_config(config);
        let screen = screen_from_bytes(4, 10, b"");
        let img = renderer.render(&screen);

        let cw = renderer.cell_width();
        let ch = renderer.cell_height();
        assert_eq!(img.width(), 10 * cw);
        assert_eq!(img.height(), 4 * ch);
    }

    #[test]
    fn test_empty_screen() {
        let mut renderer = ScreenRenderer::new();
        let screen = screen_from_bytes(4, 10, b"");
        let img = renderer.render(&screen);

        // All pixels should be background color
        let bg = Rgba([30u8, 30, 30, 255]);
        // Check a few sample pixels (not including cursor position)
        let pad = 8u32;
        // Row 1, col 1 (away from cursor at 0,0)
        let cw = renderer.cell_width();
        let ch = renderer.cell_height();
        let px = pad + cw + cw / 2; // middle of cell (1,0) area
        let py = pad + ch + ch / 2; // row 1
        assert_eq!(*img.get_pixel(px, py), bg);
    }

    #[test]
    fn test_wide_character_handling() {
        // CJK character (wide) followed by normal char
        let screen = screen_from_bytes(4, 20, "你A".as_bytes());
        let mut renderer = ScreenRenderer::new();
        let img = renderer.render(&screen);

        // Should not panic and should produce a valid image
        assert!(img.width() > 0);
        assert!(img.height() > 0);

        let bytes = renderer.render_to_png_bytes(&screen).unwrap();
        assert!(bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]));
    }

    #[test]
    fn test_cell_width_and_height_accessors() {
        let renderer = ScreenRenderer::new();
        assert!(renderer.cell_width() > 0);
        assert!(renderer.cell_height() > 0);
    }

    #[test]
    fn test_default_impl() {
        let renderer = ScreenRenderer::default();
        assert!(renderer.cell_width() > 0);
        assert!(renderer.cell_height() > 0);
    }

    #[test]
    fn test_render_config_default() {
        let config = RenderConfig::default();
        assert_eq!(config.font_size, 14.0);
        assert_eq!(config.padding, 8);
        assert!(config.show_cursor);
    }

    #[test]
    fn test_render_256_color_text() {
        // Use 256 color mode: ESC[38;5;196m for fg color 196
        let screen = screen_from_bytes(4, 20, b"\x1b[38;5;196mR\x1b[0m");
        let mut renderer = ScreenRenderer::new();
        let bytes = renderer.render_to_png_bytes(&screen).unwrap();
        assert!(!bytes.is_empty());
    }

    #[test]
    fn test_render_true_color_text() {
        // True color: ESC[38;2;255;128;0m for fg color RGB(255, 128, 0)
        let screen = screen_from_bytes(4, 20, b"\x1b[38;2;255;128;0mO\x1b[0m");
        let mut renderer = ScreenRenderer::new();
        let img = renderer.render(&screen);

        let pad = 8u32;
        let cw = renderer.cell_width();
        let ch = renderer.cell_height();
        let bg = [30u8, 30, 30];

        // Look for orange-ish pixels
        let mut found_orange = false;
        for py in pad..pad + ch {
            for px in pad..pad + cw {
                let pixel = img.get_pixel(px, py);
                if pixel[0] != bg[0] || pixel[1] != bg[1] || pixel[2] != bg[2] {
                    // Orange: high red, medium green, low blue
                    if pixel[0] > pixel[1] && pixel[1] > pixel[2] {
                        found_orange = true;
                    }
                }
            }
        }
        assert!(
            found_orange,
            "Should find orange pixels for true color text"
        );
    }

    #[test]
    fn test_render_with_background_color() {
        // Green background: ESC[42m
        let screen = screen_from_bytes(4, 20, b"\x1b[42m \x1b[0m");
        let mut renderer = ScreenRenderer::new();
        let img = renderer.render(&screen);

        let pad = 8u32;
        let cw = renderer.cell_width();
        let ch = renderer.cell_height();

        // The cell at (0,0) should have green background [13, 188, 121]
        // Check center of the cell
        let px = pad + cw / 2;
        let py = pad + ch / 2;
        let pixel = img.get_pixel(px, py);
        assert_eq!(
            [pixel[0], pixel[1], pixel[2]],
            [13, 188, 121],
            "Cell background should be green"
        );
    }

    #[test]
    fn test_fill_rect_clipping() {
        // Test that fill_rect doesn't panic when rect extends beyond image
        let mut img = RgbaImage::from_pixel(10, 10, Rgba([0, 0, 0, 255]));
        let color = Rgba([255, 0, 0, 255]);

        // Rect that extends beyond image bounds
        ScreenRenderer::fill_rect(&mut img, 5, 5, 20, 20, color);

        // Check that pixels within bounds were colored
        assert_eq!(*img.get_pixel(5, 5), color);
        assert_eq!(*img.get_pixel(9, 9), color);
    }

    #[test]
    fn test_composite_glyph_clipping() {
        // Test that composite_glyph handles negative coordinates gracefully
        use super::super::font::RasterizedGlyph;
        use fontdue::Metrics;

        let mut img = RgbaImage::from_pixel(10, 10, Rgba([0, 0, 0, 255]));
        let glyph = RasterizedGlyph {
            metrics: Metrics {
                xmin: 0,
                ymin: 0,
                width: 4,
                height: 4,
                advance_width: 4.0,
                advance_height: 0.0,
                bounds: fontdue::OutlineBounds {
                    xmin: 0.0,
                    ymin: 0.0,
                    width: 4.0,
                    height: 4.0,
                },
            },
            coverage: vec![128; 16],
        };

        // Glyph at negative position should not panic
        ScreenRenderer::composite_glyph(&mut img, &glyph, -2, -2, Rgba([255, 255, 255, 255]));

        // Glyph beyond image bounds should not panic
        ScreenRenderer::composite_glyph(&mut img, &glyph, 8, 8, Rgba([255, 255, 255, 255]));
    }

    #[test]
    fn test_resolve_cell_colors_default() {
        let renderer = ScreenRenderer::new();
        let parser = vt100::Parser::new(4, 10, 0);
        let screen = parser.screen();
        let cell = screen.cell(0, 0).unwrap();

        let (fg, bg) = renderer.resolve_cell_colors(cell);
        assert_eq!(fg, Rgba([204, 204, 204, 255]));
        assert_eq!(bg, Rgba([30, 30, 30, 255]));
    }

    #[test]
    fn test_resolve_cell_colors_bold_promotion() {
        let mut parser = vt100::Parser::new(4, 10, 0);
        // Bold + blue (index 4) should promote to bright blue (index 12)
        parser.process(b"\x1b[1;34mX\x1b[0m");
        let screen = parser.screen();
        let cell = screen.cell(0, 0).unwrap();

        let renderer = ScreenRenderer::new();
        let (fg, _bg) = renderer.resolve_cell_colors(cell);
        // Bright blue = [59, 142, 234]
        assert_eq!(fg, Rgba([59, 142, 234, 255]));
    }

    #[test]
    fn test_resolve_cell_colors_inverse() {
        let mut parser = vt100::Parser::new(4, 10, 0);
        parser.process(b"\x1b[7mX\x1b[0m");
        let screen = parser.screen();
        let cell = screen.cell(0, 0).unwrap();

        let renderer = ScreenRenderer::new();
        let (fg, bg) = renderer.resolve_cell_colors(cell);
        // Inverse swaps: fg becomes bg color, bg becomes fg color
        assert_eq!(fg, Rgba([30, 30, 30, 255]));
        assert_eq!(bg, Rgba([204, 204, 204, 255]));
    }

    #[test]
    fn test_multiple_attributes_combined() {
        // Bold + underline + color
        let screen = screen_from_bytes(4, 20, b"\x1b[1;4;32mTest\x1b[0m");
        let mut renderer = ScreenRenderer::new();
        let bytes = renderer.render_to_png_bytes(&screen).unwrap();
        assert!(!bytes.is_empty());
    }

    #[test]
    fn test_full_screen_text() {
        // Fill entire screen with text
        let mut data = Vec::new();
        for row in 0..4u16 {
            data.extend(std::iter::repeat_n(b'A', 10));
            if row < 3 {
                data.extend_from_slice(b"\r\n");
            }
        }
        let screen = screen_from_bytes(4, 10, &data);
        let mut renderer = ScreenRenderer::new();
        let bytes = renderer.render_to_png_bytes(&screen).unwrap();
        assert!(!bytes.is_empty());
    }

    #[test]
    fn test_cursor_rendering() {
        // Cursor should be visible at the default position
        let screen_with_text = screen_from_bytes(4, 20, b"AB");
        let mut renderer = ScreenRenderer::new();
        let img = renderer.render(&screen_with_text);

        let pad = 8u32;
        let cw = renderer.cell_width();
        let ch = renderer.cell_height();

        // Cursor is at (0, 2) after writing "AB"
        let cx = pad + 2 * cw;
        let cy = pad;

        // The cursor block should be filled with the fg color
        let center_pixel = img.get_pixel(cx + cw / 2, cy + ch / 2);
        // Default fg is [204, 204, 204] - cursor should show this
        assert!(
            center_pixel[0] > 100,
            "Cursor block should have fg color (bright)"
        );
    }

    #[test]
    fn test_render_to_png_invalid_path() {
        let mut renderer = ScreenRenderer::new();
        let screen = screen_from_bytes(4, 10, b"");
        let result = renderer.render_to_png(&screen, "/nonexistent/directory/test.png");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, TuiTestError::Render(_)));
    }
}
