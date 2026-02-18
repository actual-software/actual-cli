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
        let client = reqwest::Client::builder()
            .timeout(timeout)
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
            let body = response.text().await.unwrap_or_else(|_| String::new());
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
        let content_blocks: Vec<ContentBlock> = response
            .get("content")
            .and_then(|c| serde_json::from_value(c.clone()).ok())
            .unwrap_or_default();

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

    // Test 6: from_env returns Err when ANTHROPIC_API_KEY not set
    #[test]
    fn test_from_env_missing_key() {
        // Remove the env var to ensure it's not set
        std::env::remove_var("ANTHROPIC_API_KEY");

        let result =
            AnthropicApiRunner::from_env("claude-sonnet-4-5".to_string(), Duration::from_secs(30));

        assert!(
            matches!(result, Err(ActualError::ClaudeNotAuthenticated)),
            "expected ClaudeNotAuthenticated, got: {:?}",
            result
        );
    }

    // Test 7: from_env succeeds when ANTHROPIC_API_KEY is set
    #[test]
    fn test_from_env_with_key() {
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
}
