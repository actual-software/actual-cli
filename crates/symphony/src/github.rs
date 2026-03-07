use crate::config::GitHubConfig;
use crate::error::{Result, SymphonyError};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Minimal PR info returned by GitHub API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrInfo {
    pub number: u64,
    pub mergeable: Option<bool>,
    #[serde(default)]
    pub mergeable_state: Option<String>,
    pub state: String,
}

/// GitHub API client for checking PR status.
pub struct GitHubClient {
    http: Client,
    token: String,
    api_base: String,
    repo_owner: String,
    repo_name: String,
}

impl GitHubClient {
    pub fn new(config: &GitHubConfig, http: Client) -> Self {
        Self {
            http,
            token: config.token.clone(),
            api_base: config.api_base.clone(),
            repo_owner: config.repo_owner.clone(),
            repo_name: config.repo_name.clone(),
        }
    }

    /// Find an open PR for the given branch name (e.g., "symphony/actcli-42").
    /// Uses `GET /repos/{owner}/{repo}/pulls?head={owner}:{branch}&state=open`
    pub async fn find_open_pr(&self, branch: &str) -> Result<Option<PrInfo>> {
        let url = format!(
            "{}/repos/{}/{}/pulls",
            self.api_base, self.repo_owner, self.repo_name
        );

        let head = format!("{}:{}", self.repo_owner, branch);

        let response = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "symphony")
            .query(&[("head", head.as_str()), ("state", "open")])
            .send()
            .await
            .map_err(|e| SymphonyError::GitHubApiRequest {
                reason: e.to_string(),
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(SymphonyError::GitHubApiStatus {
                status: status.as_u16(),
                body,
            });
        }

        let prs: Vec<PrInfo> =
            response
                .json()
                .await
                .map_err(|e| SymphonyError::GitHubApiRequest {
                    reason: e.to_string(),
                })?;

        Ok(prs.into_iter().next())
    }

    /// Get detailed PR info including mergeable status.
    /// Uses `GET /repos/{owner}/{repo}/pulls/{number}`
    pub async fn get_pr_details(&self, pr_number: u64) -> Result<PrInfo> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}",
            self.api_base, self.repo_owner, self.repo_name, pr_number
        );

        let response = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "symphony")
            .send()
            .await
            .map_err(|e| SymphonyError::GitHubApiRequest {
                reason: e.to_string(),
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(SymphonyError::GitHubApiStatus {
                status: status.as_u16(),
                body,
            });
        }

        response
            .json()
            .await
            .map_err(|e| SymphonyError::GitHubApiRequest {
                reason: e.to_string(),
            })
    }

    /// Check if a PR has merge conflicts. Returns:
    /// - `Some(true)` if mergeable
    /// - `Some(false)` if conflicting
    /// - `None` if GitHub hasn't computed it yet or no PR exists
    pub async fn is_pr_mergeable(&self, branch: &str) -> Result<Option<bool>> {
        let pr = match self.find_open_pr(branch).await? {
            Some(pr) => pr,
            None => {
                debug!(branch = %branch, "no open PR found for branch");
                return Ok(None);
            }
        };

        let details = self.get_pr_details(pr.number).await?;
        Ok(details.mergeable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_github_config(api_base: &str) -> GitHubConfig {
        GitHubConfig {
            token: "test-token-123".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: api_base.to_string(),
            branch_prefix: "symphony/".to_string(),
        }
    }

    fn new_http() -> Client {
        Client::new()
    }

    // ── GitHubClient::new ──────────────────────────────────────────

    #[test]
    fn test_github_client_new_sets_fields() {
        let config = test_github_config("https://api.github.com");
        let client = GitHubClient::new(&config, new_http());
        assert_eq!(client.token, "test-token-123");
        assert_eq!(client.api_base, "https://api.github.com");
        assert_eq!(client.repo_owner, "test-org");
        assert_eq!(client.repo_name, "test-repo");
    }

    // ── find_open_pr ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_find_open_pr_found() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:feature/test".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "open".to_string()),
            ]))
            .match_header("Authorization", "Bearer test-token-123")
            .match_header("Accept", "application/vnd.github+json")
            .match_header("User-Agent", "symphony")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "mergeable": true,
                    "mergeable_state": "clean"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.find_open_pr("feature/test").await.unwrap();
        assert!(result.is_some());
        let pr = result.unwrap();
        assert_eq!(pr.number, 42);
        assert_eq!(pr.state, "open");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_find_open_pr_not_found() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:feature/none".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "open".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.find_open_pr("feature/none").await.unwrap();
        assert!(result.is_none());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_find_open_pr_api_error() {
        let config = test_github_config("http://127.0.0.1:1");
        let client = GitHubClient::new(&config, new_http());
        let result = client.find_open_pr("feature/test").await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SymphonyError::GitHubApiRequest { .. }
        ));
    }

    #[tokio::test]
    async fn test_find_open_pr_non_200_status() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(403)
            .with_body("forbidden")
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.find_open_pr("feature/test").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            SymphonyError::GitHubApiStatus { status: 403, .. }
        ));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_find_open_pr_invalid_json() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("not valid json")
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.find_open_pr("feature/test").await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SymphonyError::GitHubApiRequest { .. }
        ));
        mock.assert_async().await;
    }

    // ── get_pr_details ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_pr_details_mergeable_true() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .match_header("Authorization", "Bearer test-token-123")
            .match_header("Accept", "application/vnd.github+json")
            .match_header("User-Agent", "symphony")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "mergeable": true,
                    "mergeable_state": "clean"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let pr = client.get_pr_details(42).await.unwrap();
        assert_eq!(pr.number, 42);
        assert_eq!(pr.mergeable, Some(true));
        assert_eq!(pr.mergeable_state, Some("clean".to_string()));
        assert_eq!(pr.state, "open");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_pr_details_mergeable_false() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/99")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 99,
                    "state": "open",
                    "mergeable": false,
                    "mergeable_state": "dirty"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let pr = client.get_pr_details(99).await.unwrap();
        assert_eq!(pr.mergeable, Some(false));
        assert_eq!(pr.mergeable_state, Some("dirty".to_string()));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_pr_details_mergeable_null() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/50")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 50,
                    "state": "open",
                    "mergeable": null,
                    "mergeable_state": "unknown"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let pr = client.get_pr_details(50).await.unwrap();
        assert_eq!(pr.mergeable, None);
        assert_eq!(pr.mergeable_state, Some("unknown".to_string()));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_pr_details_api_error() {
        let config = test_github_config("http://127.0.0.1:1");
        let client = GitHubClient::new(&config, new_http());
        let result = client.get_pr_details(42).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SymphonyError::GitHubApiRequest { .. }
        ));
    }

    #[tokio::test]
    async fn test_get_pr_details_non_200_status() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(404)
            .with_body("not found")
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.get_pr_details(42).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            SymphonyError::GitHubApiStatus { status: 404, .. }
        ));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_pr_details_invalid_json() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("not json")
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.get_pr_details(42).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SymphonyError::GitHubApiRequest { .. }
        ));
        mock.assert_async().await;
    }

    // ── is_pr_mergeable ────────────────────────────────────────────

    #[tokio::test]
    async fn test_is_pr_mergeable_clean() {
        let mut server = mockito::Server::new_async().await;

        // First call: find open PR
        let find_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/actcli-42".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "open".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 10,
                    "state": "open",
                    "mergeable": null
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // Second call: get PR details
        let details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/10")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 10,
                    "state": "open",
                    "mergeable": true,
                    "mergeable_state": "clean"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.is_pr_mergeable("symphony/actcli-42").await.unwrap();
        assert_eq!(result, Some(true));
        find_mock.assert_async().await;
        details_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_is_pr_mergeable_conflicting() {
        let mut server = mockito::Server::new_async().await;

        let find_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 20,
                    "state": "open"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/20")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 20,
                    "state": "open",
                    "mergeable": false,
                    "mergeable_state": "dirty"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.is_pr_mergeable("symphony/actcli-42").await.unwrap();
        assert_eq!(result, Some(false));
        find_mock.assert_async().await;
        details_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_is_pr_mergeable_unknown() {
        let mut server = mockito::Server::new_async().await;

        let find_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 30,
                    "state": "open"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/30")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 30,
                    "state": "open",
                    "mergeable": null,
                    "mergeable_state": "unknown"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.is_pr_mergeable("symphony/actcli-42").await.unwrap();
        assert_eq!(result, None);
        find_mock.assert_async().await;
        details_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_is_pr_mergeable_no_pr_exists() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.is_pr_mergeable("symphony/actcli-99").await.unwrap();
        assert_eq!(result, None);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_is_pr_mergeable_api_error() {
        let config = test_github_config("http://127.0.0.1:1");
        let client = GitHubClient::new(&config, new_http());
        let result = client.is_pr_mergeable("symphony/actcli-42").await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SymphonyError::GitHubApiRequest { .. }
        ));
    }

    // ── PrInfo serialization ───────────────────────────────────────

    #[test]
    fn test_pr_info_serializes() {
        let pr = PrInfo {
            number: 42,
            mergeable: Some(true),
            mergeable_state: Some("clean".to_string()),
            state: "open".to_string(),
        };
        let json = serde_json::to_string(&pr).unwrap();
        assert!(json.contains("\"number\":42"));
        assert!(json.contains("\"mergeable\":true"));
        assert!(json.contains("\"state\":\"open\""));
    }

    #[test]
    fn test_pr_info_deserializes_with_missing_optional_fields() {
        let json = r#"{"number": 1, "state": "open"}"#;
        let pr: PrInfo = serde_json::from_str(json).unwrap();
        assert_eq!(pr.number, 1);
        assert_eq!(pr.state, "open");
        assert!(pr.mergeable.is_none());
        assert!(pr.mergeable_state.is_none());
    }

    #[test]
    fn test_pr_info_debug_impl() {
        let pr = PrInfo {
            number: 42,
            mergeable: Some(false),
            mergeable_state: Some("dirty".to_string()),
            state: "open".to_string(),
        };
        let debug = format!("{:?}", pr);
        assert!(debug.contains("PrInfo"));
        assert!(debug.contains("42"));
    }

    #[test]
    fn test_pr_info_clone() {
        let pr = PrInfo {
            number: 42,
            mergeable: Some(true),
            mergeable_state: Some("clean".to_string()),
            state: "open".to_string(),
        };
        let cloned = pr.clone();
        assert_eq!(cloned.number, 42);
        assert_eq!(cloned.mergeable, Some(true));
    }
}
