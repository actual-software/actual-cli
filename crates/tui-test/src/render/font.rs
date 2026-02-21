use fontdue::{Font, FontSettings, Metrics};
use std::collections::HashMap;

const FONT_REGULAR_BYTES: &[u8] = include_bytes!("../../fonts/JetBrainsMonoNL-Regular.ttf");
const FONT_BOLD_BYTES: &[u8] = include_bytes!("../../fonts/JetBrainsMonoNL-Bold.ttf");

pub(crate) struct RasterizedGlyph {
    pub metrics: Metrics,
    pub coverage: Vec<u8>,
}

pub(crate) struct FontAtlas {
    regular: Font,
    bold: Font,
    font_size: f32,
    cache: HashMap<(char, bool), RasterizedGlyph>,
    pub cell_width: u32,
    pub cell_height: u32,
    pub ascent: f32,
}

impl FontAtlas {
    pub fn new(font_size: f32) -> Self {
        let regular = Font::from_bytes(FONT_REGULAR_BYTES, FontSettings::default())
            .expect("Failed to parse regular font");
        let bold = Font::from_bytes(FONT_BOLD_BYTES, FontSettings::default())
            .expect("Failed to parse bold font");

        let m_metrics = regular.metrics('M', font_size);
        let line_metrics = regular
            .horizontal_line_metrics(font_size)
            .expect("Font missing line metrics");

        let cell_width = m_metrics.advance_width.ceil() as u32;
        let cell_height = line_metrics.new_line_size.ceil() as u32;
        let ascent = line_metrics.ascent;

        Self {
            regular,
            bold,
            font_size,
            cache: HashMap::new(),
            cell_width,
            cell_height,
            ascent,
        }
    }

    pub fn rasterize(&mut self, ch: char, bold: bool) -> &RasterizedGlyph {
        let key = (ch, bold);
        let font_size = self.font_size;
        let regular = &self.regular;
        let bold_font = &self.bold;
        self.cache.entry(key).or_insert_with(|| {
            let font = if bold { bold_font } else { regular };
            let (metrics, coverage) = font.rasterize(ch, font_size);
            RasterizedGlyph { metrics, coverage }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_FONT_SIZE: f32 = 14.0;

    #[test]
    fn test_font_atlas_new_valid_dimensions() {
        let atlas = FontAtlas::new(TEST_FONT_SIZE);
        assert!(atlas.cell_width > 0, "Cell width should be positive");
        assert!(atlas.cell_height > 0, "Cell height should be positive");
        assert!(atlas.ascent > 0.0, "Ascent should be positive");
    }

    #[test]
    fn test_font_atlas_different_sizes() {
        let atlas_small = FontAtlas::new(10.0);
        let atlas_large = FontAtlas::new(24.0);
        assert!(
            atlas_large.cell_width >= atlas_small.cell_width,
            "Larger font should have wider cells"
        );
        assert!(
            atlas_large.cell_height >= atlas_small.cell_height,
            "Larger font should have taller cells"
        );
    }

    #[test]
    fn test_rasterize_printable_char() {
        let mut atlas = FontAtlas::new(TEST_FONT_SIZE);
        let glyph = atlas.rasterize('A', false);
        assert!(glyph.metrics.width > 0, "Glyph width should be positive");
        assert!(glyph.metrics.height > 0, "Glyph height should be positive");
        assert!(
            !glyph.coverage.is_empty(),
            "Coverage should not be empty for 'A'"
        );
        assert_eq!(
            glyph.coverage.len(),
            glyph.metrics.width * glyph.metrics.height,
            "Coverage size should match width * height"
        );
    }

    #[test]
    fn test_rasterize_bold_vs_regular() {
        let mut atlas = FontAtlas::new(TEST_FONT_SIZE);
        let regular = atlas.rasterize('A', false);
        let regular_coverage = regular.coverage.clone();

        let bold = atlas.rasterize('A', true);
        let bold_coverage = bold.coverage.clone();

        // Bold and regular glyphs for 'A' should differ in some way
        // (either metrics or coverage values)
        let metrics_differ = regular_coverage.len() != bold_coverage.len();
        let coverage_differs = if !metrics_differ {
            regular_coverage != bold_coverage
        } else {
            true
        };
        assert!(
            metrics_differ || coverage_differs,
            "Bold and regular glyphs should differ"
        );
    }

    #[test]
    fn test_rasterize_cache_hit() {
        let mut atlas = FontAtlas::new(TEST_FONT_SIZE);

        // First call populates cache
        let glyph1 = atlas.rasterize('B', false);
        let coverage1 = glyph1.coverage.clone();
        let metrics1_width = glyph1.metrics.width;
        let metrics1_height = glyph1.metrics.height;

        // Second call should return cached data
        let glyph2 = atlas.rasterize('B', false);
        assert_eq!(glyph2.coverage, coverage1);
        assert_eq!(glyph2.metrics.width, metrics1_width);
        assert_eq!(glyph2.metrics.height, metrics1_height);
    }

    #[test]
    fn test_rasterize_space_char() {
        let mut atlas = FontAtlas::new(TEST_FONT_SIZE);
        let glyph = atlas.rasterize(' ', false);
        // Space character has zero dimensions in fontdue
        assert_eq!(
            glyph.metrics.width * glyph.metrics.height,
            glyph.coverage.len(),
            "Coverage size should match dimensions"
        );
    }

    #[test]
    fn test_rasterize_various_chars() {
        let mut atlas = FontAtlas::new(TEST_FONT_SIZE);
        for ch in ['a', 'z', '0', '9', '!', '@', '#', '~'] {
            let glyph = atlas.rasterize(ch, false);
            assert_eq!(
                glyph.coverage.len(),
                glyph.metrics.width * glyph.metrics.height,
                "Coverage size mismatch for char '{ch}'"
            );
        }
    }

    #[test]
    fn test_rasterize_unicode_char() {
        let mut atlas = FontAtlas::new(TEST_FONT_SIZE);
        // Test with a common unicode character
        let glyph = atlas.rasterize('\u{2588}', false); // Full block
        assert_eq!(
            glyph.coverage.len(),
            glyph.metrics.width * glyph.metrics.height,
        );
    }

    #[test]
    fn test_font_atlas_ascent_relationship() {
        let atlas = FontAtlas::new(TEST_FONT_SIZE);
        // Ascent should be less than cell height
        assert!(
            atlas.ascent < atlas.cell_height as f32,
            "Ascent ({}) should be less than cell height ({})",
            atlas.ascent,
            atlas.cell_height
        );
    }

    #[test]
    fn test_cache_different_bold_entries() {
        let mut atlas = FontAtlas::new(TEST_FONT_SIZE);
        // Rasterize same char with different bold flags
        let _ = atlas.rasterize('X', false);
        let _ = atlas.rasterize('X', true);
        // Both should be in cache (different keys)
        assert!(atlas.cache.contains_key(&('X', false)));
        assert!(atlas.cache.contains_key(&('X', true)));
    }
}
