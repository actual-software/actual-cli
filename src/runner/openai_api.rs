use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ActualError;
use crate::tailoring::types::TailoringOutput;

use super::subprocess::TailoringRunner;

/// Maximum number of retries on HTTP 429 rate-limit responses.
const MAX_RATE_LIMIT_RETRIES: u32 = 3;

/// Map a `reqwest::Error` from `send()` into an [`ActualError`].
///
/// The `is_timeout` predicate allows tests to inject the timeout classification
/// without requiring a real network roundtrip to produce a timed-out error.
fn map_send_error_with<F>(e: reqwest::Error, timeout: Duration, is_timeout: F) -> ActualError
where
    F: Fn(&reqwest::Error) -> bool,
{
    if is_timeout(&e) {
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

/// Map a `reqwest::Error` from `send()` into an [`ActualError`].
fn map_send_error(e: reqwest::Error, timeout: Duration) -> ActualError {
    map_send_error_with(e, timeout, |err| err.is_timeout())
}

/// Runner that uses the OpenAI Responses API with structured JSON output.
///
/// Implements [`TailoringRunner`] by calling `POST /v1/responses` with a
/// JSON schema constraint, so the model always returns well-formed
/// [`TailoringOutput`] JSON without tool calls or multi-turn agentic loops.
#[derive(Debug)]
pub struct OpenAiApiRunner {
    api_key: String,
    model: String,
    client: reqwest::Client,
    timeout: Duration,
    /// Base URL for the OpenAI API (overridable for tests via mockito).
    base_url: String,
    /// Base duration for exponential back-off on 429 responses.
    /// Defaults to 1 second; tests override this to zero for speed.
    retry_base: Duration,
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

/// Map a reqwest client build error into an [`ActualError`].
fn map_client_build_error(e: reqwest::Error) -> ActualError {
    ActualError::ClaudeSubprocessFailed {
        message: format!("Failed to initialize HTTP client: {e}"),
        stderr: String::new(),
    }
}

impl OpenAiApiRunner {
    /// Create a new runner with an explicit API key and model.
    pub fn new(api_key: String, model: String, timeout: Duration) -> Result<Self, ActualError> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .https_only(true)
            .build()
            .map_err(map_client_build_error)?;
        Ok(Self {
            api_key,
            model,
            client,
            timeout,
            base_url: "https://api.openai.com".to_string(),
            retry_base: Duration::from_secs(1),
        })
    }

    /// Override the base URL (used in tests to point at a mockito server).
    ///
    /// Rebuilds the inner `reqwest::Client` with `https_only` disabled when the
    /// URL is a loopback address so that mock servers work in tests.
    #[cfg(test)]
    fn with_base_url(mut self, base_url: String) -> Self {
        let is_localhost = base_url.starts_with("http://localhost")
            || base_url.starts_with("http://127.0.0.1")
            || base_url.starts_with("http://[::1]");
        if is_localhost {
            self.client = reqwest::Client::builder()
                .timeout(self.timeout)
                .https_only(false)
                .build()
                .expect("failed to build reqwest client for test");
        }
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

        let mut attempt = 0u32;
        let api_response: ApiResponse = loop {
            let response = self
                .client
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await
                .map_err(|e| map_send_error(e, self.timeout))?;

            let status = response.status();

            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
            {
                return Err(ActualError::ClaudeNotAuthenticated);
            }

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                attempt += 1;
                if attempt > MAX_RATE_LIMIT_RETRIES {
                    return Err(ActualError::ClaudeSubprocessFailed {
                        message: format!(
                            "OpenAI API rate limited after {MAX_RATE_LIMIT_RETRIES} retries"
                        ),
                        stderr: String::new(),
                    });
                }
                // Respect Retry-After header if present, else use exponential backoff.
                let wait_secs =
                    parse_retry_after(response.headers()).unwrap_or_else(|| 1u64 << (attempt - 1)); // 1s, 2s, 4s
                let wait_secs = wait_secs.min(60);
                tracing::warn!(
                    "OpenAI API rate limited, waiting {}s before retry {}/{}",
                    wait_secs,
                    attempt,
                    MAX_RATE_LIMIT_RETRIES
                );
                tokio::time::sleep(self.retry_base * wait_secs as u32).await;
                continue;
            }

            if status.is_server_error() {
                let body_bytes = response
                    .bytes()
                    .await
                    .unwrap_or_else(|e| format!("<body read error: {e}>").into_bytes().into());
                let truncated = &body_bytes[..body_bytes.len().min(4096)];
                let body_text = String::from_utf8_lossy(truncated).into_owned();
                return Err(ActualError::ClaudeSubprocessFailed {
                    message: format!("OpenAI API error: {status}"),
                    stderr: body_text,
                });
            }

            break response.json().await.map_err(|e| {
                // reqwest JSON parse failure wraps serde_json; surface as ClaudeSubprocessFailed
                // with the original message since we can't recover the raw bytes at this point.
                ActualError::ClaudeSubprocessFailed {
                    message: format!("Failed to parse OpenAI API response: {e}"),
                    stderr: String::new(),
                }
            })?;
        };

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

/// Parse the `Retry-After` header value into a number of seconds.
///
/// Returns `Some(seconds)` if the header is present and contains a valid
/// non-negative integer.  Returns `None` if the header is absent, non-UTF-8,
/// or not an integer (e.g., an HTTP-date string).
fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let value = headers.get("retry-after")?.to_str().ok()?;
    value.trim().parse::<u64>().ok()
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
        let mut runner = OpenAiApiRunner::new(
            "test-key".to_string(),
            "gpt-4o".to_string(),
            Duration::from_secs(10),
        )
        .expect("failed to build test runner")
        .with_base_url(server.url());
        // Use zero-duration back-off so 429 retry tests run without real sleeps.
        runner.retry_base = Duration::ZERO;
        runner
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

    // Test 3: HTTP 429 exhausts all retries and returns ClaudeSubprocessFailed
    #[tokio::test]
    async fn test_429_exhausts_retries() {
        let mut server = Server::new_async().await;

        // Return 429 on every attempt: 1 initial + MAX_RATE_LIMIT_RETRIES retries = 4 total.
        let mock = server
            .mock("POST", "/v1/responses")
            .with_status(429)
            .with_body(r#"{"error": "Too Many Requests"}"#)
            .expect(4)
            .create_async()
            .await;

        let runner = make_runner(&server);
        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await;

        mock.assert_async().await;
        match result {
            Err(ActualError::ClaudeSubprocessFailed { message, .. }) => {
                assert!(
                    message.contains("rate"),
                    "expected 'rate' in message, got: {message}"
                );
            }
            other => panic!("expected ClaudeSubprocessFailed, got: {:?}", other),
        }
    }

    // Test 3b: 429 retries then succeeds on a subsequent 200
    #[tokio::test]
    async fn test_429_retries_and_succeeds() {
        let mut server = Server::new_async().await;

        // Return 429 three times, then a valid 200.
        let mock_429 = server
            .mock("POST", "/v1/responses")
            .with_status(429)
            .with_body(r#"{"error": "Too Many Requests"}"#)
            .expect(3)
            .create_async()
            .await;

        let inner = minimal_tailoring_output_json();
        let body = responses_body(&inner);
        let mock_200 = server
            .mock("POST", "/v1/responses")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&body)
            .create_async()
            .await;

        let runner = make_runner(&server);
        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await;

        mock_429.assert_async().await;
        mock_200.assert_async().await;
        assert!(
            result.is_ok(),
            "expected Ok after retrying, got: {:?}",
            result
        );
    }

    // Test 3c: 429 with Retry-After header is respected, then succeeds
    #[tokio::test]
    async fn test_429_respects_retry_after_header() {
        let mut server = Server::new_async().await;

        let mock_429 = server
            .mock("POST", "/v1/responses")
            .with_status(429)
            .with_header("retry-after", "5")
            .with_body(r#"{"error": "Too Many Requests"}"#)
            .expect(1)
            .create_async()
            .await;

        let inner = minimal_tailoring_output_json();
        let body = responses_body(&inner);
        let mock_200 = server
            .mock("POST", "/v1/responses")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&body)
            .create_async()
            .await;

        let runner = make_runner(&server);
        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await;

        mock_429.assert_async().await;
        mock_200.assert_async().await;
        assert!(
            result.is_ok(),
            "expected Ok after Retry-After wait, got: {:?}",
            result
        );
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

    // Test 6: new() constructs successfully with default config
    #[test]
    fn test_new_constructs_successfully() {
        let result = OpenAiApiRunner::new(
            "test-key".to_string(),
            "gpt-4o".to_string(),
            Duration::from_secs(10),
        );
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
    }

    // Test 6b: map_client_build_error formats the error message correctly
    #[tokio::test]
    async fn test_map_client_build_error_formats_message() {
        // Obtain a reqwest::Error from a real failed network operation so we can
        // pass it to map_client_build_error and verify the output format.
        let client = reqwest::Client::builder().build().unwrap();
        let err = client
            .get("http://127.0.0.1:1/") // port 1 is reserved, always refused
            .send()
            .await
            .unwrap_err();

        let mapped = map_client_build_error(err);
        match mapped {
            ActualError::ClaudeSubprocessFailed { message, stderr } => {
                assert!(
                    message.contains("Failed to initialize HTTP client"),
                    "expected 'Failed to initialize HTTP client' prefix in: {message}"
                );
                assert!(stderr.is_empty(), "expected empty stderr, got: {stderr}");
            }
            other => panic!("expected ClaudeSubprocessFailed, got: {:?}", other),
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
        .expect("failed to build test runner")
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

    // Test 18a: map_send_error_with — non-timeout classifier → ClaudeSubprocessFailed
    #[tokio::test]
    async fn test_map_send_error_non_timeout() {
        // Connection refused produces a reliable network error on loopback.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let err = client
            .get("http://127.0.0.1:1/test") // port 1 always refused
            .send()
            .await
            .unwrap_err();

        // Force is_timeout → false so we always hit the ClaudeSubprocessFailed branch.
        let mapped = map_send_error_with(err, Duration::from_secs(10), |_| false);
        match mapped {
            ActualError::ClaudeSubprocessFailed { message, stderr } => {
                assert!(
                    message.contains("OpenAI API request failed"),
                    "expected request-failed prefix in: {message}"
                );
                assert!(stderr.is_empty(), "expected empty stderr, got: {stderr}");
            }
            other => panic!("expected ClaudeSubprocessFailed, got: {:?}", other),
        }
    }

    // Test 18b: map_send_error_with — timeout classifier → ClaudeTimeout
    #[tokio::test]
    async fn test_map_send_error_timeout_branch() {
        // Connection refused produces a reliable network error on loopback.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let err = client
            .get("http://127.0.0.1:1/test") // port 1 always refused
            .send()
            .await
            .unwrap_err();

        // Force is_timeout → true to exercise the ClaudeTimeout branch regardless
        // of how the OS classifies the underlying network error.
        let mapped = map_send_error_with(err, Duration::from_secs(30), |_| true);
        match mapped {
            ActualError::ClaudeTimeout { seconds } => {
                assert_eq!(seconds, 30, "expected timeout duration forwarded");
            }
            other => panic!("expected ClaudeTimeout, got: {:?}", other),
        }
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
        .expect("failed to build test runner")
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

    // Test 20: with_base_url for a non-localhost (https) URL does not rebuild the client.
    //
    // This exercises the `else` branch of `if is_localhost` in `with_base_url`,
    // ensuring the existing https_only client is retained when the base URL is
    // already HTTPS.  We confirm the base URL is set and the runner can be
    // constructed without panicking; we do not make a real network request.
    #[test]
    fn test_with_base_url_non_localhost_retains_client() {
        let runner = OpenAiApiRunner::new(
            "test-key".to_string(),
            "gpt-4o".to_string(),
            Duration::from_secs(10),
        )
        .expect("failed to build test runner")
        .with_base_url("https://api.example.com".to_string());

        assert_eq!(runner.base_url, "https://api.example.com");
    }

    // Test 21: large server error body is truncated to 4096 bytes.
    #[tokio::test]
    async fn test_500_large_body_is_truncated() {
        let mut server = Server::new_async().await;

        // Body larger than 4096 bytes.
        let large_body = "X".repeat(8192);

        let mock = server
            .mock("POST", "/v1/responses")
            .with_status(500)
            .with_body(&large_body)
            .create_async()
            .await;

        let runner = make_runner(&server);
        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await;

        match result {
            Err(ActualError::ClaudeSubprocessFailed { stderr, .. }) => {
                assert_eq!(
                    stderr.len(),
                    4096,
                    "expected error body truncated to 4096 bytes, got {} bytes",
                    stderr.len()
                );
            }
            other => panic!("expected ClaudeSubprocessFailed, got: {:?}", other),
        }

        mock.assert_async().await;
    }

    // Tests for parse_retry_after helper
    #[test]
    fn test_parse_retry_after_integer() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "retry-after",
            reqwest::header::HeaderValue::from_static("30"),
        );
        assert_eq!(parse_retry_after(&headers), Some(30));
    }

    #[test]
    fn test_parse_retry_after_missing() {
        let headers = reqwest::header::HeaderMap::new();
        assert_eq!(parse_retry_after(&headers), None);
    }

    #[test]
    fn test_parse_retry_after_invalid() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "retry-after",
            reqwest::header::HeaderValue::from_static("not-a-number"),
        );
        assert_eq!(parse_retry_after(&headers), None);
    }
}
