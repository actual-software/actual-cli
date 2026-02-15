use std::path::Path;

use console::style;

use crate::cli::args::{ConfigAction, ConfigArgs};
use crate::config;
use crate::error::ActualError;

pub fn exec(args: &ConfigArgs) -> i32 {
    match run(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{} {}", style("Error:").red().bold(), e);
            e.exit_code()
        }
    }
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

fn run_with_path(args: &ConfigArgs, path: &Path) -> Result<(), ActualError> {
    match &args.action {
        ConfigAction::Path => {
            println!("{}", path.display());
            Ok(())
        }
        ConfigAction::Show => {
            let cfg = config::paths::load_from(path)?;
            // Config contains only Option<String>, Option<bool>, Option<usize>, etc.
            // serde_yaml serialization is infallible for these types, so unwrap is safe.
            let yaml = serde_yaml::to_string(&cfg).unwrap();
            print!("{yaml}");
            Ok(())
        }
        ConfigAction::Set(args) => {
            let mut cfg = config::paths::load_from(path)?;
            config::dotpath::set(&mut cfg, &args.key, &args.value)?;
            config::paths::save_to(&cfg, path)?;
            println!("Set {} = {}", args.key, args.value);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::ConfigSetArgs;
    use std::sync::Mutex;
    use tempfile::tempdir;

    /// Mutex to serialize tests that manipulate env vars.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    fn restore_env(key: &str, saved: Option<String>) {
        match saved {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    fn with_temp_config<F: FnOnce()>(f: F) {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");
        let saved = std::env::var("ACTUAL_CONFIG").ok();
        std::env::set_var("ACTUAL_CONFIG", config_file.to_str().unwrap());
        f();
        restore_env("ACTUAL_CONFIG", saved);
    }

    fn with_invalid_config<F: FnOnce()>(f: F) {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = std::env::var("ACTUAL_CONFIG").ok();
        // Point to a path inside /dev/null which cannot be a directory
        std::env::set_var("ACTUAL_CONFIG", "/dev/null/impossible.yaml");
        f();
        restore_env("ACTUAL_CONFIG", saved);
    }

    #[test]
    fn test_exec_path_returns_zero() {
        with_temp_config(|| {
            let args = ConfigArgs {
                action: ConfigAction::Path,
            };
            assert_eq!(exec(&args), 0);
        });
    }

    #[test]
    fn test_exec_show_returns_zero() {
        with_temp_config(|| {
            let args = ConfigArgs {
                action: ConfigAction::Show,
            };
            assert_eq!(exec(&args), 0);
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
            assert_eq!(exec(&args), 0);
        });
    }

    #[test]
    fn test_exec_error_show() {
        with_invalid_config(|| {
            let args = ConfigArgs {
                action: ConfigAction::Show,
            };
            let code = exec(&args);
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
            let code = exec(&args);
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
            assert_ne!(exec(&args), 0);
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

    #[test]
    fn test_restore_env_both_branches() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let key = "ACTUAL_TEST_RESTORE_ENV";

        // Exercise Some branch: restore a previously set value
        std::env::set_var(key, "original");
        restore_env(key, Some("restored".to_string()));
        assert_eq!(std::env::var(key).unwrap(), "restored");

        // Exercise None branch: remove the variable
        restore_env(key, None);
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
