//! Production I/O wrappers for terminal and stdin.
//!
//! This module is excluded from coverage measurement because
//! these types wrap OS-level I/O (`console::Term`, `std::io::stdin`)
//! that cannot be made to return errors in unit tests.

use crate::cli::ui::confirm::InputReader;
use crate::cli::ui::file_confirm::TerminalIO;
use crate::error::ActualError;

/// Production terminal backed by `console::Term`.
pub struct RealTerminal {
    term: console::Term,
}

impl Default for RealTerminal {
    fn default() -> Self {
        Self {
            term: console::Term::stdout(),
        }
    }
}

/// Convert a terminal I/O error into an `ActualError` with the given context.
fn terminal_io_error(context: &str, err: std::io::Error) -> ActualError {
    ActualError::ConfigError(format!("{context}: {err}"))
}

impl TerminalIO for RealTerminal {
    fn read_line(&self, prompt: &str) -> Result<String, ActualError> {
        if let Err(e) = self.term.write_str(prompt) {
            return Err(terminal_io_error("terminal write failed", e));
        }
        match self.term.read_line() {
            Ok(line) => Ok(line),
            Err(e) => Err(terminal_io_error("terminal read failed", e)),
        }
    }

    fn write_line(&self, text: &str) {
        let _ = self.term.write_line(text);
    }
}

/// Reads a line from stdin for the project confirmation prompt.
///
/// This is a thin wrapper around `std::io::stdin().read_line()` that
/// implements [`InputReader`]. It lives in this coverage-excluded module
/// because stdin cannot be controlled in unit tests.
pub struct StdinReader;

impl InputReader for StdinReader {
    fn read_line(&self) -> std::io::Result<String> {
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
        Ok(buf)
    }
}
