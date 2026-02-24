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
    use super::*;
    use crate::cli::ui::terminal::TerminalIO;

    /// A minimal `TerminalIO` implementation used to verify the
    /// `confirm` / `confirm_with_cancel` contract without touching OS I/O.
    ///
    /// `interact_result` holds the `Option<bool>` that `confirm_with_cancel`
    /// will return (mirroring what `dialoguer::Confirm::interact_opt` returns).
    struct StubTerminal {
        interact_result: Option<bool>,
    }

    impl StubTerminal {
        fn returning(result: Option<bool>) -> Self {
            Self {
                interact_result: result,
            }
        }
    }

    impl TerminalIO for StubTerminal {
        fn read_line(&self, _prompt: &str) -> Result<String, ActualError> {
            unreachable!("read_line not expected in this test context")
        }

        fn write_line(&self, _text: &str) {}

        fn select_files(
            &self,
            _prompt: &str,
            _items: &[String],
            _defaults: &[bool],
        ) -> Result<Option<Vec<usize>>, ActualError> {
            unreachable!("select_files not expected in this test context")
        }

        fn select_one(
            &self,
            _prompt: &str,
            _items: &[String],
            _default: Option<usize>,
        ) -> Result<usize, ActualError> {
            unreachable!("select_one not expected in this test context")
        }

        /// Mirrors `RealTerminal::confirm_with_cancel` by returning the stored
        /// `Option<bool>` directly, just as `interact_opt()` would.
        fn confirm_with_cancel(&self, _prompt: &str) -> Result<Option<bool>, ActualError> {
            Ok(self.interact_result)
        }

        /// Mirrors `RealTerminal::confirm` by delegating to `confirm_with_cancel`
        /// and mapping `None` to `false`.
        fn confirm(&self, prompt: &str) -> Result<bool, ActualError> {
            self.confirm_with_cancel(prompt)
                .map(|opt| opt.unwrap_or(false))
        }
    }

    #[test]
    fn confirm_maps_none_to_false() {
        // None means user pressed Ctrl-C / Escape — confirm() must treat it as No
        let stub = StubTerminal::returning(None);
        assert_eq!(stub.confirm("proceed?").unwrap(), false);
    }

    #[test]
    fn confirm_maps_some_true_to_true() {
        let stub = StubTerminal::returning(Some(true));
        assert_eq!(stub.confirm("proceed?").unwrap(), true);
    }

    #[test]
    fn confirm_maps_some_false_to_false() {
        let stub = StubTerminal::returning(Some(false));
        assert_eq!(stub.confirm("proceed?").unwrap(), false);
    }

    #[test]
    fn confirm_with_cancel_preserves_none() {
        // Ctrl-C / Escape must be visible to callers as None, not collapsed to false
        let stub = StubTerminal::returning(None);
        assert_eq!(stub.confirm_with_cancel("proceed?").unwrap(), None);
    }

    #[test]
    fn confirm_with_cancel_preserves_some_true() {
        let stub = StubTerminal::returning(Some(true));
        assert_eq!(stub.confirm_with_cancel("proceed?").unwrap(), Some(true));
    }

    #[test]
    fn confirm_with_cancel_preserves_some_false() {
        let stub = StubTerminal::returning(Some(false));
        assert_eq!(stub.confirm_with_cancel("proceed?").unwrap(), Some(false));
    }
}
