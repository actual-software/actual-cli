pub mod analysis;
pub mod api;
pub mod auth;
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
    AdvisorArgs, CacheAction, CacheArgs, Cli, Command, ConfigAction, ConfigArgs, ConfigSetArgs,
    LoginArgs, ModelsArgs, RunnerChoice, StatusArgs, SyncArgs,
};

pub fn run(cli: Cli) -> Result<(), ActualError> {
    match &cli.command {
        Command::AdrBot(args) => cli::commands::sync::exec(args),
        Command::Status(args) => cli::commands::status::exec(args),
        Command::Auth => cli::commands::auth::exec(),
        Command::Login(args) => cli::commands::login::exec(args),
        Command::Logout => cli::commands::logout::exec(),
        Command::Whoami => cli::commands::whoami::exec(),
        Command::Advisor(args) => cli::commands::advisor::exec(args),
        Command::MintToken(args) => cli::commands::mint_token::exec(args),
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

    #[test]
    fn test_run_whoami_logged_out_returns_err() {
        use crate::testutil::{EnvGuard, ENV_MUTEX};
        use tempfile::tempdir;

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        // Exercises the Command::Whoami dispatch arm (not signed in → error).
        let cli = Cli::parse_from(["actual", "whoami"]);
        assert!(run(cli).is_err());
    }

    #[test]
    fn test_run_logout_not_signed_in_returns_ok() {
        use crate::testutil::{EnvGuard, ENV_MUTEX};
        use tempfile::tempdir;

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        // Exercises the Command::Logout dispatch arm (not signed in → ok).
        let cli = Cli::parse_from(["actual", "logout"]);
        assert!(run(cli).is_ok());
    }

    #[test]
    fn test_run_login_bad_url_returns_err() {
        use crate::testutil::{EnvGuard, ENV_MUTEX};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard::remove("ACTUAL_AUTH_URL");

        // Exercises the Command::Login dispatch arm. resolve_auth_url now defaults
        // to the prod OAuth URL, so login no longer errors on a missing URL — pass
        // a non-HTTPS, non-loopback URL, which is rejected at the HTTPS check
        // before any network/browser work.
        let cli = Cli::parse_from([
            "actual",
            "login",
            "--no-browser",
            "--api-url",
            "http://example.com",
        ]);
        assert!(run(cli).is_err());
    }

    #[test]
    fn test_run_advisor_logged_out_returns_err() {
        use crate::testutil::{EnvGuard, ENV_MUTEX};
        use tempfile::tempdir;

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        // Exercises the Command::Advisor dispatch arm (not signed in → error,
        // before any network).
        let cli = Cli::parse_from(["actual", "advisor", "why?"]);
        assert!(run(cli).is_err());
    }

    #[test]
    fn test_run_mint_token_missing_key_returns_err() {
        // Exercises the Command::MintToken dispatch arm in-process. The
        // subprocess end-to-end tests (tests/mint_token_cli.rs) cannot cover
        // this arm: the binary exits via process::exit, which drops the
        // coverage profile before it is written. Here an explicit HTTPS issuer
        // and a --key path that does not exist make exec() fail cleanly at key
        // load — before any HTTP client is built or any request is sent — so
        // the arm is covered hermetically and deterministically, with no
        // network and no dependence on ambient environment variables (the
        // explicit --key wins over ACTUAL_SERVICE_ACCOUNT_KEY_FILE, and the
        // Some(path) read short-circuits before ACTUAL_SERVICE_ACCOUNT_KEY).
        let cli = Cli::parse_from([
            "actual",
            "mint-token",
            "--service-account-id",
            "00000000-0000-0000-0000-000000000000",
            "--kid",
            "test-kid",
            "--issuer",
            "https://issuer.example.test",
            "--key",
            "/no/such/mint-token-key.pem",
        ]);
        assert!(run(cli).is_err());
    }
}
