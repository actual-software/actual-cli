//! Secure local persistence for Actual AI platform credentials.
//!
//! Credentials live under the existing config-dir convention
//! (`~/.actualai/actual/credentials.yaml`) and are written with `0600`
//! permissions, mirroring [`crate::config::paths`]. Token material is never
//! logged: the [`std::fmt::Debug`] impl redacts it.
//!
//! Every write of the stored sign-in happens under one exclusive [`Lock`]:
//! `login`'s save, `logout`'s delete, and a refresh by a command or by the
//! telemetry uploader. The server takes a refresh token once and revokes it
//! when it rotates, so two processes refreshing the same sign-in side by side
//! would leave the slower one signed out. A lock holder reads the sign-in again
//! once it has the lock and acts on what it finds then.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::config::paths::config_dir;
use crate::error::ActualError;

/// Filename for the persisted platform credentials, stored alongside the CLI
/// config under the config-dir convention (`~/.actualai/actual/`).
const CREDENTIALS_FILENAME: &str = "credentials.yaml";

/// The file [`Lock`] locks, beside the credentials. Nothing is ever written to
/// it.
const LOCK_FILENAME: &str = "credentials.lock";

/// Maximum credentials file size. Tokens are small; anything larger is
/// rejected to avoid loading a pathological file into memory.
const MAX_CREDENTIALS_SIZE: u64 = 64 * 1024; // 64 KiB

fn credentials_error(msg: impl std::fmt::Display) -> ActualError {
    ActualError::ConfigError(msg.to_string())
}

fn default_token_type() -> String {
    "Bearer".to_string()
}

/// Platform credentials issued by the Actual AI OAuth server for the
/// signed-in user and their selected organization.
///
/// Persisted locally with `0600` permissions. The [`std::fmt::Debug`] impl is
/// hand-written to redact the token material so secrets never reach logs,
/// panic output, or `{:?}` formatting.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCredentials {
    /// OAuth access token (Bearer). **Sensitive — never logged.**
    pub access_token: String,
    /// OAuth refresh token used for rotation. **Sensitive — never logged.**
    pub refresh_token: String,
    /// Token type, normally `Bearer`.
    #[serde(default = "default_token_type")]
    pub token_type: String,
    /// Absolute expiry of the access token, derived from `expires_in` at the
    /// time the token was issued. `None` if the server provided no lifetime.
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    /// Space-delimited granted scopes, if returned by the server.
    #[serde(default)]
    pub scope: Option<String>,
    /// The organization the user authenticated to (multi-org selection binds
    /// the chosen org's id into the token; we mirror it here).
    pub organization_id: String,
    /// The user's member id within `organization_id`.
    pub member_id: String,
    /// The user's email, if present in the id token / userinfo.
    #[serde(default)]
    pub email: Option<String>,
    /// The OIDC subject (stable user id), if present.
    #[serde(default)]
    pub subject: Option<String>,
    /// The auth server this session was issued by, so `logout`/refresh can
    /// reach the same endpoint without the user re-specifying it.
    /// Non-sensitive (just an endpoint).
    #[serde(default)]
    pub auth_url: Option<String>,
}

impl StoredCredentials {
    /// Returns `true` if the access token is expired at `now`.
    ///
    /// Credentials with no recorded `expires_at` are treated as not expired —
    /// the server did not commit to a lifetime, so refresh-on-401 handles it.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        match self.expires_at {
            Some(exp) => now >= exp,
            None => false,
        }
    }

    /// Returns `true` if the access token expires within `leeway` of `now`.
    /// Used to refresh proactively rather than waiting for a 401.
    pub fn expires_within(&self, now: DateTime<Utc>, leeway: Duration) -> bool {
        match self.expires_at {
            Some(exp) => now + leeway >= exp,
            None => false,
        }
    }
}

/// Redacted debug output — token fields are masked so secrets never leak into
/// logs, panic messages, or `{:?}` formatting.
impl std::fmt::Debug for StoredCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredCredentials")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("token_type", &self.token_type)
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .field("organization_id", &self.organization_id)
            .field("member_id", &self.member_id)
            .field("email", &self.email)
            .field("subject", &self.subject)
            .field("auth_url", &self.auth_url)
            .finish()
    }
}

/// Path to the credentials file: `<config_dir>/credentials.yaml`.
pub fn credentials_path() -> Result<PathBuf, ActualError> {
    Ok(config_dir()?.join(CREDENTIALS_FILENAME))
}

/// Load stored credentials, or `None` if the user is not logged in.
pub fn load() -> Result<Option<StoredCredentials>, ActualError> {
    load_from(&credentials_path()?)
}

/// Load credentials from a specific path. Returns `None` if the file is absent.
pub fn load_from(path: &Path) -> Result<Option<StoredCredentials>, ActualError> {
    match std::fs::read(path) {
        Ok(bytes) => {
            if bytes.len() as u64 > MAX_CREDENTIALS_SIZE {
                return Err(credentials_error(format!(
                    "Credentials file is too large ({} bytes, max {} bytes)",
                    bytes.len(),
                    MAX_CREDENTIALS_SIZE
                )));
            }
            let contents = String::from_utf8(bytes).map_err(|e| {
                credentials_error(format!("Credentials file is not valid UTF-8: {e}"))
            })?;
            let creds = crate::yaml::from_str(&contents)
                .map_err(|e| credentials_error(format!("Failed to parse credentials: {e}")))?;
            Ok(Some(creds))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(credentials_error(format!(
            "Failed to read credentials file: {e}"
        ))),
    }
}

/// Persist credentials using the default path, with `0600` permissions, under
/// [`lock`]. A caller that already holds the lock saves through it instead.
pub fn save(creds: &StoredCredentials) -> Result<(), ActualError> {
    lock()?.save(creds)
}

/// An exclusive hold on the stored sign-in, released on drop. Its methods read
/// and write the sign-in at the default path.
pub struct Lock {
    // Held only for the advisory lock, which the OS releases when it closes.
    _file: std::fs::File,
    path: PathBuf,
}

impl Lock {
    /// The stored sign-in, or `None` when there is none.
    pub fn load(&self) -> Result<Option<StoredCredentials>, ActualError> {
        load_from(&self.path)
    }

    /// Replace the stored sign-in with `creds`.
    pub fn save(&self, creds: &StoredCredentials) -> Result<(), ActualError> {
        save_to(creds, &self.path)
    }

    /// Remove the stored sign-in. Idempotent.
    pub fn delete(&self) -> Result<(), ActualError> {
        delete_from(&self.path)
    }
}

/// Take the lock on the stored sign-in, waiting while another process holds
/// it.
pub fn lock() -> Result<Lock, ActualError> {
    let path = credentials_path()?;
    let file = open_lock_file(&path)?;
    file.lock().map_err(lock_error)?;
    Ok(Lock { _file: file, path })
}

/// Take the lock on the stored sign-in if nobody holds it, or `None` when
/// another process does.
pub fn try_lock() -> Result<Option<Lock>, ActualError> {
    let path = credentials_path()?;
    let file = open_lock_file(&path)?;
    match file.try_lock().map_err(std::io::Error::from) {
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        held => held
            .map(|()| Some(Lock { _file: file, path }))
            .map_err(lock_error),
    }
}

/// Open the lock file beside the credentials at `path`, creating it, and the
/// config directory, when absent. Owner-only on unix.
fn open_lock_file(path: &Path) -> Result<std::fs::File, ActualError> {
    let lock_path = path.with_file_name(LOCK_FILENAME);
    // `credentials_path` always has a parent; the fallback keeps this branch-free.
    std::fs::create_dir_all(lock_path.parent().unwrap_or(Path::new("."))).map_err(lock_error)?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(&lock_path).map_err(lock_error)
}

fn lock_error(e: std::io::Error) -> ActualError {
    credentials_error(format!("Failed to lock the stored sign-in: {e}"))
}

/// Persist credentials to a specific path. Creates parent directories; on unix
/// the file is created `0600` atomically (no TOCTOU gap).
pub fn save_to(creds: &StoredCredentials, path: &Path) -> Result<(), ActualError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| credentials_error(format!("Failed to create config directory: {e}")))?;
    }
    let yaml = crate::yaml::to_string(creds)
        .map_err(|e| credentials_error(format!("Failed to serialize credentials: {e}")))?;
    write_secure(path, &yaml)
}

/// Delete credentials at a specific path. Idempotent.
///
/// Also removes the `.tmp` staging sibling [`crate::config::paths::write_secure`]
/// may have left behind if a prior save died between staging and rename —
/// that sibling holds the same plaintext access and refresh tokens, so
/// `actual logout` leaving it behind would mean logout did not actually
/// revoke local access to those credentials.
pub fn delete_from(path: &Path) -> Result<(), ActualError> {
    crate::config::paths::remove_secure(path)
        .map_err(|e| credentials_error(format!("Failed to remove credentials file: {e}")))
}

/// Write credentials content, ensuring the file is never world-readable.
///
/// Delegates the actual write to [`crate::config::paths::write_secure`] for
/// its atomic stage-then-rename idiom (see that function's own doc) rather
/// than re-implementing the `O_CREAT | mode(0o600)` idiom here a second
/// time.
fn write_secure(path: &Path, content: &str) -> Result<(), ActualError> {
    crate::config::paths::write_secure(path, content.as_bytes())
        .map_err(|e| credentials_error(format!("Failed to write credentials file: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use tempfile::tempdir;

    fn sample() -> StoredCredentials {
        StoredCredentials {
            access_token: "access-secret-AKIA-shaped".to_string(),
            refresh_token: "refresh-secret-value".to_string(),
            token_type: "Bearer".to_string(),
            expires_at: Some(Utc::now() + Duration::hours(1)),
            scope: Some("openid profile adr:query".to_string()),
            organization_id: "mock-org-0001".to_string(),
            member_id: "mock-member-0001".to_string(),
            email: Some("dev@example.com".to_string()),
            subject: Some("mock-user-0001".to_string()),
            auth_url: Some("http://localhost:4000".to_string()),
        }
    }

    #[test]
    fn test_save_then_load_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(CREDENTIALS_FILENAME);

        let creds = sample();
        save_to(&creds, &path).unwrap();
        let loaded = load_from(&path).unwrap().expect("credentials should load");
        assert_eq!(creds, loaded);
    }

    #[test]
    fn test_load_from_absent_returns_none() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(CREDENTIALS_FILENAME);
        assert!(load_from(&path).unwrap().is_none());
    }

    #[test]
    fn test_save_creates_nested_directories() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a").join("b").join(CREDENTIALS_FILENAME);
        save_to(&sample(), &path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn test_delete_removes_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(CREDENTIALS_FILENAME);
        save_to(&sample(), &path).unwrap();
        assert!(path.exists());
        delete_from(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn test_delete_absent_is_ok() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(CREDENTIALS_FILENAME);
        // Idempotent: deleting a non-existent credentials file succeeds.
        delete_from(&path).unwrap();
    }

    /// The gap this guards: a write that staged the `.tmp` sibling but died
    /// before rename leaves plaintext tokens on disk that `logout` must
    /// still remove, not just whatever file happened to complete a rename.
    #[test]
    fn test_delete_from_also_removes_a_leftover_tmp_sibling() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(CREDENTIALS_FILENAME);
        save_to(&sample(), &path).unwrap();
        let tmp = dir.path().join(format!("{CREDENTIALS_FILENAME}.tmp"));
        std::fs::write(&tmp, b"stale plaintext tokens from an interrupted save").unwrap();

        delete_from(&path).unwrap();

        assert!(!path.exists());
        assert!(!tmp.exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_saved_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let path = dir.path().join(CREDENTIALS_FILENAME);
        save_to(&sample(), &path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "credentials file must be created with mode 0600"
        );
    }

    #[test]
    fn test_debug_redacts_tokens() {
        let creds = sample();
        let debug = format!("{creds:?}");
        assert!(
            !debug.contains("access-secret-AKIA-shaped"),
            "Debug output must not contain the access token: {debug}"
        );
        assert!(
            !debug.contains("refresh-secret-value"),
            "Debug output must not contain the refresh token: {debug}"
        );
        assert!(
            debug.contains("<redacted>"),
            "expected redaction marker: {debug}"
        );
        // Non-sensitive identity fields are fine to show.
        assert!(
            debug.contains("mock-org-0001"),
            "org id should be visible: {debug}"
        );
    }

    #[test]
    fn test_is_expired() {
        let now = Utc::now();
        let mut creds = sample();

        creds.expires_at = Some(now - Duration::seconds(1));
        assert!(creds.is_expired(now), "past expiry should be expired");

        creds.expires_at = Some(now + Duration::hours(1));
        assert!(
            !creds.is_expired(now),
            "future expiry should not be expired"
        );

        creds.expires_at = None;
        assert!(
            !creds.is_expired(now),
            "no expiry recorded => treated as not expired"
        );
    }

    #[test]
    fn test_expires_within() {
        let now = Utc::now();
        let mut creds = sample();

        creds.expires_at = Some(now + Duration::seconds(30));
        assert!(
            creds.expires_within(now, Duration::seconds(60)),
            "token expiring in 30s should be within a 60s leeway"
        );
        assert!(
            !creds.expires_within(now, Duration::seconds(10)),
            "token expiring in 30s should not be within a 10s leeway"
        );

        creds.expires_at = None;
        assert!(
            !creds.expires_within(now, Duration::hours(1)),
            "no expiry recorded => never within leeway"
        );
    }

    #[test]
    fn test_load_from_rejects_oversized_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(CREDENTIALS_FILENAME);
        let big = vec![b'#'; (MAX_CREDENTIALS_SIZE + 1) as usize];
        std::fs::write(&path, &big).unwrap();
        let err = load_from(&path).unwrap_err();
        assert!(
            err.to_string().contains("too large"),
            "expected size-limit error, got: {err}"
        );
    }

    #[test]
    fn test_load_from_invalid_utf8() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(CREDENTIALS_FILENAME);
        std::fs::write(&path, [0xFF, 0xFE, 0x00, 0x01]).unwrap();
        let err = load_from(&path).unwrap_err();
        assert!(
            err.to_string().contains("not valid UTF-8"),
            "expected UTF-8 error, got: {err}"
        );
    }

    #[test]
    fn test_load_from_invalid_yaml() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(CREDENTIALS_FILENAME);
        // Valid YAML structurally, but missing required fields => parse/deser error.
        std::fs::write(&path, "not_a_credentials_struct: true\n").unwrap();
        let err = load_from(&path).unwrap_err();
        assert!(
            err.to_string().contains("Failed to parse credentials"),
            "expected parse error, got: {err}"
        );
    }

    #[test]
    fn test_token_type_defaults_to_bearer_when_absent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(CREDENTIALS_FILENAME);
        // A minimal credentials file omitting token_type should default it.
        let yaml = "access_token: a\nrefresh_token: r\norganization_id: org\nmember_id: m\n";
        std::fs::write(&path, yaml).unwrap();
        let creds = load_from(&path).unwrap().expect("should load");
        assert_eq!(creds.token_type, "Bearer");
    }

    #[test]
    fn test_credentials_path_uses_config_dir() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::remove("ACTUAL_CONFIG_DIR");

        let tmp = tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        let path = credentials_path().unwrap();
        assert_eq!(path, tmp.path().join(CREDENTIALS_FILENAME));
    }

    #[test]
    fn test_default_path_save_load_delete_roundtrip() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        let creds = sample();
        assert!(load().unwrap().is_none());
        save(&creds).unwrap();
        assert_eq!(load().unwrap().unwrap(), creds);
        let lock = lock().unwrap();
        assert_eq!(lock.load().unwrap().unwrap(), creds);
        lock.delete().unwrap();
        assert!(lock.load().unwrap().is_none());
        drop(lock);
        assert!(load().unwrap().is_none());
    }

    // ── the lock every writer takes ─────────────────────────────────────

    /// The lock is exclusive between handles, as it is between processes:
    /// while one is held, another can't be taken, and dropping it frees it.
    #[test]
    fn test_the_lock_is_exclusive_until_dropped() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        let held = lock().unwrap();
        assert!(try_lock().unwrap().is_none());
        drop(held);
        let taken = try_lock().unwrap().expect("a free lock is taken");
        let creds = sample();
        taken.save(&creds).unwrap();
        assert_eq!(taken.load().unwrap(), Some(creds));
    }

    /// `save` waits for a sign-in someone else is writing, then lands after
    /// them, so a sign-in saved during a refresh survives it.
    #[test]
    fn test_save_waits_for_the_lock() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        let mut earlier = sample();
        earlier.organization_id = "org-before".to_string();
        let mut later = sample();
        later.organization_id = "org-after".to_string();

        let held = lock().unwrap();
        let saver = std::thread::spawn({
            let later = later.clone();
            move || save(&later)
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!saver.is_finished(), "save went ahead without the lock");
        held.save(&earlier).unwrap();
        drop(held);
        saver.join().unwrap().unwrap();

        assert_eq!(load().unwrap().unwrap().organization_id, "org-after");
    }

    /// The lock file sits beside the credentials, owner-only, and the config
    /// directory is created for it on a first sign-in.
    #[cfg(unix)]
    #[test]
    fn test_the_lock_file_is_owner_only_beside_the_credentials() {
        use std::os::unix::fs::PermissionsExt;
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("fresh");
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", dir.to_str().unwrap());

        let _held = lock().unwrap();

        let mode = std::fs::metadata(dir.join(LOCK_FILENAME))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(!dir.join(CREDENTIALS_FILENAME).exists());
    }

    /// A lock file that can't be opened is an error for both ways of taking
    /// it, and nothing is written.
    #[test]
    fn test_an_unopenable_lock_file_is_an_error() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        std::fs::create_dir(tmp.path().join(LOCK_FILENAME)).unwrap();

        let err = lock().err().expect("a directory can't be locked");
        assert!(err
            .to_string()
            .contains("Failed to lock the stored sign-in"));
        assert!(try_lock().is_err());
        assert!(save(&sample()).is_err());
        assert!(!tmp.path().join(CREDENTIALS_FILENAME).exists());
    }

    /// A config directory that can't be created is an error too.
    #[cfg(unix)]
    #[test]
    fn test_a_config_dir_that_cant_be_created_is_an_error() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", "/dev/null/actual");

        assert!(lock().is_err());
    }

    #[test]
    fn test_load_from_read_error_on_directory() {
        // Reading a path that is a directory triggers a non-NotFound read error.
        let dir = tempdir().unwrap();
        let err = load_from(dir.path()).unwrap_err();
        assert!(
            err.to_string().contains("Failed to read credentials file"),
            "got: {err}"
        );
    }

    #[test]
    fn test_delete_from_error_on_directory() {
        // remove_file on a directory errors with something other than NotFound.
        let dir = tempdir().unwrap();
        let sub = dir.path().join("subdir");
        std::fs::create_dir(&sub).unwrap();
        let err = delete_from(&sub).unwrap_err();
        assert!(
            err.to_string()
                .contains("Failed to remove credentials file"),
            "got: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_save_to_open_error_when_path_is_directory() {
        // write_secure stages its content at a sibling `.tmp` path, which
        // succeeds even when `path` itself is a directory -- the failure
        // instead surfaces at the final rename into place, since renaming a
        // file onto an existing directory is rejected by the OS.
        let dir = tempdir().unwrap();
        let as_dir = dir.path().join("creds-dir");
        std::fs::create_dir(&as_dir).unwrap();
        let err = save_to(&sample(), &as_dir).unwrap_err();
        assert!(
            err.to_string().contains("Failed to write credentials file"),
            "got: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_save_to_create_dir_error() {
        // Parent under a file (/dev/null) → create_dir_all fails (ENOTDIR).
        let path = PathBuf::from("/dev/null/nope/credentials.yaml");
        let err = save_to(&sample(), &path).unwrap_err();
        assert!(
            err.to_string()
                .contains("Failed to create config directory"),
            "got: {err}"
        );
    }

    #[test]
    fn test_save_to_path_without_parent() {
        // "/" has no parent → exercises the skipped-create-dir branch; the
        // secure write then fails (can't open "/" for writing).
        let err = save_to(&sample(), Path::new("/")).unwrap_err();
        assert!(err.to_string().contains("Failed to"), "got: {err}");
    }

    /// A credentials file serde_yml 0.0.12 wrote reads back unchanged.
    #[test]
    fn test_reads_credentials_written_by_serde_yml() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("credentials.yaml");
        std::fs::write(
            &path,
            include_str!("../../tests/fixtures/serde_yml/credentials.yaml"),
        )
        .unwrap();
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/serde_yml/credentials.json"
        ))
        .unwrap();
        let creds = load_from(&path).unwrap().expect("credentials present");
        assert_eq!(serde_json::to_value(&creds).unwrap(), expected);

        save_to(&creds, &path).unwrap();
        assert_eq!(load_from(&path).unwrap(), Some(creds));
    }
}
