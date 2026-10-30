use crate::config::types::Config;

/// True when telemetry is disabled by either opt-out mechanism: the
/// `ACTUAL_NO_TELEMETRY` env var (any non-empty value) or
/// `telemetry.enabled: false` in config.
///
/// Extracted from `reporter::try_report_metrics` so every telemetry sender
/// (sync counters, plan-governance events) shares exactly one opt-out check
/// — see `PRIVACY.md`'s "How to opt out of telemetry."
/// (A build without the `telemetry` feature isn't representable here, since
/// it removes this module entirely.)
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

/// The config a telemetry sender may send under, or `None` when it must send
/// nothing: the config file couldn't be read or parsed, or [`is_disabled`]
/// finds telemetry off. A file that fails to load may be the one holding
/// `telemetry.enabled: false`, so it counts as opted out instead of falling
/// back to [`Config::default`], which has telemetry on.
///
/// Every sender and the uploader take their config from here, so none can
/// load it one way and check the opt-out another.
pub fn load_if_enabled() -> Option<Config> {
    crate::config::paths::load()
        .ok()
        .filter(|config| !is_disabled(config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::TelemetryConfig;
    use crate::testutil::{
        EnvGuard, ENV_MUTEX, LOADABLE_TELEMETRY_ON, UNLOADABLE_TELEMETRY_OPT_OUT,
    };

    /// [`load_if_enabled`]'s answer with `contents` in the config file and
    /// `ACTUAL_NO_TELEMETRY` set to `no_telemetry`, or unset.
    fn load_if_enabled_with(contents: &str, no_telemetry: Option<&str>) -> Option<Config> {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _file = EnvGuard::remove("ACTUAL_CONFIG");
        let _env = match no_telemetry {
            Some(value) => EnvGuard::set("ACTUAL_NO_TELEMETRY", value),
            None => EnvGuard::remove("ACTUAL_NO_TELEMETRY"),
        };
        std::fs::write(home.path().join("config.yaml"), contents).unwrap();
        load_if_enabled()
    }

    #[test]
    fn test_load_if_enabled_returns_the_config_it_loaded() {
        let config = load_if_enabled_with(LOADABLE_TELEMETRY_ON, None).expect("telemetry is on");
        assert_eq!(config.batch_size, Some(10));
    }

    /// The opt-out in a file that doesn't parse counts, and so does a file
    /// with no opt-out in it: the CLI can't tell what it would have said.
    #[test]
    fn test_load_if_enabled_is_none_when_the_config_cannot_parse() {
        assert!(load_if_enabled_with(UNLOADABLE_TELEMETRY_OPT_OUT, None).is_none());
        assert!(load_if_enabled_with("batch_size: lots\n", None).is_none());
    }

    #[test]
    fn test_load_if_enabled_is_none_when_the_config_cannot_be_read() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _file = EnvGuard::remove("ACTUAL_CONFIG");
        let _env = EnvGuard::remove("ACTUAL_NO_TELEMETRY");
        // A directory where the file belongs: reading it fails, and it isn't
        // missing, so nothing replaces it with a default.
        std::fs::create_dir(home.path().join("config.yaml")).unwrap();
        assert!(load_if_enabled().is_none());
    }

    #[test]
    fn test_load_if_enabled_is_none_when_opted_out() {
        let opted_out = "telemetry:\n  enabled: false\nbatch_size: 10\n";
        assert!(load_if_enabled_with(opted_out, None).is_none());
        assert!(load_if_enabled_with(LOADABLE_TELEMETRY_ON, Some("1")).is_none());
    }

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
                ..Default::default()
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
                ..Default::default()
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
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(is_disabled(&config));
    }
}
