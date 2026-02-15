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

use crate::claude::binary::find_claude_binary;
use crate::claude::subprocess::CliClaudeRunner;
use crate::cli::args::SyncArgs;
use crate::cli::commands::auth::check_auth;
use crate::cli::commands::sync::{resolve_cwd, run_sync};
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

    let runner = CliClaudeRunner::new(binary_path, Duration::from_secs(300));
    let reader = StdinReader;
    run_sync(args, &root_dir, &term, &runner, &reader)
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
