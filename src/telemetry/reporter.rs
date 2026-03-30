use crate::api::client::ActualApiClient;
use crate::config::types::Config;
use crate::error::ActualError;
use crate::telemetry::metrics::SyncMetrics;

/// Public write-only telemetry key — safe to embed in client binaries.
///
/// This key is scoped exclusively to submitting counter metrics via
/// `POST /counter/record`. It does not grant read, admin, or any other
/// access to the telemetry service. Embedding it in the compiled binary
/// is intentional and carries the same risk profile as a public Segment
/// write key or a Mixpanel project token.
///
/// If this key is rotated, update it here and redeploy. No other files
/// reference this constant.
const SERVICE_KEY: &str = "ak_telemetry_prod_actual_cli";

/// Send collected sync metrics to the telemetry endpoint.
///
/// Called from the sync pipeline on every successful sync run. This function
/// is fire-and-forget: errors are logged at debug level and then discarded.
/// Telemetry is opt-out — it can be disabled via the `ACTUAL_NO_TELEMETRY`
/// environment variable (any non-empty value) or by setting
/// `telemetry.enabled: false` in config.
pub async fn report_metrics(metrics: &SyncMetrics, config: &Config, api_url: &str) {
    if let Err(e) = try_report_metrics(metrics, config, api_url).await {
        tracing::debug!("telemetry: {e}");
    }
}

/// Inner implementation that returns errors instead of swallowing them.
///
/// Extracted so callers can inspect failures in tests while
/// [`report_metrics`] remains fire-and-forget.
async fn try_report_metrics(
    metrics: &SyncMetrics,
    config: &Config,
    api_url: &str,
) -> Result<(), ActualError> {
    // Opt-out via env var
    if std::env::var("ACTUAL_NO_TELEMETRY")
        .ok()
        .filter(|v| !v.is_empty())
        .is_some()
    {
        return Ok(());
    }

    // Opt-out via config
    if !config
        .telemetry
        .as_ref()
        .and_then(|t| t.enabled)
        .unwrap_or(true)
    {
        return Ok(());
    }

    let request = metrics.to_counter_request();
    let client = ActualApiClient::new(api_url)?;
    client.post_telemetry(&request, SERVICE_KEY).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::TelemetryConfig;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

    fn sample_metrics() -> SyncMetrics {
        SyncMetrics {
            adrs_fetched: 5,
            adrs_tailored: 3,
            adrs_rejected: 1,
            adrs_written: 2,
            repo_hash: "abc123".to_string(),
            repo_url_hash: "urlhash123".to_string(),
            version: "1.0.0".to_string(),
            matched_adr_ids: vec![],
        }
    }

    #[tokio::test]
    async fn test_report_sends_post_with_auth_header() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/counter/record")
            .match_header("authorization", mockito::Matcher::Regex("Bearer .+".into()))
            .match_header("content-type", "application/json")
            .with_status(200)
            .create_async()
            .await;

        let metrics = sample_metrics();
        let config = Config::default();

        report_metrics(&metrics, &config, &server.url()).await;
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_try_report_returns_ok_on_success() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/counter/record")
            .with_status(200)
            .create_async()
            .await;

        let metrics = sample_metrics();
        let config = Config::default();

        let result = try_report_metrics(&metrics, &config, &server.url()).await;
        assert!(result.is_ok());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_try_report_returns_err_on_network_failure() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        let metrics = sample_metrics();
        let config = Config::default();

        let result = try_report_metrics(&metrics, &config, "http://127.0.0.1:1").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_report_opt_out_via_config() {
        // Hold the env mutex and ensure the env var is unset so the config
        // check is actually reached (tests run in parallel).
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/counter/record")
            .expect(0)
            .create_async()
            .await;

        let metrics = sample_metrics();
        let config = Config {
            telemetry: Some(TelemetryConfig {
                enabled: Some(false),
            }),
            ..Default::default()
        };

        report_metrics(&metrics, &config, &server.url()).await;
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_report_opt_out_via_env_var() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/counter/record")
            .expect(0)
            .create_async()
            .await;

        let metrics = sample_metrics();
        let config = Config::default();

        report_metrics(&metrics, &config, &server.url()).await;
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_report_opt_out_via_env_var_true() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("ACTUAL_NO_TELEMETRY", "true");

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/counter/record")
            .expect(0)
            .create_async()
            .await;

        let metrics = sample_metrics();
        let config = Config::default();

        report_metrics(&metrics, &config, &server.url()).await;
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_report_opt_out_via_env_var_yes() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("ACTUAL_NO_TELEMETRY", "yes");

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/counter/record")
            .expect(0)
            .create_async()
            .await;

        let metrics = sample_metrics();
        let config = Config::default();

        report_metrics(&metrics, &config, &server.url()).await;
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_report_not_opted_out_via_empty_env_var() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("ACTUAL_NO_TELEMETRY", "");

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/counter/record")
            .with_status(200)
            .create_async()
            .await;

        let metrics = sample_metrics();
        let config = Config::default();

        report_metrics(&metrics, &config, &server.url()).await;
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_report_network_failure_no_panic() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        let metrics = sample_metrics();
        let config = Config::default();
        // Point at unreachable address — should NOT panic
        report_metrics(&metrics, &config, "http://127.0.0.1:1").await;
    }

    #[test]
    fn test_service_key_not_in_log_output() {
        let source = include_str!("reporter.rs");
        let has_violation = source.lines().any(|line| {
            line.contains("SERVICE_KEY")
                && (line.contains("eprintln!")
                    || line.contains("println!")
                    || line.contains("dbg!"))
        });
        assert!(!has_violation);
    }

    #[test]
    fn test_env_guard_round_trip() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Ensure clean state
        let _guard = EnvGuard::remove("ACTUAL_NO_TELEMETRY");
        assert!(std::env::var("ACTUAL_NO_TELEMETRY").is_err());

        // Set it via guard and verify
        {
            let _g = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
            assert_eq!(
                std::env::var("ACTUAL_NO_TELEMETRY").ok().as_deref(),
                Some("1")
            );
        } // guard drops here, restores to absent

        assert!(std::env::var("ACTUAL_NO_TELEMETRY").is_err());
    }
}
