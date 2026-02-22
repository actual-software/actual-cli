mod adr_utils;
mod cache;
mod pipeline;
mod write;

use crate::cli::args::SyncArgs;
use crate::error::ActualError;
pub(crate) use pipeline::{resolve_cwd, run_sync};
pub use write::{confirm_and_write, SyncResult};

pub fn exec(args: &SyncArgs) -> Result<(), ActualError> {
    super::sync_wiring::sync_run(args)
}
