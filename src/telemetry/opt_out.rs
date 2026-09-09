use crate::config::types::Config;

/// True when telemetry is disabled by either opt-out mechanism: the
/// `ACTUAL_NO_TELEMETRY` env var (any non-empty value) or
/// `telemetry.enabled: false` in config.
///
/// Extracted from `reporter::try_report_metrics` so every telemetry sender
/// (sync counters, plan-governance events) shares exactly one opt-out check
/// — see `PRIVACY.md`'s "three independent ways to disable telemetry."
/// (The third way, compile-time removal of the `telemetry` feature, isn't
/// representable here since it removes this module entirely.)
pub fn is_disabled(config: &Config) -> bool {
    if std::env::var("ACTUAL_NO_TELEMETRY")
        .ok()
        .filter(|v| !v.is_empty())
        .is_some()
    {
        return true;
    }

    !config
        .telemetry
        .as_ref()
        .and_then(|t| t.enabled)
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::TelemetryConfig;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

    #[test]
    fn test_not_disabled_by_default() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("ACTUAL_NO_TELEMETRY");
        assert!(!is_disabled(&Config::default()));
    }

    #[test]
    fn test_disabled_via_env_var() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
        assert!(is_disabled(&Config::default()));
    }

    #[test]
    fn test_not_disabled_via_empty_env_var() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("ACTUAL_NO_TELEMETRY", "");
        assert!(!is_disabled(&Config::default()));
    }

    #[test]
    fn test_disabled_via_config() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("ACTUAL_NO_TELEMETRY");
        let config = Config {
            telemetry: Some(TelemetryConfig {
                enabled: Some(false),
            }),
            ..Default::default()
        };
        assert!(is_disabled(&config));
    }

    #[test]
    fn test_not_disabled_via_config_enabled_true() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("ACTUAL_NO_TELEMETRY");
        let config = Config {
            telemetry: Some(TelemetryConfig {
                enabled: Some(true),
            }),
            ..Default::default()
        };
        assert!(!is_disabled(&config));
    }

    #[test]
    fn test_env_var_wins_over_config_enabled_true() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
        let config = Config {
            telemetry: Some(TelemetryConfig {
                enabled: Some(true),
            }),
            ..Default::default()
        };
        assert!(is_disabled(&config));
    }
}
