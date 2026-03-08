//! Central terminal-size resolver.
//!
//! Replaces the scattered `console::Term::stdout/stderr().size_checked()` call
//! sites with a single, consistent helper backed by crossterm.

use std::io;

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

/// Normalize a crossterm `(cols, rows)` result into `(rows, cols)`, falling
/// back to `(24, 80)` on error.
fn normalize_size(raw: Result<(u16, u16), io::Error>) -> (u16, u16) {
    match raw {
        Ok((cols, rows)) => (rows, cols),
        Err(_) => (24, 80),
    }
}

/// Query the current terminal size as (rows, cols).
///
/// Falls back to (24, 80) if the size cannot be determined.
pub fn terminal_size() -> (u16, u16) {
    normalize_size(crossterm::terminal::size())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_width_returns_positive() {
        // In a test environment the TTY may not be available, so we just
        // verify the function returns a sane default (>= 40).
        let w = terminal_width();
        assert!(w >= 40, "expected at least 40 cols, got {w}");
    }

    #[test]
    fn terminal_size_returns_positive_dimensions() {
        let (rows, cols) = terminal_size();
        // In a non-TTY environment (CI, pipes) the fallback (24, 80) applies,
        // so rows < cols.  When a real TTY is present rows and cols are both
        // positive — we accept either situation.
        assert!(rows >= 1, "expected at least 1 row, got {rows}");
        assert!(cols >= 1, "expected at least 1 col, got {cols}");
    }

    #[test]
    fn normalize_size_swaps_cols_rows_on_success() {
        // crossterm returns (cols, rows); normalize_size must return (rows, cols).
        let ok: Result<(u16, u16), io::Error> = Ok((80, 24));
        assert_eq!(normalize_size(ok), (24, 80));

        let ok2: Result<(u16, u16), io::Error> = Ok((120, 40));
        assert_eq!(normalize_size(ok2), (40, 120));
    }

    #[test]
    fn normalize_size_returns_fallback_on_error() {
        let err: Result<(u16, u16), io::Error> =
            Err(io::Error::new(io::ErrorKind::Other, "no tty"));
        assert_eq!(normalize_size(err), (24, 80));
    }
}
