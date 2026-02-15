use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

/// Symbols for terminal status output
pub const SUCCESS_SYMBOL: &str = "✔";
pub const ERROR_SYMBOL: &str = "✖";
pub const WARN_SYMBOL: &str = "⚠";

/// An animated terminal spinner with completion states.
///
/// When `quiet` is true, all operations are no-ops (no output produced).
pub struct Spinner {
    bar: Option<ProgressBar>,
}

impl Spinner {
    /// Create a new spinner with the given message.
    /// If `quiet` is true, no output is produced.
    pub fn new(message: &str, quiet: bool) -> Self {
        if quiet {
            return Self { bar: None };
        }

        let bar = ProgressBar::new_spinner();
        bar.set_style(
            ProgressStyle::with_template("{spinner} {msg}").expect("invalid spinner template"),
        );
        bar.set_message(message.to_string());
        bar.enable_steady_tick(Duration::from_millis(80));

        Self { bar: Some(bar) }
    }

    /// Update the spinner message while running.
    pub fn update(&self, message: &str) {
        if let Some(bar) = &self.bar {
            bar.set_message(message.to_string());
        }
    }

    /// Stop the spinner and show a success (green checkmark) message.
    pub fn success(&self, message: &str) {
        if let Some(bar) = &self.bar {
            bar.finish_with_message(format!("{} {}", style(SUCCESS_SYMBOL).green(), message));
        }
    }

    /// Stop the spinner and show an error (red X) message.
    pub fn error(&self, message: &str) {
        if let Some(bar) = &self.bar {
            bar.finish_with_message(format!("{} {}", style(ERROR_SYMBOL).red(), message));
        }
    }

    /// Stop the spinner and show a warning (yellow triangle) message.
    pub fn warn(&self, message: &str) {
        if let Some(bar) = &self.bar {
            bar.finish_with_message(format!("{} {}", style(WARN_SYMBOL).yellow(), message));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spinner_quiet_mode() {
        let spinner = Spinner::new("test", true);
        assert!(
            spinner.bar.is_none(),
            "expected bar to be None in quiet mode"
        );
    }

    #[test]
    fn test_spinner_active_mode() {
        let spinner = Spinner::new("test", false);
        assert!(
            spinner.bar.is_some(),
            "expected bar to be Some in active mode"
        );
    }

    #[test]
    fn test_spinner_quiet_operations_no_panic() {
        let spinner = Spinner::new("test", true);
        spinner.update("updating");
        spinner.success("done");
        spinner.error("failed");
        spinner.warn("careful");
    }

    #[test]
    fn test_symbol_constants() {
        assert_eq!(SUCCESS_SYMBOL, "✔", "expected green checkmark symbol");
        assert_eq!(ERROR_SYMBOL, "✖", "expected red X symbol");
        assert_eq!(WARN_SYMBOL, "⚠", "expected yellow triangle symbol");
    }

    #[test]
    fn test_spinner_active_update() {
        let spinner = Spinner::new("initial", false);
        spinner.update("updated message");
        let bar = spinner.bar.as_ref().expect("expected active spinner");
        assert_eq!(bar.message(), "updated message");
    }

    #[test]
    fn test_spinner_active_success() {
        let spinner = Spinner::new("working", false);
        spinner.success("completed");
        let bar = spinner.bar.as_ref().expect("expected active spinner");
        let msg = bar.message().to_string();
        assert!(msg.contains("completed"), "expected 'completed' in: {msg}");
        assert!(bar.is_finished(), "expected spinner to be finished");
    }

    #[test]
    fn test_spinner_active_error() {
        let spinner = Spinner::new("working", false);
        spinner.error("something broke");
        let bar = spinner.bar.as_ref().expect("expected active spinner");
        let msg = bar.message().to_string();
        assert!(
            msg.contains("something broke"),
            "expected 'something broke' in: {msg}"
        );
        assert!(bar.is_finished(), "expected spinner to be finished");
    }

    #[test]
    fn test_spinner_active_warn() {
        let spinner = Spinner::new("working", false);
        spinner.warn("be careful");
        let bar = spinner.bar.as_ref().expect("expected active spinner");
        let msg = bar.message().to_string();
        assert!(
            msg.contains("be careful"),
            "expected 'be careful' in: {msg}"
        );
        assert!(bar.is_finished(), "expected spinner to be finished");
    }
}
