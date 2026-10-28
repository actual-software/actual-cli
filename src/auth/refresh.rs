//! Keeping the stored sign-in's access token fresh for whoever sends it: the
//! `advisor` and `auth create-token` commands, and the telemetry uploader.
//!
//! A renewal runs under [`store::lock`] and starts by reading the sign-in again.
//! A process that waited for the lock therefore reuses the renewal the holder
//! just saved, rather than spending a refresh token the server has already
//! taken, and a sign-in saved in the meantime is the one it acts on.

use chrono::{Duration, Utc};

use crate::auth::oauth;
use crate::auth::store::{self, StoredCredentials};
use crate::error::ActualError;

/// How close to its expiry an access token gets renewed.
const REFRESH_LEEWAY_SECONDS: i64 = 60;

/// `creds`, renewed and saved first when its access token expires within a
/// minute. The renewal waits for [`store::lock`] and works on the sign-in
/// stored then, which is what comes back. A session [`oauth::refresh`] can't
/// renew, or a sign-in removed meanwhile, surfaces as `NotLoggedIn`; any other
/// refresh failure keeps its own error.
pub async fn ensure_fresh(creds: StoredCredentials) -> Result<StoredCredentials, ActualError> {
    if !needs_refresh(&creds) {
        return Ok(creds);
    }
    refresh_held(&store::lock()?).await
}

/// The sign-in stored under `lock`, renewed and saved first when it still
/// needs it. `NotLoggedIn` when none is stored.
pub async fn refresh_held(lock: &store::Lock) -> Result<StoredCredentials, ActualError> {
    let current = lock.load()?.ok_or(ActualError::NotLoggedIn)?;
    if !needs_refresh(&current) {
        return Ok(current);
    }
    let refreshed = oauth::refresh(&current).await?;
    lock.save(&refreshed)?;
    Ok(refreshed)
}

/// Whether `creds` should be renewed before use: it carries a refresh token,
/// and its access token expires within a minute.
pub fn needs_refresh(creds: &StoredCredentials) -> bool {
    !creds.refresh_token.is_empty()
        && creds.expires_within(Utc::now(), Duration::seconds(REFRESH_LEEWAY_SECONDS))
}

#[cfg(test)]
mod tests {
    // These async tests hold `ENV_MUTEX` (a std Mutex) across `.await` points on
    // purpose: it is an env-serialization latch held across await points by design,
    // serializing tests that mutate process-global env vars so they never race.
    #![allow(clippy::await_holding_lock)]
    use super::*;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use tempfile::tempdir;

    fn creds(auth_url: Option<String>, expires_in: Duration) -> StoredCredentials {
        StoredCredentials {
            access_token: "old-at".to_string(),
            refresh_token: "old-rt".to_string(),
            token_type: "Bearer".to_string(),
            expires_at: Some(Utc::now() + expires_in),
            scope: None,
            organization_id: "org-a".to_string(),
            member_id: "member-a".to_string(),
            email: None,
            subject: Some("user-a".to_string()),
            auth_url,
        }
    }

    fn rotated(access: &str, refresh: &str) -> String {
        format!(
            r#"{{"access_token":"{access}","token_type":"Bearer","expires_in":3600,"refresh_token":"{refresh}"}}"#
        )
    }

    /// A config dir of the test's own, so nothing here reads or locks the
    /// machine's real sign-in.
    fn isolated() -> (tempfile::TempDir, Vec<EnvGuard>) {
        let home = tempdir().unwrap();
        let guards = vec![
            EnvGuard::remove("ACTUAL_CONFIG"),
            EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap()),
        ];
        (home, guards)
    }

    #[test]
    fn test_needs_refresh() {
        let auth = Some("http://localhost:4000".to_string());
        assert!(needs_refresh(&creds(auth.clone(), Duration::seconds(30))));
        assert!(needs_refresh(&creds(auth.clone(), Duration::seconds(-1))));
        assert!(!needs_refresh(&creds(auth.clone(), Duration::seconds(120))));
        let mut no_refresh_token = creds(auth, Duration::seconds(-1));
        no_refresh_token.refresh_token = String::new();
        assert!(!needs_refresh(&no_refresh_token));
    }

    /// A sign-in that doesn't need renewing comes back as it was, without the
    /// lock or a read of the store.
    #[tokio::test]
    async fn test_ensure_fresh_leaves_a_fresh_sign_in_alone() {
        let mut fresh = creds(None, Duration::hours(1));
        let out = ensure_fresh(fresh.clone()).await.unwrap();
        assert_eq!(out, fresh);

        fresh.refresh_token = String::new();
        fresh.expires_at = Some(Utc::now() - Duration::seconds(1));
        let out = ensure_fresh(fresh.clone()).await.unwrap();
        assert_eq!(out, fresh, "nothing to renew it with");
    }

    #[tokio::test]
    async fn test_ensure_fresh_renews_and_saves_an_expiring_sign_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = isolated();
        let mut server = mockito::Server::new_async().await;
        let token = server
            .mock("POST", "/api/oauth/token")
            .match_body(mockito::Matcher::UrlEncoded(
                "refresh_token".into(),
                "old-rt".into(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(rotated("new-at", "new-rt"))
            .expect(1)
            .create_async()
            .await;
        let stale = creds(Some(server.url()), Duration::seconds(30));
        store::save(&stale).unwrap();

        let out = ensure_fresh(stale).await.unwrap();

        token.assert_async().await;
        assert_eq!(out.access_token, "new-at");
        let saved = store::load().unwrap().unwrap();
        assert_eq!(saved.access_token, "new-at");
        assert_eq!(saved.refresh_token, "new-rt");
        assert_eq!(saved.organization_id, "org-a");
    }

    /// An auth server that never answered: the error names the refresh step
    /// rather than claiming a sign-out, and the stored sign-in stays.
    #[tokio::test]
    async fn test_ensure_fresh_with_an_unreachable_auth_server_is_not_signed_out() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = isolated();
        let stale = creds(
            Some("http://127.0.0.1:1".to_string()),
            Duration::seconds(-1),
        );
        store::save(&stale).unwrap();

        let err = ensure_fresh(stale.clone()).await.unwrap_err();

        assert!(
            matches!(err, ActualError::ApiError(ref m) if m.starts_with("Token refresh request failed")),
            "got: {err:?}"
        );
        assert_eq!(store::load().unwrap(), Some(stale));
    }

    /// An auth server that answers with an outage is reported as itself,
    /// since signing in again won't fix it.
    #[tokio::test]
    async fn test_ensure_fresh_in_an_auth_server_outage_is_not_signed_out() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = isolated();
        let mut server = mockito::Server::new_async().await;
        let _token = server
            .mock("POST", "/api/oauth/token")
            .with_status(500)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":"server_error"}"#)
            .create_async()
            .await;
        let stale = creds(Some(server.url()), Duration::hours(-1));
        store::save(&stale).unwrap();

        let err = ensure_fresh(stale.clone()).await.unwrap_err();

        assert!(
            matches!(err, ActualError::ApiError(ref m)
                if m == "Token refresh failed (HTTP 500 Internal Server Error): server_error"),
            "got: {err:?}"
        );
        assert_eq!(store::load().unwrap(), Some(stale));
    }

    #[tokio::test]
    async fn test_ensure_fresh_without_an_auth_url_is_not_logged_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = isolated();
        let stale = creds(None, Duration::seconds(-1));
        store::save(&stale).unwrap();

        let err = ensure_fresh(stale).await.unwrap_err();

        assert!(matches!(err, ActualError::NotLoggedIn), "got: {err:?}");
        assert_eq!(err.exit_code(), 2);
        assert_eq!(err.hint().as_deref(), Some("actual login"));
    }

    /// A refresh token the server refuses ends the session for the command,
    /// and the stored sign-in stays for the next command to report.
    #[tokio::test]
    async fn test_ensure_fresh_with_a_refused_refresh_token_is_not_logged_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = isolated();
        let mut server = mockito::Server::new_async().await;
        let _token = server
            .mock("POST", "/api/oauth/token")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":"invalid_grant"}"#)
            .create_async()
            .await;
        let stale = creds(Some(server.url()), Duration::seconds(-1));
        store::save(&stale).unwrap();

        let err = ensure_fresh(stale.clone()).await.unwrap_err();

        assert!(matches!(err, ActualError::NotLoggedIn), "got: {err:?}");
        assert_eq!(store::load().unwrap(), Some(stale));
    }

    /// Signed out between reading the sign-in and renewing it: nothing to
    /// renew, so the command is not logged in, and no refresh is spent.
    #[tokio::test]
    async fn test_ensure_fresh_after_a_sign_out_is_not_logged_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = isolated();
        let mut server = mockito::Server::new_async().await;
        let token = server
            .mock("POST", "/api/oauth/token")
            .expect(0)
            .create_async()
            .await;

        let err = ensure_fresh(creds(Some(server.url()), Duration::seconds(-1)))
            .await
            .unwrap_err();

        assert!(matches!(err, ActualError::NotLoggedIn), "got: {err:?}");
        token.assert_async().await;
    }

    /// Signed in to another organization between reading the sign-in and
    /// renewing it: the new sign-in comes back, untouched, and the old one's
    /// refresh token is never spent.
    #[tokio::test]
    async fn test_ensure_fresh_after_a_new_sign_in_uses_the_new_one() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = isolated();
        let mut server = mockito::Server::new_async().await;
        let token = server
            .mock("POST", "/api/oauth/token")
            .expect(0)
            .create_async()
            .await;
        let stale = creds(Some(server.url()), Duration::seconds(-1));
        let mut newer = creds(Some(server.url()), Duration::hours(1));
        newer.organization_id = "org-b".to_string();
        newer.access_token = "org-b-at".to_string();
        store::save(&newer).unwrap();

        let out = ensure_fresh(stale).await.unwrap();

        assert_eq!(out, newer);
        assert_eq!(store::load().unwrap(), Some(newer));
        token.assert_async().await;
    }

    /// Three commands find the same sign-in about to expire at once. One
    /// renews it, the other two wait for the lock and reuse that renewal, and
    /// the server sees one refresh. Without the lock each would spend the same
    /// refresh token, which the server takes once.
    #[test]
    fn test_commands_renewing_at_once_share_one_refresh() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = isolated();
        let mut server = mockito::Server::new();
        let token = server
            .mock("POST", "/api/oauth/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body_from_request(|_| {
                std::thread::sleep(std::time::Duration::from_millis(300));
                rotated("new-at", "new-rt").into()
            })
            .expect(1)
            .create();
        let stale = creds(Some(server.url()), Duration::seconds(30));
        store::save(&stale).unwrap();

        let commands: Vec<_> = (0..3)
            .map(|_| {
                let stale = stale.clone();
                std::thread::spawn(move || {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap()
                        .block_on(ensure_fresh(stale))
                })
            })
            .collect();
        let tokens: Vec<String> = commands
            .into_iter()
            .map(|command| command.join().unwrap().unwrap().access_token)
            .collect();

        assert_eq!(tokens, ["new-at", "new-at", "new-at"]);
        token.assert();
        assert_eq!(store::load().unwrap().unwrap().refresh_token, "new-rt");
    }
}
