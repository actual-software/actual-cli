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

/// Normalize crossterm's `(cols, rows)` into our `(rows, cols)` convention.
fn cols_rows_to_rows_cols((cols, rows): (u16, u16)) -> (u16, u16) {
    (rows, cols)
}

/// Query the current terminal size as (rows, cols).
///
/// Falls back to (24, 80) if the size cannot be determined.
pub fn terminal_size() -> (u16, u16) {
    crossterm::terminal::size()
        .map(cols_rows_to_rows_cols)
        .unwrap_or((24, 80))
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
    fn terminal_size_fallback_has_correct_order() {
        // When no TTY is available crossterm::terminal::size() fails and
        // we fall back to (24, 80).  Verify the tuple is (rows, cols) —
        // i.e. the first element (rows) is less than the second (cols).
        let (rows, cols) = terminal_size();
        // CI and piped environments always hit the fallback path.
        // A real terminal *could* have rows >= cols, so this assertion is
        // only meaningful in non-TTY contexts (which is how CI runs).
        if crossterm::terminal::size().is_err() {
            assert_eq!(
                (rows, cols),
                (24, 80),
                "fallback should be (24 rows, 80 cols)"
            );
        }
    }

    #[test]
    fn cols_rows_to_rows_cols_swaps_order() {
        // Directly test the normalization helper so the mapping path is
        // covered even when no TTY is available (CI).
        assert_eq!(cols_rows_to_rows_cols((80, 24)), (24, 80));
        assert_eq!(cols_rows_to_rows_cols((120, 40)), (40, 120));
        assert_eq!(cols_rows_to_rows_cols((1, 1)), (1, 1));
    }
}
