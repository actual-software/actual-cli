use crate::cli::args::{CacheAction, CacheArgs};
use crate::config;
use crate::error::ActualError;

pub fn exec(args: &CacheArgs) -> Result<(), ActualError> {
    run_with_resolver(args, config::paths::config_path)
}

fn run_with_resolver(
    args: &CacheArgs,
    resolve_path: impl FnOnce() -> Result<std::path::PathBuf, ActualError>,
) -> Result<(), ActualError> {
    let path = resolve_path()?;
    run_with_path(args, &path)
}

fn run_with_path(args: &CacheArgs, path: &std::path::Path) -> Result<(), ActualError> {
    match &args.action {
        CacheAction::Clear => {
            let mut cfg = config::paths::load_from(path)?;

            let had_analysis = cfg.cached_analysis.take().is_some();
            let had_tailoring = cfg.cached_tailoring.take().is_some();

            config::paths::save_to(&cfg, path)?;

            match (had_analysis, had_tailoring) {
                (false, false) => println!("No cache entries to clear."),
                _ => {
                    if had_analysis {
                        println!("Cleared cached analysis.");
                    }
                    if had_tailoring {
                        println!("Cleared cached tailoring.");
                    }
                }
            }

            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::types::RepoAnalysis;
    use crate::cli::args::{CacheAction, CacheArgs};
    use crate::cli::commands::handle_result;
    use crate::config::types::{CachedAnalysis, CachedTailoring};
    use crate::tailoring::types::{TailoringOutput, TailoringSummary};
    use tempfile::tempdir;

    fn make_args_clear() -> CacheArgs {
        CacheArgs {
            action: CacheAction::Clear,
        }
    }

    #[test]
    fn test_clear_when_no_cache_reports_nothing_to_clear() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");
        config::paths::save_to(&config::Config::default(), &config_file).unwrap();

        let args = make_args_clear();
        let result = run_with_path(&args, &config_file);
        assert!(result.is_ok());

        // Verify config is still intact
        let loaded = config::paths::load_from(&config_file).unwrap();
        assert!(loaded.cached_analysis.is_none());
        assert!(loaded.cached_tailoring.is_none());
    }

    #[test]
    fn test_clear_removes_analysis_and_tailoring_cache() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");

        let cfg = config::Config {
            cached_analysis: Some(CachedAnalysis {
                repo_path: "/repo".to_string(),
                head_commit: Some("abc123".to_string()),
                config_hash: Some("hash".to_string()),
                analysis: RepoAnalysis {
                    is_monorepo: false,
                    workspace_type: None,
                    projects: vec![],
                },
                analyzed_at: chrono::Utc::now(),
            }),
            cached_tailoring: Some(CachedTailoring {
                cache_key: "key".to_string(),
                repo_path: "/repo".to_string(),
                tailoring: TailoringOutput {
                    files: vec![],
                    skipped_adrs: vec![],
                    summary: TailoringSummary::default(),
                },
                tailored_at: chrono::Utc::now(),
            }),
            ..config::Config::default()
        };
        config::paths::save_to(&cfg, &config_file).unwrap();

        let args = make_args_clear();
        let result = run_with_path(&args, &config_file);
        assert!(result.is_ok());

        // Verify both caches cleared
        let loaded = config::paths::load_from(&config_file).unwrap();
        assert!(
            loaded.cached_analysis.is_none(),
            "analysis cache should be cleared"
        );
        assert!(
            loaded.cached_tailoring.is_none(),
            "tailoring cache should be cleared"
        );
    }

    #[test]
    fn test_clear_preserves_other_config_fields() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");

        let cfg = config::Config {
            model: Some("opus".to_string()),
            batch_size: Some(20),
            cached_tailoring: Some(CachedTailoring {
                cache_key: "key".to_string(),
                repo_path: "/repo".to_string(),
                tailoring: TailoringOutput {
                    files: vec![],
                    skipped_adrs: vec![],
                    summary: TailoringSummary::default(),
                },
                tailored_at: chrono::Utc::now(),
            }),
            ..config::Config::default()
        };
        config::paths::save_to(&cfg, &config_file).unwrap();

        let args = make_args_clear();
        run_with_path(&args, &config_file).unwrap();

        let loaded = config::paths::load_from(&config_file).unwrap();
        assert_eq!(
            loaded.model,
            Some("opus".to_string()),
            "model should be preserved"
        );
        assert_eq!(
            loaded.batch_size,
            Some(20),
            "batch_size should be preserved"
        );
        assert!(
            loaded.cached_tailoring.is_none(),
            "tailoring cache should be cleared"
        );
    }

    #[test]
    fn test_exec_returns_ok() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");

        let args = make_args_clear();
        let result = run_with_resolver(&args, || Ok(config_file.clone()));
        assert!(result.is_ok());
    }

    #[test]
    fn test_exec_error_on_invalid_path() {
        let args = make_args_clear();
        let result = run_with_resolver(&args, || {
            Err(ActualError::ConfigError("simulated failure".into()))
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_handle_result_exec_returns_zero() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");
        let args = make_args_clear();
        let result = run_with_path(&args, &config_file);
        assert_eq!(handle_result(result), 0);
    }

    #[test]
    fn test_exec_uses_config_path_via_env_var() {
        use crate::testutil::{EnvGuard, ENV_MUTEX};

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Redirect the real config path to a temp file via ACTUAL_CONFIG.
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");
        config::paths::save_to(&config::Config::default(), &config_file).unwrap();

        let _guard = EnvGuard::set("ACTUAL_CONFIG", config_file.to_str().unwrap());
        let args = make_args_clear();
        let result = exec(&args);

        assert!(result.is_ok());
    }
}
