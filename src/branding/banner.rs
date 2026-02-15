/// ASCII art banner based on the actual logo.
///
/// The logo is a stylized "a" shaped like a bird with speed lines,
/// representing AI-powered code management. Sourced from `docs/logo.txt`.
pub const BANNER: &str = "\
                 ..==++++++++=============
              +*******************+++++++
           .************************++**
         =********:        :********+**
        =******:              .+*******.........................
      *******:                  -***********++++++++++++++++++=
  -*********                     .*****+-+*****++++++++++++++=
***********         ******.       .*** :*
 .*********       :********-       ***:.- :--===-----------
   *******=       **********       +******************++++=
    *******       *********-       =*******-=************=
     :*****        +******.        +*****+ *+
      ******          ::           +****** -. -===-====.
       ******.                     +*******************
        ******=                    +******************
         +******+=-        .:      ****************=
           +*********************- *****************-
             +****************************************.
               .+*************************************+:.
                   :************************************-:
                         ::-+++**************+*::";

/// Gradient start color: #00FB7E (green).
const GRADIENT_START: (u8, u8, u8) = (0x00, 0xFB, 0x7E);

/// Gradient end color: #179CA9 (teal).
const GRADIENT_END: (u8, u8, u8) = (0x17, 0x9C, 0xA9);

/// Linearly interpolate between two color components.
fn lerp_color(start: u8, end: u8, t: f64) -> u8 {
    let s = f64::from(start);
    let e = f64::from(end);
    (s + (e - s) * t).round() as u8
}

/// Compute the gradient RGB for a given line index out of total lines.
fn gradient_rgb(line_idx: usize, total_lines: usize) -> (u8, u8, u8) {
    let t = if total_lines <= 1 {
        0.0
    } else {
        line_idx as f64 / (total_lines - 1) as f64
    };
    (
        lerp_color(GRADIENT_START.0, GRADIENT_END.0, t),
        lerp_color(GRADIENT_START.1, GRADIENT_END.1, t),
        lerp_color(GRADIENT_START.2, GRADIENT_END.2, t),
    )
}

/// Print the ASCII banner to stderr with a vertical gradient.
///
/// If `quiet` is true, the banner is suppressed entirely.
/// The gradient goes from #00FB7E (green) at the top to #179CA9 (teal)
/// at the bottom, using 24-bit true-color ANSI escape sequences.
pub fn print_banner(quiet: bool) {
    if quiet {
        return;
    }

    let lines: Vec<&str> = BANNER.lines().collect();
    let total = lines.len();

    for (i, line) in lines.iter().enumerate() {
        let (r, g, b) = gradient_rgb(i, total);
        // \x1b[38;2;R;G;Bm sets 24-bit foreground color, \x1b[0m resets
        eprintln!("\x1b[38;2;{r};{g};{b}m{line}\x1b[0m");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_is_non_empty_and_contains_actual() {
        assert!(!BANNER.is_empty(), "BANNER should not be empty");
        // The logo is based on the actual brand — it contains asterisk-based art
        assert!(
            BANNER.contains('*'),
            "BANNER should contain asterisk characters from the logo"
        );
    }

    #[test]
    fn print_banner_quiet_produces_no_output() {
        // Calling with quiet=true should return immediately without error.
        print_banner(true);
    }

    #[test]
    fn print_banner_non_quiet_produces_no_panic() {
        // Calling with quiet=false should print the banner to stderr without error.
        // This exercises the gradient calculation and ANSI output code paths.
        print_banner(false);
    }

    #[test]
    fn gradient_start_is_green() {
        let (r, g, b) = gradient_rgb(0, 21);
        assert_eq!((r, g, b), GRADIENT_START);
    }

    #[test]
    fn gradient_end_is_teal() {
        let (r, g, b) = gradient_rgb(20, 21);
        assert_eq!((r, g, b), GRADIENT_END);
    }

    #[test]
    fn gradient_midpoint_is_between_start_and_end() {
        let (r, g, b) = gradient_rgb(10, 21);
        // Midpoint should be roughly halfway between start and end
        assert!(r > GRADIENT_START.0 && r < GRADIENT_END.0);
        assert!(g < GRADIENT_START.1 && g > GRADIENT_END.1);
        // Blue component goes from 0x7E (126) to 0xA9 (169)
        assert!(b > GRADIENT_START.2 && b < GRADIENT_END.2);
    }

    #[test]
    fn gradient_single_line_returns_start() {
        let (r, g, b) = gradient_rgb(0, 1);
        assert_eq!((r, g, b), GRADIENT_START);
    }

    #[test]
    fn lerp_color_boundaries() {
        assert_eq!(lerp_color(0, 255, 0.0), 0);
        assert_eq!(lerp_color(0, 255, 1.0), 255);
        assert_eq!(lerp_color(0, 200, 0.5), 100);
    }

    #[test]
    fn banner_has_expected_line_count() {
        let count = BANNER.lines().count();
        // The logo from docs/logo.txt has 21 non-empty lines
        assert_eq!(count, 21, "BANNER should have 21 lines");
    }
}
