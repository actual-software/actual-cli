//! Central terminal-size resolver.
//!
//! Replaces the scattered `console::Term::stdout/stderr().size_checked()` call
//! sites with a single, consistent helper backed by crossterm.

/// Query the current terminal width in columns.
///
/// Falls back to 80 if the terminal size cannot be determined (non-TTY,
/// piped, CI, etc.). Does NOT apply any maximum cap — callers are responsible
/// for capping to their own layout constraints.
pub fn terminal_width() -> usize {
    crossterm::terminal::size()
        .map(|(cols, _)| cols as usize)
        .unwrap_or(80)
}

/// Query the current terminal size as (rows, cols).
///
/// Falls back to (24, 80) if the size cannot be determined.
pub fn terminal_size() -> (u16, u16) {
    crossterm::terminal::size().unwrap_or((24, 80))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_width_returns_positive() {
        // In a test environment the TTY may not be available, so we just
        // verify the function returns a sane default (≥ 40).
        let w = terminal_width();
        assert!(w >= 40, "expected at least 40 cols, got {w}");
    }

    #[test]
    fn terminal_size_returns_positive_dimensions() {
        let (rows, cols) = terminal_size();
        // In a non-TTY environment (CI, pipes) crossterm may return a small
        // pseudo-size, so we only require non-zero values here.
        assert!(rows >= 1, "expected at least 1 row, got {rows}");
        assert!(cols >= 1, "expected at least 1 col, got {cols}");
    }
}
