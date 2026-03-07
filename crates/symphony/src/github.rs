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
    pub merged: Option<bool>,
}

/// A single PR review from the GitHub API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrReview {
    pub id: u64,
    pub state: String,
    pub user: Option<ReviewUser>,
}

/// User who submitted a review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewUser {
    pub login: String,
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

    /// Get reviews for a PR.
    /// Uses `GET /repos/{owner}/{repo}/pulls/{number}/reviews`
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

    /// Check if a PR has been approved (at least one APPROVED review,
    /// no CHANGES_REQUESTED that is more recent).
    pub async fn is_pr_approved(&self, pr_number: u64) -> Result<bool> {
        let reviews = self.get_pr_reviews(pr_number).await?;
        Ok(has_approval(&reviews))
    }

    /// Enable auto-merge on a PR via the GitHub API.
    /// Uses `PUT /repos/{owner}/{repo}/pulls/{number}/merge` with merge_method.
    /// Returns Ok(true) if merge succeeded, Ok(false) if not mergeable yet.
    pub async fn enable_auto_merge(&self, pr_number: u64) -> Result<bool> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}/merge",
            self.api_base, self.repo_owner, self.repo_name, pr_number
        );

        let response = self
            .http
            .put(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "symphony")
            .json(&serde_json::json!({
                "merge_method": "squash"
            }))
            .send()
            .await
            .map_err(|e| SymphonyError::GitHubApiRequest {
                reason: e.to_string(),
            })?;

        let status = response.status();
        if status.as_u16() == 200 {
            return Ok(true);
        }
        if status.as_u16() == 405 {
            // 405 = not mergeable (e.g., checks pending, review required)
            debug!(pr_number, "PR not mergeable yet (405)");
            return Ok(false);
        }
        if status.as_u16() == 409 {
            // 409 = conflict (e.g., head branch was modified)
            debug!(pr_number, "PR merge conflict (409)");
            return Ok(false);
        }

        let body = response.text().await.unwrap_or_default();
        Err(SymphonyError::GitHubApiStatus {
            status: status.as_u16(),
            body,
        })
    }

    /// Check if a PR for a given branch has been merged.
    /// Returns `Some(true)` if merged, `Some(false)` if still open, `None` if no PR.
    pub async fn is_pr_merged(&self, branch: &str) -> Result<Option<bool>> {
        // Search for any PR (open or closed) on this branch
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
            .query(&[("head", head.as_str()), ("state", "all")])
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

        match prs.first() {
            None => Ok(None),
            Some(pr) => Ok(Some(pr_is_merged(pr))),
        }
    }
}

/// Check if reviews contain an approval with no outstanding changes requested.
fn has_approval(reviews: &[PrReview]) -> bool {
    // Walk reviews in order. The last review state per user wins.
    let mut user_states: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for review in reviews {
        if let Some(ref user) = review.user {
            let state = review.state.as_str();
            // Only track APPROVED and CHANGES_REQUESTED
            if state == "APPROVED" || state == "CHANGES_REQUESTED" {
                user_states.insert(user.login.as_str(), state);
            }
        }
    }

    if user_states.is_empty() {
        return false;
    }

    // Must have at least one approval and no outstanding changes requested
    let has_approved = user_states.values().any(|s| *s == "APPROVED");
    let has_changes_requested = user_states.values().any(|s| *s == "CHANGES_REQUESTED");

    has_approved && !has_changes_requested
}

/// Check if a PrInfo represents a merged PR.
fn pr_is_merged(pr: &PrInfo) -> bool {
    // GitHub sets merged=true when a PR is merged. The state becomes "closed".
    pr.merged == Some(true)
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
            merged: Some(false),
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
        assert!(pr.merged.is_none());
    }

    #[test]
    fn test_pr_info_debug_impl() {
        let pr = PrInfo {
            number: 42,
            mergeable: Some(false),
            mergeable_state: Some("dirty".to_string()),
            state: "open".to_string(),
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
            merged: Some(false),
        };
        let cloned = pr.clone();
        assert_eq!(cloned.number, 42);
        assert_eq!(cloned.mergeable, Some(true));
        assert_eq!(cloned.merged, Some(false));
    }

    // ── get_pr_reviews ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_pr_reviews_returns_reviews() {
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
                    serde_json::json!({
                        "id": 1,
                        "state": "APPROVED",
                        "user": { "login": "reviewer1" }
                    }),
                    serde_json::json!({
                        "id": 2,
                        "state": "COMMENTED",
                        "user": { "login": "reviewer2" }
                    }),
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
        assert_eq!(reviews[0].user.as_ref().unwrap().login, "reviewer1");
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
        assert!(matches!(
            result.unwrap_err(),
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

    // ── is_pr_approved ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_is_pr_approved_with_approval() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42/reviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "id": 1,
                    "state": "APPROVED",
                    "user": { "login": "reviewer1" }
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        assert!(client.is_pr_approved(42).await.unwrap());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_is_pr_approved_with_changes_requested() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42/reviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![
                    serde_json::json!({
                        "id": 1,
                        "state": "APPROVED",
                        "user": { "login": "reviewer1" }
                    }),
                    serde_json::json!({
                        "id": 2,
                        "state": "CHANGES_REQUESTED",
                        "user": { "login": "reviewer2" }
                    }),
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

    // ── has_approval (unit tests) ──────────────────────────────────

    #[test]
    fn test_has_approval_empty() {
        assert!(!has_approval(&[]));
    }

    #[test]
    fn test_has_approval_only_comments() {
        let reviews = vec![PrReview {
            id: 1,
            state: "COMMENTED".to_string(),
            user: Some(ReviewUser {
                login: "user1".to_string(),
            }),
        }];
        assert!(!has_approval(&reviews));
    }

    #[test]
    fn test_has_approval_approved() {
        let reviews = vec![PrReview {
            id: 1,
            state: "APPROVED".to_string(),
            user: Some(ReviewUser {
                login: "user1".to_string(),
            }),
        }];
        assert!(has_approval(&reviews));
    }

    #[test]
    fn test_has_approval_changes_then_approved_same_user() {
        let reviews = vec![
            PrReview {
                id: 1,
                state: "CHANGES_REQUESTED".to_string(),
                user: Some(ReviewUser {
                    login: "user1".to_string(),
                }),
            },
            PrReview {
                id: 2,
                state: "APPROVED".to_string(),
                user: Some(ReviewUser {
                    login: "user1".to_string(),
                }),
            },
        ];
        // Last review from user1 is APPROVED
        assert!(has_approval(&reviews));
    }

    #[test]
    fn test_has_approval_approved_then_changes_same_user() {
        let reviews = vec![
            PrReview {
                id: 1,
                state: "APPROVED".to_string(),
                user: Some(ReviewUser {
                    login: "user1".to_string(),
                }),
            },
            PrReview {
                id: 2,
                state: "CHANGES_REQUESTED".to_string(),
                user: Some(ReviewUser {
                    login: "user1".to_string(),
                }),
            },
        ];
        // Last review from user1 is CHANGES_REQUESTED
        assert!(!has_approval(&reviews));
    }

    #[test]
    fn test_has_approval_mixed_users() {
        let reviews = vec![
            PrReview {
                id: 1,
                state: "APPROVED".to_string(),
                user: Some(ReviewUser {
                    login: "user1".to_string(),
                }),
            },
            PrReview {
                id: 2,
                state: "CHANGES_REQUESTED".to_string(),
                user: Some(ReviewUser {
                    login: "user2".to_string(),
                }),
            },
        ];
        // user1 approved, user2 requested changes → not approved
        assert!(!has_approval(&reviews));
    }

    #[test]
    fn test_has_approval_review_without_user() {
        let reviews = vec![PrReview {
            id: 1,
            state: "APPROVED".to_string(),
            user: None,
        }];
        // No user means review is ignored
        assert!(!has_approval(&reviews));
    }

    // ── pr_is_merged ───────────────────────────────────────────────

    #[test]
    fn test_pr_is_merged_true() {
        let pr = PrInfo {
            number: 42,
            mergeable: None,
            mergeable_state: None,
            state: "closed".to_string(),
            merged: Some(true),
        };
        assert!(pr_is_merged(&pr));
    }

    #[test]
    fn test_pr_is_merged_false() {
        let pr = PrInfo {
            number: 42,
            mergeable: Some(true),
            mergeable_state: Some("clean".to_string()),
            state: "open".to_string(),
            merged: Some(false),
        };
        assert!(!pr_is_merged(&pr));
    }

    #[test]
    fn test_pr_is_merged_none() {
        let pr = PrInfo {
            number: 42,
            mergeable: None,
            mergeable_state: None,
            state: "open".to_string(),
            merged: None,
        };
        assert!(!pr_is_merged(&pr));
    }

    // ── enable_auto_merge ──────────────────────────────────────────

    #[tokio::test]
    async fn test_enable_auto_merge_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("PUT", "/repos/test-org/test-repo/pulls/42/merge")
            .match_header("Authorization", "Bearer test-token-123")
            .match_header("Accept", "application/vnd.github+json")
            .match_header("User-Agent", "symphony")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"sha": "abc123", "merged": true}"#)
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.enable_auto_merge(42).await.unwrap();
        assert!(result);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_enable_auto_merge_not_mergeable() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("PUT", "/repos/test-org/test-repo/pulls/42/merge")
            .with_status(405)
            .with_body("not mergeable")
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.enable_auto_merge(42).await.unwrap();
        assert!(!result);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_enable_auto_merge_conflict() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("PUT", "/repos/test-org/test-repo/pulls/42/merge")
            .with_status(409)
            .with_body("conflict")
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.enable_auto_merge(42).await.unwrap();
        assert!(!result);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_enable_auto_merge_api_error() {
        let config = test_github_config("http://127.0.0.1:1");
        let client = GitHubClient::new(&config, new_http());
        let result = client.enable_auto_merge(42).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SymphonyError::GitHubApiRequest { .. }
        ));
    }

    #[tokio::test]
    async fn test_enable_auto_merge_unexpected_status() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("PUT", "/repos/test-org/test-repo/pulls/42/merge")
            .with_status(403)
            .with_body("forbidden")
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.enable_auto_merge(42).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SymphonyError::GitHubApiStatus { status: 403, .. }
        ));
        mock.assert_async().await;
    }

    // ── is_pr_merged ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_is_pr_merged_yes() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/actcli-42".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "closed",
                    "merged": true
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.is_pr_merged("symphony/actcli-42").await.unwrap();
        assert_eq!(result, Some(true));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_is_pr_merged_no_still_open() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/actcli-42".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "merged": false
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.is_pr_merged("symphony/actcli-42").await.unwrap();
        assert_eq!(result, Some(false));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_is_pr_merged_no_pr_exists() {
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
        let result = client.is_pr_merged("symphony/actcli-99").await.unwrap();
        assert_eq!(result, None);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_is_pr_merged_api_error() {
        let config = test_github_config("http://127.0.0.1:1");
        let client = GitHubClient::new(&config, new_http());
        let result = client.is_pr_merged("symphony/actcli-42").await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SymphonyError::GitHubApiRequest { .. }
        ));
    }

    #[tokio::test]
    async fn test_is_pr_merged_non_200_status() {
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
        let result = client.is_pr_merged("symphony/actcli-42").await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SymphonyError::GitHubApiStatus { status: 403, .. }
        ));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_is_pr_merged_invalid_json() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("not json")
            .create_async()
            .await;

        let config = test_github_config(&server.url());
        let client = GitHubClient::new(&config, new_http());
        let result = client.is_pr_merged("symphony/actcli-42").await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SymphonyError::GitHubApiRequest { .. }
        ));
        mock.assert_async().await;
    }

    // ── PrReview / ReviewUser ──────────────────────────────────────

    #[test]
    fn test_pr_review_deserializes() {
        let json = r#"{"id": 1, "state": "APPROVED", "user": {"login": "alice"}}"#;
        let review: PrReview = serde_json::from_str(json).unwrap();
        assert_eq!(review.id, 1);
        assert_eq!(review.state, "APPROVED");
        assert_eq!(review.user.unwrap().login, "alice");
    }

    #[test]
    fn test_pr_review_deserializes_without_user() {
        let json = r#"{"id": 1, "state": "APPROVED"}"#;
        let review: PrReview = serde_json::from_str(json).unwrap();
        assert!(review.user.is_none());
    }

    #[test]
    fn test_pr_review_debug_and_clone() {
        let review = PrReview {
            id: 1,
            state: "APPROVED".to_string(),
            user: Some(ReviewUser {
                login: "alice".to_string(),
            }),
        };
        let debug = format!("{:?}", review);
        assert!(debug.contains("PrReview"));
        let cloned = review.clone();
        assert_eq!(cloned.id, 1);
        assert_eq!(cloned.user.unwrap().login, "alice");
    }

    #[test]
    fn test_review_user_debug_and_clone() {
        let user = ReviewUser {
            login: "bob".to_string(),
        };
        let debug = format!("{:?}", user);
        assert!(debug.contains("ReviewUser"));
        let cloned = user.clone();
        assert_eq!(cloned.login, "bob");
    }
}
