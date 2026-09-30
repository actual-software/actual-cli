//! The internal `__telemetry-flush` uploader subcommand (APR-004).
//!
//! `rules select` writes its scope event to a durable local spool and then
//! spawns this command as a **detached child process**, so scope-telemetry
//! delivery has an owner that outlives the short-lived parent command. Because a
//! child process is reparented on the parent's exit rather than terminated with
//! it, delivery is no longer at the mercy of whether a detached thread happens to
//! outlive `main` — the parent returns immediately while this process drains the
//! spool through the proxy and exits.
//!
//! Hidden from `--help`: it is an internal delivery mechanism, never a
//! user-facing command. It is deliberately incurious about its outcome — a slow,
//! failed, or opted-out send simply leaves events queued for the next run (see
//! [`crate::telemetry::spool::flush`]).

use crate::error::ActualError;

/// Drain the scope-telemetry spool once, then return. Never fails: telemetry
/// delivery must never surface an error, and this process exists only to try.
#[cfg(feature = "telemetry")]
pub fn exec() -> Result<(), ActualError> {
    let cfg = crate::config::paths::load().unwrap_or_default();
    // The opt-out gate and the empty-spool short-circuit both live inside
    // `flush`; this owns only the runtime the blocking send needs.
    let api_url = crate::telemetry::spool::resolved_api_url(&cfg);
    if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        rt.block_on(crate::telemetry::spool::flush(&cfg, &api_url));
    }
    Ok(())
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
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use tempfile::tempdir;

    /// The flush command drains a spooled event through the proxy and exits Ok.
    /// It owns delivery for the parent `rules select` process, so it must both
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

    /// With no spooled events there is nothing to send and nothing to fail.
    #[test]
    fn test_exec_on_empty_spool_returns_ok() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        assert!(exec().is_ok());
    }
}
