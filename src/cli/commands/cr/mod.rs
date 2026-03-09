pub mod adr_reader;
pub mod diff;
pub mod schemas;
pub mod types;

use crate::cli::args::CrArgs;
use crate::error::ActualError;

/// Entry point for the `actual cr` command.
///
/// Currently a stub — the full pipeline (diff, ADR reading, parallel lens
/// reviews, aggregation, synthesis, and display) will be wired up in
/// subsequent issues.
pub fn exec(_args: &CrArgs) -> Result<(), ActualError> {
    eprintln!("actual cr: not yet implemented");
    Ok(())
}
