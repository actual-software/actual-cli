//! Pending operation persistence for the CLI's auth-then-act flow.
//!
//! When the CLI needs to authenticate before performing an action (e.g.,
//! `repo onboard`), it saves the intended operation to disk so it can resume
//! after the OAuth flow completes.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::paths::config_dir;
use crate::error::ActualError;

const PENDING_FILENAME: &str = "pending_op.yaml";
const MAX_FILE_SIZE: u64 = 64 * 1024;

fn pending_error(msg: impl std::fmt::Display) -> ActualError {
    ActualError::ConfigError(msg.to_string())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PendingOperation {
    RepoOnboard { url: String },
}

pub fn pending_path() -> Result<PathBuf, ActualError> {
    Ok(config_dir()?.join(PENDING_FILENAME))
}

pub fn load() -> Result<Option<PendingOperation>, ActualError> {
    load_from(&pending_path()?)
}

pub fn load_from(path: &Path) -> Result<Option<PendingOperation>, ActualError> {
    match std::fs::read(path) {
        Ok(bytes) => {
            if bytes.len() as u64 > MAX_FILE_SIZE {
                return Err(pending_error(format!(
                    "Pending operation file too large ({} bytes)",
                    bytes.len()
                )));
            }
            let contents = String::from_utf8(bytes)
                .map_err(|e| pending_error(format!("Not valid UTF-8: {e}")))?;
            let op = serde_yml::from_str(&contents)
                .map_err(|e| pending_error(format!("Failed to parse: {e}")))?;
            Ok(Some(op))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(pending_error(format!("Failed to read: {e}"))),
    }
}

pub fn save(op: &PendingOperation) -> Result<(), ActualError> {
    save_to(op, &pending_path()?)
}

pub fn save_to(op: &PendingOperation, path: &Path) -> Result<(), ActualError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| pending_error(format!("Failed to create directory: {e}")))?;
    }
    let yaml = serde_yml::to_string(op)
        .map_err(|e| pending_error(format!("Failed to serialize: {e}")))?;
    std::fs::write(path, yaml)
        .map_err(|e| pending_error(format!("Failed to write: {e}")))?;
    Ok(())
}

pub fn clear() -> Result<(), ActualError> {
    clear_at(&pending_path()?)
}

pub fn clear_at(path: &Path) -> Result<(), ActualError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(pending_error(format!("Failed to remove: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn save_load_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("pending_op.yaml");
        let op = PendingOperation::RepoOnboard {
            url: "https://github.com/acme/widgets".to_string(),
        };
        save_to(&op, &path).unwrap();
        let loaded = load_from(&path).unwrap().expect("should load");
        assert_eq!(loaded, op);
    }

    #[test]
    fn load_missing_returns_none() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.yaml");
        assert!(load_from(&path).unwrap().is_none());
    }

    #[test]
    fn clear_removes_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("pending_op.yaml");
        let op = PendingOperation::RepoOnboard {
            url: "https://github.com/acme/widgets".to_string(),
        };
        save_to(&op, &path).unwrap();
        assert!(path.exists());
        clear_at(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn clear_missing_is_ok() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.yaml");
        assert!(clear_at(&path).is_ok());
    }
}
