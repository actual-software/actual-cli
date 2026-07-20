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

/// Default scopes for the browserless device-authorization grant. Colon-form
/// resource scopes only; `offline_access` is intentionally omitted.
///
/// Verified against the OAuth server's device-code grant: an approved device
/// login mints a full session (access + refresh) regardless of the requested
/// scopes, so it receives a refresh token without `offline_access` and refreshes
/// the same way the browser session does. `offline_access` is therefore
/// unnecessary here — the refresh token is issued regardless of scope.
/// Overridable via `ACTUAL_OAUTH_SCOPES`.
const DEFAULT_DEVICE_SCOPES: &str = "adr:query adr:review mcp:invoke";

/// URN grant type for the OAuth 2.0 device-authorization grant (RFC 8628 §3.4).
const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Poll interval (seconds) used when the authorization response omits
/// `interval` (RFC 8628 §3.2 recommends a 5-second default).
const DEFAULT_DEVICE_INTERVAL_SECS: i64 = 5;

/// Seconds added to the poll interval on a `slow_down` response (RFC 8628 §3.5).
const DEVICE_SLOW_DOWN_INCREMENT_SECS: i64 = 5;

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

    /// Build a config for the browserless device-authorization grant. Same
    /// client-id resolution as [`OAuthConfig::new`], but defaults to the
    /// colon-form device scopes ([`DEFAULT_DEVICE_SCOPES`]).
    pub fn new_device(base_url: impl Into<String>) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let client_id = std::env::var("ACTUAL_OAUTH_CLIENT_ID")
            .unwrap_or_else(|_| DEFAULT_CLIENT_ID.to_string());
        let scopes = std::env::var("ACTUAL_OAUTH_SCOPES")
            .unwrap_or_else(|_| DEFAULT_DEVICE_SCOPES.to_string());
        Self {
            base_url,
            client_id,
            scopes,
        }
    }
}

/// Production OAuth server base URL — the canonical app origin (matches the
/// server's `CANONICAL_APP_DOMAIN`). This is what `login`/`whoami` default to
/// when nothing is passed; override with `--api-url` or `ACTUAL_AUTH_URL` for
/// staging (`https://app.staging.actual.ai`) or local (`http://localhost:…`).
pub const DEFAULT_AUTH_URL: &str = "https://app.actual.ai";

/// Resolve the auth server base URL. Precedence: explicit `cli_flag` (`--api-url`)
/// wins, then the `ACTUAL_AUTH_URL` env var, then the production default
/// ([`DEFAULT_AUTH_URL`]). The default makes `actual login` work out of the box
/// against prod; non-prod environments pass an override.
pub fn resolve_auth_url(cli_flag: Option<&str>) -> Result<String, ActualError> {
    if let Some(url) = cli_flag {
        return Ok(url.to_string());
    }
    if let Ok(url) = std::env::var("ACTUAL_AUTH_URL") {
        if !url.is_empty() {
            return Ok(url);
        }
    }
    Ok(DEFAULT_AUTH_URL.to_string())
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
    opener: &dyn Fn(&str),
    timeout: Duration,
) -> Result<StoredCredentials, ActualError> {
    let pkce_pair = PkcePair::generate();
    let state = pkce::generate_state();
    let server = LoopbackServer::bind().await?;
    login_with(
        cfg,
        organization_id,
        open_browser,
        opener,
        timeout,
        pkce_pair,
        state,
        server,
    )
    .await
}

/// Inner login flow with the PKCE pair, `state`, and bound loopback injected.
///
/// Splitting these out of [`login`] lets tests drive the full happy path
/// (feed the loopback with a redirect carrying a known `state`) without a real
/// browser. In production [`login`] supplies freshly generated/bound values.
#[allow(clippy::too_many_arguments)]
async fn login_with(
    cfg: &OAuthConfig,
    organization_id: Option<String>,
    open_browser: bool,
    opener: &dyn Fn(&str),
    timeout: Duration,
    pkce_pair: PkcePair,
    state: String,
    server: LoopbackServer,
) -> Result<StoredCredentials, ActualError> {
    let http = build_http_client(&cfg.base_url)?;
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
        // Best-effort: the URL is already printed, so a failure to launch is
        // non-fatal. The concrete opener is injected by the caller.
        opener(&auth_url);
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

// ── Device-authorization grant (RFC 8628) ───────────────────────────────────
//
// The browserless flow for `actual login --device`: request a device + user
// code, show the user where to approve, then poll the token endpoint until the
// grant is approved, denied, or the code expires. Reuses the same HTTP client
// guard, `/whoami` identity lookup, and credential mapping as the browser flow.

/// `/api/oauth/device_authorization` success response (RFC 8628 §3.2).
#[derive(Debug, Deserialize)]
pub struct DeviceCodeResponse {
    /// Opaque code the client polls the token endpoint with.
    pub device_code: String,
    /// Short, human-enterable code shown to the user.
    pub user_code: String,
    /// Where the user goes to approve the request.
    pub verification_uri: String,
    /// Convenience URL with the user code pre-filled (optional per the RFC).
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    /// Minimum seconds the server wants between polls (optional; default 5).
    #[serde(default)]
    pub interval: Option<i64>,
    /// Lifetime of the device code, in seconds.
    pub expires_in: i64,
}

/// The result of a single poll of the token endpoint (RFC 8628 §3.4–3.5).
#[derive(Debug)]
enum DevicePollOutcome {
    /// Approved — carries the issued session tokens.
    Approved(TokenResponse),
    /// Not yet approved; keep polling at the current interval.
    Pending,
    /// The server asked us to poll less frequently.
    SlowDown,
    /// The user declined the request in the browser.
    Denied,
    /// The device code expired before it was approved.
    Expired,
}

/// OAuth error body the token endpoint returns for a not-yet-complete or
/// failed device grant (RFC 8628 §3.5 reuses the OAuth `error` field).
#[derive(Debug, Deserialize)]
struct DeviceTokenErrorBody {
    error: String,
}

/// Request a device + user code to start the browserless flow (RFC 8628 §3.1).
async fn request_device_code(
    http: &reqwest::Client,
    base_url: &str,
    client_id: &str,
    scopes: &str,
) -> Result<DeviceCodeResponse, ActualError> {
    let url = format!("{base_url}/api/oauth/device_authorization");
    let form = [("client_id", client_id), ("scope", scopes)];
    let response = http
        .post(&url)
        .form(&form)
        .send()
        .await
        .map_err(|e| ActualError::ApiError(format!("Device code request failed: {e}")))?;
    if !response.status().is_success() {
        return Err(auth_error("Device authorization failed", response).await);
    }
    response.json::<DeviceCodeResponse>().await.map_err(|e| {
        ActualError::ApiError(format!(
            "Failed to parse device authorization response: {e}"
        ))
    })
}

/// Poll the token endpoint once with the device grant. `authorization_pending`
/// and `slow_down` come back as an HTTP 4xx carrying an OAuth `error` body, so
/// a non-success status is not necessarily fatal — the error code decides.
async fn poll_device_token(
    http: &reqwest::Client,
    base_url: &str,
    client_id: &str,
    device_code: &str,
) -> Result<DevicePollOutcome, ActualError> {
    let url = format!("{base_url}/api/oauth/token");
    let form = [
        ("grant_type", DEVICE_GRANT_TYPE),
        ("device_code", device_code),
        ("client_id", client_id),
    ];
    let response = http
        .post(&url)
        .form(&form)
        .send()
        .await
        .map_err(|e| ActualError::ApiError(format!("Device token poll failed: {e}")))?;
    let status = response.status();
    if status.is_success() {
        let token = response
            .json::<TokenResponse>()
            .await
            .map_err(|e| ActualError::ApiError(format!("Failed to parse token response: {e}")))?;
        return Ok(DevicePollOutcome::Approved(token));
    }
    // Non-2xx: the RFC 8628 §3.5 status codes arrive as a JSON `error`. Anything
    // we don't recognize (or a body without an `error`) is a hard failure.
    let body = response.text().await.unwrap_or_default();
    let code = serde_json::from_str::<DeviceTokenErrorBody>(&body)
        .map(|b| b.error)
        .unwrap_or_default();
    match code.as_str() {
        "authorization_pending" => Ok(DevicePollOutcome::Pending),
        "slow_down" => Ok(DevicePollOutcome::SlowDown),
        "access_denied" => Ok(DevicePollOutcome::Denied),
        "expired_token" => Ok(DevicePollOutcome::Expired),
        _ => {
            let truncated: String = body.chars().take(2048).collect();
            Err(ActualError::ApiError(format!(
                "Device token poll failed (HTTP {status}): {truncated}"
            )))
        }
    }
}

/// Message shown (and returned as an error) when the device code is no longer
/// usable — whether the server said `expired_token` or our own poll budget ran
/// out. Names the command so the user can retry directly.
fn device_expired_error() -> ActualError {
    ActualError::ApiError(
        "The device code expired before it was approved. Run `actual login --device` again."
            .to_string(),
    )
}

/// Drive the full device-authorization flow and return credentials to persist.
///
/// Requests a code, hands it to `on_prompt` for display, then polls the token
/// endpoint — sleeping the server-requested interval between polls and backing
/// off on `slow_down` — until the grant is approved, denied, or expires.
pub async fn device_login(
    cfg: &OAuthConfig,
    on_prompt: &dyn Fn(&DeviceCodeResponse),
) -> Result<StoredCredentials, ActualError> {
    let http = build_http_client(&cfg.base_url)?;
    let device = request_device_code(&http, &cfg.base_url, &cfg.client_id, &cfg.scopes).await?;
    on_prompt(&device);

    let mut interval = device.interval.unwrap_or(DEFAULT_DEVICE_INTERVAL_SECS);
    if interval < 1 {
        interval = 1;
    }
    // Client-side budget so we stop polling once the code can no longer be
    // approved; the server's `expired_token` is still the authoritative signal.
    let mut remaining = device.expires_in;

    let token = loop {
        if remaining <= 0 {
            return Err(device_expired_error());
        }
        tokio::time::sleep(Duration::from_secs(interval as u64)).await;
        remaining -= interval;
        match poll_device_token(&http, &cfg.base_url, &cfg.client_id, &device.device_code).await? {
            DevicePollOutcome::Approved(token) => break token,
            DevicePollOutcome::Pending => {}
            DevicePollOutcome::SlowDown => interval += DEVICE_SLOW_DOWN_INCREMENT_SECS,
            DevicePollOutcome::Denied => {
                return Err(ActualError::ApiError(
                    "The sign-in request was denied in the browser.".to_string(),
                ));
            }
            DevicePollOutcome::Expired => return Err(device_expired_error()),
        }
    };

    let who = fetch_identity(&http, &cfg.base_url, &token.access_token).await?;
    let mut creds = credentials_from(token, who);
    // Remember which auth server issued this session so refresh/logout can reach
    // the same endpoint without the user re-specifying it.
    creds.auth_url = Some(cfg.base_url.clone());
    Ok(creds)
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
    fn test_resolve_auth_url_defaults_to_prod() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard::remove("ACTUAL_AUTH_URL");
        // With nothing passed, login targets the production OAuth server.
        let url = resolve_auth_url(None).unwrap();
        assert_eq!(url, DEFAULT_AUTH_URL);
        assert_eq!(url, "https://app.actual.ai");
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

    #[tokio::test]
    async fn test_login_with_success_drives_full_flow() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpStream;

        let mut server = mockito::Server::new_async().await;
        let tok = server
            .mock("POST", "/api/oauth/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            // token_type omitted → also exercises the `default_bearer` serde default.
            .with_body(
                r#"{"access_token":"at","expires_in":3600,"refresh_token":"rt","scope":"openid"}"#,
            )
            .create_async()
            .await;
        let who = server
            .mock("GET", "/whoami")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"sub":"u1","email":"dev@example.com","organization_id":"org-1","member_id":"m-1","scope":"openid"}"#,
            )
            .create_async()
            .await;

        let cfg = test_cfg(&server.url());
        let pkce = PkcePair::generate();
        let state = "known-state".to_string();
        let loopback = LoopbackServer::bind().await.unwrap();
        let port = loopback.port();

        // Feed the loopback with the redirect carrying the known state.
        let st = state.clone();
        tokio::spawn(async move {
            let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            let req = format!("GET /callback?code=the-code&state={st} HTTP/1.1\r\nHost: x\r\n\r\n");
            s.write_all(req.as_bytes()).await.unwrap();
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf).await;
        });

        // open_browser=true with a no-op opener covers the opener call without a browser.
        let opener = |_: &str| {};
        let creds = login_with(
            &cfg,
            Some("org-1".to_string()),
            true,
            &opener,
            Duration::from_secs(5),
            pkce,
            state,
            loopback,
        )
        .await
        .unwrap();

        assert_eq!(creds.access_token, "at");
        assert_eq!(creds.token_type, "Bearer"); // defaulted
        assert_eq!(creds.refresh_token, "rt");
        assert_eq!(creds.organization_id, "org-1");
        assert_eq!(creds.auth_url.as_deref(), Some(cfg.base_url.as_str()));
        tok.assert_async().await;
        who.assert_async().await;
    }

    #[tokio::test]
    async fn test_login_wrapper_times_out_without_redirect() {
        // Exercises the `login` wrapper (pkce/state/bind) + login_with up to the
        // wait; no redirect arrives, so it times out quickly.
        let cfg = test_cfg("http://127.0.0.1:1");
        let opener = |_: &str| {};
        let result = login(&cfg, None, true, &opener, Duration::from_millis(100)).await;
        assert!(
            matches!(result, Err(ActualError::ApiError(ref m)) if m.contains("Timed out")),
            "got: {result:?}"
        );
    }

    #[test]
    fn test_resolve_auth_url_empty_env_defaults_to_prod() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard::set("ACTUAL_AUTH_URL", "");
        // An empty env var is treated as unset → falls through to the prod default.
        let url = resolve_auth_url(None).unwrap();
        assert_eq!(url, DEFAULT_AUTH_URL);
    }

    #[tokio::test]
    async fn test_refresh_error_status() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/oauth/token")
            .with_status(400)
            .with_body(r#"{"error":"invalid_grant"}"#)
            .create_async()
            .await;
        let creds = creds_with_auth(Some(server.url()), "stale");
        let err = refresh(&creds).await.unwrap_err();
        assert!(matches!(err, ActualError::ApiError(ref m) if m.contains("invalid_grant")));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_revoke_uses_access_token_when_no_refresh() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/oauth/revoke")
            .match_body(mockito::Matcher::UrlEncoded(
                "token".into(),
                "the-access".into(),
            ))
            .with_status(200)
            .create_async()
            .await;
        let mut creds = creds_with_auth(Some(server.url()), ""); // empty refresh token
        creds.access_token = "the-access".to_string();
        revoke(&creds).await.unwrap();
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_revoke_error_status() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/oauth/revoke")
            .with_status(500)
            .with_body("boom")
            .create_async()
            .await;
        let creds = creds_with_auth(Some(server.url()), "r");
        let err = revoke(&creds).await.unwrap_err();
        assert!(matches!(err, ActualError::ApiError(ref m) if m.contains("Revocation failed")));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_login_with_authorize_url_error() {
        // base_url passes the loopback HTTP-client guard (localhost) but fails
        // URL parsing (invalid port) → exercises the build_authorize_url `?`
        // error path inside login_with.
        let cfg = test_cfg("http://127.0.0.1:99999");
        let server = LoopbackServer::bind().await.unwrap();
        let opener = |_: &str| {};
        let err = login_with(
            &cfg,
            None,
            false,
            &opener,
            Duration::from_secs(1),
            PkcePair::generate(),
            "s".to_string(),
            server,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(_)), "got: {err:?}");
    }

    // ── Device-authorization grant (RFC 8628) ──────────────────────────────

    const DEVICE_AUTH_OK: &str = r#"{"device_code":"dev-code-abc","user_code":"WXYZ-1234","verification_uri":"https://auth.example/device","verification_uri_complete":"https://auth.example/device?user_code=WXYZ-1234","interval":0,"expires_in":300}"#;
    const DEVICE_TOKEN_OK: &str = r#"{"access_token":"at-device","token_type":"Bearer","expires_in":3600,"refresh_token":"rt-device","scope":"adr:query adr:review mcp:invoke"}"#;
    const DEVICE_WHOAMI_OK: &str = r#"{"sub":"u-dev","email":"dev@example.com","organization_id":"org-dev","member_id":"m-dev","scope":"adr:query"}"#;

    #[test]
    fn test_new_device_uses_colon_scopes_and_default_client() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _c = EnvGuard::remove("ACTUAL_OAUTH_CLIENT_ID");
        let _s = EnvGuard::remove("ACTUAL_OAUTH_SCOPES");
        let cfg = OAuthConfig::new_device("https://auth.example/");
        assert_eq!(cfg.base_url, "https://auth.example"); // trailing slash trimmed
        assert_eq!(cfg.client_id, "actual-cli");
        assert_eq!(cfg.scopes, "adr:query adr:review mcp:invoke");
    }

    #[test]
    fn test_new_device_honors_scope_and_client_overrides() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _c = EnvGuard::set("ACTUAL_OAUTH_CLIENT_ID", "custom-client");
        let _s = EnvGuard::set("ACTUAL_OAUTH_SCOPES", "adr:query");
        let cfg = OAuthConfig::new_device("http://localhost:4000");
        assert_eq!(cfg.client_id, "custom-client");
        assert_eq!(cfg.scopes, "adr:query");
    }

    #[tokio::test]
    async fn test_request_device_code_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/oauth/device_authorization")
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("client_id".into(), "actual-cli".into()),
                mockito::Matcher::UrlEncoded(
                    "scope".into(),
                    "adr:query adr:review mcp:invoke".into(),
                ),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(DEVICE_AUTH_OK)
            .create_async()
            .await;

        let http = build_http_client(&server.url()).unwrap();
        let device = request_device_code(
            &http,
            &server.url(),
            "actual-cli",
            "adr:query adr:review mcp:invoke",
        )
        .await
        .unwrap();
        assert_eq!(device.device_code, "dev-code-abc");
        assert_eq!(device.user_code, "WXYZ-1234");
        assert_eq!(device.verification_uri, "https://auth.example/device");
        assert_eq!(
            device.verification_uri_complete.as_deref(),
            Some("https://auth.example/device?user_code=WXYZ-1234")
        );
        assert_eq!(device.interval, Some(0));
        assert_eq!(device.expires_in, 300);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_request_device_code_minimal_optional_fields_default() {
        let mut server = mockito::Server::new_async().await;
        // Omit verification_uri_complete + interval → both default via serde.
        let mock = server
            .mock("POST", "/api/oauth/device_authorization")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"device_code":"d","user_code":"U","verification_uri":"https://auth.example/device","expires_in":120}"#,
            )
            .create_async()
            .await;
        let http = build_http_client(&server.url()).unwrap();
        let device = request_device_code(&http, &server.url(), "actual-cli", "adr:query")
            .await
            .unwrap();
        assert_eq!(device.verification_uri_complete, None);
        assert_eq!(device.interval, None);
        assert_eq!(device.expires_in, 120);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_request_device_code_error_status() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/oauth/device_authorization")
            .with_status(400)
            .with_body(r#"{"error":"invalid_client"}"#)
            .create_async()
            .await;
        let http = build_http_client(&server.url()).unwrap();
        let err = request_device_code(&http, &server.url(), "bad", "adr:query")
            .await
            .unwrap_err();
        assert!(
            matches!(err, ActualError::ApiError(ref m) if m.contains("invalid_client")),
            "got: {err:?}"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_request_device_code_bad_json() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/oauth/device_authorization")
            .with_status(200)
            .with_body("not json")
            .create_async()
            .await;
        let http = build_http_client(&server.url()).unwrap();
        let err = request_device_code(&http, &server.url(), "actual-cli", "adr:query")
            .await
            .unwrap_err();
        assert!(
            matches!(err, ActualError::ApiError(ref m) if m.contains("parse device authorization")),
            "got: {err:?}"
        );
        mock.assert_async().await;
    }

    /// Mock the token endpoint with a device-grant body + status, then poll once.
    async fn poll_once_with(status: usize, body: &str) -> Result<DevicePollOutcome, ActualError> {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/api/oauth/token")
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "grant_type".into(),
                    "urn:ietf:params:oauth:grant-type:device_code".into(),
                ),
                mockito::Matcher::UrlEncoded("device_code".into(), "dev-code-abc".into()),
            ]))
            .with_status(status)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;
        let http = build_http_client(&server.url()).unwrap();
        poll_device_token(&http, &server.url(), "actual-cli", "dev-code-abc").await
    }

    #[tokio::test]
    async fn test_poll_device_token_approved() {
        let outcome = poll_once_with(200, DEVICE_TOKEN_OK).await.unwrap();
        match outcome {
            DevicePollOutcome::Approved(token) => {
                assert_eq!(token.access_token, "at-device");
                assert_eq!(token.refresh_token.as_deref(), Some("rt-device"));
            }
            other => panic!("expected Approved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_poll_device_token_pending() {
        let outcome = poll_once_with(400, r#"{"error":"authorization_pending"}"#)
            .await
            .unwrap();
        assert!(matches!(outcome, DevicePollOutcome::Pending));
    }

    #[tokio::test]
    async fn test_poll_device_token_slow_down() {
        let outcome = poll_once_with(400, r#"{"error":"slow_down"}"#)
            .await
            .unwrap();
        assert!(matches!(outcome, DevicePollOutcome::SlowDown));
    }

    #[tokio::test]
    async fn test_poll_device_token_denied() {
        let outcome = poll_once_with(400, r#"{"error":"access_denied"}"#)
            .await
            .unwrap();
        assert!(matches!(outcome, DevicePollOutcome::Denied));
    }

    #[tokio::test]
    async fn test_poll_device_token_expired() {
        let outcome = poll_once_with(400, r#"{"error":"expired_token"}"#)
            .await
            .unwrap();
        assert!(matches!(outcome, DevicePollOutcome::Expired));
    }

    #[tokio::test]
    async fn test_poll_device_token_unknown_error_is_hard_failure() {
        let err = poll_once_with(400, r#"{"error":"invalid_client"}"#)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ActualError::ApiError(ref m) if m.contains("invalid_client")),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_poll_device_token_non_json_body_is_hard_failure() {
        let err = poll_once_with(500, "upstream boom").await.unwrap_err();
        assert!(
            matches!(err, ActualError::ApiError(ref m) if m.contains("boom")),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_poll_device_token_success_bad_json() {
        let err = poll_once_with(200, "not json").await.unwrap_err();
        assert!(
            matches!(err, ActualError::ApiError(ref m) if m.contains("parse token")),
            "got: {err:?}"
        );
    }

    /// Build a device `OAuthConfig` pointing at a mock server url.
    fn device_cfg(base_url: &str) -> OAuthConfig {
        OAuthConfig {
            base_url: base_url.trim_end_matches('/').to_string(),
            client_id: "actual-cli".to_string(),
            scopes: "adr:query adr:review mcp:invoke".to_string(),
        }
    }

    #[tokio::test]
    async fn test_device_login_success_first_poll() {
        let mut server = mockito::Server::new_async().await;
        let auth = server
            .mock("POST", "/api/oauth/device_authorization")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(DEVICE_AUTH_OK)
            .create_async()
            .await;
        let tok = server
            .mock("POST", "/api/oauth/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(DEVICE_TOKEN_OK)
            .create_async()
            .await;
        let who = server
            .mock("GET", "/whoami")
            .match_header("authorization", "Bearer at-device")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(DEVICE_WHOAMI_OK)
            .create_async()
            .await;

        let cfg = device_cfg(&server.url());
        let prompted = std::sync::atomic::AtomicUsize::new(0);
        let on_prompt = |d: &DeviceCodeResponse| {
            assert_eq!(d.user_code, "WXYZ-1234");
            prompted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        };
        let creds = device_login(&cfg, &on_prompt).await.unwrap();

        assert_eq!(creds.access_token, "at-device");
        assert_eq!(creds.refresh_token, "rt-device");
        assert_eq!(creds.organization_id, "org-dev");
        assert_eq!(creds.member_id, "m-dev");
        assert_eq!(creds.auth_url.as_deref(), Some(cfg.base_url.as_str()));
        assert_eq!(prompted.load(std::sync::atomic::Ordering::SeqCst), 1);
        auth.assert_async().await;
        tok.assert_async().await;
        who.assert_async().await;
    }

    #[tokio::test]
    async fn test_device_login_pending_until_client_expiry() {
        // expires_in:1 with a floored 1s interval → one poll (Pending), then the
        // client-side budget is exhausted and we surface the expiry error.
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/api/oauth/device_authorization")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"device_code":"dev-code-abc","user_code":"U","verification_uri":"https://auth.example/d","interval":0,"expires_in":1}"#,
            )
            .create_async()
            .await;
        server
            .mock("POST", "/api/oauth/token")
            .with_status(400)
            .with_body(r#"{"error":"authorization_pending"}"#)
            .create_async()
            .await;

        let cfg = device_cfg(&server.url());
        let noop = |_: &DeviceCodeResponse| {};
        let err = device_login(&cfg, &noop).await.unwrap_err();
        assert!(
            matches!(err, ActualError::ApiError(ref m) if m.contains("expired")),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_device_login_slow_down_backs_off() {
        // slow_down bumps the interval; expires_in:1 exhausts the budget on the
        // next loop, so the arm executes without a long real sleep.
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/api/oauth/device_authorization")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"device_code":"dev-code-abc","user_code":"U","verification_uri":"https://auth.example/d","interval":0,"expires_in":1}"#,
            )
            .create_async()
            .await;
        server
            .mock("POST", "/api/oauth/token")
            .with_status(400)
            .with_body(r#"{"error":"slow_down"}"#)
            .create_async()
            .await;

        let cfg = device_cfg(&server.url());
        let noop = |_: &DeviceCodeResponse| {};
        let err = device_login(&cfg, &noop).await.unwrap_err();
        assert!(
            matches!(err, ActualError::ApiError(ref m) if m.contains("expired")),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_device_login_denied() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/api/oauth/device_authorization")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(DEVICE_AUTH_OK)
            .create_async()
            .await;
        server
            .mock("POST", "/api/oauth/token")
            .with_status(400)
            .with_body(r#"{"error":"access_denied"}"#)
            .create_async()
            .await;

        let cfg = device_cfg(&server.url());
        let noop = |_: &DeviceCodeResponse| {};
        let err = device_login(&cfg, &noop).await.unwrap_err();
        assert!(
            matches!(err, ActualError::ApiError(ref m) if m.contains("denied")),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_device_login_server_expired_token() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/api/oauth/device_authorization")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(DEVICE_AUTH_OK) // expires_in:300, so the server code drives it
            .create_async()
            .await;
        server
            .mock("POST", "/api/oauth/token")
            .with_status(400)
            .with_body(r#"{"error":"expired_token"}"#)
            .create_async()
            .await;

        let cfg = device_cfg(&server.url());
        let noop = |_: &DeviceCodeResponse| {};
        let err = device_login(&cfg, &noop).await.unwrap_err();
        assert!(
            matches!(err, ActualError::ApiError(ref m) if m.contains("expired")),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_device_login_rejects_non_https_base() {
        let cfg = device_cfg("http://example.com");
        let noop = |_: &DeviceCodeResponse| {};
        let err = device_login(&cfg, &noop).await.unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(ref m) if m.contains("HTTPS")),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_device_login_device_authorization_failure_propagates() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/api/oauth/device_authorization")
            .with_status(400)
            .with_body(r#"{"error":"invalid_scope"}"#)
            .create_async()
            .await;
        let cfg = device_cfg(&server.url());
        let noop = |_: &DeviceCodeResponse| {};
        let err = device_login(&cfg, &noop).await.unwrap_err();
        assert!(
            matches!(err, ActualError::ApiError(ref m) if m.contains("invalid_scope")),
            "got: {err:?}"
        );
    }
}
