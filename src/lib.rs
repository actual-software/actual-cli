pub mod analysis;
pub mod api;
pub mod branding;
pub mod cli;
pub mod config;
pub mod error;
pub mod generation;
pub mod runner;
pub mod tailoring;
pub mod telemetry;

pub use cli::commands::handle_result;

#[cfg(test)]
pub(crate) mod testutil;

pub use error::ActualError;

// Re-export CLI types for backward compatibility with tests
pub use cli::args::{
    Cli, Command, ConfigAction, ConfigArgs, ConfigSetArgs, RunnerChoice, StatusArgs, SyncArgs,
};

pub fn run(cli: Cli) -> Result<(), ActualError> {
    match &cli.command {
        Command::Sync(args) => cli::commands::sync::exec(args),
        Command::Status(args) => cli::commands::status::exec(args),
        Command::Auth => cli::commands::auth::exec(),
        Command::Config(args) => cli::commands::config::exec(args),
        Command::Runners => cli::commands::runners::exec(),
        Command::Models => cli::commands::models::exec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_run_runners_returns_ok() {
        let cli = Cli::parse_from(["actual", "runners"]);
        assert!(run(cli).is_ok());
    }
}
