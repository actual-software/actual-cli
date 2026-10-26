//! `actual whoami` — show the signed-in Actual AI platform identity.
//!
//! Reads the locally cached credentials (resolved at `login` time); makes no
//! network call. Errors with [`ActualError::NotLoggedIn`] when signed out.

use chrono::{DateTime, Utc};

use crate::auth::store::{self, StoredCredentials};
use crate::cli::ui::theme;
use crate::error::{sanitize_server_text, ActualError};

pub fn exec() -> Result<(), ActualError> {
    match store::load()? {
        Some(creds) => {
            print_identity(&creds);
            Ok(())
        }
        None => Err(ActualError::NotLoggedIn),
    }
}

/// Print the cached identity. **Never prints token material.**
fn print_identity(creds: &StoredCredentials) {
    print!("{}", format_identity(creds, Utc::now()));
}

/// The identity as `print_identity` prints it at `now`. The auth server
/// supplied every field except the expiry, so control characters are dropped
/// from each of them.
fn format_identity(creds: &StoredCredentials, now: DateTime<Utc>) -> String {
    let mut text = format!(
        "{} Signed in to {}\n",
        theme::success(&theme::SUCCESS),
        theme::success("Actual AI")
    );
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

        let err = exec().unwrap_err();
        assert!(matches!(err, ActualError::NotLoggedIn));
    }

    #[test]
    fn test_whoami_logged_in_ok() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        store::save(&creds()).unwrap();
        assert!(exec().is_ok());
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
}
