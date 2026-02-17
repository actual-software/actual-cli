//! Production I/O wrappers for terminal and stdin.
//!
//! This module is excluded from coverage measurement because
//! these types wrap OS-level I/O (`console::Term`, `std::io::stdin`)
//! that cannot be made to return errors in unit tests.

use dialoguer::MultiSelect;

use dialoguer::Confirm as DialoguerConfirm;

use crate::cli::ui::terminal::TerminalIO;
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
fn terminal_io_error(context: &str, err: impl std::fmt::Display) -> ActualError {
    ActualError::TerminalIOError(format!("{context}: {err}"))
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

    fn confirm(&self, prompt: &str) -> Result<bool, ActualError> {
        DialoguerConfirm::new()
            .with_prompt(prompt)
            .default(false)
            .interact_opt()
            .map_err(|e| terminal_io_error("confirmation failed", e))
            .map(|opt| opt.unwrap_or(false))
    }

    fn select_files(
        &self,
        prompt: &str,
        items: &[String],
        defaults: &[bool],
    ) -> Result<Option<Vec<usize>>, ActualError> {
        MultiSelect::new()
            .with_prompt(prompt)
            .items(items)
            .defaults(defaults)
            .interact_on_opt(&self.term)
            .map_err(|e| terminal_io_error("file selection failed", e))
    }
}
