//! Shared test utilities for terminal I/O mocking.
//!
//! Provides a single [`MockTerminal`] that replaces the three near-identical
//! copies that previously lived in `confirm.rs`, `file_confirm.rs`, and
//! `commands/sync.rs`.

use crate::cli::ui::terminal::TerminalIO;
use crate::error::ActualError;
use std::sync::Mutex;

/// A configurable mock terminal for testing that records output and returns
/// predetermined inputs in sequence.
///
/// Uses the default `confirm()` implementation from [`TerminalIO`],
/// which reads from `read_line` and parses y/n.
///
/// # Examples
///
/// ```ignore
/// // Simple read_line mock:
/// let term = MockTerminal::new(vec!["y"]);
///
/// // With file selection:
/// let term = MockTerminal::new(vec!["y"]).with_selection(Some(vec![0, 1]));
///
/// // Selection-only (read_line always errors):
/// let term = MockTerminal::with_selection_only(Some(vec![0, 1]));
/// ```
pub struct MockTerminal {
    inputs: Mutex<Vec<String>>,
    select_result: Mutex<Option<Option<Vec<usize>>>>,
    output: Mutex<Vec<String>>,
}

impl MockTerminal {
    /// Create a mock with predetermined `read_line` responses.
    ///
    /// `select_files` will return `Err(UserCancelled)` unless
    /// [`with_selection`](Self::with_selection) is chained.
    pub fn new(inputs: Vec<&str>) -> Self {
        Self {
            inputs: Mutex::new(inputs.into_iter().map(|s| s.to_string()).collect()),
            select_result: Mutex::new(None),
            output: Mutex::new(Vec::new()),
        }
    }

    /// Create a mock with only a file-selection result.
    ///
    /// `read_line` will always return `Err(UserCancelled)`.
    pub fn with_selection_only(result: Option<Vec<usize>>) -> Self {
        Self {
            inputs: Mutex::new(Vec::new()),
            select_result: Mutex::new(Some(result)),
            output: Mutex::new(Vec::new()),
        }
    }

    /// Set the file-selection result (builder pattern).
    pub fn with_selection(mut self, result: Option<Vec<usize>>) -> Self {
        self.select_result = Mutex::new(Some(result));
        self
    }

    /// Get all recorded output joined by newlines.
    pub fn output_text(&self) -> String {
        self.output.lock().unwrap().join("\n")
    }
}

impl TerminalIO for MockTerminal {
    fn read_line(&self, _prompt: &str) -> Result<String, ActualError> {
        let mut inputs = self.inputs.lock().unwrap();
        if inputs.is_empty() {
            return Err(ActualError::UserCancelled);
        }
        Ok(inputs.remove(0))
    }

    fn write_line(&self, text: &str) {
        self.output.lock().unwrap().push(text.to_string());
    }

    fn select_files(
        &self,
        _prompt: &str,
        _items: &[String],
        _defaults: &[bool],
    ) -> Result<Option<Vec<usize>>, ActualError> {
        self.select_result
            .lock()
            .unwrap()
            .take()
            .ok_or(ActualError::UserCancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_line_returns_inputs_in_order() {
        let term = MockTerminal::new(vec!["first", "second"]);
        assert_eq!(term.read_line("p").unwrap(), "first");
        assert_eq!(term.read_line("p").unwrap(), "second");
    }

    #[test]
    fn read_line_returns_error_when_exhausted() {
        let term = MockTerminal::new(vec![]);
        assert!(matches!(
            term.read_line("p"),
            Err(ActualError::UserCancelled)
        ));
    }

    #[test]
    fn write_line_records_output() {
        let term = MockTerminal::new(vec![]);
        term.write_line("hello");
        term.write_line("world");
        assert_eq!(term.output_text(), "hello\nworld");
    }

    #[test]
    fn select_files_returns_preset_result() {
        let term = MockTerminal::new(vec![]).with_selection(Some(vec![0, 2]));
        let result = term.select_files("p", &[], &[]).unwrap();
        assert_eq!(result, Some(vec![0, 2]));
    }

    #[test]
    fn select_files_returns_error_when_no_result_set() {
        let term = MockTerminal::new(vec![]);
        let result = term.select_files("p", &[], &[]);
        assert!(matches!(result, Err(ActualError::UserCancelled)));
    }

    #[test]
    fn with_selection_only_read_line_errors() {
        let term = MockTerminal::with_selection_only(Some(vec![0]));
        assert!(matches!(
            term.read_line("p"),
            Err(ActualError::UserCancelled)
        ));
        // But select_files works
        let result = term.select_files("p", &[], &[]).unwrap();
        assert_eq!(result, Some(vec![0]));
    }

    #[test]
    fn confirm_uses_default_trait_impl() {
        let term = MockTerminal::new(vec!["y"]);
        assert!(term.confirm("proceed?").unwrap());

        let term = MockTerminal::new(vec!["n"]);
        assert!(!term.confirm("proceed?").unwrap());
    }

    #[test]
    fn confirm_with_cancel_default_wraps_confirm_result_in_some() {
        // The default `confirm_with_cancel` delegates to `confirm` and wraps in `Some`.
        // MockTerminal uses the default implementation (does not override it).
        let term = MockTerminal::new(vec!["y"]);
        assert_eq!(term.confirm_with_cancel("proceed?").unwrap(), Some(true));

        let term = MockTerminal::new(vec!["n"]);
        assert_eq!(term.confirm_with_cancel("proceed?").unwrap(), Some(false));
    }

    #[test]
    fn select_files_none_means_cancel() {
        let term = MockTerminal::new(vec![]).with_selection(None);
        let result = term.select_files("p", &[], &[]).unwrap();
        assert_eq!(result, None);
    }
}
