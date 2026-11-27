//! Signing in from a personal access token in the `ACTUAL_TOKEN` env var, for
//! machines nobody can open a browser on, such as an Amp orb.
//!
//! With `ACTUAL_TOKEN` set, [`crate::auth::store::load`] returns credentials
//! built from it instead of `credentials.yaml`: the token is the bearer, and
//! the organization, member, email, scopes and expiry come from asking
//! Actual's API who the token belongs to (`GET /v1/oauth/tokens/current`).
//! The answer is cached in `token-identity.yaml` beside the credentials, keyed
//! by the token's SHA-256 and holding nothing from the token itself, because
//! hooks read the sign-in on every tool call:
//!
//! - an identity is reused for 24 hours, and past that whenever Actual's API
//!   can't be reached, until the token's own expiry;
//! - a 401 is remembered for an hour, so a revoked token fails fast;
//! - a request that got no usable answer (a timeout, a 404, a 5xx) waits five
//!   minutes before the next one;
//! - each request gets three seconds in all.
//!
//! The token is sent only to Actual's api-service, production or staging, or
//! to a loopback server, and never refreshed or revoked by this CLI.

use std::path::PathBuf;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::auth::pat::PAT_PREFIX;
use crate::auth::store::StoredCredentials;
use crate::config::paths::config_dir;
use crate::error::ActualError;

/// The env var that holds the token.
pub const ENV: &str = "ACTUAL_TOKEN";

/// The api-service origins `ACTUAL_TOKEN` may be sent to, besides loopback.
const ALLOWED_ORIGINS: [&str; 2] = [
    "https://api-service.api.prod.actual.ai",
    "https://api-service.api.staging.actual.ai",
];

/// The introspection route, appended to the API base.
pub const CURRENT_PATH: &str = "/v1/oauth/tokens/current";

const CACHE_FILENAME: &str = "token-identity.yaml";
const REQUEST_TIMEOUT: StdDuration = StdDuration::from_secs(3);
const FRESH_FOR_HOURS: i64 = 24;
const REJECTED_FOR_HOURS: i64 = 1;
const UNREACHABLE_BACKOFF_MINUTES: i64 = 5;

pub const REJECTED: &str =
    "Your ACTUAL_TOKEN has expired or was revoked. On a computer signed in to \
     the actual CLI, run `actual auth create-token --name amp-orbs --scopes adr:query,adr:review`, \
     then replace ACTUAL_TOKEN in Amp Settings and start a new thread (or run `amp orb \
     restart-processes` in this one). If you think it's still valid, run `actual whoami --check`.";

pub const UNSUPPORTED: &str = "ACTUAL_TOKEN isn't an Actual access token. On a computer signed in \
     to the actual CLI, run `actual auth create-token --name amp-orbs --scopes \
     adr:query,adr:review`, then replace ACTUAL_TOKEN in Amp Settings.";

pub const FOREIGN_HOST: &str = "ACTUAL_TOKEN works only with Actual AI. Unset ACTUAL_API_URL (or \
     --api-url), or unset ACTUAL_TOKEN to use another server.";

pub const OTHER_API_URL: &str =
    "With ACTUAL_TOKEN set, choose the server with ACTUAL_API_URL, not --api-url.";

pub const REFUSED_REQUEST: &str =
    "Actual AI refused this request, though your ACTUAL_TOKEN still works. \
     Check that the token's organization is the one this command uses.";

pub const SIGN_IN_UNCHANGED: &str = "You're using ACTUAL_TOKEN, so this doesn't change your \
     sign-in. To stop using it, remove ACTUAL_TOKEN, or revoke the token in Actual AI → User \
     Settings → Access tokens.";

/// Who a token belongs to, as Actual's API answered, and when it answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub id: String,
    pub name: String,
    pub organization_id: String,
    pub member_id: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    pub expires_at: DateTime<Utc>,
}

/// The one entry `token-identity.yaml` holds, for the current token only.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Entry {
    key: String,
    #[serde(default)]
    identity: Option<CachedIdentity>,
    #[serde(default)]
    last_attempt_at: Option<DateTime<Utc>>,
    #[serde(default)]
    rejected_at: Option<DateTime<Utc>>,
}

/// [`Identity`] as the cache keeps it, `checked_at` included.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedIdentity {
    #[serde(flatten)]
    identity: Identity,
    checked_at: DateTime<Utc>,
}

/// What one introspection request found.
enum Answer {
    Accepted(Identity),
    Rejected,
    Unreachable(ActualError),
}

/// The token in `ACTUAL_TOKEN`, or `None` when it's unset or blank.
pub fn token() -> Option<String> {
    std::env::var(ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The sign-in `token` gives, from the cache when it can.
pub fn credentials(token: &str) -> Result<StoredCredentials, ActualError> {
    Ok(to_credentials(token, identity(token)?))
}

/// Who `token` belongs to, from the cache when it can be, else from one
/// request to Actual's API.
pub fn identity(token: &str) -> Result<Identity, ActualError> {
    let base = api_base(token)?;
    let now = Utc::now();
    let entry = read_entry(token);
    if entry
        .rejected_at
        .is_some_and(|at| now - at < Duration::hours(REJECTED_FOR_HOURS))
    {
        return Err(rejected());
    }
    if let Some(cached) = &entry.identity {
        if now - cached.checked_at < Duration::hours(FRESH_FOR_HOURS) {
            return usable(&cached.identity, now);
        }
    }
    if entry
        .last_attempt_at
        .is_some_and(|at| now - at < Duration::minutes(UNREACHABLE_BACKOFF_MINUTES))
    {
        return match &entry.identity {
            Some(cached) => usable(&cached.identity, now),
            None => Err(ActualError::ApiError(
                "Couldn't reach Actual AI to check ACTUAL_TOKEN. Check your connection, then run \
                 `actual whoami --check`."
                    .to_string(),
            )),
        };
    }
    match ask(&base, token, entry) {
        Err(err @ ActualError::ActualTokenRefused(_)) => Err(err),
        // A server that couldn't be reached leaves the identity it last gave,
        // read again since another command may have stored one meanwhile.
        Err(err) => match read_entry(token).identity {
            Some(cached) => usable(&cached.identity, now),
            None => Err(err),
        },
        answer => answer,
    }
}

/// Who `token` belongs to, asking Actual's API now whatever the cache holds.
pub fn check(token: &str) -> Result<Identity, ActualError> {
    let base = api_base(token)?;
    ask(&base, token, read_entry(token))
}

/// After a command got a 401 with `ACTUAL_TOKEN` set: ask Actual's API about
/// the token once more, so the cache records what it says, and report what it
/// said in place of `actual login`'s hint. Any other error, or no token, passes
/// through unchanged.
pub fn explain(err: ActualError) -> ActualError {
    match (&err, token()) {
        (ActualError::NotLoggedIn, Some(token)) => match check(&token) {
            // Still valid: the route refused this request, not the token.
            Ok(_) => ActualError::ActualTokenRefused(REFUSED_REQUEST.to_string()),
            Err(e) => e,
        },
        _ => err,
    }
}

/// Refuse to send `ACTUAL_TOKEN` to `base` unless it's Actual's api-service or
/// loopback. Nothing to check when the variable isn't set.
pub fn ensure_destination(base: &str) -> Result<(), ActualError> {
    if token().is_none() {
        return Ok(());
    }
    if !allowed(base) {
        return Err(ActualError::ActualTokenRefused(FOREIGN_HOST.to_string()));
    }
    // The token's identity is checked at ACTUAL_API_URL's server, so a command
    // can't send it to a different one.
    let origin = |url: &str| Url::parse(url).ok().map(|url| url.origin());
    if origin(base) != origin(&crate::api::resolve_api_url(None)) {
        return Err(ActualError::ActualTokenRefused(OTHER_API_URL.to_string()));
    }
    Ok(())
}

/// Remove the identity cache. Idempotent.
pub fn forget() -> Result<(), ActualError> {
    crate::config::paths::remove_secure(&cache_path()?)
        .map_err(|e| ActualError::ConfigError(format!("Failed to remove {CACHE_FILENAME}: {e}")))
}

fn rejected() -> ActualError {
    ActualError::ActualTokenRefused(REJECTED.to_string())
}

/// The API base `token` goes to, once the token and the base are both ones
/// this CLI sends.
fn api_base(token: &str) -> Result<String, ActualError> {
    if !token.starts_with(PAT_PREFIX) {
        return Err(ActualError::ActualTokenRefused(UNSUPPORTED.to_string()));
    }
    let base = crate::api::resolve_api_url(None);
    if !allowed(&base) {
        return Err(ActualError::ActualTokenRefused(FOREIGN_HOST.to_string()));
    }
    Ok(base)
}

/// Whether `base` has the scheme, host and port of an allowed api-service
/// origin, names no user, or is plain-http loopback.
fn allowed(base: &str) -> bool {
    if crate::net::is_loopback_http_url(base) {
        return true;
    }
    let origin = |url: &str| {
        Url::parse(url)
            .ok()
            .filter(|url| url.username().is_empty() && url.password().is_none())
            .map(|url| url.origin())
    };
    origin(base).is_some_and(|base| {
        ALLOWED_ORIGINS
            .iter()
            .any(|allowed| origin(allowed) == Some(base.clone()))
    })
}

/// An identity whose token hasn't expired at `now`.
fn usable(identity: &Identity, now: DateTime<Utc>) -> Result<Identity, ActualError> {
    if identity.expires_at <= now {
        return Err(rejected());
    }
    Ok(identity.clone())
}

/// The sign-in `identity` describes, with `token` as its bearer.
pub(crate) fn to_credentials(token: &str, identity: Identity) -> StoredCredentials {
    StoredCredentials {
        access_token: token.to_string(),
        refresh_token: String::new(),
        token_type: "Bearer".to_string(),
        expires_at: Some(identity.expires_at),
        scope: Some(identity.scopes.join(" ")),
        organization_id: identity.organization_id,
        member_id: identity.member_id,
        email: identity.email,
        subject: None,
        auth_url: None,
    }
}

/// Ask Actual's API about `token`, record the answer in the cache, and return
/// it.
fn ask(base: &str, token: &str, mut entry: Entry) -> Result<Identity, ActualError> {
    let now = Utc::now();
    entry.last_attempt_at = Some(now);
    let answer = introspect(base, token);
    let result = match answer {
        Answer::Accepted(identity) => {
            entry.rejected_at = None;
            entry.identity = Some(CachedIdentity {
                identity: identity.clone(),
                checked_at: now,
            });
            Ok(identity)
        }
        Answer::Rejected => {
            entry.rejected_at = Some(now);
            entry.identity = None;
            Err(rejected())
        }
        Answer::Unreachable(err) => {
            // Another command may have recorded an answer while this request
            // waited; keep it, and record only this attempt.
            let current = read_entry(token);
            if current.identity.is_some() || current.rejected_at.is_some() {
                entry.identity = current.identity;
                entry.rejected_at = current.rejected_at;
            }
            Err(err)
        }
    };
    write_entry(&entry);
    result
}

/// One `GET /v1/oauth/tokens/current` with `token`, on a thread of its own
/// with its own runtime, so sync and async callers alike can wait for it,
/// bounded by [`REQUEST_TIMEOUT`] in all.
fn introspect(base: &str, token: &str) -> Answer {
    let url = format!("{}{CURRENT_PATH}", base.trim_end_matches('/'));
    let client = crate::auth::oauth::build_http_client(base);
    let token = token.to_string();
    let answer = std::thread::spawn(move || {
        let client = client.map_err(|e| e.to_string())?;
        let request = async move {
            let response = client.get(&url).bearer_auth(&token).send().await?;
            let status = response.status();
            if status == reqwest::StatusCode::UNAUTHORIZED {
                return Ok(Answer::Rejected);
            }
            if !status.is_success() {
                return Ok(Answer::Unreachable(ActualError::ApiError(format!(
                    "Couldn't check ACTUAL_TOKEN with Actual AI (HTTP {status})"
                ))));
            }
            Ok(Answer::Accepted(response.json::<Identity>().await?))
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let answer =
            runtime.block_on(async { tokio::time::timeout(REQUEST_TIMEOUT, request).await });
        // A name lookup runs on a blocking thread the timeout can't stop; dropping the
        // runtime would wait for it, so leave it behind to keep the 3-second bound.
        runtime.shutdown_background();
        answer
            .map_err(|_| "no answer within 3 seconds".to_string())?
            .map_err(|e: reqwest::Error| e.to_string())
    })
    .join()
    .unwrap_or_else(|_| Err("the request stopped unexpectedly".to_string()));
    answer.unwrap_or_else(|reason| {
        Answer::Unreachable(ActualError::ApiError(format!(
            "Couldn't check ACTUAL_TOKEN with Actual AI: {reason}"
        )))
    })
}

fn cache_path() -> Result<PathBuf, ActualError> {
    Ok(config_dir()?.join(CACHE_FILENAME))
}

fn key(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The cache's entry for `token`, or an empty one when the file is missing,
/// unreadable, or holds another token's entry.
fn read_entry(token: &str) -> Entry {
    let key = key(token);
    cache_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| crate::yaml::from_str::<Entry>(&text).ok())
        .filter(|entry| entry.key == key)
        .unwrap_or(Entry {
            key,
            ..Entry::default()
        })
}

/// Replace the cache with `entry`. Best effort: without it, the next command
/// asks again.
fn write_entry(entry: &Entry) {
    let Ok(path) = cache_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(yaml) = crate::yaml::to_string(entry) {
        let _ = crate::config::paths::write_secure(&path, yaml.as_bytes());
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

    pub(crate) const TOKEN: &str = "actl_pat_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    pub(crate) fn identity_json(expires_at: DateTime<Utc>) -> String {
        serde_json::json!({
            "id": "tok-1",
            "name": "amp-orbs",
            "organization_id": "org-pat",
            "member_id": "member-pat",
            "email": "dev@example.com",
            "scopes": ["adr:query", "adr:review"],
            "expires_at": expires_at,
        })
        .to_string()
    }

    /// A config dir of the test's own, `ACTUAL_TOKEN` set to [`TOKEN`] and
    /// Actual's API at `api`.
    pub(crate) fn with_token(api: &str) -> (tempfile::TempDir, Vec<EnvGuard>) {
        let home = tempfile::tempdir().unwrap();
        let guards = vec![
            EnvGuard::remove("ACTUAL_CONFIG"),
            EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap()),
            EnvGuard::set("ACTUAL_API_URL", api),
            EnvGuard::set(ENV, TOKEN),
        ];
        (home, guards)
    }

    fn current(server: &mut mockito::Server, status: usize, expect: usize) -> mockito::Mock {
        server
            .mock("GET", CURRENT_PATH)
            .match_header("authorization", format!("Bearer {TOKEN}").as_str())
            .with_status(status)
            .with_header("content-type", "application/json")
            .with_body(identity_json(Utc::now() + Duration::days(90)))
            .expect(expect)
            .create()
    }

    fn rewrite(home: &std::path::Path, edit: impl FnOnce(&mut Entry)) {
        let path = home.join(CACHE_FILENAME);
        let mut entry: Entry =
            crate::yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        edit(&mut entry);
        std::fs::write(path, crate::yaml::to_string(&entry).unwrap()).unwrap();
    }

    /// The first load asks once and builds the sign-in from the answer; the
    /// next uses the cache with no request. The cache file is owner-only,
    /// holds no token, and only the current token's entry.
    #[test]
    fn test_a_cached_identity_makes_no_request() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let mut server = mockito::Server::new();
        let (home, _env) = with_token(&server.url());
        let mock = current(&mut server, 200, 1);
        std::fs::write(home.path().join(CACHE_FILENAME), "key: another-token\n").unwrap();

        let first = crate::auth::store::load().unwrap().unwrap();
        let second = crate::auth::store::lock().unwrap().load().unwrap().unwrap();

        mock.assert();
        assert_eq!(first, second);
        assert_eq!(first.access_token, TOKEN);
        assert_eq!(first.refresh_token, "");
        assert_eq!(first.organization_id, "org-pat");
        assert_eq!(first.member_id, "member-pat");
        assert_eq!(first.scope.as_deref(), Some("adr:query adr:review"));
        assert_eq!(first.auth_url, None);
        let text = std::fs::read_to_string(home.path().join(CACHE_FILENAME)).unwrap();
        assert!(
            !text.contains(TOKEN) && !text.contains("actl_pat_"),
            "{text}"
        );
        assert!(!text.contains("another-token"), "{text}");
        assert!(text.contains(&key(TOKEN)), "{text}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(home.path().join(CACHE_FILENAME)).unwrap();
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }
    }

    /// An identity checked more than 24 hours ago is checked again, once.
    #[test]
    fn test_a_stale_identity_is_checked_again() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let mut server = mockito::Server::new();
        let (home, _env) = with_token(&server.url());
        let mock = current(&mut server, 200, 2);
        identity(TOKEN).unwrap();
        rewrite(home.path(), |entry| {
            let ago = Utc::now() - Duration::hours(25);
            entry.identity.as_mut().unwrap().checked_at = ago;
            entry.last_attempt_at = Some(ago);
        });

        identity(TOKEN).unwrap();
        identity(TOKEN).unwrap();

        mock.assert();
    }

    /// A 401 is remembered: the next load fails the same way with no request.
    #[test]
    fn test_a_rejection_is_cached() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let mut server = mockito::Server::new();
        let (_home, _env) = with_token(&server.url());
        let mock = current(&mut server, 401, 1);

        for _ in 0..2 {
            let err = crate::auth::store::load().unwrap_err();
            assert_eq!(err.to_string(), REJECTED);
            assert_eq!(err.exit_code(), 2);
            assert_eq!(err.hint(), None);
        }
        mock.assert();
    }

    /// A failed request keeps an identity another command stored while it
    /// waited, so the next command uses it instead of backing off with none.
    #[test]
    fn test_a_failed_request_keeps_an_identity_stored_meanwhile() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let mut server = mockito::Server::new();
        let (_home, _env) = with_token(&server.url());
        let mock = server
            .mock("GET", CURRENT_PATH)
            .with_status(503)
            .with_body_from_request(|_| {
                // The other command's answer lands while this request waits.
                let identity: Identity =
                    serde_json::from_str(&identity_json(Utc::now() + Duration::days(90))).unwrap();
                write_entry(&Entry {
                    key: key(TOKEN),
                    identity: Some(CachedIdentity {
                        identity,
                        checked_at: Utc::now(),
                    }),
                    ..Entry::default()
                });
                Vec::new()
            })
            .expect(1)
            .create();

        assert_eq!(identity(TOKEN).unwrap().organization_id, "org-pat");
        assert_eq!(identity(TOKEN).unwrap().organization_id, "org-pat");

        mock.assert();
        assert!(read_entry(TOKEN).last_attempt_at.is_some());
    }

    /// A cached identity whose token has expired is refused with no request.
    #[test]
    fn test_an_expired_token_is_refused_from_the_cache() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let mut server = mockito::Server::new();
        let (home, _env) = with_token(&server.url());
        let mock = current(&mut server, 200, 1);
        identity(TOKEN).unwrap();
        rewrite(home.path(), |entry| {
            entry.identity.as_mut().unwrap().identity.expires_at =
                Utc::now() - Duration::minutes(1);
        });

        let err = identity(TOKEN).unwrap_err();

        assert_eq!(err.to_string(), REJECTED);
        mock.assert();
    }

    /// A 503 or a 404 isn't a rejection: it's reported as unreachable (exit 3)
    /// and backs off, and `whoami --check` asks again at once.
    #[test]
    fn test_503_and_404_are_not_cached_as_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        for status in [503, 404] {
            let mut server = mockito::Server::new();
            let (home, _env) = with_token(&server.url());
            let mock = current(&mut server, status, 2);

            let err = crate::auth::store::load().unwrap_err();
            assert_eq!(err.exit_code(), 3, "{status}: {err}");
            assert!(err.to_string().contains(&status.to_string()), "{err}");
            let backed_off = crate::auth::store::load().unwrap_err();
            assert_eq!(backed_off.exit_code(), 3, "{backed_off}");
            assert_eq!(check(TOKEN).unwrap_err().exit_code(), 3);

            mock.assert();
            assert!(home.path().join(CACHE_FILENAME).exists());
            assert_eq!(read_entry(TOKEN).rejected_at, None);
        }
    }

    /// A server that can't be reached is tried once in five minutes; a
    /// successful identity stays usable through it until the token expires.
    #[test]
    fn test_unreachable_backs_off_and_keeps_the_identity() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let mut server = mockito::Server::new();
        let (home, _env) = with_token(&server.url());
        let ok = current(&mut server, 200, 1);
        identity(TOKEN).unwrap();
        ok.assert();
        let _closed = EnvGuard::set("ACTUAL_API_URL", "http://127.0.0.1:1");
        rewrite(home.path(), |entry| {
            let ago = Utc::now() - Duration::hours(25);
            entry.identity.as_mut().unwrap().checked_at = ago;
            entry.last_attempt_at = Some(ago);
        });

        assert_eq!(identity(TOKEN).unwrap().organization_id, "org-pat");
        let attempted = read_entry(TOKEN).last_attempt_at.unwrap();
        assert!(Utc::now() - attempted < Duration::minutes(1));
        assert_eq!(identity(TOKEN).unwrap().organization_id, "org-pat");
        assert_eq!(
            read_entry(TOKEN).last_attempt_at,
            Some(attempted),
            "backed off"
        );

        std::fs::remove_file(home.path().join(CACHE_FILENAME)).unwrap();
        let err = identity(TOKEN).unwrap_err();
        assert_eq!(err.exit_code(), 3, "{err}");
        let attempted = read_entry(TOKEN).last_attempt_at;
        assert_eq!(identity(TOKEN).unwrap_err().exit_code(), 3);
        assert_eq!(read_entry(TOKEN).last_attempt_at, attempted, "backed off");
    }

    /// A server that never answers costs at most about three seconds.
    #[test]
    fn test_a_silent_server_times_out_within_three_seconds() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let (_home, _env) = with_token(&api);

        let started = std::time::Instant::now();
        let err = identity(TOKEN).unwrap_err();

        assert_eq!(err.exit_code(), 3, "{err}");
        assert!(err.to_string().contains("3 seconds"), "{err}");
        assert!(started.elapsed() < StdDuration::from_secs(5));
        drop(listener);
    }

    /// A value without the PAT prefix is refused before any request.
    #[test]
    fn test_a_value_that_isnt_a_pat_is_refused() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let mut server = mockito::Server::new();
        let (_home, _env) = with_token(&server.url());
        let mock = current(&mut server, 200, 0);
        let _value = EnvGuard::set(ENV, "eyJhbGciOiJSUzI1NiJ9.session.jwt");

        let err = crate::auth::store::load().unwrap_err();

        assert_eq!(err.to_string(), UNSUPPORTED);
        assert_eq!(err.exit_code(), 2);
        mock.assert();
    }

    /// The token goes only to Actual's api-service origins and loopback.
    #[test]
    fn test_only_actual_api_and_loopback_get_the_token() {
        for base in [
            "https://api-service.api.prod.actual.ai",
            "https://api-service.api.staging.actual.ai/",
            "https://API-SERVICE.api.staging.actual.ai:443/x",
            "http://127.0.0.1:4100",
            "http://localhost:3002",
            "http://[::1]:9",
        ] {
            assert!(allowed(base), "{base}");
        }
        for base in [
            "https://api.example.com",
            "http://api-service.api.prod.actual.ai",
            "https://api-service.api.prod.actual.ai:8443",
            "https://api-service.api.prod.actual.ai.evil.example",
            "https://user@api-service.api.prod.actual.ai",
            "https://app.actual.ai",
            "not a url",
        ] {
            assert!(!allowed(base), "{base}");
        }
    }

    /// A foreign `ACTUAL_API_URL` is refused before anything is sent.
    #[test]
    fn test_a_foreign_api_url_is_refused() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = with_token("https://api.example.com");

        let err = crate::auth::store::load().unwrap_err();
        assert_eq!(err.to_string(), FOREIGN_HOST);
        assert_eq!(err.exit_code(), 2);
        assert!(ensure_destination("https://api.example.com").is_err());
    }

    /// A command's --api-url must be the server ACTUAL_API_URL names, where the
    /// token's identity is checked; another allowed server is refused too.
    #[test]
    fn test_a_command_sends_the_token_only_to_actual_api_url() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_home, _env) = with_token("https://api-service.api.staging.actual.ai");

        assert!(ensure_destination("https://api-service.api.staging.actual.ai/").is_ok());
        for other in [
            "https://api-service.api.prod.actual.ai",
            "http://127.0.0.1:3002",
        ] {
            let err = ensure_destination(other).unwrap_err();
            assert_eq!(err.to_string(), OTHER_API_URL, "{other}");
            assert_eq!(err.exit_code(), 2);
        }
    }

    /// After a route's 401, the message follows what Actual AI says about the
    /// token now: still valid means the request was refused, not the token.
    #[test]
    fn test_a_refused_request_names_the_token_only_when_it_was_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let mut server = mockito::Server::new();
        let (_home, _env) = with_token(&server.url());

        let valid = current(&mut server, 200, 1);
        assert_eq!(
            explain(ActualError::NotLoggedIn).to_string(),
            REFUSED_REQUEST
        );
        valid.remove();
        current(&mut server, 401, 1);
        assert_eq!(explain(ActualError::NotLoggedIn).to_string(), REJECTED);
    }

    /// A PAT sign-in near its expiry is never renewed, so `credentials.yaml`,
    /// holding a session that would need renewal, is never read or touched.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn test_ensure_fresh_never_reads_credentials_yaml() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let mut server = mockito::Server::new_async().await;
        let (home, _env) = with_token(&server.url());
        let refresh = server
            .mock("POST", "/api/oauth/token")
            .expect(0)
            .create_async()
            .await;
        let stored = "access_token: oauth-at\nrefresh_token: oauth-rt\norganization_id: org-a\n\
                      member_id: m\nauth_url: http://127.0.0.1:1\n";
        std::fs::write(home.path().join("credentials.yaml"), stored).unwrap();
        let mut pat = to_credentials(
            TOKEN,
            serde_json::from_str(&identity_json(Utc::now() + Duration::seconds(30))).unwrap(),
        );
        pat.expires_at = Some(Utc::now() + Duration::seconds(30));

        let out = crate::auth::refresh::ensure_fresh(pat.clone())
            .await
            .unwrap();

        assert_eq!(out, pat);
        refresh.assert_async().await;
        assert_eq!(
            std::fs::read_to_string(home.path().join("credentials.yaml")).unwrap(),
            stored
        );
    }
}
