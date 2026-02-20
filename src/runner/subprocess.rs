use std::path::PathBuf;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

use crate::error::ActualError;
use crate::tailoring::types::TailoringOutput;

/// Envelope returned by Claude Code when invoked with `--print`.
///
/// With `--output-format json` this is the only line on stdout.
/// With `--output-format stream-json` this is the final line (type = "result").
/// The actual structured output (matching `--json-schema`) is in `structured_output`.
#[derive(Deserialize)]
struct ClaudeEnvelope<T> {
    #[serde(rename = "type")]
    result_type: Option<String>,
    subtype: Option<String>,
    is_error: Option<bool>,
    structured_output: Option<T>,
    result: Option<String>,
}

/// Backend-agnostic trait for running tailoring invocations.
///
/// Accepts high-level parameters instead of raw CLI args, keeping the
/// subprocess wire format as an implementation detail of [`CliClaudeRunner`].
///
/// Uses native async fn in trait (stable since Rust 1.75). The lint is suppressed
/// because this trait is internal and all implementors are `Send + Sync`.
#[allow(async_fn_in_trait)]
pub trait TailoringRunner: Send + Sync {
    async fn run_tailoring(
        &self,
        prompt: &str,
        schema: &str,
        model_override: Option<&str>,
        max_budget_usd: Option<f64>,
    ) -> Result<TailoringOutput, ActualError>;

    /// Register a channel for streaming subprocess events (tool calls, stderr lines).
    ///
    /// Each message is a human-readable summary line such as
    /// `"Claude: Read src/app.tsx"` or `"[stderr] some error"`.
    /// A default no-op implementation is provided so test fakes do not need to
    /// implement this method.
    fn set_event_tx(&self, _tx: UnboundedSender<String>) {}
}

/// Format a single `stream-json` stdout line into a concise human-readable
/// summary for display in the TUI, or `None` for events that are not worth
/// showing (e.g., raw text tokens, user tool-result messages, result events).
///
/// Recognised event types:
/// - `system / init` → `"Claude Code started (model: <m>)"`
/// - `assistant` with `tool_use` content → `"Claude: <Tool> <path|pattern>"`
/// - Everything else → `None` (suppressed to avoid noise)
pub(crate) fn format_stream_event(line: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let event_type = value["type"].as_str()?;

    match event_type {
        "system" => {
            let model = value["model"].as_str().unwrap_or("unknown");
            Some(format!("Claude Code started (model: {model})"))
        }
        "assistant" => {
            let content = value["message"]["content"].as_array()?;
            let tool_uses: Vec<String> = content
                .iter()
                .filter_map(|item| {
                    if item["type"].as_str() != Some("tool_use") {
                        return None;
                    }
                    let name = item["name"].as_str()?;
                    let summary = match name {
                        "Read" | "Write" | "Edit" => {
                            let path = item["input"]["file_path"]
                                .as_str()
                                .or_else(|| item["input"]["path"].as_str())
                                .unwrap_or("?");
                            format!("{name} {path}")
                        }
                        "MultiEdit" => {
                            let path = item["input"]["file_path"].as_str().unwrap_or("?");
                            format!("MultiEdit {path}")
                        }
                        "Glob" => {
                            let pattern = item["input"]["pattern"].as_str().unwrap_or("*");
                            format!("Glob {pattern}")
                        }
                        "Grep" => {
                            let pattern = item["input"]["pattern"].as_str().unwrap_or("?");
                            format!("Grep {pattern}")
                        }
                        other => other.to_string(),
                    };
                    Some(summary)
                })
                .collect();

            if tool_uses.is_empty() {
                None // text-only messages are too noisy
            } else {
                Some(format!("Claude: {}", tool_uses.join(", ")))
            }
        }
        _ => None,
    }
}

/// Trait for running Claude Code and deserializing output.
///
/// Generic over the output type T at the method level — any `DeserializeOwned` type
/// can be returned. This allows callers to use the same runner for different schemas
/// (e.g., `RepoAnalysis`, `TailoringOutput`).
///
/// Uses native async fn in trait (stable since Rust 1.75). The lint is suppressed
/// because this trait is internal and all implementors are `Send + Sync`.
#[allow(async_fn_in_trait)]
pub trait ClaudeRunner: Send + Sync {
    async fn run<T: DeserializeOwned + Send>(&self, args: &[String]) -> Result<T, ActualError>;
}

/// Production implementation that spawns `claude --print` as a subprocess.
pub struct CliClaudeRunner {
    /// Path to the Claude CLI binary.
    binary_path: PathBuf,
    /// Maximum time to wait for subprocess completion.
    timeout: Duration,
    /// Optional override for the max-turns limit (overrides the default in `InvocationOptions`).
    max_turns_override: Option<u32>,
    /// Optional sender for streaming subprocess events.  Set via
    /// [`TailoringRunner::set_event_tx`] before calling `run_tailoring`.
    event_tx: std::sync::Mutex<Option<UnboundedSender<String>>>,
}

impl CliClaudeRunner {
    pub fn new(binary_path: PathBuf, timeout: Duration) -> Self {
        Self {
            binary_path,
            timeout,
            max_turns_override: None,
            event_tx: std::sync::Mutex::new(None),
        }
    }

    /// Set an explicit max-turns limit, overriding the default from `InvocationOptions`.
    pub fn with_max_turns(mut self, max_turns: u32) -> Self {
        self.max_turns_override = Some(max_turns);
        self
    }
}

/// Convert an I/O error from subprocess operations into an `ActualError`.
fn io_err(context: &str, e: std::io::Error) -> ActualError {
    ActualError::ClaudeSubprocessFailed {
        message: format!("{context}: {e}"),
        stderr: String::new(),
    }
}

/// Parse the output of a completed subprocess, returning the deserialized result
/// or an appropriate error.
///
/// Claude Code wraps structured output in an envelope JSON object when invoked
/// with `--print --output-format json --json-schema <schema>`. This function
/// first tries to parse as a `ClaudeEnvelope<T>` and extract `structured_output`,
/// falling back to direct parsing as `T` for backwards compatibility (e.g., test
/// fake binaries that emit raw JSON).
pub(crate) fn parse_output<T: DeserializeOwned>(
    output: std::process::Output,
) -> Result<T, ActualError> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        return Err(ActualError::ClaudeSubprocessFailed {
            message: format!("Claude Code exited with code {code}"),
            stderr,
        });
    }

    // Try envelope parse first, then fall back to direct parse.
    if let Ok(envelope) = serde_json::from_slice::<ClaudeEnvelope<T>>(&output.stdout) {
        // Check for error states in the envelope.
        if envelope.is_error == Some(true) || envelope.subtype.as_deref() == Some("error_max_turns")
        {
            let detail = envelope
                .result
                .unwrap_or_else(|| "unknown error".to_string());
            return Err(ActualError::ClaudeSubprocessFailed {
                message: format!("Claude Code returned an error: {detail}"),
                stderr: String::new(),
            });
        }

        // Only treat as an envelope if it looks like one (has `type` = "result").
        if envelope.result_type.as_deref() == Some("result") {
            return envelope
                .structured_output
                .ok_or_else(|| ActualError::ClaudeSubprocessFailed {
                    message: "Claude Code envelope missing structured_output field".to_string(),
                    stderr: String::new(),
                });
        }
    }

    // Fallback: direct parse as T (backwards compat with test fake binaries).
    let parsed: T = serde_json::from_slice(&output.stdout)?;
    Ok(parsed)
}

/// Resolve the result of a timeout-wrapped `wait_with_output` call.
///
/// Maps the three possible outcomes:
/// - `Ok(Ok(output))` — subprocess finished within the timeout
/// - `Ok(Err(e))` — subprocess I/O error (e.g., pipe failure during wait)
/// - `Err(_)` — timeout expired before subprocess finished
pub(crate) fn resolve_output(
    result: Result<Result<std::process::Output, std::io::Error>, tokio::time::error::Elapsed>,
    timeout: Duration,
) -> Result<std::process::Output, ActualError> {
    match result {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(io_err("Failed to wait for Claude Code", e)),
        Err(_) => Err(ActualError::ClaudeTimeout {
            seconds: timeout.as_secs(),
        }),
    }
}

/// Spawn the binary, wait with timeout, and parse JSON output.
///
/// Extracted from the `ClaudeRunner` impl so the trait method is a
/// thin delegation.
async fn run_subprocess<T: DeserializeOwned>(
    binary_path: &std::path::Path,
    timeout: Duration,
    args: &[String],
) -> Result<T, ActualError> {
    let mut cmd = Command::new(binary_path);
    cmd.arg("--print");
    cmd.args(args);
    // When the timeout fires, the `wait_with_output` future is dropped, which drops
    // the `Child` it owns. With `kill_on_drop(true)`, tokio sends SIGKILL to the child
    // on drop, preventing orphaned processes.
    cmd.kill_on_drop(true);

    let child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| io_err("Failed to spawn Claude Code", e))?;

    let result = tokio::time::timeout(timeout, child.wait_with_output()).await;
    let output = resolve_output(result, timeout)?;

    parse_output(output)
}

/// Spawn the binary in streaming mode (`--output-format stream-json`), read
/// stdout line-by-line, forward formatted event summaries to `event_tx`, and
/// return the structured output extracted from the final `"type":"result"` event.
///
/// Stderr is drained concurrently in a background task and each line is
/// forwarded to `event_tx` prefixed with `"[stderr] "` so callers see it in
/// the same channel as tool-call events.
async fn run_subprocess_streaming<T: DeserializeOwned>(
    binary_path: &std::path::Path,
    timeout: Duration,
    args: &[String],
    event_tx: Option<UnboundedSender<String>>,
) -> Result<T, ActualError> {
    let mut cmd = Command::new(binary_path);
    cmd.arg("--print");
    cmd.args(args);
    cmd.kill_on_drop(true);

    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| io_err("Failed to spawn Claude Code", e))?;

    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");

    // Drain stderr in a background task; forward each line to event_tx.
    let event_tx_for_stderr = event_tx.clone();
    let stderr_task: tokio::task::JoinHandle<Vec<u8>> = tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        let mut all_bytes: Vec<u8> = Vec::new();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(tx) = &event_tx_for_stderr {
                let _ = tx.send(format!("[stderr] {line}"));
            }
            all_bytes.extend_from_slice(line.as_bytes());
            all_bytes.push(b'\n');
        }
        all_bytes
    });

    // Read stdout line-by-line, forwarding formatted summaries and looking for
    // the final "type":"result" event.
    let find_result = async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(tx) = &event_tx {
                if let Some(summary) = format_stream_event(&line) {
                    let _ = tx.send(summary);
                }
            }
            if let Ok(envelope) = serde_json::from_str::<ClaudeEnvelope<T>>(&line) {
                if envelope.result_type.as_deref() == Some("result") {
                    return Ok(Some(envelope));
                }
            }
        }
        Ok::<_, std::io::Error>(None)
    };

    match tokio::time::timeout(timeout, find_result).await {
        Ok(Ok(Some(envelope))) => {
            if envelope.is_error == Some(true)
                || envelope.subtype.as_deref() == Some("error_max_turns")
            {
                let detail = envelope
                    .result
                    .unwrap_or_else(|| "unknown error".to_string());
                let stderr_bytes = stderr_task.await.unwrap_or_default();
                let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();
                Err(ActualError::ClaudeSubprocessFailed {
                    message: format!("Claude Code returned an error: {detail}"),
                    stderr,
                })
            } else {
                envelope
                    .structured_output
                    .ok_or_else(|| ActualError::ClaudeSubprocessFailed {
                        message: "Claude Code envelope missing structured_output field".to_string(),
                        stderr: String::new(),
                    })
            }
        }
        Ok(Ok(None)) => {
            let stderr_bytes = stderr_task.await.unwrap_or_default();
            let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();
            Err(ActualError::ClaudeSubprocessFailed {
                message: "Claude Code exited without producing a result".to_string(),
                stderr,
            })
        }
        Ok(Err(e)) => Err(io_err("Failed to read Claude Code output", e)),
        Err(_) => Err(ActualError::ClaudeTimeout {
            seconds: timeout.as_secs(),
        }),
    }
}

impl TailoringRunner for CliClaudeRunner {
    async fn run_tailoring(
        &self,
        prompt: &str,
        schema: &str,
        model_override: Option<&str>,
        max_budget_usd: Option<f64>,
    ) -> Result<TailoringOutput, ActualError> {
        use crate::runner::options::InvocationOptions;

        let mut opts = InvocationOptions::for_tailoring(model_override);
        if let Some(t) = self.max_turns_override {
            opts.max_turns = t;
        }
        opts.json_schema = Some(schema.to_string());
        opts.max_budget_usd = max_budget_usd;

        let mut args = opts.to_args();
        args.push("-p".to_string());
        args.push(prompt.to_string());

        let event_tx = self.event_tx.lock().unwrap().clone();
        run_subprocess_streaming(&self.binary_path, self.timeout, &args, event_tx).await
    }

    fn set_event_tx(&self, tx: UnboundedSender<String>) {
        *self.event_tx.lock().unwrap() = Some(tx);
    }
}

impl ClaudeRunner for CliClaudeRunner {
    async fn run<T: DeserializeOwned + Send>(&self, args: &[String]) -> Result<T, ActualError> {
        // Analysis invocations don't need event streaming — pass no event_tx.
        run_subprocess(&self.binary_path, self.timeout, args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    /// Extract `ClaudeSubprocessFailed` fields from an `ActualError`, panicking on mismatch.
    #[rustfmt::skip]
    fn subprocess_failed(e: ActualError) -> (String, String) {
        let ActualError::ClaudeSubprocessFailed { message, stderr } = e else { unreachable!() };
        (message, stderr)
    }

    /// Extract `ClaudeTimeout` seconds from an `ActualError`, panicking on mismatch.
    #[rustfmt::skip]
    fn timeout_seconds(e: ActualError) -> u64 {
        let ActualError::ClaudeTimeout { seconds } = e else { unreachable!() };
        seconds
    }

    /// Mock implementation for testing trait mockability.
    struct MockClaudeRunner {
        json_response: String,
    }

    impl ClaudeRunner for MockClaudeRunner {
        async fn run<T: DeserializeOwned + Send>(
            &self,
            _args: &[String],
        ) -> Result<T, ActualError> {
            let parsed: T = serde_json::from_str(&self.json_response)?;
            Ok(parsed)
        }
    }

    #[tokio::test]
    async fn test_mock_claude_runner_invalid_json() {
        let runner = MockClaudeRunner {
            json_response: "not valid json".to_string(),
        };
        let result: Result<serde_json::Value, _> = runner.run(&[]).await;
        assert!(matches!(result, Err(ActualError::ClaudeOutputParse(_))));
    }

    #[tokio::test]
    async fn test_mock_claude_runner() {
        let runner = MockClaudeRunner {
            json_response: r#"{"key": "value", "num": 42}"#.to_string(),
        };

        let result: serde_json::Value = runner.run(&[]).await.unwrap();
        assert_eq!(result["key"], "value");
        assert_eq!(result["num"], 42);
    }

    /// Build a stream-json result line for the given structured output JSON value.
    ///
    /// The fake Claude subprocess scripts emit this as the final line on stdout
    /// when `--output-format stream-json` is in effect.
    fn stream_result_line(structured_output: serde_json::Value) -> String {
        serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "structured_output": structured_output
        })
        .to_string()
    }

    /// Verify that `CliClaudeRunner::run_tailoring` passes `--json-schema` and `-p`
    /// args correctly, constructing the right subprocess invocation from a
    /// `(prompt, schema)` pair.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_cli_runner_run_tailoring_passes_prompt_and_schema() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let args_file = dir.path().join("captured-args.txt");
        // Script captures all args to a file, then emits a minimal valid TailoringOutput
        // wrapped in a stream-json result envelope (required by run_subprocess_streaming).
        let tailoring_json = serde_json::json!({
            "files": [],
            "skipped_adrs": [],
            "summary": {
                "total_input": 0,
                "applicable": 0,
                "not_applicable": 0,
                "files_generated": 0
            }
        });
        let result_line = stream_result_line(tailoring_json);
        let script_content = format!(
            "#!/bin/sh\nfor arg in \"$@\"; do echo \"$arg\" >> \"{}\"; done\necho '{}'\n",
            args_file.display(),
            result_line
        );
        let script = dir.path().join("fake-claude.sh");
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner = CliClaudeRunner::new(script, Duration::from_secs(10));
        let prompt = "My tailoring prompt";
        let schema = r#"{"type":"object"}"#;
        let result = runner
            .run_tailoring(prompt, schema, None, None)
            .await
            .unwrap();

        // Verify the output is deserialized correctly
        assert_eq!(result.files.len(), 0);
        assert_eq!(result.summary.total_input, 0);

        // Verify captured args contain expected flags
        let captured = std::fs::read_to_string(&args_file).unwrap();
        let lines: Vec<&str> = captured.lines().collect();

        // --print is always first
        assert_eq!(lines[0], "--print");

        // Should contain --json-schema with our schema value
        let schema_pos = lines
            .iter()
            .position(|&l| l == "--json-schema")
            .expect("--json-schema flag should be present");
        assert_eq!(lines[schema_pos + 1], schema);

        // Should contain -p with our prompt
        let prompt_pos = lines
            .iter()
            .position(|&l| l == "-p")
            .expect("-p flag should be present");
        assert_eq!(lines[prompt_pos + 1], prompt);
    }

    /// Verify that `with_max_turns` overrides the `--max-turns` arg passed to the subprocess.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_cli_runner_run_tailoring_respects_max_turns_override() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let args_file = dir.path().join("captured-args.txt");
        let tailoring_json = serde_json::json!({
            "files": [],
            "skipped_adrs": [],
            "summary": {
                "total_input": 0,
                "applicable": 0,
                "not_applicable": 0,
                "files_generated": 0
            }
        });
        let result_line = stream_result_line(tailoring_json);
        let script_content = format!(
            "#!/bin/sh\nfor arg in \"$@\"; do echo \"$arg\" >> \"{}\"; done\necho '{}'\n",
            args_file.display(),
            result_line
        );
        let script = dir.path().join("fake-claude.sh");
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner = CliClaudeRunner::new(script, Duration::from_secs(10)).with_max_turns(3);
        runner
            .run_tailoring("prompt", r#"{"type":"object"}"#, None, None)
            .await
            .unwrap();

        let captured = std::fs::read_to_string(&args_file).unwrap();
        let lines: Vec<&str> = captured.lines().collect();
        let max_turns_pos = lines
            .iter()
            .position(|&l| l == "--max-turns")
            .expect("--max-turns flag should be present");
        assert_eq!(lines[max_turns_pos + 1], "3");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_cli_runner_subprocess_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'something went wrong' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner = CliClaudeRunner::new(script, Duration::from_secs(10));
        let result: Result<serde_json::Value, _> = runner.run(&[]).await;

        let (message, stderr) = subprocess_failed(result.unwrap_err());
        assert!(message.contains("exited with code 1"));
        assert!(stderr.contains("something went wrong"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_cli_runner_invalid_json() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        std::fs::write(&script, "#!/bin/sh\necho 'not json'\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner = CliClaudeRunner::new(script, Duration::from_secs(10));
        let result: Result<serde_json::Value, _> = runner.run(&[]).await;

        assert!(matches!(result, Err(ActualError::ClaudeOutputParse(_))));
    }

    #[derive(Deserialize, PartialEq, Debug)]
    struct TestOutput {
        name: String,
        count: u32,
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_cli_runner_success() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\necho '{\"name\":\"test\",\"count\":42}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner = CliClaudeRunner::new(script, Duration::from_secs(10));
        let result: TestOutput = runner.run(&[]).await.unwrap();

        assert_eq!(result.name, "test");
        assert_eq!(result.count, 42);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_cli_runner_timeout() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 10\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner = CliClaudeRunner::new(script, Duration::from_millis(100));
        let result: Result<serde_json::Value, _> = runner.run(&[]).await;

        assert_eq!(timeout_seconds(result.unwrap_err()), 0);
    }

    #[tokio::test]
    async fn test_cli_runner_spawn_failure() {
        let runner = CliClaudeRunner::new(
            PathBuf::from("/nonexistent/binary/that/does/not/exist"),
            Duration::from_secs(10),
        );
        let result: Result<serde_json::Value, _> = runner.run(&[]).await;

        let (message, stderr) = subprocess_failed(result.unwrap_err());
        assert!(message.contains("Failed to spawn Claude Code"));
        assert!(stderr.is_empty());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_cli_runner_killed_by_signal() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        // Script kills itself with SIGKILL — no exit code available
        std::fs::write(&script, "#!/bin/sh\nkill -9 $$\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let runner = CliClaudeRunner::new(script, Duration::from_secs(10));
        let result: Result<serde_json::Value, _> = runner.run(&[]).await;

        let (message, _) = subprocess_failed(result.unwrap_err());
        assert!(message.contains("unknown"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_cli_runner_passes_args() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        // Script writes all args (one per line) to a file, then outputs valid JSON
        let args_file = dir.path().join("captured-args.txt");
        let script_content = format!(
            "#!/bin/sh\nfor arg in \"$@\"; do echo \"$arg\" >> \"{}\"; done\necho '{{\"ok\":true}}'\n",
            args_file.display()
        );
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let args = vec!["--model".to_string(), "opus".to_string()];
        let runner = CliClaudeRunner::new(script, Duration::from_secs(10));
        let result: serde_json::Value = runner.run(&args).await.unwrap();
        assert_eq!(result["ok"], true);

        // Verify captured args: first should be "--print", then our args
        let captured = std::fs::read_to_string(&args_file).unwrap();
        let lines: Vec<&str> = captured.lines().collect();
        assert_eq!(lines[0], "--print");
        assert_eq!(lines[1], "--model");
        assert_eq!(lines[2], "opus");
    }

    #[test]
    fn test_with_max_turns_overrides_default() {
        let runner = CliClaudeRunner::new(PathBuf::from("/fake/binary"), Duration::from_secs(10))
            .with_max_turns(3);
        assert_eq!(runner.max_turns_override, Some(3));
    }

    #[test]
    fn test_io_err_produces_subprocess_failed() {
        let error = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe broke");
        let (message, stderr) = subprocess_failed(io_err("Failed to wait for Claude Code", error));
        assert!(message.contains("Failed to wait for Claude Code"));
        assert!(message.contains("pipe broke"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn test_resolve_output_success() {
        // Use a real subprocess to get a valid Output with a real ExitStatus
        let output = std::process::Command::new("echo")
            .arg("hello")
            .output()
            .unwrap();
        let result = resolve_output(Ok(Ok(output)), Duration::from_secs(30));
        let output = result.unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("hello"));
    }

    #[test]
    fn test_resolve_output_io_error() {
        let io_error = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe broke");
        let result = resolve_output(Ok(Err(io_error)), Duration::from_secs(30));
        let (message, stderr) = subprocess_failed(result.unwrap_err());
        assert!(message.contains("Failed to wait for Claude Code"));
        assert!(message.contains("pipe broke"));
        assert!(stderr.is_empty());
    }

    #[tokio::test]
    async fn test_resolve_output_timeout() {
        // Create a real Elapsed error by timing out
        let elapsed = tokio::time::timeout(
            Duration::from_nanos(1),
            tokio::time::sleep(Duration::from_secs(1)),
        )
        .await
        .unwrap_err();
        let result = resolve_output(Err(elapsed), Duration::from_secs(30));
        assert_eq!(timeout_seconds(result.unwrap_err()), 30);
    }

    /// Build a fake successful `std::process::Output` with the given stdout bytes.
    ///
    /// Uses `true` (unix) or `cmd /C exit 0` (windows) to obtain a real
    /// `ExitStatus` with code 0, since `ExitStatus` has no public constructor.
    fn success_output(stdout: &[u8]) -> std::process::Output {
        #[cfg(unix)]
        let status = std::process::Command::new("true").status().unwrap();
        #[cfg(windows)]
        let status = std::process::Command::new("cmd")
            .args(["/C", "exit", "0"])
            .status()
            .unwrap();

        std::process::Output {
            status,
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        }
    }

    // ---- Envelope parsing tests ----

    #[test]
    fn test_parse_envelope_extracts_structured_output() {
        let json = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "duration_ms": 8130,
            "result": "Done!",
            "session_id": "abc-123",
            "total_cost_usd": 0.023,
            "structured_output": {
                "name": "hello",
                "count": 7
            }
        });
        let output = success_output(json.to_string().as_bytes());
        let parsed: TestOutput = parse_output(output).unwrap();
        assert_eq!(parsed.name, "hello");
        assert_eq!(parsed.count, 7);
    }

    #[test]
    fn test_parse_envelope_is_error_true() {
        let json = serde_json::json!({
            "type": "result",
            "subtype": "error",
            "is_error": true,
            "result": "Something went terribly wrong",
            "structured_output": null
        });
        let output = success_output(json.to_string().as_bytes());
        let result: Result<TestOutput, _> = parse_output(output);
        let (message, _) = subprocess_failed(result.unwrap_err());
        assert!(message.contains("returned an error"));
        assert!(message.contains("Something went terribly wrong"));
    }

    #[test]
    fn test_parse_envelope_error_max_turns() {
        let json = serde_json::json!({
            "type": "result",
            "subtype": "error_max_turns",
            "is_error": false,
            "result": "Hit the maximum number of turns",
            "structured_output": null
        });
        let output = success_output(json.to_string().as_bytes());
        let result: Result<TestOutput, _> = parse_output(output);
        let (message, _) = subprocess_failed(result.unwrap_err());
        assert!(message.contains("returned an error"));
        assert!(message.contains("Hit the maximum number of turns"));
    }

    #[test]
    fn test_parse_envelope_missing_structured_output() {
        let json = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": "Completed, but no structured output",
            "structured_output": null
        });
        let output = success_output(json.to_string().as_bytes());
        let result: Result<TestOutput, _> = parse_output(output);
        let (message, _) = subprocess_failed(result.unwrap_err());
        assert!(message.contains("missing structured_output"));
    }

    #[test]
    fn test_parse_fallback_direct_json() {
        // Raw JSON without envelope — backwards compat with test fake binaries.
        let json = r#"{"name":"direct","count":99}"#;
        let output = success_output(json.as_bytes());
        let parsed: TestOutput = parse_output(output).unwrap();
        assert_eq!(parsed.name, "direct");
        assert_eq!(parsed.count, 99);
    }

    // ── format_stream_event tests ──

    #[test]
    fn test_format_stream_event_system_init() {
        let line = r#"{"type":"system","model":"claude-sonnet-4-5"}"#;
        assert_eq!(
            format_stream_event(line),
            Some("Claude Code started (model: claude-sonnet-4-5)".to_string())
        );
    }

    #[test]
    fn test_format_stream_event_system_unknown_model() {
        let line = r#"{"type":"system"}"#;
        assert_eq!(
            format_stream_event(line),
            Some("Claude Code started (model: unknown)".to_string())
        );
    }

    #[test]
    fn test_format_stream_event_assistant_read_tool() {
        let line = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "name": "Read",
                    "input": {"file_path": "src/main.rs"}
                }]
            }
        })
        .to_string();
        assert_eq!(
            format_stream_event(&line),
            Some("Claude: Read src/main.rs".to_string())
        );
    }

    #[test]
    fn test_format_stream_event_assistant_write_tool() {
        let line = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "name": "Write",
                    "input": {"file_path": "CLAUDE.md"}
                }]
            }
        })
        .to_string();
        assert_eq!(
            format_stream_event(&line),
            Some("Claude: Write CLAUDE.md".to_string())
        );
    }

    #[test]
    fn test_format_stream_event_assistant_glob_tool() {
        let line = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "name": "Glob",
                    "input": {"pattern": "**/*.rs"}
                }]
            }
        })
        .to_string();
        assert_eq!(
            format_stream_event(&line),
            Some("Claude: Glob **/*.rs".to_string())
        );
    }

    #[test]
    fn test_format_stream_event_assistant_grep_tool() {
        let line = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "name": "Grep",
                    "input": {"pattern": "fn main"}
                }]
            }
        })
        .to_string();
        assert_eq!(
            format_stream_event(&line),
            Some("Claude: Grep fn main".to_string())
        );
    }

    #[test]
    fn test_format_stream_event_assistant_unknown_tool() {
        let line = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "name": "SomeFutureTool",
                    "input": {}
                }]
            }
        })
        .to_string();
        assert_eq!(
            format_stream_event(&line),
            Some("Claude: SomeFutureTool".to_string())
        );
    }

    #[test]
    fn test_format_stream_event_assistant_text_only_suppressed() {
        // Text-only assistant messages are suppressed (too noisy)
        let line = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "text",
                    "text": "Thinking about the task..."
                }]
            }
        })
        .to_string();
        assert_eq!(format_stream_event(&line), None);
    }

    #[test]
    fn test_format_stream_event_result_suppressed() {
        // Result events are not forwarded (handled separately)
        let line =
            r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{}}"#;
        assert_eq!(format_stream_event(line), None);
    }

    #[test]
    fn test_format_stream_event_invalid_json_suppressed() {
        assert_eq!(format_stream_event("not json at all"), None);
    }

    #[test]
    fn test_format_stream_event_multiple_tools() {
        let line = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "tool_use", "name": "Read", "input": {"file_path": "a.rs"}},
                    {"type": "tool_use", "name": "Glob", "input": {"pattern": "*.toml"}}
                ]
            }
        })
        .to_string();
        assert_eq!(
            format_stream_event(&line),
            Some("Claude: Read a.rs, Glob *.toml".to_string())
        );
    }

    // ── run_subprocess_streaming tests ──

    /// Verify that run_subprocess_streaming returns an error when the subprocess
    /// exits without emitting any result event.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_run_subprocess_streaming_no_result_event() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        // Outputs something but no result event
        std::fs::write(&script, "#!/bin/sh\necho 'hello world'\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let result: Result<serde_json::Value, _> =
            run_subprocess_streaming(&script, Duration::from_secs(5), &[], None).await;
        let (message, _) = subprocess_failed(result.unwrap_err());
        assert!(
            message.contains("exited without producing a result"),
            "unexpected message: {message}"
        );
    }

    /// Verify that run_subprocess_streaming times out correctly.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_run_subprocess_streaming_timeout() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 10\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let result: Result<serde_json::Value, _> =
            run_subprocess_streaming(&script, Duration::from_millis(100), &[], None).await;
        assert_eq!(timeout_seconds(result.unwrap_err()), 0);
    }

    /// Verify that run_subprocess_streaming forwards stderr lines to event_tx.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_run_subprocess_streaming_forwards_stderr() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        let tailoring_json = serde_json::json!({
            "files": [],
            "skipped_adrs": [],
            "summary": {
                "total_input": 0,
                "applicable": 0,
                "not_applicable": 0,
                "files_generated": 0
            }
        });
        let result_line = stream_result_line(tailoring_json);
        let script_content = format!("#!/bin/sh\necho 'err line' >&2\necho '{}'\n", result_line);
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let _result: crate::tailoring::types::TailoringOutput =
            run_subprocess_streaming(&script, Duration::from_secs(10), &[], Some(tx))
                .await
                .unwrap();

        // Collect all messages; the stderr line should be prefixed with "[stderr] "
        let mut messages = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            messages.push(msg);
        }
        assert!(
            messages.iter().any(|m| m == "[stderr] err line"),
            "expected '[stderr] err line' in messages, got: {messages:?}"
        );
    }

    /// Verify that run_subprocess_streaming returns an error for error result events.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_run_subprocess_streaming_error_result() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        let error_line = serde_json::json!({
            "type": "result",
            "subtype": "error",
            "is_error": true,
            "result": "Something went wrong",
            "structured_output": null
        })
        .to_string();
        std::fs::write(&script, format!("#!/bin/sh\necho '{}'\n", error_line)).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let result: Result<serde_json::Value, _> =
            run_subprocess_streaming(&script, Duration::from_secs(5), &[], None).await;
        let (message, _) = subprocess_failed(result.unwrap_err());
        assert!(
            message.contains("returned an error"),
            "unexpected message: {message}"
        );
        assert!(
            message.contains("Something went wrong"),
            "unexpected message: {message}"
        );
    }

    // ── set_event_tx tests ──

    /// Verify that `set_event_tx` stores the sender in the runner's mutex.
    #[test]
    fn test_set_event_tx_stores_sender() {
        let runner = CliClaudeRunner::new(PathBuf::from("/fake/binary"), Duration::from_secs(10));
        assert!(runner.event_tx.lock().unwrap().is_none(), "initially None");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        runner.set_event_tx(tx);
        assert!(
            runner.event_tx.lock().unwrap().is_some(),
            "stored after set_event_tx"
        );
    }

    /// Verify that tool-call events from the subprocess are forwarded via event_tx.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_cli_runner_streams_tool_call_events() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        let tailoring_json = serde_json::json!({
            "files": [],
            "skipped_adrs": [],
            "summary": {
                "total_input": 0,
                "applicable": 0,
                "not_applicable": 0,
                "files_generated": 0
            }
        });
        let result_line = stream_result_line(tailoring_json);
        // Emit a system event, then a tool-call event, then the result.
        let system_line = r#"{"type":"system","model":"claude-sonnet-4-5"}"#;
        let tool_line = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "name": "Read",
                    "input": {"file_path": "src/main.rs"}
                }]
            }
        })
        .to_string();
        let script_content = format!(
            "#!/bin/sh\necho '{}'\necho '{}'\necho '{}'\n",
            system_line, tool_line, result_line
        );
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner = CliClaudeRunner::new(script, Duration::from_secs(10));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        runner.set_event_tx(tx);

        let result = runner
            .run_tailoring("prompt", r#"{"type":"object"}"#, None, None)
            .await;
        assert!(result.is_ok(), "expected Ok: {result:?}");

        let mut events = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            events.push(msg);
        }
        assert!(
            events.iter().any(|e| e.contains("Claude Code started")),
            "missing system event: {events:?}"
        );
        assert!(
            events.iter().any(|e| e.contains("Read src/main.rs")),
            "missing tool-call event: {events:?}"
        );
    }
}
