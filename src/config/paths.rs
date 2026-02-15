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
    if !path.exists() {
        let default = Config::default();
        save_to(&default, path)?;
        return Ok(default);
    }

    let contents = std::fs::read_to_string(path)
        .map_err(|e| config_error(format!("Failed to read config file: {e}")))?;

    serde_yaml::from_str(&contents)
        .map_err(|e| config_error(format!("Failed to parse config YAML: {e}")))
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

    let yaml = serialize_config(config)?;
    write_config_file(path, &yaml)?;
    set_config_permissions(path)?;

    Ok(())
}

fn serialize_config(config: &Config) -> Result<String, ActualError> {
    serde_yaml::to_string(config)
        .map_err(|e| config_error(format!("Failed to serialize config: {e}")))
}

fn write_config_file(path: &Path, contents: &str) -> Result<(), ActualError> {
    std::fs::write(path, contents)
        .map_err(|e| config_error(format!("Failed to write config file: {e}")))
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
    use std::sync::Mutex;
    use tempfile::tempdir;

    /// Mutex to serialize tests that manipulate env vars, since env vars are
    /// process-global and tests run in parallel by default.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// Helper: save and clear both config env vars, returning saved values.
    fn clear_env_vars() -> (Option<String>, Option<String>) {
        let saved_config = std::env::var("ACTUAL_CONFIG").ok();
        let saved_config_dir = std::env::var("ACTUAL_CONFIG_DIR").ok();
        std::env::remove_var("ACTUAL_CONFIG");
        std::env::remove_var("ACTUAL_CONFIG_DIR");
        (saved_config, saved_config_dir)
    }

    /// Helper: restore env vars from saved values.
    fn restore_env_vars(saved: (Option<String>, Option<String>)) {
        match saved.0 {
            Some(v) => std::env::set_var("ACTUAL_CONFIG", v),
            None => std::env::remove_var("ACTUAL_CONFIG"),
        }
        match saved.1 {
            Some(v) => std::env::set_var("ACTUAL_CONFIG_DIR", v),
            None => std::env::remove_var("ACTUAL_CONFIG_DIR"),
        }
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
        let msg = err.to_string();
        assert!(
            msg.contains("Failed to parse config YAML"),
            "expected parse error, got: {msg}"
        );
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
        assert!(
            dir.ends_with(".actualai/actual"),
            "expected path ending in .actualai/actual, got: {dir:?}"
        );

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
        assert!(
            path.ends_with(".actualai/actual/config.yaml"),
            "expected path ending in .actualai/actual/config.yaml, got: {path:?}"
        );

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
        let msg = err.to_string();
        // The read_to_string will fail because config_file is a directory
        assert!(
            msg.contains("Failed to read config file"),
            "expected read error, got: {msg}"
        );
    }

    #[test]
    fn test_save_to_invalid_path() {
        // Try to save to a path that can't be created (under /dev/null on unix)
        #[cfg(unix)]
        {
            let path = PathBuf::from("/dev/null/impossible/config.yaml");
            let err = save_to(&Config::default(), &path).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("Failed to create config directory"),
                "expected dir creation error, got: {msg}"
            );
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
        let msg = err.to_string();
        assert!(
            msg.contains("Failed to write config file"),
            "expected write error, got: {msg}"
        );

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
        let msg = err.to_string();
        assert!(
            msg.contains("ACTUAL_CONFIG has no parent directory"),
            "expected no-parent error, got: {msg}"
        );

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
        let msg = err.to_string();
        assert!(
            msg.contains("Unable to determine home directory"),
            "expected home dir error, got: {msg}"
        );

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
    fn test_serialize_config_success() {
        let yaml = serialize_config(&Config::default()).unwrap();
        assert!(!yaml.is_empty());
    }

    #[test]
    fn test_write_config_file_success() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        write_config_file(&path, "test: value\n").unwrap();
        assert!(path.exists());
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "test: value\n");
    }

    #[test]
    fn test_write_config_file_error() {
        // Write to an invalid path to trigger the error
        #[cfg(unix)]
        {
            let path = PathBuf::from("/dev/null/impossible.yaml");
            let err = write_config_file(&path, "test").unwrap_err();
            assert!(
                err.to_string().contains("Failed to write config file"),
                "unexpected error: {}",
                err
            );
        }
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
        assert!(
            err.to_string()
                .contains("Failed to set config file permissions"),
            "unexpected error: {}",
            err
        );
    }
}
