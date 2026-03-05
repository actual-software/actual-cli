use crate::config::CodingAgentConfig;
use crate::error::{Result, SymphonyError};
use crate::model::{AgentEvent, Issue};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Agent session representing a running Claude Code subprocess.
pub struct AgentSession {
    child: Child,
    pub pid: Option<u32>,
}

impl AgentSession {
    /// Launch a Claude Code subprocess in the given workspace directory.
    pub async fn launch(
        config: &CodingAgentConfig,
        workspace_path: &Path,
        prompt: &str,
        issue: &Issue,
    ) -> Result<(Self, mpsc::Receiver<AgentEvent>)> {
        // Validate workspace path
        if !workspace_path.is_dir() {
            return Err(SymphonyError::InvalidWorkspaceCwd {
                path: workspace_path.display().to_string(),
            });
        }

        let title = format!("{}: {}", issue.identifier, issue.title);

        // Build the command. We use `bash -lc` to launch Claude Code in print mode
        // with stream-json output for structured event streaming.
        let full_command = build_agent_command(&config.command, prompt, &title);

        debug!(
            command = %config.command,
            workspace = %workspace_path.display(),
            "launching agent subprocess"
        );

        let mut child = Command::new("bash")
            .arg("-lc")
            .arg(&full_command)
            .current_dir(workspace_path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| SymphonyError::AgentStartupFailed {
                reason: format!("failed to spawn: {e}"),
            })?;

        let pid = child.id();

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SymphonyError::AgentStartupFailed {
                reason: "failed to capture stdout".to_string(),
            })?;

        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| SymphonyError::AgentStartupFailed {
                reason: "failed to capture stderr".to_string(),
            })?;

        let (event_tx, event_rx) = mpsc::channel(256);

        // Spawn stdout reader
        let tx_stdout = event_tx.clone();
        tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<serde_json::Value>(&line) {
                    Ok(json) => {
                        if let Some(event) = parse_agent_message(&json) {
                            if tx_stdout.send(event).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(_) => {
                        // Non-JSON stdout line, log as notification
                        debug!(line = %line, "non-json agent stdout");
                    }
                }
            }
        });

        // Spawn stderr reader (diagnostic only, not protocol)
        tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.trim().is_empty() {
                    debug!(line = %line, "agent stderr");
                }
            }
        });

        // Emit session_started event
        let _ = event_tx
            .send(AgentEvent::SessionStarted {
                session_id: format!("claude-{}", pid.unwrap_or(0)),
                pid,
            })
            .await;

        Ok((Self { child, pid }, event_rx))
    }

    /// Wait for the agent process to complete with a timeout.
    pub async fn wait_with_timeout(&mut self, timeout_ms: u64) -> Result<bool> {
        let timeout = Duration::from_millis(timeout_ms);
        match tokio::time::timeout(timeout, self.child.wait()).await {
            Ok(Ok(status)) => {
                if status.success() {
                    Ok(true)
                } else {
                    Err(SymphonyError::AgentTurnFailed {
                        reason: format!("agent exited with status: {status}"),
                    })
                }
            }
            Ok(Err(e)) => Err(SymphonyError::AgentProcessExited {
                reason: e.to_string(),
            }),
            Err(_) => Err(SymphonyError::AgentTurnTimeout),
        }
    }

    /// Kill the agent process.
    pub async fn kill(&mut self) {
        if let Err(e) = self.child.kill().await {
            warn!(error = %e, "failed to kill agent process");
        }
    }
}

/// Build the full shell command to run Claude Code.
fn build_agent_command(base_command: &str, prompt: &str, _title: &str) -> String {
    // Escape single quotes in the prompt for shell safety
    let escaped_prompt = prompt.replace('\'', "'\\''");

    // If the command already includes arguments (like -p, --output-format, etc.),
    // we just append the prompt. Otherwise, we add the standard flags.
    if base_command.contains("-p")
        || base_command.contains("--print")
        || base_command.contains("app-server")
    {
        format!("{base_command} '{escaped_prompt}'")
    } else {
        // Default: claude -p with stream-json for structured output
        format!(
            "{base_command} -p --output-format stream-json --dangerously-skip-permissions --max-turns 20 '{escaped_prompt}'"
        )
    }
}

/// Parse a JSON message from the agent subprocess stdout.
///
/// Claude Code stream-json format emits various message types:
/// - system messages (init, result)
/// - assistant messages (text, tool_use)
/// - tool results
fn parse_agent_message(json: &serde_json::Value) -> Option<AgentEvent> {
    let msg_type = json.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match msg_type {
        "system" => {
            let subtype = json.get("subtype").and_then(|v| v.as_str()).unwrap_or("");

            match subtype {
                "init" => {
                    let session_id = json
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    Some(AgentEvent::SessionStarted {
                        session_id,
                        pid: None,
                    })
                }
                _ => None,
            }
        }
        "result" => {
            // Final result message
            let result_text = json
                .get("result")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // Check for token usage in the result
            if let Some(usage) = json.get("usage") {
                let input = usage
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let output = usage
                    .get("output_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                Some(AgentEvent::TokenUsage {
                    input_tokens: input,
                    output_tokens: output,
                    total_tokens: input + output,
                })
            } else {
                Some(AgentEvent::TurnCompleted {
                    message: if result_text.is_empty() {
                        None
                    } else {
                        Some(result_text)
                    },
                })
            }
        }
        "assistant" => {
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
                Some(AgentEvent::Notification { message })
            } else {
                None
            }
        }
        _ => {
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
                    return Some(AgentEvent::TokenUsage {
                        input_tokens: input,
                        output_tokens: output,
                        total_tokens: input + output,
                    });
                }
            }

            if !msg_type.is_empty() {
                Some(AgentEvent::AgentMessage {
                    event_type: msg_type.to_string(),
                    message: json
                        .get("message")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                })
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_agent_command_with_flags() {
        let cmd = build_agent_command(
            "claude -p --output-format stream-json --dangerously-skip-permissions",
            "Fix the bug",
            "PROJ-1: Fix bug",
        );
        assert!(cmd.contains("claude -p"));
        assert!(cmd.contains("Fix the bug"));
    }

    #[test]
    fn test_build_agent_command_bare() {
        let cmd = build_agent_command("claude", "Fix the bug", "PROJ-1: Fix bug");
        assert!(cmd.contains("claude"));
        assert!(cmd.contains("-p"));
        assert!(cmd.contains("--output-format stream-json"));
        assert!(cmd.contains("Fix the bug"));
    }

    #[test]
    fn test_build_agent_command_escapes_quotes() {
        let cmd = build_agent_command("claude -p", "It's a test", "PROJ-1: Test");
        assert!(cmd.contains("It'\\''s a test"));
    }

    #[test]
    fn test_parse_system_init() {
        let json = serde_json::json!({
            "type": "system",
            "subtype": "init",
            "session_id": "abc-123"
        });
        let event = parse_agent_message(&json).unwrap();
        match event {
            AgentEvent::SessionStarted { session_id, .. } => {
                assert_eq!(session_id, "abc-123");
            }
            _ => panic!("expected SessionStarted"),
        }
    }

    #[test]
    fn test_parse_result() {
        let json = serde_json::json!({
            "type": "result",
            "result": "Done fixing the bug"
        });
        let event = parse_agent_message(&json).unwrap();
        match event {
            AgentEvent::TurnCompleted { message } => {
                assert_eq!(message, Some("Done fixing the bug".to_string()));
            }
            _ => panic!("expected TurnCompleted"),
        }
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
        let event = parse_agent_message(&json).unwrap();
        match event {
            AgentEvent::TokenUsage {
                input_tokens,
                output_tokens,
                total_tokens,
            } => {
                assert_eq!(input_tokens, 1000);
                assert_eq!(output_tokens, 500);
                assert_eq!(total_tokens, 1500);
            }
            _ => panic!("expected TokenUsage"),
        }
    }
}
