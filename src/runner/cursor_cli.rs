use std::path::PathBuf;
use std::time::Duration;

use tokio::process::Command;

use crate::error::ActualError;
use crate::tailoring::types::TailoringOutput;

use super::subprocess::{resolve_output, TailoringRunner};
use super::util::{find_binary, model_name_regex, strip_markdown_json_fences};

/// Environment variable that overrides the default binary lookup.
/// Useful for testing and CI environments.
const CURSOR_BINARY_ENV: &str = "CURSOR_BINARY";

/// Production implementation that spawns `cursor-agent` (Cursor CLI) as a subprocess.
///
/// Cursor CLI is Cursor's headless agent tool. This runner invokes it in
/// non-interactive mode using `-p --output-format json`.
///
/// # Invocation
///
/// ```text
/// cursor-agent -p --output-format json --trust \
///     [--model <model>] \
///     "<prompt_with_embedded_schema>"
/// ```
///
/// Since Cursor CLI does not support `--json-schema`, the JSON schema is
/// embedded in the prompt itself with instructions to output conforming JSON.
/// The agent's response is written to stdout as a JSON envelope, which is
/// parsed to extract the `result` field containing the `TailoringOutput`.
///
/// The `max_budget_usd` parameter is not forwarded because Cursor CLI does not
/// expose a cost-limit flag.
///
/// # Authentication
///
/// Cursor CLI supports two auth methods:
/// - `CURSOR_API_KEY` environment variable
/// - `cursor-agent login` (OAuth / Cursor account)
///
/// If `api_key` is provided (from config or env), it is injected as
/// `CURSOR_API_KEY` in the subprocess environment. If not provided, the
/// subprocess inherits whatever auth the user has configured.
pub struct CursorCliRunner {
    /// Path to the Cursor CLI binary.
    binary_path: PathBuf,
    /// Model to use (e.g. `"claude-4.6-sonnet"`, `"gpt-5.2"`, `"auto"`).
    ///
    /// When `None`, the `--model` flag is omitted and Cursor CLI uses its own
    /// default. This avoids hardcoding a model that may not be available on the
    /// user's account tier.
    model: Option<String>,
    /// Maximum time to wait for subprocess completion.
    timeout: Duration,
    /// Optional API key to inject as `CURSOR_API_KEY` in the subprocess env.
    /// When `None`, the subprocess inherits the parent's environment (which
    /// may already have `CURSOR_API_KEY` set, or the user may have run
    /// `agent login`).
    api_key: Option<String>,
}

impl CursorCliRunner {
    pub fn new(binary_path: PathBuf, model: Option<String>, timeout: Duration) -> Self {
        Self {
            binary_path,
            model,
            timeout,
            api_key: None,
        }
    }

    /// Set an API key to inject as `CURSOR_API_KEY` in the subprocess environment.
    ///
    /// This allows the config-file `cursor_api_key` fallback to reach the Cursor
    /// CLI subprocess, mirroring the Codex CLI runner's auth inheritance pattern.
    pub fn with_api_key(mut self, api_key: String) -> Self {
        self.api_key = Some(api_key);
        self
    }
}

/// Locate the Cursor CLI binary on the system.
///
/// Resolution order:
/// 1. If `CURSOR_BINARY` env var is set, use that path (must exist and be executable).
/// 2. Otherwise, search PATH for `cursor-agent` (Homebrew install) first,
///    then fall back to `agent` (older or alternative installs).
///
/// Returns the absolute path to the binary on success, or
/// `ActualError::CursorNotFound` if the binary cannot be located or is invalid.
pub fn find_cursor_binary() -> Result<PathBuf, ActualError> {
    find_binary(CURSOR_BINARY_ENV, &["cursor-agent", "agent"], || {
        ActualError::CursorNotFound
    })
}

/// Convert an I/O error from subprocess operations into an `ActualError`.
fn io_err(context: &str, e: std::io::Error) -> ActualError {
    ActualError::RunnerFailed {
        message: format!("{context}: {e}"),
        stderr: String::new(),
    }
}

/// Build the argument list for the Cursor CLI invocation.
///
/// Produces: `-p --output-format json --trust [--model <model>] <prompt>`
///
/// The `-p` flag runs Cursor in non-interactive (print) mode.
/// `--trust` bypasses the workspace trust prompt required in headless mode.
/// `--output-format json` produces a JSON envelope on stdout containing the
/// agent's response in the `result` field.
///
/// When `model` is `None`, the `--model` flag is omitted and Cursor CLI uses
/// its own default model.
///
/// # Errors
///
/// Returns `ActualError::ConfigError` if:
/// - `model` is `Some` and does not match `^[a-zA-Z0-9][a-zA-Z0-9._/-]{0,99}$`
/// - `prompt` starts with `-` (would be interpreted as a flag by the CLI parser)
fn build_cursor_args(prompt: &str, model: Option<&str>) -> Result<Vec<String>, ActualError> {
    if let Some(m) = model {
        if !model_name_regex().is_match(m) {
            return Err(ActualError::ConfigError(format!(
                "Invalid model name: {m:?}. Must match [a-zA-Z0-9][a-zA-Z0-9._/-]{{0,99}}"
            )));
        }
    }
    if prompt.starts_with('-') {
        return Err(ActualError::ConfigError(
            "Invalid prompt: must not start with '-'".to_string(),
        ));
    }
    let mut args = vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "json".to_string(),
        "--trust".to_string(),
    ];
    if let Some(m) = model {
        args.push("--model".to_string());
        args.push(m.to_string());
    }
    args.push(prompt.to_string());
    Ok(args)
}

/// JSON envelope returned by Cursor CLI when invoked with `--output-format json`.
///
/// The `result` field contains the agent's text response as a string, which
/// we then parse as `TailoringOutput` JSON.
#[derive(serde::Deserialize)]
struct CursorEnvelope {
    #[serde(default)]
    is_error: bool,
    result: Option<String>,
    #[serde(default)]
    subtype: Option<String>,
}

/// Spawn the Cursor binary, wait with timeout, and parse JSON output from
/// the stdout JSON envelope.
///
/// Unlike the Codex CLI runner which reads from a temp file, Cursor CLI writes
/// a JSON envelope to stdout. This function reads stdout, deserializes the
/// envelope, extracts the `result` field, strips any markdown fences, and
/// parses it as `TailoringOutput`.
///
/// Cursor CLI uses proper exit codes — non-zero on failure — so there is no
/// need for empty-output detection like in the Codex runner.
async fn run_cursor_subprocess(
    binary_path: &std::path::Path,
    timeout: Duration,
    args: &[String],
    api_key: Option<&str>,
) -> Result<TailoringOutput, ActualError> {
    let mut cmd = Command::new(binary_path);
    cmd.args(args);
    // Inject CURSOR_API_KEY if provided (from config fallback).
    // The subprocess also inherits the parent env, so if the user already
    // has CURSOR_API_KEY set in their shell, it will be available regardless.
    if let Some(key) = api_key {
        cmd.env("CURSOR_API_KEY", key);
    }
    // When the timeout fires, the `wait_with_output` future is dropped, which drops
    // the `Child` it owns. With `kill_on_drop(true)`, tokio sends SIGKILL to the child
    // on drop, preventing orphaned processes.
    cmd.kill_on_drop(true);

    let child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| io_err("Failed to spawn Cursor CLI", e))?;

    let result = tokio::time::timeout(timeout, child.wait_with_output()).await;
    let output = resolve_output(result, timeout)?;

    let stderr_text = String::from_utf8_lossy(&output.stderr).to_string();

    // Check exit status first.
    if !output.status.success() {
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        return Err(ActualError::RunnerFailed {
            message: format!("Cursor CLI exited with code {code}"),
            stderr: stderr_text,
        });
    }

    let stdout_text = String::from_utf8_lossy(&output.stdout).to_string();

    // Parse the JSON envelope from stdout.
    let envelope: CursorEnvelope =
        serde_json::from_str(&stdout_text).map_err(|e| ActualError::RunnerFailed {
            message: format!("Failed to parse Cursor CLI JSON envelope: {e}"),
            stderr: stderr_text.clone(),
        })?;

    // Check for error states in the envelope.
    if envelope.is_error {
        let detail = envelope
            .result
            .as_deref()
            .or(envelope.subtype.as_deref())
            .unwrap_or("unknown error");
        return Err(ActualError::RunnerFailed {
            message: format!("Cursor CLI returned an error: {detail}"),
            stderr: stderr_text,
        });
    }

    // Extract the result field.
    let result_text = envelope.result.ok_or_else(|| ActualError::RunnerFailed {
        message: "Cursor CLI envelope missing result field".to_string(),
        stderr: stderr_text,
    })?;

    // Strip markdown JSON fences if present (models sometimes wrap output).
    let json_str = strip_markdown_json_fences(&result_text);

    let parsed: TailoringOutput = serde_json::from_str(json_str)?;
    Ok(parsed)
}

impl TailoringRunner for CursorCliRunner {
    async fn run_tailoring(
        &self,
        prompt: &str,
        schema: &str,
        _model_override: Option<&str>,
        _max_budget_usd: Option<f64>,
    ) -> Result<TailoringOutput, ActualError> {
        // Ignore `_model_override` — it comes from `ConcurrentTailoringConfig`
        // which resolves from `config.model` (a Claude Code alias like "haiku").
        // The Cursor runner's `self.model` was already correctly resolved in
        // `sync_wiring` from `--model` flag > `config.model` > None
        // (Cursor CLI default), so we always use it here.
        let model = self.model.as_deref();

        // Embed the JSON schema in the prompt since Cursor CLI does not
        // support --json-schema.  The original prompt already contains the
        // tailoring instructions; we append a schema conformance directive.
        let augmented_prompt = format!(
            "{prompt}\n\n\
             IMPORTANT: Your response must be ONLY valid JSON (no markdown fences, no commentary) \
             conforming to this JSON schema:\n{schema}"
        );

        let args = build_cursor_args(&augmented_prompt, model)?;
        run_cursor_subprocess(
            &self.binary_path,
            self.timeout,
            &args,
            self.api_key.as_deref(),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

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

    /// Build a Cursor CLI JSON envelope string with the given result text.
    fn cursor_envelope(result_json: &str) -> String {
        // The result field is a string containing JSON, so it needs to be
        // properly escaped inside the envelope.
        let escaped = serde_json::to_string(result_json).unwrap();
        format!(
            r#"{{"type":"result","subtype":"success","is_error":false,"result":{escaped},"session_id":"test"}}"#
        )
    }

    // ---- Test 1: valid JSON parsed correctly ----

    #[tokio::test]
    #[cfg(unix)]
    async fn test_valid_json_parsed_correctly() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-agent.sh");
        let envelope = cursor_envelope(&minimal_tailoring_json());
        let script_content = format!("#!/bin/sh\necho '{}'\n", envelope.replace('\'', "'\\''"));
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner = CursorCliRunner::new(
            script,
            Some("claude-4.6-sonnet".to_string()),
            Duration::from_secs(10),
        );
        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await
            .unwrap();

        assert_eq!(result.files.len(), 0);
        assert_eq!(result.summary.total_input, 0);
    }

    // ---- Test 2: non-zero exit maps to RunnerFailed ----

    #[tokio::test]
    #[cfg(unix)]
    async fn test_nonzero_exit_maps_to_subprocess_failed() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-agent.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'something went wrong' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner = CursorCliRunner::new(
            script,
            Some("claude-4.6-sonnet".to_string()),
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
        let script = dir.path().join("fake-agent.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 10\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner = CursorCliRunner::new(
            script,
            Some("claude-4.6-sonnet".to_string()),
            Duration::from_millis(100),
        );
        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await;

        // 100ms timeout → 0 seconds when truncated
        assert_eq!(timeout_seconds(result.unwrap_err()), 0);
    }

    // ---- Test 4: spawn failure (bad binary path) ----

    #[tokio::test]
    async fn test_spawn_failure() {
        let runner = CursorCliRunner::new(
            PathBuf::from("/nonexistent/agent/binary"),
            Some("claude-4.6-sonnet".to_string()),
            Duration::from_secs(10),
        );
        let result = runner
            .run_tailoring("prompt", r#"{"type":"object"}"#, None, None)
            .await;

        let (message, stderr) = subprocess_failed(result.unwrap_err());
        assert!(
            message.contains("Failed to spawn Cursor CLI"),
            "message: {message}"
        );
        assert!(stderr.is_empty());
    }

    // ---- Test 5: find_cursor_binary respects CURSOR_BINARY env var ----

    #[test]
    #[cfg(unix)]
    fn test_find_cursor_binary_respects_env_var() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let fake_binary = dir.path().join("fake-agent");
        std::fs::write(&fake_binary, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&fake_binary, std::fs::Permissions::from_mode(0o755)).unwrap();

        let _guard = EnvGuard::set("CURSOR_BINARY", fake_binary.to_str().unwrap());
        let result = find_cursor_binary();
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), fake_binary);
    }

    // ---- Test 6: find_cursor_binary returns CursorNotFound when not in PATH ----

    #[test]
    fn test_find_cursor_binary_not_found() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("CURSOR_BINARY");
        let _g2 = EnvGuard::set("PATH", "");
        let result = find_cursor_binary();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::CursorNotFound));
    }

    // ---- Test 7: find_cursor_binary env nonexistent path ----

    #[test]
    fn test_find_cursor_binary_env_nonexistent_path() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("CURSOR_BINARY", "/nonexistent/path/to/agent");
        let result = find_cursor_binary();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::CursorNotFound));
    }

    // ---- Test 8: find_cursor_binary env non-executable ----

    #[test]
    #[cfg(unix)]
    fn test_find_cursor_binary_env_non_executable() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let non_exec = dir.path().join("agent-not-executable");
        // Create the file but do NOT set executable bit
        std::fs::write(&non_exec, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&non_exec, std::fs::Permissions::from_mode(0o644)).unwrap();

        let _guard = EnvGuard::set("CURSOR_BINARY", non_exec.to_str().unwrap());
        let result = find_cursor_binary();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::CursorNotFound));
    }

    // ---- Test 9: find_cursor_binary env directory rejected ----

    #[test]
    #[cfg(unix)]
    fn test_find_cursor_binary_env_directory_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();

        let _guard = EnvGuard::set("CURSOR_BINARY", dir.path().to_str().unwrap());
        let result = find_cursor_binary();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::CursorNotFound));
    }

    // ---- Test 10: runner uses self.model, not model_override ----

    #[tokio::test]
    #[cfg(unix)]
    async fn test_runner_uses_self_model_not_override() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let args_file = dir.path().join("captured-args.txt");
        let envelope = cursor_envelope(&minimal_tailoring_json());
        let script_content = format!(
            r#"#!/bin/sh
ARGS_CAPTURED=""
for arg in "$@"; do
    echo "$arg" >> "{args_file}"
done
echo '{envelope}'
"#,
            args_file = args_file.display(),
            envelope = envelope.replace('\'', "'\\''")
        );
        let script = dir.path().join("fake-agent.sh");
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Runner is constructed with model "gpt-5.2" (set by sync_wiring).
        // model_override is "haiku" (a Claude alias from config.model).
        // The runner should use "gpt-5.2", NOT "haiku".
        let runner =
            CursorCliRunner::new(script, Some("gpt-5.2".to_string()), Duration::from_secs(10));
        runner
            .run_tailoring("prompt", r#"{"type":"object"}"#, Some("haiku"), None)
            .await
            .unwrap();

        let captured = std::fs::read_to_string(&args_file).unwrap();

        // Verify -p flag
        assert!(captured.starts_with("-p\n"), "should start with -p flag");

        // Verify --output-format json
        assert!(
            captured.contains("--output-format\njson\n"),
            "--output-format json missing"
        );

        // Verify --trust flag (required for headless/non-interactive mode)
        assert!(
            captured.contains("--trust\n"),
            "--trust flag missing (required to bypass workspace trust prompt in headless mode)"
        );

        // Verify self.model ("gpt-5.2") is used, NOT model_override ("haiku")
        assert!(
            captured.contains("--model\ngpt-5.2\n"),
            "--model gpt-5.2 missing (should use self.model, not model_override)"
        );
        assert!(
            !captured.contains("haiku"),
            "haiku should NOT appear in args (model_override should be ignored)"
        );

        // Verify the augmented prompt contains the original prompt and schema directive
        assert!(
            captured.contains("prompt"),
            "prompt text missing from captured args"
        );
        assert!(
            captured.contains("JSON schema"),
            "schema directive missing from captured args"
        );
    }

    // ---- Test 11: runner omits --model when self.model is None ----

    #[tokio::test]
    #[cfg(unix)]
    async fn test_runner_omits_model_when_none() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let args_file = dir.path().join("captured-args.txt");
        let envelope = cursor_envelope(&minimal_tailoring_json());
        let script_content = format!(
            r#"#!/bin/sh
for arg in "$@"; do
    echo "$arg" >> "{args_file}"
done
echo '{envelope}'
"#,
            args_file = args_file.display(),
            envelope = envelope.replace('\'', "'\\''")
        );
        let script = dir.path().join("fake-agent.sh");
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Runner has no model (None) — Cursor CLI should use its own default.
        let runner = CursorCliRunner::new(script, None, Duration::from_secs(10));
        runner
            .run_tailoring("prompt", r#"{"type":"object"}"#, Some("haiku"), None)
            .await
            .unwrap();

        let captured = std::fs::read_to_string(&args_file).unwrap();

        // --model flag should NOT be present
        assert!(
            !captured.contains("--model"),
            "--model should not be present when self.model is None"
        );
    }

    // ---- Test 12: build_cursor_args with model ----

    #[test]
    fn test_build_cursor_args_with_model() {
        let args = build_cursor_args("my prompt", Some("gpt-5.2")).unwrap();
        assert_eq!(args[0], "-p");
        assert_eq!(args[1], "--output-format");
        assert_eq!(args[2], "json");
        assert_eq!(args[3], "--trust");
        assert_eq!(args[4], "--model");
        assert_eq!(args[5], "gpt-5.2");
        assert_eq!(args[6], "my prompt");
    }

    // ---- Test 13: build_cursor_args without model ----

    #[test]
    fn test_build_cursor_args_without_model() {
        let args = build_cursor_args("my prompt", None).unwrap();
        assert_eq!(args[0], "-p");
        assert_eq!(args[1], "--output-format");
        assert_eq!(args[2], "json");
        assert_eq!(args[3], "--trust");
        assert_eq!(args[4], "my prompt");
        // No --model flag — Cursor CLI uses its own default
        assert!(
            !args.iter().any(|a| a == "--model"),
            "--model should not be present when model is None"
        );
    }

    // ---- Test 14: build_cursor_args rejects flag-like model ----

    #[test]
    fn test_build_cursor_args_rejects_flag_model() {
        let err = build_cursor_args("valid prompt", Some("--dangerous-flag")).unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(_)),
            "expected ConfigError, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains("Invalid model name"), "message: {msg}");
    }

    // ---- Test 15: build_cursor_args rejects model with spaces ----

    #[test]
    fn test_build_cursor_args_rejects_model_with_spaces() {
        let err = build_cursor_args("valid prompt", Some("model with spaces")).unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(_)));
    }

    // ---- Test 16: build_cursor_args rejects model with shell metacharacters ----

    #[test]
    fn test_build_cursor_args_rejects_model_with_shell_metacharacters() {
        for bad_model in &["model|cmd", "model;cmd", "model$var", "model`cmd`"] {
            let err = build_cursor_args("valid prompt", Some(bad_model)).unwrap_err();
            assert!(
                matches!(err, ActualError::ConfigError(_)),
                "expected ConfigError for model {bad_model:?}, got: {err:?}"
            );
        }
    }

    // ---- Test 17: build_cursor_args rejects flag-like prompt ----

    #[test]
    fn test_build_cursor_args_rejects_flag_prompt() {
        let err = build_cursor_args("--bad-flag", Some("gpt-5.2")).unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(_)),
            "expected ConfigError, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains("Invalid prompt"), "message: {msg}");
    }

    // ---- Test 18: build_cursor_args accepts valid model names ----

    #[test]
    fn test_build_cursor_args_accepts_valid_model_names() {
        let valid_models = [
            "gpt-5",
            "claude-4.6-sonnet",
            "gpt-5.2",
            "auto",
            "openai/gpt-5",
            "provider/model-name_v2",
        ];
        for model in &valid_models {
            assert!(
                build_cursor_args("prompt", Some(model)).is_ok(),
                "expected Ok for model {model:?}"
            );
        }
    }

    // ---- Test 20: with_api_key injects CURSOR_API_KEY env var ----

    #[tokio::test]
    #[cfg(unix)]
    async fn test_with_api_key_injects_env_var() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let env_file = dir.path().join("captured-env.txt");
        let envelope = cursor_envelope(&minimal_tailoring_json());
        let script_content = format!(
            r#"#!/bin/sh
echo "$CURSOR_API_KEY" > "{env_file}"
echo '{envelope}'
"#,
            env_file = env_file.display(),
            envelope = envelope.replace('\'', "'\\''")
        );
        let script = dir.path().join("fake-agent.sh");
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner =
            CursorCliRunner::new(script, Some("gpt-5.2".to_string()), Duration::from_secs(10))
                .with_api_key("cursor-test-key-12345".to_string());

        runner
            .run_tailoring("prompt", r#"{"type":"object"}"#, None, None)
            .await
            .unwrap();

        let captured_key = std::fs::read_to_string(&env_file).unwrap();
        assert_eq!(captured_key.trim(), "cursor-test-key-12345");
    }

    // ---- Test 21: envelope is_error true ----

    #[tokio::test]
    #[cfg(unix)]
    async fn test_envelope_is_error_true() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-agent.sh");
        let script_content =
            "#!/bin/sh\necho '{\"type\":\"result\",\"is_error\":true,\"result\":\"auth failed\",\"session_id\":\"test\"}'\n";
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner =
            CursorCliRunner::new(script, Some("gpt-5.2".to_string()), Duration::from_secs(10));
        let result = runner
            .run_tailoring("prompt", r#"{"type":"object"}"#, None, None)
            .await;

        let (message, _stderr) = subprocess_failed(result.unwrap_err());
        assert!(message.contains("returned an error"), "message: {message}");
        assert!(message.contains("auth failed"), "message: {message}");
    }

    // ---- Test 22: envelope missing result field ----

    #[tokio::test]
    #[cfg(unix)]
    async fn test_envelope_missing_result() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-agent.sh");
        let script_content =
            "#!/bin/sh\necho '{\"type\":\"result\",\"is_error\":false,\"session_id\":\"test\"}'\n";
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner =
            CursorCliRunner::new(script, Some("gpt-5.2".to_string()), Duration::from_secs(10));
        let result = runner
            .run_tailoring("prompt", r#"{"type":"object"}"#, None, None)
            .await;

        let (message, _stderr) = subprocess_failed(result.unwrap_err());
        assert!(
            message.contains("missing result field"),
            "message: {message}"
        );
    }

    // ---- Test 23: malformed JSON on stdout ----

    #[tokio::test]
    #[cfg(unix)]
    async fn test_envelope_malformed_json() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-agent.sh");
        let script_content = "#!/bin/sh\necho 'this is not json at all'\n";
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner =
            CursorCliRunner::new(script, Some("gpt-5.2".to_string()), Duration::from_secs(10));
        let result = runner
            .run_tailoring("prompt", r#"{"type":"object"}"#, None, None)
            .await;

        let (message, _stderr) = subprocess_failed(result.unwrap_err());
        assert!(
            message.contains("Failed to parse Cursor CLI JSON envelope"),
            "message: {message}"
        );
    }
}
