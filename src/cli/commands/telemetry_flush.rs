//! The internal `__telemetry-flush` uploader subcommand (APR-004).
//!
//! `rules select`, `plan-check`, `impl-check` and `check-override` write their
//! events to a durable local spool and then spawn this command as a **detached
//! child process**, so delivery has an owner that outlives the short-lived
//! parent command. Because a child process is reparented on the parent's exit
//! rather than terminated with it, delivery is no longer at the mercy of whether
//! a detached thread happens to outlive `main` — the parent returns immediately
//! while this process drains the spool through the proxy and exits.
//!
//! `rules brief --claude-hook` spawns it the same way for the OTLP log spool,
//! and every run drains both spools (see [`crate::telemetry::otlp::flush`]).
//!
//! Hidden from `--help`: it is an internal delivery mechanism, never a
//! user-facing command. It is deliberately incurious about its outcome — a slow,
//! failed, or opted-out send simply leaves events queued for the next run, and
//! an event the proxy refuses on its own is dropped (see
//! [`crate::telemetry::spool::flush`]). OTLP records leave their spool
//! before they are sent, so a failed send drops them; an opted-out run, or one
//! whose config file can't load, leaves both spools queued.

use crate::error::ActualError;

/// Drain the scope-telemetry spool and the OTLP log spool once, side by side,
/// then return. Never fails: telemetry delivery must never surface an error,
/// and this process exists only to try.
#[cfg(feature = "telemetry")]
pub fn exec() -> Result<(), ActualError> {
    let Some(cfg) = crate::telemetry::opt_out::load_if_enabled() else {
        return Ok(());
    };
    // The opt-out gates and the empty-spool short-circuits live inside each
    // `flush`; this owns only the runtime the blocking sends need.
    let api_url = crate::telemetry::spool::resolved_api_url(&cfg);
    if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        rt.block_on(drain(&cfg, &api_url));
    }
    Ok(())
}

/// Both spools at once, so neither waits on the other's endpoint.
#[cfg(feature = "telemetry")]
async fn drain(cfg: &crate::config::types::Config, api_url: &str) {
    tokio::join!(
        crate::telemetry::spool::flush(cfg, api_url),
        crate::telemetry::otlp::flush(cfg),
    );
}

/// Without the telemetry feature there is no spool to drain, so the command is a
/// no-op that still parses and exits cleanly.
#[cfg(not(feature = "telemetry"))]
pub fn exec() -> Result<(), ActualError> {
    Ok(())
}

#[cfg(all(test, feature = "telemetry"))]
mod tests {
    use super::*;
    use crate::testutil::{
        EnvGuard, ENV_MUTEX, LOADABLE_TELEMETRY_ON, UNLOADABLE_TELEMETRY_OPT_OUT,
    };
    use tempfile::tempdir;

    /// The flush command drains a spooled event through the proxy and exits Ok.
    /// It owns delivery for the command that spawned it, so it must both
    /// deliver and never surface an error.
    #[test]
    fn test_exec_drains_the_spool_and_returns_ok() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _no_tel = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/plan-governance/record")
            .with_status(200)
            .with_body(r#"{"recorded":1,"failed":0}"#)
            .create();

        // Point config at the mock, and seed one event into the spool.
        let config_yaml = format!("api_url: {}\n", server.url());
        std::fs::write(home.path().join("config.yaml"), config_yaml).unwrap();
        let event = crate::api::types::PlanGovernanceEvent {
            event: crate::api::types::PlanGovernanceEventName::PlanGovernanceScopeSelect,
            distinct_id: "install-abc".to_string(),
            properties: None,
            timestamp: None,
            insert_id: "id-flush-1".to_string(),
        };
        crate::telemetry::spool::append(&event).unwrap();

        assert!(exec().is_ok());
        mock.assert();
        assert!(
            crate::telemetry::spool::load_all().is_empty(),
            "the delivered event must be drained from the spool"
        );
    }

    /// The same run drains the OTLP log spool, here to a file sink.
    #[test]
    fn test_exec_drains_the_otlp_spool_too() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _guards: Vec<EnvGuard> = [
            "ACTUAL_NO_TELEMETRY",
            "ACTUAL_OTLP_LOGS_ENDPOINT",
            "ACTUAL_OTLP_FILE",
        ]
        .into_iter()
        .map(EnvGuard::remove)
        .collect();
        let sink = home.path().join("otlp.jsonl");
        let config_yaml = format!("telemetry:\n  otlp_file: {}\n", sink.display());
        std::fs::write(home.path().join("config.yaml"), config_yaml).unwrap();
        let record = crate::telemetry::otlp::Origin::resolve().context_added(
            "s1",
            None,
            "grp-1",
            &["adr-1".to_string()],
        );
        crate::telemetry::otlp::spool(&[record]);

        assert!(exec().is_ok());

        let written = std::fs::read_to_string(&sink).unwrap();
        assert_eq!(written.lines().count(), 1, "{written}");
        assert!(written.contains(crate::telemetry::otlp::ADR_DETECTED));
        assert!(!home.path().join("otlp-logs-spool.jsonl").exists());
    }

    /// With no spooled events there is nothing to send and nothing to fail.
    #[test]
    fn test_exec_on_empty_spool_returns_ok() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        assert!(exec().is_ok());
    }

    /// Spool one OTLP record, run once with `config` in the config file and
    /// `ACTUAL_OTLP_FILE` naming a local sink, and return what reached the sink
    /// and whether the record is still queued. The test uses the OTLP spool
    /// because its output can be a file. Through the scope spool, an uploader
    /// that ignored a config it couldn't load would post to the production API.
    fn drain_one_record_under(config: &str) -> (Option<String>, bool) {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _guards: Vec<EnvGuard> = [
            "ACTUAL_CONFIG",
            "ACTUAL_NO_TELEMETRY",
            "ACTUAL_OTLP_LOGS_ENDPOINT",
        ]
        .into_iter()
        .map(EnvGuard::remove)
        .collect();
        let sink = home.path().join("otlp.jsonl");
        let _sink = EnvGuard::set("ACTUAL_OTLP_FILE", sink.to_str().unwrap());
        std::fs::write(home.path().join("config.yaml"), config).unwrap();
        let record = crate::telemetry::otlp::Origin::resolve().context_added(
            "s1",
            None,
            "grp-1",
            &["adr-1".to_string()],
        );
        crate::telemetry::otlp::spool(&[record]);

        assert!(exec().is_ok());
        let queued = home.path().join("otlp-logs-spool.jsonl").exists();
        (std::fs::read_to_string(&sink).ok(), queued)
    }

    /// `telemetry.enabled: false` in a config file that doesn't load: nothing
    /// is sent, and the record stays queued.
    #[test]
    fn test_exec_drains_nothing_when_the_config_cannot_load() {
        let (sent, queued) = drain_one_record_under(UNLOADABLE_TELEMETRY_OPT_OUT);
        assert_eq!(sent, None);
        assert!(queued);
    }

    /// The positive control: a config that loads lets the record through.
    #[test]
    fn test_exec_drains_the_record_when_the_config_loads() {
        let (sent, queued) = drain_one_record_under(LOADABLE_TELEMETRY_ON);
        assert_eq!(sent.map(|body| body.lines().count()), Some(1));
        assert!(!queued);
    }
}
