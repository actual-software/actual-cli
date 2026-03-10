// ── Shared mutex for env-var serialization ───────────────────────────

/// Global mutex used by integration tests to serialize access to
/// environment variables. Integration tests cannot access
/// `actual_cli::testutil::ENV_MUTEX` from within the same process
/// context when it matters most, so we provide one here for
/// `tests/` modules that need it.
// Shared test helper — used by lib_test but appears dead in other test binaries.
#[allow(dead_code)]
pub static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ── RAII env-var guard ───────────────────────────────────────────────

/// RAII guard that saves and restores (or removes) an environment variable.
///
/// On construction, saves the previous value and sets the new one.
/// On drop, restores the previous value (or removes the variable if it
/// was absent before). Requires `ENV_MUTEX` to be held by the caller.
// Shared test helper — used by lib_test but appears dead in other test binaries.
#[allow(dead_code)]
pub struct EnvGuard {
    key: String,
    old: Option<String>,
}

impl EnvGuard {
    /// Set `key` to `val`, saving the previous value for restoration on drop.
    ///
    /// The caller must hold `ENV_MUTEX` (or equivalent) before calling this
    /// function to serialise access, as required by the deprecated
    /// `set_var`/`remove_var` APIs.
    // Shared test helper — used by lib_test but appears dead in other test binaries.
    #[allow(dead_code)]
    pub fn set(key: &str, val: &str) -> Self {
        let old = std::env::var(key).ok();
        #[allow(deprecated)]
        unsafe {
            std::env::set_var(key, val)
        };
        Self {
            key: key.to_string(),
            old,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.old {
            Some(v) => {
                #[allow(deprecated)]
                unsafe {
                    std::env::set_var(&self.key, v)
                }
            }
            None => {
                #[allow(deprecated)]
                unsafe {
                    std::env::remove_var(&self.key)
                }
            }
        }
    }
}

// ── Shell escaping helpers ───────────────────────────────────────────

/// Escape a string so it is safe to embed inside a shell single-quoted string.
///
/// In POSIX shell, single-quoted strings cannot contain a literal single
/// quote. The idiomatic escape is to end the single-quoted segment, emit a
/// backslash-escaped (or double-quoted) single quote, then re-open the
/// single-quoted segment: `'` → `'\''`.
///
/// Example: `it's` becomes `it'\''s`, which when wrapped as `'it'\''s'`
/// yields the string `it's` in the shell.
#[cfg(unix)]
pub fn shell_single_quote_escape(s: &str) -> String {
    debug_assert!(
        !s.contains('\x00'),
        "shell_single_quote_escape: null bytes not supported — they are silently stripped by POSIX shells"
    );
    s.replace('\'', "'\\''")
}

// ── Standard JSON constants ──────────────────────────────────────────

// Shared test helpers — each constant is used by some test binaries but
// appears dead in others due to per-binary compilation of integration tests.
#[allow(dead_code)]
pub const AUTH_OK: &str =
    r#"{"loggedIn": true, "authMethod": "claude.ai", "email": "test@example.com"}"#;
#[allow(dead_code)]
pub const AUTH_FAIL: &str = r#"{"loggedIn": false}"#;

#[allow(dead_code)]
pub const ANALYSIS_SINGLE_PROJECT: &str = r#"{"is_monorepo": false, "projects": [{"path": ".", "name": "test-app", "languages": ["rust"], "frameworks": [], "package_manager": "cargo"}]}"#;

#[allow(dead_code)]
pub const ANALYSIS_MONOREPO: &str = r#"{"is_monorepo": true, "projects": [{"path": "apps/web", "name": "web-app", "languages": ["typescript"], "frameworks": [{"name": "nextjs", "category": "web-frontend"}], "package_manager": "npm"}, {"path": "apps/api", "name": "api-server", "languages": ["rust"], "frameworks": [], "package_manager": "cargo"}, {"path": "libs/shared", "name": "shared-lib", "languages": ["typescript"], "frameworks": [], "package_manager": "npm"}]}"#;

// ── Platform support note ────────────────────────────────────────────
//
// Integration tests are **Unix-only** because the fake Claude binary stubs are
// implemented as POSIX shell scripts (`#!/bin/sh`). This is a known limitation.
//
// To add Windows support, the shell script approach would need to be replaced
// with one of:
//   1. Compiled Rust test-helper binaries (cross-platform, most robust)
//   2. `.bat` / `.cmd` scripts (Windows-only, requires separate implementation)
//   3. A cross-platform script runner (e.g., `python` subprocess)
//
// Until then, all integration test files gate their contents with `#[cfg(unix)]`.

// ── Fake binary builders (Unix only) ────────────────────────────────

/// Create a fake Claude binary that handles `auth` and `--print` invocations.
// Shared test helper — used by some test binaries but appears dead in others.
#[cfg(unix)]
#[allow(dead_code)]
pub fn create_fake_claude_binary(
    dir: &std::path::Path,
    auth_json: &str,
    analysis_json: &str,
) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("fake-claude");
    let auth = shell_single_quote_escape(auth_json);
    let analysis = shell_single_quote_escape(analysis_json);
    let script_content = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"auth\" ]; then\n\
         printf '%s\\n' '{auth}'\n\
         exit 0\n\
         elif [ \"$1\" = \"--print\" ]; then\n\
         printf '%s\\n' '{analysis}'\n\
         exit 0\n\
         else\n\
         echo \"unexpected args: $@\" >&2\n\
         exit 1\n\
         fi\n",
        auth = auth,
        analysis = analysis,
    );
    std::fs::write(&script, script_content).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// Create a fake Claude binary that distinguishes analysis from tailoring
/// by checking whether `--json-schema` appears as an argument (tailoring
/// invocations always pass `--json-schema`; analysis invocations never do).
///
/// The tailoring response is wrapped in a `stream-json` result envelope so the
/// production `run_subprocess_streaming` code can parse it correctly.
// Shared test helper — used by some test binaries but appears dead in others.
#[cfg(unix)]
#[allow(dead_code)]
pub fn create_fake_claude_binary_with_tailoring(
    dir: &std::path::Path,
    auth_json: &str,
    analysis_json: &str,
    tailoring_json: &str,
) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("fake-claude");
    let auth = shell_single_quote_escape(auth_json);
    let analysis = shell_single_quote_escape(analysis_json);
    // Wrap the tailoring payload in a stream-json result envelope so
    // `run_subprocess_streaming` can extract `structured_output`.
    let tailoring_value: serde_json::Value =
        serde_json::from_str(tailoring_json).expect("tailoring_json must be valid JSON");
    let envelope = serde_json::json!({
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "structured_output": tailoring_value,
    });
    let tailoring = shell_single_quote_escape(&envelope.to_string());
    let script_content = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"auth\" ]; then\n\
         printf '%s\\n' '{auth}'\n\
         exit 0\n\
         elif [ \"$1\" = \"--print\" ]; then\n\
         _is_tailoring=0\n\
         for _arg in \"$@\"; do\n\
         if [ \"$_arg\" = \"--json-schema\" ]; then\n\
         _is_tailoring=1\n\
         break\n\
         fi\n\
         done\n\
         if [ \"$_is_tailoring\" = \"1\" ]; then\n\
         printf '%s\\n' '{tailoring}'\n\
         exit 0\n\
         else\n\
         printf '%s\\n' '{analysis}'\n\
         exit 0\n\
         fi\n\
         else\n\
         echo \"unexpected args: $@\" >&2\n\
         exit 1\n\
         fi\n",
        auth = auth,
        tailoring = tailoring,
        analysis = analysis,
    );
    std::fs::write(&script, script_content).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// Create a fake Claude binary that captures all args on `--print` invocations.
// Shared test helper — used by some test binaries but appears dead in others.
#[cfg(unix)]
#[allow(dead_code)]
pub fn create_fake_claude_binary_capturing(
    dir: &std::path::Path,
    auth_json: &str,
    analysis_json: &str,
    capture_file: &std::path::Path,
) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("fake-claude");
    let auth = shell_single_quote_escape(auth_json);
    let analysis = shell_single_quote_escape(analysis_json);
    // Escape the capture path so it is safe inside single quotes.
    let capture = shell_single_quote_escape(capture_file.to_str().unwrap());
    let script_content = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"auth\" ]; then\n\
         printf '%s\\n' '{auth}'\n\
         exit 0\n\
         elif [ \"$1\" = \"--print\" ]; then\n\
         echo \"$@\" >> '{capture}'\n\
         printf '%s\\n' '{analysis}'\n\
         exit 0\n\
         else\n\
         echo \"unexpected args: $@\" >&2\n\
         exit 1\n\
         fi\n",
        auth = auth,
        capture = capture,
        analysis = analysis,
    );
    std::fs::write(&script, script_content).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// Create a fake Claude binary that captures all args AND distinguishes
/// analysis from tailoring by checking whether `--json-schema` appears as
/// an argument (tailoring invocations always pass `--json-schema`; analysis
/// invocations never do).
///
/// Each invocation is written to `capture_file` with args delimited by `\x00`
/// and invocations separated by `---INVOCATION---\n`. Use
/// [`parse_captured_invocations`] to parse the output.
///
/// The tailoring response is wrapped in a `stream-json` result envelope so the
/// production `run_subprocess_streaming` code can parse it correctly.
// Shared test helper — used by some test binaries but appears dead in others.
#[cfg(unix)]
#[allow(dead_code)]
pub fn create_fake_claude_binary_capturing_with_tailoring(
    dir: &std::path::Path,
    auth_json: &str,
    analysis_json: &str,
    tailoring_json: &str,
    capture_file: &std::path::Path,
) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("fake-claude");
    let auth = shell_single_quote_escape(auth_json);
    let analysis = shell_single_quote_escape(analysis_json);
    // Wrap the tailoring payload in a stream-json result envelope.
    let tailoring_value: serde_json::Value =
        serde_json::from_str(tailoring_json).expect("tailoring_json must be valid JSON");
    let envelope = serde_json::json!({
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "structured_output": tailoring_value,
    });
    let tailoring = shell_single_quote_escape(&envelope.to_string());
    // Escape the capture path so it is safe inside single quotes.
    let capture = shell_single_quote_escape(capture_file.to_str().unwrap());
    // Use printf with \0 delimiters between args and a clear invocation separator
    let script_content = format!(
        r#"#!/bin/sh
if [ "$1" = "auth" ]; then
  printf '%s\n' '{auth}'
  exit 0
elif [ "$1" = "--print" ]; then
  printf '%s\n' '---INVOCATION---' >> '{capture}'
  for arg in "$@"; do
    printf '%s\0' "$arg" >> '{capture}'
  done
  printf '\n' >> '{capture}'
  _is_tailoring=0
  for _arg in "$@"; do
    if [ "$_arg" = "--json-schema" ]; then
      _is_tailoring=1
      break
    fi
  done
  if [ "$_is_tailoring" = "1" ]; then
    printf '%s\n' '{tailoring}'
    exit 0
  else
    printf '%s\n' '{analysis}'
    exit 0
  fi
else
  echo "unexpected args: $@" >&2
  exit 1
fi
"#,
        auth = auth,
        capture = capture,
        tailoring = tailoring,
        analysis = analysis,
    );
    std::fs::write(&script, script_content).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// Parse the captured invocations file written by
/// `create_fake_claude_binary_capturing_with_tailoring`.
///
/// Returns a list of invocations, where each invocation is a list of args.
// Shared test helper — used by some test binaries but appears dead in others.
#[cfg(unix)]
#[allow(dead_code)]
pub fn parse_captured_invocations(content: &str) -> Vec<Vec<String>> {
    content
        .split("---INVOCATION---\n")
        .filter(|s| !s.is_empty())
        .map(|invocation| {
            invocation
                .trim_end_matches('\n')
                .split('\0')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect()
        })
        .collect()
}

// ── JSON builders ───────────────────────────────────────────────────

/// Wrap an ADR array JSON string in a full MatchResponse envelope.
// Shared test helper — used by some test binaries but appears dead in others.
#[allow(dead_code)]
pub fn make_match_response_json(adrs_json: &str) -> String {
    let adrs: Vec<serde_json::Value> = serde_json::from_str(adrs_json).unwrap();
    let total = adrs.len() as u32;

    let mut by_framework: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for adr in &adrs {
        if let Some(applies_to) = adr.get("applies_to") {
            if let Some(frameworks) = applies_to.get("frameworks").and_then(|f| f.as_array()) {
                for fw in frameworks {
                    if let Some(name) = fw.as_str() {
                        *by_framework.entry(name.to_string()).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    let metadata = serde_json::json!({
        "total_matched": total,
        "by_framework": by_framework,
        "deduplicated_count": total,
    });

    serde_json::json!({
        "matched_adrs": adrs,
        "metadata": metadata,
    })
    .to_string()
}

/// Build a single ADR JSON object with sensible defaults.
// Shared test helper — used by some test binaries but appears dead in others.
#[allow(dead_code)]
pub fn make_adr_json(
    id: &str,
    title: &str,
    policies: &[&str],
    instructions: &[&str],
    matched_projects: &[&str],
) -> String {
    serde_json::json!({
        "id": id,
        "title": title,
        "context": null,
        "policies": policies,
        "instructions": instructions,
        "category": {
            "id": "cat-general",
            "name": "General",
            "path": "General"
        },
        "applies_to": {
            "languages": ["rust"],
            "frameworks": []
        },
        "matched_projects": matched_projects,
    })
    .to_string()
}

// ── TestEnv ─────────────────────────────────────────────────────────

// Shared test helper — used by some test binaries but appears dead in others.
#[allow(dead_code)]
pub struct TestEnv {
    pub dir: tempfile::TempDir,
    pub config_path: std::path::PathBuf,
    pub binary_path: std::path::PathBuf,
    pub api_url: String,
}

// TestEnv is Unix-only; see the platform support note above.
#[cfg(unix)]
impl TestEnv {
    #[allow(dead_code)]
    pub fn new(server: &mockito::Server, auth_json: &str, analysis_json: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.yaml");
        let binary_path = create_fake_claude_binary(dir.path(), auth_json, analysis_json);
        let api_url = server.url();
        Self {
            dir,
            config_path,
            binary_path,
            api_url,
        }
    }

    #[allow(dead_code)]
    pub fn new_with_tailoring(
        server: &mockito::Server,
        auth_json: &str,
        analysis_json: &str,
        tailoring_json: &str,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.yaml");
        let binary_path = create_fake_claude_binary_with_tailoring(
            dir.path(),
            auth_json,
            analysis_json,
            tailoring_json,
        );
        let api_url = server.url();
        Self {
            dir,
            config_path,
            binary_path,
            api_url,
        }
    }

    #[allow(dead_code)]
    pub fn cmd(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::from(assert_cmd::cargo::cargo_bin_cmd!("actual"));
        cmd.env("CLAUDE_BINARY", self.binary_path.to_str().unwrap());
        cmd.env("ACTUAL_CONFIG", self.config_path.to_str().unwrap());
        cmd.current_dir(self.dir.path());
        cmd
    }

    #[allow(dead_code)]
    pub fn read_file(&self, path: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(path)).unwrap()
    }

    #[allow(dead_code)]
    pub fn file_exists(&self, path: &str) -> bool {
        self.dir.path().join(path).exists()
    }

    #[allow(dead_code)]
    pub fn write_file(&self, path: &str, content: &str) {
        let full_path = self.dir.path().join(path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full_path, content).unwrap()
    }

    /// Create a pnpm monorepo directory structure that the static analyzer
    /// will detect as a monorepo with projects at apps/web, apps/api, and
    /// libs/shared — matching the layout expected by ANALYSIS_MONOREPO.
    #[allow(dead_code)]
    pub fn setup_monorepo(&self) {
        self.write_file(
            "pnpm-workspace.yaml",
            "packages:\n  - \"apps/*\"\n  - \"libs/*\"\n",
        );
        self.write_file("apps/web/package.json", r#"{"name": "web-app"}"#);
        self.write_file(
            "apps/api/Cargo.toml",
            "[package]\nname = \"api-server\"\nversion = \"0.1.0\"\n",
        );
        self.write_file("libs/shared/package.json", r#"{"name": "shared-lib"}"#);
    }
}

#[cfg(all(test, unix))]
mod shell_escape_tests {
    use super::shell_single_quote_escape;

    #[test]
    fn escape_empty_string() {
        assert_eq!(shell_single_quote_escape(""), "");
    }

    #[test]
    fn escape_no_special_chars() {
        assert_eq!(shell_single_quote_escape("hello world"), "hello world");
        assert_eq!(
            shell_single_quote_escape("no-quotes-here"),
            "no-quotes-here"
        );
    }

    #[test]
    fn escape_single_quote() {
        assert_eq!(shell_single_quote_escape("it's alive"), "it'\\''s alive");
    }

    #[test]
    fn escape_multiple_single_quotes() {
        assert_eq!(
            shell_single_quote_escape("it's all 'good'"),
            "it'\\''s all '\\''good'\\''",
        );
        assert_eq!(shell_single_quote_escape("'''"), "'\\'''\\'''\\''");
    }

    #[test]
    fn escape_quote_at_boundaries() {
        assert_eq!(shell_single_quote_escape("'leading"), "'\\''leading");
        assert_eq!(shell_single_quote_escape("trailing'"), "trailing'\\''");
    }

    #[test]
    fn escape_safe_passthrough_metacharacters() {
        // These chars are safe inside POSIX single-quoted strings — no escaping needed.
        assert_eq!(shell_single_quote_escape("foo`id`bar"), "foo`id`bar");
        assert_eq!(shell_single_quote_escape("$(id)"), "$(id)");
        assert_eq!(shell_single_quote_escape("cost $5"), "cost $5");
        assert_eq!(shell_single_quote_escape("say \"hello\""), "say \"hello\"");
        assert_eq!(shell_single_quote_escape("foo\\bar"), "foo\\bar");
    }

    #[test]
    fn escape_newline_passthrough() {
        // Literal newlines are safe in POSIX single-quoted strings
        assert_eq!(shell_single_quote_escape("line1\nline2"), "line1\nline2");
        assert_eq!(shell_single_quote_escape("line1\rline2"), "line1\rline2");
    }
}
