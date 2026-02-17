use crate::api::client::ActualApiClient;
use crate::config::types::Config;
use crate::telemetry::metrics::SyncMetrics;

const SERVICE_KEY: &str = "ak_telemetry_prod_actual_cli";

/// Send collected sync metrics to the telemetry endpoint.
///
/// This function is fire-and-forget: all errors are silently swallowed.
/// Telemetry is opt-out — it can be disabled via the `ACTUAL_NO_TELEMETRY=1`
/// environment variable or by setting `telemetry.enabled: false` in config.
pub async fn report_metrics(metrics: &SyncMetrics, config: &Config, api_url: &str) {
    // Opt-out via env var
    if std::env::var("ACTUAL_NO_TELEMETRY").ok().as_deref() == Some("1") {
        return;
    }

    // Opt-out via config
    if !config
        .telemetry
        .as_ref()
        .and_then(|t| t.enabled)
        .unwrap_or(true)
    {
        return;
    }

    let request = metrics.to_counter_request();
    let client = match ActualApiClient::new(api_url) {
        Ok(c) => c,
        Err(_) => return,
    };
    let _ = client.post_telemetry(&request, SERVICE_KEY).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::TelemetryConfig;
    use crate::testutil::ENV_MUTEX;

    fn sample_metrics() -> SyncMetrics {
        SyncMetrics {
            adrs_fetched: 5,
            adrs_tailored: 3,
            adrs_rejected: 1,
            adrs_written: 2,
            repo_hash: "abc123".to_string(),
            version: "1.0.0".to_string(),
        }
    }

    /// Save and clear the ACTUAL_NO_TELEMETRY env var, returning the saved value.
    fn save_and_clear_env() -> Option<String> {
        let saved = std::env::var("ACTUAL_NO_TELEMETRY").ok();
        std::env::remove_var("ACTUAL_NO_TELEMETRY");
        saved
    }

    /// Restore the ACTUAL_NO_TELEMETRY env var from a previously saved value.
    fn restore_env(saved: Option<String>) {
        if let Some(v) = saved {
            std::env::set_var("ACTUAL_NO_TELEMETRY", v);
        }
    }

    #[tokio::test]
    async fn test_report_sends_post_with_auth_header() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_and_clear_env();

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

        restore_env(saved);
    }

    #[tokio::test]
    async fn test_report_opt_out_via_config() {
        // Hold the env mutex and ensure the env var is unset so the config
        // check is actually reached (tests run in parallel).
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_and_clear_env();

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

        restore_env(saved);
    }

    #[tokio::test]
    async fn test_report_opt_out_via_env_var() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_and_clear_env();

        std::env::set_var("ACTUAL_NO_TELEMETRY", "1");

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

        // Restore: remove the var we set, then restore any previous value
        std::env::remove_var("ACTUAL_NO_TELEMETRY");
        restore_env(saved);
    }

    #[tokio::test]
    async fn test_report_network_failure_no_panic() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_and_clear_env();

        let metrics = sample_metrics();
        let config = Config::default();
        // Point at unreachable address — should NOT panic
        report_metrics(&metrics, &config, "http://127.0.0.1:1").await;

        restore_env(saved);
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
    fn test_save_and_restore_env_round_trip() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // Ensure clean state
        std::env::remove_var("ACTUAL_NO_TELEMETRY");
        assert!(std::env::var("ACTUAL_NO_TELEMETRY").is_err());

        // Save (None) and clear
        let saved = save_and_clear_env();
        assert!(saved.is_none());

        // Set it, then save and clear
        std::env::set_var("ACTUAL_NO_TELEMETRY", "1");
        let saved = save_and_clear_env();
        assert_eq!(saved.as_deref(), Some("1"));
        assert!(std::env::var("ACTUAL_NO_TELEMETRY").is_err());

        // Restore it
        restore_env(saved);
        assert_eq!(
            std::env::var("ACTUAL_NO_TELEMETRY").ok().as_deref(),
            Some("1")
        );

        // Restore None (no-op)
        restore_env(None);
        assert_eq!(
            std::env::var("ACTUAL_NO_TELEMETRY").ok().as_deref(),
            Some("1")
        );

        // Clean up
        std::env::remove_var("ACTUAL_NO_TELEMETRY");
    }
}
