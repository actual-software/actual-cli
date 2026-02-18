#![allow(dead_code)]

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
    s.replace('\'', "'\\''")
}

// ── Standard JSON constants ──────────────────────────────────────────

pub const AUTH_OK: &str =
    r#"{"loggedIn": true, "authMethod": "claude.ai", "email": "test@example.com"}"#;
pub const AUTH_FAIL: &str = r#"{"loggedIn": false}"#;

pub const ANALYSIS_SINGLE_PROJECT: &str = r#"{"is_monorepo": false, "projects": [{"path": ".", "name": "test-app", "languages": ["rust"], "frameworks": [], "package_manager": "cargo"}]}"#;

pub const ANALYSIS_MONOREPO: &str = r#"{"is_monorepo": true, "projects": [{"path": "apps/web", "name": "web-app", "languages": ["typescript"], "frameworks": [{"name": "nextjs", "category": "web-frontend"}], "package_manager": "npm"}, {"path": "apps/api", "name": "api-server", "languages": ["rust"], "frameworks": [], "package_manager": "cargo"}, {"path": "libs/shared", "name": "shared-lib", "languages": ["typescript"], "frameworks": [], "package_manager": "npm"}]}"#;

// ── Fake binary builders (Unix only) ────────────────────────────────

/// Create a fake Claude binary that handles `auth` and `--print` invocations.
#[cfg(unix)]
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
/// by checking whether `skipped_adrs` appears in the arguments.
#[cfg(unix)]
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
    let tailoring = shell_single_quote_escape(tailoring_json);
    let script_content = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"auth\" ]; then\n\
         printf '%s\\n' '{auth}'\n\
         exit 0\n\
         elif [ \"$1\" = \"--print\" ]; then\n\
         if echo \"$@\" | grep -q \"skipped_adrs\"; then\n\
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
#[cfg(unix)]
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
/// analysis from tailoring (by checking for `skipped_adrs` in `$@`).
///
/// Each invocation is written to `capture_file` with args delimited by `\x00`
/// and invocations separated by `---INVOCATION---\n`. Use
/// [`parse_captured_invocations`] to parse the output.
#[cfg(unix)]
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
    let tailoring = shell_single_quote_escape(tailoring_json);
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
  if echo "$@" | grep -q "skipped_adrs"; then
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
#[cfg(unix)]
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

pub struct TestEnv {
    pub dir: tempfile::TempDir,
    pub config_path: std::path::PathBuf,
    pub binary_path: std::path::PathBuf,
    pub api_url: String,
}

#[cfg(unix)]
impl TestEnv {
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

    pub fn cmd(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::from(assert_cmd::cargo::cargo_bin_cmd!("actual"));
        cmd.env("CLAUDE_BINARY", self.binary_path.to_str().unwrap());
        cmd.env("ACTUAL_CONFIG", self.config_path.to_str().unwrap());
        cmd.current_dir(self.dir.path());
        cmd
    }

    pub fn read_file(&self, path: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(path)).unwrap()
    }

    pub fn file_exists(&self, path: &str) -> bool {
        self.dir.path().join(path).exists()
    }

    pub fn write_file(&self, path: &str, content: &str) {
        let full_path = self.dir.path().join(path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full_path, content).unwrap();
    }

    /// Create a pnpm monorepo directory structure that the static analyzer
    /// will detect as a monorepo with projects at apps/web, apps/api, and
    /// libs/shared — matching the layout expected by ANALYSIS_MONOREPO.
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
