//! `actual auth create-token` — mint a scoped personal access token (PAT).
//!
//! Builds on the existing `actual login` session: it loads the stored
//! credentials, refreshes the access token if it is near expiry, and calls the
//! authenticated issuance endpoint with that token as the bearer (no second
//! browser dance). The raw `actl_pat_…` is printed exactly once, on stdout, and
//! the CLI keeps no copy. A machine uses it from the `ACTUAL_TOKEN` env var.
//!
//! Refused while `ACTUAL_TOKEN` is set: a token can't mint another, and the
//! sign-in that can is the person's own `actual login`.

use std::io::{self, Write};

use crate::api::resolve_api_url;
use crate::auth::refresh::ensure_fresh;
use crate::auth::{actual_token, pat, store};
use crate::cli::args::CreateTokenArgs;
use crate::cli::ui::theme;
use crate::error::{sanitize_server_text, ActualError};

/// Why `create-token` won't run with `ACTUAL_TOKEN` set.
const REFUSED_WITH_ACTUAL_TOKEN: &str = "ACTUAL_TOKEN is set, so this computer can't create \
     another token with it. Run `actual auth create-token` on a computer signed in with \
     `actual login`, where ACTUAL_TOKEN isn't set.";

/// The step after creating a token, on stderr.
const NEXT_STEP: &str = "Add it in Amp Settings → Secrets & Env Vars → Personal as ACTUAL_TOKEN, \
     then start a new thread (or run `amp orb restart-processes`). Don't paste it into a chat.";

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

async fn run(args: &CreateTokenArgs) -> Result<(), ActualError> {
    let stdout = io::stdout();
    let stderr = io::stderr();
    run_inner(&mut stdout.lock(), &mut stderr.lock(), args).await
}

/// The mint flow, with the secret going to `out` and all chrome to `err`.
/// Split out from [`run`] so tests can inject buffers and assert the exact
/// bytes each stream carries — the stdout=secret / stderr=chrome split is a
/// load-bearing security contract.
async fn run_inner(
    out: &mut dyn Write,
    err: &mut dyn Write,
    args: &CreateTokenArgs,
) -> Result<(), ActualError> {
    if actual_token::token().is_some() {
        return Err(ActualError::ActualTokenRefused(
            REFUSED_WITH_ACTUAL_TOKEN.to_string(),
        ));
    }
    let creds = store::load()?.ok_or(ActualError::NotLoggedIn)?;
    let creds = ensure_fresh(creds).await?;

    let base_url = resolve_api_url(args.api_url.as_deref());
    let issued = pat::issue(&base_url, &creds.access_token, &args.name, &args.scopes).await?;

    print_result(out, err, &issued)
        .map_err(|e| ActualError::InternalError(format!("failed to write token output: {e}")))
}

/// Print the mint result. The raw token goes to `out` first, on its own line
/// (so `TOKEN=$(actual auth create-token …)` captures just the secret, and a
/// broken stderr can't keep it from the user); the organization, expiry and
/// next step go to `err`. The token is printed once here and never logged.
fn print_result(
    out: &mut dyn Write,
    err: &mut dyn Write,
    issued: &pat::IssuedToken,
) -> io::Result<()> {
    writeln!(out, "{}", issued.token)?;
    out.flush()?;

    let name = issued.name.as_deref().unwrap_or_default();
    writeln!(
        err,
        "{} Created access token {}",
        theme::success(&theme::SUCCESS),
        theme::success(format!("\"{}\"", sanitize_server_text(name)))
    )?;
    if let Some(org) = issued
        .organization_name
        .as_ref()
        .or(issued.organization_id.as_ref())
    {
        writeln!(
            err,
            "  {:<14} {}",
            "Organization:",
            sanitize_server_text(org)
        )?;
    }
    if let Some(exp) = &issued.expires_at {
        writeln!(err, "  {:<14} {}", "Expires:", sanitize_server_text(exp))?;
    }
    writeln!(err)?;
    writeln!(err, "{}", theme::hint(NEXT_STEP))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    // These async tests hold `ENV_MUTEX` (a std Mutex) across `.await` points on
    // purpose: it is an env-serialization latch held across await points by design,
    // serializing tests that mutate process-global env vars so they never race.
    #![allow(clippy::await_holding_lock)]
    use super::*;
    use crate::auth::store::StoredCredentials;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

    fn isolated() -> (tempfile::TempDir, Vec<EnvGuard>) {
        let home = tempfile::tempdir().unwrap();
        let guards = vec![
            EnvGuard::remove("ACTUAL_CONFIG"),
            EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap()),
            EnvGuard::remove(actual_token::ENV),
        ];
        (home, guards)
    }

    fn args(api_url: Option<String>) -> CreateTokenArgs {
        CreateTokenArgs {
            name: "amp-orbs".to_string(),
            scopes: vec!["adr:query".to_string(), "adr:review".to_string()],
            api_url,
        }
    }

    #[test]
    fn test_run_not_logged_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = isolated();
        let err = exec(&args(Some("http://127.0.0.1:1".to_string()))).unwrap_err();
        assert!(matches!(err, ActualError::NotLoggedIn), "got: {err:?}");
    }

    /// With ACTUAL_TOKEN set nothing is sent, even with a stored session.
    #[tokio::test]
    async fn test_refused_while_actual_token_is_set() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let mut server = mockito::Server::new_async().await;
        let (_home, _env) = actual_token::tests::with_token(&server.url());
        let mint = server
            .mock("POST", pat::ISSUANCE_PATH)
            .expect(0)
            .create_async()
            .await;
        store::save_to(&sample_creds(), &store::credentials_path().unwrap()).unwrap();

        let (mut out, mut err) = (Vec::new(), Vec::new());
        let e = run_inner(&mut out, &mut err, &args(Some(server.url())))
            .await
            .unwrap_err();

        assert_eq!(e.to_string(), REFUSED_WITH_ACTUAL_TOKEN);
        assert_eq!(e.exit_code(), 2);
        assert!(out.is_empty());
        mint.assert_async().await;
    }

    /// A mint posts to /v1/oauth/tokens with the session's bearer, puts only the
    /// token on stdout, the organization, expiry and next step on stderr, and
    /// writes no file holding it.
    #[tokio::test]
    async fn test_mints_prints_once_and_stores_nothing() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (home, _env) = isolated();
        store::save(&sample_creds()).unwrap();
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/oauth/tokens")
            .match_header("authorization", "Bearer session-token")
            .match_body(mockito::Matcher::PartialJsonString(
                r#"{"name":"amp-orbs","scopes":["adr:query","adr:review"]}"#.to_string(),
            ))
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"token":"actl_pat_minted","id":"tok_9","name":"amp-orbs","scopes":["adr:query","adr:review"],"organization_id":"org-1","organization_name":"Mockingbird\u001b[31m Maps","created_at":"2026-11-27T00:00:00Z","expires_at":"2027-02-25T00:00:00Z"}"#,
            )
            .create_async()
            .await;

        let (mut out, mut err) = (Vec::new(), Vec::new());
        run_inner(&mut out, &mut err, &args(Some(server.url())))
            .await
            .unwrap();

        mock.assert_async().await;
        assert_eq!(String::from_utf8(out).unwrap(), "actl_pat_minted\n");
        let err = console::strip_ansi_codes(&String::from_utf8(err).unwrap()).to_string();
        assert!(
            err.contains("Organization:  Mockingbird[31m Maps\n"),
            "{err}"
        );
        assert!(
            err.contains("Expires:       2027-02-25T00:00:00Z\n"),
            "{err}"
        );
        assert!(err.contains(NEXT_STEP), "{err}");
        assert!(!err.contains("actl_pat_minted"), "{err}");
        let files = glob::glob(&format!("{}/**/*", home.path().display())).unwrap();
        for entry in files.flatten() {
            let text = std::fs::read(&entry).unwrap_or_default();
            assert!(
                !String::from_utf8_lossy(&text).contains("actl_pat_minted"),
                "{entry:?} holds the token"
            );
        }
    }

    fn issued(organization_name: Option<&str>, expires_at: Option<&str>) -> pat::IssuedToken {
        pat::IssuedToken {
            token: "actl_pat_minted".to_string(),
            id: None,
            name: Some("amp-orbs".to_string()),
            scopes: Vec::new(),
            organization_id: None,
            organization_name: organization_name.map(str::to_string),
            created_at: None,
            expires_at: expires_at.map(str::to_string),
        }
    }

    /// Whichever stderr write fails, the token is already on stdout and the
    /// failure is reported. A stderr of `room` bytes fails once it's full.
    #[test]
    fn test_a_broken_stderr_still_leaves_the_token_on_stdout() {
        let issued = issued(Some("Mockingbird Maps"), Some("2027-02-25T00:00:00Z"));
        let mut failures = 0;
        for room in 0.. {
            let (mut out, mut buf) = (Vec::new(), vec![0u8; room]);
            let result = print_result(&mut out, &mut buf.as_mut_slice(), &issued);
            assert_eq!(String::from_utf8(out).unwrap(), "actl_pat_minted\n");
            if result.is_ok() {
                break;
            }
            failures += 1;
        }
        assert!(failures > 5, "{failures}");
    }

    /// Without an organization or an expiry, those lines are left out.
    #[test]
    fn test_print_result_without_organization_or_expiry() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        print_result(&mut out, &mut err, &issued(None, None)).unwrap();
        let err = console::strip_ansi_codes(&String::from_utf8(err).unwrap()).to_string();
        assert!(
            !err.contains("Organization:") && !err.contains("Expires:"),
            "{err}"
        );
        assert!(err.contains(NEXT_STEP), "{err}");
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
