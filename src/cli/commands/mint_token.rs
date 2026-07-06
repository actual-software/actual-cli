//! `actual mint-token` — the fully-headless RFC 7523 jwt-bearer client.
//!
//! Signs a short-lived service-account assertion with a registered private key
//! and exchanges it for an access token, with **no browser and no human**. This
//! is the unattended agent path: contrast `login` (browser OAuth) and `login
//! --device` (a human approves a code), which enroll a *human-delegated*
//! session. Here the agent already holds a private key whose matching public
//! key is registered server-side.
//!
//! Output contract (mirrors `auth create-token` / the advisor command): the raw
//! access token is the **only** thing written to stdout, so
//! `TOKEN=$(actual mint-token …)` captures exactly the token. All status goes
//! to stderr. `--json` swaps stdout for the full token response as one JSON
//! line. The token never appears on stderr and is never logged.

use std::path::Path;

use crate::auth::jwt_bearer::{
    self, AssertionAlgorithm, AssertionParams, DEFAULT_ASSERTION_LIFETIME_SECONDS,
};
use crate::auth::oauth;
use crate::cli::args::MintTokenArgs;
use crate::cli::ui::theme;
use crate::error::ActualError;

/// Environment variable holding the private key PEM inline (used when no key
/// file is given). Preferred with a secret manager — keeps the key off argv.
const KEY_INLINE_ENV: &str = "ACTUAL_SERVICE_ACCOUNT_KEY";

pub fn exec(args: &MintTokenArgs) -> Result<(), ActualError> {
    // Resolve the issuer base URL (flag > ACTUAL_AUTH_URL > prod default) and
    // strip any trailing slash so URL/audience construction is stable.
    let base_url = oauth::resolve_auth_url(args.issuer.as_deref())?
        .trim_end_matches('/')
        .to_string();

    let key_pem = load_key_pem(
        args.key.as_deref(),
        std::env::var(KEY_INLINE_ENV).ok().as_deref(),
    )?;
    let algorithm = resolve_algorithm(args.alg.as_deref(), &key_pem)?;
    let audience = resolve_audience(args.aud.as_deref(), &base_url);
    let scope = resolve_scope(&args.scopes);

    let lifetime = if args.assertion_ttl_seconds == 0 {
        DEFAULT_ASSERTION_LIFETIME_SECONDS
    } else {
        args.assertion_ttl_seconds
    };

    let assertion = jwt_bearer::build_and_sign_assertion(
        &AssertionParams {
            service_account_id: args.service_account_id.trim(),
            audience: &audience,
            kid: args.kid.trim(),
            algorithm,
            lifetime_seconds: lifetime,
        },
        &key_pem,
        now_unix()?,
    )?;

    eprintln!(
        "{} minting a token via jwt-bearer ({}) as {}…",
        theme::hint("mint-token"),
        algorithm.as_str(),
        args.service_account_id.trim()
    );

    // The HTTPS transport guard lives in `build_http_client`: a non-HTTPS,
    // non-loopback issuer is rejected before any assertion is sent.
    let http = oauth::build_http_client(&base_url)?;
    let token = build_runtime()?.block_on(jwt_bearer::mint_token(
        &http,
        &base_url,
        &assertion,
        scope.as_deref(),
    ))?;

    // Token → stdout, ONLY the token (or the full JSON with --json).
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    jwt_bearer::write_token_output(&mut handle, &token, args.json)
        .map_err(|e| ActualError::InternalError(format!("failed to write token to stdout: {e}")))?;

    // Non-sensitive success summary → stderr. Never the token itself.
    let expiry = token
        .expires_in
        .map(|s| format!("expires in {s}s"))
        .unwrap_or_else(|| "no expiry reported".to_string());
    let scope_note = token
        .scope
        .as_deref()
        .map(|s| format!("scope: {s}"))
        .unwrap_or_else(|| "scope: (server default)".to_string());
    eprintln!(
        "{} minted token ({expiry}; {scope_note})",
        theme::success(&theme::SUCCESS)
    );

    Ok(())
}

/// Load the private key PEM. A key **file** (`--key` / its env) wins when given;
/// otherwise the `inline` PEM (from [`KEY_INLINE_ENV`]) is used. Errors cleanly
/// when neither is supplied or the file cannot be read.
fn load_key_pem(key_path: Option<&Path>, inline: Option<&str>) -> Result<Vec<u8>, ActualError> {
    if let Some(path) = key_path {
        return std::fs::read(path).map_err(|e| {
            ActualError::ConfigError(format!(
                "Failed to read the private key file '{}': {e}",
                path.display()
            ))
        });
    }
    if let Some(pem) = inline {
        if !pem.trim().is_empty() {
            return Ok(pem.as_bytes().to_vec());
        }
    }
    Err(ActualError::ConfigError(format!(
        "No private key provided. Pass --key <PATH>, or set {KEY_INLINE_ENV} \
         (the PEM contents) or ACTUAL_SERVICE_ACCOUNT_KEY_FILE (a path)."
    )))
}

/// Resolve the signing algorithm: the explicit `--alg` flag when present,
/// otherwise inferred from the key PEM.
fn resolve_algorithm(
    alg_flag: Option<&str>,
    key_pem: &[u8],
) -> Result<AssertionAlgorithm, ActualError> {
    match alg_flag {
        Some(value) => AssertionAlgorithm::parse(value),
        None => AssertionAlgorithm::infer_from_pem(key_pem),
    }
}

/// Resolve the assertion audience: the explicit `--aud` flag (trailing slash
/// trimmed) when present, otherwise the issuer base URL (which the server
/// accepts alongside `<issuer>/api/oauth/token`).
fn resolve_audience(aud_flag: Option<&str>, base_url: &str) -> String {
    match aud_flag {
        Some(value) => value.trim_end_matches('/').to_string(),
        None => base_url.to_string(),
    }
}

/// Join repeated `--scope` flags into a single space-delimited string, or `None`
/// when no scope was requested (the server then mints the principal's full
/// whitelist).
fn resolve_scope(scopes: &[String]) -> Option<String> {
    let joined = scopes
        .iter()
        .flat_map(|s| s.split_whitespace())
        .collect::<Vec<_>>()
        .join(" ");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

/// Current time in seconds since the Unix epoch.
fn now_unix() -> Result<u64, ActualError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|e| {
            ActualError::InternalError(format!("system clock is before the Unix epoch: {e}"))
        })
}

/// Build a single-threaded tokio runtime so the sync CLI dispatch path can drive
/// the async mint (mirrors `commands::login`).
fn build_runtime() -> Result<tokio::runtime::Runtime, ActualError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ActualError::InternalError(format!("failed to build tokio runtime: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_scope_joins_and_normalizes() {
        assert_eq!(resolve_scope(&[]), None);
        assert_eq!(
            resolve_scope(&["adr:query".to_string(), "adr:review".to_string()]),
            Some("adr:query adr:review".to_string())
        );
        // A single space-delimited value is also accepted and normalized.
        assert_eq!(
            resolve_scope(&["adr:query adr:review".to_string()]),
            Some("adr:query adr:review".to_string())
        );
        // Whitespace-only collapses to None.
        assert_eq!(resolve_scope(&["   ".to_string()]), None);
    }

    #[test]
    fn resolve_audience_defaults_to_issuer_and_trims() {
        assert_eq!(
            resolve_audience(None, "https://app.example.test"),
            "https://app.example.test"
        );
        assert_eq!(
            resolve_audience(
                Some("https://aud.example.test/"),
                "https://issuer.example.test"
            ),
            "https://aud.example.test"
        );
    }

    #[test]
    fn resolve_algorithm_honors_flag_then_infers() {
        // Explicit flag wins and rejects forbidden algorithms.
        assert_eq!(
            resolve_algorithm(Some("es256"), b"unused").unwrap(),
            AssertionAlgorithm::Es256
        );
        assert!(resolve_algorithm(Some("HS256"), b"unused").is_err());
        // With no flag and an unrecognizable key, inference errors cleanly.
        assert!(resolve_algorithm(None, b"not a pem").is_err());
    }

    #[test]
    fn load_key_pem_prefers_file_then_inline_then_errors() {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"-----BEGIN PRIVATE KEY-----\nFILE\n-----END PRIVATE KEY-----\n")
            .unwrap();

        // File wins over inline.
        let got = load_key_pem(Some(file.path()), Some("INLINE-PEM")).unwrap();
        assert!(String::from_utf8(got).unwrap().contains("FILE"));

        // Inline used when no file.
        let got = load_key_pem(None, Some("INLINE-PEM")).unwrap();
        assert_eq!(got, b"INLINE-PEM");

        // Neither → clean error.
        let err = load_key_pem(None, None).unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(ref m) if m.contains("No private key")));

        // Whitespace-only inline is treated as absent.
        let err = load_key_pem(None, Some("   ")).unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(_)));
    }

    #[test]
    fn load_key_pem_missing_file_is_a_clean_error() {
        let err = load_key_pem(Some(Path::new("/no/such/key.pem")), None).unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(ref m) if m.contains("Failed to read")));
    }

    #[test]
    fn build_runtime_succeeds() {
        assert!(build_runtime().is_ok());
    }

    #[test]
    fn now_unix_is_after_2020() {
        // 2020-01-01T00:00:00Z
        assert!(now_unix().unwrap() > 1_577_836_800);
    }
}
