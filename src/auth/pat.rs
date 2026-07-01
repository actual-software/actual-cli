//! Scoped personal access token (PAT) issuance against the Actual AI platform.
//!
//! `actual auth create-token` mints a PAT by calling the authenticated
//! issuance endpoint with the user's existing `actual login` session token as
//! the bearer — no second browser dance, the established session *is* the
//! authentication for issuance. The endpoint returns the raw `actl_pat_…`
//! exactly once; the CLI prints it once and otherwise keeps it only behind the
//! keychain / encrypted-file store (see [`crate::auth::token_store`]).
//!
//! ## Endpoint contract (developed against; confirm against the backend)
//!
//! The backend issuance endpoint is still landing, so this module is written
//! against the documented contract and is trivially repointed once the server
//! ships:
//!
//! - `POST <base>/api/oauth/tokens`
//! - `Authorization: Bearer <session access token>`
//! - request body: `{ "name": "<label>", "scopes": ["adr:query", …] }`
//! - response body: `{ "token": "actl_pat_…", "id": …, "name": …, "scopes":
//!   […], "created_at": …, "expires_at": … }` — the raw `token` is returned
//!   exactly once.
//!
//! `<base>` is resolved by the command layer (the `--api-url` flag, the
//! `ACTUAL_API_URL` env var, or the api-service default), so steering the call
//! at a local mock or a future production path needs no code change here.

use serde::{Deserialize, Serialize};

use crate::auth::oauth::build_http_client;
use crate::error::ActualError;

/// Opaque-token prefix. Chosen so secret-scanning can spot a leaked PAT, and
/// asserted on the issuance response so a malformed/foreign token is rejected.
pub const PAT_PREFIX: &str = "actl_pat_";

/// Path of the authenticated issuance endpoint, appended to the resolved base.
pub const ISSUANCE_PATH: &str = "/api/oauth/tokens";

/// Request body for token issuance.
#[derive(Debug, Serialize)]
struct IssueTokenRequest {
    name: String,
    scopes: Vec<String>,
}

/// Issuance response. Only `token` is required; the metadata fields are
/// surfaced when the server provides them.
#[derive(Deserialize)]
struct IssueTokenResponse {
    token: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
}

/// A freshly minted PAT plus its non-secret metadata.
///
/// The [`std::fmt::Debug`] impl redacts the raw token so it never reaches a log
/// line, a panic message, or `{:?}` formatting.
#[derive(Clone)]
pub struct IssuedToken {
    /// The raw `actl_pat_…` secret. **Sensitive — shown once, never logged.**
    pub token: String,
    /// Server-assigned token id, when provided.
    pub id: Option<String>,
    /// The credential label echoed back by the server, when provided.
    pub name: Option<String>,
    /// Granted scopes echoed back by the server.
    pub scopes: Vec<String>,
    /// Creation timestamp, when provided.
    pub created_at: Option<String>,
    /// Expiry timestamp, when provided.
    pub expires_at: Option<String>,
}

impl std::fmt::Debug for IssuedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedToken")
            .field("token", &"<redacted>")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("scopes", &self.scopes)
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Map an unsuccessful issuance response to an [`ActualError`]. The body is an
/// error payload (not a secret); it is truncated and surfaced for diagnosis.
async fn issuance_error(response: reqwest::Response) -> ActualError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let truncated: String = body.chars().take(2048).collect();
    ActualError::ApiError(format!(
        "Token issuance failed (HTTP {status}): {truncated}"
    ))
}

/// Mint a PAT named `name` with `scopes`, authenticating with `access_token`
/// (the user's `actual login` session bearer) against `base_url`.
///
/// On success the raw token is validated to carry the [`PAT_PREFIX`] and
/// returned in [`IssuedToken`]. The access token and the minted token are never
/// logged.
pub async fn issue(
    base_url: &str,
    access_token: &str,
    name: &str,
    scopes: &[String],
) -> Result<IssuedToken, ActualError> {
    let base = base_url.trim_end_matches('/');
    let http = build_http_client(base)?;
    let url = format!("{base}{ISSUANCE_PATH}");

    let request = IssueTokenRequest {
        name: name.to_string(),
        scopes: scopes.to_vec(),
    };

    let response = http
        .post(&url)
        .bearer_auth(access_token)
        .json(&request)
        .send()
        .await
        .map_err(|e| ActualError::ApiError(format!("Token issuance request failed: {e}")))?;

    if !response.status().is_success() {
        return Err(issuance_error(response).await);
    }

    let body: IssueTokenResponse = response
        .json()
        .await
        .map_err(|e| ActualError::ApiError(format!("Failed to parse issuance response: {e}")))?;

    if !body.token.starts_with(PAT_PREFIX) {
        return Err(ActualError::ApiError(format!(
            "Issuance endpoint returned a token without the expected `{PAT_PREFIX}` prefix"
        )));
    }

    Ok(IssuedToken {
        token: body.token,
        id: body.id,
        name: body.name,
        scopes: body.scopes.unwrap_or_default(),
        created_at: body.created_at,
        expires_at: body.expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scopes() -> Vec<String> {
        vec!["adr:query".to_string(), "adr:review".to_string()]
    }

    #[tokio::test]
    async fn test_issue_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", ISSUANCE_PATH)
            .match_header("authorization", "Bearer session-token-abc")
            .match_body(mockito::Matcher::PartialJsonString(
                r#"{"name":"ci-agent","scopes":["adr:query","adr:review"]}"#.to_string(),
            ))
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"token":"actl_pat_abc123","id":"tok_1","name":"ci-agent","scopes":["adr:query","adr:review"],"created_at":"2026-06-30T00:00:00Z"}"#,
            )
            .create_async()
            .await;

        let issued = issue(&server.url(), "session-token-abc", "ci-agent", &scopes())
            .await
            .unwrap();
        assert_eq!(issued.token, "actl_pat_abc123");
        assert_eq!(issued.id.as_deref(), Some("tok_1"));
        assert_eq!(issued.name.as_deref(), Some("ci-agent"));
        assert_eq!(issued.scopes, vec!["adr:query", "adr:review"]);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_issue_minimal_response() {
        // Only `token` returned; metadata absent.
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", ISSUANCE_PATH)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"token":"actl_pat_minimal"}"#)
            .create_async()
            .await;

        let issued = issue(&server.url(), "tok", "name", &scopes())
            .await
            .unwrap();
        assert_eq!(issued.token, "actl_pat_minimal");
        assert!(issued.id.is_none());
        assert!(issued.scopes.is_empty());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_issue_rejects_token_without_prefix() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", ISSUANCE_PATH)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"token":"oops_wrong_prefix"}"#)
            .create_async()
            .await;

        let err = issue(&server.url(), "tok", "name", &scopes())
            .await
            .unwrap_err();
        assert!(
            matches!(err, ActualError::ApiError(ref m) if m.contains(PAT_PREFIX)),
            "got: {err:?}"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_issue_error_status_surfaces_body() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", ISSUANCE_PATH)
            .with_status(403)
            .with_body(r#"{"error":"insufficient_scope"}"#)
            .create_async()
            .await;

        let err = issue(&server.url(), "tok", "name", &scopes())
            .await
            .unwrap_err();
        assert!(
            matches!(err, ActualError::ApiError(ref m) if m.contains("insufficient_scope") && m.contains("403")),
            "got: {err:?}"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_issue_rejects_non_https_non_loopback() {
        // The shared client builder refuses to send a bearer over clear-text
        // http to a non-loopback host.
        let err = issue("http://example.com", "tok", "name", &scopes())
            .await
            .unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(ref m) if m.contains("HTTPS")),
            "got: {err:?}"
        );
    }

    #[test]
    fn test_issued_token_debug_redacts_secret() {
        let issued = IssuedToken {
            token: "actl_pat_supersecret".to_string(),
            id: Some("tok_1".to_string()),
            name: Some("agent".to_string()),
            scopes: scopes(),
            created_at: None,
            expires_at: None,
        };
        let debug = format!("{issued:?}");
        assert!(
            !debug.contains("actl_pat_supersecret"),
            "Debug must not leak the token: {debug}"
        );
        assert!(debug.contains("<redacted>"));
        assert!(debug.contains("tok_1"), "non-secret id should show");
    }
}
