/// Integration tests for AnthropicApiRunner and OpenAiApiRunner.
///
/// These tests exercise the full sync pipeline end-to-end using:
/// - A mockito HTTP server for the ADR matching API endpoint
/// - A second mockito HTTP server for the LLM API (Anthropic / OpenAI)
/// - `ANTHROPIC_API_BASE_URL` / `OPENAI_API_BASE_URL` env vars to redirect
///   the binary to the mock LLM server
/// - `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` env vars set to `test-key`
///
/// No real API calls are made.  No fake subprocess binary is needed for these
/// runners — the binary makes direct HTTP calls.
#[cfg(unix)]
mod common;

#[cfg(unix)]
mod tests {
    use super::common::*;
    use predicates::prelude::*;

    // ── Response builders ────────────────────────────────────────────────────

    /// Build a minimal valid [`TailoringOutput`] JSON value.
    fn minimal_tailoring_output() -> serde_json::Value {
        serde_json::json!({
            "files": [
                {
                    "path": "CLAUDE.md",
                    "sections": [
                        {
                            "adr_id": "adr-001",
                            "content": "# Tailored Rules\n\n- Always use Result"
                        }
                    ],
                    "reasoning": "Applied error handling ADR"
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

    /// Build the Anthropic Messages API response envelope wrapping a `tool_use`
    /// block that contains `tailoring_output` as the tool input.
    fn make_anthropic_response(tailoring_output: &serde_json::Value) -> String {
        serde_json::json!({
            "id": "msg_01XFDUDYJgAACzvnptvVoYEL",
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "tool_use",
                    "id": "toolu_01A09q90qw90lq917835lq9",
                    "name": "return_result",
                    "input": tailoring_output
                }
            ],
            "model": "claude-sonnet-4-6",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 100, "output_tokens": 200}
        })
        .to_string()
    }

    /// Build the OpenAI Responses API response envelope containing
    /// `tailoring_output_json` as the `output_text` content.
    fn make_openai_response(tailoring_output: &serde_json::Value) -> String {
        // The OpenAI runner expects the TailoringOutput as a JSON string inside
        // the `output_text` content item (not a nested object).
        let inner = tailoring_output.to_string();
        let escaped = serde_json::to_string(&inner).unwrap();
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

    // ── TestEnvHttp ──────────────────────────────────────────────────────────

    /// Test environment for HTTP runner integration tests.
    ///
    /// Holds a temporary directory (used as the project root) and a config file
    /// path pointing into it.  The LLM server URL is passed via env vars when
    /// building the command.
    struct TestEnvHttp {
        dir: tempfile::TempDir,
        config_path: std::path::PathBuf,
        adr_api_url: String,
        anthropic_url: String,
        openai_url: String,
    }

    impl TestEnvHttp {
        fn new(adr_server: &mockito::Server, llm_server: &mockito::Server) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let config_path = dir.path().join("config.yaml");
            Self {
                adr_api_url: adr_server.url(),
                anthropic_url: llm_server.url(),
                openai_url: llm_server.url(),
                dir,
                config_path,
            }
        }

        /// Build a command that invokes the binary with the Anthropic API runner.
        ///
        /// Sets:
        /// - `ANTHROPIC_API_KEY=test-key`
        /// - `ANTHROPIC_API_BASE_URL` → mock server URL
        /// - `ACTUAL_CONFIG` → temp config file
        /// - Clears `CLAUDE_BINARY` so the binary is not found (we must not
        ///   fall back to the ClaudeCli runner accidentally).
        fn cmd_anthropic(&self) -> assert_cmd::Command {
            let mut cmd = assert_cmd::Command::from(assert_cmd::cargo::cargo_bin_cmd!("actual"));
            cmd.env("ANTHROPIC_API_KEY", "test-key");
            cmd.env("ANTHROPIC_API_BASE_URL", &self.anthropic_url);
            cmd.env("ACTUAL_CONFIG", self.config_path.to_str().unwrap());
            // Ensure the ClaudeCli runner cannot be selected by accident.
            cmd.env("CLAUDE_BINARY", "/nonexistent/claude");
            cmd.current_dir(self.dir.path());
            cmd
        }

        /// Build a command that invokes the binary with the OpenAI API runner.
        fn cmd_openai(&self) -> assert_cmd::Command {
            let mut cmd = assert_cmd::Command::from(assert_cmd::cargo::cargo_bin_cmd!("actual"));
            cmd.env("OPENAI_API_KEY", "test-key");
            cmd.env("OPENAI_API_BASE_URL", &self.openai_url);
            cmd.env("ACTUAL_CONFIG", self.config_path.to_str().unwrap());
            cmd.env("CLAUDE_BINARY", "/nonexistent/claude");
            cmd.current_dir(self.dir.path());
            cmd
        }

        fn read_file(&self, path: &str) -> String {
            std::fs::read_to_string(self.dir.path().join(path)).unwrap()
        }

        fn file_exists(&self, path: &str) -> bool {
            self.dir.path().join(path).exists()
        }
    }

    // ── Anthropic tests ──────────────────────────────────────────────────────

    /// Test 1: Anthropic runner with `--no-tailor` creates CLAUDE.md from ADR
    /// policies without calling the LLM.
    #[test]
    fn anthropic_no_tailor_writes_claude_md() {
        let mut adr_server = mockito::Server::new();
        let llm_server = mockito::Server::new(); // no LLM call expected

        let adr = make_adr_json(
            "adr-001",
            "Error Handling",
            &["Always use Result<T, E>"],
            &[],
            &["."],
        );
        let response = make_match_response_json(&format!("[{}]", adr));
        adr_server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let env = TestEnvHttp::new(&adr_server, &llm_server);
        env.cmd_anthropic()
            .args([
                "sync",
                "--force",
                "--no-tailor",
                "--runner",
                "anthropic-api",
                "--api-url",
                &env.adr_api_url,
            ])
            .assert()
            .success();

        assert!(env.file_exists("CLAUDE.md"));
        let content = env.read_file("CLAUDE.md");
        assert!(content.contains("# Project Guidelines"));
        assert!(content.contains("Error Handling"));
        assert!(content.contains("Always use Result<T, E>"));
    }

    /// Test 2: Anthropic runner with tailoring writes the LLM-generated content.
    #[test]
    fn anthropic_with_tailoring_writes_tailored_content() {
        let mut adr_server = mockito::Server::new();
        let mut llm_server = mockito::Server::new();

        let adr = make_adr_json(
            "adr-001",
            "Error Handling",
            &["Always use Result<T, E>"],
            &[],
            &["."],
        );
        let response = make_match_response_json(&format!("[{}]", adr));
        adr_server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let tailoring_output = minimal_tailoring_output();
        let llm_body = make_anthropic_response(&tailoring_output);
        let _llm_mock = llm_server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&llm_body)
            .create();

        let env = TestEnvHttp::new(&adr_server, &llm_server);
        env.cmd_anthropic()
            .args([
                "sync",
                "--force",
                "--runner",
                "anthropic-api",
                "--api-url",
                &env.adr_api_url,
            ])
            .assert()
            .success();

        assert!(env.file_exists("CLAUDE.md"));
        let content = env.read_file("CLAUDE.md");
        assert!(content.contains("Tailored Rules"));
        assert!(content.contains("Always use Result"));
    }

    /// Test 3: Anthropic runner returns 401 → exit code 2 (not authenticated).
    #[test]
    fn anthropic_401_exits_code_2() {
        let mut adr_server = mockito::Server::new();
        let mut llm_server = mockito::Server::new();

        let adr = make_adr_json("adr-001", "Test ADR", &["policy"], &[], &["."]);
        let response = make_match_response_json(&format!("[{}]", adr));
        adr_server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        llm_server
            .mock("POST", "/v1/messages")
            .with_status(401)
            .with_body(r#"{"error": {"type": "authentication_error"}}"#)
            .create();

        let env = TestEnvHttp::new(&adr_server, &llm_server);
        env.cmd_anthropic()
            .args([
                "sync",
                "--force",
                "--runner",
                "anthropic-api",
                "--api-url",
                &env.adr_api_url,
            ])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("not authenticated"));
    }

    /// Test 4: Anthropic runner returns 500 → exit code 1 (runner failed).
    #[test]
    fn anthropic_500_exits_code_1() {
        let mut adr_server = mockito::Server::new();
        let mut llm_server = mockito::Server::new();

        let adr = make_adr_json("adr-001", "Test ADR", &["policy"], &[], &["."]);
        let response = make_match_response_json(&format!("[{}]", adr));
        adr_server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        llm_server
            .mock("POST", "/v1/messages")
            .with_status(500)
            .with_body("Internal Server Error")
            .create();

        let env = TestEnvHttp::new(&adr_server, &llm_server);
        env.cmd_anthropic()
            .args([
                "sync",
                "--force",
                "--runner",
                "anthropic-api",
                "--api-url",
                &env.adr_api_url,
            ])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("Runner failed"));
    }

    /// Test 5: Anthropic runner with missing API key exits with code 2.
    #[test]
    fn anthropic_missing_api_key_exits_with_error() {
        let mut adr_server = mockito::Server::new();
        let llm_server = mockito::Server::new();

        let adr = make_adr_json("adr-001", "Test ADR", &["policy"], &[], &["."]);
        let response = make_match_response_json(&format!("[{}]", adr));
        adr_server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let env = TestEnvHttp::new(&adr_server, &llm_server);
        let mut cmd = assert_cmd::Command::from(assert_cmd::cargo::cargo_bin_cmd!("actual"));
        cmd.env("ACTUAL_CONFIG", env.config_path.to_str().unwrap());
        cmd.env("CLAUDE_BINARY", "/nonexistent/claude");
        // Explicitly remove ANTHROPIC_API_KEY so the runner fails to find a key.
        cmd.env_remove("ANTHROPIC_API_KEY");
        cmd.current_dir(env.dir.path());
        cmd.args([
            "sync",
            "--force",
            "--runner",
            "anthropic-api",
            "--api-url",
            &env.adr_api_url,
        ])
        .assert()
        .code(2)
        .stderr(
            predicate::str::contains("API key not set")
                .or(predicate::str::contains("ANTHROPIC_API_KEY")),
        );
    }

    /// Test 11: Anthropic runner receives 429 then 200 → succeeds (retry behavior).
    ///
    /// We use `expect(4)` on the 429 mock (initial + 3 retries) then a 200.
    /// Since `retry_base` in the binary is 1 second, this test would be slow
    /// with the real backoff.  We work around this by sending the 429 only
    /// ONCE and then 200 on the second request, relying on the runner's
    /// retry-after logic when no `Retry-After` header is sent (falls back to
    /// exponential backoff starting at 1s).
    ///
    /// For integration speed we cap to a single retry: 1 × 429 then 200.
    #[test]
    fn anthropic_429_retries_and_succeeds() {
        let mut adr_server = mockito::Server::new();
        let mut llm_server = mockito::Server::new();

        let adr = make_adr_json("adr-001", "Test ADR", &["policy"], &[], &["."]);
        let response = make_match_response_json(&format!("[{}]", adr));
        adr_server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        // First request: 429 with Retry-After: 0 so the retry is immediate.
        let _mock_429 = llm_server
            .mock("POST", "/v1/messages")
            .with_status(429)
            .with_header("retry-after", "0")
            .with_body(r#"{"error": {"type": "rate_limit_error"}}"#)
            .expect(1)
            .create();

        let tailoring_output = minimal_tailoring_output();
        let llm_body = make_anthropic_response(&tailoring_output);
        let _mock_200 = llm_server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&llm_body)
            .create();

        let env = TestEnvHttp::new(&adr_server, &llm_server);
        env.cmd_anthropic()
            .args([
                "sync",
                "--force",
                "--runner",
                "anthropic-api",
                "--api-url",
                &env.adr_api_url,
            ])
            .assert()
            .success();

        assert!(env.file_exists("CLAUDE.md"));
    }

    // ── OpenAI tests ─────────────────────────────────────────────────────────

    /// Test 6: OpenAI runner with `--no-tailor` creates CLAUDE.md from ADR
    /// policies without calling the LLM.
    #[test]
    fn openai_no_tailor_writes_claude_md() {
        let mut adr_server = mockito::Server::new();
        let llm_server = mockito::Server::new(); // no LLM call expected

        let adr = make_adr_json(
            "adr-001",
            "Error Handling",
            &["Always use Result<T, E>"],
            &[],
            &["."],
        );
        let response = make_match_response_json(&format!("[{}]", adr));
        adr_server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let env = TestEnvHttp::new(&adr_server, &llm_server);
        env.cmd_openai()
            .args([
                "sync",
                "--force",
                "--no-tailor",
                "--runner",
                "openai-api",
                "--api-url",
                &env.adr_api_url,
            ])
            .assert()
            .success();

        assert!(env.file_exists("CLAUDE.md"));
        let content = env.read_file("CLAUDE.md");
        assert!(content.contains("# Project Guidelines"));
        assert!(content.contains("Error Handling"));
        assert!(content.contains("Always use Result<T, E>"));
    }

    /// Test 7: OpenAI runner with tailoring writes the LLM-generated content.
    #[test]
    fn openai_with_tailoring_writes_tailored_content() {
        let mut adr_server = mockito::Server::new();
        let mut llm_server = mockito::Server::new();

        let adr = make_adr_json(
            "adr-001",
            "Error Handling",
            &["Always use Result<T, E>"],
            &[],
            &["."],
        );
        let response = make_match_response_json(&format!("[{}]", adr));
        adr_server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let tailoring_output = minimal_tailoring_output();
        let llm_body = make_openai_response(&tailoring_output);
        let _llm_mock = llm_server
            .mock("POST", "/v1/responses")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&llm_body)
            .create();

        let env = TestEnvHttp::new(&adr_server, &llm_server);
        env.cmd_openai()
            .args([
                "sync",
                "--force",
                "--runner",
                "openai-api",
                "--api-url",
                &env.adr_api_url,
            ])
            .assert()
            .success();

        assert!(env.file_exists("CLAUDE.md"));
        let content = env.read_file("CLAUDE.md");
        assert!(content.contains("Tailored Rules"));
        assert!(content.contains("Always use Result"));
    }

    /// Test 8: OpenAI runner returns 401 → exit code 2 (not authenticated).
    #[test]
    fn openai_401_exits_code_2() {
        let mut adr_server = mockito::Server::new();
        let mut llm_server = mockito::Server::new();

        let adr = make_adr_json("adr-001", "Test ADR", &["policy"], &[], &["."]);
        let response = make_match_response_json(&format!("[{}]", adr));
        adr_server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        llm_server
            .mock("POST", "/v1/responses")
            .with_status(401)
            .with_body(r#"{"error": "Unauthorized"}"#)
            .create();

        let env = TestEnvHttp::new(&adr_server, &llm_server);
        env.cmd_openai()
            .args([
                "sync",
                "--force",
                "--runner",
                "openai-api",
                "--api-url",
                &env.adr_api_url,
            ])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("not authenticated"));
    }

    /// Test 9: OpenAI runner returns 500 → exit code 1 (runner failed).
    #[test]
    fn openai_500_exits_code_1() {
        let mut adr_server = mockito::Server::new();
        let mut llm_server = mockito::Server::new();

        let adr = make_adr_json("adr-001", "Test ADR", &["policy"], &[], &["."]);
        let response = make_match_response_json(&format!("[{}]", adr));
        adr_server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        llm_server
            .mock("POST", "/v1/responses")
            .with_status(500)
            .with_body("Internal Server Error")
            .create();

        let env = TestEnvHttp::new(&adr_server, &llm_server);
        env.cmd_openai()
            .args([
                "sync",
                "--force",
                "--runner",
                "openai-api",
                "--api-url",
                &env.adr_api_url,
            ])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("Runner failed"));
    }

    /// Test 10: OpenAI runner with missing API key exits with code 2.
    #[test]
    fn openai_missing_api_key_exits_with_error() {
        let mut adr_server = mockito::Server::new();
        let llm_server = mockito::Server::new();

        let adr = make_adr_json("adr-001", "Test ADR", &["policy"], &[], &["."]);
        let response = make_match_response_json(&format!("[{}]", adr));
        adr_server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let env = TestEnvHttp::new(&adr_server, &llm_server);
        let mut cmd = assert_cmd::Command::from(assert_cmd::cargo::cargo_bin_cmd!("actual"));
        cmd.env("ACTUAL_CONFIG", env.config_path.to_str().unwrap());
        cmd.env("CLAUDE_BINARY", "/nonexistent/claude");
        // Explicitly remove OPENAI_API_KEY so the runner fails to find a key.
        cmd.env_remove("OPENAI_API_KEY");
        cmd.current_dir(env.dir.path());
        cmd.args([
            "sync",
            "--force",
            "--runner",
            "openai-api",
            "--api-url",
            &env.adr_api_url,
        ])
        .assert()
        .code(2)
        .stderr(
            predicate::str::contains("API key not set")
                .or(predicate::str::contains("OPENAI_API_KEY")),
        );
    }

    /// Test 12: OpenAI runner receives a refusal response → exit code 1.
    #[test]
    fn openai_refusal_exits_code_1() {
        let mut adr_server = mockito::Server::new();
        let mut llm_server = mockito::Server::new();

        let adr = make_adr_json("adr-001", "Test ADR", &["policy"], &[], &["."]);
        let response = make_match_response_json(&format!("[{}]", adr));
        adr_server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let refusal_body = r#"{
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
        llm_server
            .mock("POST", "/v1/responses")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(refusal_body)
            .create();

        let env = TestEnvHttp::new(&adr_server, &llm_server);
        env.cmd_openai()
            .args([
                "sync",
                "--force",
                "--runner",
                "openai-api",
                "--api-url",
                &env.adr_api_url,
            ])
            .assert()
            .code(1)
            .stderr(
                predicate::str::contains("OpenAI refused")
                    .or(predicate::str::contains("validation failed")),
            );
    }
}
