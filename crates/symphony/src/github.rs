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
    #[serde(default)]
    pub node_id: Option<String>,
    #[serde(default)]
    pub merged: Option<bool>,
}

/// A single PR review returned by GitHub API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrReview {
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

    /// Get the node_id of a PR (needed for GraphQL mutations).
    /// Uses `GET /repos/{owner}/{repo}/pulls/{number}` and extracts `node_id`.
    pub async fn get_pr_node_id(&self, pr_number: u64) -> Result<String> {
        let details = self.get_pr_details(pr_number).await?;
        details
            .node_id
            .ok_or_else(|| SymphonyError::GitHubApiRequest {
                reason: format!("PR {pr_number} missing node_id"),
            })
    }

    /// Fetch reviews for a PR.
    /// Uses `GET /repos/{owner}/{repo}/pulls/{number}/reviews`.
    pub async fn get_pr_reviews(&self, pr_number: u64) -> Result<Vec<PrReview>> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}/reviews",
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

    /// Check if a PR has been approved (at least one APPROVED, no CHANGES_REQUESTED).
    pub async fn is_pr_approved(&self, pr_number: u64) -> Result<bool> {
        let reviews = self.get_pr_reviews(pr_number).await?;
        let has_approved = reviews.iter().any(|r| r.state == "APPROVED");
        let has_changes_requested = reviews.iter().any(|r| r.state == "CHANGES_REQUESTED");
        Ok(has_approved && !has_changes_requested)
    }

    /// Enable auto-merge on a PR using GitHub's GraphQL API.
    /// This is idempotent — calling it on an already-queued PR is safe.
    pub async fn add_to_merge_queue(&self, pr_number: u64) -> Result<()> {
        let node_id = self.get_pr_node_id(pr_number).await?;

        let graphql_url = format!("{}/graphql", self.api_base.trim_end_matches('/'));

        let query = format!(
            r#"mutation {{ enablePullRequestAutoMerge(input: {{ pullRequestId: "{node_id}" }}) {{ pullRequest {{ autoMergeRequest {{ enabledAt }} }} }} }}"#
        );

        let body = serde_json::json!({ "query": query });

        let response = self
            .http
            .post(&graphql_url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "symphony")
            .json(&body)
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

        let json: serde_json::Value =
            response
                .json()
                .await
                .map_err(|e| SymphonyError::GitHubApiRequest {
                    reason: e.to_string(),
                })?;

        // Check for GraphQL errors
        if let Some(errors) = json.get("errors") {
            if let Some(arr) = errors.as_array() {
                if !arr.is_empty() {
                    let msgs: Vec<String> = arr
                        .iter()
                        .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                        .map(|s| s.to_string())
                        .collect();
                    return Err(SymphonyError::GitHubApiRequest {
                        reason: format!("GraphQL errors: {}", msgs.join("; ")),
                    });
                }
            }
        }

        debug!(pr_number = pr_number, "enabled auto-merge on PR");
        Ok(())
    }

    /// Check if a PR has been merged.
    /// Uses `GET /repos/{owner}/{repo}/pulls/{number}`.
    pub async fn is_pr_merged(&self, pr_number: u64) -> Result<bool> {
        let details = self.get_pr_details(pr_number).await?;
        Ok(details.state == "closed" && details.merged == Some(true))
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
            auto_merge: false,
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
            node_id: Some("PR_abc123".to_string()),
            merged: Some(false),
        };
        let json = serde_json::to_string(&pr).unwrap();
        assert!(json.contains("\"number\":42"));
        assert!(json.contains("\"mergeable\":true"));
        assert!(json.contains("\"state\":\"open\""));
        assert!(json.contains("\"node_id\":\"PR_abc123\""));
        assert!(json.contains("\"merged\":false"));
    }

    #[test]
    fn test_pr_info_deserializes_with_missing_optional_fields() {
        let json = r#"{"number": 1, "state": "open"}"#;
        let pr: PrInfo = serde_json::from_str(json).unwrap();
        assert_eq!(pr.number, 1);
        assert_eq!(pr.state, "open");
        assert!(pr.mergeable.is_none());
        assert!(pr.mergeable_state.is_none());
        assert!(pr.node_id.is_none());
        assert!(pr.merged.is_none());
    }

    #[test]
    fn test_pr_info_deserializes_with_all_fields() {
        let json = r#"{"number": 1, "state": "closed", "node_id": "PR_xyz", "merged": true, "mergeable": null, "mergeable_state": null}"#;
        let pr: PrInfo = serde_json::from_str(json).unwrap();
        assert_eq!(pr.number, 1);
        assert_eq!(pr.state, "closed");
        assert_eq!(pr.node_id, Some("PR_xyz".to_string()));
        assert_eq!(pr.merged, Some(true));
    }

    #[test]
    fn test_pr_info_debug_impl() {
        let pr = PrInfo {
            number: 42,
            mergeable: Some(false),
            mergeable_state: Some("dirty".to_string()),
            state: "open".to_string(),
            node_id: None,
            merged: None,
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
            node_id: Some("PR_node".to_string()),
            merged: Some(false),
        };
        let cloned = pr.clone();
        assert_eq!(cloned.number, 42);
        assert_eq!(cloned.mergeable, Some(true));
        assert_eq!(cloned.node_id, Some("PR_node".to_string()));
        assert_eq!(cloned.merged, Some(false));
    }

    // ── PrReview serialization ────────────────────────────────────

    #[test]
    fn test_pr_review_serializes() {
        let review = PrReview {
            state: "APPROVED".to_string(),
        };
        let json = serde_json::to_string(&review).unwrap();
        assert!(json.contains("\"state\":\"APPROVED\""));
    }

    #[test]
    fn test_pr_review_deserializes() {
        let json = r#"{"state": "CHANGES_REQUESTED"}"#;
        let review: PrReview = serde_json::from_str(json).unwrap();
        assert_eq!(review.state, "CHANGES_REQUESTED");
    }

    #[test]
    fn test_pr_review_debug_impl() {
        let review = PrReview {
            state: "APPROVED".to_string(),
        };
        let debug = format!("{:?}", review);
        assert!(debug.contains("PrReview"));
        assert!(debug.contains("APPROVED"));
    }

    #[test]
    fn test_pr_review_clone() {
        let review = PrReview {
            state: "APPROVED".to_string(),
        };
        let cloned = review.clone();
        assert_eq!(cloned.state, "APPROVED");
    }

    // ── get_pr_node_id ────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_pr_node_id_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "node_id": "PR_kwDOTest",
                    "merged": false
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let node_id = client.get_pr_node_id(42).await.unwrap();
        assert_eq!(node_id, "PR_kwDOTest");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_pr_node_id_missing() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "open"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.get_pr_node_id(42).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SymphonyError::GitHubApiRequest { .. }
        ));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_pr_node_id_api_error() {
        let config = test_github_config("http://127.0.0.1:1");
        let client = GitHubClient::new(&config, new_http());
        let result = client.get_pr_node_id(42).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SymphonyError::GitHubApiRequest { .. }
        ));
    }

    // ── get_pr_reviews ────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_pr_reviews_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42/reviews")
            .match_header("Authorization", "Bearer test-token-123")
            .match_header("Accept", "application/vnd.github+json")
            .match_header("User-Agent", "symphony")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![
                    serde_json::json!({"state": "APPROVED"}),
                    serde_json::json!({"state": "COMMENTED"}),
                ])
                .unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let reviews = client.get_pr_reviews(42).await.unwrap();
        assert_eq!(reviews.len(), 2);
        assert_eq!(reviews[0].state, "APPROVED");
        assert_eq!(reviews[1].state, "COMMENTED");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_pr_reviews_empty() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42/reviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let reviews = client.get_pr_reviews(42).await.unwrap();
        assert!(reviews.is_empty());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_pr_reviews_api_error() {
        let config = test_github_config("http://127.0.0.1:1");
        let client = GitHubClient::new(&config, new_http());
        let result = client.get_pr_reviews(42).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SymphonyError::GitHubApiRequest { .. }
        ));
    }

    #[tokio::test]
    async fn test_get_pr_reviews_non_200_status() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42/reviews")
            .with_status(404)
            .with_body("not found")
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.get_pr_reviews(42).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            SymphonyError::GitHubApiStatus { status: 404, .. }
        ));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_pr_reviews_invalid_json() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42/reviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("not json")
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.get_pr_reviews(42).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SymphonyError::GitHubApiRequest { .. }
        ));
        mock.assert_async().await;
    }

    // ── is_pr_approved ────────────────────────────────────────────

    #[tokio::test]
    async fn test_is_pr_approved_yes() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42/reviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({"state": "APPROVED"})]).unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        assert!(client.is_pr_approved(42).await.unwrap());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_is_pr_approved_no_reviews() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42/reviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        assert!(!client.is_pr_approved(42).await.unwrap());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_is_pr_approved_changes_requested() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42/reviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![
                    serde_json::json!({"state": "APPROVED"}),
                    serde_json::json!({"state": "CHANGES_REQUESTED"}),
                ])
                .unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        assert!(!client.is_pr_approved(42).await.unwrap());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_is_pr_approved_only_commented() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42/reviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({"state": "COMMENTED"})]).unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        assert!(!client.is_pr_approved(42).await.unwrap());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_is_pr_approved_api_error() {
        let config = test_github_config("http://127.0.0.1:1");
        let client = GitHubClient::new(&config, new_http());
        let result = client.is_pr_approved(42).await;
        assert!(result.is_err());
    }

    // ── add_to_merge_queue ────────────────────────────────────────

    #[tokio::test]
    async fn test_add_to_merge_queue_success() {
        let mut server = mockito::Server::new_async().await;

        // First: get PR details to get node_id
        let details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "node_id": "PR_kwDOTest123"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        // Second: GraphQL mutation
        let graphql_mock = server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "enablePullRequestAutoMerge": {
                            "pullRequest": {
                                "autoMergeRequest": {
                                    "enabledAt": "2025-01-01T00:00:00Z"
                                }
                            }
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        client.add_to_merge_queue(42).await.unwrap();
        details_mock.assert_async().await;
        graphql_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_add_to_merge_queue_graphql_error() {
        let mut server = mockito::Server::new_async().await;

        let details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "node_id": "PR_kwDOTest123"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        let graphql_mock = server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "errors": [
                        {"message": "Pull request is not in a mergeable state"}
                    ]
                })
                .to_string(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.add_to_merge_queue(42).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("GraphQL errors"));
        details_mock.assert_async().await;
        graphql_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_add_to_merge_queue_missing_node_id() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "open"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.add_to_merge_queue(42).await;
        assert!(result.is_err());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_add_to_merge_queue_http_error() {
        let mut server = mockito::Server::new_async().await;

        let details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "node_id": "PR_kwDOTest123"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        let graphql_mock = server
            .mock("POST", "/graphql")
            .with_status(500)
            .with_body("internal server error")
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.add_to_merge_queue(42).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            SymphonyError::GitHubApiStatus { status: 500, .. }
        ));
        details_mock.assert_async().await;
        graphql_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_add_to_merge_queue_invalid_json_response() {
        let mut server = mockito::Server::new_async().await;

        let details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "node_id": "PR_kwDOTest123"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        let graphql_mock = server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("not json")
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.add_to_merge_queue(42).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SymphonyError::GitHubApiRequest { .. }
        ));
        details_mock.assert_async().await;
        graphql_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_add_to_merge_queue_api_error() {
        let config = test_github_config("http://127.0.0.1:1");
        let client = GitHubClient::new(&config, new_http());
        let result = client.add_to_merge_queue(42).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_add_to_merge_queue_empty_graphql_errors_array() {
        let mut server = mockito::Server::new_async().await;

        let details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "node_id": "PR_kwDOTest123"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        let graphql_mock = server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "enablePullRequestAutoMerge": {
                            "pullRequest": {
                                "autoMergeRequest": {
                                    "enabledAt": "2025-01-01T00:00:00Z"
                                }
                            }
                        }
                    },
                    "errors": []
                })
                .to_string(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        // Empty errors array should not be treated as an error
        client.add_to_merge_queue(42).await.unwrap();
        details_mock.assert_async().await;
        graphql_mock.assert_async().await;
    }

    // ── is_pr_merged ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_is_pr_merged_true() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "closed",
                    "merged": true
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        assert!(client.is_pr_merged(42).await.unwrap());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_is_pr_merged_closed_not_merged() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "closed",
                    "merged": false
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        assert!(!client.is_pr_merged(42).await.unwrap());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_is_pr_merged_open() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "merged": false
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        assert!(!client.is_pr_merged(42).await.unwrap());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_is_pr_merged_no_merged_field() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "closed"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        // merged defaults to None, which is not Some(true), so not merged
        assert!(!client.is_pr_merged(42).await.unwrap());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_is_pr_merged_api_error() {
        let config = test_github_config("http://127.0.0.1:1");
        let client = GitHubClient::new(&config, new_http());
        let result = client.is_pr_merged(42).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SymphonyError::GitHubApiRequest { .. }
        ));
    }

    // ── add_to_merge_queue: send() error on graphql POST ─────────────

    #[tokio::test]
    async fn test_add_to_merge_queue_graphql_send_error() {
        // Spin up a raw TCP server that serves a valid GET response (for
        // get_pr_node_id) then immediately resets the next connection
        // (the POST /graphql), triggering a .send() error.
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");

        let server_handle = tokio::spawn(async move {
            // Connection 1: GET /repos/.../pulls/42 → return valid JSON with node_id
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;
            let body = r#"{"number":42,"state":"open","node_id":"PR_kwDOTest123"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.ok();

            // Connection 2: POST /graphql → accept then immediately reset
            if let Ok((socket, _)) = listener.accept().await {
                drop(socket); // RST the connection
            }
        });

        let config = test_github_config(&base_url);
        let client = GitHubClient::new(&config, new_http());
        let result = client.add_to_merge_queue(42).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SymphonyError::GitHubApiRequest { .. }
        ));

        server_handle.abort();
    }

    // ── add_to_merge_queue: non-array graphql errors ─────────────────

    #[tokio::test]
    async fn test_add_to_merge_queue_non_array_graphql_errors() {
        let mut server = mockito::Server::new_async().await;

        // GET /repos/.../pulls/42 → valid node_id
        let details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "node_id": "PR_kwDOTest123"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        // POST /graphql → errors field is a string, not an array
        let graphql_mock = server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "enablePullRequestAutoMerge": {
                            "pullRequest": {
                                "autoMergeRequest": {
                                    "enabledAt": "2025-01-01T00:00:00Z"
                                }
                            }
                        }
                    },
                    "errors": "some non-array error"
                })
                .to_string(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        // Should succeed — non-array errors field is ignored (fallthrough)
        client.add_to_merge_queue(42).await.unwrap();
        details_mock.assert_async().await;
        graphql_mock.assert_async().await;
    }
}
