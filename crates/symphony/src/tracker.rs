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
    team_key: String,
    page_size: u32,
    timeout_ms: u64,
}

impl LinearClient {
    /// Create a new LinearClient, sharing the given `reqwest::Client` for
    /// connection pooling. Call sites should create one `reqwest::Client` at
    /// startup and pass it to every `LinearClient` instance.
    pub fn new(config: &TrackerConfig, http: reqwest::Client) -> Self {
        Self {
            http,
            endpoint: config.endpoint.clone(),
            api_key: config.api_key.clone(),
            team_key: config.team_key.clone(),
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

    /// Transition an issue to a new workflow state by name.
    ///
    /// Resolves the target state name to a workflow state ID for the team,
    /// then calls `issueUpdate` to apply the transition.
    pub async fn transition_issue_state(
        &self,
        issue_id: &str,
        target_state_name: &str,
    ) -> Result<()> {
        let state_id = self.resolve_state_id(target_state_name).await?;
        self.update_issue_state(issue_id, &state_id).await
    }

    async fn resolve_state_id(&self, state_name: &str) -> Result<String> {
        let query = r#"
            query WorkflowStates($teamKey: String!) {
                workflowStates(filter: { team: { key: { eq: $teamKey } } }) {
                    nodes { id name }
                }
            }
        "#;
        let variables = serde_json::json!({ "teamKey": self.team_key });
        let body = serde_json::json!({ "query": query, "variables": variables });
        let response = self.execute_graphql(&body).await?;

        let nodes = extract_nodes(&response, &["data", "workflowStates", "nodes"])?;
        for node in nodes {
            if let Some(id) = extract_matching_state_id(&node, state_name) {
                return Ok(id);
            }
        }

        Err(SymphonyError::LinearUnknownPayload {
            reason: format!("workflow state '{state_name}' not found for team"),
        })
    }

    async fn update_issue_state(&self, issue_id: &str, state_id: &str) -> Result<()> {
        let query = r#"
            mutation UpdateIssueState($issueId: String!, $stateId: String!) {
                issueUpdate(id: $issueId, input: { stateId: $stateId }) {
                    success
                }
            }
        "#;
        let variables = serde_json::json!({
            "issueId": issue_id,
            "stateId": state_id,
        });
        let body = serde_json::json!({ "query": query, "variables": variables });
        let response = self.execute_graphql(&body).await?;

        let success = navigate_json(&response, &["data", "issueUpdate", "success"])
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if success {
            Ok(())
        } else {
            Err(SymphonyError::LinearUnknownPayload {
                reason: format!("issueUpdate returned success=false for issue {issue_id}"),
            })
        }
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
            query CandidateIssues($teamKey: String!, $states: [String!]!, $first: Int!, $after: String) {
                issues(
                    filter: {
                        team: { key: { eq: $teamKey } }
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
            "teamKey": self.team_key,
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

/// Extract a workflow state ID from a node if its name matches (case-insensitive).
///
/// Extracted as a named function to avoid LLVM coverage splits on nested
/// `if let` / `.and_then()` closures that differ between macOS and Linux.
fn extract_matching_state_id(node: &Value, target_name: &str) -> Option<String> {
    let name = node.get("name").and_then(Value::as_str)?;
    if !name.eq_ignore_ascii_case(target_name) {
        return None;
    }
    node.get("id").and_then(Value::as_str).map(String::from)
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

    // ── navigate_json ────────────────────────────────────────────────

    #[test]
    fn test_navigate_json_nested_success() {
        let val = serde_json::json!({
            "data": { "issues": { "nodes": [1, 2, 3] } }
        });
        let result = navigate_json(&val, &["data", "issues", "nodes"]);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), &serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn test_navigate_json_empty_path_returns_root() {
        let val = serde_json::json!({"foo": "bar"});
        let result = navigate_json(&val, &[]);
        assert_eq!(result.unwrap(), &val);
    }

    #[test]
    fn test_navigate_json_missing_key_returns_none() {
        let val = serde_json::json!({"data": {"issues": {}}});
        let result = navigate_json(&val, &["data", "issues", "nodes"]);
        assert!(result.is_none());
    }

    #[test]
    fn test_navigate_json_non_object_intermediate_returns_none() {
        let val = serde_json::json!({"data": "not_an_object"});
        let result = navigate_json(&val, &["data", "issues", "nodes"]);
        assert!(result.is_none());
    }

    // ── extract_nodes ────────────────────────────────────────────────

    #[test]
    fn test_extract_nodes_valid_array() {
        let val = serde_json::json!({
            "data": { "issues": { "nodes": [{"id": "a"}, {"id": "b"}] } }
        });
        let nodes = extract_nodes(&val, &["data", "issues", "nodes"]).unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0]["id"], "a");
        assert_eq!(nodes[1]["id"], "b");
    }

    #[test]
    fn test_extract_nodes_missing_path_returns_error() {
        let val = serde_json::json!({"data": {}});
        let err = extract_nodes(&val, &["data", "issues", "nodes"]).unwrap_err();
        assert!(
            matches!(&err, SymphonyError::LinearUnknownPayload { reason }
            if reason.contains("missing path") && reason.contains("data.issues.nodes"))
        );
    }

    #[test]
    fn test_extract_nodes_non_array_returns_error() {
        let val = serde_json::json!({
            "data": { "issues": { "nodes": "not_an_array" } }
        });
        let err = extract_nodes(&val, &["data", "issues", "nodes"]).unwrap_err();
        assert!(
            matches!(&err, SymphonyError::LinearUnknownPayload { reason }
            if reason.contains("not an array"))
        );
    }

    // ── normalize_issue ──────────────────────────────────────────────

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
        assert_eq!(issue.description.as_deref(), Some("A bug fix"));
        assert_eq!(issue.priority, Some(2));
        assert_eq!(issue.state, "Todo");
        assert_eq!(issue.branch_name, None);
        assert_eq!(issue.url.as_deref(), Some("https://linear.app/proj/PROJ-1"));
        assert_eq!(issue.labels, vec!["bug", "p1"]);
        assert!(issue.blocked_by.is_empty());
        assert!(issue.created_at.is_some());
        assert!(issue.updated_at.is_some());
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

    #[test]
    fn test_normalize_issue_missing_id_returns_error() {
        let node = serde_json::json!({
            "identifier": "PROJ-1",
            "title": "No ID",
            "state": { "name": "Todo" }
        });
        let err = normalize_issue(&node).unwrap_err();
        assert!(
            matches!(&err, SymphonyError::LinearUnknownPayload { reason }
            if reason.contains("missing issue id"))
        );
    }

    #[test]
    fn test_normalize_issue_no_labels_nodes_defaults_empty() {
        let node = serde_json::json!({
            "id": "x1",
            "identifier": "PROJ-5",
            "title": "No labels key",
            "state": { "name": "Todo" }
        });
        let issue = normalize_issue(&node).unwrap();
        assert!(issue.labels.is_empty());
    }

    #[test]
    fn test_normalize_issue_no_relations_nodes_defaults_empty() {
        let node = serde_json::json!({
            "id": "x2",
            "identifier": "PROJ-6",
            "title": "No relations key",
            "state": { "name": "Todo" }
        });
        let issue = normalize_issue(&node).unwrap();
        assert!(issue.blocked_by.is_empty());
    }

    #[test]
    fn test_normalize_issue_null_optional_fields() {
        let node = serde_json::json!({
            "id": "x3",
            "identifier": null,
            "title": null,
            "description": null,
            "priority": null,
            "state": { "name": null },
            "branchName": null,
            "url": null,
            "labels": { "nodes": null },
            "relations": { "nodes": null },
            "createdAt": null,
            "updatedAt": null
        });
        let issue = normalize_issue(&node).unwrap();
        assert_eq!(issue.identifier, "");
        assert_eq!(issue.title, "");
        assert!(issue.description.is_none());
        assert!(issue.priority.is_none());
        assert_eq!(issue.state, "");
        assert!(issue.branch_name.is_none());
        assert!(issue.url.is_none());
        assert!(issue.labels.is_empty());
        assert!(issue.blocked_by.is_empty());
        assert!(issue.created_at.is_none());
        assert!(issue.updated_at.is_none());
    }

    #[test]
    fn test_normalize_issue_invalid_dates_result_in_none() {
        let node = serde_json::json!({
            "id": "x4",
            "identifier": "PROJ-7",
            "title": "Bad dates",
            "state": { "name": "Todo" },
            "createdAt": "not-a-date",
            "updatedAt": "also-not-a-date"
        });
        let issue = normalize_issue(&node).unwrap();
        assert!(issue.created_at.is_none());
        assert!(issue.updated_at.is_none());
    }

    #[test]
    fn test_normalize_issue_multiple_blocks_relations() {
        let node = serde_json::json!({
            "id": "x5",
            "identifier": "PROJ-8",
            "title": "Multi blockers",
            "state": { "name": "Todo" },
            "relations": {
                "nodes": [
                    {
                        "type": "blocks",
                        "relatedIssue": {
                            "id": "b1", "identifier": "PROJ-10",
                            "state": { "name": "Todo" }
                        }
                    },
                    {
                        "type": "blocks",
                        "relatedIssue": {
                            "id": "b2", "identifier": "PROJ-11",
                            "state": { "name": "In Progress" }
                        }
                    },
                    {
                        "type": "blocks",
                        "relatedIssue": {
                            "id": "b3", "identifier": "PROJ-12",
                            "state": { "name": "Done" }
                        }
                    }
                ]
            }
        });
        let issue = normalize_issue(&node).unwrap();
        assert_eq!(issue.blocked_by.len(), 3);
        assert_eq!(issue.blocked_by[0].id.as_deref(), Some("b1"));
        assert_eq!(issue.blocked_by[1].id.as_deref(), Some("b2"));
        assert_eq!(issue.blocked_by[2].id.as_deref(), Some("b3"));
    }

    #[test]
    fn test_normalize_issue_related_type_filtered_out() {
        let node = serde_json::json!({
            "id": "x6",
            "identifier": "PROJ-9",
            "title": "Only related",
            "state": { "name": "Todo" },
            "relations": {
                "nodes": [
                    {
                        "type": "related",
                        "relatedIssue": {
                            "id": "r1", "identifier": "PROJ-20",
                            "state": { "name": "Done" }
                        }
                    }
                ]
            }
        });
        let issue = normalize_issue(&node).unwrap();
        assert!(issue.blocked_by.is_empty());
    }

    // ── LinearClient::new ────────────────────────────────────────────

    fn new_http() -> reqwest::Client {
        reqwest::Client::new()
    }

    #[test]
    fn test_linear_client_new_sets_fields() {
        let config = TrackerConfig {
            kind: "linear".to_string(),
            endpoint: "https://api.linear.app/graphql".to_string(),
            api_key: "lin_api_test123".to_string(),
            team_key: "my-project".to_string(),
            active_states: vec!["Todo".to_string()],
            terminal_states: vec!["Done".to_string()],
        };
        let client = LinearClient::new(&config, new_http());
        assert_eq!(client.endpoint, "https://api.linear.app/graphql");
        assert_eq!(client.api_key, "lin_api_test123");
        assert_eq!(client.team_key, "my-project");
        assert_eq!(client.page_size, 50);
        assert_eq!(client.timeout_ms, 30_000);
    }

    // ── Helper: build a TrackerConfig pointing at a mockito server ───

    fn mock_tracker_config(server_url: &str) -> TrackerConfig {
        TrackerConfig {
            kind: "linear".to_string(),
            endpoint: server_url.to_string(),
            api_key: "test-api-key".to_string(),
            team_key: "test-project".to_string(),
            active_states: vec!["Todo".to_string(), "In Progress".to_string()],
            terminal_states: vec!["Done".to_string()],
        }
    }

    /// Build a minimal valid issue JSON node for mock responses.
    fn mock_issue_node(id: &str, identifier: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "identifier": identifier,
            "title": format!("Issue {identifier}"),
            "description": "desc",
            "priority": 1,
            "state": { "name": "Todo" },
            "branchName": "feat/branch",
            "url": format!("https://linear.app/{identifier}"),
            "labels": { "nodes": [{ "name": "Feature" }] },
            "relations": { "nodes": [] },
            "createdAt": "2025-06-01T12:00:00Z",
            "updatedAt": "2025-06-02T12:00:00Z"
        })
    }

    // ── execute_graphql (via LinearClient) ───────────────────────────

    #[tokio::test]
    async fn test_execute_graphql_200_valid_json() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data": {"viewer": {"id": "u1"}}}"#)
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let client = LinearClient::new(&config, new_http());
        let body = serde_json::json!({"query": "{ viewer { id } }"});
        let result = client.execute_graphql(&body).await;

        assert!(result.is_ok());
        let json = result.unwrap();
        assert_eq!(json["data"]["viewer"]["id"], "u1");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_execute_graphql_200_with_graphql_errors() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"errors": [{"message": "Field 'foo' not found"}], "data": null}"#)
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let client = LinearClient::new(&config, new_http());
        let body = serde_json::json!({"query": "{ foo }"});
        let err = client.execute_graphql(&body).await.unwrap_err();

        match err {
            SymphonyError::LinearGraphqlErrors { errors } => {
                assert!(errors.contains("Field 'foo' not found"));
            }
            other => panic!("expected LinearGraphqlErrors, got: {other:?}"),
        }
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_execute_graphql_200_with_empty_errors_array_is_ok() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"errors": [], "data": {"ok": true}}"#)
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let client = LinearClient::new(&config, new_http());
        let body = serde_json::json!({"query": "{ ok }"});
        let result = client.execute_graphql(&body).await;
        assert!(result.is_ok());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_execute_graphql_non_200_status() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(401)
            .with_body("Unauthorized")
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let client = LinearClient::new(&config, new_http());
        let body = serde_json::json!({"query": "{ viewer { id } }"});
        let err = client.execute_graphql(&body).await.unwrap_err();

        match err {
            SymphonyError::LinearApiStatus { status, body } => {
                assert_eq!(status, 401);
                assert_eq!(body, "Unauthorized");
            }
            other => panic!("expected LinearApiStatus, got: {other:?}"),
        }
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_execute_graphql_invalid_json_response() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("this is not json")
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let client = LinearClient::new(&config, new_http());
        let body = serde_json::json!({"query": "{ viewer { id } }"});
        let err = client.execute_graphql(&body).await.unwrap_err();

        match err {
            SymphonyError::LinearApiRequest { reason } => {
                assert!(reason.contains("failed to parse response"));
            }
            other => panic!("expected LinearApiRequest, got: {other:?}"),
        }
        mock.assert_async().await;
    }

    // ── fetch_candidate_issues ───────────────────────────────────────

    #[tokio::test]
    async fn test_fetch_candidate_issues_empty_states_returns_empty() {
        // No server needed — short-circuits before HTTP
        let config = mock_tracker_config("http://unused");
        let client = LinearClient::new(&config, new_http());
        let result = client.fetch_candidate_issues(&[]).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_fetch_candidate_issues_single_page() {
        let mut server = mockito::Server::new_async().await;
        let response_body = serde_json::json!({
            "data": {
                "issues": {
                    "nodes": [
                        mock_issue_node("id1", "PROJ-1"),
                        mock_issue_node("id2", "PROJ-2")
                    ],
                    "pageInfo": {
                        "hasNextPage": false,
                        "endCursor": null
                    }
                }
            }
        });
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(response_body.to_string())
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let client = LinearClient::new(&config, new_http());
        let issues = client
            .fetch_candidate_issues(&["Todo".to_string()])
            .await
            .unwrap();

        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].id, "id1");
        assert_eq!(issues[1].id, "id2");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_fetch_candidate_issues_multi_page() {
        let mut server = mockito::Server::new_async().await;

        // Page 1: hasNextPage=true with endCursor
        let page1 = serde_json::json!({
            "data": {
                "issues": {
                    "nodes": [mock_issue_node("id1", "PROJ-1")],
                    "pageInfo": {
                        "hasNextPage": true,
                        "endCursor": "cursor_abc"
                    }
                }
            }
        });
        // Page 2: hasNextPage=false
        let page2 = serde_json::json!({
            "data": {
                "issues": {
                    "nodes": [mock_issue_node("id2", "PROJ-2")],
                    "pageInfo": {
                        "hasNextPage": false,
                        "endCursor": null
                    }
                }
            }
        });

        let mock1 = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(page1.to_string())
            .expect(1)
            .create_async()
            .await;

        let mock2 = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(page2.to_string())
            .expect(1)
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let client = LinearClient::new(&config, new_http());
        let issues = client
            .fetch_candidate_issues(&["Todo".to_string()])
            .await
            .unwrap();

        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].id, "id1");
        assert_eq!(issues[1].id, "id2");
        mock1.assert_async().await;
        mock2.assert_async().await;
    }

    #[tokio::test]
    async fn test_fetch_candidate_issues_missing_end_cursor_on_has_next_page() {
        let mut server = mockito::Server::new_async().await;
        let response_body = serde_json::json!({
            "data": {
                "issues": {
                    "nodes": [mock_issue_node("id1", "PROJ-1")],
                    "pageInfo": {
                        "hasNextPage": true,
                        "endCursor": null
                    }
                }
            }
        });
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(response_body.to_string())
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let client = LinearClient::new(&config, new_http());
        let err = client
            .fetch_candidate_issues(&["Todo".to_string()])
            .await
            .unwrap_err();

        assert!(matches!(&err, SymphonyError::LinearMissingEndCursor));
        mock.assert_async().await;
    }

    // ── fetch_issue_states_by_ids ────────────────────────────────────

    #[tokio::test]
    async fn test_fetch_issue_states_by_ids_empty_ids_returns_empty() {
        let config = mock_tracker_config("http://unused");
        let client = LinearClient::new(&config, new_http());
        let result = client.fetch_issue_states_by_ids(&[]).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_fetch_issue_states_by_ids_valid_response() {
        let mut server = mockito::Server::new_async().await;
        let response_body = serde_json::json!({
            "data": {
                "issues": {
                    "nodes": [
                        mock_issue_node("id1", "PROJ-1"),
                        mock_issue_node("id2", "PROJ-2")
                    ]
                }
            }
        });
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(response_body.to_string())
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let client = LinearClient::new(&config, new_http());
        let issues = client
            .fetch_issue_states_by_ids(&["id1".to_string(), "id2".to_string()])
            .await
            .unwrap();

        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].identifier, "PROJ-1");
        assert_eq!(issues[1].identifier, "PROJ-2");
        mock.assert_async().await;
    }

    // ── fetch_issues_by_states ───────────────────────────────────────

    #[tokio::test]
    async fn test_fetch_issues_by_states_empty_states_returns_empty() {
        let config = mock_tracker_config("http://unused");
        let client = LinearClient::new(&config, new_http());
        let result = client.fetch_issues_by_states(&[]).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_fetch_issues_by_states_delegates_to_fetch_candidate() {
        let mut server = mockito::Server::new_async().await;
        let response_body = serde_json::json!({
            "data": {
                "issues": {
                    "nodes": [mock_issue_node("id1", "PROJ-1")],
                    "pageInfo": {
                        "hasNextPage": false,
                        "endCursor": null
                    }
                }
            }
        });
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(response_body.to_string())
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let client = LinearClient::new(&config, new_http());
        let issues = client
            .fetch_issues_by_states(&["Done".to_string()])
            .await
            .unwrap();

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "id1");
        mock.assert_async().await;
    }

    // ── execute_linear_graphql (free function) ───────────────────────

    #[tokio::test]
    async fn test_execute_linear_graphql_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data": {"viewer": {"id": "u1"}}}"#)
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let result = execute_linear_graphql(&config, "{ viewer { id } }", None).await;
        assert!(result.is_ok());
        let json = result.unwrap();
        assert_eq!(json["data"]["viewer"]["id"], "u1");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_execute_linear_graphql_with_variables() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data": {"issue": {"id": "i1"}}}"#)
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let vars = serde_json::json!({"id": "i1"});
        let result = execute_linear_graphql(
            &config,
            "query($id: ID!) { issue(id: $id) { id } }",
            Some(&vars),
        )
        .await;

        assert!(result.is_ok());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_execute_linear_graphql_http_error() {
        // Point at a port where nothing is listening
        let config = TrackerConfig {
            kind: "linear".to_string(),
            endpoint: "http://127.0.0.1:1".to_string(),
            api_key: "key".to_string(),
            team_key: "proj".to_string(),
            active_states: vec![],
            terminal_states: vec![],
        };
        let result = execute_linear_graphql(&config, "{ viewer { id } }", None).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("transport error"));
    }

    #[tokio::test]
    async fn test_execute_linear_graphql_non_200_status() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(403)
            .with_body("Forbidden")
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let result = execute_linear_graphql(&config, "{ viewer { id } }", None).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("HTTP 403"));
        assert!(err.contains("Forbidden"));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_execute_linear_graphql_invalid_json_response() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("not json at all")
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let result = execute_linear_graphql(&config, "{ viewer { id } }", None).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("parse error"));
        mock.assert_async().await;
    }

    // ── fetch_issues_page: missing pageInfo ──────────────────────────

    #[tokio::test]
    async fn test_fetch_issues_page_missing_page_info() {
        let mut server = mockito::Server::new_async().await;
        // Response has nodes but no pageInfo
        let response_body = serde_json::json!({
            "data": {
                "issues": {
                    "nodes": [mock_issue_node("id1", "PROJ-1")]
                }
            }
        });
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(response_body.to_string())
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let client = LinearClient::new(&config, new_http());
        let err = client
            .fetch_candidate_issues(&["Todo".to_string()])
            .await
            .unwrap_err();

        assert!(
            matches!(&err, SymphonyError::LinearUnknownPayload { reason }
            if reason.contains("missing pageInfo"))
        );
        mock.assert_async().await;
    }

    // ── execute_graphql: transport error ─────────────────────────────

    #[tokio::test]
    async fn test_execute_graphql_transport_error() {
        // Connect to a port that is not listening to trigger a send error
        let config = mock_tracker_config("http://127.0.0.1:1");
        let client = LinearClient::new(&config, new_http());
        let body = serde_json::json!({"query": "{ viewer { id } }"});
        let err = client.execute_graphql(&body).await.unwrap_err();
        assert!(matches!(&err, SymphonyError::LinearApiRequest { .. }));
    }

    // ── transition_issue_state ─────────────────────────────────────

    #[tokio::test]
    async fn test_transition_issue_state_success() {
        let mut server = mockito::Server::new_async().await;

        // First call: resolve_state_id queries workflow states
        let resolve_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "workflowStates": {
                            "nodes": [
                                { "id": "state-todo", "name": "Todo" },
                                { "id": "state-in-progress", "name": "In Progress" },
                                { "id": "state-done", "name": "Done" }
                            ]
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        // Second call: update_issue_state performs the mutation
        let update_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": { "issueUpdate": { "success": true } }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let client = LinearClient::new(&config, new_http());
        let result = client
            .transition_issue_state("issue-123", "In Progress")
            .await;
        assert!(result.is_ok());
        resolve_mock.assert_async().await;
        update_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_transition_issue_state_unknown_state() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "workflowStates": {
                            "nodes": [
                                { "id": "state-todo", "name": "Todo" }
                            ]
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let client = LinearClient::new(&config, new_http());
        let result = client
            .transition_issue_state("issue-123", "Nonexistent")
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, SymphonyError::LinearUnknownPayload { .. }));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_transition_issue_state_case_insensitive() {
        let mut server = mockito::Server::new_async().await;

        let resolve_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "workflowStates": {
                            "nodes": [
                                { "id": "state-ip", "name": "In Progress" }
                            ]
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let update_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": { "issueUpdate": { "success": true } }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let client = LinearClient::new(&config, new_http());
        // Use different casing — should still match
        let result = client
            .transition_issue_state("issue-123", "in progress")
            .await;
        assert!(result.is_ok());
        resolve_mock.assert_async().await;
        update_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_update_issue_state_returns_false() {
        let mut server = mockito::Server::new_async().await;

        let resolve_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "workflowStates": {
                            "nodes": [
                                { "id": "state-ip", "name": "In Progress" }
                            ]
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let update_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": { "issueUpdate": { "success": false } }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let client = LinearClient::new(&config, new_http());
        let result = client
            .transition_issue_state("issue-123", "In Progress")
            .await;
        assert!(result.is_err());
        resolve_mock.assert_async().await;
        update_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_resolve_state_id_api_error() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("POST", "/")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let client = LinearClient::new(&config, new_http());
        let result = client
            .transition_issue_state("issue-123", "In Progress")
            .await;
        assert!(result.is_err());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_resolve_state_id_node_missing_id() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "workflowStates": {
                            "nodes": [
                                { "name": "In Progress" }
                            ]
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let client = LinearClient::new(&config, new_http());
        let result = client
            .transition_issue_state("issue-123", "In Progress")
            .await;
        // Node has name but no id — should fail with not found
        assert!(result.is_err());
        mock.assert_async().await;
    }

    // ── extract_matching_state_id ──────────────────────────────────

    #[test]
    fn test_extract_matching_state_id_match() {
        let node = serde_json::json!({ "id": "s1", "name": "In Progress" });
        assert_eq!(
            extract_matching_state_id(&node, "In Progress"),
            Some("s1".to_string())
        );
    }

    #[test]
    fn test_extract_matching_state_id_case_insensitive() {
        let node = serde_json::json!({ "id": "s1", "name": "in progress" });
        assert_eq!(
            extract_matching_state_id(&node, "In Progress"),
            Some("s1".to_string())
        );
    }

    #[test]
    fn test_extract_matching_state_id_no_match() {
        let node = serde_json::json!({ "id": "s1", "name": "Todo" });
        assert_eq!(extract_matching_state_id(&node, "In Progress"), None);
    }

    #[test]
    fn test_extract_matching_state_id_missing_name() {
        let node = serde_json::json!({ "id": "s1" });
        assert_eq!(extract_matching_state_id(&node, "In Progress"), None);
    }

    #[test]
    fn test_extract_matching_state_id_missing_id() {
        let node = serde_json::json!({ "name": "In Progress" });
        assert_eq!(extract_matching_state_id(&node, "In Progress"), None);
    }

    // ── execute_graphql: errors field is not an array ────────────────

    #[tokio::test]
    async fn test_execute_graphql_errors_not_array_is_ok() {
        let mut server = mockito::Server::new_async().await;
        // errors is a string, not an array — should be ignored
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"errors": "not an array", "data": {"ok": true}}"#)
            .create_async()
            .await;

        let config = mock_tracker_config(&server.url());
        let client = LinearClient::new(&config, new_http());
        let body = serde_json::json!({"query": "{ ok }"});
        let result = client.execute_graphql(&body).await;
        assert!(result.is_ok());
        mock.assert_async().await;
    }
}
