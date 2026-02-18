use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::ActualError;

fn config_error(msg: impl std::fmt::Display) -> ActualError {
    ActualError::ConfigError(msg.to_string())
}

/// Returns the config directory: `~/.actualai/actual/`.
///
/// Resolution order:
/// 1. If `ACTUAL_CONFIG` is set, return its parent directory.
/// 2. If `ACTUAL_CONFIG_DIR` is set, return that path.
/// 3. Otherwise, `$HOME/.actualai/actual`.
pub fn config_dir() -> Result<PathBuf, ActualError> {
    config_dir_with_home(dirs::home_dir())
}

fn config_dir_with_home(home: Option<PathBuf>) -> Result<PathBuf, ActualError> {
    if let Ok(config_file) = std::env::var("ACTUAL_CONFIG") {
        let p = PathBuf::from(config_file);
        return p
            .parent()
            .map(|d| d.to_path_buf())
            .ok_or_else(|| config_error("ACTUAL_CONFIG has no parent directory"));
    }

    if let Ok(dir) = std::env::var("ACTUAL_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }

    home.map(|h| h.join(".actualai").join("actual"))
        .ok_or_else(|| config_error("Unable to determine home directory"))
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
    match std::fs::read_to_string(path) {
        Ok(contents) => serde_yaml::from_str(&contents)
            .map_err(|e| config_error(format!("Failed to parse config YAML: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let default = Config::default();
            save_to(&default, path)?;
            Ok(default)
        }
        Err(e) => Err(config_error(format!("Failed to read config file: {e}"))),
    }
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
        std::fs::create_dir_all(parent)
            .map_err(|e| config_error(format!("Failed to create config directory: {e}")))?;
    }

    // Config contains only Option<String>, Option<bool>, Option<usize>, etc.
    // serde_yaml serialization is infallible for these types, so unwrap is safe.
    let yaml = serde_yaml::to_string(config).unwrap();
    std::fs::write(path, yaml)
        .map_err(|e| config_error(format!("Failed to write config file: {e}")))?;
    set_config_permissions(path)?;

    Ok(())
}

#[cfg(unix)]
fn set_config_permissions(path: &Path) -> Result<(), ActualError> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms)
        .map_err(|e| config_error(format!("Failed to set config file permissions: {e}")))
}

#[cfg(not(unix))]
fn set_config_permissions(_path: &Path) -> Result<(), ActualError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::ENV_MUTEX;
    use tempfile::tempdir;

    /// Helper: save and clear both config env vars, returning saved values.
    fn clear_env_vars() -> (Option<String>, Option<String>) {
        let saved_config = std::env::var("ACTUAL_CONFIG").ok();
        let saved_config_dir = std::env::var("ACTUAL_CONFIG_DIR").ok();
        std::env::remove_var("ACTUAL_CONFIG");
        std::env::remove_var("ACTUAL_CONFIG_DIR");
        (saved_config, saved_config_dir)
    }

    /// Helper: restore a single env var from a saved value.
    fn restore_env_var(name: &str, value: Option<String>) {
        match value {
            Some(v) => std::env::set_var(name, v),
            None => std::env::remove_var(name),
        }
    }

    /// Helper: restore env vars from saved values.
    fn restore_env_vars(saved: (Option<String>, Option<String>)) {
        restore_env_var("ACTUAL_CONFIG", saved.0);
        restore_env_var("ACTUAL_CONFIG_DIR", saved.1);
    }

    #[test]
    fn test_restore_env_vars_with_saved_values() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // Set both env vars so clear_env_vars saves Some(...) values
        std::env::set_var("ACTUAL_CONFIG", "/tmp/saved_config.yaml");
        std::env::set_var("ACTUAL_CONFIG_DIR", "/tmp/saved_dir");

        let saved = clear_env_vars();
        assert!(saved.0.is_some());
        assert!(saved.1.is_some());
        // Both env vars should be cleared
        assert!(std::env::var("ACTUAL_CONFIG").is_err());
        assert!(std::env::var("ACTUAL_CONFIG_DIR").is_err());

        // Restore should set them back (exercises the Some branch)
        restore_env_vars(saved);
        assert_eq!(
            std::env::var("ACTUAL_CONFIG").unwrap(),
            "/tmp/saved_config.yaml"
        );
        assert_eq!(
            std::env::var("ACTUAL_CONFIG_DIR").unwrap(),
            "/tmp/saved_dir"
        );

        // Clean up
        std::env::remove_var("ACTUAL_CONFIG");
        std::env::remove_var("ACTUAL_CONFIG_DIR");
    }

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
    fn test_load_from_nonexistent_path_creates_default() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("subdir").join("config.yaml");
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

    #[test]
    fn test_load_from_invalid_yaml() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");

        std::fs::write(&config_file, "{{{{not valid yaml").unwrap();

        let err = load_from(&config_file).unwrap_err();
        assert!(err.to_string().contains("Failed to parse config YAML"));
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
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = clear_env_vars();

        let dir = config_dir().unwrap();
        assert!(dir.ends_with(".actualai/actual"));

        restore_env_vars(saved);
    }

    #[test]
    fn test_config_dir_with_actual_config_env() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = clear_env_vars();

        let tmp = tempdir().unwrap();
        let config_file = tmp.path().join("myconfig.yaml");
        std::env::set_var("ACTUAL_CONFIG", config_file.to_str().unwrap());

        let dir = config_dir().unwrap();
        assert_eq!(dir, tmp.path());

        restore_env_vars(saved);
    }

    #[test]
    fn test_config_dir_with_actual_config_dir_env() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = clear_env_vars();

        let tmp = tempdir().unwrap();
        std::env::set_var("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        let dir = config_dir().unwrap();
        assert_eq!(dir, tmp.path());

        restore_env_vars(saved);
    }

    #[test]
    fn test_config_path_default() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = clear_env_vars();

        let path = config_path().unwrap();
        assert!(path.ends_with(".actualai/actual/config.yaml"));

        restore_env_vars(saved);
    }

    #[test]
    fn test_config_path_with_actual_config_env() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = clear_env_vars();

        let tmp = tempdir().unwrap();
        let config_file = tmp.path().join("custom.yaml");
        std::env::set_var("ACTUAL_CONFIG", config_file.to_str().unwrap());

        let path = config_path().unwrap();
        assert_eq!(path, config_file);

        restore_env_vars(saved);
    }

    #[test]
    fn test_load_and_save_via_env_var() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = clear_env_vars();

        let tmp = tempdir().unwrap();
        let config_file = tmp.path().join("config.yaml");
        std::env::set_var("ACTUAL_CONFIG", config_file.to_str().unwrap());

        // save() should create the file at the env-var path
        let config = Config {
            model: Some("haiku".to_string()),
            ..Config::default()
        };
        save(&config).unwrap();
        assert!(config_file.exists());

        // load() should read it back
        let loaded = load().unwrap();
        assert_eq!(loaded.model, Some("haiku".to_string()));

        restore_env_vars(saved);
    }

    #[test]
    fn test_save_creates_nested_directories() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("a").join("b").join("c").join("config.yaml");

        save_to(&Config::default(), &config_file).unwrap();
        assert!(config_file.exists());
    }

    #[test]
    fn test_config_dir_actual_config_takes_precedence_over_config_dir() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = clear_env_vars();

        let tmp1 = tempdir().unwrap();
        let tmp2 = tempdir().unwrap();
        let config_file = tmp1.path().join("config.yaml");
        std::env::set_var("ACTUAL_CONFIG", config_file.to_str().unwrap());
        std::env::set_var("ACTUAL_CONFIG_DIR", tmp2.path().to_str().unwrap());

        // ACTUAL_CONFIG should take precedence
        let dir = config_dir().unwrap();
        assert_eq!(dir, tmp1.path());

        restore_env_vars(saved);
    }

    #[test]
    fn test_load_from_unreadable_directory() {
        // Attempt to read from a path inside a nonexistent restricted location.
        // On any OS, reading a file that exists but contains invalid YAML triggers
        // the parse error path; reading from a nonexistent file triggers auto-create.
        // We test the read error by making the file a directory instead.
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");
        std::fs::create_dir_all(&config_file).unwrap();

        let err = load_from(&config_file).unwrap_err();
        // The read_to_string will fail because config_file is a directory
        assert!(err.to_string().contains("Failed to read config file"));
    }

    #[test]
    fn test_save_to_invalid_path() {
        // Try to save to a path that can't be created (under /dev/null on unix)
        #[cfg(unix)]
        {
            let path = PathBuf::from("/dev/null/impossible/config.yaml");
            let err = save_to(&Config::default(), &path).unwrap_err();
            assert!(err
                .to_string()
                .contains("Failed to create config directory"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_save_to_readonly_directory() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let readonly_dir = dir.path().join("readonly");
        std::fs::create_dir_all(&readonly_dir).unwrap();

        // Create the file first, then make the directory read-only
        let config_file = readonly_dir.join("config.yaml");
        std::fs::write(&config_file, "old content").unwrap();
        // Make file read-only
        std::fs::set_permissions(&config_file, std::fs::Permissions::from_mode(0o444)).unwrap();
        // Make directory read-only
        std::fs::set_permissions(&readonly_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let err = save_to(&Config::default(), &config_file).unwrap_err();
        assert!(err.to_string().contains("Failed to write config file"));

        // Restore permissions for cleanup
        std::fs::set_permissions(&readonly_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&config_file, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn test_config_dir_actual_config_bare_filename() {
        // When ACTUAL_CONFIG is set to a bare filename, parent is Some("").
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = clear_env_vars();

        std::env::set_var("ACTUAL_CONFIG", "config.yaml");
        let dir = config_dir().unwrap();
        assert_eq!(dir, PathBuf::from(""));

        restore_env_vars(saved);
    }

    #[test]
    fn test_config_dir_actual_config_empty_string() {
        // When ACTUAL_CONFIG is set to an empty string, parent() returns None.
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = clear_env_vars();

        std::env::set_var("ACTUAL_CONFIG", "");
        let err = config_dir().unwrap_err();
        assert!(err
            .to_string()
            .contains("ACTUAL_CONFIG has no parent directory"));

        restore_env_vars(saved);
    }

    #[test]
    fn test_load_via_env_var_absent_creates_default() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = clear_env_vars();

        let tmp = tempdir().unwrap();
        let config_file = tmp.path().join("new_config.yaml");
        std::env::set_var("ACTUAL_CONFIG", config_file.to_str().unwrap());

        // load() with env var pointing to nonexistent file should auto-create
        let config = load().unwrap();
        assert_eq!(config, Config::default());
        assert!(config_file.exists());

        restore_env_vars(saved);
    }

    #[test]
    fn test_config_path_with_config_dir_env() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = clear_env_vars();

        let tmp = tempdir().unwrap();
        std::env::set_var("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        let path = config_path().unwrap();
        assert_eq!(path, tmp.path().join("config.yaml"));

        restore_env_vars(saved);
    }

    #[test]
    fn test_config_dir_no_home_directory() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = clear_env_vars();

        let err = config_dir_with_home(None).unwrap_err();
        assert!(err
            .to_string()
            .contains("Unable to determine home directory"));

        restore_env_vars(saved);
    }

    #[test]
    fn test_config_dir_with_home_provided() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = clear_env_vars();

        let home = PathBuf::from("/fake/home");
        let dir = config_dir_with_home(Some(home)).unwrap();
        assert_eq!(dir, PathBuf::from("/fake/home/.actualai/actual"));

        restore_env_vars(saved);
    }

    #[test]
    fn test_save_to_path_without_parent() {
        // When path.parent() returns None (root path), save_to skips
        // directory creation. On unix "/" has no writable location so
        // the write itself will fail, but the important thing is we
        // exercise the None branch of `if let Some(parent)`.
        let path = Path::new("/");
        let err = save_to(&Config::default(), path).unwrap_err();
        // The error is from std::fs::write, not from create_dir_all
        assert!(!err
            .to_string()
            .contains("Failed to create config directory"));
    }

    #[cfg(unix)]
    #[test]
    fn test_set_config_permissions_success() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        std::fs::write(&path, "content").unwrap();

        set_config_permissions(&path).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn test_set_config_permissions_nonexistent_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.yaml");

        let err = set_config_permissions(&path).unwrap_err();
        assert!(err
            .to_string()
            .contains("Failed to set config file permissions"));
    }
}
