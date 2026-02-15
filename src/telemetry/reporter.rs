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
    let client = ActualApiClient::new(api_url);
    let _ = client.post_telemetry(&request, SERVICE_KEY).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::TelemetryConfig;
    use std::sync::Mutex;

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

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

    #[tokio::test]
    async fn test_report_sends_post_with_auth_header() {
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
    async fn test_report_opt_out_via_config() {
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
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = std::env::var("ACTUAL_NO_TELEMETRY").ok();

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

        // Restore
        match saved {
            Some(v) => std::env::set_var("ACTUAL_NO_TELEMETRY", v),
            None => std::env::remove_var("ACTUAL_NO_TELEMETRY"),
        }
    }

    #[tokio::test]
    async fn test_report_network_failure_no_panic() {
        let metrics = sample_metrics();
        let config = Config::default();
        // Point at unreachable address — should NOT panic
        report_metrics(&metrics, &config, "http://127.0.0.1:1").await;
    }

    #[test]
    fn test_service_key_not_in_log_output() {
        let source = include_str!("reporter.rs");
        for line in source.lines() {
            if line.contains("SERVICE_KEY") {
                assert!(
                    !line.contains("eprintln!")
                        && !line.contains("println!")
                        && !line.contains("dbg!"),
                    "SERVICE_KEY must not appear in log output: {line}"
                );
            }
        }
    }
}
