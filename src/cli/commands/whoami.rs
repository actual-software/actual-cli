//! `actual whoami` — show the signed-in Actual AI platform identity.
//!
//! Reads the locally cached credentials (resolved at `login` time); makes no
//! network call. Errors with [`ActualError::NotLoggedIn`] when signed out.
//!
//! `whoami --check` also asks the auth server whether the sign-in still
//! works, renewing the access token first when it is about to expire, and
//! errors with `NotLoggedIn` when the server no longer accepts it.
//!
//! With `ACTUAL_TOKEN` set, both show the token's sign-in instead: `whoami`
//! from the identity cache, `whoami --check` from asking Actual's API now.

use chrono::{DateTime, Utc};

use crate::auth::store::{self, StoredCredentials};
use crate::auth::{actual_token, oauth, refresh};
use crate::cli::args::WhoamiArgs;
use crate::cli::ui::theme;
use crate::error::{sanitize_server_text, ActualError};

pub fn exec(args: &WhoamiArgs) -> Result<(), ActualError> {
    if let Some(token) = actual_token::token() {
        let identity = if args.check {
            actual_token::check(&token)?
        } else {
            actual_token::identity(&token)?
        };
        print!("{}", format_token_identity(&token, identity));
        return Ok(());
    }
    let creds = store::load()?.ok_or(ActualError::NotLoggedIn)?;
    if args.check {
        let creds = build_runtime()?.block_on(check(creds))?;
        print!("{}", format_checked(&creds, Utc::now()));
    } else {
        print_identity(&creds);
    }
    Ok(())
}

/// Single-threaded tokio runtime so the sync CLI dispatch path can drive the
/// async check (mirrors `commands::create_token`).
fn build_runtime() -> Result<tokio::runtime::Runtime, ActualError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ActualError::InternalError(format!("failed to build tokio runtime: {e}")))
}

/// `creds` once the auth server that issued it has accepted it, renewed and
/// saved first when its access token expires within a minute.
async fn check(creds: StoredCredentials) -> Result<StoredCredentials, ActualError> {
    let creds = refresh::ensure_fresh(creds).await?;
    oauth::verify(&creds).await?;
    Ok(creds)
}

/// The identity as `whoami --check` prints it at `now`: `whoami`'s rows, then
/// the auth server that accepted the sign-in.
fn format_checked(creds: &StoredCredentials, now: DateTime<Utc>) -> String {
    let mut text = format_identity(creds, now);
    if let Some(auth_url) = &creds.auth_url {
        text.push_str(&server_row("Checked with:", auth_url));
    }
    text
}

/// Print the cached identity. **Never prints token material.**
fn print_identity(creds: &StoredCredentials) {
    print!("{}", format_identity(creds, Utc::now()));
}

/// The identity as `print_identity` prints it at `now`. The auth server
/// supplied every field except the expiry, so control characters are dropped
/// from each of them.
fn format_identity(creds: &StoredCredentials, now: DateTime<Utc>) -> String {
    let header = format!(
        "{} Signed in to {}\n",
        theme::success(&theme::SUCCESS),
        theme::success("Actual AI")
    );
    header + &identity_rows(creds, now)
}

/// `ACTUAL_TOKEN`'s sign-in as `whoami` prints it: the token's name, the rows
/// of [`format_identity`], and the token's expiry.
fn format_token_identity(token: &str, identity: actual_token::Identity) -> String {
    let name = sanitize_server_text(&identity.name);
    let expires = identity.expires_at.format("%Y-%m-%d %H:%M UTC").to_string();
    let creds = actual_token::to_credentials(token, identity);
    format!(
        "{} Signed in to {} with ACTUAL_TOKEN ({name})\n{}  {:<14} {expires}\n",
        theme::success(&theme::SUCCESS),
        theme::success("Actual AI"),
        identity_rows(&creds, Utc::now()),
        "Expires:"
    )
}

/// The rows under `whoami`'s header.
fn identity_rows(creds: &StoredCredentials, now: DateTime<Utc>) -> String {
    let mut text = String::new();
    text.push_str(&server_row("Organization:", &creds.organization_id));
    if let Some(email) = &creds.email {
        text.push_str(&server_row("Account:", email));
    } else if let Some(subject) = &creds.subject {
        text.push_str(&server_row("Account:", subject));
    }
    text.push_str(&server_row("Member:", &creds.member_id));
    if let Some(scope) = &creds.scope {
        text.push_str(&server_row("Scopes:", scope));
    }
    if let Some(expired) = expired_note(creds, now) {
        text.push_str(&format!(
            "  {:<14} {}\n",
            "Access token:",
            theme::warning(expired)
        ));
    }
    text
}

/// One `label  value` row whose value the auth server supplied.
pub(crate) fn server_row(label: &str, value: &str) -> String {
    format!("  {label:<14} {}\n", sanitize_server_text(value))
}

/// `"expired <when>"` once the access token's expiry has passed at `now`, or
/// `None` while it is valid or the session records no expiry.
pub(crate) fn expired_note(creds: &StoredCredentials, now: DateTime<Utc>) -> Option<String> {
    let expires_at = creds.expires_at.filter(|_| creds.is_expired(now))?;
    Some(format!(
        "expired {}",
        expires_at.format("%Y-%m-%d %H:%M UTC")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use tempfile::tempdir;

    fn creds() -> StoredCredentials {
        StoredCredentials {
            access_token: "secret".to_string(),
            refresh_token: "secret".to_string(),
            token_type: "Bearer".to_string(),
            expires_at: None,
            scope: Some("openid adr:query".to_string()),
            organization_id: "mock-org-0001".to_string(),
            member_id: "mock-member-0001".to_string(),
            email: Some("dev@example.com".to_string()),
            subject: Some("mock-user-0001".to_string()),
            auth_url: Some("http://localhost:4000".to_string()),
        }
    }

    #[test]
    fn test_whoami_logged_out_errors() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        let _no_token = EnvGuard::remove("ACTUAL_TOKEN");

        let err = exec(&WhoamiArgs { check: false }).unwrap_err();
        assert!(matches!(err, ActualError::NotLoggedIn));
    }

    #[test]
    fn test_whoami_logged_in_ok() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        let _no_token = EnvGuard::remove("ACTUAL_TOKEN");

        store::save(&creds()).unwrap();
        assert!(exec(&WhoamiArgs { check: false }).is_ok());
    }

    #[test]
    fn test_print_identity_does_not_panic_minimal() {
        let mut c = creds();
        c.email = None;
        c.subject = None;
        c.scope = None;
        print_identity(&c);
    }

    #[test]
    fn test_print_identity_falls_back_to_subject() {
        // email absent + subject present → exercises the subject fallback branch.
        let mut c = creds();
        c.email = None;
        c.subject = Some("sub-123".to_string());
        print_identity(&c);
    }

    #[test]
    fn test_format_identity_drops_control_characters_from_server_fields() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        let _no_token = EnvGuard::remove("ACTUAL_TOKEN");

        // The fields as the auth server's JSON sent them, read back from the
        // credentials file the way `exec` reads them.
        let signed_in: StoredCredentials = serde_json::from_str(
            r#"{"access_token":"secret","refresh_token":"secret","organization_id":"\u001b[31morg\u0007-1","member_id":"member\u007f-1","email":"\u001b]0;pwned\u0007dev@example.com","subject":"unused","scope":"openid\u0007 adr:query"}"#,
        )
        .unwrap();
        store::save(&signed_in).unwrap();
        let mut creds = store::load().unwrap().unwrap();
        let header = format!(
            "{} Signed in to {}\n",
            theme::success(&theme::SUCCESS),
            theme::success("Actual AI")
        );
        assert_eq!(
            format_identity(&creds, Utc::now()),
            format!(
                "{header}  Organization:  [31morg-1\n  Account:       ]0;pwneddev@example.com\n  \
                 Member:        member-1\n  Scopes:        openid adr:query\n"
            )
        );

        creds.email = None;
        creds.subject = Some("\u{1b}[2Juser\u{7f}-1".to_string());
        assert!(
            format_identity(&creds, Utc::now()).contains("\n  Account:       [2Juser-1\n"),
            "the subject fallback is filtered too"
        );
    }

    #[test]
    fn test_print_identity_with_expired_access_token() {
        let mut c = creds();
        c.expires_at = Some(Utc::now() - chrono::Duration::hours(1));
        print_identity(&c);
    }

    #[test]
    fn test_expired_note_only_once_the_expiry_has_passed() {
        let now = DateTime::parse_from_rfc3339("2026-10-23T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut c = creds();

        c.expires_at = Some(now - chrono::Duration::minutes(90));
        assert_eq!(
            expired_note(&c, now).as_deref(),
            Some("expired 2026-10-23 10:30 UTC")
        );

        c.expires_at = Some(now + chrono::Duration::minutes(5));
        assert_eq!(expired_note(&c, now), None);

        c.expires_at = None;
        assert_eq!(expired_note(&c, now), None);
    }

    // ---- whoami --check ----

    const CHECK: WhoamiArgs = WhoamiArgs { check: true };

    const IDENTITY: &str = r#"{"sub":"mock-user-0001","organization_id":"mock-org-0001","member_id":"mock-member-0001","scope":"openid adr:query"}"#;

    /// A config dir of the test's own, so nothing here reads, renews or locks
    /// the machine's real sign-in.
    fn isolated() -> (tempfile::TempDir, Vec<EnvGuard>) {
        let home = tempdir().unwrap();
        let guards = vec![
            EnvGuard::remove("ACTUAL_CONFIG"),
            EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap()),
            EnvGuard::remove("ACTUAL_TOKEN"),
        ];
        (home, guards)
    }

    /// A sign-in issued by `auth_url`, whose access token expires in `expires_in`.
    fn issued_by(
        auth_url: String,
        access_token: &str,
        expires_in: chrono::Duration,
    ) -> StoredCredentials {
        StoredCredentials {
            access_token: access_token.to_string(),
            refresh_token: "old-rt".to_string(),
            expires_at: Some(Utc::now() + expires_in),
            auth_url: Some(auth_url),
            ..creds()
        }
    }

    #[test]
    fn test_whoami_check_logged_out_errors() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = isolated();

        let err = exec(&CHECK).unwrap_err();
        assert!(matches!(err, ActualError::NotLoggedIn), "got: {err:?}");
    }

    /// A token that isn't due for renewal goes to the server that issued it,
    /// and nothing is renewed or rewritten.
    #[test]
    fn test_whoami_check_asks_the_server_that_issued_the_sign_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = isolated();
        let mut server = mockito::Server::new();
        let token = server.mock("POST", "/api/oauth/token").expect(0).create();
        let whoami = server
            .mock("GET", "/whoami")
            .match_header("authorization", "Bearer current-at")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(IDENTITY)
            .expect(1)
            .create();
        let signed_in = issued_by(server.url(), "current-at", chrono::Duration::hours(1));
        store::save(&signed_in).unwrap();

        exec(&CHECK).unwrap();

        whoami.assert();
        token.assert();
        assert_eq!(store::load().unwrap(), Some(signed_in));
    }

    /// A token about to expire is renewed and saved first, and the server is
    /// asked with the new one.
    #[test]
    fn test_whoami_check_renews_an_expiring_token_first() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = isolated();
        let mut server = mockito::Server::new();
        let token = server
            .mock("POST", "/api/oauth/token")
            .match_body(mockito::Matcher::UrlEncoded(
                "refresh_token".into(),
                "old-rt".into(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"access_token":"new-at","token_type":"Bearer","expires_in":3600,"refresh_token":"new-rt"}"#,
            )
            .expect(1)
            .create();
        let whoami = server
            .mock("GET", "/whoami")
            .match_header("authorization", "Bearer new-at")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(IDENTITY)
            .expect(1)
            .create();
        store::save(&issued_by(
            server.url(),
            "old-at",
            chrono::Duration::seconds(30),
        ))
        .unwrap();

        exec(&CHECK).unwrap();

        token.assert();
        whoami.assert();
        let saved = store::load().unwrap().unwrap();
        assert_eq!(saved.access_token, "new-at");
        assert_eq!(saved.refresh_token, "new-rt");
    }

    /// Signed out elsewhere, or revoked: the server refuses the token, so the
    /// check fails as signed out (exit 2, pointing at `actual login`) and the
    /// stored sign-in stays as it was.
    #[test]
    fn test_whoami_check_with_a_token_the_server_refuses_is_not_logged_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = isolated();
        let mut server = mockito::Server::new();
        let _whoami = server
            .mock("GET", "/whoami")
            .with_status(401)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":"invalid_token"}"#)
            .create();
        let signed_in = issued_by(server.url(), "revoked-at", chrono::Duration::hours(1));
        store::save(&signed_in).unwrap();

        let err = exec(&CHECK).unwrap_err();

        assert!(matches!(err, ActualError::NotLoggedIn), "got: {err:?}");
        assert_eq!(err.exit_code(), 2);
        assert_eq!(err.hint().as_deref(), Some("actual login"));
        assert_eq!(store::load().unwrap(), Some(signed_in));
    }

    /// Lapsed: the server refuses the refresh token, so the check fails as
    /// signed out without asking `/whoami`.
    #[test]
    fn test_whoami_check_with_a_refused_refresh_token_is_not_logged_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = isolated();
        let mut server = mockito::Server::new();
        let _token = server
            .mock("POST", "/api/oauth/token")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":"invalid_grant"}"#)
            .create();
        let whoami = server.mock("GET", "/whoami").expect(0).create();
        store::save(&issued_by(
            server.url(),
            "old-at",
            chrono::Duration::hours(-1),
        ))
        .unwrap();

        let err = exec(&CHECK).unwrap_err();

        assert!(matches!(err, ActualError::NotLoggedIn), "got: {err:?}");
        whoami.assert();
    }

    /// An outage is reported as itself (exit 3), not as a sign-out.
    #[test]
    fn test_whoami_check_in_an_auth_server_outage_is_not_signed_out() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = isolated();
        let mut server = mockito::Server::new();
        let _whoami = server
            .mock("GET", "/whoami")
            .with_status(503)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":"temporarily_unavailable"}"#)
            .create();
        store::save(&issued_by(
            server.url(),
            "current-at",
            chrono::Duration::hours(1),
        ))
        .unwrap();

        let err = exec(&CHECK).unwrap_err();

        assert!(matches!(err, ActualError::ApiError(_)), "got: {err:?}");
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn test_format_checked_names_the_server_that_accepted_the_sign_in() {
        let now = Utc::now();
        let mut c = creds();
        assert_eq!(
            format_checked(&c, now),
            format!(
                "{}  Checked with:  http://localhost:4000\n",
                format_identity(&c, now)
            )
        );

        c.auth_url = None;
        assert_eq!(format_checked(&c, now), format_identity(&c, now));
    }

    /// With ACTUAL_TOKEN set, `whoami --check` asks Actual's API even with a
    /// fresh cache, a 401 exits 2 with the ACTUAL_TOKEN message, and the
    /// stored `actual login` session is never consulted.
    #[test]
    fn test_whoami_check_with_actual_token_asks_now() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let mut server = mockito::Server::new();
        let (_home, _env) = crate::auth::actual_token::tests::with_token(&server.url());
        let ok = server
            .mock("GET", crate::auth::actual_token::CURRENT_PATH)
            .with_status(200)
            .with_body(crate::auth::actual_token::tests::identity_json(
                Utc::now() + chrono::Duration::days(90),
            ))
            .expect(2)
            .create();
        exec(&WhoamiArgs { check: false }).unwrap();
        exec(&CHECK).unwrap();
        ok.assert();
        ok.remove();
        let refused = server
            .mock("GET", crate::auth::actual_token::CURRENT_PATH)
            .with_status(401)
            .expect(1)
            .create();

        let err = exec(&CHECK).unwrap_err();

        refused.assert();
        assert_eq!(err.exit_code(), 2);
        assert_eq!(err.to_string(), crate::auth::actual_token::REJECTED);
    }

    #[test]
    fn test_format_token_identity_names_the_token_and_its_expiry() {
        let identity: crate::auth::actual_token::Identity =
            serde_json::from_str(&crate::auth::actual_token::tests::identity_json(
                DateTime::parse_from_rfc3339("2027-02-25T09:30:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ))
            .unwrap();
        let text = format_token_identity("actl_pat_x", identity);
        let header = format!(
            "{} Signed in to {} with ACTUAL_TOKEN (amp-orbs)\n",
            theme::success(&theme::SUCCESS),
            theme::success("Actual AI")
        );
        assert_eq!(
            text,
            format!(
                "{header}  Organization:  org-pat\n  Account:       dev@example.com\n  \
                 Member:        member-pat\n  Scopes:        adr:query adr:review\n  \
                 Expires:       2027-02-25 09:30 UTC\n"
            )
        );
        assert!(!text.contains("actl_pat_x"));
    }
}
