use crate::error::ActualError;

/// Trait abstracting terminal I/O for testability.
pub trait TerminalIO: Send + Sync {
    /// Read a line of input from the user, displaying the given prompt.
    fn read_line(&self, prompt: &str) -> Result<String, ActualError>;
    /// Write a line of text to the terminal.
    fn write_line(&self, text: &str);

    /// Prompt the user for a yes/no confirmation.
    ///
    /// Default implementation uses `read_line` as fallback.
    /// `RealTerminal` overrides this with `dialoguer::Confirm`.
    fn confirm(&self, prompt: &str) -> Result<bool, ActualError> {
        let input = self.read_line(prompt)?;
        match input.trim().to_lowercase().as_str() {
            "y" | "yes" => Ok(true),
            "n" | "no" => Ok(false),
            _ => Ok(false), // Default to rejection for safety
        }
    }

    /// Prompt the user for a yes/no confirmation, distinguishing cancel (Ctrl-C/Escape)
    /// from an explicit "No" response.
    ///
    /// Returns:
    /// - `Ok(Some(true))` — user confirmed
    /// - `Ok(Some(false))` — user rejected
    /// - `Ok(None)` — user cancelled (pressed Ctrl-C or Escape)
    ///
    /// Default implementation delegates to `confirm()` and wraps the result in `Some`.
    /// `RealTerminal` overrides this to pass through `interact_opt()`'s `None` directly.
    fn confirm_with_cancel(&self, prompt: &str) -> Result<Option<bool>, ActualError> {
        self.confirm(prompt).map(Some)
    }

    /// Present a multi-select file picker.
    /// Returns `Ok(Some(indices))` for confirmed selection, `Ok(None)` for cancel.
    fn select_files(
        &self,
        prompt: &str,
        items: &[String],
        defaults: &[bool],
    ) -> Result<Option<Vec<usize>>, ActualError>;

    /// Present a single-select list and return the index of the chosen item.
    /// Returns `Err(UserCancelled)` if the user presses Escape or Ctrl-C.
    fn select_one(
        &self,
        prompt: &str,
        items: &[String],
        default: Option<usize>,
    ) -> Result<usize, ActualError>;
}
