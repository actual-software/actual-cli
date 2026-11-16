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

/// What a sender may do with its events this run.
#[derive(Debug)]
pub(crate) enum Gate {
    /// Telemetry is on: send under this config.
    Send(Config),
    /// Telemetry is off and debug mode is on: build the events under this
    /// config and log them as not sent, and spool, send or store nothing.
    DebugOnly(Config),
    /// Send nothing and build nothing.
    Off,
}

/// [`load_if_enabled`]'s answer, with debug mode's exception: when telemetry
/// is off and `ACTUAL_TELEMETRY_DEBUG` is on, [`Gate::DebugOnly`]. A config
/// file that won't load is still [`Gate::Off`], and in debug mode `stream`'s
/// sender logs a `not_sent` line saying so.
pub(crate) fn gate(stream: crate::telemetry::debug::Stream) -> Gate {
    let gate = quiet_gate();
    if matches!(gate, Gate::Off)
        && crate::telemetry::debug::enabled()
        && crate::config::paths::load().is_err()
    {
        crate::telemetry::debug::config_unreadable(stream);
    }
    gate
}

/// [`gate`] without the `config_unreadable` line, for a step that runs
/// within a sender whose own [`gate`] logs it.
pub(crate) fn quiet_gate() -> Gate {
    match crate::config::paths::load() {
        Ok(config) if !is_disabled(&config) => Gate::Send(config),
        Ok(config) if crate::telemetry::debug::enabled() => Gate::DebugOnly(config),
        _ => Gate::Off,
    }
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

    /// [`gate`]'s answer with `contents` in the config file, `no_telemetry`
    /// as `ACTUAL_NO_TELEMETRY` and `debug` as `ACTUAL_TELEMETRY_DEBUG`, and
    /// the debug log's lines.
    fn gate_with(
        contents: &str,
        no_telemetry: Option<&str>,
        debug: bool,
    ) -> (Gate, Vec<serde_json::Value>) {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _guards = crate::telemetry::debug::tests::debug_on(home.path(), "gate");
        let _debug = (!debug).then(|| EnvGuard::remove(crate::telemetry::debug::ENV));
        let _env = match no_telemetry {
            Some(value) => EnvGuard::set("ACTUAL_NO_TELEMETRY", value),
            None => EnvGuard::remove("ACTUAL_NO_TELEMETRY"),
        };
        std::fs::write(home.path().join("config.yaml"), contents).unwrap();
        let gate = gate(crate::telemetry::debug::Stream::Otlp);
        (gate, crate::telemetry::debug::tests::logged("gate"))
    }

    #[test]
    fn test_gate_sends_when_telemetry_is_on_in_either_mode() {
        for debug in [false, true] {
            let (gate, lines) = gate_with(LOADABLE_TELEMETRY_ON, None, debug);
            assert!(matches!(gate, Gate::Send(c) if c.batch_size == Some(10)));
            assert!(lines.is_empty());
        }
    }

    #[test]
    fn test_gate_is_debug_only_when_telemetry_is_off_in_debug_mode() {
        let opted_out = "telemetry:\n  enabled: false\nbatch_size: 10\n";
        assert!(matches!(gate_with(opted_out, None, false).0, Gate::Off));
        assert!(matches!(
            gate_with(opted_out, None, true).0,
            Gate::DebugOnly(c) if c.batch_size == Some(10)
        ));
        assert!(matches!(
            gate_with(LOADABLE_TELEMETRY_ON, Some("1"), true).0,
            Gate::DebugOnly(_)
        ));
    }

    /// An unloadable config still sends nothing, debug mode or not, and debug
    /// mode logs one line with no event.
    #[test]
    fn test_gate_is_off_for_an_unloadable_config_and_debug_mode_says_why() {
        let (gate, lines) = gate_with(UNLOADABLE_TELEMETRY_OPT_OUT, None, false);
        assert!(matches!(gate, Gate::Off));
        assert!(lines.is_empty());

        let (gate, lines) = gate_with(UNLOADABLE_TELEMETRY_OPT_OUT, None, true);
        assert!(matches!(gate, Gate::Off));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["stage"], "not_sent");
        assert_eq!(lines[0]["reason"], "config_unreadable");
        assert!(lines[0].get("event").is_none());
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
