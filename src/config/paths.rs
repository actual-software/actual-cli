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
        let dir_path = PathBuf::from(&dir);
        if !dir_path.is_absolute() {
            return Err(config_error("ACTUAL_CONFIG_DIR must be an absolute path"));
        }
        if dir_path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
        {
            return Err(config_error(
                "ACTUAL_CONFIG_DIR must not contain '..' components",
            ));
        }
        return Ok(dir_path);
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
    const MAX_CONFIG_SIZE: u64 = 1024 * 1024; // 1 MiB
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_CONFIG_SIZE => {
            return Err(config_error(format!(
                "Config file is too large ({} bytes, max {} bytes)",
                meta.len(),
                MAX_CONFIG_SIZE
            )));
        }
        _ => {}
    }
    match std::fs::read_to_string(path) {
        Ok(contents) => serde_yml::from_str(&contents)
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
/// On unix, creates the file with permissions `0600` atomically (no TOCTOU gap).
pub fn save_to(config: &Config, path: &Path) -> Result<(), ActualError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| config_error(format!("Failed to create config directory: {e}")))?;
    }

    let yaml = serde_yml::to_string(config)
        .map_err(|e| config_error(format!("Failed to serialize config to YAML: {e}")))?;
    write_config_secure(path, &yaml)?;

    Ok(())
}

/// Write config content to a file, ensuring it is never world-readable.
///
/// On unix, opens the file with `O_CREAT | mode(0o600)` so the file is created
/// with restricted permissions from the start — no TOCTOU gap.
/// On non-unix, falls back to a plain write.
#[cfg(unix)]
fn write_config_secure(path: &Path, content: &str) -> Result<(), ActualError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| config_error(format!("Failed to open config file for writing: {e}")))?;
    file.write_all(content.as_bytes())
        .map_err(|e| config_error(format!("Failed to write config file: {e}")))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_config_secure(path: &Path, content: &str) -> Result<(), ActualError> {
    std::fs::write(path, content)
        .map_err(|e| config_error(format!("Failed to write config file: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use tempfile::tempdir;

    #[test]
    fn test_env_guard_restores_both_config_vars() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Set both env vars
        {
            let _g1 = EnvGuard::set("ACTUAL_CONFIG", "/tmp/saved_config.yaml");
            let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", "/tmp/saved_dir");
            assert_eq!(
                std::env::var("ACTUAL_CONFIG").unwrap(),
                "/tmp/saved_config.yaml"
            );
            assert_eq!(
                std::env::var("ACTUAL_CONFIG_DIR").unwrap(),
                "/tmp/saved_dir"
            );
        }
        // Guards dropped: both should be removed (were absent before)
        assert!(std::env::var("ACTUAL_CONFIG").is_err());
        assert!(std::env::var("ACTUAL_CONFIG_DIR").is_err());
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
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::remove("ACTUAL_CONFIG_DIR");

        let dir = config_dir().unwrap();
        assert!(dir.ends_with(".actualai/actual"));
    }

    #[test]
    fn test_config_dir_with_actual_config_env() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::remove("ACTUAL_CONFIG_DIR");

        let tmp = tempdir().unwrap();
        let config_file = tmp.path().join("myconfig.yaml");
        let _guard = EnvGuard::set("ACTUAL_CONFIG", config_file.to_str().unwrap());

        let dir = config_dir().unwrap();
        assert_eq!(dir, tmp.path());
    }

    #[test]
    fn test_config_dir_with_actual_config_dir_env() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::remove("ACTUAL_CONFIG_DIR");

        let tmp = tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        let dir = config_dir().unwrap();
        assert_eq!(dir, tmp.path());
    }

    #[test]
    fn test_config_path_default() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::remove("ACTUAL_CONFIG_DIR");

        let path = config_path().unwrap();
        assert!(path.ends_with(".actualai/actual/config.yaml"));
    }

    #[test]
    fn test_config_path_with_actual_config_env() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::remove("ACTUAL_CONFIG_DIR");

        let tmp = tempdir().unwrap();
        let config_file = tmp.path().join("custom.yaml");
        let _guard = EnvGuard::set("ACTUAL_CONFIG", config_file.to_str().unwrap());

        let path = config_path().unwrap();
        assert_eq!(path, config_file);
    }

    #[test]
    fn test_load_and_save_via_env_var() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::remove("ACTUAL_CONFIG_DIR");

        let tmp = tempdir().unwrap();
        let config_file = tmp.path().join("config.yaml");
        let _guard = EnvGuard::set("ACTUAL_CONFIG", config_file.to_str().unwrap());

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
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::remove("ACTUAL_CONFIG_DIR");

        let tmp1 = tempdir().unwrap();
        let tmp2 = tempdir().unwrap();
        let config_file = tmp1.path().join("config.yaml");
        let _ga = EnvGuard::set("ACTUAL_CONFIG", config_file.to_str().unwrap());
        let _gb = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp2.path().to_str().unwrap());

        // ACTUAL_CONFIG should take precedence
        let dir = config_dir().unwrap();
        assert_eq!(dir, tmp1.path());
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
        // With write_config_secure, the open() call fails on read-only file
        assert!(
            err.to_string()
                .contains("Failed to open config file for writing"),
            "Unexpected error: {err}"
        );

        // Restore permissions for cleanup
        std::fs::set_permissions(&readonly_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&config_file, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn test_config_dir_actual_config_bare_filename() {
        // When ACTUAL_CONFIG is set to a bare filename, parent is Some("").
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::remove("ACTUAL_CONFIG_DIR");
        let _guard = EnvGuard::set("ACTUAL_CONFIG", "config.yaml");

        let dir = config_dir().unwrap();
        assert_eq!(dir, PathBuf::from(""));
    }

    #[test]
    fn test_config_dir_actual_config_empty_string() {
        // When ACTUAL_CONFIG is set to an empty string, parent() returns None.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::remove("ACTUAL_CONFIG_DIR");
        let _guard = EnvGuard::set("ACTUAL_CONFIG", "");

        let err = config_dir().unwrap_err();
        assert!(err
            .to_string()
            .contains("ACTUAL_CONFIG has no parent directory"));
    }

    #[test]
    fn test_load_via_env_var_absent_creates_default() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::remove("ACTUAL_CONFIG_DIR");

        let tmp = tempdir().unwrap();
        let config_file = tmp.path().join("new_config.yaml");
        let _guard = EnvGuard::set("ACTUAL_CONFIG", config_file.to_str().unwrap());

        // load() with env var pointing to nonexistent file should auto-create
        let config = load().unwrap();
        assert_eq!(config, Config::default());
        assert!(config_file.exists());
    }

    #[test]
    fn test_config_path_with_config_dir_env() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::remove("ACTUAL_CONFIG_DIR");

        let tmp = tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        let path = config_path().unwrap();
        assert_eq!(path, tmp.path().join("config.yaml"));
    }

    #[test]
    fn test_config_dir_no_home_directory() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::remove("ACTUAL_CONFIG_DIR");

        let err = config_dir_with_home(None).unwrap_err();
        assert!(err
            .to_string()
            .contains("Unable to determine home directory"));
    }

    #[test]
    fn test_config_dir_with_home_provided() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::remove("ACTUAL_CONFIG_DIR");

        let home = PathBuf::from("/fake/home");
        let dir = config_dir_with_home(Some(home)).unwrap();
        assert_eq!(dir, PathBuf::from("/fake/home/.actualai/actual"));
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

    // Tests for g5j.3: secure config write (no TOCTOU)
    #[cfg(unix)]
    #[test]
    fn test_save_to_creates_file_with_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");

        save_to(&Config::default(), &config_file).unwrap();

        let mode = std::fs::metadata(&config_file)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "Config file must be created with mode 0600");
    }

    #[test]
    fn test_save_to_returns_error_on_serialization_failure() {
        use crate::config::types::CachedTailoring;

        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");

        // Construct a CachedTailoring with a serde_yml::Value that cannot be
        // serialized by serde_yml 0.9 to YAML: a Mapping whose key is itself a
        // Mapping. serde_yml cannot emit mapping keys that are themselves
        // mappings (not valid YAML) and returns an error.
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

        // Verify the value is actually unserializable (guards against library
        // version changes that may start accepting this).
        assert!(
            serde_yml::to_string(&bad_value).is_err(),
            "serde_yml must reject a mapping with mapping keys for this test to be valid"
        );

        let config = Config {
            cached_tailoring: Some(CachedTailoring {
                cache_key: "key".to_string(),
                repo_path: "/tmp/repo".to_string(),
                tailoring: bad_value,
                tailored_at: chrono::Utc::now(),
            }),
            ..Config::default()
        };

        let err = save_to(&config, &config_file).unwrap_err();
        assert!(
            err.to_string()
                .contains("Failed to serialize config to YAML"),
            "Expected serialization error, got: {err}"
        );
    }

    // Tests for g5j.20: config file size limit
    #[test]
    fn test_load_from_rejects_oversized_file() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("big_config.yaml");

        // Write slightly more than 1 MiB
        let big_content = vec![b'#'; 1024 * 1024 + 1];
        std::fs::write(&config_file, &big_content).unwrap();

        let err = load_from(&config_file).unwrap_err();
        assert!(
            err.to_string().contains("Config file is too large"),
            "Expected size-limit error, got: {err}"
        );
    }

    #[test]
    fn test_load_from_accepts_file_exactly_at_limit() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");

        // 1 MiB exactly — should not be rejected by the size check.
        // Use `{` repeated to produce invalid YAML, so we get a parse error
        // rather than a size-limit error.
        let content = vec![b'{'; 1024 * 1024];
        std::fs::write(&config_file, &content).unwrap();

        let err = load_from(&config_file).unwrap_err();
        assert!(
            !err.to_string().contains("Config file is too large"),
            "A file exactly at the limit should not trigger the size-limit error, got: {err}"
        );
        assert!(
            err.to_string().contains("Failed to parse config YAML"),
            "Expected YAML parse error, got: {err}"
        );
    }

    // Tests for g5j.21: ACTUAL_CONFIG_DIR validation
    #[test]
    fn test_config_dir_rejects_relative_actual_config_dir() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::remove("ACTUAL_CONFIG_DIR");

        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", "relative/path");

        let err = config_dir().unwrap_err();
        assert!(
            err.to_string().contains("must be an absolute path"),
            "Expected absolute-path error, got: {err}"
        );
    }

    #[test]
    fn test_config_dir_accepts_absolute_actual_config_dir() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::remove("ACTUAL_CONFIG_DIR");

        let tmp = tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        let dir = config_dir().unwrap();
        assert_eq!(dir, tmp.path());
    }

    #[test]
    fn test_config_dir_rejects_actual_config_dir_with_dotdot() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::remove("ACTUAL_CONFIG_DIR");

        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", "/some/path/../etc");

        let err = config_dir().unwrap_err();
        assert!(
            err.to_string().contains("must not contain '..'"),
            "Expected traversal error, got: {err}"
        );
    }
}
