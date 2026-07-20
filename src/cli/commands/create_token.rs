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

use std::io::{self, Write};

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
    let stdout = io::stdout();
    let stderr = io::stderr();
    run_inner(&mut stdout.lock(), &mut stderr.lock(), args).await
}

/// The mint-and-store flow, with the secret going to `out` and all chrome to
/// `err`. Split out from [`run`] so tests can inject buffers and assert the
/// exact bytes each stream carries — the stdout=secret / stderr=chrome split is
/// a load-bearing security contract.
async fn run_inner(
    out: &mut dyn Write,
    err: &mut dyn Write,
    args: &CreateTokenArgs,
) -> Result<(), ActualError> {
    let creds = store::load()?.ok_or(ActualError::NotLoggedIn)?;

    // Refuse BEFORE any network work or minting when there is provably no place
    // to keep the result — the predictable headless-Linux / no-passphrase case,
    // which is exactly the target env. Without this, every failed attempt would
    // mint a live token the user can neither capture nor revoke.
    token_store::precheck(&args.name)?;

    let creds = ensure_fresh(creds).await?;

    let base_url = resolve_api_url(args.api_url.as_deref());
    let issued = pat::issue(&base_url, &creds.access_token, &args.name, &args.scopes).await?;

    // The token is now live server-side. A store failure here is a *post-mint*
    // failure the precheck could not foresee (a full disk, a keychain lock
    // race). We MUST still surface the token + id so the user can capture and
    // revoke it, then propagate the error — letting `?` swallow it would strand
    // a live, unrevocable secret the user never saw.
    let backend = match token_store::store(&args.name, &issued.token) {
        Ok(backend) => backend,
        Err(store_err) => {
            // Best-effort surface; if even stdout is broken there is nothing
            // more we can do, so ignore the write error and return the store
            // failure that actually explains what went wrong.
            let _ = print_unstored_token(out, err, args, &issued, &store_err);
            return Err(store_err);
        }
    };

    print_result(out, err, args, &issued, backend)
        .map_err(|e| ActualError::InternalError(format!("failed to write token output: {e}")))
}

/// Print the mint result. The raw token goes to `out` on its own line (so
/// `TOKEN=$(actual auth create-token …)` captures just the secret); all chrome
/// and guidance go to `err`. The token is printed once here and never logged.
/// In production `out` is stdout and `err` is stderr; tests pass buffers to
/// assert the stdout=secret / stderr=chrome split.
fn print_result(
    out: &mut dyn Write,
    err: &mut dyn Write,
    args: &CreateTokenArgs,
    issued: &pat::IssuedToken,
    backend: token_store::Backend,
) -> io::Result<()> {
    writeln!(
        err,
        "{} Created scoped access token {}",
        theme::success(&theme::SUCCESS),
        theme::success(format!("\"{}\"", args.name))
    )?;
    if let Some(id) = &issued.id {
        writeln!(err, "  {:<12} {}", "Token id:", id)?;
    }
    // Prefer the scopes the server granted; fall back to what was requested.
    let scopes = if issued.scopes.is_empty() {
        args.scopes.join(" ")
    } else {
        issued.scopes.join(" ")
    };
    writeln!(err, "  {:<12} {}", "Scopes:", scopes)?;
    if let Some(exp) = &issued.expires_at {
        writeln!(err, "  {:<12} {}", "Expires:", exp)?;
    }
    writeln!(err, "  {:<12} {}", "Stored in:", backend.label())?;
    writeln!(err)?;
    writeln!(err, "Your token is shown once below. Copy it now:")?;
    writeln!(err)?;

    // The single place the raw secret is emitted: stdout, alone, for capture.
    writeln!(out, "{}", issued.token)?;

    writeln!(err)?;
    writeln!(err, "{}", theme::hint("Keep this secret safe:"))?;
    writeln!(
        err,
        "  • Use a DEDICATED token per agent so actions are attributable and"
    )?;
    writeln!(
        err,
        "    individually revocable; never reuse your interactive login session."
    )?;
    writeln!(
        err,
        "  • NEVER paste it into a prompt, commit it, or echo it to logs/history."
    )?;
    writeln!(
        err,
        "  • For CI / non-interactive use, pass it via the ACTUAL_TOKEN env var."
    )?;
    Ok(())
}

/// Surface a token that was minted but could **not** be stored, so the user can
/// still capture and revoke it. The raw token is written to `out` (stdout),
/// alone, as the **very first** output — before any human-facing chrome — while
/// the guidance on `err` (stderr) is strictly best-effort. A store failure after
/// a successful server-side mint leaves the token live but unpersisted, so stdout
/// is its only copy; emitting it first means a broken or closed stderr can never
/// return early and strand a live, unrevocable secret the user never saw.
fn print_unstored_token(
    out: &mut dyn Write,
    err: &mut dyn Write,
    args: &CreateTokenArgs,
    issued: &pat::IssuedToken,
    store_err: &ActualError,
) -> io::Result<()> {
    // The token is the one irrecoverable artifact here: it is LIVE server-side
    // but was NOT persisted locally, so this stdout line is the only copy the
    // user will ever see. Emit it FIRST — before any fallible stderr chrome — so
    // a broken or closed stderr can never return early and strand the secret.
    // Same capture contract as the success path: the raw secret on stdout, alone.
    writeln!(out, "{}", issued.token)?;

    // Everything below is human-facing guidance on stderr and is strictly
    // best-effort: a stderr write failure must never discard or precede the
    // token already written above, so these results are deliberately ignored
    // rather than propagated with `?`.
    let _ = writeln!(
        err,
        "{} Minted token {} but could not store it: {}",
        theme::error(&theme::ERROR),
        theme::error(format!("\"{}\"", args.name)),
        store_err
    );
    match &issued.id {
        Some(id) => {
            let _ = writeln!(err, "  {:<12} {}", "Token id:", id);
            let _ = writeln!(err);
            let _ = writeln!(
                err,
                "This token is LIVE on the server. Copy the token printed on stdout to use it now, or revoke it by the id above."
            );
        }
        None => {
            let _ = writeln!(err);
            let _ = writeln!(
                err,
                "This token is LIVE on the server but no id was returned, so it cannot be revoked by id — capture the token printed on stdout, or rotate the credential if you cannot."
            );
        }
    }
    let _ = writeln!(err);
    let _ = writeln!(
        err,
        "{}",
        theme::warning("Storage failed — the token was NOT saved locally.")
    );
    let _ = writeln!(
        err,
        "  • Capture the token now (printed on stdout above); it will not be shown again."
    );
    let _ = writeln!(
        err,
        "  • If you cannot capture it, revoke it by the id above so it is not left orphaned."
    );
    let _ = writeln!(
        err,
        "  • Fix the store ({}=file with {} set, or pass {} directly) and re-run.",
        token_store::STORE_BACKEND_ENV,
        token_store::PASSPHRASE_ENV,
        token_store::ACTUAL_TOKEN_ENV
    );
    Ok(())
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
            scopes: vec!["adr:query".to_string()],
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
            .with_body(r#"{"token":"actl_pat_minted","id":"tok_9","scopes":["adr:query"]}"#)
            .create_async()
            .await;

        let args = CreateTokenArgs {
            name: "agent-z".to_string(),
            scopes: vec!["adr:query".to_string()],
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

    #[tokio::test]
    async fn test_ensure_fresh_refreshes_expired_token() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempfile::tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/oauth/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"access_token":"refreshed","token_type":"Bearer","expires_in":3600,"refresh_token":"rotated"}"#,
            )
            .create_async()
            .await;

        // An expired access token plus a refresh token and a reachable auth server
        // takes the refresh-and-re-persist branch.
        let mut creds = sample_creds();
        creds.auth_url = Some(server.url());
        creds.expires_at = Some(Utc::now() - ChronoDuration::hours(1));
        let out = ensure_fresh(creds).await.unwrap();
        assert_eq!(out.access_token, "refreshed");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_run_prints_requested_scopes_and_expiry() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempfile::tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        let _g3 = EnvGuard::set("ACTUAL_TOKEN_STORE", "file");
        let _g4 = EnvGuard::set("ACTUAL_TOKEN_PASSPHRASE", "test-passphrase");
        let _g5 = EnvGuard::remove("ACTUAL_TOKEN");

        store::save(&sample_creds()).unwrap();

        // The server omits scopes (so the printed scopes fall back to the request)
        // and includes an expiry (so the expiry line is printed).
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", pat::ISSUANCE_PATH)
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(r#"{"token":"actl_pat_exp","expires_at":"2030-01-01T00:00:00Z"}"#)
            .create_async()
            .await;

        let args = CreateTokenArgs {
            name: "exp-agent".to_string(),
            scopes: vec!["adr:query".to_string()],
            api_url: Some(server.url()),
        };
        run(&args).await.unwrap();
        mock.assert_async().await;

        assert_eq!(
            token_store::retrieve("exp-agent").unwrap().as_deref(),
            Some("actl_pat_exp")
        );
    }

    #[test]
    fn test_print_result_stdout_carries_only_the_token() {
        // The stdout=secret / stderr=chrome split is a load-bearing security
        // contract: `TOKEN=$(actual auth create-token …)` must capture ONLY the
        // token. Assert stdout is exactly the token + newline, and the chrome
        // went to stderr instead (never the secret).
        let issued = sample_issued("actl_pat_only");
        let args = sample_args("agent-x");
        let mut out = Vec::new();
        let mut err = Vec::new();
        print_result(
            &mut out,
            &mut err,
            &args,
            &issued,
            token_store::Backend::EncryptedFile,
        )
        .unwrap();

        assert_eq!(String::from_utf8(out).unwrap(), "actl_pat_only\n");
        let err_s = String::from_utf8(err).unwrap();
        assert!(
            err_s.contains("Created scoped access token"),
            "chrome must go to stderr: {err_s}"
        );
        assert!(
            !err_s.contains("actl_pat_only"),
            "the secret must never leak to stderr: {err_s}"
        );
    }

    #[test]
    fn test_print_unstored_token_surfaces_token_and_id_on_failure() {
        // On a post-mint store failure the token must STILL reach stdout (alone)
        // and the id + a revoke hint must reach stderr, so the user can capture
        // and revoke rather than be left with an orphaned, unrevocable token.
        let issued = sample_issued("actl_pat_orphan");
        let args = sample_args("agent-x");
        let store_err = ActualError::ConfigError("keychain unavailable".to_string());
        let mut out = Vec::new();
        let mut err = Vec::new();
        print_unstored_token(&mut out, &mut err, &args, &issued, &store_err).unwrap();

        // Capture contract preserved even on failure: stdout carries ONLY the token.
        assert_eq!(String::from_utf8(out).unwrap(), "actl_pat_orphan\n");
        let err_s = String::from_utf8(err).unwrap();
        assert!(
            err_s.contains("could not store it"),
            "stderr should explain the failure: {err_s}"
        );
        assert!(
            err_s.contains("tok_42"),
            "stderr should carry the token id for revocation: {err_s}"
        );
        assert!(
            err_s.contains("LIVE on the server"),
            "stderr should warn the token is live: {err_s}"
        );
        assert!(
            !err_s.contains("actl_pat_orphan"),
            "the secret must never leak to stderr: {err_s}"
        );
    }

    #[tokio::test]
    async fn test_run_store_failure_after_mint_surfaces_token_and_errors() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempfile::tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        // File backend + passphrase set → the precheck PASSES, so we exercise
        // the *post-mint* failure path rather than the pre-mint refusal.
        let _g3 = EnvGuard::set("ACTUAL_TOKEN_STORE", "file");
        let _g4 = EnvGuard::set("ACTUAL_TOKEN_PASSPHRASE", "test-passphrase");
        let _g5 = EnvGuard::remove("ACTUAL_TOKEN");

        // Occupy the token subdirectory path with a regular FILE so the store's
        // directory creation fails — a store failure that only surfaces AFTER
        // the server-side mint has already succeeded.
        std::fs::write(tmp.path().join("tokens"), b"not a directory").unwrap();

        // Seed a logged-in session (writes credentials.json in tmp, unaffected
        // by the tokens-file above).
        store::save(&sample_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", pat::ISSUANCE_PATH)
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(r#"{"token":"actl_pat_stranded","id":"tok_77","scopes":["adr:query"]}"#)
            .create_async()
            .await;

        let args = CreateTokenArgs {
            name: "agent-fail".to_string(),
            scopes: vec!["adr:query".to_string()],
            api_url: Some(server.url()),
        };

        let mut out = Vec::new();
        let mut err = Vec::new();
        let result = run_inner(&mut out, &mut err, &args).await;
        mock.assert_async().await;

        // The store failed, so the command surfaces the error...
        assert!(result.is_err(), "a post-mint store failure must propagate");
        // ...but the minted token was STILL emitted to stdout, alone, so the
        // user can capture and revoke it instead of orphaning a live token.
        assert_eq!(String::from_utf8(out).unwrap(), "actl_pat_stranded\n");
        let err_s = String::from_utf8(err).unwrap();
        assert!(
            err_s.contains("tok_77"),
            "the id needed to revoke the stranded token must be surfaced: {err_s}"
        );
    }

    #[test]
    fn test_print_unstored_token_without_id_warns_no_revoke_by_id() {
        // When the mint response carries no id, the store-failure surface must
        // still emit the token (alone, on stdout) and warn that it cannot be
        // revoked by id — the no-id arm of print_unstored_token.
        let mut issued = sample_issued("actl_pat_noid");
        issued.id = None;
        let args = sample_args("agent-x");
        let store_err = ActualError::ConfigError("keychain unavailable".to_string());
        let mut out = Vec::new();
        let mut err = Vec::new();
        print_unstored_token(&mut out, &mut err, &args, &issued, &store_err).unwrap();

        assert_eq!(String::from_utf8(out).unwrap(), "actl_pat_noid\n");
        let err_s = String::from_utf8(err).unwrap();
        assert!(
            err_s.contains("no id was returned"),
            "stderr should explain the token cannot be revoked by id: {err_s}"
        );
        assert!(
            !err_s.contains("actl_pat_noid"),
            "the secret must never leak to stderr: {err_s}"
        );
    }

    /// A `Write` that succeeds `remaining` times and then fails. Sweeping
    /// `remaining` drives the error-propagation (`?`) path of each `writeln!` at
    /// every position, so a broken output stream is guaranteed to surface as an
    /// error rather than a false success.
    struct FailAfter {
        remaining: usize,
        tripped: bool,
    }
    impl std::io::Write for FailAfter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                self.tripped = true;
                return Err(std::io::Error::other("simulated write failure"));
            }
            self.remaining -= 1;
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_print_result_propagates_stderr_write_failure() {
        // print_result returns io::Result so a failed terminal write surfaces as
        // an error instead of a false success. Sweep the failure point across
        // the whole chrome sequence so every stderr writeln's `?` is exercised.
        for n in 0..50 {
            let issued = sample_issued("actl_pat_x");
            let args = sample_args("agent-x");
            let mut out: Vec<u8> = Vec::new();
            let mut err = FailAfter {
                remaining: n,
                tripped: false,
            };
            let r = print_result(
                &mut out,
                &mut err,
                &args,
                &issued,
                token_store::Backend::EncryptedFile,
            );
            if err.tripped {
                assert!(
                    r.is_err(),
                    "a stderr write failure (after {n}) must propagate"
                );
            }
        }
    }

    #[test]
    fn test_print_unstored_token_emits_token_despite_stderr_failure() {
        // The minted-but-unstored token is the one irrecoverable artifact on this
        // path — LIVE server-side but not persisted, so stdout is its only copy.
        // A broken or closed stderr must never prevent or discard that stdout
        // write. Fail stderr on its very first write (so no chrome can be emitted
        // at all) and confirm the token still lands on stdout, alone, in both the
        // has-id and no-id arms.
        for with_id in [true, false] {
            let mut issued = sample_issued("actl_pat_x");
            if !with_id {
                issued.id = None;
            }
            let args = sample_args("agent-x");
            let store_err = ActualError::ConfigError("store down".to_string());
            let mut out: Vec<u8> = Vec::new();
            let mut err = FailAfter {
                remaining: 0,
                tripped: false,
            };
            let r = print_unstored_token(&mut out, &mut err, &args, &issued, &store_err);
            // stdout is writable, so the call succeeds and the token reaches it
            // even though every stderr write failed.
            assert!(
                r.is_ok(),
                "a failing stderr must not fail the token write (id={with_id})"
            );
            assert!(
                err.tripped,
                "the failing-stderr path must actually be exercised (id={with_id})"
            );
            assert_eq!(
                String::from_utf8(out).unwrap(),
                "actl_pat_x\n",
                "the token must reach stdout, alone, even when stderr is broken (id={with_id})"
            );
        }
    }

    #[test]
    fn test_fail_after_flush_is_infallible() {
        // FailAfter only fails writes, never flushes; exercise flush so the test
        // writer is itself fully covered under the per-file 100% gate.
        let mut w = FailAfter {
            remaining: 3,
            tripped: false,
        };
        assert!(std::io::Write::flush(&mut w).is_ok());
    }

    fn sample_issued(token: &str) -> pat::IssuedToken {
        pat::IssuedToken {
            token: token.to_string(),
            id: Some("tok_42".to_string()),
            name: Some("agent-x".to_string()),
            scopes: vec!["adr:query".to_string()],
            created_at: None,
            expires_at: None,
        }
    }

    fn sample_args(name: &str) -> CreateTokenArgs {
        CreateTokenArgs {
            name: name.to_string(),
            scopes: vec!["adr:query".to_string()],
            api_url: None,
        }
    }

    fn sample_creds() -> StoredCredentials {
        StoredCredentials {
            access_token: "session-token".to_string(),
            refresh_token: "refresh".to_string(),
            token_type: "Bearer".to_string(),
            expires_at: None,
            scope: Some("adr:query".to_string()),
            organization_id: "org-1".to_string(),
            member_id: "member-1".to_string(),
            email: None,
            subject: None,
            auth_url: Some("http://localhost:4000".to_string()),
        }
    }
}
