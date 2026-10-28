//! `actual logout` — sign out of the Actual AI platform.
//!
//! Best-effort server-side revocation of the session (RFC 7009), then always
//! clears the local credentials — even if revocation fails (e.g. offline), so
//! the local machine is never left holding a token the user meant to drop.
//!
//! Both happen under the stored sign-in's lock, so a refresh that would rotate
//! the session waits for the sign-out rather than slipping between the revoke
//! and the delete, which would leave the rotated session live.

use crate::auth::{oauth, store};
use crate::cli::ui::theme;
use crate::error::ActualError;

pub fn exec() -> Result<(), ActualError> {
    let lock = store::lock()?;
    let Some(creds) = lock.load()? else {
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

    lock.delete()?;
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

    fn creds_with(auth_url: Option<String>) -> StoredCredentials {
        StoredCredentials {
            access_token: "a".to_string(),
            refresh_token: "r".to_string(),
            token_type: "Bearer".to_string(),
            expires_at: None,
            scope: None,
            organization_id: "org".to_string(),
            member_id: "m".to_string(),
            email: None,
            subject: None,
            auth_url,
        }
    }

    #[test]
    fn test_logout_revokes_then_clears() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        let mut server = mockito::Server::new();
        let m = server
            .mock("POST", "/api/oauth/revoke")
            .with_status(200)
            .create();

        store::save(&creds_with(Some(server.url()))).unwrap();
        exec().unwrap();
        m.assert();
        assert!(store::load().unwrap().is_none());
    }

    #[test]
    fn test_logout_revoke_failure_still_clears() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        // Unreachable auth server → revoke fails → warning branch; creds still cleared.
        store::save(&creds_with(Some("http://127.0.0.1:1".to_string()))).unwrap();
        exec().unwrap();
        assert!(store::load().unwrap().is_none());
    }

    /// A sign-out that starts while a refresh is rotating the session waits
    /// for it, then revokes and removes the session the refresh saved, so the
    /// rotated refresh token isn't left live on the server.
    #[test]
    fn test_logout_waits_for_a_refresh_and_revokes_what_it_saved() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        let mut server = mockito::Server::new();
        let revoke = server
            .mock("POST", "/api/oauth/revoke")
            .match_body(mockito::Matcher::UrlEncoded(
                "token".into(),
                "rotated-r".into(),
            ))
            .with_status(200)
            .expect(1)
            .create();
        store::save(&creds_with(Some(server.url()))).unwrap();

        let refresh = store::lock().unwrap();
        let logout = std::thread::spawn(exec);
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!logout.is_finished(), "logout went ahead without the lock");
        let mut rotated = creds_with(Some(server.url()));
        rotated.refresh_token = "rotated-r".to_string();
        refresh.save(&rotated).unwrap();
        drop(refresh);
        logout.join().unwrap().unwrap();

        revoke.assert();
        assert!(store::load().unwrap().is_none());
    }
}
