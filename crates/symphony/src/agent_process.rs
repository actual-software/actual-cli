//! Shared process-management and JSON-parsing functions for agent subprocesses.
//!
//! These functions are extracted from `agent.rs` so they can be reused by both
//! the local orchestrator (`AgentSession`) and the distributed worker
//! (`ClaudeAgentLauncher`).

use crate::error::{Result, SymphonyError};
use crate::protocol::AgentEvent;
use regex::Regex;
use std::collections::HashMap;
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tracing::debug;

/// Build the full shell command to run Claude Code.
///
/// The prompt is NOT included in the command — it is passed via stdin to
/// avoid shell injection from untrusted issue data (titles, descriptions).
///
/// `max_turns` from the config is injected into the `--max-turns` flag for
/// bare commands. Pass-through commands (`-p`/`--print`/`app-server`) are
/// NOT modified — they are used as-is.
///
/// If `mcp_config_path` is provided, `--mcp-config <path>` is appended to
/// the command so the agent subprocess can use MCP tools.
pub fn build_agent_command(
    base_command: &str,
    max_turns: u32,
    mcp_config_path: Option<&Path>,
    resume_session_id: Option<&str>,
) -> String {
    // If the command already includes arguments (like -p, --output-format, etc.),
    // we just use it as-is (pass-through mode). Otherwise, we add the standard flags.
    // The prompt arrives on stdin, so Claude Code reads from stdin in -p mode.
    let mut cmd = if base_command.contains("-p")
        || base_command.contains("--print")
        || base_command.contains("app-server")
    {
        base_command.to_string()
    } else {
        // Default: claude -p with stream-json for structured output.
        // Use max_turns from config instead of a hardcoded value.
        let c = format!(
            "{base_command} -p --output-format stream-json --verbose --dangerously-skip-permissions --max-turns {max_turns}"
        );
        // If the base command already had --max-turns, rewrite it to match config.
        rewrite_max_turns(&c, max_turns)
    };

    // When resuming a previous session, inject --resume <session-id>.
    // This allows the agent to continue with full conversation context
    // (e.g., after a PR was in review and is now merged).
    if let Some(session_id) = resume_session_id {
        cmd.push_str(&format!(" --resume {session_id}"));
    }

    if let Some(path) = mcp_config_path {
        cmd.push_str(&format!(" --mcp-config {}", path.display()));
    }

    cmd
}

/// Rewrite any existing `--max-turns <N>` in the command to the given value.
pub fn rewrite_max_turns(command: &str, max_turns: u32) -> String {
    let re = Regex::new(r"--max-turns\s+\d+").expect("valid regex");
    re.replace_all(command, format!("--max-turns {max_turns}"))
        .into_owned()
}

/// Spawn the agent subprocess via `bash -lc`.
///
/// Stdin is piped so the caller can write the prompt without shell interpolation.
/// `env_vars` are injected into the child process environment.
pub fn spawn_agent_process(
    full_command: &str,
    workspace_path: &Path,
    env_vars: &HashMap<String, String>,
) -> Result<Child> {
    Command::new("bash")
        .arg("-lc")
        .arg(full_command)
        .current_dir(workspace_path)
        .envs(env_vars)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| SymphonyError::AgentStartupFailed {
            reason: format!("failed to spawn: {e}"),
        })
}

/// Convert a child wait result into an Ok/Err.
pub fn await_child_exit(result: std::io::Result<std::process::ExitStatus>) -> Result<bool> {
    let status = result.map_err(|e| SymphonyError::AgentProcessExited {
        reason: e.to_string(),
    })?;
    if status.success() {
        Ok(true)
    } else {
        Err(SymphonyError::AgentTurnFailed {
            reason: format!("agent exited with status: {status}"),
        })
    }
}

/// Write the prompt to the agent's stdin, then close it.
///
/// Extracted as a named async function (rather than an inline closure) to avoid
/// LLVM coverage instrumentation quirks on Linux where the implicit drop region
/// of a spawned `async move { … }` closure is marked as uncovered.
pub async fn write_prompt_to_stdin(mut stdin: tokio::process::ChildStdin, prompt_bytes: Vec<u8>) {
    let _ = stdin.write_all(&prompt_bytes).await;
    let _ = stdin.shutdown().await;
}

/// Read JSON lines from the agent's stdout and send parsed events to the channel.
///
/// Extracted as a named async function (rather than an inline closure) to avoid
/// LLVM coverage instrumentation quirks on Linux where the implicit drop region
/// of a spawned `async move { … }` closure is marked as uncovered.
pub async fn read_stdout_lines(stdout: tokio::process::ChildStdout, tx: mpsc::Sender<AgentEvent>) {
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(json) => {
                for event in parse_agent_message(&json) {
                    if tx.send(event).await.is_err() {
                        return;
                    }
                }
            }
            Err(_) => {
                // Non-JSON stdout line, log as notification
                log_non_json_stdout(&line);
            }
        }
    }
}

/// Log a non-JSON stdout line.
///
/// Extracted to a standalone function so that the tracing macro does not create
/// a separate LLVM coverage region.
fn log_non_json_stdout(line: &str) {
    debug!(line = %line, "non-json agent stdout");
}

/// Read diagnostic lines from the agent's stderr.
///
/// Extracted as a named async function (rather than an inline closure) to avoid
/// LLVM coverage instrumentation quirks on Linux where the implicit drop region
/// of a spawned `async move { … }` closure is marked as uncovered.
pub async fn read_stderr_lines(stderr: tokio::process::ChildStderr) {
    let reader = BufReader::new(stderr);
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        log_stderr_line_if_nonempty(&line);
    }
}

/// Log a single stderr line when it is non-empty.
///
/// Extracted to a standalone function so that the `if`-body closing brace does
/// not create a separate LLVM coverage region on Linux (the well-known
/// split-region flake for single-line branches inside `while let` loops).
pub fn log_stderr_line_if_nonempty(line: &str) {
    if !line.trim().is_empty() {
        debug!(line = %line, "agent stderr");
    }
}

/// Parse a JSON message from the agent subprocess stdout.
///
/// Claude Code stream-json format emits various message types:
/// - system messages (init, result)
/// - assistant messages (text, tool_use)
/// - tool results
///
/// Returns a `Vec<AgentEvent>` because a single message (e.g. a "result"
/// message with both text and usage data) can produce multiple events.
pub fn parse_agent_message(json: &serde_json::Value) -> Vec<AgentEvent> {
    let msg_type = json.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match msg_type {
        "system" => parse_system_message(json),
        "result" => parse_result_message(json),
        "assistant" => parse_assistant_message(json),
        // §11.2: Populate rate limits from Claude CLI rate_limit_event messages
        "rate_limit_event" => vec![AgentEvent::RateLimitUpdate { data: json.clone() }],
        _ => parse_fallback_message(json, msg_type),
    }
}

/// Parse a "system" type message.
pub fn parse_system_message(json: &serde_json::Value) -> Vec<AgentEvent> {
    let subtype = json.get("subtype").and_then(|v| v.as_str()).unwrap_or("");

    match subtype {
        "init" => {
            let session_id = json
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            vec![AgentEvent::SessionStarted {
                session_id,
                pid: None,
            }]
        }
        _ => vec![],
    }
}

/// Parse a "result" type message.
///
/// Emits both a `TurnCompleted` event (with result text) and a `TokenUsage`
/// event when usage data is present, so neither is dropped.
pub fn parse_result_message(json: &serde_json::Value) -> Vec<AgentEvent> {
    let result_text = json
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut events = Vec::new();

    // Always emit TurnCompleted for result messages
    events.push(AgentEvent::TurnCompleted {
        message: if result_text.is_empty() {
            None
        } else {
            Some(result_text)
        },
    });

    // Also emit TokenUsage if usage data is present
    if let Some(usage) = json.get("usage") {
        let input = usage
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output = usage
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        events.push(AgentEvent::TokenUsage {
            input_tokens: input,
            output_tokens: output,
            total_tokens: input + output,
        });
    }

    events
}

/// Parse an "assistant" type message.
pub fn parse_assistant_message(json: &serde_json::Value) -> Vec<AgentEvent> {
    let message = json
        .get("message")
        .and_then(|v| {
            // Could be a string or an object with content
            v.as_str().map(|s| s.to_string()).or_else(|| {
                v.get("content").and_then(|c| c.as_array()).and_then(|arr| {
                    arr.iter()
                        .filter_map(|item| {
                            if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                                item.get("text")
                                    .and_then(|t| t.as_str())
                                    .map(|s| s.to_string())
                            } else {
                                None
                            }
                        })
                        .next()
                })
            })
        })
        .unwrap_or_default();

    if !message.is_empty() {
        vec![AgentEvent::Notification { message }]
    } else {
        vec![]
    }
}

/// Parse a message with an unrecognised type, checking for usage data.
pub fn parse_fallback_message(json: &serde_json::Value, msg_type: &str) -> Vec<AgentEvent> {
    // Check for usage/token events in any message
    if let Some(usage) = json.get("usage").or_else(|| json.get("total_token_usage")) {
        let input = usage
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output = usage
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if input > 0 || output > 0 {
            return vec![AgentEvent::TokenUsage {
                input_tokens: input,
                output_tokens: output,
                total_tokens: input + output,
            }];
        }
    }

    if !msg_type.is_empty() {
        vec![AgentEvent::AgentMessage {
            event_type: msg_type.to_string(),
            message: json
                .get("message")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        }]
    } else {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tokio::process::Command;
    use tracing_test::traced_test;

    // ---- build_agent_command tests ----

    #[test]
    fn test_build_agent_command_with_flags() {
        let cmd = build_agent_command(
            "claude -p --output-format stream-json --dangerously-skip-permissions",
            20,
            None,
            None,
        );
        assert!(cmd.contains("claude -p"));
        // Prompt is passed via stdin, not in the command
        assert!(!cmd.contains("--max-turns"));
    }

    #[test]
    fn test_build_agent_command_bare() {
        let cmd = build_agent_command("claude", 20, None, None);
        assert!(cmd.contains("claude"));
        assert!(cmd.contains("-p"));
        assert!(cmd.contains("--output-format stream-json"));
        assert!(cmd.contains("--verbose"));
    }

    #[test]
    fn test_build_agent_command_no_prompt_in_command() {
        // Verify that the prompt is never embedded in the shell command
        let cmd = build_agent_command("claude -p", 20, None, None);
        // Should just be the base command as-is, no prompt appended
        assert_eq!(cmd, "claude -p");
    }

    #[test]
    fn test_build_agent_command_bare_uses_config_max_turns() {
        // Bare command gets --max-turns from the config value, not hardcoded 20
        let cmd = build_agent_command("claude", 42, None, None);
        assert!(cmd.contains("--max-turns 42"));
        assert!(!cmd.contains("--max-turns 20"));
    }

    #[test]
    fn test_build_agent_command_passthrough_no_rewrite() {
        // Command with -p and --max-turns 30 should NOT be rewritten
        let cmd = build_agent_command("claude -p --max-turns 30", 50, None, None);
        assert_eq!(cmd, "claude -p --max-turns 30");
    }

    #[test]
    fn test_build_agent_command_passthrough_without_max_turns() {
        // Command with -p but no --max-turns stays as-is
        let cmd = build_agent_command("claude -p --output-format json", 50, None, None);
        assert_eq!(cmd, "claude -p --output-format json");
    }

    #[test]
    fn test_build_agent_command_with_mcp_config_bare() {
        let cmd = build_agent_command("claude", 20, Some(Path::new("/tmp/mcp.json")), None);
        assert!(cmd.contains("--mcp-config /tmp/mcp.json"));
        assert!(cmd.contains("-p"));
    }

    #[test]
    fn test_build_agent_command_with_mcp_config_passthrough() {
        let cmd = build_agent_command(
            "claude -p --output-format json",
            20,
            Some(Path::new("/tmp/mcp.json")),
            None,
        );
        assert!(cmd.contains("--mcp-config /tmp/mcp.json"));
        assert!(cmd.starts_with("claude -p --output-format json"));
    }

    #[test]
    fn test_build_agent_command_with_mcp_config_none() {
        let cmd = build_agent_command("claude", 20, None, None);
        assert!(!cmd.contains("--mcp-config"));
    }

    #[test]
    fn test_build_agent_command_with_print_flag() {
        let cmd = build_agent_command("claude --print", 20, None, None);
        // --print matches, so it should use the command as-is
        assert_eq!(cmd, "claude --print");
        assert!(!cmd.contains("--output-format"));
    }

    #[test]
    fn test_build_agent_command_with_app_server() {
        let cmd = build_agent_command("claude app-server --port 3000", 20, None, None);
        // app-server matches, so it should use the command as-is
        assert_eq!(cmd, "claude app-server --port 3000");
        assert!(!cmd.contains("--output-format"));
    }

    #[test]
    fn test_build_agent_command_with_resume_session_id() {
        let cmd = build_agent_command(
            "claude",
            30,
            None,
            Some("550e8400-e29b-41d4-a716-446655440000"),
        );
        assert!(cmd.contains("--resume 550e8400-e29b-41d4-a716-446655440000"));
        assert!(cmd.contains("-p"));
    }

    #[test]
    fn test_build_agent_command_without_resume() {
        let cmd = build_agent_command("claude", 30, None, None);
        assert!(!cmd.contains("--resume"));
    }

    // ---- rewrite_max_turns tests ----

    #[test]
    fn test_rewrite_max_turns() {
        // Existing --max-turns value gets rewritten
        let result = rewrite_max_turns("claude -p --max-turns 30 --verbose", 50);
        assert_eq!(result, "claude -p --max-turns 50 --verbose");
    }

    // ---- parse_agent_message tests ----

    #[test]
    fn test_parse_system_init() {
        let json = serde_json::json!({
            "type": "system",
            "subtype": "init",
            "session_id": "abc-123"
        });
        let events = parse_agent_message(&json);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], AgentEvent::SessionStarted { session_id, .. } if session_id == "abc-123")
        );
    }

    #[test]
    fn test_parse_result() {
        let json = serde_json::json!({
            "type": "result",
            "result": "Done fixing the bug"
        });
        let events = parse_agent_message(&json);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], AgentEvent::TurnCompleted { message } if message.as_deref() == Some("Done fixing the bug"))
        );
    }

    #[test]
    fn test_parse_result_with_usage() {
        let json = serde_json::json!({
            "type": "result",
            "result": "Done",
            "usage": {
                "input_tokens": 1000,
                "output_tokens": 500
            }
        });
        let events = parse_agent_message(&json);
        assert_eq!(events.len(), 2);
        assert!(
            matches!(&events[0], AgentEvent::TurnCompleted { message } if message.as_deref() == Some("Done"))
        );
        assert!(matches!(
            &events[1],
            AgentEvent::TokenUsage {
                input_tokens: 1000,
                output_tokens: 500,
                total_tokens: 1500
            }
        ));
    }

    #[test]
    fn test_parse_result_with_usage_and_empty_text() {
        let json = serde_json::json!({
            "type": "result",
            "result": "",
            "usage": {
                "input_tokens": 400,
                "output_tokens": 200
            }
        });
        let events = parse_agent_message(&json);
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], AgentEvent::TurnCompleted { message } if message.is_none()));
        assert!(matches!(
            &events[1],
            AgentEvent::TokenUsage {
                input_tokens: 400,
                output_tokens: 200,
                total_tokens: 600
            }
        ));
    }

    #[test]
    fn test_parse_system_non_init_subtype() {
        let json = serde_json::json!({
            "type": "system",
            "subtype": "shutdown"
        });
        assert!(parse_agent_message(&json).is_empty());
    }

    #[test]
    fn test_parse_system_init_missing_session_id() {
        let json = serde_json::json!({
            "type": "system",
            "subtype": "init"
        });
        let events = parse_agent_message(&json);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], AgentEvent::SessionStarted { session_id, pid } if session_id == "unknown" && pid.is_none())
        );
    }

    #[test]
    fn test_parse_result_empty_text() {
        let json = serde_json::json!({
            "type": "result",
            "result": ""
        });
        let events = parse_agent_message(&json);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AgentEvent::TurnCompleted { message } if message.is_none()));
    }

    #[test]
    fn test_parse_assistant_string_message() {
        let json = serde_json::json!({
            "type": "assistant",
            "message": "Hello, I'm working on it"
        });
        let events = parse_agent_message(&json);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], AgentEvent::Notification { message } if message == "Hello, I'm working on it")
        );
    }

    #[test]
    fn test_parse_assistant_content_array() {
        let json = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "text", "text": "Here is the result"},
                    {"type": "tool_use", "name": "bash"}
                ]
            }
        });
        let events = parse_agent_message(&json);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], AgentEvent::Notification { message } if message == "Here is the result")
        );
    }

    #[test]
    fn test_parse_assistant_empty_message() {
        let json = serde_json::json!({
            "type": "assistant",
            "message": ""
        });
        assert!(parse_agent_message(&json).is_empty());
    }

    #[test]
    fn test_parse_unknown_type_with_usage() {
        let json = serde_json::json!({
            "type": "token_update",
            "usage": {
                "input_tokens": 200,
                "output_tokens": 100
            }
        });
        let events = parse_agent_message(&json);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            AgentEvent::TokenUsage {
                input_tokens: 200,
                output_tokens: 100,
                total_tokens: 300
            }
        ));
    }

    #[test]
    fn test_parse_unknown_type_with_total_token_usage() {
        let json = serde_json::json!({
            "type": "summary",
            "total_token_usage": {
                "input_tokens": 5000,
                "output_tokens": 3000
            }
        });
        let events = parse_agent_message(&json);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            AgentEvent::TokenUsage {
                input_tokens: 5000,
                output_tokens: 3000,
                total_tokens: 8000
            }
        ));
    }

    #[test]
    fn test_parse_unknown_type_zero_tokens_skips() {
        let json = serde_json::json!({
            "type": "heartbeat",
            "usage": {
                "input_tokens": 0,
                "output_tokens": 0
            }
        });
        let events = parse_agent_message(&json);
        assert_eq!(events.len(), 1);
        // Zero tokens should skip TokenUsage, fall through to AgentMessage
        assert!(
            matches!(&events[0], AgentEvent::AgentMessage { event_type, .. } if event_type == "heartbeat")
        );
    }

    #[test]
    fn test_parse_unknown_non_empty_type_no_usage() {
        let json = serde_json::json!({
            "type": "tool_result",
            "message": "file written"
        });
        let events = parse_agent_message(&json);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], AgentEvent::AgentMessage { event_type, message }
            if event_type == "tool_result" && message.as_deref() == Some("file written"))
        );
    }

    // §11.2: rate_limit_event parsing
    #[test]
    fn test_parse_rate_limit_event() {
        let json = serde_json::json!({
            "type": "rate_limit_event",
            "requests_remaining": 50,
            "tokens_remaining": 100000,
            "reset_at": "2025-01-01T00:00:30Z"
        });
        let events = parse_agent_message(&json);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], AgentEvent::RateLimitUpdate { data } if data["requests_remaining"] == 50)
        );
    }

    #[test]
    fn test_parse_empty_type_no_usage() {
        let json = serde_json::json!({
            "something": "else"
        });
        assert!(parse_agent_message(&json).is_empty());
    }

    #[test]
    fn test_parse_assistant_content_array_non_text_only() {
        let json = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "tool_use", "name": "bash"},
                    {"type": "tool_result", "output": "done"}
                ]
            }
        });
        // All items are non-text, so filter_map yields nothing -> message is empty -> empty Vec
        assert!(parse_agent_message(&json).is_empty());
    }

    // ---- spawn_agent_process tests ----

    #[test]
    fn test_spawn_agent_process_nonexistent_dir() {
        // Using a nonexistent current_dir causes spawn to fail
        let bad_dir = std::path::Path::new("/nonexistent_dir_for_test_29384729");
        let err = spawn_agent_process("echo hello", bad_dir, &HashMap::new()).unwrap_err();
        assert!(matches!(err, SymphonyError::AgentStartupFailed { .. }));
    }

    #[tokio::test]
    async fn test_spawn_agent_process_with_env_vars() {
        let tmp = tempfile::tempdir().unwrap();
        let mut env_vars = HashMap::new();
        env_vars.insert(
            "SYMPHONY_TEST_ENV_VAR".to_string(),
            "test_value_12345".to_string(),
        );

        let child =
            spawn_agent_process("echo $SYMPHONY_TEST_ENV_VAR", tmp.path(), &env_vars).unwrap();
        let output = child.wait_with_output().await.unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("test_value_12345"),
            "env var should be visible in child process, got: {stdout}"
        );
    }

    #[tokio::test]
    async fn test_spawn_agent_process_with_empty_env_vars() {
        let tmp = tempfile::tempdir().unwrap();
        let env_vars = HashMap::new();

        let child = spawn_agent_process("echo hello", tmp.path(), &env_vars).unwrap();
        let output = child.wait_with_output().await.unwrap();
        assert!(output.status.success());
    }

    #[tokio::test]
    async fn test_spawn_agent_process_with_multiple_env_vars() {
        let tmp = tempfile::tempdir().unwrap();
        let mut env_vars = HashMap::new();
        env_vars.insert("VAR_A".to_string(), "alpha".to_string());
        env_vars.insert("VAR_B".to_string(), "beta".to_string());

        let child = spawn_agent_process("echo ${VAR_A}_${VAR_B}", tmp.path(), &env_vars).unwrap();
        let output = child.wait_with_output().await.unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("alpha_beta"),
            "multiple env vars should be visible, got: {stdout}"
        );
    }

    // ---- await_child_exit tests ----

    #[test]
    fn test_await_child_exit_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::Other, "wait failed");
        let err = await_child_exit(Err(io_err)).unwrap_err();
        assert!(matches!(err, SymphonyError::AgentProcessExited { .. }));
    }

    #[test]
    fn test_await_child_exit_success() {
        // Create a successful exit status by running a trivial command
        let output = std::process::Command::new("true").status().unwrap();
        let result = await_child_exit(Ok(output));
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_await_child_exit_failure() {
        // Create a failed exit status by running a command that fails
        let output = std::process::Command::new("false").status().unwrap();
        let result = await_child_exit(Ok(output));
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SymphonyError::AgentTurnFailed { .. }
        ));
    }

    // ---- write_prompt_to_stdin tests ----

    #[tokio::test]
    async fn test_write_prompt_to_stdin_writes_and_closes() {
        // Spawn a process that reads stdin and echoes it to stdout
        let mut child = Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("cat should spawn");

        let stdin = child.stdin.take().expect("stdin was piped");
        let prompt = b"hello from test".to_vec();

        // Call the function directly (not via tokio::spawn) to ensure coverage
        write_prompt_to_stdin(stdin, prompt).await;

        let output = child.wait_with_output().await.expect("cat should finish");
        assert_eq!(output.stdout, b"hello from test");
    }

    // ---- read_stdout_lines tests ----

    #[tokio::test]
    async fn test_read_stdout_lines_parses_json() {
        let mut child = Command::new("bash")
            .arg("-c")
            .arg(r#"printf '{"type":"result","result":"done"}\n'"#)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("bash should spawn");

        let stdout = child.stdout.take().expect("stdout was piped");
        let (tx, mut rx) = mpsc::channel(256);

        read_stdout_lines(stdout, tx).await;

        let mut events = vec![];
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        assert!(!events.is_empty());
        assert!(
            matches!(&events[0], AgentEvent::TurnCompleted { message } if message.as_deref() == Some("done"))
        );

        let _ = child.wait().await;
    }

    #[traced_test]
    #[tokio::test]
    async fn test_read_stdout_lines_skips_empty_and_non_json() {
        let mut child = Command::new("bash")
            .arg("-c")
            .arg(r#"printf '\nnot json\n{"type":"result","result":"ok"}\n\n'"#)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("bash should spawn");

        let stdout = child.stdout.take().expect("stdout was piped");
        let (tx, mut rx) = mpsc::channel(256);

        read_stdout_lines(stdout, tx).await;

        let mut events = vec![];
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        // Should only get TurnCompleted from the valid JSON, not from empty or non-JSON lines
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AgentEvent::TurnCompleted { .. }));
        // The non-JSON line should have been logged
        assert!(logs_contain("non-json agent stdout"));

        let _ = child.wait().await;
    }

    #[tokio::test]
    async fn test_read_stdout_lines_receiver_drop_breaks_loop() {
        let mut child = Command::new("bash")
            .arg("-c")
            .arg(r#"printf '{"type":"result","result":"first"}\n'; sleep 1; printf '{"type":"result","result":"second"}\n'"#)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("bash should spawn");

        let stdout = child.stdout.take().expect("stdout was piped");
        let (tx, rx) = mpsc::channel(1);

        // Drop the receiver so send will fail
        drop(rx);

        // read_stdout_lines should return when send fails
        read_stdout_lines(stdout, tx).await;

        let _ = child.kill().await;
    }

    // ---- read_stderr_lines tests ----

    #[traced_test]
    #[tokio::test]
    async fn test_read_stderr_lines_logs_nonempty() {
        let mut child = Command::new("bash")
            .arg("-c")
            .arg("echo 'stderr output' >&2")
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("bash should spawn");

        let stderr = child.stderr.take().expect("stderr was piped");
        read_stderr_lines(stderr).await;

        assert!(logs_contain("agent stderr"));

        let _ = child.wait().await;
    }

    #[traced_test]
    #[tokio::test]
    async fn test_read_stderr_lines_skips_empty() {
        let mut child = Command::new("bash")
            .arg("-c")
            .arg("echo '' >&2")
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("bash should spawn");

        let stderr = child.stderr.take().expect("stderr was piped");
        read_stderr_lines(stderr).await;

        assert!(!logs_contain("agent stderr"));

        let _ = child.wait().await;
    }

    // ---- log_stderr_line_if_nonempty tests ----

    #[traced_test]
    #[test]
    fn test_log_stderr_line_if_nonempty_logs_nonempty() {
        log_stderr_line_if_nonempty("real error");
        assert!(logs_contain("agent stderr"));
    }

    #[traced_test]
    #[test]
    fn test_log_stderr_line_if_nonempty_skips_empty() {
        log_stderr_line_if_nonempty("");
        log_stderr_line_if_nonempty("   ");
        assert!(!logs_contain("agent stderr"));
    }

    // ---- log_non_json_stdout tests ----

    #[traced_test]
    #[test]
    fn test_log_non_json_stdout_logs_line() {
        log_non_json_stdout("not json at all");
        assert!(logs_contain("non-json agent stdout"));
    }
}
