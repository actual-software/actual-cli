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
    match &args.action {
        ConfigAction::Path => {
            let path = config::paths::config_path()?;
            println!("{}", path.display());
            Ok(())
        }
        ConfigAction::Show => {
            let cfg = config::paths::load()?;
            let yaml = serde_yaml::to_string(&cfg).map_err(|e| {
                ActualError::ConfigError(format!("failed to serialize config: {e}"))
            })?;
            print!("{yaml}");
            Ok(())
        }
        ConfigAction::Set(args) => {
            let mut cfg = config::paths::load()?;
            config::dotpath::set(&mut cfg, &args.key, &args.value)?;
            config::paths::save(&cfg)?;
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

    fn with_temp_config<F: FnOnce()>(f: F) {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");
        let saved = std::env::var("ACTUAL_CONFIG").ok();
        std::env::set_var("ACTUAL_CONFIG", config_file.to_str().unwrap());
        f();
        match saved {
            Some(v) => std::env::set_var("ACTUAL_CONFIG", v),
            None => std::env::remove_var("ACTUAL_CONFIG"),
        }
    }

    fn with_invalid_config<F: FnOnce()>(f: F) {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = std::env::var("ACTUAL_CONFIG").ok();
        // Point to a path inside /dev/null which cannot be a directory
        std::env::set_var("ACTUAL_CONFIG", "/dev/null/impossible.yaml");
        f();
        match saved {
            Some(v) => std::env::set_var("ACTUAL_CONFIG", v),
            None => std::env::remove_var("ACTUAL_CONFIG"),
        }
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
}
