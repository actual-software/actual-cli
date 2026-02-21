/// Terminal color scheme configuration.
#[derive(Clone, Debug)]
pub struct ColorScheme {
    /// Default foreground color as RGB.
    pub fg: [u8; 3],
    /// Default background color as RGB.
    pub bg: [u8; 3],
    /// Basic ANSI 16 colors as RGB.
    pub ansi_16: [[u8; 3]; 16],
}

impl Default for ColorScheme {
    fn default() -> Self {
        Self {
            fg: [204, 204, 204],
            bg: [30, 30, 30],
            ansi_16: [
                [0, 0, 0],       // 0: Black
                [205, 49, 49],   // 1: Red
                [13, 188, 121],  // 2: Green
                [229, 229, 16],  // 3: Yellow
                [36, 114, 200],  // 4: Blue
                [188, 63, 188],  // 5: Magenta
                [17, 168, 205],  // 6: Cyan
                [204, 204, 204], // 7: White
                [102, 102, 102], // 8: Bright Black
                [241, 76, 76],   // 9: Bright Red
                [35, 209, 139],  // 10: Bright Green
                [245, 245, 67],  // 11: Bright Yellow
                [59, 142, 234],  // 12: Bright Blue
                [214, 112, 214], // 13: Bright Magenta
                [41, 184, 219],  // 14: Bright Cyan
                [242, 242, 242], // 15: Bright White
            ],
        }
    }
}

impl ColorScheme {
    /// Resolve a `vt100::Color` to RGB.
    pub fn resolve(&self, color: vt100::Color, is_fg: bool) -> [u8; 3] {
        match color {
            vt100::Color::Default => {
                if is_fg {
                    self.fg
                } else {
                    self.bg
                }
            }
            vt100::Color::Idx(idx) => self.idx_to_rgb(idx),
            vt100::Color::Rgb(r, g, b) => [r, g, b],
        }
    }

    /// Map a 0-255 color index to RGB.
    pub fn idx_to_rgb(&self, idx: u8) -> [u8; 3] {
        match idx {
            0..=15 => self.ansi_16[idx as usize],
            16..=231 => {
                let i = idx - 16;
                let r = i / 36;
                let g = (i % 36) / 6;
                let b = i % 6;
                let to_val = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
                [to_val(r), to_val(g), to_val(b)]
            }
            232..=255 => {
                let v = 8 + 10 * (idx - 232);
                [v, v, v]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_scheme_fg_bg() {
        let scheme = ColorScheme::default();
        assert_eq!(scheme.fg, [204, 204, 204]);
        assert_eq!(scheme.bg, [30, 30, 30]);
    }

    #[test]
    fn test_default_scheme_ansi_16_length() {
        let scheme = ColorScheme::default();
        assert_eq!(scheme.ansi_16.len(), 16);
    }

    #[test]
    fn test_default_scheme_ansi_16_values() {
        let scheme = ColorScheme::default();
        // Spot-check a few colors
        assert_eq!(scheme.ansi_16[0], [0, 0, 0]); // Black
        assert_eq!(scheme.ansi_16[1], [205, 49, 49]); // Red
        assert_eq!(scheme.ansi_16[7], [204, 204, 204]); // White
        assert_eq!(scheme.ansi_16[8], [102, 102, 102]); // Bright Black
        assert_eq!(scheme.ansi_16[15], [242, 242, 242]); // Bright White
    }

    #[test]
    fn test_resolve_default_fg() {
        let scheme = ColorScheme::default();
        assert_eq!(scheme.resolve(vt100::Color::Default, true), [204, 204, 204]);
    }

    #[test]
    fn test_resolve_default_bg() {
        let scheme = ColorScheme::default();
        assert_eq!(scheme.resolve(vt100::Color::Default, false), [30, 30, 30]);
    }

    #[test]
    fn test_resolve_idx_ansi_16() {
        let scheme = ColorScheme::default();
        for idx in 0..16u8 {
            let result = scheme.resolve(vt100::Color::Idx(idx), true);
            assert_eq!(result, scheme.ansi_16[idx as usize]);
        }
    }

    #[test]
    fn test_resolve_idx_ansi_16_bg() {
        let scheme = ColorScheme::default();
        // is_fg should not matter for Idx colors
        let result_fg = scheme.resolve(vt100::Color::Idx(5), true);
        let result_bg = scheme.resolve(vt100::Color::Idx(5), false);
        assert_eq!(result_fg, result_bg);
    }

    #[test]
    fn test_resolve_rgb() {
        let scheme = ColorScheme::default();
        assert_eq!(
            scheme.resolve(vt100::Color::Rgb(100, 200, 50), true),
            [100, 200, 50]
        );
        assert_eq!(scheme.resolve(vt100::Color::Rgb(0, 0, 0), false), [0, 0, 0]);
        assert_eq!(
            scheme.resolve(vt100::Color::Rgb(255, 255, 255), true),
            [255, 255, 255]
        );
    }

    #[test]
    fn test_idx_to_rgb_ansi_16_range() {
        let scheme = ColorScheme::default();
        for idx in 0..=15u8 {
            assert_eq!(scheme.idx_to_rgb(idx), scheme.ansi_16[idx as usize]);
        }
    }

    #[test]
    fn test_idx_to_rgb_216_cube_origin() {
        let scheme = ColorScheme::default();
        // Index 16 = (0,0,0) in the 6x6x6 cube → all zeros
        assert_eq!(scheme.idx_to_rgb(16), [0, 0, 0]);
    }

    #[test]
    fn test_idx_to_rgb_216_cube_max() {
        let scheme = ColorScheme::default();
        // Index 231 = (5,5,5) in the 6x6x6 cube → [255, 255, 255]
        assert_eq!(scheme.idx_to_rgb(231), [255, 255, 255]);
    }

    #[test]
    fn test_idx_to_rgb_216_cube_mid_values() {
        let scheme = ColorScheme::default();
        // Index 16 + 1*36 + 0*6 + 0 = 52 → r=1,g=0,b=0 → [95, 0, 0]
        assert_eq!(scheme.idx_to_rgb(52), [95, 0, 0]);
        // Index 16 + 0*36 + 1*6 + 0 = 22 → r=0,g=1,b=0 → [0, 95, 0]
        assert_eq!(scheme.idx_to_rgb(22), [0, 95, 0]);
        // Index 16 + 0*36 + 0*6 + 1 = 17 → r=0,g=0,b=1 → [0, 0, 95]
        assert_eq!(scheme.idx_to_rgb(17), [0, 0, 95]);
    }

    #[test]
    fn test_idx_to_rgb_216_cube_mixed() {
        let scheme = ColorScheme::default();
        // Index 16 + 2*36 + 3*6 + 4 = 16 + 72 + 18 + 4 = 110
        // r=2 → 55+80=135, g=3 → 55+120=175, b=4 → 55+160=215
        assert_eq!(scheme.idx_to_rgb(110), [135, 175, 215]);
    }

    #[test]
    fn test_idx_to_rgb_grayscale_start() {
        let scheme = ColorScheme::default();
        // Index 232 = 8 + 10*(232-232) = 8
        assert_eq!(scheme.idx_to_rgb(232), [8, 8, 8]);
    }

    #[test]
    fn test_idx_to_rgb_grayscale_end() {
        let scheme = ColorScheme::default();
        // Index 255 = 8 + 10*(255-232) = 8 + 230 = 238
        assert_eq!(scheme.idx_to_rgb(255), [238, 238, 238]);
    }

    #[test]
    fn test_idx_to_rgb_grayscale_mid() {
        let scheme = ColorScheme::default();
        // Index 244 = 8 + 10*(244-232) = 8 + 120 = 128
        assert_eq!(scheme.idx_to_rgb(244), [128, 128, 128]);
    }

    #[test]
    fn test_custom_scheme() {
        let scheme = ColorScheme {
            fg: [255, 0, 0],
            bg: [0, 255, 0],
            ansi_16: [[10; 3]; 16],
        };
        assert_eq!(scheme.resolve(vt100::Color::Default, true), [255, 0, 0]);
        assert_eq!(scheme.resolve(vt100::Color::Default, false), [0, 255, 0]);
        assert_eq!(scheme.resolve(vt100::Color::Idx(0), true), [10, 10, 10]);
        assert_eq!(scheme.resolve(vt100::Color::Idx(15), true), [10, 10, 10]);
    }

    #[test]
    fn test_clone_and_debug() {
        let scheme = ColorScheme::default();
        let cloned = scheme.clone();
        assert_eq!(cloned.fg, scheme.fg);
        assert_eq!(cloned.bg, scheme.bg);
        // Debug trait should not panic
        let debug_str = format!("{scheme:?}");
        assert!(!debug_str.is_empty());
    }
}
