//! The sign-in an OTLP upload to Actual's API carries.
//!
//! When the uploader's endpoint has the scheme, host and port of the address
//! `actual advisor` sends the sign-in to ([`crate::api::resolve_api_url`]), the
//! request carries the stored sign-in's access token as its bearer, so Actual
//! records a signed-in user's events as theirs and takes the organization from
//! the token. No other endpoint gets it.
//!
//! **Records go with the sign-in they were recorded under.** Each record is
//! spooled with a [`stamp`] of the sign-in in force when it was recorded, and
//! the token goes only with records whose stamp matches the sign-in at send
//! time. The rest, recorded signed out or under another account or
//! organization, go in a request of their own without it. The stamp is a hash
//! of the sign-in's ids, holds nothing from the token, and never leaves the
//! spool.
//!
//! **The token is read when it is sent.** The uploader reads the stored sign-in
//! itself, renewing it first when it expires within a minute, and puts the
//! token in the request's `Authorization` header only: never in the spool, the
//! file sink, its own command line or environment, or a log line.

use std::time::{Duration, Instant};

use chrono::Utc;
use url::Url;

use crate::auth::refresh;
use crate::auth::store::{self, StoredCredentials};

/// How long the uploader waits between tries for a lock another process holds.
const LOCK_RETRY: Duration = Duration::from_millis(25);

/// What a request to Actual's API carries for one sign-in. Not `Debug`, so the
/// token can't reach a log through `{:?}`.
pub(crate) struct SignIn {
    token: String,
    stamp: String,
}

impl SignIn {
    /// The access token, for the request's bearer.
    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    /// Whether a record spooled with `stamp` was recorded under this sign-in.
    pub(crate) fn covers(&self, stamp: Option<&str>) -> bool {
        stamp == Some(self.stamp.as_str())
    }
}

/// Whether OTLP requests to `endpoint` may carry the sign-in: its scheme, host
/// and port are the Actual API address's, and neither URL names a user.
pub(crate) fn is_actual_api(endpoint: &str) -> bool {
    let origin = |url: &str| {
        Url::parse(url)
            .ok()
            .filter(|url| url.username().is_empty() && url.password().is_none())
            .map(|url| url.origin())
    };
    origin(endpoint)
        .is_some_and(|endpoint| Some(endpoint) == origin(&crate::api::resolve_api_url(None)))
}

/// The stamp records spooled under `creds` carry: a hash of the sign-in's
/// organization, member and subject.
pub(crate) fn stamp(creds: &StoredCredentials) -> String {
    let ids = serde_json::json!([creds.organization_id, creds.member_id, creds.subject]);
    crate::telemetry::identity::hash_identifier("sign-in", &ids.to_string())
}

/// The stamp for records spooled now for `endpoint`: the stored sign-in's,
/// when the endpoint is Actual's API and someone is signed in.
pub(crate) fn recording_stamp(endpoint: Option<&str>) -> Option<String> {
    endpoint.filter(|endpoint| is_actual_api(endpoint))?;
    load().map(|creds| stamp(&creds))
}

/// The sign-in an upload to `endpoint` carries: none unless the endpoint is
/// Actual's API and someone is signed in with an access token that hasn't
/// expired. A token within a minute of expiry is renewed and saved first, as
/// the commands renew it and under the same lock, which the uploader waits for
/// until `deadline`. A renewal that fails or can't start by then leaves the
/// upload signed out and the stored sign-in as it was.
pub(crate) async fn for_upload(endpoint: &str, deadline: Instant) -> Option<SignIn> {
    if !is_actual_api(endpoint) {
        return None;
    }
    let mut creds = load()?;
    if refresh::needs_refresh(&creds) {
        creds = renewed(deadline).await?;
    }
    if creds.access_token.is_empty() || creds.is_expired(Utc::now()) {
        return None;
    }
    Some(SignIn {
        stamp: stamp(&creds),
        token: creds.access_token,
    })
}

/// The stored sign-in, renewed under the lock when it still needs it once the
/// lock is held, or `None` when the renewal fails or the lock isn't free by
/// `deadline`.
async fn renewed(deadline: Instant) -> Option<StoredCredentials> {
    loop {
        if let Some(lock) = store::try_lock().ok()? {
            return refresh::refresh_held(&lock).await.ok();
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(LOCK_RETRY).await;
    }
}

/// The stored sign-in, or `None` when there is none or it can't be read.
fn load() -> Option<StoredCredentials> {
    #[cfg(test)]
    tests::READS.with(|reads| reads.set(reads.get() + 1));
    store::load().ok().flatten()
}

#[cfg(test)]
pub(crate) mod tests {
    // These async tests hold `ENV_MUTEX` (a std Mutex) across `.await` points on
    // purpose: it is an env-serialization latch held across await points by design,
    // serializing tests that mutate process-global env vars so they never race.
    #![allow(clippy::await_holding_lock)]
    use std::cell::Cell;

    use chrono::Duration as ChronoDuration;

    use super::*;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

    thread_local! {
        /// How many times this thread has read the stored sign-in here.
        pub(crate) static READS: Cell<usize> = const { Cell::new(0) };
    }

    /// The reads of the stored sign-in on this thread since the last call.
    pub(crate) fn reads() -> usize {
        READS.with(|reads| reads.replace(0))
    }

    /// A signed-in user of `org`, with an access token `token` that expires
    /// `expires_in` from now and renews at `auth_url`.
    pub(crate) fn signed_in(
        org: &str,
        token: &str,
        expires_in: ChronoDuration,
        auth_url: Option<String>,
    ) -> StoredCredentials {
        StoredCredentials {
            access_token: token.to_string(),
            refresh_token: format!("{token}-refresh"),
            token_type: "Bearer".to_string(),
            expires_at: Some(Utc::now() + expires_in),
            scope: Some("openid adr:query".to_string()),
            organization_id: org.to_string(),
            member_id: format!("{org}-member"),
            email: Some("dev@example.com".to_string()),
            subject: Some("user-1".to_string()),
            auth_url,
        }
    }

    /// The token endpoint's answer to a refresh that rotates the session.
    pub(crate) fn rotated(token: &str) -> String {
        format!(
            r#"{{"access_token":"{token}","token_type":"Bearer","expires_in":3600,"refresh_token":"{token}-refresh"}}"#
        )
    }

    /// A config dir of the test's own, and the Actual API address set to
    /// `api`, so nothing here reads or locks the machine's real sign-in.
    pub(crate) fn isolated(api: &str) -> (tempfile::TempDir, Vec<EnvGuard>) {
        let home = tempfile::tempdir().unwrap();
        let guards = vec![
            EnvGuard::remove("ACTUAL_CONFIG"),
            EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap()),
            EnvGuard::set("ACTUAL_API_URL", api),
        ];
        (home, guards)
    }

    // ── where the token may go ──────────────────────────────────────────

    /// With `ACTUAL_API_URL` unset, the built-in address is Actual's API, the
    /// same one the advisor uses.
    #[test]
    fn test_the_built_in_address_is_actual_api() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove("ACTUAL_API_URL");

        assert!(is_actual_api(
            "https://api-service.api.prod.actual.ai/v1/logs"
        ));
        assert!(is_actual_api(
            "https://API-SERVICE.api.prod.actual.ai:443/v1/logs?x=1"
        ));
    }

    /// `ACTUAL_API_URL` moves the address, so the built-in one stops
    /// counting, and a path on it doesn't matter.
    #[test]
    fn test_actual_api_url_moves_the_address() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::set("ACTUAL_API_URL", "http://127.0.0.1:4100/api");

        assert!(is_actual_api("http://127.0.0.1:4100/v1/logs"));
        assert!(!is_actual_api(
            "https://api-service.api.prod.actual.ai/v1/logs"
        ));
    }

    /// Only the same scheme, host and port count: another host, port or
    /// scheme, a lookalike host, or a URL naming a user gets no token.
    #[test]
    fn test_any_other_origin_is_not_actual_api() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove("ACTUAL_API_URL");

        for endpoint in [
            "https://collector.example.com/v1/logs",
            "https://api-service.api.prod.actual.ai:4318/v1/logs",
            "http://api-service.api.prod.actual.ai/v1/logs",
            "https://api-service.api.prod.actual.ai.evil.example/v1/logs",
            "https://evil-api-service.api.prod.actual.ai/v1/logs",
            "https://api.prod.actual.ai/v1/logs",
            "https://api-service.api.prod.actual.ai@evil.example/v1/logs",
            "https://user@api-service.api.prod.actual.ai/v1/logs",
            "https://user:pass@api-service.api.prod.actual.ai/v1/logs",
            "not a url",
        ] {
            assert!(!is_actual_api(endpoint), "{endpoint}");
        }
    }

    /// An Actual API address that names a user, or isn't a URL, matches
    /// nothing.
    #[test]
    fn test_an_unusable_api_address_matches_nothing() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        for api in ["https://user@api.example.com", "not a url"] {
            let _env = EnvGuard::set("ACTUAL_API_URL", api);
            assert!(!is_actual_api("https://api.example.com/v1/logs"), "{api}");
            assert!(!is_actual_api(api), "{api}");
        }
    }

    // ── the stamp ───────────────────────────────────────────────────────

    /// The stamp names the account and organization, and holds nothing from
    /// either token.
    #[test]
    fn test_the_stamp_names_the_account_and_organization() {
        let one = signed_in("org-a", "token-a", ChronoDuration::hours(1), None);
        let stamp_one = stamp(&one);

        let mut refreshed = one.clone();
        refreshed.access_token = "token-b".to_string();
        refreshed.refresh_token = "token-b-refresh".to_string();
        assert_eq!(stamp(&refreshed), stamp_one, "a refresh keeps the stamp");

        let mut other_org = one.clone();
        other_org.organization_id = "org-b".to_string();
        let mut other_member = one.clone();
        other_member.member_id = "someone-else".to_string();
        let mut other_user = one.clone();
        other_user.subject = None;
        for other in [other_org, other_member, other_user] {
            assert_ne!(stamp(&other), stamp_one);
        }

        assert_eq!(stamp_one.len(), 64);
        assert!(!stamp_one.contains("token-a"));
        assert!(!stamp_one.contains("org-a"));
    }

    /// Records are stamped only when they're bound for Actual's API and
    /// someone is signed in.
    #[test]
    fn test_recording_stamp() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = isolated("https://api.example.com");
        let endpoint = "https://api.example.com/v1/logs";
        reads();

        assert_eq!(recording_stamp(Some(endpoint)), None, "signed out");
        let creds = signed_in("org-a", "token-a", ChronoDuration::hours(1), None);
        store::save(&creds).unwrap();
        assert_eq!(recording_stamp(Some(endpoint)), Some(stamp(&creds)));
        assert_eq!(reads(), 2);

        assert_eq!(
            recording_stamp(Some("https://collector.example.com/v1/logs")),
            None
        );
        assert_eq!(recording_stamp(None), None);
        assert_eq!(reads(), 0, "a stamp no one can use reads nothing");
    }

    // ── the sign-in an upload carries ───────────────────────────────────

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[tokio::test]
    async fn test_an_upload_to_actual_api_carries_the_stored_sign_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = isolated("https://api.example.com");
        let creds = signed_in("org-a", "token-a", ChronoDuration::hours(1), None);
        store::save(&creds).unwrap();

        let sign_in = for_upload("https://api.example.com/v1/logs", soon())
            .await
            .unwrap();

        assert_eq!(sign_in.token(), "token-a");
        assert!(sign_in.covers(Some(&stamp(&creds))));
        assert!(!sign_in.covers(None));
        assert!(!sign_in.covers(Some("another sign-in")));
    }

    /// Another endpoint gets no sign-in, and the store isn't even read.
    #[tokio::test]
    async fn test_an_upload_elsewhere_reads_no_sign_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = isolated("https://api.example.com");
        store::save(&signed_in("org-a", "a", ChronoDuration::hours(1), None)).unwrap();
        reads();

        assert!(for_upload("https://collector.example.com/v1/logs", soon())
            .await
            .is_none());
        assert_eq!(reads(), 0);
    }

    /// Signed out, or holding a token that has expired with nothing to renew
    /// it, or an empty one, the upload goes signed out.
    #[tokio::test]
    async fn test_an_upload_with_no_usable_sign_in_goes_signed_out() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = isolated("https://api.example.com");
        let endpoint = "https://api.example.com/v1/logs";

        assert!(for_upload(endpoint, soon()).await.is_none());

        let mut expired = signed_in("org-a", "a", ChronoDuration::seconds(-1), None);
        expired.refresh_token = String::new();
        store::save(&expired).unwrap();
        assert!(for_upload(endpoint, soon()).await.is_none());

        let mut empty = signed_in("org-a", "a", ChronoDuration::hours(1), None);
        empty.access_token = String::new();
        store::save(&empty).unwrap();
        assert!(for_upload(endpoint, soon()).await.is_none());
    }

    /// A token within a minute of expiry is renewed and saved before it goes.
    #[tokio::test]
    async fn test_an_expiring_sign_in_is_renewed_and_saved_first() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let mut auth = mockito::Server::new_async().await;
        let (_home, _env) = isolated("https://api.example.com");
        let token = auth
            .mock("POST", "/api/oauth/token")
            .match_body(mockito::Matcher::UrlEncoded(
                "refresh_token".into(),
                "old-refresh".into(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(rotated("new"))
            .expect(1)
            .create_async()
            .await;
        let stale = signed_in(
            "org-a",
            "old",
            ChronoDuration::seconds(30),
            Some(auth.url()),
        );
        store::save(&stale).unwrap();

        let sign_in = for_upload("https://api.example.com/v1/logs", soon())
            .await
            .unwrap();

        token.assert_async().await;
        assert_eq!(sign_in.token(), "new");
        assert!(sign_in.covers(Some(&stamp(&stale))));
        let saved = store::load().unwrap().unwrap();
        assert_eq!(saved.access_token, "new");
        assert_eq!(saved.refresh_token, "new-refresh");
    }

    /// A renewal the server refuses leaves the upload signed out and the
    /// stored sign-in in place, for the next command to report.
    #[tokio::test]
    async fn test_a_refused_renewal_goes_signed_out_and_keeps_the_sign_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let mut auth = mockito::Server::new_async().await;
        let (_home, _env) = isolated("https://api.example.com");
        let _token = auth
            .mock("POST", "/api/oauth/token")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":"invalid_grant"}"#)
            .create_async()
            .await;
        let stale = signed_in(
            "org-a",
            "old",
            ChronoDuration::seconds(30),
            Some(auth.url()),
        );
        store::save(&stale).unwrap();

        assert!(for_upload("https://api.example.com/v1/logs", soon())
            .await
            .is_none());

        assert_eq!(store::load().unwrap(), Some(stale));
    }

    /// A lock held past the deadline, or one that can't be taken at all,
    /// leaves the upload signed out without a renewal.
    #[tokio::test]
    async fn test_a_lock_it_cannot_get_leaves_the_upload_signed_out() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let mut auth = mockito::Server::new_async().await;
        let (home, _env) = isolated("https://api.example.com");
        let token = auth
            .mock("POST", "/api/oauth/token")
            .expect(0)
            .create_async()
            .await;
        let stale = signed_in(
            "org-a",
            "old",
            ChronoDuration::seconds(30),
            Some(auth.url()),
        );
        store::save(&stale).unwrap();
        let endpoint = "https://api.example.com/v1/logs";

        let held = store::lock().unwrap();
        let started = Instant::now();
        let deadline = started + Duration::from_millis(200);
        assert!(for_upload(endpoint, deadline).await.is_none());
        assert!(started.elapsed() >= Duration::from_millis(200));
        drop(held);

        let lock_file = home.path().join("credentials.lock");
        std::fs::remove_file(&lock_file).unwrap();
        std::fs::create_dir(&lock_file).unwrap();
        assert!(for_upload(endpoint, soon()).await.is_none());

        token.assert_async().await;
        assert_eq!(store::load().unwrap(), Some(stale));
    }

    /// Two uploaders and a command find the sign-in about to expire at once.
    /// One of them renews it, the others wait for the lock and reuse that
    /// renewal, nobody ends up signed out, and the server sees one refresh.
    #[test]
    fn test_uploaders_and_a_command_renewing_at_once_share_one_refresh() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let mut auth = mockito::Server::new();
        let (_home, _env) = isolated("https://api.example.com");
        let token = auth
            .mock("POST", "/api/oauth/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body_from_request(|_| {
                std::thread::sleep(std::time::Duration::from_millis(300));
                rotated("new").into()
            })
            .expect(1)
            .create();
        let stale = signed_in(
            "org-a",
            "old",
            ChronoDuration::seconds(30),
            Some(auth.url()),
        );
        store::save(&stale).unwrap();
        let run = |task: std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send>>| {
            std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(task)
            })
        };
        let upload = || async {
            for_upload("https://api.example.com/v1/logs", soon())
                .await
                .map(|sign_in| sign_in.token().to_string())
                .unwrap_or_default()
        };

        let workers = [
            run(Box::pin(upload())),
            run(Box::pin(upload())),
            run(Box::pin({
                let stale = stale.clone();
                async move { refresh::ensure_fresh(stale).await.unwrap().access_token }
            })),
        ];
        let tokens: Vec<String> = workers.into_iter().map(|w| w.join().unwrap()).collect();

        assert_eq!(tokens, ["new", "new", "new"]);
        token.assert();
        assert_eq!(store::load().unwrap().unwrap().refresh_token, "new-refresh");
    }

    /// The user signs in to another organization while an uploader is
    /// renewing the old sign-in. The new sign-in waits for the renewal and
    /// lands after it, so it survives, and the uploader's token is the old
    /// organization's, for the records recorded under it.
    #[test]
    fn test_a_sign_in_during_a_renewal_survives_it() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let mut auth = mockito::Server::new();
        let (_home, _env) = isolated("https://api.example.com");
        let (asked, refreshing) = std::sync::mpsc::channel();
        let asked = std::sync::Mutex::new(asked);
        let _token = auth
            .mock("POST", "/api/oauth/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body_from_request(move |_| {
                asked.lock().unwrap().send(()).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(300));
                rotated("org-a-new").into()
            })
            .create();
        let stale = signed_in(
            "org-a",
            "org-a-old",
            ChronoDuration::seconds(30),
            Some(auth.url()),
        );
        store::save(&stale).unwrap();
        let newer = signed_in("org-b", "org-b", ChronoDuration::hours(1), None);

        let uploader = std::thread::spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(for_upload("https://api.example.com/v1/logs", soon()))
                .map(|sign_in| sign_in.token().to_string())
        });
        refreshing
            .recv_timeout(Duration::from_secs(10))
            .expect("the uploader never asked to renew");
        store::save(&newer).unwrap();

        assert_eq!(uploader.join().unwrap().as_deref(), Some("org-a-new"));
        assert_eq!(store::load().unwrap(), Some(newer));
    }
}
