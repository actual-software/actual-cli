//! Production I/O wrappers for terminal and stdin, plus the sync entry point.
//!
//! This module is excluded from coverage measurement because
//! these types wrap OS-level I/O (`console::Term`, `std::io::stdin`)
//! that cannot be made to return errors in unit tests.
//!
//! `sync_run` lives here so that the `CliClaudeRunner` generic instantiation
//! of `run_sync` only exists in this coverage-excluded file — keeping
//! `sync.rs` at 100% line coverage.

use std::time::Duration;

use dialoguer::MultiSelect;

use crate::claude::binary::find_claude_binary;
use crate::claude::subprocess::CliClaudeRunner;
use crate::cli::args::SyncArgs;
use crate::cli::commands::auth::check_auth;
use crate::cli::commands::sync::{resolve_cwd, run_sync};
use crate::cli::ui::header::{print_header_bar, AuthDisplay};
use dialoguer::Confirm as DialoguerConfirm;

use crate::cli::ui::terminal::TerminalIO;
use crate::config::paths::config_path;
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
            .map_err(|e| ActualError::TerminalIOError(format!("confirmation failed: {e}")))
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
            .map_err(|e| ActualError::TerminalIOError(format!("file selection failed: {e}")))
    }
}

/// Production wiring for `actual sync`.
///
/// Resolves system dependencies (cwd, terminal, Claude binary) and delegates
/// to [`run_sync`].  Kept minimal so nearly all logic lives in the
/// fully-testable [`run_sync`].
pub(crate) fn sync_run(args: &SyncArgs) -> Result<(), ActualError> {
    let root_dir = resolve_cwd();
    let term = RealTerminal::default();

    // Phase 1 env check: locate Claude binary and verify auth
    let binary_path = find_claude_binary()?;
    let auth_status = check_auth(&binary_path)?;
    if !auth_status.is_usable() {
        return Err(ActualError::ClaudeNotAuthenticated);
    }

    let auth_display = AuthDisplay {
        authenticated: auth_status.logged_in,
        email: auth_status.email.clone(),
    };
    print_header_bar(&auth_display);

    let cfg_path = config_path()?;
    let runner = CliClaudeRunner::new(binary_path, Duration::from_secs(300));
    run_sync(args, &root_dir, &cfg_path, &term, &runner)
}
