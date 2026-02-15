pub mod analysis;
pub mod api;
pub mod branding;
pub mod claude;
pub mod cli;
pub mod config;
pub mod error;
pub mod generation;
pub mod tailoring;
pub mod telemetry;

pub use error::ActualError;

// Re-export CLI types for backward compatibility with tests
pub use cli::args::{Cli, Command, ConfigAction, ConfigArgs, ConfigSetArgs, StatusArgs, SyncArgs};

pub fn run(cli: Cli) -> i32 {
    match &cli.command {
        Command::Sync(args) => cli::commands::sync::exec(args),
        Command::Status(args) => cli::commands::status::exec(args),
        Command::Auth => cli::commands::auth::exec(),
        Command::Config(args) => cli::commands::config::exec(args),
    }
}
