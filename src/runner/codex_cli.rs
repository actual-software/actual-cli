use std::path::PathBuf;
use std::time::Duration;

use tokio::process::Command;

use crate::error::ActualError;
use crate::tailoring::types::TailoringOutput;

use super::subprocess::{parse_output, resolve_output, TailoringRunner};

/// The default binary name to search for on PATH.
const CODEX_BINARY_NAME: &str = "codex";

/// Environment variable that overrides the default binary lookup.
/// Useful for testing and CI environments.
const CODEX_BINARY_ENV: &str = "CODEX_BINARY";

/// Production implementation that spawns `codex` as a subprocess.
///
/// Codex CLI (`@openai/codex`) is OpenAI's open-source CLI tool. This runner
/// invokes it in non-interactive mode using `exec --approval-mode full-auto -q`
/// (analogous to Claude Code's `--print -p`).
///
/// # Invocation
///
/// ```text
/// codex exec --approval-mode full-auto -q "<prompt>" \
///     --model <model> \
///     --json-schema <schema>
/// ```
///
/// Output is expected to be raw JSON written to stdout matching the
/// `TailoringOutput` schema. The `max_budget_usd` parameter is not
/// forwarded because Codex CLI does not expose a cost-limit flag.
pub struct CodexCliRunner {
    /// Path to the Codex CLI binary.
    binary_path: PathBuf,
    /// Default model to use (e.g. `"codex-mini-latest"`, `"o4-mini"`).
    model: String,
    /// Maximum time to wait for subprocess completion.
    timeout: Duration,
}

impl CodexCliRunner {
    pub fn new(binary_path: PathBuf, model: String, timeout: Duration) -> Self {
        Self {
            binary_path,
            model,
            timeout,
        }
    }
}

/// Locate the Codex CLI binary on the system.
///
/// Resolution order:
/// 1. If `CODEX_BINARY` env var is set, use that path directly.
/// 2. Otherwise, search PATH for `codex` using `which::which`.
///
/// Returns the absolute path to the binary on success, or
/// `ActualError::CodexNotFound` if the binary cannot be located.
pub fn find_codex_binary() -> Result<PathBuf, ActualError> {
    if let Ok(path) = std::env::var(CODEX_BINARY_ENV) {
        return Ok(PathBuf::from(path));
    }
    which::which(CODEX_BINARY_NAME).map_err(|_| ActualError::CodexNotFound)
}

/// Convert an I/O error from subprocess operations into an `ActualError`.
fn io_err(context: &str, e: std::io::Error) -> ActualError {
    ActualError::ClaudeSubprocessFailed {
        message: format!("{context}: {e}"),
        stderr: String::new(),
    }
}

/// Build the argument list for the Codex CLI invocation.
///
/// Produces: `exec --approval-mode full-auto -q <prompt> --model <model> --json-schema <schema>`
///
/// The `exec` subcommand runs Codex in non-interactive mode.
/// `--approval-mode full-auto` suppresses all confirmation prompts.
/// `-q` requests quiet / non-interactive output.
fn build_codex_args(prompt: &str, schema: &str, model: &str) -> Vec<String> {
    vec![
        "exec".to_string(),
        "--approval-mode".to_string(),
        "full-auto".to_string(),
        "-q".to_string(),
        prompt.to_string(),
        "--model".to_string(),
        model.to_string(),
        "--json-schema".to_string(),
        schema.to_string(),
    ]
}

/// Spawn the Codex binary, wait with timeout, and parse JSON output.
async fn run_codex_subprocess(
    binary_path: &std::path::Path,
    timeout: Duration,
    args: &[String],
) -> Result<TailoringOutput, ActualError> {
    let mut cmd = Command::new(binary_path);
    cmd.args(args);
    // When the timeout fires, the `wait_with_output` future is dropped, which drops
    // the `Child` it owns. With `kill_on_drop(true)`, tokio sends SIGKILL to the child
    // on drop, preventing orphaned processes.
    cmd.kill_on_drop(true);

    let child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| io_err("Failed to spawn Codex CLI", e))?;

    let result = tokio::time::timeout(timeout, child.wait_with_output()).await;
    let output = resolve_output(result, timeout)?;

    parse_output(output)
}

impl TailoringRunner for CodexCliRunner {
    async fn run_tailoring(
        &self,
        prompt: &str,
        schema: &str,
        model_override: Option<&str>,
        _max_budget_usd: Option<f64>,
    ) -> Result<TailoringOutput, ActualError> {
        let model = model_override.unwrap_or(&self.model);
        let args = build_codex_args(prompt, schema, model);
        run_codex_subprocess(&self.binary_path, self.timeout, &args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

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

    /// Build a minimal valid `TailoringOutput` JSON string.
    fn minimal_tailoring_json() -> String {
        serde_json::json!({
            "files": [],
            "skipped_adrs": [],
            "summary": {
                "total_input": 0,
                "applicable": 0,
                "not_applicable": 0,
                "files_generated": 0
            }
        })
        .to_string()
    }

    // ---- Test 1: valid JSON output is parsed correctly ----

    #[tokio::test]
    #[cfg(unix)]
    async fn test_valid_json_parsed_correctly() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-codex.sh");
        let script_content = format!("#!/bin/sh\necho '{}'\n", minimal_tailoring_json());
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner = CodexCliRunner::new(
            script,
            "codex-mini-latest".to_string(),
            Duration::from_secs(10),
        );
        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await
            .unwrap();

        assert_eq!(result.files.len(), 0);
        assert_eq!(result.summary.total_input, 0);
    }

    // ---- Test 2: non-zero exit maps to ClaudeSubprocessFailed ----

    #[tokio::test]
    #[cfg(unix)]
    async fn test_nonzero_exit_maps_to_subprocess_failed() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-codex.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'something went wrong' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner = CodexCliRunner::new(
            script,
            "codex-mini-latest".to_string(),
            Duration::from_secs(10),
        );
        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await;

        let (message, stderr) = subprocess_failed(result.unwrap_err());
        assert!(message.contains("exited with code 1"), "message: {message}");
        assert!(stderr.contains("something went wrong"), "stderr: {stderr}");
    }

    // ---- Test 3: timeout fires correctly ----

    #[tokio::test]
    #[cfg(unix)]
    async fn test_timeout_fires() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-codex.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 10\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner = CodexCliRunner::new(
            script,
            "codex-mini-latest".to_string(),
            Duration::from_millis(100),
        );
        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await;

        // 100ms timeout → 0 seconds when truncated
        assert_eq!(timeout_seconds(result.unwrap_err()), 0);
    }

    // ---- Test 4: find_codex_binary respects CODEX_BINARY env var ----

    #[test]
    fn test_find_codex_binary_respects_env_var() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("CODEX_BINARY", "/some/path/to/codex");
        let result = find_codex_binary();
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), PathBuf::from("/some/path/to/codex"));
    }

    // ---- Test 5: find_codex_binary returns CodexNotFound when not in PATH ----

    #[test]
    fn test_find_codex_binary_not_found() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("CODEX_BINARY");
        let _g2 = EnvGuard::set("PATH", "");
        let result = find_codex_binary();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::CodexNotFound));
    }

    // ---- Additional: model_override is forwarded in args ----

    #[tokio::test]
    #[cfg(unix)]
    async fn test_model_override_forwarded() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let args_file = dir.path().join("captured-args.txt");
        let script_content = format!(
            "#!/bin/sh\nfor arg in \"$@\"; do echo \"$arg\" >> \"{}\"; done\necho '{}'\n",
            args_file.display(),
            minimal_tailoring_json()
        );
        let script = dir.path().join("fake-codex.sh");
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner = CodexCliRunner::new(
            script,
            "codex-mini-latest".to_string(),
            Duration::from_secs(10),
        );
        runner
            .run_tailoring("prompt", r#"{"type":"object"}"#, Some("o4-mini"), None)
            .await
            .unwrap();

        let captured = std::fs::read_to_string(&args_file).unwrap();
        let lines: Vec<&str> = captured.lines().collect();

        // Verify exec subcommand
        assert_eq!(lines[0], "exec");

        // Verify --model flag uses override
        let model_pos = lines
            .iter()
            .position(|&l| l == "--model")
            .expect("--model flag should be present");
        assert_eq!(lines[model_pos + 1], "o4-mini");

        // Verify --json-schema is present
        assert!(
            lines.iter().any(|&l| l == "--json-schema"),
            "--json-schema flag missing"
        );

        // Verify prompt is present
        assert!(
            lines.iter().any(|&l| l == "prompt"),
            "prompt missing from args"
        );
    }

    // ---- Additional: build_codex_args produces correct structure ----

    #[test]
    fn test_build_codex_args() {
        let args = build_codex_args("my prompt", r#"{"type":"object"}"#, "o4-mini");
        assert_eq!(args[0], "exec");
        assert_eq!(args[1], "--approval-mode");
        assert_eq!(args[2], "full-auto");
        assert_eq!(args[3], "-q");
        assert_eq!(args[4], "my prompt");
        assert_eq!(args[5], "--model");
        assert_eq!(args[6], "o4-mini");
        assert_eq!(args[7], "--json-schema");
        assert_eq!(args[8], r#"{"type":"object"}"#);
    }

    // ---- Additional: spawn failure (bad binary path) ----

    #[tokio::test]
    async fn test_spawn_failure() {
        let runner = CodexCliRunner::new(
            PathBuf::from("/nonexistent/codex/binary"),
            "codex-mini-latest".to_string(),
            Duration::from_secs(10),
        );
        let result = runner
            .run_tailoring("prompt", r#"{"type":"object"}"#, None, None)
            .await;

        let (message, stderr) = subprocess_failed(result.unwrap_err());
        assert!(
            message.contains("Failed to spawn Codex CLI"),
            "message: {message}"
        );
        assert!(stderr.is_empty());
    }
}
