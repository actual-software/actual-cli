//! Ephemeral credential storage for the CLI's public-repo onboarding flow.
//!
//! Unlike [`super::store::StoredCredentials`], ephemeral credentials hold only
//! an access token (no refresh token). The browser owns the long-lived session;
//! the CLI re-launches the browser PKCE flow when the access token expires.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::paths::config_dir;
use crate::error::ActualError;

const EPHEMERAL_FILENAME: &str = "ephemeral_session.yaml";
const MAX_FILE_SIZE: u64 = 64 * 1024;

fn ephemeral_error(msg: impl std::fmt::Display) -> ActualError {
    ActualError::ConfigError(msg.to_string())
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EphemeralCredentials {
    pub access_token: String,
    pub token_type: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub scope: Option<String>,
    pub organization_id: String,
    pub member_id: String,
    pub auth_url: Option<String>,
}

impl std::fmt::Debug for EphemeralCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EphemeralCredentials")
            .field("access_token", &"<redacted>")
            .field("token_type", &self.token_type)
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .field("organization_id", &self.organization_id)
            .field("member_id", &self.member_id)
            .field("auth_url", &self.auth_url)
            .finish()
    }
}

impl EphemeralCredentials {
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        match self.expires_at {
            Some(exp) => now >= exp,
            None => false,
        }
    }
}

pub fn ephemeral_path() -> Result<PathBuf, ActualError> {
    Ok(config_dir()?.join(EPHEMERAL_FILENAME))
}

pub fn load() -> Result<Option<EphemeralCredentials>, ActualError> {
    load_from(&ephemeral_path()?)
}

pub fn load_from(path: &Path) -> Result<Option<EphemeralCredentials>, ActualError> {
    match std::fs::read(path) {
        Ok(bytes) => {
            if bytes.len() as u64 > MAX_FILE_SIZE {
                return Err(ephemeral_error(format!(
                    "Ephemeral credentials file too large ({} bytes)",
                    bytes.len()
                )));
            }
            let contents = String::from_utf8(bytes)
                .map_err(|e| ephemeral_error(format!("Not valid UTF-8: {e}")))?;
            let creds = serde_yml::from_str(&contents)
                .map_err(|e| ephemeral_error(format!("Failed to parse: {e}")))?;
            Ok(Some(creds))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ephemeral_error(format!("Failed to read: {e}"))),
    }
}

pub fn save(creds: &EphemeralCredentials) -> Result<(), ActualError> {
    save_to(creds, &ephemeral_path()?)
}

pub fn save_to(creds: &EphemeralCredentials, path: &Path) -> Result<(), ActualError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ephemeral_error(format!("Failed to create directory: {e}")))?;
    }
    let yaml = serde_yml::to_string(creds)
        .map_err(|e| ephemeral_error(format!("Failed to serialize: {e}")))?;
    write_secure(path, &yaml)
}

pub fn clear() -> Result<(), ActualError> {
    clear_at(&ephemeral_path()?)
}

pub fn clear_at(path: &Path) -> Result<(), ActualError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ephemeral_error(format!("Failed to remove: {e}"))),
    }
}

#[cfg(unix)]
fn write_secure(path: &Path, content: &str) -> Result<(), ActualError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| ephemeral_error(format!("Failed to open for writing: {e}")))?;
    file.write_all(content.as_bytes())
        .map_err(|e| ephemeral_error(format!("Failed to write: {e}")))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_secure(path: &Path, content: &str) -> Result<(), ActualError> {
    std::fs::write(path, content)
        .map_err(|e| ephemeral_error(format!("Failed to write: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use tempfile::tempdir;

    fn sample() -> EphemeralCredentials {
        EphemeralCredentials {
            access_token: "test-access-token".to_string(),
            token_type: "Bearer".to_string(),
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
            scope: Some("openid profile repo:onboard".to_string()),
            organization_id: "org-1".to_string(),
            member_id: "member-1".to_string(),
            auth_url: Some("https://app.actual.ai".to_string()),
        }
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ephemeral_session.yaml");
        let creds = sample();
        save_to(&creds, &path).unwrap();
        let loaded = load_from(&path).unwrap().expect("should load");
        assert_eq!(loaded.access_token, creds.access_token);
        assert_eq!(loaded.organization_id, creds.organization_id);
        assert_eq!(loaded.member_id, creds.member_id);
    }

    #[test]
    fn load_missing_returns_none() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.yaml");
        let loaded = load_from(&path).unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn clear_removes_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ephemeral_session.yaml");
        save_to(&sample(), &path).unwrap();
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

    #[test]
    fn is_expired_true_when_past() {
        let mut creds = sample();
        creds.expires_at = Some(Utc::now() - chrono::Duration::hours(1));
        assert!(creds.is_expired(Utc::now()));
    }

    #[test]
    fn is_expired_false_when_future() {
        let creds = sample();
        assert!(!creds.is_expired(Utc::now()));
    }

    #[test]
    fn debug_redacts_token() {
        let creds = sample();
        let debug = format!("{:?}", creds);
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("test-access-token"));
    }
}
