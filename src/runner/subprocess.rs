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

            if !tool_uses.is_empty() {
                return Some(format!("Claude: {}", tool_uses.join(", ")));
            }

            // Forward text-only assistant messages so users see Claude's
            // planning and reasoning in the TUI instead of a silent spinner.
            let text: String = content
                .iter()
                .filter_map(|item| {
                    if item["type"].as_str() == Some("text") {
                        item["text"].as_str()
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");

            if text.is_empty() {
                None
            } else {
                const MAX_LEN: usize = 200;
                let display = if text.chars().count() > MAX_LEN {
                    format!("{}…", text.chars().take(MAX_LEN).collect::<String>())
                } else {
                    text
                };
                // Flatten newlines so the single log line stays readable.
                Some(format!("Claude: {}", display.replace('\n', " ")))
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
    ActualError::RunnerFailed {
        message: format!("{context}: {e}"),
        stderr: String::new(),
    }
}

/// Build a human-readable error detail from a Claude Code error envelope.
///
/// Prefers the envelope's `result` field (Claude Code's own error message).
/// When that is absent, falls back to the first non-empty line of stderr
/// (which often contains diagnostics like quota errors, rate limits, or auth
/// failures).  If both are empty, synthesises a message from the `subtype`
/// field so the user sees something more actionable than "unknown error".
fn error_detail_from_envelope<T>(envelope: &ClaudeEnvelope<T>, stderr: &str) -> String {
    // 1. Prefer the explicit result text from the envelope.
    if let Some(ref result) = envelope.result {
        if !result.is_empty() {
            return result.clone();
        }
    }

    // 2. Fall back to the first meaningful stderr line.
    let stderr_first_line = stderr
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if !stderr_first_line.is_empty() {
        return stderr_first_line.to_string();
    }

    // 3. Synthesise from subtype so the message is at least somewhat descriptive.
    match envelope.subtype.as_deref() {
        Some("error_max_turns") => "hit the maximum number of turns".to_string(),
        Some(subtype) if !subtype.is_empty() => format!("{subtype} (no further details available)"),
        _ => "unknown error (no details in result or stderr)".to_string(),
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
        return Err(ActualError::RunnerFailed {
            message: format!("Claude Code exited with code {code}"),
            stderr,
        });
    }

    // Try envelope parse first, then fall back to direct parse.
    if let Ok(envelope) = serde_json::from_slice::<ClaudeEnvelope<T>>(&output.stdout) {
        // Check for error states in the envelope.
        if envelope.is_error == Some(true) || envelope.subtype.as_deref() == Some("error_max_turns")
        {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let detail = error_detail_from_envelope(&envelope, &stderr);
            return Err(ActualError::RunnerFailed {
                message: format!("Claude Code returned an error: {detail}"),
                stderr,
            });
        }

        // Only treat as an envelope if it looks like one (has `type` = "result").
        if envelope.result_type.as_deref() == Some("result") {
            return envelope
                .structured_output
                .ok_or_else(|| ActualError::RunnerFailed {
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
        Err(_) => Err(ActualError::RunnerTimeout {
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

/// Number of consecutive identical stderr or stdout event lines that triggers
/// early abort. When a subprocess enters a pathological loop (e.g., credit
/// balance errors), it emits the same message over and over. Detecting the
/// repetition structurally avoids string-matching on specific error messages.
const REPETITIVE_LINE_THRESHOLD: usize = 3;

/// Tool-use summary strings that are exempt from repetitive output detection.
/// These are Claude Code internal tools that legitimately emit the same event
/// multiple times during normal operation (e.g., structured output extraction).
const REPETITION_EXEMPT_TOOLS: &[&str] = &["StructuredOutput"];

/// Returns `true` if the given formatted event summary should be excluded from
/// repetitive output detection. This prevents false-positive abort signals for
/// Claude Code internal mechanisms that naturally repeat.
fn is_repetition_exempt(summary: &str) -> bool {
    let raw = summary.strip_prefix("Claude: ").unwrap_or(summary);
    REPETITION_EXEMPT_TOOLS
        .iter()
        .any(|tool| raw.starts_with(tool))
}

/// Keywords that indicate a credit/billing/quota problem. When a repeated
/// message contains any of these (case-insensitive), we classify it as
/// `CreditBalanceTooLow`. Otherwise, a repetitive loop is treated as a
/// generic subprocess failure.
const CREDIT_KEYWORDS: &[&str] = &[
    "credit",
    "balance",
    "billing",
    "quota",
    "rate limit",
    "rate_limit",
    "insufficient",
    "payment",
    "subscription",
];

/// Returns `true` if the message looks like a credit/billing/quota error.
fn looks_like_credit_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    CREDIT_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

/// Classify an error result from the Claude Code subprocess into a more
/// specific `ActualError` variant when possible.
///
/// When the subprocess entered a repetitive error loop (detected by
/// `repetitive_message` being `Some`), we check whether the repeated message
/// looks like a credit/billing error. Only messages containing credit-related
/// keywords are mapped to `CreditBalanceTooLow`; other repetitive loops
/// produce a generic `RunnerFailed` error.
fn classify_subprocess_error(
    subtype: Option<&str>,
    result_text: &str,
    stderr: &str,
    repetitive_message: Option<&str>,
) -> ActualError {
    // If we detected a repetitive loop, check whether the message is
    // credit-related before assuming it's a billing problem.
    if let Some(repeated_msg) = repetitive_message {
        if looks_like_credit_error(repeated_msg) {
            return ActualError::CreditBalanceTooLow {
                message: repeated_msg.to_string(),
            };
        }
        // The subprocess was stuck in a loop but the message doesn't look
        // credit-related — surface it as a generic subprocess failure.
        return ActualError::RunnerFailed {
            message: format!("Claude Code entered a repetitive loop: {repeated_msg}"),
            stderr: stderr.to_string(),
        };
    }

    // Check for the error_max_turns subtype — this is always a generic error.
    if subtype == Some("error_max_turns") {
        return ActualError::RunnerFailed {
            message: format!("Claude Code returned an error: {result_text}"),
            stderr: stderr.to_string(),
        };
    }

    // Generic subprocess failure — covers `is_error == true` without a special
    // subtype as well as any other unexpected combination.
    ActualError::RunnerFailed {
        message: format!("Claude Code returned an error: {result_text}"),
        stderr: stderr.to_string(),
    }
}

/// Spawn the binary in streaming mode (`--output-format stream-json`), read
/// stdout line-by-line, forward formatted event summaries to `event_tx`, and
/// return the structured output extracted from the final `"type":"result"` event.
///
/// Stderr is drained concurrently in a background task and each line is
/// forwarded to `event_tx` prefixed with `"[stderr] "` so callers see it in
/// the same channel as tool-call events.
///
/// **Repetitive output detection:** Both the stderr drainer and the stdout
/// event loop track consecutive identical messages. When the same message
/// appears [`REPETITIVE_LINE_THRESHOLD`] times in a row, the subprocess is
/// killed and a [`ActualError::CreditBalanceTooLow`] error is returned. This
/// detects pathological loops (e.g., credit balance errors) structurally
/// without string-matching on specific error messages.
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

    // Shared flag: set by either the stderr drainer or the stdout reader when
    // they detect a repetitive loop. The child is killed from the main select
    // loop when this fires.
    let repetitive_abort = std::sync::Arc::new(tokio::sync::Notify::new());
    // Stores the repeated message for error reporting.
    let repetitive_msg: std::sync::Arc<std::sync::Mutex<Option<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));

    // Drain stderr in a background task; forward each line to event_tx.
    // Consecutive duplicate lines are suppressed after the first occurrence
    // to avoid flooding the TUI with identical error messages.
    let event_tx_for_stderr = event_tx.clone();
    let abort_for_stderr = repetitive_abort.clone();
    let msg_for_stderr = repetitive_msg.clone();
    let stderr_task: tokio::task::JoinHandle<Vec<u8>> = tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        let mut all_bytes: Vec<u8> = Vec::new();
        let mut last_line: Option<String> = None;
        let mut repeat_count: usize = 0;
        while let Ok(Some(line)) = lines.next_line().await {
            // Track consecutive duplicates.
            let is_repeat = last_line.as_deref() == Some(&line);
            if is_repeat {
                repeat_count += 1;
            } else {
                repeat_count = 1;
                last_line = Some(line.clone());
            }

            // Only forward the first occurrence to avoid TUI spam.
            if !is_repeat {
                if let Some(tx) = &event_tx_for_stderr {
                    let _ = tx.send(format!("[stderr] {line}"));
                }
            }

            all_bytes.extend_from_slice(line.as_bytes());
            all_bytes.push(b'\n');

            // Signal abort if we detect a pathological loop.
            if repeat_count >= REPETITIVE_LINE_THRESHOLD {
                *msg_for_stderr
                    .lock()
                    .expect("repetitive_msg mutex poisoned") = last_line.clone();
                abort_for_stderr.notify_one();
                break;
            }
        }
        all_bytes
    });

    // Read stdout line-by-line, forwarding formatted summaries and looking for
    // the final "type":"result" event. Consecutive duplicate summaries are
    // suppressed and trigger early abort when the threshold is reached.
    let abort_for_stdout = repetitive_abort.clone();
    let msg_for_stdout = repetitive_msg.clone();
    let find_result = async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        let mut last_summary: Option<String> = None;
        let mut repeat_count: usize = 0;
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(tx) = &event_tx {
                if let Some(summary) = format_stream_event(&line) {
                    // Skip repetitive detection for known Claude Code
                    // internal tool names that legitimately repeat during
                    // normal operation (e.g., structured output extraction).
                    let exempt = is_repetition_exempt(&summary);

                    let is_repeat = last_summary.as_deref() == Some(&summary);
                    if is_repeat {
                        repeat_count += 1;
                    } else {
                        repeat_count = 1;
                        last_summary = Some(summary.clone());
                        // Only forward the first occurrence.
                        let _ = tx.send(summary);
                    }

                    // Signal abort if the subprocess is stuck in a loop,
                    // but only for non-exempt messages.
                    if !exempt && repeat_count >= REPETITIVE_LINE_THRESHOLD {
                        // Extract the raw message (strip "Claude: " prefix if present).
                        let raw = last_summary
                            .as_deref()
                            .unwrap_or("unknown error")
                            .strip_prefix("Claude: ")
                            .unwrap_or(last_summary.as_deref().unwrap_or("unknown error"));
                        *msg_for_stdout
                            .lock()
                            .expect("repetitive_msg mutex poisoned") = Some(raw.to_string());
                        abort_for_stdout.notify_one();
                        return None;
                    }
                }
            }
            if let Ok(envelope) = serde_json::from_str::<ClaudeEnvelope<T>>(&line) {
                if envelope.result_type.as_deref() == Some("result") {
                    return Some(envelope);
                }
            }
        }
        None
    };

    // Race: result extraction vs timeout vs repetitive-abort.
    tokio::pin!(find_result);
    let result = tokio::select! {
        r = tokio::time::timeout(timeout, &mut find_result) => r,
        _ = repetitive_abort.notified() => {
            // Kill the child process immediately to stop the loop.
            let _ = child.kill().await;
            // The find_result future will end when stdout closes after kill.
            // We don't need its result — we already know this is a repetitive error.
            Ok(None)
        }
    };

    match result {
        Ok(Some(envelope)) => {
            let repeated = repetitive_msg
                .lock()
                .expect("repetitive_msg mutex poisoned")
                .clone();
            if envelope.is_error == Some(true)
                || envelope.subtype.as_deref() == Some("error_max_turns")
            {
                let stderr_bytes = stderr_task.await.unwrap_or_default();
                let stderr_str = String::from_utf8_lossy(&stderr_bytes).to_string();
                let detail = error_detail_from_envelope(&envelope, &stderr_str);
                Err(classify_subprocess_error(
                    envelope.subtype.as_deref(),
                    &detail,
                    &stderr_str,
                    repeated.as_deref(),
                ))
            } else {
                // Await stderr_task so its channel sends complete before we return,
                // ensuring tests (and callers) see all stderr events.
                let _ = stderr_task.await;
                envelope
                    .structured_output
                    .ok_or_else(|| ActualError::RunnerFailed {
                        message: "Claude Code envelope missing structured_output field".to_string(),
                        stderr: String::new(),
                    })
            }
        }
        Ok(None) => {
            let stderr_bytes = stderr_task.await.unwrap_or_default();
            let stderr_str = String::from_utf8_lossy(&stderr_bytes).to_string();
            let repeated = repetitive_msg
                .lock()
                .expect("repetitive_msg mutex poisoned")
                .clone();
            Err(classify_subprocess_error(
                None,
                "Claude Code exited without producing a result",
                &stderr_str,
                repeated.as_deref(),
            ))
        }
        Err(_) => {
            // kill_on_drop fires when `child` goes out of scope at function
            // return; await the stderr drainer so any last-gasp diagnostic
            // lines are flushed through event_tx before we return.
            let _ = stderr_task.await;
            Err(ActualError::RunnerTimeout {
                seconds: timeout.as_secs(),
            })
        }
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

        let event_tx = self
            .event_tx
            .lock()
            .expect("event_tx mutex poisoned")
            .clone();
        run_subprocess_streaming(&self.binary_path, self.timeout, &args, event_tx).await
    }

    fn set_event_tx(&self, tx: UnboundedSender<String>) {
        *self.event_tx.lock().expect("event_tx mutex poisoned") = Some(tx);
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

    /// Extract `RunnerFailed` fields from an `ActualError`, panicking on mismatch.
    #[rustfmt::skip]
    fn subprocess_failed(e: ActualError) -> (String, String) {
        let ActualError::RunnerFailed { message, stderr } = e else { unreachable!() };
        (message, stderr)
    }

    /// Extract `RunnerTimeout` seconds from an `ActualError`, panicking on mismatch.
    #[rustfmt::skip]
    fn timeout_seconds(e: ActualError) -> u64 {
        let ActualError::RunnerTimeout { seconds } = e else { unreachable!() };
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
        assert!(matches!(result, Err(ActualError::RunnerOutputParse(_))));
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

        assert!(matches!(result, Err(ActualError::RunnerOutputParse(_))));
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

    /// Build a fake successful `std::process::Output` with the given stdout
    /// and stderr bytes.
    fn success_output_with_stderr(stdout: &[u8], stderr: &[u8]) -> std::process::Output {
        let mut out = success_output(stdout);
        out.stderr = stderr.to_vec();
        out
    }

    // ── error_detail_from_envelope tests ──

    #[test]
    fn test_error_detail_prefers_result_field() {
        let json = serde_json::json!({
            "type": "result",
            "subtype": "error",
            "is_error": true,
            "result": "Rate limit exceeded",
            "structured_output": null
        });
        let envelope: ClaudeEnvelope<TestOutput> = serde_json::from_value(json).unwrap();
        let detail = error_detail_from_envelope(&envelope, "some stderr noise");
        assert_eq!(detail, "Rate limit exceeded");
    }

    #[test]
    fn test_error_detail_falls_back_to_stderr_when_result_null() {
        let json = serde_json::json!({
            "type": "result",
            "subtype": "error",
            "is_error": true,
            "result": null,
            "structured_output": null
        });
        let envelope: ClaudeEnvelope<TestOutput> = serde_json::from_value(json).unwrap();
        let detail = error_detail_from_envelope(&envelope, "Error: quota exceeded\n");
        assert_eq!(detail, "Error: quota exceeded");
    }

    #[test]
    fn test_error_detail_falls_back_to_stderr_when_result_absent() {
        let json = serde_json::json!({
            "type": "result",
            "subtype": "error",
            "is_error": true,
            "structured_output": null
        });
        let envelope: ClaudeEnvelope<TestOutput> = serde_json::from_value(json).unwrap();
        let detail = error_detail_from_envelope(&envelope, "\n  Auth failure: invalid token\n");
        assert_eq!(detail, "Auth failure: invalid token");
    }

    #[test]
    fn test_error_detail_falls_back_to_subtype_when_no_result_or_stderr() {
        let json = serde_json::json!({
            "type": "result",
            "subtype": "error",
            "is_error": true,
            "result": null,
            "structured_output": null
        });
        let envelope: ClaudeEnvelope<TestOutput> = serde_json::from_value(json).unwrap();
        let detail = error_detail_from_envelope(&envelope, "");
        assert_eq!(detail, "error (no further details available)");
    }

    #[test]
    fn test_error_detail_max_turns_subtype_without_result() {
        let json = serde_json::json!({
            "type": "result",
            "subtype": "error_max_turns",
            "is_error": false,
            "result": null,
            "structured_output": null
        });
        let envelope: ClaudeEnvelope<TestOutput> = serde_json::from_value(json).unwrap();
        let detail = error_detail_from_envelope(&envelope, "");
        assert_eq!(detail, "hit the maximum number of turns");
    }

    #[test]
    fn test_error_detail_completely_empty() {
        let json = serde_json::json!({
            "type": "result",
            "is_error": true,
            "structured_output": null
        });
        let envelope: ClaudeEnvelope<TestOutput> = serde_json::from_value(json).unwrap();
        let detail = error_detail_from_envelope(&envelope, "");
        assert_eq!(detail, "unknown error (no details in result or stderr)");
    }

    #[test]
    fn test_error_detail_empty_result_string_falls_through() {
        let json = serde_json::json!({
            "type": "result",
            "subtype": "error",
            "is_error": true,
            "result": "",
            "structured_output": null
        });
        let envelope: ClaudeEnvelope<TestOutput> = serde_json::from_value(json).unwrap();
        let detail = error_detail_from_envelope(&envelope, "stderr: something");
        assert_eq!(detail, "stderr: something");
    }

    #[test]
    fn test_parse_envelope_is_error_null_result_with_stderr() {
        // Integration test: parse_output should include stderr in the error
        // detail when the envelope result field is null.
        let json = serde_json::json!({
            "type": "result",
            "subtype": "error",
            "is_error": true,
            "result": null,
            "structured_output": null
        });
        let output = success_output_with_stderr(
            json.to_string().as_bytes(),
            b"Error: API rate limit exceeded\n",
        );
        let result: Result<TestOutput, _> = parse_output(output);
        let (message, stderr) = subprocess_failed(result.unwrap_err());
        assert!(
            message.contains("API rate limit exceeded"),
            "expected stderr content in message, got: {message}"
        );
        assert!(
            stderr.contains("API rate limit exceeded"),
            "expected stderr to be preserved, got: {stderr}"
        );
    }

    #[test]
    fn test_parse_envelope_is_error_null_result_no_stderr() {
        // When both result and stderr are empty, the subtype should appear.
        let json = serde_json::json!({
            "type": "result",
            "subtype": "error",
            "is_error": true,
            "result": null,
            "structured_output": null
        });
        let output = success_output(json.to_string().as_bytes());
        let result: Result<TestOutput, _> = parse_output(output);
        let (message, _) = subprocess_failed(result.unwrap_err());
        assert!(
            message.contains("error (no further details available)"),
            "expected subtype-based fallback in message, got: {message}"
        );
    }

    // ── format_stream_event tests ──

    #[test]
    fn test_format_stream_event_system_init() {
        let line = r#"{"type":"system","model":"claude-sonnet-4-6"}"#;
        assert_eq!(
            format_stream_event(line),
            Some("Claude Code started (model: claude-sonnet-4-6)".to_string())
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
    fn test_format_stream_event_assistant_text_forwarded() {
        // Text-only assistant messages are forwarded so users see Claude's reasoning.
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
        assert_eq!(
            format_stream_event(&line),
            Some("Claude: Thinking about the task...".to_string())
        );
    }

    #[test]
    fn test_format_stream_event_assistant_text_truncated_at_200() {
        // Text longer than 200 chars is truncated with an ellipsis.
        let long_text = "A".repeat(300);
        let line = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{"type": "text", "text": long_text}]
            }
        })
        .to_string();
        let result = format_stream_event(&line).unwrap();
        assert!(result.starts_with("Claude: "), "expected 'Claude: ' prefix");
        assert!(result.ends_with('…'), "expected ellipsis suffix");
        // "Claude: " (8) + 200 chars + "…" (1 char, but multi-byte) = 209 chars + 2 bytes
        assert_eq!(result.chars().count(), 8 + 200 + 1);
    }

    #[test]
    fn test_format_stream_event_assistant_text_newlines_flattened() {
        // Newlines in text are replaced with spaces so the TUI log line stays readable.
        let line = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{"type": "text", "text": "Line one\nLine two\nLine three"}]
            }
        })
        .to_string();
        assert_eq!(
            format_stream_event(&line),
            Some("Claude: Line one Line two Line three".to_string())
        );
    }

    #[test]
    fn test_format_stream_event_assistant_tool_takes_priority_over_text() {
        // When a message has both tool_use and text content, tool_use is shown.
        let line = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "text", "text": "I will read the file."},
                    {"type": "tool_use", "name": "Read", "input": {"file_path": "src/main.rs"}}
                ]
            }
        })
        .to_string();
        assert_eq!(
            format_stream_event(&line),
            Some("Claude: Read src/main.rs".to_string())
        );
    }

    #[test]
    fn test_format_stream_event_assistant_non_text_items_skipped_in_text_pass() {
        // Non-text, non-tool_use items (e.g. "thinking" blocks) are skipped when
        // collecting text; only real "text" blocks contribute to the output.
        let line = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "thinking", "thinking": "internal reasoning"},
                    {"type": "text", "text": "Here is my answer."}
                ]
            }
        })
        .to_string();
        assert_eq!(
            format_stream_event(&line),
            Some("Claude: Here is my answer.".to_string())
        );
    }

    #[test]
    fn test_format_stream_event_assistant_no_text_no_tool_suppressed() {
        // Content with only non-text, non-tool_use items yields no displayable
        // text and should be suppressed (returns None).
        let line = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "thinking", "thinking": "internal reasoning only"}
                ]
            }
        })
        .to_string();
        assert_eq!(format_stream_event(&line), None);
    }

    #[test]
    fn test_format_stream_event_assistant_multiedit_tool() {
        let line = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "name": "MultiEdit",
                    "input": {"file_path": "src/lib.rs"}
                }]
            }
        })
        .to_string();
        assert_eq!(
            format_stream_event(&line),
            Some("Claude: MultiEdit src/lib.rs".to_string())
        );
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
        assert!(messages.iter().any(|m| m == "[stderr] err line"));
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
        assert!(message.contains("returned an error"));
        assert!(message.contains("Something went wrong"));
    }

    /// Verify that run_subprocess_streaming returns an error when the result event
    /// has no structured_output field.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_run_subprocess_streaming_missing_structured_output() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        // Emit a success result envelope but with null structured_output
        let result_line = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": "",
            "structured_output": null
        })
        .to_string();
        std::fs::write(&script, format!("#!/bin/sh\necho '{}'\n", result_line)).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let result: Result<serde_json::Value, _> =
            run_subprocess_streaming(&script, Duration::from_secs(5), &[], None).await;
        let (message, _) = subprocess_failed(result.unwrap_err());
        assert!(message.contains("missing structured_output"));
    }

    // ── set_event_tx tests ──

    /// Verify that `set_event_tx` stores the sender in the runner's mutex.
    #[test]
    fn test_set_event_tx_stores_sender() {
        let runner = CliClaudeRunner::new(PathBuf::from("/fake/binary"), Duration::from_secs(10));
        assert!(runner.event_tx.lock().unwrap().is_none(), "initially None");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        runner.set_event_tx(tx);
        assert!(runner.event_tx.lock().unwrap().is_some());
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
        let system_line = r#"{"type":"system","model":"claude-sonnet-4-6"}"#;
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
        assert!(events.iter().any(|e| e.contains("Claude Code started")));
        assert!(events.iter().any(|e| e.contains("Read src/main.rs")));
    }

    // ── repetitive output detection tests ──

    /// Verify that repetitive stderr lines trigger early abort and return
    /// `CreditBalanceTooLow`. The subprocess emits the same stderr message
    /// REPETITIVE_LINE_THRESHOLD+ times.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_run_subprocess_streaming_repetitive_stderr_aborts() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        // Emit the same stderr line many times, then sleep (simulating a stuck process).
        // The repetitive detection should kill it before the sleep completes.
        let repeat_count = REPETITIVE_LINE_THRESHOLD + 5;
        let mut script_content = "#!/bin/sh\n".to_string();
        for _ in 0..repeat_count {
            script_content.push_str("echo 'Credit balance is too low' >&2\n");
        }
        script_content.push_str("sleep 30\n"); // would timeout without early abort
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let result: Result<serde_json::Value, _> =
            run_subprocess_streaming(&script, Duration::from_secs(10), &[], Some(tx)).await;

        let err = result.unwrap_err();
        assert!(
            matches!(err, ActualError::CreditBalanceTooLow { .. }),
            "expected CreditBalanceTooLow, got: {err:?}"
        );

        // Only the first stderr line should be forwarded (deduplication).
        let mut messages = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            messages.push(msg);
        }
        let stderr_msgs: Vec<_> = messages
            .iter()
            .filter(|m| m.starts_with("[stderr]"))
            .collect();
        assert_eq!(
            stderr_msgs.len(),
            1,
            "expected exactly 1 deduplicated stderr message, got: {stderr_msgs:?}"
        );
    }

    /// Verify that non-repetitive stderr lines are all forwarded normally.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_run_subprocess_streaming_non_repetitive_stderr_forwarded() {
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
        // Emit different stderr lines — should all be forwarded.
        let script_content = format!(
            "#!/bin/sh\necho 'line A' >&2\necho 'line B' >&2\necho 'line C' >&2\necho '{}'\n",
            result_line
        );
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let _result: crate::tailoring::types::TailoringOutput =
            run_subprocess_streaming(&script, Duration::from_secs(10), &[], Some(tx))
                .await
                .unwrap();

        let mut messages = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            messages.push(msg);
        }
        let stderr_msgs: Vec<_> = messages
            .iter()
            .filter(|m| m.starts_with("[stderr]"))
            .collect();
        assert_eq!(
            stderr_msgs.len(),
            3,
            "expected 3 distinct stderr messages, got: {stderr_msgs:?}"
        );
    }

    /// Verify that repetitive stdout assistant messages trigger early abort.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_run_subprocess_streaming_repetitive_stdout_aborts() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        // Emit the same assistant text message many times on stdout.
        let repeat_msg = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{"type": "text", "text": "Credit balance is too low"}]
            }
        })
        .to_string();
        let repeat_count = REPETITIVE_LINE_THRESHOLD + 5;
        let mut script_content = "#!/bin/sh\n".to_string();
        for _ in 0..repeat_count {
            script_content.push_str(&format!("echo '{}'\n", repeat_msg));
        }
        script_content.push_str("sleep 30\n");
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let result: Result<serde_json::Value, _> =
            run_subprocess_streaming(&script, Duration::from_secs(10), &[], Some(tx)).await;

        let err = result.unwrap_err();
        assert!(
            matches!(err, ActualError::CreditBalanceTooLow { .. }),
            "expected CreditBalanceTooLow, got: {err:?}"
        );

        // Only the first message should be forwarded.
        let mut messages = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            messages.push(msg);
        }
        let credit_msgs: Vec<_> = messages
            .iter()
            .filter(|m| m.contains("Credit balance"))
            .collect();
        assert_eq!(
            credit_msgs.len(),
            1,
            "expected exactly 1 deduplicated stdout message, got: {credit_msgs:?}"
        );
    }

    /// Verify that the `classify_subprocess_error` function correctly maps
    /// a repetitive message to `CreditBalanceTooLow`.
    #[test]
    fn test_classify_subprocess_error_with_repetitive_msg() {
        let err = classify_subprocess_error(
            Some("error"),
            "error detail",
            "",
            Some("Credit balance is too low"),
        );
        assert!(
            matches!(err, ActualError::CreditBalanceTooLow { .. }),
            "expected CreditBalanceTooLow, got: {err:?}"
        );
    }

    /// Verify that without a repetitive message, `classify_subprocess_error`
    /// returns `RunnerFailed`.
    #[test]
    fn test_classify_subprocess_error_without_repetitive_msg() {
        let err =
            classify_subprocess_error(Some("error"), "Something went wrong", "stderr output", None);
        assert!(
            matches!(err, ActualError::RunnerFailed { .. }),
            "expected RunnerFailed, got: {err:?}"
        );
    }

    /// Verify that `error_max_turns` subtype always maps to `RunnerFailed`
    /// even with a repetitive message.
    #[test]
    fn test_classify_subprocess_error_max_turns_not_credit() {
        // error_max_turns should NOT map to CreditBalanceTooLow.
        let err = classify_subprocess_error(Some("error_max_turns"), "Hit max turns", "", None);
        assert!(
            matches!(err, ActualError::RunnerFailed { .. }),
            "expected RunnerFailed for error_max_turns, got: {err:?}"
        );
    }

    /// A repetitive message that does NOT look like a credit error should
    /// produce `RunnerFailed`, not `CreditBalanceTooLow`.
    #[test]
    fn test_classify_subprocess_error_non_credit_repetitive_msg() {
        let err =
            classify_subprocess_error(Some("error"), "error detail", "", Some("StructuredOutput"));
        assert!(
            matches!(err, ActualError::RunnerFailed { .. }),
            "expected RunnerFailed for non-credit repetitive msg, got: {err:?}"
        );
        // Verify the error message includes the repeated message.
        let msg = err.to_string();
        assert!(
            msg.contains("StructuredOutput"),
            "expected repeated msg in error: {msg}"
        );
        assert!(
            msg.contains("repetitive loop"),
            "expected 'repetitive loop' in error: {msg}"
        );
    }

    // ── looks_like_credit_error tests ──

    #[test]
    fn test_looks_like_credit_error_positive() {
        assert!(looks_like_credit_error("Credit balance is too low"));
        assert!(looks_like_credit_error("Insufficient credits remaining"));
        assert!(looks_like_credit_error("Rate limit exceeded"));
        assert!(looks_like_credit_error("rate_limit_error"));
        assert!(looks_like_credit_error("Please update your billing info"));
        assert!(looks_like_credit_error("Payment required"));
        assert!(looks_like_credit_error("Your subscription has expired"));
        assert!(looks_like_credit_error("QUOTA exceeded for model"));
    }

    #[test]
    fn test_looks_like_credit_error_negative() {
        assert!(!looks_like_credit_error("StructuredOutput"));
        assert!(!looks_like_credit_error("Read src/main.rs"));
        assert!(!looks_like_credit_error("Something went wrong"));
        assert!(!looks_like_credit_error("timeout"));
        assert!(!looks_like_credit_error(""));
    }

    // ── is_repetition_exempt tests ──

    #[test]
    fn test_is_repetition_exempt_structured_output() {
        assert!(is_repetition_exempt("Claude: StructuredOutput"));
        assert!(is_repetition_exempt("StructuredOutput"));
    }

    #[test]
    fn test_is_repetition_exempt_non_exempt() {
        assert!(!is_repetition_exempt("Claude: Read src/main.rs"));
        assert!(!is_repetition_exempt("Claude: Credit balance is too low"));
        assert!(!is_repetition_exempt("some random message"));
    }
}
