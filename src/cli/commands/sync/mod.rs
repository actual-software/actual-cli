mod adr_utils;
mod cache;
mod pipeline;
mod write;

pub(crate) use pipeline::{resolve_cwd, run_sync};
pub use write::{confirm_and_write, SyncResult};

use crate::cli::args::SyncArgs;
use crate::error::ActualError;

/// Entry point for `actual sync`.
///
/// Production wiring (`CliClaudeRunner`, `RealTerminal`) lives in
/// `sync_wiring.rs` (which is excluded from coverage) so that the only
/// generic instantiation of `run_sync` in `sync.rs` is `MockRunner` from
/// unit tests.
pub fn exec(args: &SyncArgs) -> Result<(), ActualError> {
    super::sync_wiring::sync_run(args)
}
