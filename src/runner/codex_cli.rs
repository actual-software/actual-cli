use std::path::PathBuf;
use std::time::Duration;

use regex::Regex;
use std::sync::OnceLock;
use tokio::process::Command;

use crate::error::ActualError;
use crate::tailoring::types::TailoringOutput;

use super::subprocess::{resolve_output, TailoringRunner};

/// Compiled regex for validating model names: alphanumeric start, then
/// alphanumeric, dots, underscores, slashes, or hyphens, up to 100 chars total.
static MODEL_NAME_RE: OnceLock<Regex> = OnceLock::new();

fn model_name_regex() -> &'static Regex {
    MODEL_NAME_RE
        .get_or_init(|| Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._/\-]{0,99}$").expect("valid regex"))
}

/// The default binary name to search for on PATH.
const CODEX_BINARY_NAME: &str = "codex";

/// Environment variable that overrides the default binary lookup.
/// Useful for testing and CI environments.
const CODEX_BINARY_ENV: &str = "CODEX_BINARY";

/// Production implementation that spawns `codex` as a subprocess.
///
/// Codex CLI (`@openai/codex`) is OpenAI's open-source CLI tool. This runner
/// invokes it in non-interactive mode using `exec --full-auto`
/// (analogous to Claude Code's `--print -p`).
///
/// # Invocation
///
/// ```text
/// codex exec --full-auto \
///     --model <model> \
///     --output-last-message <tempfile> \
///     "<prompt_with_embedded_schema>"
/// ```
///
/// Since Codex CLI does not support `--json-schema`, the JSON schema is
/// embedded in the prompt itself with instructions to output conforming JSON.
/// The agent's final message is written to a temp file via
/// `--output-last-message`, which is then read and parsed as `TailoringOutput`.
///
/// The `max_budget_usd` parameter is not forwarded because Codex CLI does not
/// expose a cost-limit flag.
///
/// # Authentication
///
/// Codex CLI supports two auth methods:
/// - `OPENAI_API_KEY` environment variable (API key from platform.openai.com)
/// - `codex login` (OAuth / ChatGPT account)
///
/// If `api_key` is provided (from config or env), it is injected as
/// `OPENAI_API_KEY` in the subprocess environment, mirroring how the Claude
/// Code runner inherits auth from the parent process. If not provided, the
/// subprocess inherits whatever auth the user has configured (env var or
/// `codex login`).
pub struct CodexCliRunner {
    /// Path to the Codex CLI binary.
    binary_path: PathBuf,
    /// Model to use (e.g. `"gpt-5"`, `"gpt-5.2"`).
    ///
    /// When `None`, the `--model` flag is omitted and Codex CLI uses its own
    /// default (currently `gpt-5` for ChatGPT OAuth accounts). This avoids
    /// hardcoding a model that may not be available on the user's account tier.
    model: Option<String>,
    /// Maximum time to wait for subprocess completion.
    timeout: Duration,
    /// Optional API key to inject as `OPENAI_API_KEY` in the subprocess env.
    /// When `None`, the subprocess inherits the parent's environment (which
    /// may already have `OPENAI_API_KEY` set, or the user may have run
    /// `codex login`).
    api_key: Option<String>,
}

impl CodexCliRunner {
    pub fn new(binary_path: PathBuf, model: Option<String>, timeout: Duration) -> Self {
        Self {
            binary_path,
            model,
            timeout,
            api_key: None,
        }
    }

    /// Set an API key to inject as `OPENAI_API_KEY` in the subprocess environment.
    ///
    /// This allows the config-file `openai_api_key` fallback to reach the Codex
    /// CLI subprocess, mirroring the Claude Code runner's auth inheritance pattern.
    pub fn with_api_key(mut self, api_key: String) -> Self {
        self.api_key = Some(api_key);
        self
    }
}

/// Check whether Codex CLI authentication is available.
///
/// Returns `Ok(())` if any auth source is detected:
/// 1. An API key was already resolved (from env var or config) — non-empty
/// 2. `~/.codex/auth.json` exists (indicates `codex login` was run)
///
/// Returns `Err(ActualError::CodexNotAuthenticated)` if none are found.
pub fn check_codex_auth(api_key: Option<&str>) -> Result<(), ActualError> {
    let auth_path = dirs::home_dir().map(|h| h.join(".codex").join("auth.json"));
    check_codex_auth_inner(api_key, auth_path.as_deref())
}

/// Inner implementation that accepts an explicit auth file path for testability.
pub(crate) fn check_codex_auth_inner(
    api_key: Option<&str>,
    auth_path: Option<&std::path::Path>,
) -> Result<(), ActualError> {
    // If an API key was already resolved (from env var or config), auth is good.
    if api_key.is_some_and(|k| !k.is_empty()) {
        return Ok(());
    }

    // Check if codex login has been run (auth file exists).
    if let Some(path) = auth_path {
        if path.is_file() {
            return Ok(());
        }
    }

    Err(ActualError::CodexNotAuthenticated)
}

/// Locate the Codex CLI binary on the system.
///
/// Resolution order:
/// 1. If `CODEX_BINARY` env var is set, use that path (must exist and be executable).
/// 2. Otherwise, search PATH for `codex` using `which::which`.
///
/// Returns the absolute path to the binary on success, or
/// `ActualError::CodexNotFound` if the binary cannot be located or is invalid.
pub fn find_codex_binary() -> Result<PathBuf, ActualError> {
    if let Ok(path_str) = std::env::var(CODEX_BINARY_ENV) {
        let path = PathBuf::from(&path_str);
        if !path.exists() {
            return Err(ActualError::CodexNotFound);
        }
        if !path.is_file() {
            return Err(ActualError::CodexNotFound);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .map(|m| m.permissions().mode())
                .unwrap_or(0);
            if mode & 0o111 == 0 {
                return Err(ActualError::CodexNotFound);
            }
        }
        return Ok(path);
    }
    which::which(CODEX_BINARY_NAME).map_err(|_| ActualError::CodexNotFound)
}

/// Convert an I/O error from subprocess operations into an `ActualError`.
fn io_err(context: &str, e: std::io::Error) -> ActualError {
    ActualError::RunnerFailed {
        message: format!("{context}: {e}"),
        stderr: String::new(),
    }
}

/// Build the argument list for the Codex CLI invocation.
///
/// Produces: `exec --full-auto [--model <model>] --output-last-message <path> <prompt>`
///
/// The `exec` subcommand runs Codex in non-interactive mode.
/// `--full-auto` is a convenience alias for `-a on-failure --sandbox workspace-write`,
/// suppressing confirmation prompts while maintaining sandboxed execution.
/// `--output-last-message <path>` writes the agent's final response to a file
/// so we can read structured output after the process exits.
///
/// When `model` is `None`, the `--model` flag is omitted and Codex CLI uses
/// its own default model (currently `gpt-5` for ChatGPT OAuth accounts).
/// This avoids hardcoding a model name that may not be available on the
/// user's account tier.
///
/// Note: Codex CLI does not support `--json-schema`, so the JSON schema is
/// embedded in the prompt by the caller. The agent's response (written to
/// `output_path`) is expected to contain JSON conforming to the schema.
///
/// # Errors
///
/// Returns `ActualError::ConfigError` if:
/// - `model` is `Some` and does not match `^[a-zA-Z0-9][a-zA-Z0-9._/-]{0,99}$`
/// - `prompt` starts with `-` (would be interpreted as a flag by the CLI parser)
fn build_codex_args(
    prompt: &str,
    model: Option<&str>,
    output_path: &std::path::Path,
) -> Result<Vec<String>, ActualError> {
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
    let mut args = vec!["exec".to_string(), "--full-auto".to_string()];
    if let Some(m) = model {
        args.push("--model".to_string());
        args.push(m.to_string());
    }
    args.push("--output-last-message".to_string());
    args.push(output_path.to_string_lossy().to_string());
    args.push(prompt.to_string());
    Ok(args)
}

/// Spawn the Codex binary, wait with timeout, and parse JSON output from the
/// `--output-last-message` file.
///
/// Unlike the Claude Code runner which reads structured output from an envelope
/// on stdout, Codex CLI writes the agent's final message to a file specified by
/// `--output-last-message`. This function reads that file after the subprocess
/// exits and parses it as `TailoringOutput`.
///
/// If the file contains markdown-fenced JSON (` ```json ... ``` `), the fences
/// are stripped before parsing.
async fn run_codex_subprocess(
    binary_path: &std::path::Path,
    timeout: Duration,
    args: &[String],
    output_path: &std::path::Path,
    api_key: Option<&str>,
) -> Result<TailoringOutput, ActualError> {
    let mut cmd = Command::new(binary_path);
    cmd.args(args);
    // Inject OPENAI_API_KEY if provided (from config fallback).
    // The subprocess also inherits the parent env, so if the user already
    // has OPENAI_API_KEY set in their shell, it will be available regardless.
    if let Some(key) = api_key {
        cmd.env("OPENAI_API_KEY", key);
    }
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

    let stderr_text = String::from_utf8_lossy(&output.stderr).to_string();
    let stdout_text = String::from_utf8_lossy(&output.stdout).to_string();

    // Check exit status first.
    if !output.status.success() {
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        return Err(ActualError::RunnerFailed {
            message: format!("Codex CLI exited with code {code}"),
            stderr: stderr_text,
        });
    }

    // Read the agent's final message from the output file.
    let raw = std::fs::read_to_string(output_path).map_err(|e| ActualError::RunnerFailed {
        message: format!(
            "Failed to read Codex output file {}: {e}",
            output_path.display()
        ),
        stderr: String::new(),
    })?;

    // Codex CLI exits 0 even when the model is unsupported or the request
    // fails (e.g. ChatGPT account limitations). In that case the output
    // file is empty and stderr/stdout contains the error details.  Detect
    // this and surface a useful error message.
    if raw.trim().is_empty() {
        // Look for model-not-supported errors in stdout (Codex logs errors there)
        let combined = format!("{stdout_text}\n{stderr_text}");
        let message = if let Some(detail) = extract_codex_error_detail(&combined) {
            format!("Codex CLI failed: {detail}")
        } else {
            "Codex CLI produced no output (output file is empty)".to_string()
        };
        return Err(ActualError::RunnerFailed {
            message,
            stderr: stderr_text,
        });
    }

    // Strip markdown JSON fences if present (models sometimes wrap output).
    let json_str = strip_markdown_json_fences(&raw);

    let parsed: TailoringOutput = serde_json::from_str(json_str)?;
    Ok(parsed)
}

/// Strip markdown JSON code fences from a string if present.
///
/// Handles both ` ```json\n...\n``` ` and ` ```\n...\n``` ` patterns.
/// Returns the inner content if fences are found, otherwise returns the
/// original string trimmed.
fn strip_markdown_json_fences(s: &str) -> &str {
    let trimmed = s.trim();

    // Try ```json first, then bare ```
    for prefix in &["```json", "```"] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            // Skip optional newline after opening fence
            let rest = rest.strip_prefix('\n').unwrap_or(rest);
            if let Some(inner) = rest.strip_suffix("```") {
                return inner.trim();
            }
        }
    }

    trimmed
}

/// Extract an error detail message from Codex CLI output.
///
/// Codex CLI logs errors to stdout (not stderr) in a format like:
/// ```text
/// [timestamp] ERROR: unexpected status 400 Bad Request: {"detail":"The 'gpt-5.2' model is not supported..."}
/// ```
///
/// This function searches for JSON fragments containing a `"detail"` field in
/// the combined stdout+stderr output, using `serde_json` for robust parsing.
/// Returns `None` if no error detail is found.
fn extract_codex_error_detail(output: &str) -> Option<String> {
    // Try to find and parse JSON fragments containing a "detail" field.
    // Codex CLI embeds error JSON like {"detail":"..."} in its log output.
    let mut search_from = 0;
    while let Some(open) = output[search_from..].find('{') {
        let open = search_from + open;
        if let Some(close_offset) = output[open..].find('}') {
            let candidate = &output[open..=open + close_offset];
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(candidate) {
                if let Some(detail) = val.get("detail").and_then(|d| d.as_str()) {
                    if !detail.is_empty() {
                        return Some(detail.to_string());
                    }
                }
            }
            search_from = open + 1;
        } else {
            break;
        }
    }

    // Fallback: look for "ERROR:" lines
    for line in output.lines() {
        if let Some(error_msg) = line.strip_prefix("] ERROR: ").or_else(|| {
            line.find("] ERROR: ")
                .map(|pos| &line[pos + "] ERROR: ".len()..])
        }) {
            let msg = error_msg.trim();
            if !msg.is_empty() {
                return Some(msg.to_string());
            }
        }
    }

    None
}

impl TailoringRunner for CodexCliRunner {
    async fn run_tailoring(
        &self,
        prompt: &str,
        schema: &str,
        _model_override: Option<&str>,
        _max_budget_usd: Option<f64>,
    ) -> Result<TailoringOutput, ActualError> {
        // Ignore `_model_override` — it comes from `ConcurrentTailoringConfig`
        // which resolves from `config.model` (a Claude Code alias like "haiku").
        // The Codex runner's `self.model` was already correctly resolved in
        // `sync_wiring` from `--model` flag > `config.model` > None
        // (Codex CLI default), so we always use it here.
        let model = self.model.as_deref();

        // Create a temp file for --output-last-message.  The file is
        // automatically cleaned up when `_tempfile` is dropped at the end
        // of this function (or on error).
        let output_tempfile = tempfile::NamedTempFile::new()
            .map_err(|e| io_err("Failed to create temp file for Codex output", e))?;
        let output_path = output_tempfile.path().to_path_buf();

        // Embed the JSON schema in the prompt since Codex CLI does not
        // support --json-schema.  The original prompt already contains the
        // tailoring instructions; we append a schema conformance directive.
        let augmented_prompt = format!(
            "{prompt}\n\n\
             IMPORTANT: Your response must be ONLY valid JSON (no markdown fences, no commentary) \
             conforming to this JSON schema:\n{schema}"
        );

        let args = build_codex_args(&augmented_prompt, model, &output_path)?;
        run_codex_subprocess(
            &self.binary_path,
            self.timeout,
            &args,
            &output_path,
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

    // ---- Test 1: valid JSON output is parsed correctly ----
    //
    // The fake script writes valid TailoringOutput JSON to the file specified
    // by --output-last-message (the arg after that flag).

    #[tokio::test]
    #[cfg(unix)]
    async fn test_valid_json_parsed_correctly() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-codex.sh");
        // Script finds the --output-last-message arg value and writes JSON there.
        let script_content = format!(
            r#"#!/bin/sh
OUTPUT_FILE=""
NEXT_IS_OUTPUT=false
for arg in "$@"; do
    if $NEXT_IS_OUTPUT; then
        OUTPUT_FILE="$arg"
        NEXT_IS_OUTPUT=false
    elif [ "$arg" = "--output-last-message" ]; then
        NEXT_IS_OUTPUT=true
    fi
done
echo '{}' > "$OUTPUT_FILE"
"#,
            minimal_tailoring_json()
        );
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner = CodexCliRunner::new(
            script,
            Some("gpt-5.2-codex".to_string()),
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
        let script = dir.path().join("fake-codex.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'something went wrong' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner = CodexCliRunner::new(
            script,
            Some("gpt-5.2-codex".to_string()),
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
            Some("gpt-5.2-codex".to_string()),
            Duration::from_millis(100),
        );
        let result = runner
            .run_tailoring("test prompt", r#"{"type":"object"}"#, None, None)
            .await;

        // 100ms timeout → 0 seconds when truncated
        assert_eq!(timeout_seconds(result.unwrap_err()), 0);
    }

    // ---- Test 4: find_codex_binary respects CODEX_BINARY env var (must be valid executable) ----

    #[test]
    #[cfg(unix)]
    fn test_find_codex_binary_respects_env_var() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let fake_binary = dir.path().join("fake-codex");
        std::fs::write(&fake_binary, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&fake_binary, std::fs::Permissions::from_mode(0o755)).unwrap();

        let _guard = EnvGuard::set("CODEX_BINARY", fake_binary.to_str().unwrap());
        let result = find_codex_binary();
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), fake_binary);
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

    // ---- Security: find_codex_binary validates CODEX_BINARY env var ----

    #[test]
    fn test_find_codex_binary_env_nonexistent_path() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("CODEX_BINARY", "/nonexistent/path/to/codex");
        let result = find_codex_binary();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::CodexNotFound));
    }

    #[test]
    #[cfg(unix)]
    fn test_find_codex_binary_env_non_executable() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let non_exec = dir.path().join("codex-not-executable");
        // Create the file but do NOT set executable bit
        std::fs::write(&non_exec, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&non_exec, std::fs::Permissions::from_mode(0o644)).unwrap();

        let _guard = EnvGuard::set("CODEX_BINARY", non_exec.to_str().unwrap());
        let result = find_codex_binary();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::CodexNotFound));
    }

    #[test]
    #[cfg(unix)]
    fn test_find_codex_binary_env_directory_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();

        let _guard = EnvGuard::set("CODEX_BINARY", dir.path().to_str().unwrap());
        let result = find_codex_binary();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ActualError::CodexNotFound));
    }

    // ---- Additional: runner uses self.model (ignores model_override) ----
    //
    // The Codex runner ignores model_override from ConcurrentTailoringConfig
    // (which may contain Claude aliases like "haiku") and always uses the
    // model set on the runner itself (resolved from --model flag or
    // config.model in sync_wiring).

    #[tokio::test]
    #[cfg(unix)]
    async fn test_runner_uses_self_model_not_override() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let args_file = dir.path().join("captured-args.txt");
        // Script captures args AND writes valid JSON to --output-last-message file.
        let script_content = format!(
            r#"#!/bin/sh
OUTPUT_FILE=""
NEXT_IS_OUTPUT=false
for arg in "$@"; do
    echo "$arg" >> "{args_file}"
    if $NEXT_IS_OUTPUT; then
        OUTPUT_FILE="$arg"
        NEXT_IS_OUTPUT=false
    elif [ "$arg" = "--output-last-message" ]; then
        NEXT_IS_OUTPUT=true
    fi
done
echo '{json}' > "$OUTPUT_FILE"
"#,
            args_file = args_file.display(),
            json = minimal_tailoring_json()
        );
        let script = dir.path().join("fake-codex.sh");
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Runner is constructed with model "gpt-5" (set by sync_wiring).
        // model_override is "haiku" (a Claude alias from config.model).
        // The runner should use "gpt-5", NOT "haiku".
        let runner =
            CodexCliRunner::new(script, Some("gpt-5".to_string()), Duration::from_secs(10));
        runner
            .run_tailoring("prompt", r#"{"type":"object"}"#, Some("haiku"), None)
            .await
            .unwrap();

        let captured = std::fs::read_to_string(&args_file).unwrap();

        // Verify exec subcommand and --full-auto
        assert!(
            captured.starts_with("exec\n"),
            "should start with exec subcommand"
        );
        assert!(
            captured.contains("--full-auto\n"),
            "--full-auto flag missing"
        );

        // Verify self.model ("gpt-5") is used, NOT model_override ("haiku")
        assert!(
            captured.contains("--model\ngpt-5\n"),
            "--model gpt-5 missing (should use self.model, not model_override)"
        );
        assert!(
            !captured.contains("haiku"),
            "haiku should NOT appear in args (model_override should be ignored)"
        );

        // Verify --output-last-message is present
        assert!(
            captured.contains("--output-last-message\n"),
            "--output-last-message flag missing"
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

    // ---- Additional: runner omits --model when self.model is None ----

    #[tokio::test]
    #[cfg(unix)]
    async fn test_runner_omits_model_when_none() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let args_file = dir.path().join("captured-args.txt");
        let script_content = format!(
            r#"#!/bin/sh
OUTPUT_FILE=""
NEXT_IS_OUTPUT=false
for arg in "$@"; do
    echo "$arg" >> "{args_file}"
    if $NEXT_IS_OUTPUT; then
        OUTPUT_FILE="$arg"
        NEXT_IS_OUTPUT=false
    elif [ "$arg" = "--output-last-message" ]; then
        NEXT_IS_OUTPUT=true
    fi
done
echo '{json}' > "$OUTPUT_FILE"
"#,
            args_file = args_file.display(),
            json = minimal_tailoring_json()
        );
        let script = dir.path().join("fake-codex.sh");
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Runner has no model (None) — Codex CLI should use its own default.
        let runner = CodexCliRunner::new(script, None, Duration::from_secs(10));
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

    // ---- Additional: build_codex_args produces correct structure ----

    #[test]
    fn test_build_codex_args_with_model() {
        use std::path::Path;
        let output_path = Path::new("/tmp/codex-output.txt");
        let args = build_codex_args("my prompt", Some("gpt-5"), output_path).unwrap();
        assert_eq!(args[0], "exec");
        assert_eq!(args[1], "--full-auto");
        assert_eq!(args[2], "--model");
        assert_eq!(args[3], "gpt-5");
        assert_eq!(args[4], "--output-last-message");
        assert_eq!(args[5], "/tmp/codex-output.txt");
        assert_eq!(args[6], "my prompt");
    }

    #[test]
    fn test_build_codex_args_without_model_uses_codex_default() {
        use std::path::Path;
        let output_path = Path::new("/tmp/codex-output.txt");
        let args = build_codex_args("my prompt", None, output_path).unwrap();
        assert_eq!(args[0], "exec");
        assert_eq!(args[1], "--full-auto");
        assert_eq!(args[2], "--output-last-message");
        assert_eq!(args[3], "/tmp/codex-output.txt");
        assert_eq!(args[4], "my prompt");
        // No --model flag — Codex CLI uses its own default
        assert!(
            !args.iter().any(|a| a == "--model"),
            "--model should not be present when model is None"
        );
    }

    // ---- Security: build_codex_args rejects flag-like model names ----

    #[test]
    fn test_build_codex_args_rejects_flag_model() {
        use std::path::Path;
        let p = Path::new("/tmp/out.txt");
        let err = build_codex_args("valid prompt", Some("--dangerous-flag"), p).unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(_)),
            "expected ConfigError, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains("Invalid model name"), "message: {msg}");
    }

    #[test]
    fn test_build_codex_args_rejects_model_with_spaces() {
        use std::path::Path;
        let p = Path::new("/tmp/out.txt");
        let err = build_codex_args("valid prompt", Some("model with spaces"), p).unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(_)));
    }

    #[test]
    fn test_build_codex_args_rejects_model_with_shell_metacharacters() {
        use std::path::Path;
        let p = Path::new("/tmp/out.txt");
        for bad_model in &["model|cmd", "model;cmd", "model$var", "model`cmd`"] {
            let err = build_codex_args("valid prompt", Some(bad_model), p).unwrap_err();
            assert!(
                matches!(err, ActualError::ConfigError(_)),
                "expected ConfigError for model {bad_model:?}, got: {err:?}"
            );
        }
    }

    #[test]
    fn test_build_codex_args_rejects_flag_prompt() {
        use std::path::Path;
        let p = Path::new("/tmp/out.txt");
        let err = build_codex_args("--bad-flag", Some("gpt-5"), p).unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(_)),
            "expected ConfigError, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains("Invalid prompt"), "message: {msg}");
    }

    #[test]
    fn test_build_codex_args_accepts_valid_model_names() {
        use std::path::Path;
        let p = Path::new("/tmp/out.txt");
        let valid_models = [
            "gpt-5",
            "gpt-5.2-codex",
            "gpt-5.1-codex-mini",
            "gpt-5.2",
            "claude-sonnet-4-6",
            "openai/gpt-5",
            "provider/model-name_v2",
        ];
        for model in &valid_models {
            assert!(
                build_codex_args("prompt", Some(model), p).is_ok(),
                "expected Ok for model {model:?}"
            );
        }
    }

    // ---- strip_markdown_json_fences tests ----

    #[test]
    fn test_strip_fences_json_block() {
        let input = "```json\n{\"key\": \"value\"}\n```";
        assert_eq!(strip_markdown_json_fences(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn test_strip_fences_bare_block() {
        let input = "```\n{\"key\": \"value\"}\n```";
        assert_eq!(strip_markdown_json_fences(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn test_strip_fences_no_fences() {
        let input = r#"{"key": "value"}"#;
        assert_eq!(strip_markdown_json_fences(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn test_strip_fences_with_surrounding_whitespace() {
        let input = "  \n```json\n{\"key\": \"value\"}\n```\n  ";
        assert_eq!(strip_markdown_json_fences(input), r#"{"key": "value"}"#);
    }

    // ---- Additional: spawn failure (bad binary path) ----

    #[tokio::test]
    async fn test_spawn_failure() {
        let runner = CodexCliRunner::new(
            PathBuf::from("/nonexistent/codex/binary"),
            Some("gpt-5.2-codex".to_string()),
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

    // ---- with_api_key and API key injection ----

    #[tokio::test]
    #[cfg(unix)]
    async fn test_with_api_key_injects_env_var() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let env_file = dir.path().join("captured-env.txt");
        // Script captures OPENAI_API_KEY and writes valid JSON to the output file.
        let script_content = format!(
            r#"#!/bin/sh
OUTPUT_FILE=""
NEXT_IS_OUTPUT=false
for arg in "$@"; do
    if $NEXT_IS_OUTPUT; then
        OUTPUT_FILE="$arg"
        NEXT_IS_OUTPUT=false
    elif [ "$arg" = "--output-last-message" ]; then
        NEXT_IS_OUTPUT=true
    fi
done
echo "$OPENAI_API_KEY" > "{env_file}"
echo '{json}' > "$OUTPUT_FILE"
"#,
            env_file = env_file.display(),
            json = minimal_tailoring_json()
        );
        let script = dir.path().join("fake-codex.sh");
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner =
            CodexCliRunner::new(script, Some("gpt-5".to_string()), Duration::from_secs(10))
                .with_api_key("sk-test-key-12345".to_string());

        runner
            .run_tailoring("prompt", r#"{"type":"object"}"#, None, None)
            .await
            .unwrap();

        let captured_key = std::fs::read_to_string(&env_file).unwrap();
        assert_eq!(captured_key.trim(), "sk-test-key-12345");
    }

    // ---- Empty output file (exit 0, no content) with error in stdout ----

    #[tokio::test]
    #[cfg(unix)]
    async fn test_empty_output_with_error_detail_in_stdout() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        // Script writes error to stdout but exits 0 with empty output file.
        let script_content = r#"#!/bin/sh
OUTPUT_FILE=""
NEXT_IS_OUTPUT=false
for arg in "$@"; do
    if $NEXT_IS_OUTPUT; then
        OUTPUT_FILE="$arg"
        NEXT_IS_OUTPUT=false
    elif [ "$arg" = "--output-last-message" ]; then
        NEXT_IS_OUTPUT=true
    fi
done
echo '[2025-01-01T00:00:00Z] ERROR: unexpected status 400 Bad Request: {"detail":"The model is not supported"}' >&1
echo "" > "$OUTPUT_FILE"
"#;
        let script = dir.path().join("fake-codex.sh");
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner =
            CodexCliRunner::new(script, Some("gpt-5".to_string()), Duration::from_secs(10));
        let result = runner
            .run_tailoring("prompt", r#"{"type":"object"}"#, None, None)
            .await;

        let (message, _stderr) = subprocess_failed(result.unwrap_err());
        assert!(
            message.contains("The model is not supported"),
            "message should contain detail: {message}"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_empty_output_without_error_detail() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        // Script exits 0 with empty output file and no error details.
        let script_content = r#"#!/bin/sh
OUTPUT_FILE=""
NEXT_IS_OUTPUT=false
for arg in "$@"; do
    if $NEXT_IS_OUTPUT; then
        OUTPUT_FILE="$arg"
        NEXT_IS_OUTPUT=false
    elif [ "$arg" = "--output-last-message" ]; then
        NEXT_IS_OUTPUT=true
    fi
done
echo "" > "$OUTPUT_FILE"
"#;
        let script = dir.path().join("fake-codex.sh");
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner =
            CodexCliRunner::new(script, Some("gpt-5".to_string()), Duration::from_secs(10));
        let result = runner
            .run_tailoring("prompt", r#"{"type":"object"}"#, None, None)
            .await;

        let (message, _stderr) = subprocess_failed(result.unwrap_err());
        assert!(
            message.contains("output file is empty"),
            "message: {message}"
        );
    }

    // ---- Output file read failure (file deleted before read) ----

    #[tokio::test]
    #[cfg(unix)]
    async fn test_output_file_read_failure() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        // Script deletes the output file (simulating a race or permission issue).
        // We use /bin/rm with absolute path to ensure it's found.
        let script_content = "#!/bin/sh\n\
            OUTPUT_FILE=\"\"\n\
            NEXT_IS_OUTPUT=false\n\
            for arg in \"$@\"; do\n\
                if $NEXT_IS_OUTPUT; then\n\
                    OUTPUT_FILE=\"$arg\"\n\
                    NEXT_IS_OUTPUT=false\n\
                elif [ \"$arg\" = \"--output-last-message\" ]; then\n\
                    NEXT_IS_OUTPUT=true\n\
                fi\n\
            done\n\
            /bin/rm -f \"$OUTPUT_FILE\"\n";
        let script = dir.path().join("fake-codex.sh");
        std::fs::write(&script, script_content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner =
            CodexCliRunner::new(script, Some("gpt-5".to_string()), Duration::from_secs(10));
        let result = runner
            .run_tailoring("prompt", r#"{"type":"object"}"#, None, None)
            .await;

        let (message, _stderr) = subprocess_failed(result.unwrap_err());
        assert!(
            message.contains("Failed to read Codex output file"),
            "message: {message}"
        );
    }

    // ---- extract_codex_error_detail unit tests ----

    #[test]
    fn test_extract_error_detail_json_fragment() {
        let output = r#"[2025-01-01] ERROR: unexpected status 400: {"detail":"The 'gpt-5.2' model is not supported"}"#;
        let detail = extract_codex_error_detail(output).unwrap();
        assert_eq!(detail, "The 'gpt-5.2' model is not supported");
    }

    #[test]
    fn test_extract_error_detail_error_line_fallback() {
        let output = "[2025-01-01] ERROR: connection refused\n";
        let detail = extract_codex_error_detail(output).unwrap();
        assert_eq!(detail, "connection refused");
    }

    #[test]
    fn test_extract_error_detail_none_when_no_match() {
        let detail = extract_codex_error_detail("all good, no errors here");
        assert!(detail.is_none());
    }

    #[test]
    fn test_extract_error_detail_empty_detail_skipped() {
        // Empty "detail" value should be skipped, falling through to ERROR line
        let output = r#"{"detail":""} [ts] ERROR: real error message"#;
        let detail = extract_codex_error_detail(output).unwrap();
        assert_eq!(detail, "real error message");
    }

    #[test]
    fn test_extract_error_detail_empty_everything() {
        // No detail, no ERROR lines → None
        let detail = extract_codex_error_detail(r#"{"detail":""}"#);
        assert!(detail.is_none());
    }

    #[test]
    fn test_extract_error_detail_escaped_quotes() {
        // Escaped quotes in the detail value — old string parsing would truncate
        let output = r#"[ts] ERROR: status 400: {"detail":"Model \"gpt-5\" is not supported"}"#;
        let detail = extract_codex_error_detail(output).unwrap();
        assert_eq!(detail, r#"Model "gpt-5" is not supported"#);
    }

    #[test]
    fn test_extract_error_detail_spaces_around_colon() {
        let output = r#"{"detail" : "spaced out error"}"#;
        let detail = extract_codex_error_detail(output).unwrap();
        assert_eq!(detail, "spaced out error");
    }

    #[test]
    fn test_extract_error_detail_no_closing_brace() {
        // `{` found but no `}` — hits the `break` branch, falls through to ERROR
        let output = "prefix { unterminated\n] ERROR: fallback";
        let detail = extract_codex_error_detail(output).unwrap();
        assert_eq!(detail, "fallback");
    }

    #[test]
    fn test_extract_error_detail_json_without_detail_key() {
        // Valid JSON but no "detail" key — skipped, falls through to ERROR
        let output = r#"{"error":"not found"} [ts] ERROR: real error"#;
        let detail = extract_codex_error_detail(output).unwrap();
        assert_eq!(detail, "real error");
    }

    #[test]
    fn test_extract_error_detail_invalid_json_braces() {
        // `{...}` present but not valid JSON — skipped, falls through to ERROR
        let output = "{not json} ] ERROR: fallback msg";
        let detail = extract_codex_error_detail(output).unwrap();
        assert_eq!(detail, "fallback msg");
    }

    // ---- strip_markdown_json_fences: bare fences without newline ----

    #[test]
    fn test_strip_fences_bare_block_no_newline() {
        // Bare ``` immediately followed by content (no newline after opening fence)
        let input = "```{\"key\": \"value\"}\n```";
        assert_eq!(strip_markdown_json_fences(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn test_strip_fences_opening_fence_no_closing() {
        // Opening ```json found but no closing ``` — returns trimmed input as-is
        let input = "```json\n{\"key\": \"value\"}";
        assert_eq!(
            strip_markdown_json_fences(input),
            "```json\n{\"key\": \"value\"}"
        );
    }

    // ---- extract_codex_error_detail: edge cases ----

    #[test]
    fn test_extract_error_detail_malformed_no_closing_quote() {
        // Malformed JSON (no closing brace) — serde_json parse fails,
        // falls through to the ERROR line fallback.
        let output = "\"detail\":\"unterminated\n] ERROR: fallback error";
        let detail = extract_codex_error_detail(output).unwrap();
        assert_eq!(detail, "fallback error");

        // Malformed JSON with no ERROR fallback → None
        let output2 = "\"detail\":\"no closing quote here";
        assert!(extract_codex_error_detail(output2).is_none());
    }

    // ---- check_codex_auth tests ----

    #[test]
    fn test_check_codex_auth_public_with_api_key() {
        // Exercises the public wrapper (covers dirs::home_dir path).
        assert!(check_codex_auth(Some("sk-test-key")).is_ok());
    }

    #[test]
    fn test_check_codex_auth_public_without_api_key() {
        // Exercises the public wrapper without an API key.
        // Result depends on whether ~/.codex/auth.json exists on the test machine,
        // so we just verify it doesn't panic and returns a valid Result.
        let _ = check_codex_auth(None);
    }

    #[test]
    fn test_check_codex_auth_with_api_key() {
        // Valid API key should always succeed, regardless of auth file.
        assert!(check_codex_auth_inner(Some("sk-test-key"), None).is_ok());
    }

    #[test]
    fn test_check_codex_auth_empty_key_no_auth_file() {
        // Empty API key + no auth file → error.
        let result = check_codex_auth_inner(Some(""), None);
        assert!(matches!(result, Err(ActualError::CodexNotAuthenticated)));
    }

    #[test]
    fn test_check_codex_auth_none_key_no_auth_file() {
        // None API key + no auth file → error.
        let result = check_codex_auth_inner(None, None);
        assert!(matches!(result, Err(ActualError::CodexNotAuthenticated)));
    }

    #[test]
    fn test_check_codex_auth_none_key_nonexistent_auth_file() {
        // Auth file path provided but file doesn't exist → error.
        let result =
            check_codex_auth_inner(None, Some(std::path::Path::new("/nonexistent/auth.json")));
        assert!(matches!(result, Err(ActualError::CodexNotAuthenticated)));
    }

    #[test]
    fn test_check_codex_auth_none_key_with_auth_file() {
        // Auth file exists → success even without API key.
        let dir = tempfile::tempdir().unwrap();
        let auth_file = dir.path().join("auth.json");
        std::fs::write(&auth_file, "{}").unwrap();
        assert!(check_codex_auth_inner(None, Some(&auth_file)).is_ok());
    }

    #[test]
    fn test_check_codex_auth_empty_key_with_auth_file() {
        // Empty API key but auth file exists → success via auth file.
        let dir = tempfile::tempdir().unwrap();
        let auth_file = dir.path().join("auth.json");
        std::fs::write(&auth_file, "{}").unwrap();
        assert!(check_codex_auth_inner(Some(""), Some(&auth_file)).is_ok());
    }

    #[test]
    fn test_check_codex_auth_key_takes_priority_over_auth_file() {
        // Valid API key should short-circuit without checking auth file.
        // Pass a nonexistent path — should still succeed because key is valid.
        assert!(check_codex_auth_inner(
            Some("sk-key"),
            Some(std::path::Path::new("/nonexistent/auth.json"))
        )
        .is_ok());
    }

    #[test]
    fn test_extract_error_detail_error_line_empty_message() {
        // ERROR line exists but the message after it is empty/whitespace only
        let output = "[ts] ERROR:   \nsome other line";
        let detail = extract_codex_error_detail(output);
        // Empty message after ERROR: should be skipped, and no other match → None
        assert!(detail.is_none());
    }
}
