use crate::config::TrackerConfig;
use crate::error::{Result, SymphonyError};
use crate::model::{BlockerRef, Issue};
use chrono::{DateTime, Utc};
use serde_json::Value;

/// Linear issue tracker client.
pub struct LinearClient {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
    project_slug: String,
    page_size: u32,
    timeout_ms: u64,
}

impl LinearClient {
    pub fn new(config: &TrackerConfig) -> Self {
        Self {
            http: reqwest::Client::new(),
            endpoint: config.endpoint.clone(),
            api_key: config.api_key.clone(),
            project_slug: config.project_slug.clone(),
            page_size: 50,
            timeout_ms: 30_000,
        }
    }

    /// Fetch candidate issues in the configured active states.
    pub async fn fetch_candidate_issues(&self, active_states: &[String]) -> Result<Vec<Issue>> {
        if active_states.is_empty() {
            return Ok(Vec::new());
        }

        let mut all_issues = Vec::new();
        let mut after_cursor: Option<String> = None;

        loop {
            let (issues, page_info) = self
                .fetch_issues_page(active_states, after_cursor.as_deref())
                .await?;

            all_issues.extend(issues);

            if !page_info.has_next_page {
                break;
            }

            match page_info.end_cursor {
                Some(cursor) => after_cursor = Some(cursor),
                None => {
                    return Err(SymphonyError::LinearMissingEndCursor);
                }
            }
        }

        Ok(all_issues)
    }

    /// Fetch current states for a set of issue IDs (reconciliation).
    pub async fn fetch_issue_states_by_ids(&self, ids: &[String]) -> Result<Vec<Issue>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let query = r#"
            query IssuesByIds($ids: [ID!]!) {
                issues(filter: { id: { in: $ids } }) {
                    nodes {
                        id
                        identifier
                        title
                        description
                        priority
                        state { name }
                        branchName
                        url
                        labels { nodes { name } }
                        relations {
                            nodes {
                                type
                                relatedIssue {
                                    id
                                    identifier
                                    state { name }
                                }
                            }
                        }
                        createdAt
                        updatedAt
                    }
                }
            }
        "#;

        let variables = serde_json::json!({ "ids": ids });
        let body = serde_json::json!({ "query": query, "variables": variables });

        let response = self.execute_graphql(&body).await?;
        let nodes = extract_nodes(&response, &["data", "issues", "nodes"])?;

        nodes.into_iter().map(normalize_issue).collect()
    }

    /// Fetch issues in specified states (for startup terminal cleanup).
    pub async fn fetch_issues_by_states(&self, states: &[String]) -> Result<Vec<Issue>> {
        if states.is_empty() {
            return Ok(Vec::new());
        }

        self.fetch_candidate_issues(states).await
    }

    async fn fetch_issues_page(
        &self,
        active_states: &[String],
        after: Option<&str>,
    ) -> Result<(Vec<Issue>, PageInfo)> {
        let query = r#"
            query CandidateIssues($projectSlug: String!, $states: [String!]!, $first: Int!, $after: String) {
                issues(
                    filter: {
                        project: { slugId: { eq: $projectSlug } }
                        state: { name: { in: $states } }
                    }
                    first: $first
                    after: $after
                    orderBy: createdAt
                ) {
                    nodes {
                        id
                        identifier
                        title
                        description
                        priority
                        state { name }
                        branchName
                        url
                        labels { nodes { name } }
                        relations {
                            nodes {
                                type
                                relatedIssue {
                                    id
                                    identifier
                                    state { name }
                                }
                            }
                        }
                        createdAt
                        updatedAt
                    }
                    pageInfo {
                        hasNextPage
                        endCursor
                    }
                }
            }
        "#;

        let mut variables = serde_json::json!({
            "projectSlug": self.project_slug,
            "states": active_states,
            "first": self.page_size,
        });

        if let Some(cursor) = after {
            variables["after"] = serde_json::Value::String(cursor.to_string());
        }

        let body = serde_json::json!({ "query": query, "variables": variables });
        let response = self.execute_graphql(&body).await?;

        let nodes = extract_nodes(&response, &["data", "issues", "nodes"])?;
        let issues: Vec<Issue> = nodes
            .into_iter()
            .map(normalize_issue)
            .collect::<Result<_>>()?;

        let page_info_val =
            navigate_json(&response, &["data", "issues", "pageInfo"]).ok_or_else(|| {
                SymphonyError::LinearUnknownPayload {
                    reason: "missing pageInfo".to_string(),
                }
            })?;

        let page_info = PageInfo {
            has_next_page: page_info_val["hasNextPage"].as_bool().unwrap_or(false),
            end_cursor: page_info_val["endCursor"].as_str().map(|s| s.to_string()),
        };

        Ok((issues, page_info))
    }

    async fn execute_graphql(&self, body: &serde_json::Value) -> Result<serde_json::Value> {
        let response = self
            .http
            .post(&self.endpoint)
            .header("Authorization", &self.api_key)
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_millis(self.timeout_ms))
            .json(body)
            .send()
            .await
            .map_err(|e| SymphonyError::LinearApiRequest {
                reason: e.to_string(),
            })?;

        let status = response.status().as_u16();
        if status != 200 {
            let body_text = response.text().await.unwrap_or_default();
            return Err(SymphonyError::LinearApiStatus {
                status,
                body: body_text,
            });
        }

        let json: serde_json::Value =
            response
                .json()
                .await
                .map_err(|e| SymphonyError::LinearApiRequest {
                    reason: format!("failed to parse response: {e}"),
                })?;

        // Check for GraphQL errors
        if let Some(errors) = json.get("errors") {
            if let Some(arr) = errors.as_array() {
                if !arr.is_empty() {
                    return Err(SymphonyError::LinearGraphqlErrors {
                        errors: serde_json::to_string(errors).unwrap_or_default(),
                    });
                }
            }
        }

        Ok(json)
    }
}

#[derive(Debug)]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

fn navigate_json<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(key)?;
    }
    Some(current)
}

fn extract_nodes<'a>(response: &'a Value, path: &[&str]) -> Result<Vec<&'a Value>> {
    let nodes =
        navigate_json(response, path).ok_or_else(|| SymphonyError::LinearUnknownPayload {
            reason: format!("missing path: {}", path.join(".")),
        })?;

    nodes
        .as_array()
        .map(|arr| arr.iter().collect())
        .ok_or_else(|| SymphonyError::LinearUnknownPayload {
            reason: "nodes is not an array".to_string(),
        })
}

fn normalize_issue(node: &Value) -> Result<Issue> {
    let id = node["id"]
        .as_str()
        .ok_or_else(|| SymphonyError::LinearUnknownPayload {
            reason: "missing issue id".to_string(),
        })?
        .to_string();

    let identifier = node["identifier"].as_str().unwrap_or("").to_string();

    let title = node["title"].as_str().unwrap_or("").to_string();
    let description = node["description"].as_str().map(|s| s.to_string());
    let priority = node["priority"].as_i64().map(|v| v as i32);
    let state = node["state"]["name"].as_str().unwrap_or("").to_string();
    let branch_name = node["branchName"].as_str().map(|s| s.to_string());
    let url = node["url"].as_str().map(|s| s.to_string());

    // Labels: normalized to lowercase
    let labels = node["labels"]["nodes"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l["name"].as_str())
                .map(|s| s.to_lowercase())
                .collect()
        })
        .unwrap_or_default();

    // Blocked_by: inverse relations of type "blocks"
    let blocked_by = node["relations"]["nodes"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|rel| rel["type"].as_str() == Some("blocks"))
                .map(|rel| {
                    let related = &rel["relatedIssue"];
                    BlockerRef {
                        id: related["id"].as_str().map(|s| s.to_string()),
                        identifier: related["identifier"].as_str().map(|s| s.to_string()),
                        state: related["state"]["name"].as_str().map(|s| s.to_string()),
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let created_at = node["createdAt"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    let updated_at = node["updatedAt"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    Ok(Issue {
        id,
        identifier,
        title,
        description,
        priority,
        state,
        branch_name,
        url,
        labels,
        blocked_by,
        created_at,
        updated_at,
    })
}

/// Execute a raw GraphQL query against Linear (for the linear_graphql extension).
pub async fn execute_linear_graphql(
    config: &TrackerConfig,
    query: &str,
    variables: Option<&serde_json::Value>,
) -> std::result::Result<serde_json::Value, String> {
    let client = reqwest::Client::new();

    let mut body = serde_json::json!({ "query": query });
    if let Some(vars) = variables {
        body["variables"] = vars.clone();
    }

    let response = client
        .post(&config.endpoint)
        .header("Authorization", &config.api_key)
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_millis(30_000))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("transport error: {e}"))?;

    let status = response.status().as_u16();
    if status != 200 {
        let body_text = response.text().await.unwrap_or_default();
        return Err(format!("HTTP {status}: {body_text}"));
    }

    response
        .json()
        .await
        .map_err(|e| format!("parse error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_issue_minimal() {
        let node = serde_json::json!({
            "id": "abc123",
            "identifier": "PROJ-1",
            "title": "Fix bug",
            "description": "A bug fix",
            "priority": 2,
            "state": { "name": "Todo" },
            "branchName": null,
            "url": "https://linear.app/proj/PROJ-1",
            "labels": { "nodes": [{ "name": "Bug" }, { "name": "P1" }] },
            "relations": { "nodes": [] },
            "createdAt": "2025-01-01T00:00:00Z",
            "updatedAt": "2025-01-02T00:00:00Z"
        });

        let issue = normalize_issue(&node).unwrap();
        assert_eq!(issue.id, "abc123");
        assert_eq!(issue.identifier, "PROJ-1");
        assert_eq!(issue.title, "Fix bug");
        assert_eq!(issue.priority, Some(2));
        assert_eq!(issue.state, "Todo");
        assert_eq!(issue.labels, vec!["bug", "p1"]);
        assert!(issue.blocked_by.is_empty());
    }

    #[test]
    fn test_normalize_issue_with_blockers() {
        let node = serde_json::json!({
            "id": "abc123",
            "identifier": "PROJ-1",
            "title": "Fix bug",
            "state": { "name": "Todo" },
            "labels": { "nodes": [] },
            "relations": {
                "nodes": [
                    {
                        "type": "blocks",
                        "relatedIssue": {
                            "id": "def456",
                            "identifier": "PROJ-2",
                            "state": { "name": "In Progress" }
                        }
                    },
                    {
                        "type": "related",
                        "relatedIssue": {
                            "id": "ghi789",
                            "identifier": "PROJ-3",
                            "state": { "name": "Done" }
                        }
                    }
                ]
            }
        });

        let issue = normalize_issue(&node).unwrap();
        assert_eq!(issue.blocked_by.len(), 1);
        assert_eq!(issue.blocked_by[0].identifier.as_deref(), Some("PROJ-2"));
    }
}
