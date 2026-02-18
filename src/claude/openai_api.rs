use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ActualError;
use crate::tailoring::types::TailoringOutput;

use super::subprocess::TailoringRunner;

/// Map a `reqwest::Error` from `send()` into an [`ActualError`].
///
/// Extracted so the timeout vs. other-network-error branch can be unit tested
/// without an actual network roundtrip.
fn map_send_error(e: reqwest::Error, timeout: Duration) -> ActualError {
    if e.is_timeout() {
        ActualError::ClaudeTimeout {
            seconds: timeout.as_secs(),
        }
    } else {
        ActualError::ClaudeSubprocessFailed {
            message: format!("OpenAI API request failed: {e}"),
            stderr: String::new(),
        }
    }
}

/// Runner that uses the OpenAI Responses API with structured JSON output.
///
/// Implements [`TailoringRunner`] by calling `POST /v1/responses` with a
/// JSON schema constraint, so the model always returns well-formed
/// [`TailoringOutput`] JSON without tool calls or multi-turn agentic loops.
pub struct OpenAiApiRunner {
    api_key: String,
    model: String,
    client: reqwest::Client,
    timeout: Duration,
    /// Base URL for the OpenAI API (overridable for tests via mockito).
    base_url: String,
}

// ── Request / response shapes ────────────────────────────────────────────────

#[derive(Serialize)]
struct RequestBody {
    model: String,
    input: Vec<InputMessage>,
    text: TextOptions,
}

#[derive(Serialize)]
struct InputMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct TextOptions {
    format: JsonSchemaFormat,
}

#[derive(Serialize)]
struct JsonSchemaFormat {
    #[serde(rename = "type")]
    format_type: String,
    name: String,
    schema: Value,
    strict: bool,
}

/// Top-level response from the OpenAI Responses API.
#[derive(Deserialize)]
struct ApiResponse {
    output: Vec<OutputItem>,
}

#[derive(Deserialize)]
struct OutputItem {
    #[serde(rename = "type")]
    item_type: String,
    content: Option<Vec<ContentItem>>,
}

#[derive(Deserialize)]
struct ContentItem {
    #[serde(rename = "type")]
    content_type: String,
    text: Option<String>,
    refusal: Option<String>,
}

// ── Constructor ───────────────────────────────────────────────────────────────

impl OpenAiApiRunner {
    /// Create a new runner with an explicit API key and model.
    pub fn new(api_key: String, model: String, timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("failed to build reqwest client");
        Self {
            api_key,
            model,
            client,
            timeout,
            base_url: "https://api.openai.com".to_string(),
        }
    }

    /// Create a runner by reading `OPENAI_API_KEY` from the environment.
    ///
    /// Returns `Err(ActualError::ClaudeNotAuthenticated)` when the variable is unset.
    pub fn from_env(model: String, timeout: Duration) -> Result<Self, ActualError> {
        let api_key =
            std::env::var("OPENAI_API_KEY").map_err(|_| ActualError::ClaudeNotAuthenticated)?;
        Ok(Self::new(api_key, model, timeout))
    }

    /// Override the base URL (used in tests to point at a mockito server).
    #[cfg(test)]
    fn with_base_url(mut self, base_url: String) -> Self {
        self.base_url = base_url;
        self
    }
}

// ── TailoringRunner impl ──────────────────────────────────────────────────────

impl TailoringRunner for OpenAiApiRunner {
    async fn run_tailoring(
        &self,
        prompt: &str,
        schema: &str,
        model_override: Option<&str>,
        _max_budget_usd: Option<f64>,
    ) -> Result<TailoringOutput, ActualError> {
        let model = model_override.unwrap_or(&self.model).to_string();
        let schema_value: Value = serde_json::from_str(schema)?;

        let body = RequestBody {
            model,
            input: vec![
                InputMessage {
                    role: "system".to_string(),
                    content: "You are an expert software architect. \
                        Analyze the provided repository context and the user's request, \
                        then respond with a valid JSON object matching the specified schema."
                        .to_string(),
                },
                InputMessage {
                    role: "user".to_string(),
                    content: prompt.to_string(),
                },
            ],
            text: TextOptions {
                format: JsonSchemaFormat {
                    format_type: "json_schema".to_string(),
                    name: "tailoring_output".to_string(),
                    schema: schema_value,
                    strict: true,
                },
            },
        };

        let url = format!("{}/v1/responses", self.base_url);

        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| map_send_error(e, self.timeout))?;

        let status = response.status();

        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ActualError::ClaudeNotAuthenticated);
        }

        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(ActualError::ClaudeSubprocessFailed {
                message: "OpenAI API rate limited".to_string(),
                stderr: String::new(),
            });
        }

        if status.is_server_error() {
            let body_text = response.text().await.unwrap_or_default();
            return Err(ActualError::ClaudeSubprocessFailed {
                message: format!("OpenAI API error: {status}"),
                stderr: body_text,
            });
        }

        let api_response: ApiResponse = response.json().await.map_err(|e| {
            // reqwest JSON parse failure wraps serde_json; surface as ClaudeSubprocessFailed
            // with the original message since we can't recover the raw bytes at this point.
            ActualError::ClaudeSubprocessFailed {
                message: format!("Failed to parse OpenAI API response: {e}"),
                stderr: String::new(),
            }
        })?;

        // Find the message output item.
        let message_item = api_response
            .output
            .into_iter()
            .find(|item| item.item_type == "message");

        let content_items = match message_item {
            Some(item) => item.content.unwrap_or_default(),
            None => {
                return Err(ActualError::ClaudeSubprocessFailed {
                    message: "OpenAI API did not return text output".to_string(),
                    stderr: String::new(),
                });
            }
        };

        // Check for refusal before looking for text.
        for content in &content_items {
            if content.content_type == "refusal" {
                let reason = content
                    .refusal
                    .clone()
                    .unwrap_or_else(|| "unknown reason".to_string());
                return Err(ActualError::TailoringValidationError(format!(
                    "OpenAI refused: {reason}"
                )));
            }
        }

        // Find output_text content item.
        let text = content_items
            .into_iter()
            .find(|c| c.content_type == "output_text")
            .and_then(|c| c.text);

        let text = match text {
            Some(t) => t,
            None => {
                return Err(ActualError::ClaudeSubprocessFailed {
                    message: "OpenAI API did not return text output".to_string(),
                    stderr: String::new(),
                });
            }
        };

        let output: TailoringOutput = serde_json::from_str(&text)?;
        Ok(output)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    /// Minimal valid [`TailoringOutput`] as a JSON string.
    fn minimal_tailoring_output_json() -> String {
        serde_json::json!({
            "files": [],
            "skipped_adrs": [],
            "summary": {
                "total_input": 0,
                "applicable": 0,
                "not_applicable": 0,
                "files_generated": 0
            }
        })
        .to_string()
    }

    /// Build a valid Responses API response body whose `output_text` contains `inner`.
    fn responses_body(inner: &str) -> String {
        // Escape the inner JSON string so it is valid JSON-in-JSON.
        let escaped = serde_json::to_string(inner).unwrap();
        format!(
            r#"{{
                "id": "resp_test",
                "output": [
                    {{
                        "type": "message",
                        "role": "assistant",
                        "content": [
                            {{
                                "type": "output_text",
                                "text": {escaped}
                            }}
                        ]
                    }}
                ]
            }}"#
        )
    }

    fn make_runner(server: &Server) -> OpenAiApiRunner {
        OpenAiApiRunner::new(
            "test-key".to_string(),
            "gpt-4o".to_string(),
            Duration::from_secs(10),
        )
        .with_base_url(server.url())
    }

    // Test 1: valid response is parsed correctly into TailoringOutput
    #[tokio::test]
    async fn test_valid_response_parsed_correctly() {
        let mut server = Server::new_async().await;

        let inner = minimal_tailoring_output_json();
        let body = responses_body(&inner);

        let mock = server
            .mock("POST", "/v1/responses")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&body)
            .create_async()
            .await;

        let runner = make_runner(&server);
        let schema = r#"{"type":"object"}"#;
        let result = runner
            .run_tailoring("test prompt", schema, None, None)
            .await
            .unwrap();

        assert_eq!(result.files.len(), 0);
        assert_eq!(result.summary.total_input, 0);

        mock.assert_async().await;
    }

    // Test 2: HTTP 401 maps to ClaudeNotAuthenticated
    #[tokio::test]
    async fn test_401_maps_to_not_authenticated() {
        let mut server = Server::new_async().await;

        let mock = server
            .mock("POST", "/v1/responses")
            .with_status(401)
            .with_body(r#"{"error": "Unauthorized"}"#)
            .create_async()
            .await;

        let runner = make_runner(&server);
        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await;

        assert!(
            matches!(result, Err(ActualError::ClaudeNotAuthenticated)),
            "expected ClaudeNotAuthenticated, got: {:?}",
            result
        );

        mock.assert_async().await;
    }

    // Test 3: HTTP 429 maps to ClaudeSubprocessFailed with rate limit message
    #[tokio::test]
    async fn test_429_maps_to_rate_limited() {
        let mut server = Server::new_async().await;

        let mock = server
            .mock("POST", "/v1/responses")
            .with_status(429)
            .with_body(r#"{"error": "Too Many Requests"}"#)
            .create_async()
            .await;

        let runner = make_runner(&server);
        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await;

        match result {
            Err(ActualError::ClaudeSubprocessFailed { message, .. }) => {
                assert!(
                    message.contains("rate"),
                    "expected 'rate' in message, got: {message}"
                );
            }
            other => panic!("expected ClaudeSubprocessFailed, got: {:?}", other),
        }

        mock.assert_async().await;
    }

    // Test 4: refusal response maps to TailoringValidationError
    #[tokio::test]
    async fn test_refusal_maps_to_validation_error() {
        let mut server = Server::new_async().await;

        let body = r#"{
            "id": "resp_test",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "refusal",
                            "refusal": "I cannot help with that request."
                        }
                    ]
                }
            ]
        }"#;

        let mock = server
            .mock("POST", "/v1/responses")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let runner = make_runner(&server);
        let result = runner
            .run_tailoring("bad request", r#"{"type":"object"}"#, None, None)
            .await;

        match result {
            Err(ActualError::TailoringValidationError(msg)) => {
                assert!(
                    msg.contains("OpenAI refused"),
                    "expected 'OpenAI refused' in: {msg}"
                );
                assert!(
                    msg.contains("I cannot help"),
                    "expected refusal text in: {msg}"
                );
            }
            other => panic!("expected TailoringValidationError, got: {:?}", other),
        }

        mock.assert_async().await;
    }

    // Test 5: missing output_text (empty output array) maps to ClaudeSubprocessFailed
    #[tokio::test]
    async fn test_empty_output_maps_to_subprocess_failed() {
        let mut server = Server::new_async().await;

        let body = r#"{
            "id": "resp_test",
            "output": []
        }"#;

        let mock = server
            .mock("POST", "/v1/responses")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let runner = make_runner(&server);
        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await;

        match result {
            Err(ActualError::ClaudeSubprocessFailed { message, .. }) => {
                assert!(
                    message.contains("did not return text output"),
                    "expected 'did not return text output' in: {message}"
                );
            }
            other => panic!("expected ClaudeSubprocessFailed, got: {:?}", other),
        }

        mock.assert_async().await;
    }

    /// Serializes tests that mutate `OPENAI_API_KEY` to avoid data races.
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // Test 6: from_env returns Err when OPENAI_API_KEY is not set
    #[test]
    fn test_from_env_missing_key() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let prev = std::env::var("OPENAI_API_KEY").ok();
        // SAFETY: protected by ENV_MUTEX; no concurrent env access in tests.
        unsafe { std::env::remove_var("OPENAI_API_KEY") };

        let result = OpenAiApiRunner::from_env("gpt-4o".to_string(), Duration::from_secs(10));
        assert!(
            matches!(result, Err(ActualError::ClaudeNotAuthenticated)),
            "expected ClaudeNotAuthenticated when OPENAI_API_KEY is unset"
        );

        if let Some(val) = prev {
            unsafe { std::env::set_var("OPENAI_API_KEY", val) };
        }
    }

    // Test 7: HTTP 403 also maps to ClaudeNotAuthenticated
    #[tokio::test]
    async fn test_403_maps_to_not_authenticated() {
        let mut server = Server::new_async().await;

        let mock = server
            .mock("POST", "/v1/responses")
            .with_status(403)
            .with_body(r#"{"error": "Forbidden"}"#)
            .create_async()
            .await;

        let runner = make_runner(&server);
        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await;

        assert!(
            matches!(result, Err(ActualError::ClaudeNotAuthenticated)),
            "expected ClaudeNotAuthenticated for 403, got: {:?}",
            result
        );

        mock.assert_async().await;
    }

    // Test 8: HTTP 500 maps to ClaudeSubprocessFailed with status in message
    #[tokio::test]
    async fn test_500_maps_to_subprocess_failed() {
        let mut server = Server::new_async().await;

        let mock = server
            .mock("POST", "/v1/responses")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let runner = make_runner(&server);
        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await;

        match result {
            Err(ActualError::ClaudeSubprocessFailed { message, stderr }) => {
                assert!(
                    message.contains("500"),
                    "expected status code in message: {message}"
                );
                assert!(
                    stderr.contains("Internal Server Error"),
                    "expected body in stderr: {stderr}"
                );
            }
            other => panic!("expected ClaudeSubprocessFailed, got: {:?}", other),
        }

        mock.assert_async().await;
    }

    // Test 9: model_override is respected
    #[tokio::test]
    async fn test_model_override_used() {
        let mut server = Server::new_async().await;

        let inner = minimal_tailoring_output_json();
        let body = responses_body(&inner);

        // We can't easily inspect the request body in mockito without a matcher,
        // but we can at least ensure the call succeeds with a model override.
        let mock = server
            .mock("POST", "/v1/responses")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&body)
            .create_async()
            .await;

        let runner = make_runner(&server);
        let result = runner
            .run_tailoring(
                "test prompt",
                r#"{"type":"object"}"#,
                Some("gpt-4o-mini"),
                None,
            )
            .await;

        assert!(
            result.is_ok(),
            "expected Ok with model override, got: {:?}",
            result
        );
        mock.assert_async().await;
    }

    // Test 10: from_env succeeds when OPENAI_API_KEY is set
    #[test]
    fn test_from_env_success() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let prev = std::env::var("OPENAI_API_KEY").ok();
        // SAFETY: protected by ENV_MUTEX.
        unsafe { std::env::set_var("OPENAI_API_KEY", "test-key-from-env") };

        let result = OpenAiApiRunner::from_env("gpt-4o".to_string(), Duration::from_secs(10));
        assert!(result.is_ok(), "expected Ok when OPENAI_API_KEY is set");

        // Restore.
        match prev {
            Some(val) => unsafe { std::env::set_var("OPENAI_API_KEY", val) },
            None => unsafe { std::env::remove_var("OPENAI_API_KEY") },
        }
    }

    // Test 11: refusal with no refusal field falls back to "unknown reason"
    #[tokio::test]
    async fn test_refusal_without_reason_text() {
        let mut server = Server::new_async().await;

        // content_type is "refusal" but the "refusal" field itself is missing/null.
        let body = r#"{
            "id": "resp_test",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "refusal"
                        }
                    ]
                }
            ]
        }"#;

        let mock = server
            .mock("POST", "/v1/responses")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let runner = make_runner(&server);
        let result = runner
            .run_tailoring("bad request", r#"{"type":"object"}"#, None, None)
            .await;

        match result {
            Err(ActualError::TailoringValidationError(msg)) => {
                assert!(
                    msg.contains("OpenAI refused"),
                    "expected 'OpenAI refused' in: {msg}"
                );
                assert!(
                    msg.contains("unknown reason"),
                    "expected 'unknown reason' fallback in: {msg}"
                );
            }
            other => panic!("expected TailoringValidationError, got: {:?}", other),
        }

        mock.assert_async().await;
    }

    // Test 12: message item with null content falls back to empty vec (unwrap_or_default),
    // and then fails with "did not return text output" (no output_text in empty content).
    #[tokio::test]
    async fn test_message_with_null_content() {
        let mut server = Server::new_async().await;

        let body = r#"{
            "id": "resp_test",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": null
                }
            ]
        }"#;

        let mock = server
            .mock("POST", "/v1/responses")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let runner = make_runner(&server);
        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await;

        match result {
            Err(ActualError::ClaudeSubprocessFailed { message, .. }) => {
                assert!(
                    message.contains("did not return text output"),
                    "expected 'did not return text output' in: {message}"
                );
            }
            other => panic!("expected ClaudeSubprocessFailed, got: {:?}", other),
        }

        mock.assert_async().await;
    }

    // Test 13: message output item with content that has no output_text (only unknown types)
    #[tokio::test]
    async fn test_message_content_no_output_text_item() {
        let mut server = Server::new_async().await;

        let body = r#"{
            "id": "resp_test",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "some_other_type",
                            "text": "hello"
                        }
                    ]
                }
            ]
        }"#;

        let mock = server
            .mock("POST", "/v1/responses")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let runner = make_runner(&server);
        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await;

        match result {
            Err(ActualError::ClaudeSubprocessFailed { message, .. }) => {
                assert!(
                    message.contains("did not return text output"),
                    "expected 'did not return text output' in: {message}"
                );
            }
            other => panic!("expected ClaudeSubprocessFailed, got: {:?}", other),
        }

        mock.assert_async().await;
    }

    // Test 14: HTTP 200 with non-JSON body maps to ClaudeSubprocessFailed (parse error)
    #[tokio::test]
    async fn test_malformed_json_response() {
        let mut server = Server::new_async().await;

        let mock = server
            .mock("POST", "/v1/responses")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("this is not valid json at all")
            .create_async()
            .await;

        let runner = make_runner(&server);
        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await;

        match result {
            Err(ActualError::ClaudeSubprocessFailed { message, .. }) => {
                assert!(
                    message.contains("Failed to parse OpenAI API response"),
                    "expected parse error message, got: {message}"
                );
            }
            other => panic!("expected ClaudeSubprocessFailed, got: {:?}", other),
        }

        mock.assert_async().await;
    }

    // Test 15: output has a non-message type item (no message item found at all)
    #[tokio::test]
    async fn test_non_message_output_type() {
        let mut server = Server::new_async().await;

        let body = r#"{
            "id": "resp_test",
            "output": [
                {
                    "type": "function_call",
                    "name": "some_tool"
                }
            ]
        }"#;

        let mock = server
            .mock("POST", "/v1/responses")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let runner = make_runner(&server);
        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await;

        match result {
            Err(ActualError::ClaudeSubprocessFailed { message, .. }) => {
                assert!(
                    message.contains("did not return text output"),
                    "expected 'did not return text output' in: {message}"
                );
            }
            other => panic!("expected ClaudeSubprocessFailed, got: {:?}", other),
        }

        mock.assert_async().await;
    }

    // Test 16: output_text with null text field (and_then returns None → ClaudeSubprocessFailed)
    #[tokio::test]
    async fn test_output_text_with_null_text() {
        let mut server = Server::new_async().await;

        let body = r#"{
            "id": "resp_test",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "output_text",
                            "text": null
                        }
                    ]
                }
            ]
        }"#;

        let mock = server
            .mock("POST", "/v1/responses")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let runner = make_runner(&server);
        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await;

        match result {
            Err(ActualError::ClaudeSubprocessFailed { message, .. }) => {
                assert!(
                    message.contains("did not return text output"),
                    "expected 'did not return text output' in: {message}"
                );
            }
            other => panic!("expected ClaudeSubprocessFailed, got: {:?}", other),
        }

        mock.assert_async().await;
    }

    // Test 17: network error (connection refused) maps to an error
    #[tokio::test]
    async fn test_network_error_maps_to_subprocess_failed() {
        // Point at a port that won't be listening.
        let runner = OpenAiApiRunner::new(
            "test-key".to_string(),
            "gpt-4o".to_string(),
            Duration::from_secs(10),
        )
        .with_base_url("http://127.0.0.1:1".to_string()); // port 1 is reserved, always refused

        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await;

        // Either ClaudeSubprocessFailed or ClaudeTimeout depending on OS behavior.
        assert!(
            matches!(
                result,
                Err(ActualError::ClaudeSubprocessFailed { .. })
                    | Err(ActualError::ClaudeTimeout { .. })
            ),
            "expected a network-related error, got: {:?}",
            result
        );
    }

    // Test 18: map_send_error with a non-timeout network error → ClaudeSubprocessFailed
    #[tokio::test]
    async fn test_map_send_error_non_timeout() {
        // Connection refused is reliably a non-timeout error on loopback.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let err = client
            .get("http://127.0.0.1:1/test") // port 1 always refused
            .send()
            .await
            .unwrap_err();

        // Regardless of whether it ends up as timeout or not, map_send_error must
        // return one of the two expected variants — no panics.
        let mapped = map_send_error(err, Duration::from_secs(10));
        assert!(
            matches!(
                mapped,
                ActualError::ClaudeSubprocessFailed { .. } | ActualError::ClaudeTimeout { .. }
            ),
            "expected ClaudeSubprocessFailed or ClaudeTimeout, got: {:?}",
            mapped
        );
    }

    // Test 19: ClaudeTimeout produced when reqwest times out before the server responds.
    //
    // We start a TCP server that accepts connections but never sends an HTTP response,
    // then send a request with a 1ms timeout.  The timeout fires before the server
    // responds, giving us a `reqwest::Error` with `is_timeout() == true`, which
    // `map_send_error` should turn into `ActualError::ClaudeTimeout`.
    #[tokio::test]
    async fn test_run_tailoring_request_timeout() {
        use std::net::TcpListener;

        // Bind a server socket on a free port.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        // Background task: accept one connection and hold it open for 2 seconds
        // without writing any HTTP response.  The client will time out first.
        tokio::task::spawn_blocking(move || {
            let (_conn, _) = listener.accept().unwrap();
            std::thread::sleep(std::time::Duration::from_secs(2));
        });

        // Use a 1ms timeout so the client times out almost immediately.
        let runner = OpenAiApiRunner::new(
            "key".to_string(),
            "gpt-4o".to_string(),
            Duration::from_millis(1),
        )
        .with_base_url(format!("http://{addr}"));

        let result = runner
            .run_tailoring("prompt", r#"{"type":"object"}"#, None, None)
            .await;

        // On almost all environments this will be ClaudeTimeout because the 1ms
        // timer fires before we even get HTTP headers back from the silent server.
        // In rare environments where the OS reports an error differently, we also
        // accept ClaudeSubprocessFailed.
        assert!(
            matches!(
                result,
                Err(ActualError::ClaudeTimeout { .. })
                    | Err(ActualError::ClaudeSubprocessFailed { .. })
            ),
            "expected ClaudeTimeout or ClaudeSubprocessFailed, got: {result:?}"
        );
    }
}
