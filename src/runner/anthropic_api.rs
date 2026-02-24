use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::sleep;

use crate::error::ActualError;
use crate::tailoring::types::TailoringOutput;

use super::subprocess::TailoringRunner;

/// The production Anthropic Messages API endpoint.
const ANTHROPIC_API_BASE: &str = "https://api.anthropic.com";

/// Maximum number of retries on HTTP 429 rate-limit responses.
const MAX_RATE_LIMIT_RETRIES: u32 = 3;

/// Direct HTTP runner that calls the Anthropic Messages API without spawning a subprocess.
///
/// Uses a forced tool call (`tool_choice: { type: "tool", name: "return_result" }`) to obtain
/// structured output in a single, non-agentic round-trip — no tool-call loop is required.
#[derive(Debug)]
pub struct AnthropicApiRunner {
    api_key: String,
    model: String,
    client: reqwest::Client,
    timeout: Duration,
    /// Base URL for the API, configurable for testing.
    base_url: String,
    /// Base duration for exponential back-off on 429 responses.
    /// Defaults to 1 second; tests override this to zero for speed.
    retry_base: Duration,
    /// Optional channel for streaming progress events to the TUI.
    event_tx: std::sync::Mutex<Option<UnboundedSender<String>>>,
}

/// Map a reqwest client build error into an [`ActualError`].
fn map_client_build_error(e: reqwest::Error) -> ActualError {
    ActualError::RunnerFailed {
        message: format!("Failed to initialize HTTP client: {e}"),
        stderr: String::new(),
    }
}

impl AnthropicApiRunner {
    /// Create a new runner with an explicit API key.
    pub fn new(api_key: String, model: String, timeout: Duration) -> Result<Self, ActualError> {
        Self::with_base_url(api_key, model, timeout, ANTHROPIC_API_BASE.to_string())
    }

    /// Create a runner with a custom base URL (used in tests to point at a mock server).
    pub(crate) fn with_base_url(
        api_key: String,
        model: String,
        timeout: Duration,
        base_url: String,
    ) -> Result<Self, ActualError> {
        let is_localhost = base_url.starts_with("http://localhost")
            || base_url.starts_with("http://127.0.0.1")
            || base_url.starts_with("http://[::1]");
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .https_only(!is_localhost)
            .build()
            .map_err(map_client_build_error)?;
        Ok(Self {
            api_key,
            model,
            client,
            timeout,
            base_url,
            retry_base: Duration::from_secs(1),
            event_tx: std::sync::Mutex::new(None),
        })
    }

    /// POST to the Anthropic Messages API and return the parsed JSON response body.
    ///
    /// On HTTP 429 rate-limit responses, retries up to [`MAX_RATE_LIMIT_RETRIES`] times
    /// with exponential backoff (1 s, 2 s, 4 s), respecting the `Retry-After` header
    /// when present.
    async fn post_messages(
        &self,
        body: Value,
        event_tx: Option<&UnboundedSender<String>>,
    ) -> Result<Value, ActualError> {
        let url = format!("{}/v1/messages", self.base_url);

        let mut attempt = 0u32;
        loop {
            let response = self
                .client
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| {
                    if e.is_timeout() {
                        ActualError::RunnerTimeout {
                            seconds: self.timeout.as_secs(),
                        }
                    } else {
                        ActualError::RunnerFailed {
                            message: format!("HTTP request failed: {e}"),
                            stderr: String::new(),
                        }
                    }
                })?;

            let status = response.status();

            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
            {
                return Err(ActualError::ApiKeyMissing {
                    env_var: "ANTHROPIC_API_KEY".to_string(),
                });
            }

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                attempt += 1;
                if attempt > MAX_RATE_LIMIT_RETRIES {
                    return Err(ActualError::RunnerFailed {
                        message: format!(
                            "Anthropic API rate limited after {MAX_RATE_LIMIT_RETRIES} retries"
                        ),
                        stderr: String::new(),
                    });
                }
                // Respect Retry-After header if present, else use exponential backoff.
                let wait_secs =
                    parse_retry_after(response.headers()).unwrap_or_else(|| 1u64 << (attempt - 1)); // 1s, 2s, 4s
                let wait_secs = wait_secs.min(60);
                tracing::warn!(
                    "Anthropic API rate limited, waiting {}s before retry {}/{}",
                    wait_secs,
                    attempt,
                    MAX_RATE_LIMIT_RETRIES
                );
                if let Some(tx) = event_tx {
                    let _ = tx.send(format!(
                        "Rate limited — retrying in {wait_secs}s ({attempt}/{MAX_RATE_LIMIT_RETRIES})..."
                    ));
                }
                sleep(self.retry_base * wait_secs as u32).await;
                continue;
            }

            // HTTP 400 (Bad Request) or 529 (overloaded) may indicate credit limit reached.
            // Parse the body and check for the "credit_limit_reached" error type.
            if status == reqwest::StatusCode::BAD_REQUEST || status.as_u16() == 529 {
                let body_bytes = response
                    .bytes()
                    .await
                    .unwrap_or_else(|e| format!("<body read error: {e}>").into_bytes().into());
                let truncated = &body_bytes[..body_bytes.len().min(4096)];
                let body_str = String::from_utf8_lossy(truncated).into_owned();
                // Check if this is a credit limit error.
                if let Ok(json) = serde_json::from_str::<Value>(&body_str) {
                    let error_type = json
                        .get("error")
                        .and_then(|e| e.get("type"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if error_type == "credit_limit_reached" {
                        return Err(ActualError::CreditBalanceTooLow {
                            message: "Anthropic API credit limit reached".to_string(),
                        });
                    }
                }
                tracing::debug!("Anthropic API error body: {body_str}");
                return Err(ActualError::RunnerFailed {
                    message: format!("Anthropic API error: {status}"),
                    stderr: body_str,
                });
            }

            if status.is_server_error() {
                let body_bytes = response
                    .bytes()
                    .await
                    .unwrap_or_else(|e| format!("<body read error: {e}>").into_bytes().into());
                let truncated = &body_bytes[..body_bytes.len().min(4096)];
                let body = String::from_utf8_lossy(truncated).into_owned();
                return Err(ActualError::RunnerFailed {
                    message: format!("Anthropic API error: {status}"),
                    stderr: body,
                });
            }

            let json: Value = response
                .json()
                .await
                .map_err(|e| ActualError::RunnerFailed {
                    message: format!("Failed to parse Anthropic API response: {e}"),
                    stderr: String::new(),
                })?;

            return Ok(json);
        }
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

/// Intermediate representation for a content block in the Anthropic response.
#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    name: Option<String>,
    input: Option<Value>,
}

impl TailoringRunner for AnthropicApiRunner {
    fn set_event_tx(&self, tx: UnboundedSender<String>) {
        *self.event_tx.lock().unwrap() = Some(tx);
    }

    async fn run_tailoring(
        &self,
        prompt: &str,
        schema: &str,
        model_override: Option<&str>,
        _max_budget_usd: Option<f64>,
    ) -> Result<TailoringOutput, ActualError> {
        let event_tx = self.event_tx.lock().unwrap().clone();
        let model = model_override.unwrap_or(&self.model);
        let schema_value: Value = serde_json::from_str(schema)?;

        if let Some(ref tx) = event_tx {
            let _ = tx.send(format!(
                "Sending request to Anthropic API (model: {model})..."
            ));
        }

        let system_prompt = "You are an expert software architect. Your task is to analyze \
            Architecture Decision Records (ADRs) and generate tailored CLAUDE.md files for a \
            repository. You must call the `return_result` tool with your structured output. \
            Be precise and generate accurate, applicable guidance.";

        let request_body = serde_json::json!({
            "model": model,
            "max_tokens": 8192,
            "system": system_prompt,
            "messages": [
                {
                    "role": "user",
                    "content": prompt
                }
            ],
            "tools": [
                {
                    "name": "return_result",
                    "description": "Return the structured tailoring result",
                    "input_schema": schema_value
                }
            ],
            "tool_choice": {
                "type": "tool",
                "name": "return_result"
            }
        });

        let response = self.post_messages(request_body, event_tx.as_ref()).await?;

        if let Some(ref tx) = event_tx {
            let _ = tx.send("Response received from Anthropic API".to_string());
        }

        // Find the tool_use block named "return_result" in the content array.
        // Propagate deserialization failures so API schema drift produces a
        // specific, actionable error rather than a confusing "no structured result".
        let content_blocks: Vec<ContentBlock> = match response.get("content") {
            Some(c) => {
                serde_json::from_value(c.clone()).map_err(|e| ActualError::RunnerFailed {
                    message: format!("Failed to parse API response content blocks: {e}"),
                    stderr: String::new(),
                })?
            }
            None => Vec::new(),
        };

        let tool_input = content_blocks
            .into_iter()
            .find(|block| {
                block.block_type == "tool_use" && block.name.as_deref() == Some("return_result")
            })
            .and_then(|block| block.input)
            .ok_or_else(|| ActualError::RunnerFailed {
                message: "Anthropic API did not return structured result".to_string(),
                stderr: String::new(),
            })?;

        let output: TailoringOutput = serde_json::from_value(tool_input)?;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    /// Create a runner pointed at `server_url` instead of the production API.
    /// Sets `retry_base` to zero so 429 retry tests run without real sleeps.
    fn make_runner(server_url: &str) -> AnthropicApiRunner {
        let mut runner = AnthropicApiRunner::with_base_url(
            "test-key".to_string(),
            "claude-sonnet-4-6".to_string(),
            Duration::from_secs(10),
            server_url.to_string(),
        )
        .expect("failed to build test runner");
        runner.retry_base = Duration::ZERO;
        runner
    }

    /// Build a minimal valid TailoringOutput JSON value.
    fn tailoring_output_json() -> Value {
        serde_json::json!({
            "files": [
                {
                    "path": "CLAUDE.md",
                    "sections": [
                        {
                            "adr_id": "adr-001",
                            "content": "# Rules"
                        }
                    ],
                    "reasoning": "General rules"
                }
            ],
            "skipped_adrs": [],
            "summary": {
                "total_input": 1,
                "applicable": 1,
                "not_applicable": 0,
                "files_generated": 1
            }
        })
    }

    /// Build the Anthropic response envelope wrapping a tool_use block.
    fn anthropic_tool_use_response(input: Value) -> Value {
        serde_json::json!({
            "id": "msg_01XFDUDYJgAACzvnptvVoYEL",
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "tool_use",
                    "id": "toolu_01A09q90qw90lq917835lq9",
                    "name": "return_result",
                    "input": input
                }
            ],
            "model": "claude-sonnet-4-6",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 100, "output_tokens": 200}
        })
    }

    // Test 1: valid response is parsed correctly
    #[tokio::test]
    async fn test_valid_response_parsed_correctly() {
        let mut server = Server::new_async().await;
        let response_body = anthropic_tool_use_response(tailoring_output_json());

        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(response_body.to_string())
            .create_async()
            .await;

        let runner = make_runner(&server.url());
        let schema = r#"{"type":"object"}"#;
        let result = runner
            .run_tailoring("Test prompt", schema, None, None)
            .await;

        mock.assert_async().await;
        let output = result.expect("expected Ok");
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "CLAUDE.md");
        assert_eq!(output.summary.total_input, 1);
    }

    // Test 2: 401 maps to ApiKeyMissing with ANTHROPIC_API_KEY
    #[tokio::test]
    async fn test_401_maps_to_api_key_missing() {
        let mut server = Server::new_async().await;

        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(401)
            .with_body(r#"{"error": {"type": "authentication_error"}}"#)
            .create_async()
            .await;

        let runner = make_runner(&server.url());
        let schema = r#"{"type":"object"}"#;
        let result = runner.run_tailoring("prompt", schema, None, None).await;

        mock.assert_async().await;
        match result {
            Err(ActualError::ApiKeyMissing { env_var }) => {
                assert_eq!(
                    env_var, "ANTHROPIC_API_KEY",
                    "expected ANTHROPIC_API_KEY, got: {env_var}"
                );
            }
            other => panic!("expected ApiKeyMissing, got: {:?}", other),
        }
    }

    // Test 3: 429 exhausts all retries and returns RunnerFailed
    #[tokio::test]
    async fn test_429_exhausts_retries() {
        let mut server = Server::new_async().await;

        // Return 429 on every attempt: 1 initial + MAX_RATE_LIMIT_RETRIES retries = 4 total.
        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(429)
            .with_body(r#"{"error": {"type": "rate_limit_error"}}"#)
            .expect(4)
            .create_async()
            .await;

        let runner = make_runner(&server.url());
        let schema = r#"{"type":"object"}"#;
        let result = runner.run_tailoring("prompt", schema, None, None).await;

        mock.assert_async().await;
        match result {
            Err(ActualError::RunnerFailed { message, .. }) => {
                assert!(
                    message.contains("rate"),
                    "expected 'rate' in message: {message}"
                );
            }
            other => panic!("expected RunnerFailed, got: {:?}", other),
        }
    }

    // Test 3b: 429 retries then succeeds on a subsequent 200
    #[tokio::test]
    async fn test_429_retries_and_succeeds() {
        let mut server = Server::new_async().await;

        // Return 429 three times, then a valid 200.
        let mock_429 = server
            .mock("POST", "/v1/messages")
            .with_status(429)
            .with_body(r#"{"error": {"type": "rate_limit_error"}}"#)
            .expect(3)
            .create_async()
            .await;

        let response_body = anthropic_tool_use_response(tailoring_output_json());
        let mock_200 = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(response_body.to_string())
            .create_async()
            .await;

        let runner = make_runner(&server.url());
        let schema = r#"{"type":"object"}"#;
        let result = runner.run_tailoring("prompt", schema, None, None).await;

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
            .mock("POST", "/v1/messages")
            .with_status(429)
            .with_header("retry-after", "5")
            .with_body(r#"{"error": {"type": "rate_limit_error"}}"#)
            .expect(1)
            .create_async()
            .await;

        let response_body = anthropic_tool_use_response(tailoring_output_json());
        let mock_200 = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(response_body.to_string())
            .create_async()
            .await;

        let runner = make_runner(&server.url());
        let schema = r#"{"type":"object"}"#;
        let result = runner.run_tailoring("prompt", schema, None, None).await;

        mock_429.assert_async().await;
        mock_200.assert_async().await;
        assert!(
            result.is_ok(),
            "expected Ok after Retry-After wait, got: {:?}",
            result
        );
    }

    // Test 4: 500 maps to RunnerFailed
    #[tokio::test]
    async fn test_500_maps_to_subprocess_failed() {
        let mut server = Server::new_async().await;

        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let runner = make_runner(&server.url());
        let schema = r#"{"type":"object"}"#;
        let result = runner.run_tailoring("prompt", schema, None, None).await;

        mock.assert_async().await;
        assert!(
            matches!(result, Err(ActualError::RunnerFailed { .. })),
            "expected RunnerFailed, got: {:?}",
            result
        );
    }

    // Test 5: missing tool_use in response maps to RunnerFailed
    #[tokio::test]
    async fn test_missing_tool_use_maps_to_structured_result_error() {
        let mut server = Server::new_async().await;

        // Response with only text content, no tool_use
        let response_body = serde_json::json!({
            "id": "msg_01XFDUDYJgAACzvnptvVoYEL",
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "text",
                    "text": "Here is the result..."
                }
            ],
            "model": "claude-sonnet-4-6",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 100, "output_tokens": 50}
        });

        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(response_body.to_string())
            .create_async()
            .await;

        let runner = make_runner(&server.url());
        let schema = r#"{"type":"object"}"#;
        let result = runner.run_tailoring("prompt", schema, None, None).await;

        mock.assert_async().await;
        match result {
            Err(ActualError::RunnerFailed { message, .. }) => {
                assert!(
                    message.contains("structured result"),
                    "expected 'structured result' in message: {message}"
                );
            }
            other => panic!("expected RunnerFailed, got: {:?}", other),
        }
    }

    // Test 6: new() constructs successfully with default config
    #[test]
    fn test_new_constructs_successfully() {
        let result = AnthropicApiRunner::new(
            "sk-test-key".to_string(),
            "claude-sonnet-4-6".to_string(),
            Duration::from_secs(30),
        );
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
    }

    // Test 8: model_override is respected
    #[tokio::test]
    async fn test_model_override_is_used() {
        let mut server = Server::new_async().await;
        let response_body = anthropic_tool_use_response(tailoring_output_json());

        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(response_body.to_string())
            .create_async()
            .await;

        let runner = make_runner(&server.url());
        let schema = r#"{"type":"object"}"#;
        let result = runner
            .run_tailoring("prompt", schema, Some("claude-opus-4-6"), None)
            .await;

        mock.assert_async().await;
        assert!(result.is_ok(), "expected Ok with model override");
    }

    // Test 9: 403 maps to ApiKeyMissing with ANTHROPIC_API_KEY
    #[tokio::test]
    async fn test_403_maps_to_api_key_missing() {
        let mut server = Server::new_async().await;

        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(403)
            .with_body(r#"{"error": {"type": "permission_error"}}"#)
            .create_async()
            .await;

        let runner = make_runner(&server.url());
        let schema = r#"{"type":"object"}"#;
        let result = runner.run_tailoring("prompt", schema, None, None).await;

        mock.assert_async().await;
        match result {
            Err(ActualError::ApiKeyMissing { env_var }) => {
                assert_eq!(
                    env_var, "ANTHROPIC_API_KEY",
                    "expected ANTHROPIC_API_KEY, got: {env_var}"
                );
            }
            other => panic!("expected ApiKeyMissing, got: {:?}", other),
        }
    }

    // Test: HTTP 400 with credit_limit_reached maps to CreditBalanceTooLow
    #[tokio::test]
    async fn test_400_credit_limit_reached_maps_to_credit_balance_too_low() {
        let mut server = Server::new_async().await;

        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"error": {"type": "credit_limit_reached", "message": "Credit limit reached"}}"#,
            )
            .create_async()
            .await;

        let runner = make_runner(&server.url());
        let schema = r#"{"type":"object"}"#;
        let result = runner.run_tailoring("prompt", schema, None, None).await;

        mock.assert_async().await;
        assert!(
            matches!(result, Err(ActualError::CreditBalanceTooLow { .. })),
            "expected CreditBalanceTooLow, got: {:?}",
            result
        );
    }

    // Test: HTTP 400 without credit_limit_reached maps to RunnerFailed
    #[tokio::test]
    async fn test_400_without_credit_limit_maps_to_runner_failed() {
        let mut server = Server::new_async().await;

        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error": {"type": "invalid_request_error", "message": "Bad request"}}"#)
            .create_async()
            .await;

        let runner = make_runner(&server.url());
        let schema = r#"{"type":"object"}"#;
        let result = runner.run_tailoring("prompt", schema, None, None).await;

        mock.assert_async().await;
        assert!(
            matches!(result, Err(ActualError::RunnerFailed { .. })),
            "expected RunnerFailed, got: {:?}",
            result
        );
    }

    // Test: HTTP 529 with credit_limit_reached maps to CreditBalanceTooLow
    #[tokio::test]
    async fn test_529_credit_limit_reached_maps_to_credit_balance_too_low() {
        let mut server = Server::new_async().await;

        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(529)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"error": {"type": "credit_limit_reached", "message": "Credit limit reached"}}"#,
            )
            .create_async()
            .await;

        let runner = make_runner(&server.url());
        let schema = r#"{"type":"object"}"#;
        let result = runner.run_tailoring("prompt", schema, None, None).await;

        mock.assert_async().await;
        assert!(
            matches!(result, Err(ActualError::CreditBalanceTooLow { .. })),
            "expected CreditBalanceTooLow, got: {:?}",
            result
        );
    }

    // Test 10: invalid schema JSON maps to RunnerOutputParse
    #[tokio::test]
    async fn test_invalid_schema_json_maps_to_parse_error() {
        let server = Server::new_async().await;

        // No mock needed — the error happens before the HTTP call.
        let runner = make_runner(&server.url());
        let result = runner
            .run_tailoring("prompt", "NOT VALID JSON", None, None)
            .await;

        assert!(
            matches!(result, Err(ActualError::RunnerOutputParse(_))),
            "expected RunnerOutputParse, got: {:?}",
            result
        );
    }

    // Test 11: 200 response with non-JSON body maps to RunnerFailed
    #[tokio::test]
    async fn test_200_non_json_body_maps_to_subprocess_failed() {
        let mut server = Server::new_async().await;

        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "text/plain")
            .with_body("this is not json")
            .create_async()
            .await;

        let runner = make_runner(&server.url());
        let schema = r#"{"type":"object"}"#;
        let result = runner.run_tailoring("prompt", schema, None, None).await;

        mock.assert_async().await;
        assert!(
            matches!(result, Err(ActualError::RunnerFailed { .. })),
            "expected RunnerFailed for non-JSON 200 body, got: {:?}",
            result
        );
    }

    // Test 12: tool_use input that doesn't match TailoringOutput schema maps to RunnerOutputParse
    #[tokio::test]
    async fn test_invalid_tool_use_input_maps_to_parse_error() {
        let mut server = Server::new_async().await;

        // Return a tool_use block whose `input` cannot be deserialized as TailoringOutput.
        let bad_input = serde_json::json!({ "unexpected_field": 42 });
        let response_body = anthropic_tool_use_response(bad_input);

        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(response_body.to_string())
            .create_async()
            .await;

        let runner = make_runner(&server.url());
        let schema = r#"{"type":"object"}"#;
        let result = runner.run_tailoring("prompt", schema, None, None).await;

        mock.assert_async().await;
        assert!(
            matches!(result, Err(ActualError::RunnerOutputParse(_))),
            "expected RunnerOutputParse for bad tool input, got: {:?}",
            result
        );
    }

    // Test 13: connection refused maps to RunnerFailed
    #[tokio::test]
    async fn test_connection_refused_maps_to_subprocess_failed() {
        // Point at a port that is not listening so reqwest returns a connection error.
        let runner = make_runner("http://127.0.0.1:1");
        let schema = r#"{"type":"object"}"#;
        let result = runner.run_tailoring("prompt", schema, None, None).await;

        assert!(
            matches!(result, Err(ActualError::RunnerFailed { .. })),
            "expected RunnerFailed for connection refused, got: {:?}",
            result
        );
    }

    // Test 14: request timeout maps to RunnerTimeout
    #[tokio::test]
    async fn test_request_timeout_maps_to_claude_timeout() {
        // Bind a TCP listener that accepts connections but never sends data,
        // so the request hangs until our very short timeout fires.
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind failed");
        let port = listener.local_addr().unwrap().port();

        // Spawn a thread that accepts the connection and sits on it so the HTTP
        // request times out waiting for the response headers.
        std::thread::spawn(move || {
            // Accept one connection then drop it after a brief delay.
            if let Ok((_stream, _)) = listener.accept() {
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        });

        // Use a 1 ms timeout — the request will time out before headers arrive.
        let runner = AnthropicApiRunner::with_base_url(
            "test-key".to_string(),
            "claude-sonnet-4-6".to_string(),
            Duration::from_millis(1),
            format!("http://127.0.0.1:{port}"),
        )
        .expect("failed to build test runner");

        let schema = r#"{"type":"object"}"#;
        let result = runner.run_tailoring("prompt", schema, None, None).await;

        assert!(
            matches!(result, Err(ActualError::RunnerTimeout { .. })),
            "expected RunnerTimeout, got: {:?}",
            result
        );
    }

    // Test 15: response with no "content" field → empty Vec → no tool_use → structured result error
    #[tokio::test]
    async fn test_response_without_content_field_maps_to_structured_result_error() {
        let mut server = Server::new_async().await;

        // Return a response that has no "content" key at all.
        let response_body = serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-6",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });

        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(response_body.to_string())
            .create_async()
            .await;

        let runner = make_runner(&server.url());
        let schema = r#"{"type":"object"}"#;
        let result = runner.run_tailoring("prompt", schema, None, None).await;

        mock.assert_async().await;
        match result {
            Err(ActualError::RunnerFailed { message, .. }) => {
                assert!(message.contains("structured result"));
            }
            other => panic!("expected RunnerFailed, got: {:?}", other),
        }
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

    // Test 7: map_client_build_error formats the error message correctly
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
            ActualError::RunnerFailed { message, stderr } => {
                assert!(
                    message.contains("Failed to initialize HTTP client"),
                    "expected 'Failed to initialize HTTP client' prefix in: {message}"
                );
                assert!(stderr.is_empty(), "expected empty stderr, got: {stderr}");
            }
            other => panic!("expected RunnerFailed, got: {:?}", other),
        }
    }

    // Test 16: content blocks deserialization failure maps to RunnerFailed
    #[tokio::test]
    async fn test_malformed_content_blocks_maps_to_subprocess_failed() {
        let mut server = Server::new_async().await;

        // Return a response where "content" is a string, not an array of content blocks.
        // This makes serde_json::from_value::<Vec<ContentBlock>> fail.
        let response_body = serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "content": "this is a string, not an array",
            "model": "claude-sonnet-4-6",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });

        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(response_body.to_string())
            .create_async()
            .await;

        let runner = make_runner(&server.url());
        let schema = r#"{"type":"object"}"#;
        let result = runner.run_tailoring("prompt", schema, None, None).await;

        mock.assert_async().await;
        match result {
            Err(ActualError::RunnerFailed { message, .. }) => {
                assert!(
                    message.contains("Failed to parse API response content blocks"),
                    "expected content block parse error in message, got: {message}"
                );
            }
            other => panic!(
                "expected RunnerFailed for malformed content, got: {:?}",
                other
            ),
        }
    }

    // Test N: set_event_tx stores the sender and events are forwarded during run_tailoring
    #[tokio::test]
    async fn test_set_event_tx_stores_sender_and_forwards_events() {
        let mut server = Server::new_async().await;
        let response_body = anthropic_tool_use_response(tailoring_output_json());

        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(response_body.to_string())
            .create_async()
            .await;

        let runner = make_runner(&server.url());
        assert!(
            runner.event_tx.lock().unwrap().is_none(),
            "event_tx should be None before set_event_tx"
        );

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        runner.set_event_tx(tx);
        assert!(
            runner.event_tx.lock().unwrap().is_some(),
            "event_tx should be Some after set_event_tx"
        );

        let result = runner
            .run_tailoring("Test prompt", r#"{"type":"object"}"#, None, None)
            .await;

        mock.assert_async().await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);

        // Collect all events emitted during run_tailoring.
        let mut events = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            events.push(msg);
        }

        assert!(
            events
                .iter()
                .any(|e| e.contains("Sending request") && e.contains("Anthropic API")),
            "expected a 'Sending request' event, got: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| e.contains("Response received from Anthropic API")),
            "expected a 'Response received' event, got: {events:?}"
        );
    }

    // Test N+1: set_event_tx emits retry event on 429 then success
    #[tokio::test]
    async fn test_event_tx_emits_retry_event_on_429() {
        let mut server = Server::new_async().await;

        let mock_429 = server
            .mock("POST", "/v1/messages")
            .with_status(429)
            .with_body(r#"{"error": {"type": "rate_limit_error"}}"#)
            .expect(1)
            .create_async()
            .await;

        let response_body = anthropic_tool_use_response(tailoring_output_json());
        let mock_200 = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(response_body.to_string())
            .create_async()
            .await;

        let runner = make_runner(&server.url());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        runner.set_event_tx(tx);

        let result = runner
            .run_tailoring("prompt", r#"{"type":"object"}"#, None, None)
            .await;

        mock_429.assert_async().await;
        mock_200.assert_async().await;
        assert!(result.is_ok(), "expected Ok after retry, got: {:?}", result);

        let mut events = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            events.push(msg);
        }

        assert!(
            events.iter().any(|e| e.contains("Rate limited")),
            "expected a 'Rate limited' retry event, got: {events:?}"
        );
    }
}
