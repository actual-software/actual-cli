//! The decision routes `actual adr` calls, under `/v1/cli/adrs`.
//!
//! Reads need the `adr:query` scope and writes `adr:review`; both are part of
//! a default `actual login`. A create isn't idempotent, so it is sent exactly
//! once and never goes through [`crate::api::with_retry`].

use reqwest::StatusCode;

use super::ActualApiClient;
use crate::api::adr_types::{
    AdrErrorBody, CreateDecisionRequest, CreatedDecision, Decision, DecisionAction, DecisionList,
    DecisionShow, DecisionState, LifecycleAction, UpdateDecisionRequest,
};
use crate::error::{sanitize_server_text, ActualError};

/// The largest page `list` asks for, the route's own limit.
pub const MAX_PAGE: u32 = 200;

/// The remedy printed when a create's outcome is unknown.
const LOOK_BEFORE_RETRY: &str = "actual adr list  (look for the decision in this repository's \
     list before you run create again)";

impl ActualApiClient {
    /// One page of the organization's decisions (`GET /v1/cli/adrs`), for one
    /// repository when `repo_unique_id` is given, and one list of the web
    /// app's when `state` is.
    pub async fn list_decisions(
        &self,
        org_id: &str,
        repo_unique_id: Option<&str>,
        state: Option<DecisionState>,
        limit: u32,
        offset: u64,
    ) -> Result<DecisionList, ActualError> {
        let mut query = vec![("org_id", org_id.to_string())];
        if let Some(repo) = repo_unique_id {
            query.push(("repo_unique_id", repo.to_string()));
        }
        if let Some(state) = state {
            query.push(("state", state.as_str().to_string()));
        }
        query.push(("limit", limit.to_string()));
        query.push(("offset", offset.to_string()));
        let request = self.client.get(self.adr_url(&[])).query(&query);
        let response = self
            .authed(request)
            .send()
            .await
            .map_err(|e| ActualError::ApiError(e.to_string()))?;
        read_adr_response(response).await
    }

    /// Every decision of one repository (or of the whole organization),
    /// page by page. Stops early if a page doesn't move the offset forward.
    pub async fn list_all_decisions(
        &self,
        org_id: &str,
        repo_unique_id: Option<&str>,
    ) -> Result<Vec<Decision>, ActualError> {
        let mut decisions = Vec::new();
        let mut offset = 0;
        loop {
            let page = self
                .list_decisions(org_id, repo_unique_id, None, MAX_PAGE, offset)
                .await?;
            decisions.extend(page.decisions);
            match page.next_offset {
                Some(next) if next > offset => offset = next,
                _ => return Ok(decisions),
            }
        }
    }

    /// One decision (`GET /v1/cli/adrs/:adr_id`). A grouped decision is
    /// found by its own id or any member row's.
    pub async fn show_decision(
        &self,
        org_id: &str,
        adr_id: &str,
        repo_unique_id: Option<&str>,
    ) -> Result<Decision, ActualError> {
        let mut query = vec![("org_id", org_id)];
        if let Some(repo) = repo_unique_id {
            query.push(("repo_unique_id", repo));
        }
        let request = self.client.get(self.adr_url(&[adr_id])).query(&query);
        let response = self
            .authed(request)
            .send()
            .await
            .map_err(|e| ActualError::ApiError(e.to_string()))?;
        read_adr_response::<DecisionShow>(response)
            .await
            .map(|show| show.decision)
    }

    /// Save a new decision (`POST /v1/cli/adrs`).
    ///
    /// Sent once. A request that never connected saved nothing and says so.
    /// Once it has been sent, a timeout or an unreadable answer may still
    /// have saved the decision, so the error is
    /// [`ActualError::AdrCreateUnconfirmed`], whose hint says how to look
    /// before trying again.
    pub async fn create_decision(
        &self,
        request: &CreateDecisionRequest,
    ) -> Result<CreatedDecision, ActualError> {
        let post = self.client.post(self.adr_url(&[])).json(request);
        let response = match self.authed(post).send().await {
            Ok(response) => response,
            Err(e) if e.is_connect() => {
                return Err(ActualError::ApiError(format!(
                    "Could not reach Actual AI, so the decision was not saved: {e}"
                )))
            }
            Err(e) => {
                return Err(unconfirmed(format!(
                    "The create request was sent, but no answer came back ({e}). \
                     The decision may have been saved."
                )))
            }
        };
        let status = response.status();
        if !status.is_success() {
            return Err(adr_error(status, response).await);
        }
        response.json::<CreatedDecision>().await.map_err(|e| {
            unconfirmed(format!(
                "Actual AI accepted the create, but its answer couldn't be read ({e}), \
                 so the new decision's id is unknown."
            ))
        })
    }

    /// Change a decision's title, context, policies, levels or scope
    /// (`POST /v1/cli/adrs/:adr_id/update`).
    pub async fn update_decision(
        &self,
        adr_id: &str,
        request: &UpdateDecisionRequest,
    ) -> Result<DecisionAction, ActualError> {
        self.post_decision(&[adr_id, "update"], request).await
    }

    /// Publish, unpublish, archive, restore or accept a decision.
    pub async fn decision_lifecycle(
        &self,
        adr_id: &str,
        action: LifecycleAction,
        org_id: &str,
    ) -> Result<DecisionAction, ActualError> {
        let body = serde_json::json!({ "org_id": org_id });
        self.post_decision(&[adr_id, action.path_segment()], &body)
            .await
    }

    /// Reject a suggested decision with a reason
    /// (`POST /v1/cli/adrs/:adr_id/reject`).
    pub async fn reject_decision(
        &self,
        adr_id: &str,
        org_id: &str,
        reason: &str,
    ) -> Result<DecisionAction, ActualError> {
        let body = serde_json::json!({ "org_id": org_id, "reason": reason });
        self.post_decision(&[adr_id, "reject"], &body).await
    }

    async fn post_decision<B: serde::Serialize + ?Sized>(
        &self,
        segments: &[&str],
        body: &B,
    ) -> Result<DecisionAction, ActualError> {
        let request = self.client.post(self.adr_url(segments)).json(body);
        let response = self
            .authed(request)
            .send()
            .await
            .map_err(|e| ActualError::ApiError(e.to_string()))?;
        read_adr_response(response).await
    }

    /// `<base>/v1/cli/adrs` followed by `segments`, each encoded as one path
    /// segment.
    fn adr_url(&self, segments: &[&str]) -> String {
        segments
            .iter()
            .fold(format!("{}/v1/cli/adrs", self.base_url), |url, segment| {
                format!("{url}/{}", path_segment(segment))
            })
    }
}

/// `text` as one URL path segment: every byte outside RFC 3986's unreserved
/// characters is percent-encoded, so an id can't add a path segment or a
/// query.
fn path_segment(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn unconfirmed(message: String) -> ActualError {
    ActualError::AdrCreateUnconfirmed {
        message,
        hint: LOOK_BEFORE_RETRY.to_string(),
    }
}

/// The body of a successful response, or the error a failed one maps to.
async fn read_adr_response<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, ActualError> {
    let status = response.status();
    if !status.is_success() {
        return Err(adr_error(status, response).await);
    }
    response
        .json::<T>()
        .await
        .map_err(|e| ActualError::ApiError(e.to_string()))
}

async fn adr_error(status: StatusCode, response: reqwest::Response) -> ActualError {
    let body = response.json::<AdrErrorBody>().await.unwrap_or_default();
    adr_error_from_body(status, body)
}

/// Map a decision route's refusal to the error the CLI reports: 401 is a
/// sign-in problem, 403 a permission problem with its own remedy, a 400 lists
/// each reason the decision or request was refused, and anything else keeps
/// the server's code and message.
fn adr_error_from_body(status: StatusCode, body: AdrErrorBody) -> ActualError {
    let message = body
        .message
        .as_deref()
        .map(sanitize_server_text)
        .filter(|m| !m.is_empty());
    match status {
        StatusCode::UNAUTHORIZED => ActualError::NotLoggedIn,
        StatusCode::FORBIDDEN => access_denied(message.as_deref()),
        StatusCode::SERVICE_UNAVAILABLE => ActualError::ServiceUnavailable,
        StatusCode::BAD_REQUEST => {
            let mut reasons: Vec<String> = body
                .errors
                .iter()
                .map(|e| sanitize_server_text(e))
                .collect();
            if reasons.is_empty() {
                reasons = body
                    .schema_issues()
                    .iter()
                    .map(|issue| sanitize_server_text(issue))
                    .collect();
            }
            if reasons.is_empty() {
                reasons.extend(message);
            }
            if reasons.is_empty() {
                ActualError::ApiError(format!("HTTP {status}"))
            } else {
                ActualError::AdrInvalid { reasons }
            }
        }
        _ => match (body.error, message) {
            (Some(code), Some(message)) => ActualError::ApiResponseError {
                code: sanitize_server_text(&code),
                message,
            },
            (None, Some(message)) => ActualError::ApiError(format!("HTTP {status}: {message}")),
            (_, None) => ActualError::ApiError(format!("HTTP {status}")),
        },
    }
}

/// A 403 with the remedy for its cause: a sign-in without the decision
/// scopes, a sign-in for another organization, or a member whose role can't
/// change decisions.
fn access_denied(message: Option<&str>) -> ActualError {
    let reason = message.unwrap_or("access denied");
    let hint = if reason.contains("scope") {
        "actual login  (sign in again so this session can read and change decisions)"
    } else if reason.contains("requested organization") {
        "actual login  (for the organization these decisions belong to)"
    } else if reason.contains("user access required") {
        "Ask an admin of your organization for a role that can change decisions"
    } else {
        "actual login"
    };
    ActualError::AdrAccessDenied {
        message: format!("Actual AI refused the request (HTTP 403): {reason}"),
        hint: hint.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use mockito::Matcher;

    use super::*;
    use crate::api::adr_types::{PolicyEdit, SaveStatus};
    use crate::rules::types::RuleLevel;

    const ORG: &str = "11111111-1111-4111-8111-111111111111";
    const REPO: &str = "22222222-2222-4222-8222-222222222222";
    const ADR: &str = "33333333-3333-4333-8333-333333333333";

    fn decision_json(id: &str, title: &str) -> String {
        format!(
            r#"{{"id":"{id}","kind":"single","repo_unique_id":"{REPO}","title":"{title}",
                "state":"active","status":"accepted","date":"2026-11-02T09:00:00.000Z",
                "context":"Why.","policies":[{{"id":"{id}:rule-1","level":"MUST","statement":"Do it."}}],
                "scope":{{"root":".","paths":[]}},"category":null,"base_adr_id":null}}"#
        )
    }

    fn client(server: &mockito::Server) -> ActualApiClient {
        ActualApiClient::new(&server.url())
            .unwrap()
            .with_bearer("tok")
    }

    fn create_request() -> CreateDecisionRequest {
        CreateDecisionRequest {
            org_id: ORG.into(),
            repo_unique_id: REPO.into(),
            status: SaveStatus::Accepted,
            markdown: "# Use Postgres\n".into(),
        }
    }

    #[tokio::test]
    async fn list_sends_every_filter() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/v1/cli/adrs")
            .match_header("authorization", "Bearer tok")
            .match_query(Matcher::Exact(format!(
                "org_id={ORG}&repo_unique_id={REPO}&state=draft&limit=20&offset=40"
            )))
            .with_status(200)
            .with_body(format!(
                r#"{{"decisions":[{}],"total":41,"next_offset":null}}"#,
                decision_json(ADR, "Use Postgres")
            ))
            .create_async()
            .await;
        let list = client(&server)
            .list_decisions(ORG, Some(REPO), Some(DecisionState::Draft), 20, 40)
            .await
            .unwrap();
        assert_eq!(list.total, 41);
        assert_eq!(list.decisions[0].title, "Use Postgres");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn list_without_filters_sends_only_the_org_and_paging() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/v1/cli/adrs")
            .match_query(Matcher::Exact(format!("org_id={ORG}&limit=50&offset=0")))
            .with_status(200)
            .with_body(r#"{"decisions":[],"total":0,"next_offset":null}"#)
            .create_async()
            .await;
        let list = client(&server)
            .list_decisions(ORG, None, None, 50, 0)
            .await
            .unwrap();
        assert!(list.decisions.is_empty());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn list_all_follows_next_offset() {
        let mut server = mockito::Server::new_async().await;
        let first = server
            .mock("GET", "/v1/cli/adrs")
            .match_query(Matcher::UrlEncoded("offset".into(), "0".into()))
            .with_status(200)
            .with_body(format!(
                r#"{{"decisions":[{}],"total":2,"next_offset":1}}"#,
                decision_json("d1", "One")
            ))
            .create_async()
            .await;
        let second = server
            .mock("GET", "/v1/cli/adrs")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("offset".into(), "1".into()),
                Matcher::UrlEncoded("limit".into(), "200".into()),
                Matcher::UrlEncoded("repo_unique_id".into(), REPO.into()),
            ]))
            .with_status(200)
            .with_body(format!(
                r#"{{"decisions":[{}],"total":2,"next_offset":null}}"#,
                decision_json("d2", "Two")
            ))
            .create_async()
            .await;
        let all = client(&server)
            .list_all_decisions(ORG, Some(REPO))
            .await
            .unwrap();
        let ids: Vec<&str> = all.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["d1", "d2"]);
        first.assert_async().await;
        second.assert_async().await;
    }

    #[tokio::test]
    async fn list_all_stops_when_the_offset_does_not_move() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/v1/cli/adrs")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(format!(
                r#"{{"decisions":[{}],"total":9,"next_offset":0}}"#,
                decision_json("d1", "One")
            ))
            .expect(1)
            .create_async()
            .await;
        let all = client(&server).list_all_decisions(ORG, None).await.unwrap();
        assert_eq!(all.len(), 1);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn list_all_passes_a_page_error_through() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", "/v1/cli/adrs")
            .match_query(Matcher::Any)
            .with_status(401)
            .create_async()
            .await;
        let err = client(&server)
            .list_all_decisions(ORG, None)
            .await
            .unwrap_err();
        assert!(matches!(err, ActualError::NotLoggedIn));
    }

    #[tokio::test]
    async fn show_encodes_the_id_and_sends_the_repository() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/v1/cli/adrs/a%2Fb%3Fc")
            .match_query(Matcher::Exact(format!(
                "org_id={ORG}&repo_unique_id={REPO}"
            )))
            .with_status(200)
            .with_body(format!(r#"{{"decision":{}}}"#, decision_json(ADR, "T")))
            .create_async()
            .await;
        let decision = client(&server)
            .show_decision(ORG, "a/b?c", Some(REPO))
            .await
            .unwrap();
        assert_eq!(decision.id, ADR);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn show_without_a_repository_and_a_missing_decision() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", format!("/v1/cli/adrs/{ADR}").as_str())
            .match_query(Matcher::Exact(format!("org_id={ORG}")))
            .with_status(404)
            .with_body(
                r#"{"error":"not_found","message":"decision not found in this organization"}"#,
            )
            .create_async()
            .await;
        let err = client(&server)
            .show_decision(ORG, ADR, None)
            .await
            .unwrap_err();
        match err {
            ActualError::ApiResponseError { code, message } => {
                assert_eq!(code, "not_found");
                assert_eq!(message, "decision not found in this organization");
            }
            other => panic!("expected ApiResponseError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_sends_the_body_once() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/cli/adrs")
            .match_header("authorization", "Bearer tok")
            .match_body(Matcher::Json(serde_json::json!({
                "org_id": ORG,
                "repo_unique_id": REPO,
                "status": "accepted",
                "markdown": "# Use Postgres\n",
            })))
            .with_status(200)
            .with_body(format!(
                r#"{{"adr_id":"{ADR}","status":"accepted","title":"Use Postgres","repo_unique_id":"{REPO}"}}"#
            ))
            .expect(1)
            .create_async()
            .await;
        let created = client(&server)
            .create_decision(&create_request())
            .await
            .unwrap();
        assert_eq!(created.adr_id, ADR);
        assert_eq!(created.title, "Use Postgres");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn create_is_not_retried_after_a_server_error() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/cli/adrs")
            .with_status(500)
            .with_body(r#"{"error":"internal_error","message":"failed to create the decision"}"#)
            .expect(1)
            .create_async()
            .await;
        let err = client(&server)
            .create_decision(&create_request())
            .await
            .unwrap_err();
        assert!(matches!(err, ActualError::ApiResponseError { .. }));
        mock.assert_async().await;
    }

    /// The sent-once guarantee: a server that reads the create and never
    /// answers receives exactly one request, and the error says the save
    /// may have landed.
    #[tokio::test]
    async fn create_that_times_out_is_sent_once_and_reported_unconfirmed() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&accepted);
        let server = tokio::spawn(async move {
            let mut held = Vec::new();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = vec![0u8; 4096];
                let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;
                held.push(socket);
            }
        });
        let client = ActualApiClient::new_with_timeout(
            &format!("http://127.0.0.1:{port}"),
            Duration::from_millis(300),
            Duration::from_secs(5),
        )
        .unwrap()
        .with_bearer("tok");
        let err = client.create_decision(&create_request()).await.unwrap_err();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
        match &err {
            ActualError::AdrCreateUnconfirmed { message, hint } => {
                assert!(message.contains("may have been saved"), "got: {message}");
                assert!(hint.contains("actual adr list"), "got: {hint}");
            }
            other => panic!("expected AdrCreateUnconfirmed, got {other:?}"),
        }
        server.abort();
    }

    #[tokio::test]
    async fn create_that_never_connects_saved_nothing() {
        // Bind and drop a listener so the port is closed.
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let client = ActualApiClient::new(&format!("http://127.0.0.1:{port}"))
            .unwrap()
            .with_bearer("tok");
        let err = client.create_decision(&create_request()).await.unwrap_err();
        match err {
            ActualError::ApiError(message) => {
                assert!(message.contains("was not saved"), "got: {message}")
            }
            other => panic!("expected ApiError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_whose_answer_is_unreadable_is_unconfirmed() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/cli/adrs")
            .with_status(200)
            .with_body("not json")
            .create_async()
            .await;
        let err = client(&server)
            .create_decision(&create_request())
            .await
            .unwrap_err();
        match err {
            ActualError::AdrCreateUnconfirmed { message, .. } => {
                assert!(message.contains("id is unknown"), "got: {message}")
            }
            other => panic!("expected AdrCreateUnconfirmed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_posts_its_body() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", format!("/v1/cli/adrs/{ADR}/update").as_str())
            .match_body(Matcher::Json(serde_json::json!({
                "org_id": ORG,
                "policies": [{"level": "MUST_NOT", "statement": "Skip the check."}],
            })))
            .with_status(200)
            .with_body(format!(
                r#"{{"success":true,"adr_id":"{ADR}","sync":"requested"}}"#
            ))
            .create_async()
            .await;
        let request = UpdateDecisionRequest {
            org_id: ORG.into(),
            policies: Some(vec![PolicyEdit {
                id: None,
                level: RuleLevel::MustNot,
                statement: "Skip the check.".into(),
            }]),
            ..Default::default()
        };
        let action = client(&server)
            .update_decision(ADR, &request)
            .await
            .unwrap();
        assert!(action.sync_requested());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn lifecycle_actions_post_the_org() {
        let mut server = mockito::Server::new_async().await;
        let actions = [
            LifecycleAction::Publish,
            LifecycleAction::Unpublish,
            LifecycleAction::Archive,
            LifecycleAction::Restore,
            LifecycleAction::Accept,
        ];
        for action in actions {
            let mock = server
                .mock(
                    "POST",
                    format!("/v1/cli/adrs/{ADR}/{}", action.path_segment()).as_str(),
                )
                .match_body(Matcher::Json(serde_json::json!({"org_id": ORG})))
                .with_status(200)
                .with_body(format!(
                    r#"{{"success":true,"adr_id":"{ADR}","sync":"not_needed"}}"#
                ))
                .create_async()
                .await;
            let result = client(&server)
                .decision_lifecycle(ADR, action, ORG)
                .await
                .unwrap();
            assert_eq!(result.adr_id, ADR);
            mock.assert_async().await;
        }
    }

    #[tokio::test]
    async fn reject_posts_the_reason() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", format!("/v1/cli/adrs/{ADR}/reject").as_str())
            .match_body(Matcher::Json(
                serde_json::json!({"org_id": ORG, "reason": "We use MySQL."}),
            ))
            .with_status(200)
            .with_body(format!(
                r#"{{"success":true,"adr_id":"{ADR}","sync":"not_needed","reason_saved":false}}"#
            ))
            .create_async()
            .await;
        let result = client(&server)
            .reject_decision(ADR, ORG, "We use MySQL.")
            .await
            .unwrap();
        assert_eq!(result.reason_saved, Some(false));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn unreachable_reads_and_writes_are_api_errors() {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let client = ActualApiClient::new(&format!("http://127.0.0.1:{port}"))
            .unwrap()
            .with_bearer("tok");
        assert!(matches!(
            client.list_decisions(ORG, None, None, 1, 0).await,
            Err(ActualError::ApiError(_))
        ));
        assert!(matches!(
            client.show_decision(ORG, ADR, None).await,
            Err(ActualError::ApiError(_))
        ));
        assert!(matches!(
            client
                .decision_lifecycle(ADR, LifecycleAction::Publish, ORG)
                .await,
            Err(ActualError::ApiError(_))
        ));
    }

    #[tokio::test]
    async fn an_unreadable_success_body_is_an_api_error() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", format!("/v1/cli/adrs/{ADR}/publish").as_str())
            .with_status(200)
            .with_body("[]")
            .create_async()
            .await;
        let err = client(&server)
            .decision_lifecycle(ADR, LifecycleAction::Publish, ORG)
            .await
            .unwrap_err();
        assert!(matches!(err, ActualError::ApiError(_)));
    }

    #[tokio::test]
    async fn refusals_map_to_their_errors() {
        let mut server = mockito::Server::new_async().await;
        let cases: Vec<(usize, &str)> = vec![
            (
                401,
                r#"{"error":"Unauthorized","message":"Invalid or expired token."}"#,
            ),
            (
                400,
                r#"{"error":"bad_request","message":"Add a title. Add context.","errors":["Add a title.","Add context."]}"#,
            ),
        ];
        for (status, body) in cases {
            server
                .mock("POST", format!("/v1/cli/adrs/{ADR}/publish").as_str())
                .with_status(status)
                .with_body(body)
                .create_async()
                .await;
            let err = client(&server)
                .decision_lifecycle(ADR, LifecycleAction::Publish, ORG)
                .await
                .unwrap_err();
            match status {
                401 => assert!(matches!(err, ActualError::NotLoggedIn)),
                _ => match err {
                    ActualError::AdrInvalid { reasons } => {
                        assert_eq!(reasons, ["Add a title.", "Add context."])
                    }
                    other => panic!("expected AdrInvalid, got {other:?}"),
                },
            }
            server.reset();
        }
    }

    fn body(json: &str) -> AdrErrorBody {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn forbidden_names_the_remedy_for_its_cause() {
        let cases = [
            (
                r#"{"error":"forbidden","message":"Insufficient scope for this operation."}"#,
                "sign in again",
            ),
            (
                r#"{"error":"forbidden","message":"OAuth token is not authorized for the requested organization"}"#,
                "for the organization",
            ),
            (
                r#"{"error":"forbidden","message":"organization user access required"}"#,
                "Ask an admin",
            ),
            (
                r#"{"error":"forbidden","message":"decision routes need a member credential"}"#,
                "actual login",
            ),
        ];
        for (json, expected) in cases {
            let err = adr_error_from_body(StatusCode::FORBIDDEN, body(json));
            assert!(
                matches!(&err, ActualError::AdrAccessDenied { message, hint }
                    if message.starts_with("Actual AI refused the request (HTTP 403): ")
                        && hint.contains(expected)),
                "{err:?} for {json}"
            );
        }
        let err = adr_error_from_body(StatusCode::FORBIDDEN, AdrErrorBody::default());
        assert!(
            matches!(&err, ActualError::AdrAccessDenied { message, hint }
                if message.ends_with("access denied") && hint == "actual login"),
            "{err:?}"
        );
    }

    #[test]
    fn bad_requests_list_every_reason_they_carry() {
        let schema = adr_error_from_body(
            StatusCode::BAD_REQUEST,
            body(
                r#"{"bodyErrors":{"issues":[{"path":["reason"],"message":"String must contain at least 1 character(s)"}],"name":"ZodError"},"queryParameterErrors":null,"pathParameterErrors":null,"headerErrors":null}"#,
            ),
        );
        assert!(
            matches!(&schema, ActualError::AdrInvalid { reasons }
                if *reasons == ["reason: String must contain at least 1 character(s)"]),
            "{schema:?}"
        );
        let writer = adr_error_from_body(
            StatusCode::BAD_REQUEST,
            body(r#"{"error":"invalid_request","message":"Policy statements cannot be empty"}"#),
        );
        assert!(
            matches!(&writer, ActualError::AdrInvalid { reasons }
                if *reasons == ["Policy statements cannot be empty"]),
            "{writer:?}"
        );
        let empty = adr_error_from_body(StatusCode::BAD_REQUEST, AdrErrorBody::default());
        assert!(matches!(empty, ActualError::ApiError(m) if m == "HTTP 400 Bad Request"));
    }

    #[test]
    fn other_refusals_keep_the_server_code_and_message() {
        let err = adr_error_from_body(
            StatusCode::CONFLICT,
            body(
                r#"{"error":"conflict","message":"Only a draft can be published; this decision is active"}"#,
            ),
        );
        assert!(
            matches!(&err, ActualError::ApiResponseError { code, message }
                if code == "conflict" && message.starts_with("Only a draft")),
            "{err:?}"
        );
        assert!(matches!(
            adr_error_from_body(StatusCode::SERVICE_UNAVAILABLE, AdrErrorBody::default()),
            ActualError::ServiceUnavailable
        ));
        assert!(matches!(
            adr_error_from_body(StatusCode::BAD_GATEWAY, body(r#"{"message":"upstream \u001b[31mdown"}"#)),
            ActualError::ApiError(m) if m == "HTTP 502 Bad Gateway: upstream [31mdown"
        ));
        assert!(matches!(
            adr_error_from_body(StatusCode::INTERNAL_SERVER_ERROR, body(r#"{"error":"internal_error","message":""}"#)),
            ActualError::ApiError(m) if m == "HTTP 500 Internal Server Error"
        ));
    }

    #[test]
    fn path_segments_keep_ids_whole() {
        assert_eq!(path_segment(ADR), ADR);
        assert_eq!(path_segment("a/b c?d#e%"), "a%2Fb%20c%3Fd%23e%25");
        assert_eq!(path_segment("é"), "%C3%A9");
        assert_eq!(path_segment("x-y_z.~"), "x-y_z.~");
    }
}
