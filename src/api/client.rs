use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};

use crate::api::types::{ApiErrorResponse, MatchRequest, MatchResponse};
use crate::error::ActualError;

pub const DEFAULT_API_URL: &str = "https://api-service.api.prod.actual.ai";

pub struct ActualApiClient {
    client: reqwest::Client,
    base_url: String,
}

impl ActualApiClient {
    pub fn new(base_url: &str) -> Self {
        let user_agent = format!("{}/{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));

        let mut default_headers = HeaderMap::new();
        default_headers.insert(USER_AGENT, HeaderValue::from_str(&user_agent).unwrap());

        let client = reqwest::Client::builder()
            .default_headers(default_headers)
            .build()
            .expect("failed to build reqwest client");

        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    pub async fn post_match(&self, request: &MatchRequest) -> Result<MatchResponse, ActualError> {
        let url = format!("{}/adrs/match", self.base_url);

        let response = self
            .client
            .post(&url)
            .json(request)
            .send()
            .await
            .map_err(|e| ActualError::ApiError(e.to_string()))?;

        let status = response.status();

        if status.is_success() {
            response
                .json::<MatchResponse>()
                .await
                .map_err(|e| ActualError::ApiError(e.to_string()))
        } else if status.is_client_error() {
            match response.json::<ApiErrorResponse>().await {
                Ok(error_response) => Err(ActualError::ApiResponseError {
                    code: error_response.error.code,
                    message: error_response.error.message,
                }),
                Err(e) => Err(ActualError::ApiError(format!(
                    "HTTP {status}: failed to parse error response: {e}"
                ))),
            }
        } else {
            let body = response.text().await.unwrap_or_default();
            Err(ActualError::ApiError(format!("HTTP {status}: {body}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{MatchFramework, MatchProject};

    fn sample_request() -> MatchRequest {
        MatchRequest {
            projects: vec![MatchProject {
                path: "apps/web".to_string(),
                name: "web-app".to_string(),
                languages: vec!["typescript".to_string()],
                frameworks: vec![MatchFramework {
                    name: "nextjs".to_string(),
                    category: "frontend".to_string(),
                }],
            }],
            options: None,
        }
    }

    #[tokio::test]
    async fn test_post_match_success() {
        let mut server = mockito::Server::new_async().await;
        let body = r#"{
            "matched_adrs": [
                {
                    "id": "550e8400-e29b-41d4-a716-446655440000",
                    "title": "Use App Router for all new pages",
                    "context": "Next.js 13+ introduced the App Router",
                    "policies": ["Use the App Router (app/ directory) for all new pages"],
                    "instructions": ["New routes go in app/"],
                    "category": {"id": "cat-123", "name": "Rendering Model", "path": "Frontend > Rendering Model"},
                    "applies_to": {"languages": ["typescript"], "frameworks": ["nextjs"]},
                    "matched_projects": ["apps/web"]
                }
            ],
            "metadata": {"total_matched": 1, "by_framework": {"nextjs": 1}, "deduplicated_count": 1}
        }"#;

        let mock = server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url());
        let result = client.post_match(&sample_request()).await;

        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.matched_adrs.len(), 1);
        assert_eq!(
            response.matched_adrs[0].id,
            "550e8400-e29b-41d4-a716-446655440000"
        );
        assert_eq!(
            response.matched_adrs[0].title,
            "Use App Router for all new pages"
        );
        assert_eq!(response.metadata.total_matched, 1);

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_post_match_api_error_response() {
        let mut server = mockito::Server::new_async().await;
        let body = r#"{"error": {"code": "INVALID_REQUEST", "message": "At least one project is required", "details": null}}"#;

        let mock = server
            .mock("POST", "/adrs/match")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url());
        let result = client.post_match(&sample_request()).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ActualError::ApiResponseError { code, message } => {
                assert_eq!(code, "INVALID_REQUEST");
                assert_eq!(message, "At least one project is required");
            }
            other => panic!("expected ApiResponseError, got: {other:?}"),
        }

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_post_match_server_error() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("POST", "/adrs/match")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url());
        let result = client.post_match(&sample_request()).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ActualError::ApiError(msg) => {
                assert!(
                    msg.contains("500"),
                    "expected '500' in error message: {msg}"
                );
            }
            other => panic!("expected ApiError, got: {other:?}"),
        }

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_user_agent_header_sent() {
        let mut server = mockito::Server::new_async().await;
        let body = r#"{
            "matched_adrs": [],
            "metadata": {"total_matched": 0, "by_framework": {}, "deduplicated_count": 0}
        }"#;

        let mock = server
            .mock("POST", "/adrs/match")
            .match_header("user-agent", mockito::Matcher::Regex("actual-cli/".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = ActualApiClient::new(&server.url());
        let result = client.post_match(&sample_request()).await;

        assert!(result.is_ok());

        mock.assert_async().await;
    }
}
