//! `actual logout` — sign out of the Actual AI platform.
//!
//! Best-effort server-side revocation of the session (RFC 7009), then always
//! clears the local credentials — even if revocation fails (e.g. offline), so
//! the local machine is never left holding a token the user meant to drop.

use crate::auth::{oauth, store};
use crate::cli::ui::theme;
use crate::error::ActualError;

pub fn exec() -> Result<(), ActualError> {
    let Some(creds) = store::load()? else {
        println!("Not signed in to Actual AI.");
        return Ok(());
    };

    // Best-effort server-side revoke. A failure here must not block clearing
    // the local credentials.
    if creds.auth_url.is_some() {
        match build_runtime().and_then(|rt| rt.block_on(oauth::revoke(&creds))) {
            Ok(()) => {}
            Err(e) => {
                eprintln!(
                    "warning: could not revoke the server-side session ({e}); \
                     clearing local credentials anyway."
                );
            }
        }
    }

    store::delete()?;
    println!(
        "{} Signed out of Actual AI.",
        theme::success(&theme::SUCCESS)
    );
    Ok(())
}

/// Single-threaded tokio runtime for the async revoke call.
fn build_runtime() -> Result<tokio::runtime::Runtime, ActualError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ActualError::InternalError(format!("failed to build tokio runtime: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::store::StoredCredentials;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use tempfile::tempdir;

    #[test]
    fn test_logout_when_not_signed_in_is_ok() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        // No credentials present — should succeed quietly.
        assert!(exec().is_ok());
    }

    #[test]
    fn test_logout_clears_local_credentials() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        // auth_url = None => revoke is skipped (no network), but local creds
        // must still be removed.
        let creds = StoredCredentials {
            access_token: "secret".to_string(),
            refresh_token: "secret".to_string(),
            token_type: "Bearer".to_string(),
            expires_at: None,
            scope: None,
            organization_id: "org".to_string(),
            member_id: "member".to_string(),
            email: None,
            subject: None,
            auth_url: None,
        };
        store::save(&creds).unwrap();
        assert!(store::load().unwrap().is_some());

        exec().unwrap();
        assert!(
            store::load().unwrap().is_none(),
            "local credentials must be cleared after logout"
        );
    }
}
