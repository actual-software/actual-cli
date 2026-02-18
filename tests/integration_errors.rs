#[cfg(unix)]
mod common;

#[cfg(unix)]
mod tests {
    use super::common::*;
    use predicates::prelude::*;

    // ── Custom fake binary builders ─────────────────────────────────────

    /// Create a fake binary that always exits with code 1 for auth invocations.
    fn create_auth_crash_binary(dir: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("fake-claude");
        let content = "#!/bin/sh\n\
                        if [ \"$1\" = \"auth\" ]; then\n\
                        echo 'auth subprocess crashed' >&2\n\
                        exit 1\n\
                        fi\n\
                        echo 'unexpected invocation' >&2\n\
                        exit 1\n";
        std::fs::write(&script, content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    /// Create a fake binary that handles auth + analysis OK but crashes on the
    /// tailoring invocation (detected via `skipped_adrs` in args).
    fn create_tailoring_crash_binary(
        dir: &std::path::Path,
        auth_json: &str,
        analysis_json: &str,
    ) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        // Write JSON to files to avoid shell quoting issues with special characters.
        let auth_file = dir.join("auth_response.json");
        let analysis_file = dir.join("analysis_response.json");
        std::fs::write(&auth_file, auth_json).unwrap();
        std::fs::write(&analysis_file, analysis_json).unwrap();
        let auth_path = auth_file.to_str().unwrap();
        let analysis_path = analysis_file.to_str().unwrap();
        let script = dir.join("fake-claude");
        let content = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"auth\" ]; then\n\
             cat '{auth_path}'\n\
             printf '\\n'\n\
             exit 0\n\
             elif [ \"$1\" = \"--print\" ]; then\n\
             if echo \"$@\" | grep -q \"skipped_adrs\"; then\n\
             echo 'tailoring subprocess crashed' >&2\n\
             exit 1\n\
             else\n\
             cat '{analysis_path}'\n\
             printf '\\n'\n\
             exit 0\n\
             fi\n\
             fi\n\
             echo 'unexpected invocation' >&2\n\
             exit 1\n",
            auth_path = auth_path,
            analysis_path = analysis_path,
        );
        std::fs::write(&script, content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    /// Create a fake binary that handles auth + analysis OK but returns invalid
    /// JSON for the tailoring invocation (detected via `skipped_adrs` in args).
    fn create_tailoring_invalid_json_binary(
        dir: &std::path::Path,
        auth_json: &str,
        analysis_json: &str,
    ) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        // Write JSON to files to avoid shell quoting issues with special characters.
        let auth_file = dir.join("auth_response.json");
        let analysis_file = dir.join("analysis_response.json");
        std::fs::write(&auth_file, auth_json).unwrap();
        std::fs::write(&analysis_file, analysis_json).unwrap();
        let auth_path = auth_file.to_str().unwrap();
        let analysis_path = analysis_file.to_str().unwrap();
        let script = dir.join("fake-claude");
        let content = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"auth\" ]; then\n\
             cat '{auth_path}'\n\
             printf '\\n'\n\
             exit 0\n\
             elif [ \"$1\" = \"--print\" ]; then\n\
             if echo \"$@\" | grep -q \"skipped_adrs\"; then\n\
             printf '%s\\n' 'not valid json'\n\
             exit 0\n\
             else\n\
             cat '{analysis_path}'\n\
             printf '\\n'\n\
             exit 0\n\
             fi\n\
             fi\n\
             echo 'unexpected invocation' >&2\n\
             exit 1\n",
            auth_path = auth_path,
            analysis_path = analysis_path,
        );
        std::fs::write(&script, content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    // ── API error tests ─────────────────────────────────────────────────

    #[test]
    fn api_server_error_retries_and_exits_3() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/adrs/match")
            .with_status(500)
            .with_body("Internal Server Error")
            .expect(3)
            .create();

        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);
        env.cmd()
            .args(["sync", "--force", "--no-tailor", "--api-url", &env.api_url])
            .assert()
            .code(3)
            .stderr(predicate::str::contains("API request failed"));

        mock.assert();
    }

    #[test]
    fn api_client_error_no_retry_exits_3() {
        let mut server = mockito::Server::new();
        let error_body =
            r#"{"error": {"code": "INVALID_REQUEST", "message": "bad input", "details": null}}"#;
        let mock = server
            .mock("POST", "/adrs/match")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(error_body)
            .expect(1)
            .create();

        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);
        env.cmd()
            .args(["sync", "--force", "--no-tailor", "--api-url", &env.api_url])
            .assert()
            .code(3)
            .stderr(predicate::str::contains("INVALID_REQUEST"))
            .stderr(predicate::str::contains("bad input"));

        mock.assert();
    }

    #[test]
    fn api_unreachable_server_exits_3() {
        // We still need a mockito server for TestEnv construction, but we
        // override --api-url to an address where nothing is listening.
        let server = mockito::Server::new();
        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);
        env.cmd()
            .args([
                "sync",
                "--force",
                "--no-tailor",
                "--api-url",
                "http://127.0.0.1:1",
            ])
            .assert()
            .code(3)
            .stderr(predicate::str::contains("API request failed"));
    }

    // ── Auth error tests ────────────────────────────────────────────────

    #[test]
    fn auth_not_authenticated_exits_2() {
        let server = mockito::Server::new();
        let env = TestEnv::new(&server, AUTH_FAIL, ANALYSIS_SINGLE_PROJECT);
        env.cmd()
            .args(["sync", "--force", "--no-tailor", "--api-url", &env.api_url])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("not authenticated"));
    }

    #[test]
    fn auth_binary_crash_exits_1() {
        let server = mockito::Server::new();
        let dir = tempfile::tempdir().unwrap();
        let binary_path = create_auth_crash_binary(dir.path());
        let config_path = dir.path().join("config.yaml");
        let env = TestEnv {
            dir,
            config_path,
            binary_path,
            api_url: server.url(),
        };

        env.cmd()
            .args(["sync", "--force", "--no-tailor", "--api-url", &env.api_url])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("subprocess failed"));
    }

    // ── Analysis error tests ────────────────────────────────────────────

    #[test]
    fn analysis_malformed_workspace_exits_1() {
        let server = mockito::Server::new();
        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);
        // Create a malformed pnpm-workspace.yaml to trigger a static analysis error
        env.write_file("pnpm-workspace.yaml", "{{invalid yaml content");
        env.cmd()
            .args(["sync", "--force", "--no-tailor", "--api-url", &env.api_url])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("Monorepo detection failed"));
    }

    // ── Tailoring error tests ───────────────────────────────────────────

    #[test]
    fn tailoring_subprocess_crash_exits_1() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json("adr-001", "Test ADR", &["policy"], &[], &["."]);
        let response = make_match_response_json(&format!("[{adr}]"));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let dir = tempfile::tempdir().unwrap();
        let binary_path =
            create_tailoring_crash_binary(dir.path(), AUTH_OK, ANALYSIS_SINGLE_PROJECT);
        let config_path = dir.path().join("config.yaml");
        let env = TestEnv {
            dir,
            config_path,
            binary_path,
            api_url: server.url(),
        };

        env.cmd()
            .args(["sync", "--force", "--api-url", &env.api_url])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("subprocess failed"));
    }

    #[test]
    fn tailoring_invalid_json_exits_1() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json("adr-001", "Test ADR", &["policy"], &[], &["."]);
        let response = make_match_response_json(&format!("[{adr}]"));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let dir = tempfile::tempdir().unwrap();
        let binary_path =
            create_tailoring_invalid_json_binary(dir.path(), AUTH_OK, ANALYSIS_SINGLE_PROJECT);
        let config_path = dir.path().join("config.yaml");
        let env = TestEnv {
            dir,
            config_path,
            binary_path,
            api_url: server.url(),
        };

        env.cmd()
            .args(["sync", "--force", "--api-url", &env.api_url])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("Failed to parse"));
    }
}
