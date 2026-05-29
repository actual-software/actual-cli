//! Browser OAuth (authorization-code + PKCE) against the Actual AI OAuth
//! server — the platform-login flow for `actual login`.
//!
//! Flow: generate a PKCE pair + `state`, bind a loopback listener for the
//! redirect, send the user to `/authorize` in their browser, capture the
//! `code`, exchange it at `/api/oauth/token`, then resolve the signed-in
//! identity via `/whoami`. The resulting [`StoredCredentials`] are returned to
//! the caller to persist.
//!
//! The OAuth base URL is **not** the ADR api-service URL; it is the auth
//! server (the mock at `http://localhost:4000` during dev). It must be
//! supplied explicitly — see [`resolve_auth_url`].

use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use serde::Deserialize;

use crate::auth::loopback::LoopbackServer;
use crate::auth::pkce::{self, PkcePair};
use crate::auth::store::StoredCredentials;
use crate::error::ActualError;

/// Default OAuth client id for the CLI. Overridable via `ACTUAL_OAUTH_CLIENT_ID`.
const DEFAULT_CLIENT_ID: &str = "actual-cli";

/// Default requested scopes. `offline_access` is required to receive a
/// refresh token. Overridable via `ACTUAL_OAUTH_SCOPES`.
const DEFAULT_SCOPES: &str = "openid profile offline_access adr:query adr:review";

/// How long to wait for the user to complete sign-in in the browser.
pub const DEFAULT_LOGIN_TIMEOUT: Duration = Duration::from_secs(180);

const USER_AGENT_VALUE: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

/// Resolved configuration for one login attempt.
pub struct OAuthConfig {
    /// Auth server base URL, with any trailing slash trimmed.
    pub base_url: String,
    pub client_id: String,
    pub scopes: String,
}

impl OAuthConfig {
    /// Build a config for `base_url`, taking `client_id`/`scopes` from
    /// `ACTUAL_OAUTH_CLIENT_ID` / `ACTUAL_OAUTH_SCOPES` or their defaults.
    pub fn new(base_url: impl Into<String>) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let client_id = std::env::var("ACTUAL_OAUTH_CLIENT_ID")
            .unwrap_or_else(|_| DEFAULT_CLIENT_ID.to_string());
        let scopes =
            std::env::var("ACTUAL_OAUTH_SCOPES").unwrap_or_else(|_| DEFAULT_SCOPES.to_string());
        Self {
            base_url,
            client_id,
            scopes,
        }
    }
}

/// Resolve the auth server base URL: explicit `cli_flag` wins, else the
/// `ACTUAL_AUTH_URL` env var. There is intentionally **no** hardcoded
/// production default yet — the prod OAuth endpoint is not finalized, and we
/// will not bake a guessed URL into a published binary.
pub fn resolve_auth_url(cli_flag: Option<&str>) -> Result<String, ActualError> {
    if let Some(url) = cli_flag {
        return Ok(url.to_string());
    }
    if let Ok(url) = std::env::var("ACTUAL_AUTH_URL") {
        if !url.is_empty() {
            return Ok(url);
        }
    }
    Err(ActualError::ConfigError(
        "No auth server URL configured. Pass --api-url \
         (e.g. http://localhost:4000 for the mock auth server) or set ACTUAL_AUTH_URL."
            .to_string(),
    ))
}

/// `/api/oauth/token` response (authorization_code + refresh grants).
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default = "default_bearer")]
    token_type: String,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

fn default_bearer() -> String {
    "Bearer".to_string()
}

/// `/whoami` (and userinfo) response — the signed-in identity + selected org.
#[derive(Debug, Deserialize)]
struct WhoamiResponse {
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    email: Option<String>,
    organization_id: String,
    member_id: String,
    #[serde(default)]
    scope: Option<String>,
}

/// Build the front-channel `/authorize` URL with PKCE + state (+ optional
/// pre-selected organization).
pub fn build_authorize_url(
    cfg: &OAuthConfig,
    redirect_uri: &str,
    code_challenge: &str,
    state: &str,
    organization_id: Option<&str>,
) -> Result<String, ActualError> {
    let mut url = reqwest::Url::parse(&format!("{}/authorize", cfg.base_url))
        .map_err(|e| ActualError::ConfigError(format!("Invalid auth server URL: {e}")))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("response_type", "code")
            .append_pair("client_id", &cfg.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("code_challenge", code_challenge)
            .append_pair("code_challenge_method", PkcePair::METHOD)
            .append_pair("state", state)
            .append_pair("scope", &cfg.scopes);
        if let Some(org) = organization_id {
            q.append_pair("organization_id", org);
        }
    }
    Ok(url.to_string())
}

/// Build an HTTP client for the auth server. Enforces HTTPS for non-loopback
/// URLs so tokens are never sent in clear text (loopback `http://` is allowed
/// for the local mock).
fn build_http_client(base_url: &str) -> Result<reqwest::Client, ActualError> {
    let is_localhost = base_url.starts_with("http://localhost")
        || base_url.starts_with("http://127.0.0.1")
        || base_url.starts_with("http://[::1]");
    if !base_url.starts_with("https://") && !is_localhost {
        return Err(ActualError::ConfigError(
            "Auth server URL must use HTTPS (got a non-HTTPS, non-loopback URL). \
             Use https:// to protect your credentials."
                .to_string(),
        ));
    }
    reqwest::Client::builder()
        .user_agent(USER_AGENT_VALUE)
        .https_only(!is_localhost)
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| ActualError::ApiError(format!("Failed to build HTTP client: {e}")))
}

/// Map an unsuccessful auth-server response to an [`ActualError`], redacting
/// nothing sensitive (these bodies are OAuth error JSON, not secrets).
async fn auth_error(context: &str, response: reqwest::Response) -> ActualError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let truncated: String = body.chars().take(2048).collect();
    ActualError::ApiError(format!("{context} (HTTP {status}): {truncated}"))
}

/// Exchange an authorization code for tokens (PKCE back-channel).
async fn exchange_code(
    http: &reqwest::Client,
    base_url: &str,
    client_id: &str,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<TokenResponse, ActualError> {
    let url = format!("{base_url}/api/oauth/token");
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("code_verifier", code_verifier),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
    ];
    let response = http
        .post(&url)
        .form(&form)
        .send()
        .await
        .map_err(|e| ActualError::ApiError(format!("Token exchange request failed: {e}")))?;
    if !response.status().is_success() {
        return Err(auth_error("Token exchange failed", response).await);
    }
    response
        .json::<TokenResponse>()
        .await
        .map_err(|e| ActualError::ApiError(format!("Failed to parse token response: {e}")))
}

/// Resolve the signed-in identity (and selected org) via `/whoami`.
async fn fetch_identity(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
) -> Result<WhoamiResponse, ActualError> {
    let url = format!("{base_url}/whoami");
    let response = http
        .get(&url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| ActualError::ApiError(format!("whoami request failed: {e}")))?;
    if !response.status().is_success() {
        return Err(auth_error("whoami failed", response).await);
    }
    response
        .json::<WhoamiResponse>()
        .await
        .map_err(|e| ActualError::ApiError(format!("Failed to parse whoami response: {e}")))
}

/// Assemble [`StoredCredentials`] from a token response + resolved identity.
/// Pure mapping, factored out for testing.
fn credentials_from(token: TokenResponse, who: WhoamiResponse) -> StoredCredentials {
    let expires_at = token
        .expires_in
        .map(|secs| Utc::now() + ChronoDuration::seconds(secs));
    StoredCredentials {
        access_token: token.access_token,
        refresh_token: token.refresh_token.unwrap_or_default(),
        token_type: token.token_type,
        expires_at,
        scope: token.scope.or(who.scope),
        organization_id: who.organization_id,
        member_id: who.member_id,
        email: who.email,
        subject: who.sub,
        // Set by `login` once the auth server URL is known.
        auth_url: None,
    }
}

/// Run the full browser OAuth login and return credentials to persist.
///
/// `organization_id` pre-selects an org (required for multi-org accounts —
/// otherwise the consent page handles selection). When `open_browser` is true
/// the default browser is launched; the URL is always printed as a fallback.
pub async fn login(
    cfg: &OAuthConfig,
    organization_id: Option<String>,
    open_browser: bool,
    timeout: Duration,
) -> Result<StoredCredentials, ActualError> {
    let http = build_http_client(&cfg.base_url)?;
    let pkce_pair = PkcePair::generate();
    let state = pkce::generate_state();

    let server = LoopbackServer::bind().await?;
    let redirect_uri = server.redirect_uri();

    let auth_url = build_authorize_url(
        cfg,
        &redirect_uri,
        &pkce_pair.challenge,
        &state,
        organization_id.as_deref(),
    )?;

    println!("Opening your browser to sign in to Actual AI…");
    println!("If it doesn't open, visit this URL:\n\n  {auth_url}\n");
    if open_browser {
        // Best-effort: failure here is non-fatal — the URL is already printed.
        let _ = open::that(&auth_url);
    }

    let redirect = server.wait_for_code(&state, timeout).await?;
    let token = exchange_code(
        &http,
        &cfg.base_url,
        &cfg.client_id,
        &redirect.code,
        &pkce_pair.verifier,
        &redirect_uri,
    )
    .await?;
    let who = fetch_identity(&http, &cfg.base_url, &token.access_token).await?;

    let mut creds = credentials_from(token, who);
    // Remember which auth server issued this session so `logout`/refresh can
    // reach the same endpoint without the user re-specifying it.
    creds.auth_url = Some(cfg.base_url.clone());
    Ok(creds)
}

/// Refresh an access token via the `refresh_token` grant (rotation: the server
/// returns a new refresh token and revokes the old one). Identity fields
/// (org/member/email/subject) and the auth URL are preserved from `creds`.
pub async fn refresh(creds: &StoredCredentials) -> Result<StoredCredentials, ActualError> {
    let base_url = creds.auth_url.clone().ok_or_else(|| {
        ActualError::ConfigError(
            "Stored credentials have no auth server URL — run `actual login` again.".to_string(),
        )
    })?;
    if creds.refresh_token.is_empty() {
        return Err(ActualError::NotLoggedIn);
    }
    let cfg = OAuthConfig::new(base_url);
    let http = build_http_client(&cfg.base_url)?;
    let token = refresh_tokens(&http, &cfg.base_url, &cfg.client_id, &creds.refresh_token).await?;
    Ok(apply_refresh(creds, token))
}

/// Back-channel `refresh_token` grant.
async fn refresh_tokens(
    http: &reqwest::Client,
    base_url: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<TokenResponse, ActualError> {
    let url = format!("{base_url}/api/oauth/token");
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
    ];
    let response = http
        .post(&url)
        .form(&form)
        .send()
        .await
        .map_err(|e| ActualError::ApiError(format!("Token refresh request failed: {e}")))?;
    if !response.status().is_success() {
        return Err(auth_error("Token refresh failed", response).await);
    }
    response
        .json::<TokenResponse>()
        .await
        .map_err(|e| ActualError::ApiError(format!("Failed to parse refresh response: {e}")))
}

/// Apply a refresh `TokenResponse` onto existing credentials, preserving the
/// identity fields the refresh response does not carry.
fn apply_refresh(old: &StoredCredentials, token: TokenResponse) -> StoredCredentials {
    let expires_at = token
        .expires_in
        .map(|secs| Utc::now() + ChronoDuration::seconds(secs));
    StoredCredentials {
        access_token: token.access_token,
        // Rotation: prefer the new refresh token; fall back to the old one if
        // the server didn't rotate it.
        refresh_token: token
            .refresh_token
            .unwrap_or_else(|| old.refresh_token.clone()),
        token_type: token.token_type,
        expires_at,
        scope: token.scope.or_else(|| old.scope.clone()),
        organization_id: old.organization_id.clone(),
        member_id: old.member_id.clone(),
        email: old.email.clone(),
        subject: old.subject.clone(),
        auth_url: old.auth_url.clone(),
    }
}

/// Best-effort server-side revocation (RFC 7009) of the session's refresh
/// token. Returns `Ok(())` with nothing to do if no auth URL is stored.
pub async fn revoke(creds: &StoredCredentials) -> Result<(), ActualError> {
    let Some(base_url) = creds.auth_url.clone() else {
        return Ok(());
    };
    let cfg = OAuthConfig::new(base_url);
    let http = build_http_client(&cfg.base_url)?;
    // Revoke the refresh token when present (it is the long-lived credential);
    // otherwise fall back to the access token.
    let token = if !creds.refresh_token.is_empty() {
        creds.refresh_token.as_str()
    } else {
        creds.access_token.as_str()
    };
    let url = format!("{}/api/oauth/revoke", cfg.base_url);
    let form = [("token", token), ("client_id", cfg.client_id.as_str())];
    let response = http
        .post(&url)
        .form(&form)
        .send()
        .await
        .map_err(|e| ActualError::ApiError(format!("Revocation request failed: {e}")))?;
    if !response.status().is_success() {
        return Err(auth_error("Revocation failed", response).await);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

    fn test_cfg(base_url: &str) -> OAuthConfig {
        OAuthConfig {
            base_url: base_url.trim_end_matches('/').to_string(),
            client_id: "actual-cli".to_string(),
            scopes: "openid profile offline_access".to_string(),
        }
    }

    #[test]
    fn test_build_authorize_url_contains_required_params() {
        let cfg = test_cfg("http://localhost:4000");
        let url = build_authorize_url(
            &cfg,
            "http://127.0.0.1:55000/callback",
            "challenge123",
            "state456",
            None,
        )
        .unwrap();
        assert!(url.starts_with("http://localhost:4000/authorize?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=actual-cli"));
        assert!(url.contains("code_challenge=challenge123"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=state456"));
        // redirect_uri must be percent-encoded.
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A55000%2Fcallback"));
        assert!(!url.contains("organization_id="));
    }

    #[test]
    fn test_build_authorize_url_includes_org_when_given() {
        let cfg = test_cfg("http://localhost:4000");
        let url = build_authorize_url(&cfg, "http://127.0.0.1:1/callback", "c", "s", Some("org-1"))
            .unwrap();
        assert!(url.contains("organization_id=org-1"));
    }

    #[test]
    fn test_build_authorize_url_rejects_bad_base() {
        let cfg = test_cfg("not a url");
        let err =
            build_authorize_url(&cfg, "http://127.0.0.1:1/callback", "c", "s", None).unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(_)));
    }

    #[test]
    fn test_resolve_auth_url_flag_wins() {
        let url = resolve_auth_url(Some("http://localhost:4000")).unwrap();
        assert_eq!(url, "http://localhost:4000");
    }

    #[test]
    fn test_resolve_auth_url_env_fallback() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard::set("ACTUAL_AUTH_URL", "http://localhost:4000");
        let url = resolve_auth_url(None).unwrap();
        assert_eq!(url, "http://localhost:4000");
    }

    #[test]
    fn test_resolve_auth_url_missing_errors() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard::remove("ACTUAL_AUTH_URL");
        let err = resolve_auth_url(None).unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(ref m) if m.contains("--api-url")));
    }

    #[test]
    fn test_build_http_client_rejects_http_non_localhost() {
        let err = build_http_client("http://example.com").unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(ref m) if m.contains("HTTPS")));
    }

    #[test]
    fn test_build_http_client_allows_localhost_http() {
        assert!(build_http_client("http://127.0.0.1:4000").is_ok());
        assert!(build_http_client("https://auth.example.com").is_ok());
    }

    #[test]
    fn test_credentials_from_maps_fields() {
        let token = TokenResponse {
            access_token: "at".to_string(),
            token_type: "Bearer".to_string(),
            expires_in: Some(3600),
            refresh_token: Some("rt".to_string()),
            scope: Some("openid adr:query".to_string()),
        };
        let who = WhoamiResponse {
            sub: Some("mock-user-0001".to_string()),
            email: Some("dev@example.com".to_string()),
            organization_id: "mock-org-0001".to_string(),
            member_id: "mock-member-0001".to_string(),
            scope: None,
        };
        let creds = credentials_from(token, who);
        assert_eq!(creds.access_token, "at");
        assert_eq!(creds.refresh_token, "rt");
        assert_eq!(creds.organization_id, "mock-org-0001");
        assert_eq!(creds.member_id, "mock-member-0001");
        assert_eq!(creds.subject.as_deref(), Some("mock-user-0001"));
        assert!(creds.expires_at.is_some());
    }

    #[test]
    fn test_credentials_from_missing_refresh_defaults_empty() {
        let token = TokenResponse {
            access_token: "at".to_string(),
            token_type: "Bearer".to_string(),
            expires_in: None,
            refresh_token: None,
            scope: None,
        };
        let who = WhoamiResponse {
            sub: None,
            email: None,
            organization_id: "o".to_string(),
            member_id: "m".to_string(),
            scope: Some("from-whoami".to_string()),
        };
        let creds = credentials_from(token, who);
        assert_eq!(creds.refresh_token, "");
        assert_eq!(creds.expires_at, None);
        // Falls back to the whoami scope when the token omits it.
        assert_eq!(creds.scope.as_deref(), Some("from-whoami"));
    }

    #[tokio::test]
    async fn test_exchange_code_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/oauth/token")
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("grant_type".into(), "authorization_code".into()),
                mockito::Matcher::UrlEncoded("code".into(), "the-code".into()),
                mockito::Matcher::UrlEncoded("code_verifier".into(), "the-verifier".into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"access_token":"at","token_type":"Bearer","expires_in":3600,"refresh_token":"rt","scope":"openid"}"#,
            )
            .create_async()
            .await;

        let http = build_http_client(&server.url()).unwrap();
        let token = exchange_code(
            &http,
            &server.url(),
            "actual-cli",
            "the-code",
            "the-verifier",
            "http://127.0.0.1:1/callback",
        )
        .await
        .unwrap();
        assert_eq!(token.access_token, "at");
        assert_eq!(token.refresh_token.as_deref(), Some("rt"));
        assert_eq!(token.expires_in, Some(3600));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_exchange_code_error_status() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/oauth/token")
            .with_status(400)
            .with_body(r#"{"error":"invalid_grant"}"#)
            .create_async()
            .await;

        let http = build_http_client(&server.url()).unwrap();
        let err = exchange_code(
            &http,
            &server.url(),
            "actual-cli",
            "bad",
            "v",
            "http://127.0.0.1:1/callback",
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, ActualError::ApiError(ref m) if m.contains("invalid_grant")),
            "got: {err:?}"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_fetch_identity_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/whoami")
            .match_header("authorization", "Bearer at-123")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"sub":"mock-user-0001","email":"dev@example.com","organization_id":"mock-org-0001","member_id":"mock-member-0001","scope":"openid"}"#,
            )
            .create_async()
            .await;

        let http = build_http_client(&server.url()).unwrap();
        let who = fetch_identity(&http, &server.url(), "at-123")
            .await
            .unwrap();
        assert_eq!(who.organization_id, "mock-org-0001");
        assert_eq!(who.member_id, "mock-member-0001");
        assert_eq!(who.email.as_deref(), Some("dev@example.com"));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_fetch_identity_unauthorized() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/whoami")
            .with_status(401)
            .with_body(r#"{"error":"invalid_token"}"#)
            .create_async()
            .await;

        let http = build_http_client(&server.url()).unwrap();
        let err = fetch_identity(&http, &server.url(), "bad")
            .await
            .unwrap_err();
        assert!(matches!(err, ActualError::ApiError(ref m) if m.contains("invalid_token")));
        mock.assert_async().await;
    }

    fn creds_with_auth(auth_url: Option<String>, refresh_token: &str) -> StoredCredentials {
        StoredCredentials {
            access_token: "old-access".to_string(),
            refresh_token: refresh_token.to_string(),
            token_type: "Bearer".to_string(),
            expires_at: None,
            scope: Some("openid".to_string()),
            organization_id: "mock-org-0001".to_string(),
            member_id: "mock-member-0001".to_string(),
            email: Some("dev@example.com".to_string()),
            subject: Some("mock-user-0001".to_string()),
            auth_url,
        }
    }

    #[test]
    fn test_apply_refresh_rotates_and_preserves_identity() {
        let old = creds_with_auth(Some("http://localhost:4000".to_string()), "old-refresh");
        let token = TokenResponse {
            access_token: "new-access".to_string(),
            token_type: "Bearer".to_string(),
            expires_in: Some(7200),
            refresh_token: Some("new-refresh".to_string()),
            scope: None,
        };
        let refreshed = apply_refresh(&old, token);
        assert_eq!(refreshed.access_token, "new-access");
        assert_eq!(refreshed.refresh_token, "new-refresh"); // rotated
        assert!(refreshed.expires_at.is_some());
        // Identity + auth URL preserved from the old credentials.
        assert_eq!(refreshed.organization_id, "mock-org-0001");
        assert_eq!(refreshed.member_id, "mock-member-0001");
        assert_eq!(refreshed.subject.as_deref(), Some("mock-user-0001"));
        assert_eq!(refreshed.auth_url.as_deref(), Some("http://localhost:4000"));
        // Scope falls back to the old one when the refresh omits it.
        assert_eq!(refreshed.scope.as_deref(), Some("openid"));
    }

    #[test]
    fn test_apply_refresh_keeps_old_refresh_when_not_rotated() {
        let old = creds_with_auth(Some("http://localhost:4000".to_string()), "keep-me");
        let token = TokenResponse {
            access_token: "new-access".to_string(),
            token_type: "Bearer".to_string(),
            expires_in: None,
            refresh_token: None, // server did not rotate
            scope: Some("openid adr:query".to_string()),
        };
        let refreshed = apply_refresh(&old, token);
        assert_eq!(refreshed.refresh_token, "keep-me");
        assert_eq!(refreshed.scope.as_deref(), Some("openid adr:query"));
    }

    #[tokio::test]
    async fn test_refresh_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/oauth/token")
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("grant_type".into(), "refresh_token".into()),
                mockito::Matcher::UrlEncoded("refresh_token".into(), "old-refresh".into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"access_token":"fresh","token_type":"Bearer","expires_in":3600,"refresh_token":"rotated"}"#,
            )
            .create_async()
            .await;

        let creds = creds_with_auth(Some(server.url()), "old-refresh");
        let refreshed = refresh(&creds).await.unwrap();
        assert_eq!(refreshed.access_token, "fresh");
        assert_eq!(refreshed.refresh_token, "rotated");
        assert_eq!(refreshed.organization_id, "mock-org-0001");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_refresh_no_auth_url_errors() {
        let creds = creds_with_auth(None, "r");
        let err = refresh(&creds).await.unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(ref m) if m.contains("auth server URL")));
    }

    #[tokio::test]
    async fn test_refresh_empty_token_is_not_logged_in() {
        let creds = creds_with_auth(Some("http://localhost:4000".to_string()), "");
        let err = refresh(&creds).await.unwrap_err();
        assert!(matches!(err, ActualError::NotLoggedIn));
    }

    #[tokio::test]
    async fn test_revoke_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/oauth/revoke")
            .match_body(mockito::Matcher::UrlEncoded(
                "token".into(),
                "the-refresh".into(),
            ))
            .with_status(200)
            .create_async()
            .await;

        let creds = creds_with_auth(Some(server.url()), "the-refresh");
        revoke(&creds).await.unwrap();
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_revoke_no_auth_url_is_noop_ok() {
        let creds = creds_with_auth(None, "r");
        // Nothing to revoke against; should succeed silently.
        revoke(&creds).await.unwrap();
    }
}
