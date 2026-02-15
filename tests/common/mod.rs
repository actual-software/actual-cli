#![allow(dead_code)]

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
    let script_content = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"auth\" ]; then\n\
         printf '%s\\n' '{}'\n\
         exit 0\n\
         elif [ \"$1\" = \"--print\" ]; then\n\
         printf '%s\\n' '{}'\n\
         exit 0\n\
         else\n\
         echo \"unexpected args: $@\" >&2\n\
         exit 1\n\
         fi\n",
        auth_json, analysis_json,
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
        auth = auth_json,
        tailoring = tailoring_json,
        analysis = analysis_json,
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
    let capture_str = capture_file.to_str().unwrap();
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
        auth = auth_json,
        capture = capture_str,
        analysis = analysis_json,
    );
    std::fs::write(&script, script_content).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
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
}
