use crate::cli::ui::panel::Panel;
use crate::cli::ui::theme;
use crate::error::ActualError;

pub mod auth;
pub mod config;
pub mod status;
pub mod sync;

/// Convert a command result to an exit code, printing any error.
pub(crate) fn handle_result(result: Result<(), ActualError>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            let width = console::Term::stderr()
                .size_checked()
                .map(|(_, cols)| cols as usize)
                .unwrap_or(80)
                .min(90);

            let error_line = format!("{} {}", theme::error_prefix().for_stderr(), e);
            let mut panel = Panel::titled("Error").line("").line(&error_line);

            if let Some(hint_text) = e.hint() {
                let hint_styled = theme::hint(hint_text).for_stderr();
                panel = panel.line("").line(&format!("Fix: {hint_styled}"));
            }

            panel = panel.line("");

            eprintln!("{}", panel.render(width));
            e.exit_code()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_result_ok() {
        assert_eq!(handle_result(Ok(())), 0);
    }

    #[test]
    fn test_handle_result_user_cancelled() {
        let code = handle_result(Err(ActualError::UserCancelled));
        assert_eq!(code, 4);
    }

    #[test]
    fn test_handle_result_config_error() {
        let code = handle_result(Err(ActualError::ConfigError("bad".to_string())));
        assert_eq!(code, 1);
    }
}
