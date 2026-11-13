//! The decision routes `actual adr` calls: the ones the web app calls, under
//! `/v1/gcp/product/`.
//!
//! Reads need the `adr:query` scope and writes `adr:review`; both are part of
//! a default `actual login`. No write goes through [`crate::api::with_retry`]:
//! a create or import isn't idempotent, and every other change is checked
//! against the decision's state before it is sent.

use reqwest::StatusCode;

use super::ActualApiClient;
use crate::api::adr_types::{
    AdrErrorBody, CreateRequest, CreateResponse, DecisionWrite, DecisionsResponse, ImportRequest,
    ImportResponse,
};
use crate::error::{sanitize_server_text, ActualError};

/// The remedy printed when a create's outcome is unknown.
const LOOK_BEFORE_RETRY: &str = "actual adr list  (look for the decision in this repository's \
     list before you run create again)";

impl ActualApiClient {
    /// The organization's decisions as the web app reads them
    /// (`GET /v1/gcp/product/decisions`), for one repository when
    /// `repository_id` is given. The read has no paging, so it returns every
    /// decision, and leaves out the per-repository counts `actual adr` never
    /// shows.
    pub async fn read_decisions(
        &self,
        org_id: &str,
        repository_id: Option<&str>,
    ) -> Result<DecisionsResponse, ActualError> {
        let mut query = vec![("organization_id", org_id)];
        if let Some(repository) = repository_id {
            query.push(("repository_id", repository));
        }
        query.push(("include_summaries", "false"));
        let request = self
            .client
            .get(self.decision_url("decisions"))
            .query(&query);
        let response = self
            .authed(request)
            .send()
            .await
            .map_err(|e| ActualError::ApiError(e.to_string()))?;
        read_adr_response(response).await
    }

    /// Save a new decision from fields, as the web app's New Decision does
    /// (`POST adr-create`), and return its id.
    pub async fn create_decision(&self, request: &CreateRequest) -> Result<String, ActualError> {
        let created: CreateResponse = self.save_once("adr-create", request).await?;
        Ok(created.data.id)
    }

    /// Save a decision's Context File markdown as a new draft, as the web
    /// app's Import does (`POST adr-import`), and return its id.
    pub async fn import_decision(&self, request: &ImportRequest) -> Result<String, ActualError> {
        let imported: ImportResponse = self.save_once("adr-import", request).await?;
        Ok(imported.adr_id)
    }

    /// Send a write that saves a new decision, exactly once. A request that
    /// never connected saved nothing and says so. Once it has been sent, a
    /// timeout or an unreadable answer may still have saved the decision, so
    /// the error is [`ActualError::AdrCreateUnconfirmed`], whose hint says how
    /// to look before trying again.
    async fn save_once<B: serde::Serialize + ?Sized, T: serde::de::DeserializeOwned>(
        &self,
        route: &str,
        body: &B,
    ) -> Result<T, ActualError> {
        let post = self.client.post(self.decision_url(route)).json(body);
        let response = match self.authed(post).send().await {
            Ok(response) => response,
            Err(e) if e.is_connect() => {
                return Err(ActualError::ApiError(format!(
                    "Could not reach Actual AI, so the decision was not saved: {e}"
                )))
            }
            Err(e) => {
                return Err(unconfirmed(format!(
                    "The save request was sent, but no answer came back ({e}). \
                     The decision may have been saved."
                )))
            }
        };
        let status = response.status();
        if !status.is_success() {
            return Err(adr_error(status, response).await);
        }
        response.json::<T>().await.map_err(|e| {
            unconfirmed(format!(
                "Actual AI accepted the save, but its answer couldn't be read ({e}), \
                 so the new decision's id is unknown."
            ))
        })
    }

    /// Send one change to a decision. The answer carries nothing `actual adr`
    /// reads, so only its status counts.
    pub async fn write_decision(&self, write: &DecisionWrite) -> Result<(), ActualError> {
        let request = self
            .client
            .post(self.decision_url(write.route))
            .json(&write.body);
        let response = self
            .authed(request)
            .send()
            .await
            .map_err(|e| ActualError::ApiError(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(adr_error(status, response).await);
        }
        Ok(())
    }

    /// `<base>/v1/gcp/product/<route>`.
    fn decision_url(&self, route: &str) -> String {
        format!("{}/v1/gcp/product/{route}", self.base_url)
    }
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
/// sign-in problem, 403 a permission problem with its own remedy, a 400 gives
/// the reasons the decision or request was refused, and anything else keeps
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
                .schema_issues()
                .iter()
                .map(|issue| sanitize_server_text(issue))
                .collect();
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
    use serde_json::json;

    use super::*;
    use crate::api::adr_types::NewDecision;
    use crate::rules::types::RuleLevel;

    const ORG: &str = "11111111-1111-4111-8111-111111111111";
    const REPO: &str = "22222222-2222-4222-8222-222222222222";
    const ADR: &str = "33333333-3333-4333-8333-333333333333";

    fn client(server: &mockito::Server) -> ActualApiClient {
        ActualApiClient::new(&server.url())
            .unwrap()
            .with_bearer("tok")
    }

    fn import_request() -> ImportRequest {
        ImportRequest {
            organization_id: ORG.into(),
            repo_unique_id: REPO.into(),
            markdown: "# Use Postgres\n".into(),
        }
    }

    fn create_request() -> CreateRequest {
        CreateRequest {
            organization_id: ORG.into(),
            repo_unique_id: REPO.into(),
            adr: NewDecision {
                title: "Use Postgres".into(),
                context: None,
                policies: vec!["Store data in Postgres.".into()],
                policy_levels: vec![RuleLevel::Must],
                status: None,
            },
        }
    }

    #[tokio::test]
    async fn read_names_the_org_the_repository_and_no_summaries() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/v1/gcp/product/decisions")
            .match_header("authorization", "Bearer tok")
            .match_query(Matcher::Exact(format!(
                "organization_id={ORG}&repository_id={REPO}&include_summaries=false"
            )))
            .with_status(200)
            .with_body(
                r#"{"adrs":[{"id":"d1","date":"","status":"accepted"}],"suggestions":[],"version":1,"repository_summaries":[]}"#,
            )
            .create_async()
            .await;
        let read = client(&server)
            .read_decisions(ORG, Some(REPO))
            .await
            .unwrap();
        assert_eq!(read.adrs[0].id, "d1");
        mock.assert_async().await;

        let org_wide = server
            .mock("GET", "/v1/gcp/product/decisions")
            .match_query(Matcher::Exact(format!(
                "organization_id={ORG}&include_summaries=false"
            )))
            .with_status(200)
            .with_body(r#"{"adrs":[],"suggestions":[]}"#)
            .create_async()
            .await;
        let read = client(&server).read_decisions(ORG, None).await.unwrap();
        assert!(read.adrs.is_empty());
        org_wide.assert_async().await;
    }

    #[tokio::test]
    async fn create_and_import_send_their_bodies_once() {
        let mut server = mockito::Server::new_async().await;
        let create = server
            .mock("POST", "/v1/gcp/product/adr-create")
            .match_header("authorization", "Bearer tok")
            .match_body(Matcher::Json(json!({
                "organization_id": ORG,
                "repo_unique_id": REPO,
                "adr": {
                    "title": "Use Postgres",
                    "policies": ["Store data in Postgres."],
                    "policyLevels": ["MUST"]
                }
            })))
            .with_status(200)
            .with_body(format!(
                r#"{{"success":true,"data":{{"id":"{ADR}","date":"d","title":"Use Postgres","status":"created-from-user"}}}}"#
            ))
            .expect(1)
            .create_async()
            .await;
        let id = client(&server)
            .create_decision(&create_request())
            .await
            .unwrap();
        assert_eq!(id, ADR);
        create.assert_async().await;

        let import = server
            .mock("POST", "/v1/gcp/product/adr-import")
            .match_body(Matcher::Json(json!({
                "organization_id": ORG,
                "repo_unique_id": REPO,
                "markdown": "# Use Postgres\n"
            })))
            .with_status(200)
            .with_body(format!(r#"{{"success":true,"adr_id":"{ADR}"}}"#))
            .expect(1)
            .create_async()
            .await;
        let id = client(&server)
            .import_decision(&import_request())
            .await
            .unwrap();
        assert_eq!(id, ADR);
        import.assert_async().await;
    }

    #[tokio::test]
    async fn a_save_is_not_retried_after_a_server_error() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/gcp/product/adr-import")
            .with_status(500)
            .with_body(r#"{"error":"internal_error","message":"failed to import ADR"}"#)
            .expect(1)
            .create_async()
            .await;
        let err = client(&server)
            .import_decision(&import_request())
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ActualError::ApiResponseError { code, .. } if code == "internal_error"),
            "{err:?}"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn a_save_that_times_out_is_sent_once_and_reported_unconfirmed() {
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
        let err = client.import_decision(&import_request()).await.unwrap_err();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
        assert!(
            matches!(&err, ActualError::AdrCreateUnconfirmed { message, hint }
                if message.contains("may have been saved") && hint.starts_with("actual adr list")),
            "{err:?}"
        );
        assert_eq!(err.exit_code(), 3);
        server.abort();
    }

    #[tokio::test]
    async fn a_save_that_never_connects_saved_nothing() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let client = ActualApiClient::new(&format!("http://{addr}"))
            .unwrap()
            .with_bearer("tok");
        let err = client.create_decision(&create_request()).await.unwrap_err();
        assert!(
            matches!(&err, ActualError::ApiError(m) if m.contains("was not saved")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_save_whose_answer_is_unreadable_is_unconfirmed() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/gcp/product/adr-import")
            .with_status(200)
            .with_body(r#"{"success":true"#)
            .create_async()
            .await;
        let err = client(&server)
            .import_decision(&import_request())
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ActualError::AdrCreateUnconfirmed { message, .. }
                if message.contains("id is unknown")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_write_posts_its_body_to_its_route() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/gcp/product/adr-publish")
            .match_header("authorization", "Bearer tok")
            .match_body(Matcher::Json(
                json!({"organization_id": ORG, "adr_id": ADR}),
            ))
            .with_status(200)
            .with_body(r#"{"success":true}"#)
            .expect(1)
            .create_async()
            .await;
        let write = DecisionWrite {
            route: "adr-publish",
            body: json!({"organization_id": ORG, "adr_id": ADR}),
        };
        client(&server).write_decision(&write).await.unwrap();
        mock.assert_async().await;

        server
            .mock("POST", "/v1/gcp/product/adr-archive")
            .with_status(409)
            .with_body(r#"{"error":"conflict","message":"Invalid ADR transition from archived via adr.archived"}"#)
            .create_async()
            .await;
        let refused = DecisionWrite {
            route: "adr-archive",
            body: json!({}),
        };
        let err = client(&server).write_decision(&refused).await.unwrap_err();
        assert!(
            matches!(&err, ActualError::ApiResponseError { code, message }
                if code == "conflict" && message.starts_with("Invalid ADR transition")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn unreachable_reads_and_writes_are_api_errors() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let client = ActualApiClient::new(&format!("http://{addr}"))
            .unwrap()
            .with_bearer("tok");
        let read = client.read_decisions(ORG, None).await.unwrap_err();
        assert!(matches!(read, ActualError::ApiError(_)), "{read:?}");
        let write = DecisionWrite {
            route: "adr-publish",
            body: json!({}),
        };
        let write = client.write_decision(&write).await.unwrap_err();
        assert!(matches!(write, ActualError::ApiError(_)), "{write:?}");
    }

    #[tokio::test]
    async fn an_unreadable_read_and_a_refused_read_are_errors() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", "/v1/gcp/product/decisions")
            .match_query(Matcher::UrlEncoded("organization_id".into(), ORG.into()))
            .with_status(200)
            .with_body("not json")
            .create_async()
            .await;
        let err = client(&server).read_decisions(ORG, None).await.unwrap_err();
        assert!(matches!(err, ActualError::ApiError(_)), "{err:?}");

        server
            .mock("GET", "/v1/gcp/product/decisions")
            .match_query(Matcher::UrlEncoded("repository_id".into(), REPO.into()))
            .with_status(401)
            .with_body(r#"{"error":"Unauthorized","message":"Invalid or expired token."}"#)
            .create_async()
            .await;
        let err = client(&server)
            .read_decisions(ORG, Some(REPO))
            .await
            .unwrap_err();
        assert!(matches!(err, ActualError::NotLoggedIn), "{err:?}");
    }

    fn body(json: &str) -> AdrErrorBody {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn forbidden_names_the_remedy_for_its_cause() {
        let hint_for = |message: &str| {
            let error = adr_error_from_body(
                StatusCode::FORBIDDEN,
                body(&format!(r#"{{"error":"forbidden","message":"{message}"}}"#)),
            );
            assert_eq!(error.exit_code(), 2);
            error.hint().unwrap().into_owned()
        };
        assert!(hint_for("Insufficient scope for this operation.").contains("sign in again"));
        assert!(
            hint_for("OAuth token is not authorized for the requested organization")
                .contains("for the organization")
        );
        assert!(hint_for("organization user access required").contains("Ask an admin"));
        assert_eq!(
            hint_for("OAuth token is missing member context"),
            "actual login"
        );
        let bare = adr_error_from_body(StatusCode::FORBIDDEN, AdrErrorBody::default());
        assert!(bare.to_string().ends_with("access denied"), "{bare}");
    }

    #[test]
    fn bad_requests_give_the_reasons_they_carry() {
        let check = adr_error_from_body(
            StatusCode::BAD_REQUEST,
            body(r#"{"error":"bad_request","message":"Add a title. Add at least one policy."}"#),
        );
        assert!(
            matches!(&check, ActualError::AdrInvalid { reasons }
                if reasons == &["Add a title. Add at least one policy."]),
            "{check:?}"
        );
        let schema = adr_error_from_body(
            StatusCode::BAD_REQUEST,
            body(
                r#"{"bodyErrors":{"issues":[{"path":["adr","policyLevels"],"message":"policyLevels must give one level per policy"}]}}"#,
            ),
        );
        assert!(
            matches!(&schema, ActualError::AdrInvalid { reasons }
                if reasons == &["adr.policyLevels: policyLevels must give one level per policy"]),
            "{schema:?}"
        );
        let empty = adr_error_from_body(StatusCode::BAD_REQUEST, AdrErrorBody::default());
        assert!(matches!(&empty, ActualError::ApiError(m) if m == "HTTP 400 Bad Request"));
    }

    #[test]
    fn other_refusals_keep_the_server_code_and_message() {
        let unauthorized = adr_error_from_body(StatusCode::UNAUTHORIZED, AdrErrorBody::default());
        assert!(matches!(unauthorized, ActualError::NotLoggedIn));
        let unavailable =
            adr_error_from_body(StatusCode::SERVICE_UNAVAILABLE, AdrErrorBody::default());
        assert!(matches!(unavailable, ActualError::ServiceUnavailable));
        let gone = adr_error_from_body(
            StatusCode::NOT_FOUND,
            body(r#"{"error":"not_found","message":"organization not found or not accessible"}"#),
        );
        assert!(
            matches!(&gone, ActualError::ApiResponseError { code, .. } if code == "not_found"),
            "{gone:?}"
        );
        let no_code = adr_error_from_body(
            StatusCode::TOO_MANY_REQUESTS,
            body(r#"{"message":"Too many requests. Please try again later."}"#),
        );
        assert!(
            matches!(&no_code, ActualError::ApiError(m) if m.starts_with("HTTP 429")),
            "{no_code:?}"
        );
        let bare = adr_error_from_body(StatusCode::BAD_GATEWAY, AdrErrorBody::default());
        assert!(matches!(&bare, ActualError::ApiError(m) if m == "HTTP 502 Bad Gateway"));
    }
}
