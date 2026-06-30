//! `actual auth create-token` — mint a scoped personal access token (PAT).
//!
//! Builds on the existing `actual login` session: it loads the stored
//! credentials, refreshes the access token if it is near expiry, and calls the
//! authenticated issuance endpoint with that token as the bearer (no second
//! browser dance). The raw `actl_pat_…` is printed exactly once and otherwise
//! kept only behind the OS keychain (or the encrypted-file fallback). The token
//! is never written to a log.
//!
//! ## Security model for agents
//!
//! - Mint a **dedicated** token per agent (`--name <agent>`); never hand an
//!   agent the human's interactive login session. One PAT per agent makes
//!   actions attributable and lets a single agent be revoked on its own.
//! - The secret must live **only** in the keychain or the `ACTUAL_TOKEN`
//!   environment variable — never in an agent's prompt/conversation context,
//!   shell history, or logs.

use chrono::{Duration as ChronoDuration, Utc};

use crate::api::DEFAULT_API_URL;
use crate::auth::store::{self, StoredCredentials};
use crate::auth::{oauth, pat, token_store};
use crate::cli::args::CreateTokenArgs;
use crate::cli::ui::theme;
use crate::error::ActualError;

pub fn exec(args: &CreateTokenArgs) -> Result<(), ActualError> {
    build_runtime()?.block_on(run(args))
}

/// Single-threaded tokio runtime so the sync CLI dispatch path can drive the
/// async issuance flow (mirrors `commands::login` / `commands::advisor`).
fn build_runtime() -> Result<tokio::runtime::Runtime, ActualError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ActualError::InternalError(format!("failed to build tokio runtime: {e}")))
}

/// Resolve the issuance API base URL: the `--api-url` flag wins, then
/// `ACTUAL_API_URL`, else the api-service default. Mirrors how `advisor`
/// resolves its endpoint, so one env export steers both halves.
fn resolve_api_url(flag: Option<&str>) -> String {
    flag.map(|s| s.to_string())
        .or_else(|| {
            std::env::var("ACTUAL_API_URL")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_API_URL.to_string())
}

/// If the stored access token is expired (or about to be), refresh it with the
/// rotation primitive and re-persist. Mirrors `commands::advisor::ensure_fresh`;
/// a refresh failure surfaces as `NotLoggedIn` so the user re-runs
/// `actual login`.
async fn ensure_fresh(creds: StoredCredentials) -> Result<StoredCredentials, ActualError> {
    if creds.refresh_token.is_empty() {
        return Ok(creds);
    }
    if creds.expires_within(Utc::now(), ChronoDuration::seconds(60)) {
        let refreshed = oauth::refresh(&creds)
            .await
            .map_err(|_| ActualError::NotLoggedIn)?;
        store::save(&refreshed)?;
        return Ok(refreshed);
    }
    Ok(creds)
}

async fn run(args: &CreateTokenArgs) -> Result<(), ActualError> {
    let creds = store::load()?.ok_or(ActualError::NotLoggedIn)?;
    let creds = ensure_fresh(creds).await?;

    let base_url = resolve_api_url(args.api_url.as_deref());
    let issued = pat::issue(&base_url, &creds.access_token, &args.name, &args.scopes).await?;

    // Persist behind the keychain / encrypted-file fallback *before* printing,
    // so a storage failure surfaces rather than leaving the user believing a
    // token they only saw on screen is safely kept.
    let backend = token_store::store(&args.name, &issued.token)?;

    print_result(args, &issued, backend);
    Ok(())
}

/// Print the mint result. The raw token goes to **stdout** on its own line (so
/// `TOKEN=$(actual auth create-token …)` captures just the secret); all chrome
/// and guidance go to **stderr**. The token is printed once here and never
/// logged.
fn print_result(args: &CreateTokenArgs, issued: &pat::IssuedToken, backend: token_store::Backend) {
    eprintln!(
        "{} Created scoped access token {}",
        theme::success(&theme::SUCCESS),
        theme::success(format!("\"{}\"", args.name))
    );
    if let Some(id) = &issued.id {
        eprintln!("  {:<12} {}", "Token id:", id);
    }
    // Prefer the scopes the server granted; fall back to what was requested.
    let scopes = if issued.scopes.is_empty() {
        args.scopes.join(" ")
    } else {
        issued.scopes.join(" ")
    };
    eprintln!("  {:<12} {}", "Scopes:", scopes);
    if let Some(exp) = &issued.expires_at {
        eprintln!("  {:<12} {}", "Expires:", exp);
    }
    eprintln!("  {:<12} {}", "Stored in:", backend.label());
    eprintln!();
    eprintln!("Your token is shown once below. Copy it now:");
    eprintln!();

    // The single place the raw secret is emitted: stdout, alone, for capture.
    println!("{}", issued.token);

    eprintln!();
    eprintln!("{}", theme::hint("Keep this secret safe:"));
    eprintln!("  • Use a DEDICATED token per agent so actions are attributable and");
    eprintln!("    individually revocable; never reuse your interactive login session.");
    eprintln!("  • NEVER paste it into a prompt, commit it, or echo it to logs/history.");
    eprintln!("  • For CI / non-interactive use, pass it via the ACTUAL_TOKEN env var.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

    #[test]
    fn test_resolve_api_url_flag_wins() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard::set("ACTUAL_API_URL", "http://env-url");
        assert_eq!(resolve_api_url(Some("http://flag-url")), "http://flag-url");
    }

    #[test]
    fn test_resolve_api_url_env_fallback() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard::set("ACTUAL_API_URL", "http://env-url");
        assert_eq!(resolve_api_url(None), "http://env-url");
    }

    #[test]
    fn test_resolve_api_url_default() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard::remove("ACTUAL_API_URL");
        assert_eq!(resolve_api_url(None), DEFAULT_API_URL);
    }

    #[test]
    fn test_resolve_api_url_empty_env_is_ignored() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard::set("ACTUAL_API_URL", "");
        assert_eq!(resolve_api_url(None), DEFAULT_API_URL);
    }

    #[tokio::test]
    async fn test_ensure_fresh_no_refresh_token_unchanged() {
        let mut creds = sample_creds();
        creds.refresh_token = String::new();
        let out = ensure_fresh(creds.clone()).await.unwrap();
        assert_eq!(out.access_token, creds.access_token);
    }

    #[tokio::test]
    async fn test_ensure_fresh_not_expired_unchanged() {
        let mut creds = sample_creds();
        creds.expires_at = Some(Utc::now() + ChronoDuration::hours(1));
        let out = ensure_fresh(creds).await.unwrap();
        assert_eq!(out.access_token, "session-token");
    }

    #[test]
    fn test_run_not_logged_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempfile::tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        let args = CreateTokenArgs {
            name: "agent".to_string(),
            scopes: vec!["adr.read".to_string()],
            api_url: Some("http://127.0.0.1:1".to_string()),
        };
        // Not signed in → NotLoggedIn before any network work.
        let err = exec(&args).unwrap_err();
        assert!(matches!(err, ActualError::NotLoggedIn), "got: {err:?}");
    }

    #[tokio::test]
    async fn test_run_happy_path_mints_and_stores() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempfile::tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        // Force the file backend so the test never touches the real keychain.
        let _g3 = EnvGuard::set("ACTUAL_TOKEN_STORE", "file");
        let _g4 = EnvGuard::set("ACTUAL_TOKEN_PASSPHRASE", "test-passphrase");
        let _g5 = EnvGuard::remove("ACTUAL_TOKEN");

        // Seed a logged-in session (no expiry → no refresh path).
        store::save(&sample_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", pat::ISSUANCE_PATH)
            .match_header("authorization", "Bearer session-token")
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(r#"{"token":"actl_pat_minted","id":"tok_9","scopes":["adr.read"]}"#)
            .create_async()
            .await;

        let args = CreateTokenArgs {
            name: "agent-z".to_string(),
            scopes: vec!["adr.read".to_string()],
            api_url: Some(server.url()),
        };
        run(&args).await.unwrap();
        mock.assert_async().await;

        // The minted token landed in the (encrypted-file) store, retrievable.
        assert_eq!(
            token_store::retrieve("agent-z").unwrap().as_deref(),
            Some("actl_pat_minted")
        );
    }

    fn sample_creds() -> StoredCredentials {
        StoredCredentials {
            access_token: "session-token".to_string(),
            refresh_token: "refresh".to_string(),
            token_type: "Bearer".to_string(),
            expires_at: None,
            scope: Some("adr.read".to_string()),
            organization_id: "org-1".to_string(),
            member_id: "member-1".to_string(),
            email: None,
            subject: None,
            auth_url: Some("http://localhost:4000".to_string()),
        }
    }
}
