use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::error::ActualError;
use crate::tailoring::types::TailoringOutput;

use super::subprocess::TailoringRunner;

/// The production Anthropic Messages API endpoint.
const ANTHROPIC_API_BASE: &str = "https://api.anthropic.com";

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
}

impl AnthropicApiRunner {
    /// Create a new runner with an explicit API key.
    pub fn new(api_key: String, model: String, timeout: Duration) -> Self {
        Self::with_base_url(api_key, model, timeout, ANTHROPIC_API_BASE.to_string())
    }

    /// Create a runner with a custom base URL (used in tests to point at a mock server).
    fn with_base_url(api_key: String, model: String, timeout: Duration, base_url: String) -> Self {
        let is_localhost = base_url.starts_with("http://localhost")
            || base_url.starts_with("http://127.0.0.1")
            || base_url.starts_with("http://[::1]");
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .https_only(!is_localhost)
            .build()
            .expect("failed to build reqwest client");
        Self {
            api_key,
            model,
            client,
            timeout,
            base_url,
        }
    }

    /// Create a runner by reading `ANTHROPIC_API_KEY` from the environment.
    ///
    /// Returns `Err(ActualError::ClaudeNotAuthenticated)` if the variable is not set.
    pub fn from_env(model: String, timeout: Duration) -> Result<Self, ActualError> {
        let api_key =
            std::env::var("ANTHROPIC_API_KEY").map_err(|_| ActualError::ClaudeNotAuthenticated)?;
        Ok(Self::new(api_key, model, timeout))
    }

    /// POST to the Anthropic Messages API and return the parsed JSON response body.
    async fn post_messages(&self, body: Value) -> Result<Value, ActualError> {
        let url = format!("{}/v1/messages", self.base_url);
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
                    ActualError::ClaudeTimeout {
                        seconds: self.timeout.as_secs(),
                    }
                } else {
                    ActualError::ClaudeSubprocessFailed {
                        message: format!("HTTP request failed: {e}"),
                        stderr: String::new(),
                    }
                }
            })?;

        let status = response.status();

        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ActualError::ClaudeNotAuthenticated);
        }

        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(ActualError::ClaudeSubprocessFailed {
                message: "Anthropic API rate limited".to_string(),
                stderr: String::new(),
            });
        }

        if status.is_server_error() {
            let body_bytes = response.bytes().await.unwrap_or_default();
            let truncated = &body_bytes[..body_bytes.len().min(4096)];
            let body = String::from_utf8_lossy(truncated).into_owned();
            return Err(ActualError::ClaudeSubprocessFailed {
                message: format!("Anthropic API error: {status}"),
                stderr: body,
            });
        }

        let json: Value =
            response
                .json()
                .await
                .map_err(|e| ActualError::ClaudeSubprocessFailed {
                    message: format!("Failed to parse Anthropic API response: {e}"),
                    stderr: String::new(),
                })?;

        Ok(json)
    }
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
    async fn run_tailoring(
        &self,
        prompt: &str,
        schema: &str,
        model_override: Option<&str>,
        _max_budget_usd: Option<f64>,
    ) -> Result<TailoringOutput, ActualError> {
        let model = model_override.unwrap_or(&self.model);
        let schema_value: Value = serde_json::from_str(schema)?;

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

        let response = self.post_messages(request_body).await?;

        // Find the tool_use block named "return_result" in the content array.
        // Propagate deserialization failures so API schema drift produces a
        // specific, actionable error rather than a confusing "no structured result".
        let content_blocks: Vec<ContentBlock> = match response.get("content") {
            Some(c) => serde_json::from_value(c.clone()).map_err(|e| {
                ActualError::ClaudeSubprocessFailed {
                    message: format!("Failed to parse API response content blocks: {e}"),
                    stderr: String::new(),
                }
            })?,
            None => Vec::new(),
        };

        let tool_input = content_blocks
            .into_iter()
            .find(|block| {
                block.block_type == "tool_use" && block.name.as_deref() == Some("return_result")
            })
            .and_then(|block| block.input)
            .ok_or_else(|| ActualError::ClaudeSubprocessFailed {
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
    fn make_runner(server_url: &str) -> AnthropicApiRunner {
        AnthropicApiRunner::with_base_url(
            "test-key".to_string(),
            "claude-sonnet-4-5".to_string(),
            Duration::from_secs(10),
            server_url.to_string(),
        )
    }

    /// Build a minimal valid TailoringOutput JSON value.
    fn tailoring_output_json() -> Value {
        serde_json::json!({
            "files": [
                {
                    "path": "CLAUDE.md",
                    "content": "# Rules",
                    "reasoning": "General rules",
                    "adr_ids": ["adr-001"]
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
            "model": "claude-sonnet-4-5",
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

    // Test 2: 401 maps to ClaudeNotAuthenticated
    #[tokio::test]
    async fn test_401_maps_to_not_authenticated() {
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
        assert!(
            matches!(result, Err(ActualError::ClaudeNotAuthenticated)),
            "expected ClaudeNotAuthenticated, got: {:?}",
            result
        );
    }

    // Test 3: 429 maps to ClaudeSubprocessFailed with rate limit message
    #[tokio::test]
    async fn test_429_maps_to_rate_limited() {
        let mut server = Server::new_async().await;

        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(429)
            .with_body(r#"{"error": {"type": "rate_limit_error"}}"#)
            .create_async()
            .await;

        let runner = make_runner(&server.url());
        let schema = r#"{"type":"object"}"#;
        let result = runner.run_tailoring("prompt", schema, None, None).await;

        mock.assert_async().await;
        match result {
            Err(ActualError::ClaudeSubprocessFailed { message, .. }) => {
                assert!(
                    message.contains("rate"),
                    "expected 'rate' in message: {message}"
                );
            }
            other => panic!("expected ClaudeSubprocessFailed, got: {:?}", other),
        }
    }

    // Test 4: 500 maps to ClaudeSubprocessFailed
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
            matches!(result, Err(ActualError::ClaudeSubprocessFailed { .. })),
            "expected ClaudeSubprocessFailed, got: {:?}",
            result
        );
    }

    // Test 5: missing tool_use in response maps to ClaudeSubprocessFailed
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
            "model": "claude-sonnet-4-5",
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
            Err(ActualError::ClaudeSubprocessFailed { message, .. }) => {
                assert!(
                    message.contains("structured result"),
                    "expected 'structured result' in message: {message}"
                );
            }
            other => panic!("expected ClaudeSubprocessFailed, got: {:?}", other),
        }
    }

    /// Mutex to serialize tests that mutate the `ANTHROPIC_API_KEY` environment variable.
    ///
    /// Rust tests run in parallel by default, so simultaneous mutations of the same
    /// environment variable cause flaky failures.  Holding this mutex during any test
    /// that reads or writes `ANTHROPIC_API_KEY` ensures they are ordered safely.
    static ENV_KEY_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // Test 6: from_env returns Err when ANTHROPIC_API_KEY not set
    #[test]
    fn test_from_env_missing_key() {
        let _guard = ENV_KEY_MUTEX.lock().unwrap();
        std::env::remove_var("ANTHROPIC_API_KEY");

        let result =
            AnthropicApiRunner::from_env("claude-sonnet-4-5".to_string(), Duration::from_secs(30));

        assert!(matches!(result, Err(ActualError::ClaudeNotAuthenticated)));
    }

    // Test 7: from_env succeeds when ANTHROPIC_API_KEY is set
    #[test]
    fn test_from_env_with_key() {
        let _guard = ENV_KEY_MUTEX.lock().unwrap();
        std::env::set_var("ANTHROPIC_API_KEY", "sk-test-key-12345");

        let result =
            AnthropicApiRunner::from_env("claude-sonnet-4-5".to_string(), Duration::from_secs(30));

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let runner = result.unwrap();
        assert_eq!(runner.api_key, "sk-test-key-12345");

        // Clean up
        std::env::remove_var("ANTHROPIC_API_KEY");
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
            .run_tailoring("prompt", schema, Some("claude-opus-4-5"), None)
            .await;

        mock.assert_async().await;
        assert!(result.is_ok(), "expected Ok with model override");
    }

    // Test 9: 403 maps to ClaudeNotAuthenticated
    #[tokio::test]
    async fn test_403_maps_to_not_authenticated() {
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
        assert!(
            matches!(result, Err(ActualError::ClaudeNotAuthenticated)),
            "expected ClaudeNotAuthenticated, got: {:?}",
            result
        );
    }

    // Test 10: invalid schema JSON maps to ClaudeOutputParse
    #[tokio::test]
    async fn test_invalid_schema_json_maps_to_parse_error() {
        let server = Server::new_async().await;

        // No mock needed — the error happens before the HTTP call.
        let runner = make_runner(&server.url());
        let result = runner
            .run_tailoring("prompt", "NOT VALID JSON", None, None)
            .await;

        assert!(
            matches!(result, Err(ActualError::ClaudeOutputParse(_))),
            "expected ClaudeOutputParse, got: {:?}",
            result
        );
    }

    // Test 11: 200 response with non-JSON body maps to ClaudeSubprocessFailed
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
            matches!(result, Err(ActualError::ClaudeSubprocessFailed { .. })),
            "expected ClaudeSubprocessFailed for non-JSON 200 body, got: {:?}",
            result
        );
    }

    // Test 12: tool_use input that doesn't match TailoringOutput schema maps to ClaudeOutputParse
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
            matches!(result, Err(ActualError::ClaudeOutputParse(_))),
            "expected ClaudeOutputParse for bad tool input, got: {:?}",
            result
        );
    }

    // Test 13: connection refused maps to ClaudeSubprocessFailed
    #[tokio::test]
    async fn test_connection_refused_maps_to_subprocess_failed() {
        // Point at a port that is not listening so reqwest returns a connection error.
        let runner = make_runner("http://127.0.0.1:1");
        let schema = r#"{"type":"object"}"#;
        let result = runner.run_tailoring("prompt", schema, None, None).await;

        assert!(
            matches!(result, Err(ActualError::ClaudeSubprocessFailed { .. })),
            "expected ClaudeSubprocessFailed for connection refused, got: {:?}",
            result
        );
    }

    // Test 14: request timeout maps to ClaudeTimeout
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
            "claude-sonnet-4-5".to_string(),
            Duration::from_millis(1),
            format!("http://127.0.0.1:{port}"),
        );

        let schema = r#"{"type":"object"}"#;
        let result = runner.run_tailoring("prompt", schema, None, None).await;

        assert!(
            matches!(result, Err(ActualError::ClaudeTimeout { .. })),
            "expected ClaudeTimeout, got: {:?}",
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
            "model": "claude-sonnet-4-5",
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
            Err(ActualError::ClaudeSubprocessFailed { message, .. }) => {
                assert!(message.contains("structured result"));
            }
            other => panic!("expected ClaudeSubprocessFailed, got: {:?}", other),
        }
    }

    // Test 16: content blocks deserialization failure maps to ClaudeSubprocessFailed
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
            "model": "claude-sonnet-4-5",
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
            Err(ActualError::ClaudeSubprocessFailed { message, .. }) => {
                assert!(
                    message.contains("Failed to parse API response content blocks"),
                    "expected content block parse error in message, got: {message}"
                );
            }
            other => panic!(
                "expected ClaudeSubprocessFailed for malformed content, got: {:?}",
                other
            ),
        }
    }
}
