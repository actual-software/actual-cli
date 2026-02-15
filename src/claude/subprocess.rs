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
    binary_path: PathBuf,
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

#[async_trait]
impl ClaudeRunner for CliClaudeRunner {
    async fn run<T: DeserializeOwned + Send>(&self, args: &[String]) -> Result<T, ActualError> {
        let mut cmd = Command::new(&self.binary_path);
        cmd.arg("--print");
        cmd.args(args);

        let child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| ActualError::ClaudeSubprocessFailed {
                message: format!("Failed to spawn Claude Code: {e}"),
                stderr: String::new(),
            })?;

        let output = match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(result) => result.map_err(|e| ActualError::ClaudeSubprocessFailed {
                message: format!("Failed to wait for Claude Code: {e}"),
                stderr: String::new(),
            })?,
            Err(_) => {
                return Err(ActualError::ClaudeTimeout {
                    seconds: self.timeout.as_secs(),
                });
            }
        };

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

        let stdout = &output.stdout;
        let parsed: T = serde_json::from_slice(stdout)?;
        Ok(parsed)
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
}
