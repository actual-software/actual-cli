#[cfg(unix)]
mod common;

#[cfg(unix)]
mod tests {
    use super::common::*;

    /// Helper: build a tailoring JSON response with given files, skipped ADRs, and summary.
    fn make_tailoring_json(
        files: &[(&str, &str, &str, &[&str])], // (path, content, reasoning, adr_ids)
        skipped: &[(&str, &str)],              // (id, reason)
    ) -> String {
        let file_objects: Vec<serde_json::Value> = files
            .iter()
            .map(|(path, content, reasoning, adr_ids)| {
                serde_json::json!({
                    "path": path,
                    "content": content,
                    "reasoning": reasoning,
                    "adr_ids": adr_ids,
                })
            })
            .collect();

        let skipped_objects: Vec<serde_json::Value> = skipped
            .iter()
            .map(|(id, reason)| {
                serde_json::json!({
                    "id": id,
                    "reason": reason,
                })
            })
            .collect();

        let applicable = files.iter().map(|(_, _, _, ids)| ids.len()).sum::<usize>();
        let not_applicable = skipped.len();
        let total_input = applicable + not_applicable;
        let files_generated = files.len();

        serde_json::json!({
            "files": file_objects,
            "skipped_adrs": skipped_objects,
            "summary": {
                "total_input": total_input,
                "applicable": applicable,
                "not_applicable": not_applicable,
                "files_generated": files_generated,
            },
        })
        .to_string()
    }

    /// Helper: build a tailoring JSON response with content containing special characters.
    ///
    /// Verifies that `make_tailoring_json` correctly handles strings with double quotes,
    /// backslashes, and other JSON-special characters — something the old `format!`-based
    /// implementation could not do.
    #[test]
    fn make_tailoring_json_handles_special_characters() {
        let json_str = make_tailoring_json(
            &[(
                "CLAUDE.md",
                r#"Use "quotes" and backslash: C:\path\to\file"#,
                r#"ADR says: use "double quotes" for strings"#,
                &["adr-001"],
            )],
            &[("adr-002", r#"Not applicable: contains "special" chars"#)],
        );

        // Must parse as valid JSON
        let parsed: serde_json::Value =
            serde_json::from_str(&json_str).expect("output must be valid JSON");

        // File content should round-trip correctly
        let content = parsed["files"][0]["content"].as_str().unwrap();
        assert!(
            content.contains("\"quotes\""),
            "double quotes should be preserved"
        );
        assert!(
            content.contains(r"C:\path\to\file"),
            "backslashes should be preserved"
        );

        // Skipped reason should also round-trip
        let reason = parsed["skipped_adrs"][0]["reason"].as_str().unwrap();
        assert!(
            reason.contains("\"special\""),
            "double quotes in skipped reason should be preserved"
        );
    }

    #[test]
    fn single_project_tailoring_creates_root_claude_md() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json(
            "adr-001",
            "Error Handling",
            &["Use Result<T, E> for all fallible operations"],
            &[],
            &["."],
        );
        let response = make_match_response_json(&format!("[{}]", adr));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let tailoring_json = make_tailoring_json(
            &[(
                "CLAUDE.md",
                "# Tailored Rules for Rust\\n\\n- Use Result everywhere",
                "Applied error handling ADR",
                &["adr-001"],
            )],
            &[],
        );

        let env =
            TestEnv::new_with_tailoring(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT, &tailoring_json);
        env.cmd()
            .args(["sync", "--force", "--api-url", &env.api_url])
            .assert()
            .success();

        assert!(env.file_exists("CLAUDE.md"));
        let content = env.read_file("CLAUDE.md");
        // Verify tailored content (not raw ADR policies)
        assert!(content.contains("Tailored Rules for Rust"));
        assert!(content.contains("Use Result everywhere"));
        // Root file should have header
        assert!(content.contains("# Project Guidelines"));
        // Managed markers
        assert!(content.contains("<!-- managed:actual-start -->"));
        assert!(content.contains("<!-- managed:actual-end -->"));
        // Metadata
        assert!(content.contains("<!-- adr-ids: adr-001 -->"));
        assert!(content.contains("<!-- version: 1 -->"));
    }

    #[test]
    fn monorepo_tailoring_creates_multiple_claude_md_files() {
        let mut server = mockito::Server::new();
        let adr1 = make_adr_json(
            "adr-001",
            "Root Standards",
            &["Use consistent error handling"],
            &[],
            &["."],
        );
        let adr2 = make_adr_json(
            "adr-002",
            "Web Standards",
            &["Use React Server Components"],
            &[],
            &["apps/web"],
        );
        let response = make_match_response_json(&format!("[{},{}]", adr1, adr2));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let tailoring_json = make_tailoring_json(
            &[
                (
                    "CLAUDE.md",
                    "# Root tailored content",
                    "Root-level rules",
                    &["adr-001"],
                ),
                (
                    "apps/web/CLAUDE.md",
                    "# Web tailored content",
                    "Web-specific rules",
                    &["adr-002"],
                ),
            ],
            &[],
        );

        let env = TestEnv::new_with_tailoring(&server, AUTH_OK, ANALYSIS_MONOREPO, &tailoring_json);
        env.setup_monorepo();
        env.cmd()
            .args(["sync", "--force", "--api-url", &env.api_url])
            .assert()
            .success();

        // Root CLAUDE.md
        assert!(env.file_exists("CLAUDE.md"));
        let root = env.read_file("CLAUDE.md");
        assert!(root.contains("Root tailored content"));
        assert!(root.contains("# Project Guidelines"));

        // Subdirectory CLAUDE.md
        assert!(env.file_exists("apps/web/CLAUDE.md"));
        let web = env.read_file("apps/web/CLAUDE.md");
        assert!(web.contains("Web tailored content"));
        // Subdirectory files do NOT get the "# Project Guidelines" header
        assert!(!web.contains("# Project Guidelines"));
    }

    #[test]
    fn tailoring_with_skipped_adrs() {
        let mut server = mockito::Server::new();
        let adr1 = make_adr_json(
            "adr-001",
            "Error Handling",
            &["Use Result<T, E>"],
            &[],
            &["."],
        );
        let adr2 = make_adr_json(
            "adr-002",
            "Mobile Guidelines",
            &["Use SwiftUI"],
            &[],
            &["."],
        );
        let response = make_match_response_json(&format!("[{},{}]", adr1, adr2));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let tailoring_json = make_tailoring_json(
            &[(
                "CLAUDE.md",
                "# Error handling rules",
                "Applied error handling",
                &["adr-001"],
            )],
            &[("adr-002", "Not applicable to this project")],
        );

        let env =
            TestEnv::new_with_tailoring(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT, &tailoring_json);
        env.cmd()
            .args(["sync", "--force", "--api-url", &env.api_url])
            .assert()
            .success();

        assert!(env.file_exists("CLAUDE.md"));
        let content = env.read_file("CLAUDE.md");
        // Only adr-001 should appear in the adr-ids
        assert!(content.contains("adr-001"));
        assert!(!content.contains("adr-002"));
    }

    #[test]
    fn model_passthrough_in_tailoring() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json("adr-001", "Test ADR", &["policy"], &[], &["."]);
        let response = make_match_response_json(&format!("[{}]", adr));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let tailoring_json = make_tailoring_json(
            &[("CLAUDE.md", "# Tailored content", "Tailored", &["adr-001"])],
            &[],
        );

        let dir = tempfile::tempdir().unwrap();
        let capture_file = dir.path().join("captured_args.txt");
        let binary_path = create_fake_claude_binary_capturing_with_tailoring(
            dir.path(),
            AUTH_OK,
            ANALYSIS_SINGLE_PROJECT,
            &tailoring_json,
            &capture_file,
        );
        let config_path = dir.path().join("config.yaml");
        let env = TestEnv {
            dir,
            config_path,
            binary_path,
            api_url: server.url(),
        };

        env.cmd()
            .args([
                "sync",
                "--force",
                "--model",
                "opus",
                "--api-url",
                &env.api_url,
            ])
            .assert()
            .success();

        let captured = env.read_file("captured_args.txt");
        let invocations = parse_captured_invocations(&captured);
        assert!(
            !invocations.is_empty(),
            "expected at least 1 invocation (tailoring), got 0",
        );
        // Find the tailoring invocation (has a --json-schema arg containing skipped_adrs)
        let tailoring_inv = invocations
            .iter()
            .find(|args| args.iter().any(|a| a.contains("skipped_adrs")))
            .expect("expected a tailoring invocation containing skipped_adrs");
        assert!(
            tailoring_inv.contains(&"--model".to_string()),
            "tailoring invocation should contain --model flag"
        );
        let model_pos = tailoring_inv.iter().position(|a| a == "--model").unwrap();
        assert_eq!(
            tailoring_inv[model_pos + 1],
            "opus",
            "model value should be opus"
        );
    }

    #[test]
    fn max_budget_usd_passthrough_in_tailoring() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json("adr-001", "Test ADR", &["policy"], &[], &["."]);
        let response = make_match_response_json(&format!("[{}]", adr));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let tailoring_json = make_tailoring_json(
            &[("CLAUDE.md", "# Tailored content", "Tailored", &["adr-001"])],
            &[],
        );

        let dir = tempfile::tempdir().unwrap();
        let capture_file = dir.path().join("captured_args.txt");
        let binary_path = create_fake_claude_binary_capturing_with_tailoring(
            dir.path(),
            AUTH_OK,
            ANALYSIS_SINGLE_PROJECT,
            &tailoring_json,
            &capture_file,
        );
        let config_path = dir.path().join("config.yaml");
        let env = TestEnv {
            dir,
            config_path,
            binary_path,
            api_url: server.url(),
        };

        env.cmd()
            .args([
                "sync",
                "--force",
                "--max-budget-usd",
                "0.50",
                "--api-url",
                &env.api_url,
            ])
            .assert()
            .success();

        let captured = env.read_file("captured_args.txt");
        let invocations = parse_captured_invocations(&captured);
        assert!(
            !invocations.is_empty(),
            "expected at least 1 invocation (tailoring), got 0",
        );
        // Find the tailoring invocation
        let tailoring_inv = invocations
            .iter()
            .find(|args| args.iter().any(|a| a.contains("skipped_adrs")))
            .expect("expected a tailoring invocation containing skipped_adrs");
        assert!(
            tailoring_inv.contains(&"--max-budget-usd".to_string()),
            "tailoring invocation should contain --max-budget-usd flag"
        );
        let budget_pos = tailoring_inv
            .iter()
            .position(|a| a == "--max-budget-usd")
            .unwrap();
        assert_eq!(
            tailoring_inv[budget_pos + 1],
            "0.5",
            "max-budget-usd value should be 0.5"
        );
    }

    #[test]
    fn path_traversal_rejected_end_to_end() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json(
            "adr-001",
            "Error Handling",
            &["Use Result<T, E>"],
            &[],
            &["."],
        );
        let response = make_match_response_json(&format!("[{}]", adr));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        // Malicious path with path traversal components
        let tailoring_json = make_tailoring_json(
            &[(
                "../../../tmp/pwned/CLAUDE.md",
                "# Malicious content",
                "Path traversal attempt",
                &["adr-001"],
            )],
            &[],
        );

        let env =
            TestEnv::new_with_tailoring(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT, &tailoring_json);
        env.cmd()
            .args(["sync", "--force", "--api-url", &env.api_url])
            .assert()
            .failure();

        // The traversal target must not have been created
        assert!(
            !std::path::Path::new("../../../tmp/pwned/CLAUDE.md").exists(),
            "relative traversal path must not exist"
        );
        assert!(
            !std::path::Path::new("/tmp/pwned/CLAUDE.md").exists(),
            "/tmp/pwned/CLAUDE.md must not have been created"
        );
    }

    #[test]
    fn absolute_path_rejected_end_to_end() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json(
            "adr-001",
            "Error Handling",
            &["Use Result<T, E>"],
            &[],
            &["."],
        );
        let response = make_match_response_json(&format!("[{}]", adr));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        // Absolute path that does not end in CLAUDE.md — fails is_valid_output_path first
        let tailoring_json = make_tailoring_json(
            &[(
                "/etc/malicious",
                "# Malicious content",
                "Absolute path attempt",
                &["adr-001"],
            )],
            &[],
        );

        let env =
            TestEnv::new_with_tailoring(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT, &tailoring_json);
        env.cmd()
            .args(["sync", "--force", "--api-url", &env.api_url])
            .assert()
            .failure();

        // The absolute target must not have been created
        assert!(
            !std::path::Path::new("/etc/malicious").exists(),
            "/etc/malicious must not exist"
        );
    }

    #[test]
    fn absolute_path_ending_in_claude_md_rejected_end_to_end() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json(
            "adr-001",
            "Error Handling",
            &["Use Result<T, E>"],
            &[],
            &["."],
        );
        let response = make_match_response_json(&format!("[{}]", adr));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        // Absolute path ending in CLAUDE.md — passes is_valid_output_path and ParentDir check,
        // but must be rejected by RootDir/Prefix component check in validate_and_filter_output.
        let tailoring_json = make_tailoring_json(
            &[(
                "/tmp/pwned/CLAUDE.md",
                "# Malicious content",
                "Absolute path with valid filename",
                &["adr-001"],
            )],
            &[],
        );

        // Ensure the target does not pre-exist
        let _ = std::fs::remove_file("/tmp/pwned/CLAUDE.md");

        let env =
            TestEnv::new_with_tailoring(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT, &tailoring_json);
        env.cmd()
            .args(["sync", "--force", "--api-url", &env.api_url])
            .assert()
            .failure();

        assert!(
            !std::path::Path::new("/tmp/pwned/CLAUDE.md").exists(),
            "/tmp/pwned/CLAUDE.md must not have been created"
        );
    }

    #[test]
    fn tailoring_not_misfired_when_adr_contains_skipped_adrs_string() {
        // Regression test: ADR policy text contains "skipped_adrs" as a substring.
        // With the old grep-based discriminator, the fake binary would mis-route
        // the analysis invocation to the tailoring branch.
        // This test verifies the fix (--json-schema flag check) works correctly.
        let mut server = mockito::Server::new();
        let adr = make_adr_json(
            "adr-001",
            "CI Pipeline",
            &["Do not reference skipped_adrs fields in external scripts"],
            &[],
            &["."],
        );
        let response = make_match_response_json(&format!("[{}]", adr));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let tailoring_json = make_tailoring_json(
            &[("CLAUDE.md", "# CI Rules", "Applied CI ADR", &["adr-001"])],
            &[],
        );

        let env =
            TestEnv::new_with_tailoring(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT, &tailoring_json);

        env.cmd()
            .args(["sync", "--force", "--api-url", &env.api_url])
            .assert()
            .success();

        // Tailoring should have written CLAUDE.md correctly
        assert!(
            env.file_exists("CLAUDE.md"),
            "CLAUDE.md should be created by tailoring"
        );
        let content = env.read_file("CLAUDE.md");
        assert!(
            content.contains("CI Rules"),
            "Expected tailored content in CLAUDE.md"
        );
    }

    #[test]
    fn tailored_content_replaces_existing_managed_section() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json(
            "adr-001",
            "Error Handling",
            &["Use Result<T, E>"],
            &[],
            &["."],
        );
        let response = make_match_response_json(&format!("[{}]", adr));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let tailoring_json = make_tailoring_json(
            &[(
                "CLAUDE.md",
                "# Updated tailored rules",
                "Updated rules",
                &["adr-001"],
            )],
            &[],
        );

        let env =
            TestEnv::new_with_tailoring(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT, &tailoring_json);

        // Pre-create a CLAUDE.md with version 1 managed section and user content
        let initial_content = "# Project Guidelines\n\n\
             <!-- managed:actual-start -->\n\
             <!-- last-synced: 2025-01-01T00:00:00Z -->\n\
             <!-- version: 1 -->\n\
             <!-- adr-ids: adr-001 -->\n\n\
             # Old tailored rules\n\n\
             <!-- managed:actual-end -->\n\n\
             ## My Custom Notes\n\n\
             These are user-written notes that should be preserved.";
        env.write_file("CLAUDE.md", initial_content);

        env.cmd()
            .args(["sync", "--force", "--api-url", &env.api_url])
            .assert()
            .success();

        let content = env.read_file("CLAUDE.md");
        // New tailored content should be present
        assert!(content.contains("Updated tailored rules"));
        // Old managed content should be gone
        assert!(!content.contains("Old tailored rules"));
        // Version should be incremented to 2
        assert!(content.contains("<!-- version: 2 -->"));
        // User content outside managed section should be preserved
        assert!(content.contains("My Custom Notes"));
        assert!(content.contains("These are user-written notes that should be preserved."));
        // Project Guidelines header should remain
        assert!(content.contains("# Project Guidelines"));
    }
}
