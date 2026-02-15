use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use tokio::process::Command;

use crate::error::ActualError;

/// Trait for running Claude Code and deserializing output.
///
/// Generic over the output type T at the method level — any `DeserializeOwned` type
/// can be returned. This allows callers to use the same runner for different schemas
/// (e.g., `RepoAnalysis`, `TailoringOutput`).
#[async_trait]
pub trait ClaudeRunner: Send + Sync {
    async fn run<T: DeserializeOwned + Send>(&self, args: &[String]) -> Result<T, ActualError>;
}

/// Production implementation that spawns `claude --print` as a subprocess.
pub struct CliClaudeRunner {
    /// Path to the Claude CLI binary.
    binary_path: PathBuf,
    /// Maximum time to wait for subprocess completion.
    timeout: Duration,
}

impl CliClaudeRunner {
    pub fn new(binary_path: PathBuf, timeout: Duration) -> Self {
        Self {
            binary_path,
            timeout,
        }
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
fn parse_output<T: DeserializeOwned>(output: std::process::Output) -> Result<T, ActualError> {
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

    let parsed: T = serde_json::from_slice(&output.stdout)?;
    Ok(parsed)
}

#[async_trait]
impl ClaudeRunner for CliClaudeRunner {
    async fn run<T: DeserializeOwned + Send>(&self, args: &[String]) -> Result<T, ActualError> {
        let mut cmd = Command::new(&self.binary_path);
        cmd.arg("--print");
        cmd.args(args);
        // Ensure the child process is killed if the timeout fires and drops the future.
        cmd.kill_on_drop(true);

        let child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| io_err("Failed to spawn Claude Code", e))?;

        let output = match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(io_err("Failed to wait for Claude Code", e)),
            Err(_) => {
                return Err(ActualError::ClaudeTimeout {
                    seconds: self.timeout.as_secs(),
                });
            }
        };

        parse_output(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    /// Mock implementation for testing trait mockability.
    struct MockClaudeRunner {
        json_response: String,
    }

    #[async_trait]
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
    async fn test_mock_claude_runner() {
        let runner = MockClaudeRunner {
            json_response: r#"{"key": "value", "num": 42}"#.to_string(),
        };

        let result: serde_json::Value = runner.run(&[]).await.unwrap();
        assert_eq!(result["key"], "value");
        assert_eq!(result["num"], 42);
    }

    #[tokio::test]
    async fn test_cli_runner_subprocess_failure() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'something went wrong' >&2\nexit 1\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let runner = CliClaudeRunner::new(script, Duration::from_secs(10));
        let result: Result<serde_json::Value, _> = runner.run(&[]).await;

        match result {
            Err(ActualError::ClaudeSubprocessFailed { message, stderr }) => {
                assert!(
                    message.contains("exited with code 1"),
                    "expected exit code in message: {message}"
                );
                assert!(
                    stderr.contains("something went wrong"),
                    "expected stderr content: {stderr}"
                );
            }
            other => panic!("expected ClaudeSubprocessFailed, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_cli_runner_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        std::fs::write(&script, "#!/bin/sh\necho 'not json'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let runner = CliClaudeRunner::new(script, Duration::from_secs(10));
        let result: Result<serde_json::Value, _> = runner.run(&[]).await;

        assert!(
            matches!(result, Err(ActualError::ClaudeOutputParse(_))),
            "expected ClaudeOutputParse, got: {result:?}"
        );
    }

    #[derive(Deserialize, PartialEq, Debug)]
    struct TestOutput {
        name: String,
        count: u32,
    }

    #[tokio::test]
    async fn test_cli_runner_success() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\necho '{\"name\":\"test\",\"count\":42}'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let runner = CliClaudeRunner::new(script, Duration::from_secs(10));
        let result: TestOutput = runner.run(&[]).await.unwrap();

        assert_eq!(result.name, "test");
        assert_eq!(result.count, 42);
    }

    #[tokio::test]
    async fn test_cli_runner_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 10\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let runner = CliClaudeRunner::new(script, Duration::from_millis(100));
        let result: Result<serde_json::Value, _> = runner.run(&[]).await;

        match result {
            Err(ActualError::ClaudeTimeout { seconds }) => {
                assert_eq!(seconds, 0, "100ms rounds down to 0 seconds");
            }
            other => panic!("expected ClaudeTimeout, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_cli_runner_spawn_failure() {
        let runner = CliClaudeRunner::new(
            PathBuf::from("/nonexistent/binary/that/does/not/exist"),
            Duration::from_secs(10),
        );
        let result: Result<serde_json::Value, _> = runner.run(&[]).await;

        match result {
            Err(ActualError::ClaudeSubprocessFailed { message, stderr }) => {
                assert!(
                    message.contains("Failed to spawn Claude Code"),
                    "expected spawn failure in message: {message}"
                );
                assert!(stderr.is_empty(), "expected empty stderr: {stderr}");
            }
            other => panic!("expected ClaudeSubprocessFailed, got: {other:?}"),
        }
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

        match result {
            Err(ActualError::ClaudeSubprocessFailed { message, .. }) => {
                assert!(
                    message.contains("unknown"),
                    "expected 'unknown' exit code in message: {message}"
                );
            }
            other => panic!("expected ClaudeSubprocessFailed, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_cli_runner_passes_args() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude.sh");
        // Script writes all args (one per line) to a file, then outputs valid JSON
        let args_file = dir.path().join("captured-args.txt");
        let script_content = format!(
            "#!/bin/sh\nfor arg in \"$@\"; do echo \"$arg\" >> \"{}\"; done\necho '{{\"ok\":true}}'\n",
            args_file.display()
        );
        std::fs::write(&script, script_content).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

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
    fn test_io_err_produces_subprocess_failed() {
        let error = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe broke");
        let result = io_err("Failed to wait for Claude Code", error);
        match result {
            ActualError::ClaudeSubprocessFailed { message, stderr } => {
                assert!(
                    message.contains("Failed to wait for Claude Code"),
                    "expected context in message: {message}"
                );
                assert!(
                    message.contains("pipe broke"),
                    "expected error in message: {message}"
                );
                assert!(stderr.is_empty());
            }
            other => panic!("expected ClaudeSubprocessFailed, got: {other:?}"),
        }
    }
}
