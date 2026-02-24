//! Production I/O wrappers for terminal and stdin.
//!
//! This module is excluded from coverage measurement because
//! these types wrap OS-level I/O (`console::Term`, `std::io::stdin`)
//! that cannot be made to return errors in unit tests.

use dialoguer::MultiSelect;
use dialoguer::Select;

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
        self.confirm_with_cancel(prompt)
            .map(|opt| opt.unwrap_or(false))
    }

    fn confirm_with_cancel(&self, prompt: &str) -> Result<Option<bool>, ActualError> {
        DialoguerConfirm::new()
            .with_prompt(prompt)
            .default(false)
            .interact_opt()
            .map_err(|e| terminal_io_error("confirmation failed", e))
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

    fn select_one(
        &self,
        prompt: &str,
        items: &[String],
        default: Option<usize>,
    ) -> Result<usize, ActualError> {
        let theme = dialoguer::theme::ColorfulTheme::default();
        let mut select = Select::with_theme(&theme).with_prompt(prompt).items(items);
        if let Some(d) = default {
            select = select.default(d);
        }
        select
            .interact_on_opt(&self.term)
            .map_err(|e| terminal_io_error("single selection failed", e))?
            .ok_or(ActualError::UserCancelled)
    }
}

#[cfg(test)]
mod tests {
    use crate::cli::ui::terminal::TerminalIO;
    use crate::cli::ui::test_utils::MockTerminal;

    // These tests verify the `confirm` / `confirm_with_cancel` default-trait
    // behaviour using `MockTerminal` from `test_utils`, which uses those
    // defaults and records I/O without touching OS resources.

    #[test]
    fn confirm_returns_true_for_yes_input() {
        let mock = MockTerminal::new(vec!["y"]);
        assert_eq!(mock.confirm("proceed?").unwrap(), true);
    }

    #[test]
    fn confirm_returns_true_for_yes_long_input() {
        let mock = MockTerminal::new(vec!["yes"]);
        assert_eq!(mock.confirm("proceed?").unwrap(), true);
    }

    #[test]
    fn confirm_returns_false_for_no_input() {
        let mock = MockTerminal::new(vec!["n"]);
        assert_eq!(mock.confirm("proceed?").unwrap(), false);
    }

    #[test]
    fn confirm_returns_false_for_unrecognised_input() {
        // Unrecognised input defaults to false (safe rejection).
        let mock = MockTerminal::new(vec!["maybe"]);
        assert_eq!(mock.confirm("proceed?").unwrap(), false);
    }

    #[test]
    fn confirm_with_cancel_wraps_true_in_some() {
        let mock = MockTerminal::new(vec!["y"]);
        assert_eq!(mock.confirm_with_cancel("proceed?").unwrap(), Some(true));
    }

    #[test]
    fn confirm_with_cancel_wraps_false_in_some() {
        let mock = MockTerminal::new(vec!["n"]);
        assert_eq!(mock.confirm_with_cancel("proceed?").unwrap(), Some(false));
    }
}
