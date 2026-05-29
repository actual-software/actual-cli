//! `actual whoami` — show the signed-in Actual AI platform identity.
//!
//! Reads the locally cached credentials (resolved at `login` time); makes no
//! network call. Errors with [`ActualError::NotLoggedIn`] when signed out.

use crate::auth::store::{self, StoredCredentials};
use crate::cli::ui::theme;
use crate::error::ActualError;

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
    println!(
        "{} Signed in to {}",
        theme::success(&theme::SUCCESS),
        theme::success("Actual AI")
    );
    println!("  {:<14} {}", "Organization:", creds.organization_id);
    if let Some(email) = &creds.email {
        println!("  {:<14} {}", "Account:", email);
    } else if let Some(subject) = &creds.subject {
        println!("  {:<14} {}", "Account:", subject);
    }
    println!("  {:<14} {}", "Member:", creds.member_id);
    if let Some(scope) = &creds.scope {
        println!("  {:<14} {}", "Scopes:", scope);
    }
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
}
