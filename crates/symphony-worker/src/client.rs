use async_trait::async_trait;
use symphony::protocol::{
    AgentEvent, WorkAssignment, WorkResult, WorkerEventPayload, WorkerExitReason, WorkerHeartbeat,
    WorkerRegistration,
};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("HTTP request failed: {reason}")]
    RequestFailed { reason: String },
    #[error("orchestrator returned {status}: {body}")]
    BadStatus { status: u16, body: String },
    #[error("failed to parse response: {reason}")]
    ParseFailed { reason: String },
}

// ---------------------------------------------------------------------------
// Trait (for mocking)
// ---------------------------------------------------------------------------

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait OrchestratorClient: Send + Sync {
    async fn claim_work(&self) -> Result<Option<WorkAssignment>, ClientError>;
    async fn send_event(
        &self,
        issue_identifier: &str,
        event: AgentEvent,
    ) -> Result<(), ClientError>;
    async fn send_complete(
        &self,
        issue_identifier: &str,
        reason: WorkerExitReason,
    ) -> Result<(), ClientError>;
    async fn send_heartbeat(&self, heartbeat: WorkerHeartbeat) -> Result<(), ClientError>;
    async fn send_register(&self, registration: WorkerRegistration) -> Result<(), ClientError>;
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

pub struct HttpOrchestratorClient {
    http: reqwest::Client,
    base_url: String,
    auth_token: String,
    worker_id: String,
}

impl HttpOrchestratorClient {
    pub fn new(base_url: String, auth_token: String, worker_id: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url,
            auth_token,
            worker_id,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers to avoid LLVM coverage splits from reqwest error mapping
// ---------------------------------------------------------------------------

fn map_request_error(e: reqwest::Error) -> ClientError {
    ClientError::RequestFailed {
        reason: e.to_string(),
    }
}

async fn read_response_body(resp: reqwest::Response) -> Result<(u16, String), ClientError> {
    let status = resp.status().as_u16();
    let body = resp.text().await.map_err(map_request_error)?;
    Ok((status, body))
}

#[async_trait]
impl OrchestratorClient for HttpOrchestratorClient {
    async fn claim_work(&self) -> Result<Option<WorkAssignment>, ClientError> {
        let url = format!("{}/api/v1/work", self.base_url);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.auth_token)
            .header("X-Worker-ID", &self.worker_id)
            .send()
            .await
            .map_err(map_request_error)?;

        let (status, body) = read_response_body(resp).await?;

        match status {
            200 => {
                let assignment: WorkAssignment =
                    serde_json::from_str(&body).map_err(|e| ClientError::ParseFailed {
                        reason: e.to_string(),
                    })?;
                Ok(Some(assignment))
            }
            204 => Ok(None),
            _ => Err(ClientError::BadStatus { status, body }),
        }
    }

    async fn send_event(
        &self,
        issue_identifier: &str,
        event: AgentEvent,
    ) -> Result<(), ClientError> {
        let url = format!("{}/api/v1/{}/events", self.base_url, issue_identifier);
        let payload = WorkerEventPayload { event };
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.auth_token)
            .header("X-Worker-ID", &self.worker_id)
            .json(&payload)
            .send()
            .await
            .map_err(map_request_error)?;

        let (status, body) = read_response_body(resp).await?;

        if status == 200 {
            Ok(())
        } else {
            Err(ClientError::BadStatus { status, body })
        }
    }

    async fn send_complete(
        &self,
        issue_identifier: &str,
        reason: WorkerExitReason,
    ) -> Result<(), ClientError> {
        let url = format!("{}/api/v1/{}/complete", self.base_url, issue_identifier);
        let payload = WorkResult { reason };
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.auth_token)
            .header("X-Worker-ID", &self.worker_id)
            .json(&payload)
            .send()
            .await
            .map_err(map_request_error)?;

        let (status, body) = read_response_body(resp).await?;

        if status == 200 {
            Ok(())
        } else {
            Err(ClientError::BadStatus { status, body })
        }
    }

    async fn send_heartbeat(&self, heartbeat: WorkerHeartbeat) -> Result<(), ClientError> {
        let url = format!("{}/api/v1/heartbeat", self.base_url);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.auth_token)
            .header("X-Worker-ID", &self.worker_id)
            .json(&heartbeat)
            .send()
            .await
            .map_err(map_request_error)?;

        let (status, body) = read_response_body(resp).await?;

        if status == 200 {
            Ok(())
        } else {
            Err(ClientError::BadStatus { status, body })
        }
    }

    async fn send_register(&self, registration: WorkerRegistration) -> Result<(), ClientError> {
        let url = format!("{}/api/v1/workers/register", self.base_url);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.auth_token)
            .header("X-Worker-ID", &self.worker_id)
            .json(&registration)
            .send()
            .await
            .map_err(map_request_error)?;

        let (status, body) = read_response_body(resp).await?;

        if status == 200 {
            Ok(())
        } else {
            Err(ClientError::BadStatus { status, body })
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Error Display tests -----------------------------------------------

    #[test]
    fn test_error_display_request_failed() {
        let err = ClientError::RequestFailed {
            reason: "connection refused".to_string(),
        };
        assert_eq!(err.to_string(), "HTTP request failed: connection refused");
    }

    #[test]
    fn test_error_display_bad_status() {
        let err = ClientError::BadStatus {
            status: 500,
            body: "internal error".to_string(),
        };
        assert_eq!(err.to_string(), "orchestrator returned 500: internal error");
    }

    #[test]
    fn test_error_display_parse_failed() {
        let err = ClientError::ParseFailed {
            reason: "invalid json".to_string(),
        };
        assert_eq!(err.to_string(), "failed to parse response: invalid json");
    }

    #[test]
    fn test_error_is_debug() {
        let err = ClientError::RequestFailed {
            reason: "timeout".to_string(),
        };
        let debug = format!("{:?}", err);
        assert!(debug.contains("RequestFailed"));
    }

    // ---- HTTP client tests (using mockito) --------------------------------

    #[tokio::test]
    async fn test_claim_work_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v1/work")
            .match_header("Authorization", "Bearer test-token")
            .match_header("X-Worker-ID", "worker-1")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&WorkAssignment {
                    issue_id: "id-1".to_string(),
                    issue_identifier: "TST-42".to_string(),
                    prompt: "Fix the bug".to_string(),
                    attempt: Some(1),
                    workspace_path: None,
                })
                .unwrap(),
            )
            .create_async()
            .await;

        let client = HttpOrchestratorClient::new(
            server.url(),
            "test-token".to_string(),
            "worker-1".to_string(),
        );

        let result = client.claim_work().await;
        assert!(result.is_ok());
        let assignment = result.unwrap();
        assert!(assignment.is_some());
        let assignment = assignment.unwrap();
        assert_eq!(assignment.issue_id, "id-1");
        assert_eq!(assignment.issue_identifier, "TST-42");
        assert_eq!(assignment.prompt, "Fix the bug");
        assert_eq!(assignment.attempt, Some(1));

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_claim_work_no_content() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v1/work")
            .with_status(204)
            .with_body("")
            .create_async()
            .await;

        let client = HttpOrchestratorClient::new(
            server.url(),
            "test-token".to_string(),
            "worker-1".to_string(),
        );

        let result = client.claim_work().await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_claim_work_error_status() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v1/work")
            .with_status(500)
            .with_body("internal server error")
            .create_async()
            .await;

        let client = HttpOrchestratorClient::new(
            server.url(),
            "test-token".to_string(),
            "worker-1".to_string(),
        );

        let result = client.claim_work().await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        match &err {
            ClientError::BadStatus { status, body } => {
                assert_eq!(*status, 500);
                assert_eq!(body, "internal server error");
            }
            other => panic!("expected BadStatus, got {:?}", other),
        }

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_claim_work_network_error() {
        // Use a URL that cannot connect
        let client = HttpOrchestratorClient::new(
            "http://127.0.0.1:1".to_string(),
            "test-token".to_string(),
            "worker-1".to_string(),
        );

        let result = client.claim_work().await;
        assert!(result.is_err());
        match &result.unwrap_err() {
            ClientError::RequestFailed { reason } => {
                assert!(!reason.is_empty());
            }
            other => panic!("expected RequestFailed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_claim_work_parse_error() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v1/work")
            .with_status(200)
            .with_body("not valid json{{{")
            .create_async()
            .await;

        let client = HttpOrchestratorClient::new(
            server.url(),
            "test-token".to_string(),
            "worker-1".to_string(),
        );

        let result = client.claim_work().await;
        assert!(result.is_err());
        match &result.unwrap_err() {
            ClientError::ParseFailed { reason } => {
                assert!(!reason.is_empty());
            }
            other => panic!("expected ParseFailed, got {:?}", other),
        }

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_send_event_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/TST-42/events")
            .match_header("Authorization", "Bearer test-token")
            .match_header("X-Worker-ID", "worker-1")
            .with_status(200)
            .with_body("")
            .create_async()
            .await;

        let client = HttpOrchestratorClient::new(
            server.url(),
            "test-token".to_string(),
            "worker-1".to_string(),
        );

        let event = AgentEvent::Notification {
            message: "hello".to_string(),
        };
        let result = client.send_event("TST-42", event).await;
        assert!(result.is_ok());

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_send_event_error() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/TST-42/events")
            .with_status(500)
            .with_body("server error")
            .create_async()
            .await;

        let client = HttpOrchestratorClient::new(
            server.url(),
            "test-token".to_string(),
            "worker-1".to_string(),
        );

        let event = AgentEvent::Notification {
            message: "hello".to_string(),
        };
        let result = client.send_event("TST-42", event).await;
        assert!(result.is_err());
        match &result.unwrap_err() {
            ClientError::BadStatus { status, body } => {
                assert_eq!(*status, 500);
                assert_eq!(body, "server error");
            }
            other => panic!("expected BadStatus, got {:?}", other),
        }

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_send_complete_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/TST-42/complete")
            .match_header("Authorization", "Bearer test-token")
            .match_header("X-Worker-ID", "worker-1")
            .with_status(200)
            .with_body("")
            .create_async()
            .await;

        let client = HttpOrchestratorClient::new(
            server.url(),
            "test-token".to_string(),
            "worker-1".to_string(),
        );

        let result = client
            .send_complete("TST-42", WorkerExitReason::Normal)
            .await;
        assert!(result.is_ok());

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_send_complete_error() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/TST-42/complete")
            .with_status(500)
            .with_body("server error")
            .create_async()
            .await;

        let client = HttpOrchestratorClient::new(
            server.url(),
            "test-token".to_string(),
            "worker-1".to_string(),
        );

        let result = client
            .send_complete("TST-42", WorkerExitReason::Normal)
            .await;
        assert!(result.is_err());
        match &result.unwrap_err() {
            ClientError::BadStatus { status, body } => {
                assert_eq!(*status, 500);
                assert_eq!(body, "server error");
            }
            other => panic!("expected BadStatus, got {:?}", other),
        }

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_send_event_network_error() {
        let client = HttpOrchestratorClient::new(
            "http://127.0.0.1:1".to_string(),
            "test-token".to_string(),
            "worker-1".to_string(),
        );

        let event = AgentEvent::Notification {
            message: "hello".to_string(),
        };
        let result = client.send_event("TST-42", event).await;
        assert!(result.is_err());
        match &result.unwrap_err() {
            ClientError::RequestFailed { .. } => {}
            other => panic!("expected RequestFailed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_send_complete_network_error() {
        let client = HttpOrchestratorClient::new(
            "http://127.0.0.1:1".to_string(),
            "test-token".to_string(),
            "worker-1".to_string(),
        );

        let result = client
            .send_complete("TST-42", WorkerExitReason::Normal)
            .await;
        assert!(result.is_err());
        match &result.unwrap_err() {
            ClientError::RequestFailed { .. } => {}
            other => panic!("expected RequestFailed, got {:?}", other),
        }
    }

    #[test]
    fn test_http_client_new() {
        let client = HttpOrchestratorClient::new(
            "http://localhost".to_string(),
            "t".to_string(),
            "w".to_string(),
        );
        assert_eq!(client.base_url, "http://localhost");
        assert_eq!(client.auth_token, "t");
        assert_eq!(client.worker_id, "w");
    }

    // ---- send_heartbeat tests -------------------------------------------

    #[tokio::test]
    async fn test_send_heartbeat_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/heartbeat")
            .match_header("Authorization", "Bearer test-token")
            .match_header("X-Worker-ID", "worker-1")
            .with_status(200)
            .with_body("")
            .create_async()
            .await;

        let client = HttpOrchestratorClient::new(
            server.url(),
            "test-token".to_string(),
            "worker-1".to_string(),
        );

        let heartbeat = WorkerHeartbeat {
            worker_id: "worker-1".to_string(),
            active_jobs: vec!["TST-1".to_string()],
            timestamp: chrono::Utc::now(),
        };
        let result = client.send_heartbeat(heartbeat).await;
        assert!(result.is_ok());

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_send_heartbeat_error() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/heartbeat")
            .with_status(500)
            .with_body("server error")
            .create_async()
            .await;

        let client = HttpOrchestratorClient::new(
            server.url(),
            "test-token".to_string(),
            "worker-1".to_string(),
        );

        let heartbeat = WorkerHeartbeat {
            worker_id: "worker-1".to_string(),
            active_jobs: vec![],
            timestamp: chrono::Utc::now(),
        };
        let result = client.send_heartbeat(heartbeat).await;
        assert!(result.is_err());
        match &result.unwrap_err() {
            ClientError::BadStatus { status, body } => {
                assert_eq!(*status, 500);
                assert_eq!(body, "server error");
            }
            other => panic!("expected BadStatus, got {:?}", other),
        }

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_send_heartbeat_network_error() {
        let client = HttpOrchestratorClient::new(
            "http://127.0.0.1:1".to_string(),
            "test-token".to_string(),
            "worker-1".to_string(),
        );

        let heartbeat = WorkerHeartbeat {
            worker_id: "worker-1".to_string(),
            active_jobs: vec![],
            timestamp: chrono::Utc::now(),
        };
        let result = client.send_heartbeat(heartbeat).await;
        assert!(result.is_err());
        match &result.unwrap_err() {
            ClientError::RequestFailed { .. } => {}
            other => panic!("expected RequestFailed, got {:?}", other),
        }
    }

    // ---- send_register tests -------------------------------------------

    #[tokio::test]
    async fn test_send_register_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/workers/register")
            .match_header("Authorization", "Bearer test-token")
            .match_header("X-Worker-ID", "worker-1")
            .with_status(200)
            .with_body(r#"{"worker_id": "worker-1"}"#)
            .create_async()
            .await;

        let client = HttpOrchestratorClient::new(
            server.url(),
            "test-token".to_string(),
            "worker-1".to_string(),
        );

        let registration = WorkerRegistration {
            worker_id: "worker-1".to_string(),
            capabilities: vec!["rust".to_string()],
            max_concurrent_jobs: 2,
        };
        let result = client.send_register(registration).await;
        assert!(result.is_ok());

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_send_register_error() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/workers/register")
            .with_status(500)
            .with_body("server error")
            .create_async()
            .await;

        let client = HttpOrchestratorClient::new(
            server.url(),
            "test-token".to_string(),
            "worker-1".to_string(),
        );

        let registration = WorkerRegistration {
            worker_id: "worker-1".to_string(),
            capabilities: vec![],
            max_concurrent_jobs: 1,
        };
        let result = client.send_register(registration).await;
        assert!(result.is_err());
        match &result.unwrap_err() {
            ClientError::BadStatus { status, body } => {
                assert_eq!(*status, 500);
                assert_eq!(body, "server error");
            }
            other => panic!("expected BadStatus, got {:?}", other),
        }

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_send_register_network_error() {
        let client = HttpOrchestratorClient::new(
            "http://127.0.0.1:1".to_string(),
            "test-token".to_string(),
            "worker-1".to_string(),
        );

        let registration = WorkerRegistration {
            worker_id: "worker-1".to_string(),
            capabilities: vec![],
            max_concurrent_jobs: 1,
        };
        let result = client.send_register(registration).await;
        assert!(result.is_err());
        match &result.unwrap_err() {
            ClientError::RequestFailed { .. } => {}
            other => panic!("expected RequestFailed, got {:?}", other),
        }
    }
}
