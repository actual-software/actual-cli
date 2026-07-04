//! Secure storage for scoped personal access tokens (PATs).
//!
//! A PAT minted by `actual auth create-token` is a long-lived bearer secret.
//! It must never sit in plaintext on disk, in shell history, in process
//! arguments, or in an agent's prompt / log stream. This module gives the CLI
//! two storage backends and one non-interactive read path, so the raw token is
//! shown once at mint time and thereafter only ever lives behind the OS
//! keychain, an encrypted file, or the process environment.
//!
//! ## Backends
//!
//! - **`Keyring`** — the OS keychain (macOS Keychain, Windows Credential
//!   Manager, Linux kernel keyutils) via the portable [`keyring`] crate. This is
//!   the primary store on a desktop / interactive box.
//! - **`EncryptedFile`** — an XChaCha20-Poly1305 AEAD file under the config
//!   directory, with the key derived by Argon2id from a passphrase read from
//!   `ACTUAL_TOKEN_PASSPHRASE`. This is the fallback for headless Linux / CI
//!   where the OS keychain is unavailable.
//!
//! ## Backend selection (`ACTUAL_TOKEN_STORE`)
//!
//! - `auto` (default / unset): try the keychain; if it is unavailable, fall
//!   back to the encrypted file (when a passphrase is configured). This is the
//!   graceful-degradation path the headless-storage POC question asks about.
//! - `keyring`: force the OS keychain.
//! - `file`: force the encrypted file.
//!
//! ## Non-interactive read precedence ([`resolve`])
//!
//! 1. The `ACTUAL_TOKEN` environment variable — the CI path, no storage needed.
//! 2. The OS keychain.
//! 3. The encrypted fallback file.
//!
//! Token material is never written to a log or a `Debug` impl in this module.

use std::path::{Path, PathBuf};

use argon2::Argon2;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::{Aead, KeyInit, OsRng, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};

use crate::config::paths::config_dir;
use crate::error::ActualError;

/// Keychain service name under which all PATs are filed. The keychain entry's
/// "account" is the per-credential `--name`.
pub const KEYRING_SERVICE: &str = "actual-cli";

/// Environment variable holding a ready-to-use PAT for non-interactive
/// (CI / agent) use. When set, it wins over any stored credential.
pub const ACTUAL_TOKEN_ENV: &str = "ACTUAL_TOKEN";

/// Environment variable selecting the storage backend: `auto` (default),
/// `keyring`, or `file`.
pub const STORE_BACKEND_ENV: &str = "ACTUAL_TOKEN_STORE";

/// Environment variable holding the passphrase that protects the encrypted-file
/// fallback. Required to use (read or write) the `file` backend.
pub const PASSPHRASE_ENV: &str = "ACTUAL_TOKEN_PASSPHRASE";

/// Subdirectory of the config dir holding encrypted token files.
const TOKEN_SUBDIR: &str = "tokens";

const ARGON2_KEY_LEN: usize = 32;
const SALT_LEN: usize = 16;
const XNONCE_LEN: usize = 24;
const MAX_NAME_LEN: usize = 200;
const MAX_TOKEN_FILE_SIZE: u64 = 64 * 1024;

fn token_error(msg: impl std::fmt::Display) -> ActualError {
    ActualError::ConfigError(msg.to_string())
}

/// Which backend a store / retrieve operation actually used. Returned by
/// [`store`] so the command can tell the user where the secret landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// The OS keychain (macOS Keychain, Windows Credential Manager, Linux
    /// kernel keyutils).
    Keyring,
    /// The Argon2id + XChaCha20-Poly1305 encrypted fallback file.
    EncryptedFile,
}

impl Backend {
    /// Human-readable label for user-facing messages. Carries no secret.
    pub fn label(self) -> &'static str {
        match self {
            Backend::Keyring => "OS keychain",
            Backend::EncryptedFile => "encrypted file",
        }
    }
}

/// Resolved backend-selection mode from `ACTUAL_TOKEN_STORE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Auto,
    Keyring,
    File,
}

fn selected_mode() -> Result<Mode, ActualError> {
    match std::env::var(STORE_BACKEND_ENV).ok().as_deref() {
        None | Some("") | Some("auto") => Ok(Mode::Auto),
        Some("keyring") => Ok(Mode::Keyring),
        Some("file") => Ok(Mode::File),
        Some(other) => Err(token_error(format!(
            "invalid {STORE_BACKEND_ENV}={other:?} (expected one of: auto, keyring, file)"
        ))),
    }
}

/// The configured fallback passphrase, or `None` when unset / empty.
fn passphrase() -> Option<String> {
    std::env::var(PASSPHRASE_ENV).ok().filter(|s| !s.is_empty())
}

/// The `ACTUAL_TOKEN` env value, or `None` when unset / empty.
fn env_token() -> Option<String> {
    std::env::var(ACTUAL_TOKEN_ENV)
        .ok()
        .filter(|s| !s.is_empty())
}

/// Reject names that are empty, over-long, or contain control characters. The
/// raw name is used as the keychain account; a sanitized form names the file.
fn validate_name(name: &str) -> Result<(), ActualError> {
    if name.is_empty() {
        return Err(token_error("token name must not be empty"));
    }
    if name.len() > MAX_NAME_LEN {
        return Err(token_error(format!(
            "token name is too long ({} chars, max {MAX_NAME_LEN})",
            name.len()
        )));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(token_error(
            "token name must not contain control characters",
        ));
    }
    Ok(())
}

/// Map a name to a safe filename stem: keep `[A-Za-z0-9._-]`, replace the rest
/// with `_`. Deterministic, so the same name always maps to the same file.
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn token_dir() -> Result<PathBuf, ActualError> {
    Ok(config_dir()?.join(TOKEN_SUBDIR))
}

fn token_file(name: &str) -> Result<PathBuf, ActualError> {
    Ok(token_dir()?.join(format!("{}.token.json", sanitize_name(name))))
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Store `token` under `name`, returning which backend held it.
///
/// In `auto` mode the OS keychain is tried first; if it is unavailable (the
/// headless-Linux / no-OS-keychain case) the encrypted file is used instead,
/// provided `ACTUAL_TOKEN_PASSPHRASE` is set. The raw token is never written in
/// plaintext under any path.
pub fn store(name: &str, token: &str) -> Result<Backend, ActualError> {
    validate_name(name)?;
    match selected_mode()? {
        Mode::Keyring => {
            keyring_set(name, token)?;
            Ok(Backend::Keyring)
        }
        Mode::File => {
            file_set(name, token)?;
            Ok(Backend::EncryptedFile)
        }
        Mode::Auto => match keyring_set(name, token) {
            Ok(()) => Ok(Backend::Keyring),
            Err(keyring_err) => {
                if passphrase().is_some() {
                    file_set(name, token)?;
                    Ok(Backend::EncryptedFile)
                } else {
                    Err(token_error(format!(
                        "the OS keychain is unavailable ({keyring_err}), and no encrypted-file \
                         fallback is configured. Set {PASSPHRASE_ENV} to enable the encrypted-file \
                         fallback, or use the {ACTUAL_TOKEN_ENV} environment variable for \
                         non-interactive use."
                    )))
                }
            }
        },
    }
}

/// Verify — **without** minting or writing anything — that a storage backend is
/// available to hold a freshly-minted PAT. Call this *before* the token is
/// minted so the predictable headless-Linux / no-keychain / no-passphrase case
/// fails cleanly instead of stranding an orphaned, unrevocable live token.
///
/// A pass here is necessary but not sufficient: a later [`store`] can still fail
/// for reasons a non-writing probe cannot see (a full disk, a keychain lock
/// race). Callers must therefore still handle a [`store`] error by surfacing the
/// minted token for manual capture and revocation — this only removes the
/// *predictable* orphan, not every possible one.
pub fn precheck(name: &str) -> Result<(), ActualError> {
    validate_name(name)?;
    match selected_mode()? {
        Mode::Keyring => {
            if keyring_available(name) {
                Ok(())
            } else {
                Err(token_error(format!(
                    "the OS keychain is unavailable and {STORE_BACKEND_ENV}=keyring forces it. \
                     Use {STORE_BACKEND_ENV}=file with {PASSPHRASE_ENV} set, or provide a token \
                     via {ACTUAL_TOKEN_ENV}, for non-interactive use."
                )))
            }
        }
        Mode::File => {
            if passphrase().is_some() {
                Ok(())
            } else {
                Err(token_error(format!(
                    "{PASSPHRASE_ENV} must be set to use the encrypted-file token store."
                )))
            }
        }
        Mode::Auto => {
            if keyring_available(name) || passphrase().is_some() {
                Ok(())
            } else {
                Err(token_error(format!(
                    "the OS keychain is unavailable and no encrypted-file fallback is configured. \
                     Set {PASSPHRASE_ENV} to enable the encrypted-file fallback, or provide a \
                     token via {ACTUAL_TOKEN_ENV}, for non-interactive use."
                )))
            }
        }
    }
}

/// Read the token stored under `name` from the keychain / file (not the
/// environment). Returns `None` when no credential is stored.
pub fn retrieve(name: &str) -> Result<Option<String>, ActualError> {
    validate_name(name)?;
    match selected_mode()? {
        Mode::Keyring => keyring_get(name),
        Mode::File => file_get(name),
        Mode::Auto => match keyring_get(name) {
            Ok(Some(tok)) => Ok(Some(tok)),
            Ok(None) => file_get(name),
            Err(keyring_err) => {
                // Keychain unavailable (headless) — fall through to the file.
                tracing::debug!("keychain read failed, trying file fallback: {keyring_err}");
                file_get(name)
            }
        },
    }
}

/// Delete the token stored under `name`. Idempotent: a missing credential is
/// success. In `auto` mode both backends are cleared.
pub fn delete(name: &str) -> Result<(), ActualError> {
    validate_name(name)?;
    match selected_mode()? {
        Mode::Keyring => keyring_delete(name),
        Mode::File => file_delete(name),
        Mode::Auto => {
            // Best-effort on the keychain (may be unavailable headless); always
            // clear the file so no fallback copy lingers.
            if let Err(keyring_err) = keyring_delete(name) {
                tracing::debug!("keychain delete failed, clearing file fallback: {keyring_err}");
            }
            file_delete(name)
        }
    }
}

/// Resolve a usable token for non-interactive use: the `ACTUAL_TOKEN`
/// environment variable wins, then the stored credential under `name`.
pub fn resolve(name: &str) -> Result<Option<String>, ActualError> {
    if let Some(tok) = env_token() {
        return Ok(Some(tok));
    }
    retrieve(name)
}

// ── Keyring backend ─────────────────────────────────────────────────────────

fn keyring_entry(name: &str) -> Result<keyring::Entry, ActualError> {
    keyring::Entry::new(KEYRING_SERVICE, name)
        .map_err(|e| token_error(format!("keychain entry init failed: {e}")))
}

/// Best-effort, non-mutating probe: can the OS keychain be reached for this
/// service right now? A missing entry (`NoEntry`) counts as *available* — the
/// store is reachable, just empty. Any other error (no keychain backend at all,
/// e.g. headless Linux) counts as unavailable. Writes nothing, so it is safe to
/// call before minting.
fn keyring_available(name: &str) -> bool {
    let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, name) else {
        return false;
    };
    matches!(entry.get_password(), Ok(_) | Err(keyring::Error::NoEntry))
}

fn keyring_set(name: &str, token: &str) -> Result<(), ActualError> {
    keyring_entry(name)?
        .set_password(token)
        .map_err(|e| token_error(format!("keychain write failed: {e}")))
}

fn keyring_get(name: &str) -> Result<Option<String>, ActualError> {
    match keyring_entry(name)?.get_password() {
        Ok(tok) => Ok(Some(tok)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(token_error(format!("keychain read failed: {e}"))),
    }
}

fn keyring_delete(name: &str) -> Result<(), ActualError> {
    match keyring_entry(name)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(token_error(format!("keychain delete failed: {e}"))),
    }
}

// ── Encrypted-file backend ──────────────────────────────────────────────────

/// On-disk shape of an encrypted token. Holds only the salt, nonce, and
/// ciphertext (which includes the Poly1305 tag) — never the plaintext token.
#[derive(Debug, Serialize, Deserialize)]
struct EncryptedBlob {
    /// Format version, for forward compatibility.
    version: u8,
    /// Key-derivation function identifier.
    kdf: String,
    /// Argon2id salt, base64.
    salt: String,
    /// XChaCha20-Poly1305 nonce (24 bytes), base64.
    nonce: String,
    /// Ciphertext + auth tag, base64.
    ciphertext: String,
}

/// Gather `N` cryptographically-random bytes from the OS RNG and return them.
///
/// Returning the freshly filled array (rather than filling a caller-owned zero
/// buffer in place) keeps the random value off any `[0u8; N]` binding, so the
/// salt and nonce that flow into the KDF and cipher are never a constant.
fn random_bytes<const N: usize>() -> Result<[u8; N], ActualError> {
    let mut buf = [0u8; N];
    OsRng
        .try_fill_bytes(&mut buf)
        .map_err(|e| token_error(format!("failed to gather randomness: {e}")))?;
    Ok(buf)
}

fn derive_key(passphrase: &[u8], salt: &[u8]) -> Result<[u8; ARGON2_KEY_LEN], ActualError> {
    let mut key = [0u8; ARGON2_KEY_LEN];
    Argon2::default()
        .hash_password_into(passphrase, salt, &mut key)
        .map_err(|e| token_error(format!("key derivation failed: {e}")))?;
    Ok(key)
}

fn encrypt_token(name: &str, token: &str, passphrase: &[u8]) -> Result<EncryptedBlob, ActualError> {
    let salt: [u8; SALT_LEN] = random_bytes()?;
    let key = derive_key(passphrase, &salt)?;
    let cipher = XChaCha20Poly1305::new_from_slice(&key)
        .map_err(|e| token_error(format!("cipher init failed: {e}")))?;

    let nonce_bytes: [u8; XNONCE_LEN] = random_bytes()?;
    let nonce = XNonce::from_slice(&nonce_bytes);

    // Bind the credential name as associated data so a file cannot be silently
    // swapped between names.
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: token.as_bytes(),
                aad: name.as_bytes(),
            },
        )
        .map_err(|_| token_error("token encryption failed"))?;

    Ok(EncryptedBlob {
        version: 1,
        kdf: "argon2id".to_string(),
        salt: B64.encode(salt),
        nonce: B64.encode(nonce_bytes),
        ciphertext: B64.encode(ciphertext),
    })
}

fn decrypt_token(
    name: &str,
    blob: &EncryptedBlob,
    passphrase: &[u8],
) -> Result<String, ActualError> {
    let salt = B64
        .decode(&blob.salt)
        .map_err(|e| token_error(format!("token file salt is not valid base64: {e}")))?;
    let nonce_bytes = B64
        .decode(&blob.nonce)
        .map_err(|e| token_error(format!("token file nonce is not valid base64: {e}")))?;
    let ciphertext = B64
        .decode(&blob.ciphertext)
        .map_err(|e| token_error(format!("token file ciphertext is not valid base64: {e}")))?;

    if nonce_bytes.len() != XNONCE_LEN {
        return Err(token_error("token file nonce has the wrong length"));
    }
    if salt.len() < 8 {
        return Err(token_error("token file salt is too short"));
    }

    let key = derive_key(passphrase, &salt)?;
    let cipher = XChaCha20Poly1305::new_from_slice(&key)
        .map_err(|e| token_error(format!("cipher init failed: {e}")))?;
    let nonce = XNonce::from_slice(&nonce_bytes);

    let plaintext = cipher
        .decrypt(
            nonce,
            Payload {
                msg: &ciphertext,
                aad: name.as_bytes(),
            },
        )
        .map_err(|_| {
            token_error("token decryption failed (wrong passphrase or corrupted token file)")
        })?;

    String::from_utf8(plaintext).map_err(|_| token_error("decrypted token is not valid UTF-8"))
}

fn file_set(name: &str, token: &str) -> Result<(), ActualError> {
    let pass = passphrase().ok_or_else(|| {
        token_error(format!(
            "{PASSPHRASE_ENV} must be set to use the encrypted-file token store"
        ))
    })?;
    let blob = encrypt_token(name, token, pass.as_bytes())?;
    let json = serde_json::to_string_pretty(&blob)
        .map_err(|e| token_error(format!("failed to serialize encrypted token: {e}")))?;
    let path = token_file(name)?;
    write_secure(&path, &json)
}

fn file_get(name: &str) -> Result<Option<String>, ActualError> {
    let path = token_file(name)?;
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(token_error(format!("failed to read token file: {e}"))),
    };
    if bytes.len() as u64 > MAX_TOKEN_FILE_SIZE {
        return Err(token_error("token file is too large"));
    }
    let pass = passphrase().ok_or_else(|| {
        token_error(format!(
            "{PASSPHRASE_ENV} must be set to read the encrypted-file token store"
        ))
    })?;
    let blob: EncryptedBlob = serde_json::from_slice(&bytes)
        .map_err(|e| token_error(format!("token file is corrupted: {e}")))?;
    let token = decrypt_token(name, &blob, pass.as_bytes())?;
    Ok(Some(token))
}

fn file_delete(name: &str) -> Result<(), ActualError> {
    let path = token_file(name)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(token_error(format!("failed to remove token file: {e}"))),
    }
}

/// Write `content` to `path`, creating parents and (on unix) forcing `0600` so
/// the encrypted blob is never world-readable. Mirrors [`crate::auth::store`].
#[cfg(unix)]
fn write_secure(path: &Path, content: &str) -> Result<(), ActualError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| token_error(format!("failed to create token directory: {e}")))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| token_error(format!("failed to open token file for writing: {e}")))?;
    file.write_all(content.as_bytes())
        .map_err(|e| token_error(format!("failed to write token file: {e}")))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_secure(path: &Path, content: &str) -> Result<(), ActualError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| token_error(format!("failed to create token directory: {e}")))?;
    }
    std::fs::write(path, content)
        .map_err(|e| token_error(format!("failed to write token file: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use tempfile::tempdir;

    /// Set up an isolated env for the file backend: a temp config dir, the
    /// `file` backend forced, and a passphrase. Returns the guards (which must
    /// be held for the duration of the test) and the temp dir.
    fn file_backend_env(
        passphrase: &str,
    ) -> (tempfile::TempDir, EnvGuard, EnvGuard, EnvGuard, EnvGuard) {
        let tmp = tempdir().unwrap();
        let g_cfg_file = EnvGuard::remove("ACTUAL_CONFIG");
        let g_cfg = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        let g_store = EnvGuard::set(STORE_BACKEND_ENV, "file");
        let g_pass = EnvGuard::set(PASSPHRASE_ENV, passphrase);
        (tmp, g_cfg_file, g_cfg, g_store, g_pass)
    }

    #[test]
    fn test_precheck_file_mode_refuses_without_passphrase() {
        // The pre-mint gate for the target env: a headless / `file` backend with
        // no passphrase configured must be refused BEFORE any token is minted,
        // so no orphaned, unrevocable token is ever created.
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g_cfg_file = EnvGuard::remove("ACTUAL_CONFIG");
        let _g_cfg = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        let _g_store = EnvGuard::set(STORE_BACKEND_ENV, "file");
        let _g_pass = EnvGuard::remove(PASSPHRASE_ENV);

        assert!(
            precheck("agent").is_err(),
            "file backend with no passphrase must fail the pre-mint precheck"
        );
    }

    #[test]
    fn test_precheck_file_mode_passes_with_passphrase() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_tmp, _g0, _g1, _g2, _g3) = file_backend_env("a-strong-passphrase");
        assert!(
            precheck("agent").is_ok(),
            "file backend with a passphrase should pass the pre-mint precheck"
        );
    }

    #[test]
    fn test_file_roundtrip_store_retrieve_delete() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let g_env = EnvGuard::remove(ACTUAL_TOKEN_ENV);
        let (_tmp, _g0, _g1, _g2, _g3) = file_backend_env("a-strong-passphrase");

        let backend = store("ci-agent", "actl_pat_secretvalue123").unwrap();
        assert_eq!(backend, Backend::EncryptedFile);

        let got = retrieve("ci-agent").unwrap();
        assert_eq!(got.as_deref(), Some("actl_pat_secretvalue123"));

        delete("ci-agent").unwrap();
        assert_eq!(retrieve("ci-agent").unwrap(), None);
        drop(g_env);
    }

    #[test]
    fn test_file_never_contains_plaintext() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (tmp, _g0, _g1, _g2, _g3) = file_backend_env("pw");

        let secret = "actl_pat_do_not_leak_me";
        store("agent-x", secret).unwrap();

        let path = tmp.path().join(TOKEN_SUBDIR).join("agent-x.token.json");
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            !on_disk.contains(secret),
            "encrypted file must not contain the plaintext token"
        );
        assert!(on_disk.contains("argon2id"), "expected the KDF marker");
    }

    #[cfg(unix)]
    #[test]
    fn test_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (tmp, _g0, _g1, _g2, _g3) = file_backend_env("pw");
        store("agent-x", "actl_pat_x").unwrap();
        let path = tmp.path().join(TOKEN_SUBDIR).join("agent-x.token.json");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token file must be 0600");
    }

    #[test]
    fn test_file_wrong_passphrase_fails_to_decrypt() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g_cfg_file = EnvGuard::remove("ACTUAL_CONFIG");
        let _g_cfg = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        let _g_store = EnvGuard::set(STORE_BACKEND_ENV, "file");

        let _g_pass = EnvGuard::set(PASSPHRASE_ENV, "right-passphrase");
        store("agent", "actl_pat_value").unwrap();
        drop(_g_pass);

        let _g_pass2 = EnvGuard::set(PASSPHRASE_ENV, "wrong-passphrase");
        let err = retrieve("agent").unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(ref m) if m.contains("decryption failed")),
            "expected a decryption failure, got: {err:?}"
        );
    }

    #[test]
    fn test_file_requires_passphrase() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g_cfg_file = EnvGuard::remove("ACTUAL_CONFIG");
        let _g_cfg = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        let _g_store = EnvGuard::set(STORE_BACKEND_ENV, "file");
        let _g_pass = EnvGuard::remove(PASSPHRASE_ENV);

        let err = store("agent", "actl_pat_value").unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(ref m) if m.contains(PASSPHRASE_ENV)),
            "expected a passphrase-required error, got: {err:?}"
        );
    }

    #[test]
    fn test_resolve_prefers_env_token() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_tmp, _g0, _g1, _g2, _g3) = file_backend_env("pw");
        // Store one value, then set ACTUAL_TOKEN to a different value: resolve
        // must return the env value, not the stored one.
        store("agent", "actl_pat_stored").unwrap();
        let _g_env = EnvGuard::set(ACTUAL_TOKEN_ENV, "actl_pat_from_env");
        assert_eq!(
            resolve("agent").unwrap().as_deref(),
            Some("actl_pat_from_env")
        );
    }

    #[test]
    fn test_resolve_falls_back_to_store_without_env() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let g_env = EnvGuard::remove(ACTUAL_TOKEN_ENV);
        let (_tmp, _g0, _g1, _g2, _g3) = file_backend_env("pw");
        store("agent", "actl_pat_stored").unwrap();
        assert_eq!(
            resolve("agent").unwrap().as_deref(),
            Some("actl_pat_stored")
        );
        drop(g_env);
    }

    #[test]
    fn test_retrieve_absent_is_none() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_tmp, _g0, _g1, _g2, _g3) = file_backend_env("pw");
        assert_eq!(retrieve("never-stored").unwrap(), None);
    }

    #[test]
    fn test_corrupted_file_errors() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (tmp, _g0, _g1, _g2, _g3) = file_backend_env("pw");
        let dir = tmp.path().join(TOKEN_SUBDIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.token.json"), "{not valid json").unwrap();
        let err = retrieve("agent").unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(ref m) if m.contains("corrupted")),
            "got: {err:?}"
        );
    }

    #[test]
    fn test_aad_binding_rejects_name_swap() {
        // A blob encrypted under one name must not decrypt under another, even
        // with the right passphrase (the name is bound as associated data).
        let blob = encrypt_token("agent-a", "actl_pat_v", b"pw").unwrap();
        let err = decrypt_token("agent-b", &blob, b"pw").unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(ref m) if m.contains("decryption failed")),
            "got: {err:?}"
        );
        // Sanity: the correct name decrypts.
        assert_eq!(
            decrypt_token("agent-a", &blob, b"pw").unwrap(),
            "actl_pat_v"
        );
    }

    #[test]
    fn test_validate_name() {
        assert!(validate_name("").is_err());
        assert!(validate_name("ok-name_1.2").is_ok());
        assert!(validate_name(&"x".repeat(MAX_NAME_LEN + 1)).is_err());
        assert!(validate_name("bad\nname").is_err());
    }

    #[test]
    fn test_sanitize_name() {
        assert_eq!(sanitize_name("ci/agent:1"), "ci_agent_1");
        assert_eq!(sanitize_name("keep.me-ok_9"), "keep.me-ok_9");
    }

    #[test]
    fn test_selected_mode_parsing() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard::set(STORE_BACKEND_ENV, "keyring");
        assert_eq!(selected_mode().unwrap(), Mode::Keyring);
        drop(_g);
        let _g = EnvGuard::set(STORE_BACKEND_ENV, "file");
        assert_eq!(selected_mode().unwrap(), Mode::File);
        drop(_g);
        let _g = EnvGuard::set(STORE_BACKEND_ENV, "auto");
        assert_eq!(selected_mode().unwrap(), Mode::Auto);
        drop(_g);
        let _g = EnvGuard::remove(STORE_BACKEND_ENV);
        assert_eq!(selected_mode().unwrap(), Mode::Auto);
        drop(_g);
        let _g = EnvGuard::set(STORE_BACKEND_ENV, "bogus");
        assert!(selected_mode().is_err());
    }

    #[test]
    fn test_backend_label() {
        assert_eq!(Backend::Keyring.label(), "OS keychain");
        assert_eq!(Backend::EncryptedFile.label(), "encrypted file");
    }

    #[test]
    fn test_encrypt_uses_fresh_salt_and_nonce() {
        // Two encryptions of the same token differ (random salt + nonce).
        let a = encrypt_token("n", "actl_pat_v", b"pw").unwrap();
        let b = encrypt_token("n", "actl_pat_v", b"pw").unwrap();
        assert_ne!(a.ciphertext, b.ciphertext);
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.salt, b.salt);
    }

    // ── Injectable in-memory keychain ─────────────────────────────────────────
    //
    // The real OS keychain cannot run deterministically under CI, so these tests
    // install a persistent in-memory credential store through keyring's public
    // `set_default_credential_builder` seam. It is keyed by (service, user) so a
    // store → retrieve → delete round-trips across the separate `keyring::Entry`
    // instances the backend creates, and it can be flipped to fail on demand to
    // drive the error and fallback arms. Every keyring-touching test below holds
    // `ENV_MUTEX` and calls `install_test_keyring()` first, so the process-global
    // builder and the shared maps never race with a sibling test.
    use keyring::credential::{Credential, CredentialApi, CredentialBuilderApi};
    use std::any::Any;
    use std::collections::HashMap;
    use std::sync::Mutex;

    static TEST_KEYRING: Mutex<Option<HashMap<(String, String), Vec<u8>>>> = Mutex::new(None);
    static TEST_KEYRING_FAIL: Mutex<bool> = Mutex::new(false);

    fn install_test_keyring() {
        *TEST_KEYRING.lock().unwrap() = Some(HashMap::new());
        *TEST_KEYRING_FAIL.lock().unwrap() = false;
        keyring::set_default_credential_builder(Box::new(TestKeyringBuilder));
    }

    /// Make every subsequent keychain op fail, simulating an unavailable keychain.
    fn fail_test_keyring() {
        *TEST_KEYRING_FAIL.lock().unwrap() = true;
    }

    struct TestKeyringBuilder;
    impl CredentialBuilderApi for TestKeyringBuilder {
        fn build(
            &self,
            _target: Option<&str>,
            service: &str,
            user: &str,
        ) -> keyring::Result<Box<Credential>> {
            Ok(Box::new(TestCredential {
                service: service.to_string(),
                user: user.to_string(),
            }))
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    struct TestCredential {
        service: String,
        user: String,
    }
    impl TestCredential {
        fn key(&self) -> (String, String) {
            (self.service.clone(), self.user.clone())
        }
    }
    impl CredentialApi for TestCredential {
        fn set_secret(&self, secret: &[u8]) -> keyring::Result<()> {
            if *TEST_KEYRING_FAIL.lock().unwrap() {
                return Err(keyring::Error::Invalid("test".into(), "unavailable".into()));
            }
            TEST_KEYRING
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .insert(self.key(), secret.to_vec());
            Ok(())
        }
        fn get_secret(&self) -> keyring::Result<Vec<u8>> {
            if *TEST_KEYRING_FAIL.lock().unwrap() {
                return Err(keyring::Error::Invalid("test".into(), "unavailable".into()));
            }
            match TEST_KEYRING
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .get(&self.key())
            {
                Some(v) => Ok(v.clone()),
                None => Err(keyring::Error::NoEntry),
            }
        }
        fn delete_credential(&self) -> keyring::Result<()> {
            if *TEST_KEYRING_FAIL.lock().unwrap() {
                return Err(keyring::Error::Invalid("test".into(), "unavailable".into()));
            }
            match TEST_KEYRING
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .remove(&self.key())
            {
                Some(_) => Ok(()),
                None => Err(keyring::Error::NoEntry),
            }
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[test]
    fn test_keyring_backend_roundtrip() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        install_test_keyring();
        let _g_env = EnvGuard::remove(ACTUAL_TOKEN_ENV);
        let _g_store = EnvGuard::set(STORE_BACKEND_ENV, "keyring");

        let backend = store("kr-agent", "actl_pat_kr").unwrap();
        assert_eq!(backend, Backend::Keyring);
        assert_eq!(
            retrieve("kr-agent").unwrap().as_deref(),
            Some("actl_pat_kr")
        );
        delete("kr-agent").unwrap();
        assert_eq!(retrieve("kr-agent").unwrap(), None);
    }

    #[test]
    fn test_keyring_read_error_surfaces() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        install_test_keyring();
        let _g_env = EnvGuard::remove(ACTUAL_TOKEN_ENV);
        let _g_store = EnvGuard::set(STORE_BACKEND_ENV, "keyring");
        fail_test_keyring();
        let err = retrieve("kr-agent").unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(ref m) if m.contains("keychain read failed")),
            "got: {err:?}"
        );
    }

    #[test]
    fn test_keyring_delete_error_surfaces() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        install_test_keyring();
        let _g_env = EnvGuard::remove(ACTUAL_TOKEN_ENV);
        let _g_store = EnvGuard::set(STORE_BACKEND_ENV, "keyring");
        fail_test_keyring();
        let err = delete("kr-agent").unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(ref m) if m.contains("keychain delete failed")),
            "got: {err:?}"
        );
    }

    #[test]
    fn test_auto_uses_keyring_when_available() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        install_test_keyring();
        let _g_env = EnvGuard::remove(ACTUAL_TOKEN_ENV);
        let _g_store = EnvGuard::set(STORE_BACKEND_ENV, "auto");

        // Keyring is available → the secret lands there, reads back, and a delete
        // clears it (after which retrieve falls through to the empty file).
        let backend = store("auto-ok", "actl_pat_auto").unwrap();
        assert_eq!(backend, Backend::Keyring);
        assert_eq!(
            retrieve("auto-ok").unwrap().as_deref(),
            Some("actl_pat_auto")
        );
        delete("auto-ok").unwrap();
        assert_eq!(retrieve("auto-ok").unwrap(), None);
    }

    #[test]
    fn test_auto_falls_back_to_file_when_keyring_unavailable() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        install_test_keyring();
        let _g_env = EnvGuard::remove(ACTUAL_TOKEN_ENV);
        let tmp = tempdir().unwrap();
        let _g_cfg_file = EnvGuard::remove("ACTUAL_CONFIG");
        let _g_cfg = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        let _g_store = EnvGuard::set(STORE_BACKEND_ENV, "auto");
        let _g_pass = EnvGuard::set(PASSPHRASE_ENV, "pw");
        fail_test_keyring();

        // Keyring write fails → encrypted-file fallback (passphrase configured).
        let backend = store("auto-fb", "actl_pat_fb").unwrap();
        assert_eq!(backend, Backend::EncryptedFile);
        // Read + delete also route past the failing keychain to the file.
        assert_eq!(retrieve("auto-fb").unwrap().as_deref(), Some("actl_pat_fb"));
        delete("auto-fb").unwrap();
        assert_eq!(retrieve("auto-fb").unwrap(), None);
    }

    #[test]
    fn test_auto_errors_when_keyring_unavailable_and_no_passphrase() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        install_test_keyring();
        let _g_env = EnvGuard::remove(ACTUAL_TOKEN_ENV);
        let tmp = tempdir().unwrap();
        let _g_cfg_file = EnvGuard::remove("ACTUAL_CONFIG");
        let _g_cfg = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        let _g_store = EnvGuard::set(STORE_BACKEND_ENV, "auto");
        let _g_pass = EnvGuard::remove(PASSPHRASE_ENV);
        fail_test_keyring();

        let err = store("auto-none", "actl_pat_x").unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(ref m) if m.contains("keychain is unavailable")),
            "got: {err:?}"
        );
    }

    #[test]
    fn test_decrypt_rejects_bad_nonce_length() {
        // A blob whose nonce decodes to the wrong length is rejected pre-decrypt.
        // "AAAA" is valid base64 for three zero bytes — a 3-byte nonce, not 24.
        let mut blob = encrypt_token("n", "actl_pat_v", b"pw").unwrap();
        blob.nonce = "AAAA".to_string();
        let err = decrypt_token("n", &blob, b"pw").unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(ref m) if m.contains("nonce")),
            "got: {err:?}"
        );
    }

    #[test]
    fn test_decrypt_rejects_short_salt() {
        // A valid nonce but a too-short salt (three bytes) is rejected pre-decrypt.
        let mut blob = encrypt_token("n", "actl_pat_v", b"pw").unwrap();
        blob.salt = "AAAA".to_string();
        let err = decrypt_token("n", &blob, b"pw").unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(ref m) if m.contains("salt")),
            "got: {err:?}"
        );
    }

    #[test]
    fn test_file_get_read_error_when_path_is_dir() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (tmp, _g0, _g1, _g2, _g3) = file_backend_env("pw");
        // Put a directory where the token file should be so read() fails with a
        // non-NotFound error.
        std::fs::create_dir_all(tmp.path().join(TOKEN_SUBDIR).join("dirtok.token.json")).unwrap();
        let err = retrieve("dirtok").unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(ref m) if m.contains("failed to read token file")),
            "got: {err:?}"
        );
    }

    #[test]
    fn test_file_get_rejects_oversize_file() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (tmp, _g0, _g1, _g2, _g3) = file_backend_env("pw");
        let dir = tmp.path().join(TOKEN_SUBDIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("big.token.json"),
            vec![b'x'; MAX_TOKEN_FILE_SIZE as usize + 1],
        )
        .unwrap();
        let err = retrieve("big").unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(ref m) if m.contains("too large")),
            "got: {err:?}"
        );
    }

    #[test]
    fn test_file_get_requires_passphrase_to_read() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g_cfg_file = EnvGuard::remove("ACTUAL_CONFIG");
        let _g_cfg = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        let _g_store = EnvGuard::set(STORE_BACKEND_ENV, "file");
        // Write the file with a passphrase, then read it back with none set.
        let g_pass = EnvGuard::set(PASSPHRASE_ENV, "pw");
        store("passless", "actl_pat_v").unwrap();
        drop(g_pass);
        let _g_nopass = EnvGuard::remove(PASSPHRASE_ENV);
        let err = retrieve("passless").unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(ref m) if m.contains(PASSPHRASE_ENV)),
            "got: {err:?}"
        );
    }

    #[test]
    fn test_file_delete_missing_is_ok() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_tmp, _g0, _g1, _g2, _g3) = file_backend_env("pw");
        // Deleting a token that was never stored is a no-op success.
        delete("never-stored").unwrap();
    }

    #[test]
    fn test_file_delete_error_when_path_is_dir() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (tmp, _g0, _g1, _g2, _g3) = file_backend_env("pw");
        // A directory at the token-file path makes remove_file fail non-NotFound.
        std::fs::create_dir_all(tmp.path().join(TOKEN_SUBDIR).join("dirdel.token.json")).unwrap();
        let err = delete("dirdel").unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(ref m) if m.contains("failed to remove token file")),
            "got: {err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_file_store_error_when_dir_cannot_be_created() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (tmp, _g0, _g1, _g2, _g3) = file_backend_env("pw");
        // A plain file where the tokens directory belongs makes create_dir_all fail.
        std::fs::write(tmp.path().join(TOKEN_SUBDIR), "x").unwrap();
        let err = store("blocked", "actl_pat_v").unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(ref m) if m.contains("failed to create token directory")),
            "got: {err:?}"
        );
    }

    #[test]
    fn test_keyring_delete_missing_is_ok() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        install_test_keyring();
        let _g_env = EnvGuard::remove(ACTUAL_TOKEN_ENV);
        let _g_store = EnvGuard::set(STORE_BACKEND_ENV, "keyring");
        // Deleting a credential that was never stored is a no-op success (the
        // keychain reports NoEntry, which the backend maps to Ok).
        delete("ghost").unwrap();
    }

    #[test]
    fn test_injected_keychain_as_any_hooks() {
        // The keyring credential traits require `as_any`; exercise both so the
        // injectable in-memory keychain is itself fully covered.
        assert!(TestKeyringBuilder
            .as_any()
            .downcast_ref::<TestKeyringBuilder>()
            .is_some());
        let cred = TestCredential {
            service: "s".to_string(),
            user: "u".to_string(),
        };
        assert!(cred.as_any().downcast_ref::<TestCredential>().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn test_write_secure_parentless_path_errors() {
        // A path whose parent is None (the filesystem root) skips the mkdir step;
        // the open then fails, covering the no-parent branch of write_secure.
        let err = write_secure(Path::new("/"), "x").unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(_)), "got: {err:?}");
    }
}
