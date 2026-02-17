//! Production wiring for `actual sync`.
//!
//! This module is excluded from coverage measurement (see coverage.yml)
//! because it instantiates `CliClaudeRunner` — a real subprocess runner
//! that cannot be used in unit tests. The generic monomorphization of
//! `run_sync<CliClaudeRunner>` only exists here, keeping `sync.rs` at
//! 100% line coverage.

use std::time::Duration;

use crate::claude::binary::find_claude_binary;
use crate::claude::subprocess::CliClaudeRunner;
use crate::cli::args::SyncArgs;
use crate::cli::commands::auth::check_auth;
use crate::cli::commands::sync::{resolve_cwd, run_sync};
use crate::cli::ui::header::{print_header_bar, AuthDisplay};
use crate::cli::ui::real_terminal::RealTerminal;
use crate::config::paths::config_path;
use crate::error::ActualError;

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
