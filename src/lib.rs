pub mod analysis;
pub mod api;
pub mod branding;
pub mod cli;
pub mod config;
pub mod error;
pub mod generation;
pub mod model_cache;
pub mod runner;
pub mod tailoring;
#[cfg(feature = "telemetry")]
pub mod telemetry;

pub use cli::commands::handle_result;

#[cfg(test)]
pub(crate) mod testutil;

pub use error::ActualError;

// Re-export CLI types for backward compatibility with tests
pub use cli::args::{
    CacheAction, CacheArgs, Cli, Command, ConfigAction, ConfigArgs, ConfigSetArgs, ModelsArgs,
    RunnerChoice, StatusArgs, SyncArgs,
};

pub fn run(cli: Cli) -> Result<(), ActualError> {
    match &cli.command {
        Command::AdrBot(args) => cli::commands::sync::exec(args),
        Command::Status(args) => cli::commands::status::exec(args),
        Command::Auth => cli::commands::auth::exec(),
        Command::Config(args) => cli::commands::config::exec(args),
        Command::Runners => cli::commands::runners::exec(),
        Command::Models(args) => cli::commands::models::exec(args.no_fetch),
        Command::Cache(args) => cli::commands::cache::exec(args),
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

    #[test]
    fn test_run_cache_clear_returns_ok() {
        use crate::testutil::{EnvGuard, ENV_MUTEX};
        use tempfile::tempdir;

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");
        config::paths::save_to(&config::Config::default(), &config_file).unwrap();

        // Redirect config path via env var so the live ~/.actualai config is not used.
        let _guard = EnvGuard::set("ACTUAL_CONFIG", config_file.to_str().unwrap());
        let cli = Cli::parse_from(["actual", "cache", "clear"]);
        let result = run(cli);

        assert!(result.is_ok());
    }
}
