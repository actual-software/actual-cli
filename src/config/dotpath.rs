use crate::config::types::{Config, TelemetryConfig};
use crate::error::ActualError;

/// Get a config value by dotpath, returning its string representation.
///
/// Returns an error if the path is unknown or the value is not set.
pub fn get(config: &Config, path: &str) -> Result<String, ActualError> {
    match path {
        "api_url" => config
            .api_url
            .clone()
            .ok_or_else(|| ActualError::ConfigError(format!("config key not set: {path}"))),
        "model" => config
            .model
            .clone()
            .ok_or_else(|| ActualError::ConfigError(format!("config key not set: {path}"))),
        "batch_size" => config
            .batch_size
            .map(|v| v.to_string())
            .ok_or_else(|| ActualError::ConfigError(format!("config key not set: {path}"))),
        "concurrency" => config
            .concurrency
            .map(|v| v.to_string())
            .ok_or_else(|| ActualError::ConfigError(format!("config key not set: {path}"))),
        "max_budget_usd" => config
            .max_budget_usd
            .map(|v| v.to_string())
            .ok_or_else(|| ActualError::ConfigError(format!("config key not set: {path}"))),
        "include_general" => config
            .include_general
            .map(|v| v.to_string())
            .ok_or_else(|| ActualError::ConfigError(format!("config key not set: {path}"))),
        "max_per_framework" => config
            .max_per_framework
            .map(|v| v.to_string())
            .ok_or_else(|| ActualError::ConfigError(format!("config key not set: {path}"))),
        "include_categories" => config
            .include_categories
            .as_ref()
            .map(|v| v.join(","))
            .ok_or_else(|| ActualError::ConfigError(format!("config key not set: {path}"))),
        "exclude_categories" => config
            .exclude_categories
            .as_ref()
            .map(|v| v.join(","))
            .ok_or_else(|| ActualError::ConfigError(format!("config key not set: {path}"))),
        "telemetry.enabled" => config
            .telemetry
            .as_ref()
            .and_then(|t| t.enabled)
            .map(|v| v.to_string())
            .ok_or_else(|| ActualError::ConfigError(format!("config key not set: {path}"))),
        "rejected_adrs" | "cached_analysis" => Err(ActualError::ConfigError(format!(
            "complex config key, use config file directly: {path}"
        ))),
        _ => Err(ActualError::ConfigError(format!(
            "unknown config key: {path}"
        ))),
    }
}

/// Set a config value by dotpath, parsing the string value to the correct type.
///
/// Returns an error if the path is unknown or the value cannot be parsed.
pub fn set(config: &mut Config, path: &str, value: &str) -> Result<(), ActualError> {
    match path {
        "api_url" => {
            config.api_url = Some(value.to_string());
        }
        "model" => {
            config.model = Some(value.to_string());
        }
        "batch_size" => {
            let v = value.parse::<usize>().map_err(|_| {
                ActualError::ConfigError(format!(
                    "invalid value for {path}: expected usize, got \"{value}\""
                ))
            })?;
            if v < 1 {
                return Err(ActualError::ConfigError(format!(
                    "{path} must be at least 1, got {v}"
                )));
            }
            config.batch_size = Some(v);
        }
        "concurrency" => {
            let v = value.parse::<usize>().map_err(|_| {
                ActualError::ConfigError(format!(
                    "invalid value for {path}: expected usize, got \"{value}\""
                ))
            })?;
            if v < 1 {
                return Err(ActualError::ConfigError(format!(
                    "{path} must be at least 1, got {v}"
                )));
            }
            config.concurrency = Some(v);
        }
        "max_budget_usd" => {
            let v = value.parse::<f64>().map_err(|_| {
                ActualError::ConfigError(format!(
                    "invalid value for {path}: expected f64, got \"{value}\""
                ))
            })?;
            if !v.is_finite() || v <= 0.0 {
                return Err(ActualError::ConfigError(format!(
                    "{path} must be a positive finite number, got {v}"
                )));
            }
            config.max_budget_usd = Some(v);
        }
        "include_general" => {
            config.include_general = Some(value.parse::<bool>().map_err(|_| {
                ActualError::ConfigError(format!(
                    "invalid value for {path}: expected bool, got \"{value}\""
                ))
            })?);
        }
        "max_per_framework" => {
            let v = value.parse::<u32>().map_err(|_| {
                ActualError::ConfigError(format!(
                    "invalid value for {path}: expected u32, got \"{value}\""
                ))
            })?;
            if v < 1 {
                return Err(ActualError::ConfigError(format!(
                    "{path} must be at least 1, got {v}"
                )));
            }
            config.max_per_framework = Some(v);
        }
        "include_categories" => {
            let cats: Vec<String> = value
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            config.include_categories = if cats.is_empty() { None } else { Some(cats) };
        }
        "exclude_categories" => {
            let cats: Vec<String> = value
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            config.exclude_categories = if cats.is_empty() { None } else { Some(cats) };
        }
        "telemetry.enabled" => {
            let enabled = value.parse::<bool>().map_err(|_| {
                ActualError::ConfigError(format!(
                    "invalid value for {path}: expected bool, got \"{value}\""
                ))
            })?;
            match config.telemetry.as_mut() {
                Some(t) => t.enabled = Some(enabled),
                None => {
                    config.telemetry = Some(TelemetryConfig {
                        enabled: Some(enabled),
                    });
                }
            }
        }
        "rejected_adrs" | "cached_analysis" => {
            return Err(ActualError::ConfigError(format!(
                "complex config key, use config file directly: {path}"
            )));
        }
        _ => {
            return Err(ActualError::ConfigError(format!(
                "unknown config key: {path}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_and_get_batch_size() {
        let mut config = Config::default();
        set(&mut config, "batch_size", "20").unwrap();
        assert_eq!(get(&config, "batch_size").unwrap(), "20");
    }

    #[test]
    fn test_set_telemetry_enabled() {
        let mut config = Config::default();
        set(&mut config, "telemetry.enabled", "false").unwrap();
        assert_eq!(config.telemetry.unwrap().enabled, Some(false));
    }

    #[test]
    fn test_set_api_url() {
        let mut config = Config::default();
        set(&mut config, "api_url", "https://custom.api.com").unwrap();
        assert_eq!(get(&config, "api_url").unwrap(), "https://custom.api.com");
        assert_eq!(config.api_url, Some("https://custom.api.com".to_string()));
    }

    #[test]
    fn test_set_max_budget_usd() {
        let mut config = Config::default();
        set(&mut config, "max_budget_usd", "0.50").unwrap();
        assert_eq!(config.max_budget_usd, Some(0.50));
    }

    #[test]
    fn test_get_unknown_path() {
        let config = Config::default();
        let err = get(&config, "nonexistent.path").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown config key"), "got: {msg}");
    }

    #[test]
    fn test_set_invalid_type() {
        let mut config = Config::default();
        let err = set(&mut config, "batch_size", "abc").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid value"), "got: {msg}");
    }

    #[test]
    fn test_get_unset_value() {
        let config = Config::default();
        let err = get(&config, "api_url").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not set"), "got: {msg}");
    }

    #[test]
    fn test_set_and_get_model() {
        let mut config = Config::default();
        set(&mut config, "model", "sonnet").unwrap();
        assert_eq!(get(&config, "model").unwrap(), "sonnet");
    }

    #[test]
    fn test_set_and_get_concurrency() {
        let mut config = Config::default();
        set(&mut config, "concurrency", "8").unwrap();
        assert_eq!(get(&config, "concurrency").unwrap(), "8");
    }

    #[test]
    fn test_set_and_get_include_general() {
        let mut config = Config::default();
        set(&mut config, "include_general", "true").unwrap();
        assert_eq!(get(&config, "include_general").unwrap(), "true");
    }

    #[test]
    fn test_set_and_get_include_categories() {
        let mut config = Config::default();
        set(&mut config, "include_categories", "testing,security").unwrap();
        assert_eq!(
            get(&config, "include_categories").unwrap(),
            "testing,security"
        );
    }

    #[test]
    fn test_set_and_get_exclude_categories() {
        let mut config = Config::default();
        set(&mut config, "exclude_categories", "deprecated,legacy").unwrap();
        assert_eq!(
            get(&config, "exclude_categories").unwrap(),
            "deprecated,legacy"
        );
    }

    #[test]
    fn test_set_complex_key_rejected_adrs() {
        let mut config = Config::default();
        let err = set(&mut config, "rejected_adrs", "value").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("complex config key"), "got: {msg}");
    }

    #[test]
    fn test_set_complex_key_cached_analysis() {
        let mut config = Config::default();
        let err = set(&mut config, "cached_analysis", "value").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("complex config key"), "got: {msg}");
    }

    #[test]
    fn test_set_invalid_bool() {
        let mut config = Config::default();
        let err = set(&mut config, "include_general", "notabool").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid value"), "got: {msg}");
    }

    #[test]
    fn test_set_invalid_f64() {
        let mut config = Config::default();
        let err = set(&mut config, "max_budget_usd", "notanumber").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid value"), "got: {msg}");
    }

    #[test]
    fn test_telemetry_creates_parent() {
        let mut config = Config::default();
        assert!(config.telemetry.is_none());
        set(&mut config, "telemetry.enabled", "true").unwrap();
        assert!(config.telemetry.is_some());
        assert_eq!(config.telemetry.unwrap().enabled, Some(true));
    }

    #[test]
    fn test_get_telemetry_enabled() {
        let mut config = Config::default();
        config.telemetry = Some(TelemetryConfig {
            enabled: Some(true),
        });
        assert_eq!(get(&config, "telemetry.enabled").unwrap(), "true");
    }

    #[test]
    fn test_set_unknown_path() {
        let mut config = Config::default();
        let err = set(&mut config, "nonexistent", "value").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown config key"), "got: {msg}");
    }

    #[test]
    fn test_get_complex_key_rejected_adrs() {
        let config = Config::default();
        let err = get(&config, "rejected_adrs").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("complex config key"), "got: {msg}");
    }

    #[test]
    fn test_get_complex_key_cached_analysis() {
        let config = Config::default();
        let err = get(&config, "cached_analysis").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("complex config key"), "got: {msg}");
    }

    #[test]
    fn test_set_telemetry_enabled_updates_existing() {
        let mut config = Config::default();
        config.telemetry = Some(TelemetryConfig {
            enabled: Some(true),
        });
        set(&mut config, "telemetry.enabled", "false").unwrap();
        assert_eq!(config.telemetry.unwrap().enabled, Some(false));
    }

    #[test]
    fn test_get_max_budget_usd() {
        let mut config = Config::default();
        config.max_budget_usd = Some(1.25);
        assert_eq!(get(&config, "max_budget_usd").unwrap(), "1.25");
    }

    #[test]
    fn test_set_invalid_concurrency() {
        let mut config = Config::default();
        let err = set(&mut config, "concurrency", "xyz").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid value"), "got: {msg}");
    }

    #[test]
    fn test_set_invalid_telemetry_enabled() {
        let mut config = Config::default();
        let err = set(&mut config, "telemetry.enabled", "notbool").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid value"), "got: {msg}");
    }

    #[test]
    fn test_get_unset_telemetry_enabled() {
        let config = Config::default();
        let err = get(&config, "telemetry.enabled").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not set"), "got: {msg}");
    }

    #[test]
    fn test_get_telemetry_enabled_none_in_struct() {
        let mut config = Config::default();
        config.telemetry = Some(TelemetryConfig { enabled: None });
        let err = get(&config, "telemetry.enabled").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not set"), "got: {msg}");
    }

    #[test]
    fn test_get_unset_model() {
        let config = Config::default();
        let err = get(&config, "model").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not set"), "got: {msg}");
    }

    #[test]
    fn test_get_unset_batch_size() {
        let config = Config::default();
        let err = get(&config, "batch_size").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not set"), "got: {msg}");
    }

    #[test]
    fn test_get_unset_concurrency() {
        let config = Config::default();
        let err = get(&config, "concurrency").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not set"), "got: {msg}");
    }

    #[test]
    fn test_get_unset_max_budget_usd() {
        let config = Config::default();
        let err = get(&config, "max_budget_usd").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not set"), "got: {msg}");
    }

    #[test]
    fn test_get_unset_include_general() {
        let config = Config::default();
        let err = get(&config, "include_general").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not set"), "got: {msg}");
    }

    #[test]
    fn test_set_and_get_max_per_framework() {
        let mut config = Config::default();
        set(&mut config, "max_per_framework", "10").unwrap();
        assert_eq!(get(&config, "max_per_framework").unwrap(), "10");
    }

    #[test]
    fn test_get_unset_max_per_framework() {
        let config = Config::default();
        let err = get(&config, "max_per_framework").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not set"), "got: {msg}");
    }

    #[test]
    fn test_set_invalid_max_per_framework() {
        let mut config = Config::default();
        let err = set(&mut config, "max_per_framework", "abc").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid value"), "got: {msg}");
    }

    #[test]
    fn test_get_unset_include_categories() {
        let config = Config::default();
        let err = get(&config, "include_categories").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not set"), "got: {msg}");
    }

    #[test]
    fn test_get_unset_exclude_categories() {
        let config = Config::default();
        let err = get(&config, "exclude_categories").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not set"), "got: {msg}");
    }

    // --- Semantic validation tests ---

    #[test]
    fn test_set_batch_size_zero_rejected() {
        let mut config = Config::default();
        let err = set(&mut config, "batch_size", "0").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("must be at least 1"), "got: {msg}");
    }

    #[test]
    fn test_set_batch_size_one_accepted() {
        let mut config = Config::default();
        set(&mut config, "batch_size", "1").unwrap();
        assert_eq!(config.batch_size, Some(1));
    }

    #[test]
    fn test_set_concurrency_zero_rejected() {
        let mut config = Config::default();
        let err = set(&mut config, "concurrency", "0").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("must be at least 1"), "got: {msg}");
    }

    #[test]
    fn test_set_concurrency_one_accepted() {
        let mut config = Config::default();
        set(&mut config, "concurrency", "1").unwrap();
        assert_eq!(config.concurrency, Some(1));
    }

    #[test]
    fn test_set_max_per_framework_zero_rejected() {
        let mut config = Config::default();
        let err = set(&mut config, "max_per_framework", "0").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("must be at least 1"), "got: {msg}");
    }

    #[test]
    fn test_set_max_per_framework_one_accepted() {
        let mut config = Config::default();
        set(&mut config, "max_per_framework", "1").unwrap();
        assert_eq!(config.max_per_framework, Some(1));
    }

    #[test]
    fn test_set_max_budget_usd_negative_rejected() {
        let mut config = Config::default();
        let err = set(&mut config, "max_budget_usd", "-5.0").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("must be a positive finite number"),
            "got: {msg}"
        );
    }

    #[test]
    fn test_set_max_budget_usd_zero_rejected() {
        let mut config = Config::default();
        let err = set(&mut config, "max_budget_usd", "0").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("must be a positive finite number"),
            "got: {msg}"
        );
    }

    #[test]
    fn test_set_max_budget_usd_infinity_rejected() {
        let mut config = Config::default();
        let err = set(&mut config, "max_budget_usd", "inf").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("must be a positive finite number"),
            "got: {msg}"
        );
    }

    #[test]
    fn test_set_max_budget_usd_small_positive_accepted() {
        let mut config = Config::default();
        set(&mut config, "max_budget_usd", "0.01").unwrap();
        assert_eq!(config.max_budget_usd, Some(0.01));
    }

    // --- Category empty string tests ---

    #[test]
    fn test_set_include_categories_empty_string_clears() {
        let mut config = Config::default();
        set(&mut config, "include_categories", "testing,security").unwrap();
        assert!(config.include_categories.is_some());
        set(&mut config, "include_categories", "").unwrap();
        assert!(config.include_categories.is_none());
        let err = get(&config, "include_categories").unwrap_err();
        assert!(err.to_string().contains("not set"));
    }

    #[test]
    fn test_set_exclude_categories_empty_string_clears() {
        let mut config = Config::default();
        set(&mut config, "exclude_categories", "deprecated,legacy").unwrap();
        assert!(config.exclude_categories.is_some());
        set(&mut config, "exclude_categories", "").unwrap();
        assert!(config.exclude_categories.is_none());
        let err = get(&config, "exclude_categories").unwrap_err();
        assert!(err.to_string().contains("not set"));
    }

    #[test]
    fn test_set_include_categories_whitespace_only_clears() {
        let mut config = Config::default();
        set(&mut config, "include_categories", " , , ").unwrap();
        assert!(config.include_categories.is_none());
    }

    #[test]
    fn test_set_include_categories_double_commas_filtered() {
        let mut config = Config::default();
        set(&mut config, "include_categories", "testing,,security").unwrap();
        assert_eq!(
            config.include_categories,
            Some(vec!["testing".to_string(), "security".to_string()])
        );
    }

    #[test]
    fn test_set_exclude_categories_whitespace_only_clears() {
        let mut config = Config::default();
        set(&mut config, "exclude_categories", " , , ").unwrap();
        assert!(config.exclude_categories.is_none());
    }
}
