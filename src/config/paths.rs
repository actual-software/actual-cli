use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::ActualError;

/// Returns the config directory: `~/.actualai/actual/`.
///
/// Resolution order:
/// 1. If `ACTUAL_CONFIG` is set, return its parent directory.
/// 2. If `ACTUAL_CONFIG_DIR` is set, return that path.
/// 3. Otherwise, `$HOME/.actualai/actual`.
pub fn config_dir() -> Result<PathBuf, ActualError> {
    if let Ok(config_file) = std::env::var("ACTUAL_CONFIG") {
        let p = PathBuf::from(config_file);
        return p.parent().map(|d| d.to_path_buf()).ok_or_else(|| {
            ActualError::ConfigError("ACTUAL_CONFIG has no parent directory".into())
        });
    }

    if let Ok(dir) = std::env::var("ACTUAL_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }

    dirs::home_dir()
        .map(|h| h.join(".actualai").join("actual"))
        .ok_or_else(|| ActualError::ConfigError("Unable to determine home directory".into()))
}

/// Returns the config file path: `~/.actualai/actual/config.yaml`.
///
/// If `ACTUAL_CONFIG` is set, that exact path is returned.
/// Otherwise returns `config_dir()?.join("config.yaml")`.
pub fn config_path() -> Result<PathBuf, ActualError> {
    if let Ok(path) = std::env::var("ACTUAL_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    Ok(config_dir()?.join("config.yaml"))
}

/// Load config from disk using the default config path.
///
/// If the file is absent, creates it with [`Config::default()`] and returns that.
/// On unix, sets file permissions to `0600`.
pub fn load() -> Result<Config, ActualError> {
    load_from(&config_path()?)
}

/// Load config from a specific path.
///
/// If the file does not exist, creates it with [`Config::default()`] and returns
/// that default. On unix, the created file has permissions `0600`.
pub fn load_from(path: &Path) -> Result<Config, ActualError> {
    if !path.exists() {
        let default = Config::default();
        save_to(&default, path)?;
        return Ok(default);
    }

    let contents = std::fs::read_to_string(path)
        .map_err(|e| ActualError::ConfigError(format!("Failed to read config file: {e}")))?;

    serde_yaml::from_str(&contents)
        .map_err(|e| ActualError::ConfigError(format!("Failed to parse config YAML: {e}")))
}

/// Save config to disk using the default config path.
///
/// Creates the parent directory recursively if needed.
/// On unix, sets file permissions to `0600`.
pub fn save(config: &Config) -> Result<(), ActualError> {
    save_to(config, &config_path()?)
}

/// Save config to a specific path.
///
/// Creates the parent directory recursively if needed.
/// On unix, sets file permissions to `0600`.
pub fn save_to(config: &Config, path: &Path) -> Result<(), ActualError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            ActualError::ConfigError(format!("Failed to create config directory: {e}"))
        })?;
    }

    let yaml = serde_yaml::to_string(config)
        .map_err(|e| ActualError::ConfigError(format!("Failed to serialize config: {e}")))?;

    std::fs::write(path, &yaml)
        .map_err(|e| ActualError::ConfigError(format!("Failed to write config file: {e}")))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms).map_err(|e| {
            ActualError::ConfigError(format!("Failed to set config file permissions: {e}"))
        })?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_load_creates_file_when_absent() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");
        assert!(!config_file.exists());

        let config = load_from(&config_file).unwrap();
        assert_eq!(config, Config::default());
        assert!(config_file.exists());
    }

    #[test]
    fn test_load_reads_existing_config() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");

        let yaml = "model: opus\nbatch_size: 10\n";
        std::fs::write(&config_file, yaml).unwrap();

        let config = load_from(&config_file).unwrap();
        assert_eq!(config.model, Some("opus".to_string()));
        assert_eq!(config.batch_size, Some(10));
        assert_eq!(config.api_url, None);
    }

    #[cfg(unix)]
    #[test]
    fn test_file_permissions_unix() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");

        save_to(&Config::default(), &config_file).unwrap();

        let metadata = std::fs::metadata(&config_file).unwrap();
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn test_save_then_load_roundtrip() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");

        let config = Config {
            api_url: Some("https://api.example.com".to_string()),
            model: Some("sonnet".to_string()),
            batch_size: Some(20),
            concurrency: Some(5),
            include_general: Some(true),
            ..Config::default()
        };

        save_to(&config, &config_file).unwrap();
        let loaded = load_from(&config_file).unwrap();
        assert_eq!(config, loaded);
    }

    #[test]
    fn test_config_dir_default() {
        // Unset both env vars so we fall through to the home_dir default.
        // We save and restore the env vars to avoid interfering with other tests.
        let saved_config = std::env::var("ACTUAL_CONFIG").ok();
        let saved_config_dir = std::env::var("ACTUAL_CONFIG_DIR").ok();
        std::env::remove_var("ACTUAL_CONFIG");
        std::env::remove_var("ACTUAL_CONFIG_DIR");

        let dir = config_dir().unwrap();
        assert!(
            dir.ends_with(".actualai/actual"),
            "expected path ending in .actualai/actual, got: {dir:?}"
        );

        // Restore env vars
        if let Some(v) = saved_config {
            std::env::set_var("ACTUAL_CONFIG", v);
        }
        if let Some(v) = saved_config_dir {
            std::env::set_var("ACTUAL_CONFIG_DIR", v);
        }
    }

    #[test]
    fn test_config_path_default() {
        let saved_config = std::env::var("ACTUAL_CONFIG").ok();
        let saved_config_dir = std::env::var("ACTUAL_CONFIG_DIR").ok();
        std::env::remove_var("ACTUAL_CONFIG");
        std::env::remove_var("ACTUAL_CONFIG_DIR");

        let path = config_path().unwrap();
        assert!(
            path.ends_with(".actualai/actual/config.yaml"),
            "expected path ending in .actualai/actual/config.yaml, got: {path:?}"
        );

        // Restore env vars
        if let Some(v) = saved_config {
            std::env::set_var("ACTUAL_CONFIG", v);
        }
        if let Some(v) = saved_config_dir {
            std::env::set_var("ACTUAL_CONFIG_DIR", v);
        }
    }

    #[test]
    fn test_save_creates_nested_directories() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("a").join("b").join("c").join("config.yaml");

        save_to(&Config::default(), &config_file).unwrap();
        assert!(config_file.exists());
    }
}
