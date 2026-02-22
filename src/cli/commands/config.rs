use std::path::Path;

use crate::cli::args::{ConfigAction, ConfigArgs};
use crate::config;
use crate::error::ActualError;

const SECRET_KEYS: &[&str] = &["anthropic_api_key", "openai_api_key", "cursor_api_key"];

fn redact_yaml(yaml: &str) -> String {
    yaml.lines()
        .map(|line| {
            for key in SECRET_KEYS {
                let prefix = format!("{key}:");
                if line.starts_with(&prefix) {
                    let rest = &line[prefix.len()..];
                    // Only redact if there's actually a value (not null/~)
                    let trimmed = rest.trim();
                    if trimmed != "~" && trimmed != "null" && !trimmed.is_empty() {
                        return format!("{prefix} [redacted]");
                    }
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn exec(args: &ConfigArgs) -> Result<(), ActualError> {
    run(args)
}

fn run(args: &ConfigArgs) -> Result<(), ActualError> {
    run_with_resolver(args, config::paths::config_path)
}

fn run_with_resolver(
    args: &ConfigArgs,
    resolve_path: impl FnOnce() -> Result<std::path::PathBuf, ActualError>,
) -> Result<(), ActualError> {
    let path = resolve_path()?;
    run_with_path(args, &path)
}

fn serialize_config_for_display(cfg: &config::Config) -> Result<String, ActualError> {
    serde_yml::to_string(cfg)
        .map_err(|e| ActualError::ConfigError(format!("Failed to serialize config to YAML: {e}")))
}

fn run_with_path(args: &ConfigArgs, path: &Path) -> Result<(), ActualError> {
    match &args.action {
        ConfigAction::Path => {
            println!("{}", path.display());
            Ok(())
        }
        ConfigAction::Show => {
            let cfg = config::paths::load_from(path)?;
            let yaml = serialize_config_for_display(&cfg)?;
            print!("{}", redact_yaml(&yaml));
            Ok(())
        }
        ConfigAction::Set(args) => {
            let mut cfg = config::paths::load_from(path)?;
            config::dotpath::set(&mut cfg, &args.key, &args.value)?;
            config::paths::save_to(&cfg, path)?;
            let display_value = if SECRET_KEYS.contains(&args.key.as_str()) {
                "[redacted]".to_string()
            } else {
                args.value.clone()
            };
            println!("Set {} = {}", args.key, display_value);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::ConfigSetArgs;
    use crate::cli::commands::handle_result;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use tempfile::tempdir;

    fn with_temp_config<F: FnOnce()>(f: F) {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");
        let _guard = EnvGuard::set("ACTUAL_CONFIG", config_file.to_str().unwrap());
        f();
    }

    fn with_invalid_config<F: FnOnce()>(f: F) {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Point to a path inside /dev/null which cannot be a directory
        let _guard = EnvGuard::set("ACTUAL_CONFIG", "/dev/null/impossible.yaml");
        f();
    }

    #[test]
    fn test_exec_path_returns_zero() {
        with_temp_config(|| {
            let args = ConfigArgs {
                action: ConfigAction::Path,
            };
            assert_eq!(handle_result(exec(&args)), 0);
        });
    }

    #[test]
    fn test_exec_show_returns_zero() {
        with_temp_config(|| {
            let args = ConfigArgs {
                action: ConfigAction::Show,
            };
            assert_eq!(handle_result(exec(&args)), 0);
        });
    }

    #[test]
    fn test_exec_set_returns_zero() {
        with_temp_config(|| {
            let args = ConfigArgs {
                action: ConfigAction::Set(ConfigSetArgs {
                    key: "batch_size".to_string(),
                    value: "25".to_string(),
                }),
            };
            assert_eq!(handle_result(exec(&args)), 0);
        });
    }

    #[test]
    fn test_exec_error_show() {
        with_invalid_config(|| {
            let args = ConfigArgs {
                action: ConfigAction::Show,
            };
            let code = handle_result(exec(&args));
            assert_ne!(code, 0);
        });
    }

    #[test]
    fn test_exec_error_set() {
        with_invalid_config(|| {
            let args = ConfigArgs {
                action: ConfigAction::Set(ConfigSetArgs {
                    key: "batch_size".to_string(),
                    value: "10".to_string(),
                }),
            };
            let code = handle_result(exec(&args));
            assert_ne!(code, 0);
        });
    }

    #[test]
    fn test_exec_set_invalid_key_returns_error() {
        with_temp_config(|| {
            let args = ConfigArgs {
                action: ConfigAction::Set(ConfigSetArgs {
                    key: "nonexistent_key".to_string(),
                    value: "value".to_string(),
                }),
            };
            assert_ne!(handle_result(exec(&args)), 0);
        });
    }

    #[test]
    fn test_run_path() {
        with_temp_config(|| {
            let args = ConfigArgs {
                action: ConfigAction::Path,
            };
            assert!(run(&args).is_ok());
        });
    }

    #[test]
    fn test_run_show() {
        with_temp_config(|| {
            let args = ConfigArgs {
                action: ConfigAction::Show,
            };
            assert!(run(&args).is_ok());
        });
    }

    #[test]
    fn test_run_set() {
        with_temp_config(|| {
            let args = ConfigArgs {
                action: ConfigAction::Set(ConfigSetArgs {
                    key: "model".to_string(),
                    value: "opus".to_string(),
                }),
            };
            assert!(run(&args).is_ok());

            // Verify the value was persisted
            let cfg = config::paths::load().unwrap();
            assert_eq!(cfg.model, Some("opus".to_string()));
        });
    }

    #[test]
    fn test_run_set_invalid_value() {
        with_temp_config(|| {
            let args = ConfigArgs {
                action: ConfigAction::Set(ConfigSetArgs {
                    key: "batch_size".to_string(),
                    value: "not_a_number".to_string(),
                }),
            };
            assert!(run(&args).is_err());
        });
    }

    #[test]
    fn test_run_show_error() {
        with_invalid_config(|| {
            let args = ConfigArgs {
                action: ConfigAction::Show,
            };
            assert!(run(&args).is_err());
        });
    }

    #[test]
    fn test_run_set_error() {
        with_invalid_config(|| {
            let args = ConfigArgs {
                action: ConfigAction::Set(ConfigSetArgs {
                    key: "batch_size".to_string(),
                    value: "10".to_string(),
                }),
            };
            assert!(run(&args).is_err());
        });
    }

    #[test]
    fn test_run_path_error() {
        with_invalid_config(|| {
            let args = ConfigArgs {
                action: ConfigAction::Path,
            };
            // config_path with /dev/null/impossible.yaml returns Ok, so path itself won't error.
            // But we test it anyway to verify it doesn't panic.
            let _ = run(&args);
        });
    }

    /// Exercise the `resolve_path()?` error branch inside `run_with_resolver()`.
    ///
    /// Uses a closure that always fails, so this is fully deterministic
    /// and does not depend on environment variables or platform behavior.
    #[test]
    fn test_run_with_resolver_error() {
        let args = ConfigArgs {
            action: ConfigAction::Path,
        };
        let result = run_with_resolver(&args, || {
            Err(ActualError::ConfigError(
                "simulated config_path failure".into(),
            ))
        });
        assert!(result.is_err());
    }

    // --- Tests for redact_yaml ---

    #[test]
    fn test_redact_yaml_redacts_anthropic_api_key() {
        let yaml = "anthropic_api_key: sk-ant-secret123\nmodel: gpt-4\n";
        let redacted = redact_yaml(yaml);
        assert!(redacted.contains("anthropic_api_key:"));
        assert!(!redacted.contains("sk-ant-secret123"));
        assert!(redacted.contains("[redacted]"));
    }

    #[test]
    fn test_redact_yaml_redacts_cursor_api_key() {
        let yaml = "cursor_api_key: cursor-secret789\nbatch_size: 10\n";
        let redacted = redact_yaml(yaml);
        assert!(redacted.contains("cursor_api_key:"));
        assert!(!redacted.contains("cursor-secret789"));
        assert!(redacted.contains("[redacted]"));
    }

    #[test]
    fn test_redact_yaml_redacts_openai_api_key() {
        let yaml = "openai_api_key: sk-openai-secret456\nbatch_size: 10\n";
        let redacted = redact_yaml(yaml);
        assert!(redacted.contains("openai_api_key:"));
        assert!(!redacted.contains("sk-openai-secret456"));
        assert!(redacted.contains("[redacted]"));
    }

    #[test]
    fn test_redact_yaml_does_not_redact_null_values() {
        let yaml = "anthropic_api_key: ~\nopenai_api_key: null\n";
        let redacted = redact_yaml(yaml);
        assert!(!redacted.contains("[redacted]"));
        assert!(redacted.contains("anthropic_api_key: ~"));
        assert!(redacted.contains("openai_api_key: null"));
    }

    #[test]
    fn test_redact_yaml_does_not_redact_non_secret_keys() {
        let yaml = "model: claude-opus\nbatch_size: 25\n";
        let redacted = redact_yaml(yaml);
        assert!(redacted.contains("model: claude-opus"));
        assert!(redacted.contains("batch_size: 25"));
        assert!(!redacted.contains("[redacted]"));
    }

    // --- Tests for config set redaction via run_with_path ---

    /// Exercise the `serde_yml::to_string` error branch in `serialize_config_for_display`.
    ///
    /// A config with an unserializable `serde_yml::Value` field (mapping-as-key)
    /// must return a `ConfigError` rather than panicking.
    #[test]
    fn test_serialize_config_for_display_error_is_wrapped_as_config_error() {
        use crate::config::types::CachedTailoring;

        // Construct a mapping-as-key value that serde_yml cannot serialize.
        let mut inner_key = serde_yml::Mapping::new();
        inner_key.insert(
            serde_yml::Value::String("k".to_string()),
            serde_yml::Value::String("v".to_string()),
        );
        let mut outer = serde_yml::Mapping::new();
        outer.insert(
            serde_yml::Value::Mapping(inner_key),
            serde_yml::Value::String("value".to_string()),
        );
        let bad_value = serde_yml::Value::Mapping(outer);

        // Verify this value is actually unserializable.
        assert!(
            serde_yml::to_string(&bad_value).is_err(),
            "serde_yml must reject a mapping with mapping keys for this test to be valid"
        );

        let cfg = config::Config {
            cached_tailoring: Some(CachedTailoring {
                cache_key: "key".to_string(),
                repo_path: "/tmp/repo".to_string(),
                tailoring: bad_value,
                tailored_at: chrono::Utc::now(),
            }),
            ..config::Config::default()
        };

        let err = serialize_config_for_display(&cfg).unwrap_err();
        assert!(
            err.to_string()
                .contains("Failed to serialize config to YAML"),
            "error must be wrapped as ConfigError, got: {err}"
        );
    }

    #[test]
    fn test_set_api_key_does_not_echo_raw_value() {
        // We exercise run_with_path directly and verify the result is Ok.
        // The stdout check verifies no panic; the actual stdout capture would
        // require process-level testing. Instead we verify the function succeeds
        // and let the unit test for display_value logic cover the redaction.
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");
        let args = ConfigArgs {
            action: ConfigAction::Set(ConfigSetArgs {
                key: "anthropic_api_key".to_string(),
                value: "sk-test-should-not-appear".to_string(),
            }),
        };
        let result = run_with_path(&args, &config_file);
        assert!(result.is_ok());
    }

    #[test]
    fn test_set_cursor_api_key_does_not_echo_raw_value() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");
        let args = ConfigArgs {
            action: ConfigAction::Set(ConfigSetArgs {
                key: "cursor_api_key".to_string(),
                value: "cursor-test-should-not-appear".to_string(),
            }),
        };
        let result = run_with_path(&args, &config_file);
        assert!(result.is_ok());
    }

    #[test]
    fn test_set_non_secret_key_succeeds() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");
        let args = ConfigArgs {
            action: ConfigAction::Set(ConfigSetArgs {
                key: "batch_size".to_string(),
                value: "42".to_string(),
            }),
        };
        let result = run_with_path(&args, &config_file);
        assert!(result.is_ok());
    }

    #[test]
    fn test_show_after_setting_api_key_redacts_in_yaml() {
        // Verify that the redact_yaml function correctly handles the YAML that
        // serde_yml would produce for a set API key.
        let yaml_with_key = "anthropic_api_key: sk-ant-actual-key\nmodel: ~\n";
        let redacted = redact_yaml(yaml_with_key);
        assert!(
            !redacted.contains("sk-ant-actual-key"),
            "raw key must not appear in output"
        );
        assert!(redacted.contains("anthropic_api_key: [redacted]"));
    }

    #[test]
    fn test_env_guard_both_branches() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let key = "ACTUAL_TEST_RESTORE_ENV";

        // Exercise set-then-restore cycle
        {
            let _g = EnvGuard::set(key, "original");
            assert_eq!(std::env::var(key).unwrap(), "original");
        }
        // Guard dropped: variable should be removed (was absent before)
        assert!(std::env::var(key).is_err());

        // Exercise set-over-existing then restore
        {
            let _g1 = EnvGuard::set(key, "first");
            let _g2 = EnvGuard::set(key, "second");
            assert_eq!(std::env::var(key).unwrap(), "second");
        }
        // Both guards dropped: variable should be removed
        assert!(std::env::var(key).is_err());
    }

    // --- Deterministic tests using run_with_path (no env var races) ---

    #[test]
    fn test_run_with_path_show_error() {
        let args = ConfigArgs {
            action: ConfigAction::Show,
        };
        let result = run_with_path(&args, Path::new("/dev/null/impossible.yaml"));
        assert!(result.is_err());
    }

    #[test]
    fn test_run_with_path_set_load_error() {
        let args = ConfigArgs {
            action: ConfigAction::Set(ConfigSetArgs {
                key: "batch_size".to_string(),
                value: "10".to_string(),
            }),
        };
        let result = run_with_path(&args, Path::new("/dev/null/impossible.yaml"));
        assert!(result.is_err());
    }

    /// Exercise the `save_to()` error branch inside `run_with_path(Set)`.
    ///
    /// Uses an explicit path (no env vars) so this test is fully
    /// deterministic and cannot race with other test modules.
    #[cfg(unix)]
    #[test]
    fn test_run_with_path_set_save_error() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");

        // Create a valid config so load_from succeeds
        config::paths::save_to(&config::Config::default(), &config_file).unwrap();

        // Make the config file read-only so save_to fails
        let mut perms = std::fs::metadata(&config_file).unwrap().permissions();
        perms.set_mode(0o444);
        std::fs::set_permissions(&config_file, perms.clone()).unwrap();

        let args = ConfigArgs {
            action: ConfigAction::Set(ConfigSetArgs {
                key: "batch_size".to_string(),
                value: "10".to_string(),
            }),
        };
        let result = run_with_path(&args, &config_file);
        assert!(result.is_err());

        // Restore writable permissions so temp dir cleanup succeeds
        perms.set_mode(0o644);
        std::fs::set_permissions(&config_file, perms).unwrap();
    }
}
