#[cfg(unix)]
mod common;

#[cfg(unix)]
mod tests {
    use super::common::*;
    use predicates::prelude::*;

    #[test]
    fn single_project_single_adr_creates_root_claude_md() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json(
            "adr-001",
            "Use Error Types",
            &[
                "Always use thiserror for errors",
                "Never use unwrap in production code",
            ],
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

        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);
        env.cmd()
            .args([
                "sync",
                "--force",
                "--no-tailor",
                "--runner",
                "claude-cli",
                "--api-url",
                &env.api_url,
            ])
            .assert()
            .success();

        assert!(env.file_exists("CLAUDE.md"));
        let content = env.read_file("CLAUDE.md");
        // Root file should have header
        assert!(content.contains("# Project Guidelines"));
        // Managed markers
        assert!(content.contains("<!-- managed:actual-start -->"));
        assert!(content.contains("<!-- managed:actual-end -->"));
        // ADR title as heading
        assert!(content.contains("## Use Error Types"));
        // Policies as bullet points
        assert!(content.contains("- Always use thiserror for errors"));
        assert!(content.contains("- Never use unwrap in production code"));
        // Metadata comments
        assert!(content.contains("<!-- adr-ids: adr-001 -->"));
        assert!(content.contains("<!-- version: 1 -->"));
    }

    #[test]
    fn single_project_multiple_adrs_all_in_root_claude_md() {
        let mut server = mockito::Server::new();
        let adr1 = make_adr_json("adr-001", "Error Handling", &["Use thiserror"], &[], &["."]);
        let adr2 = make_adr_json(
            "adr-002",
            "Testing Policy",
            &["Write unit tests"],
            &[],
            &["."],
        );
        let adr3 = make_adr_json(
            "adr-003",
            "Logging Standards",
            &["Use tracing crate"],
            &[],
            &["."],
        );
        let response = make_match_response_json(&format!("[{},{},{}]", adr1, adr2, adr3));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);
        env.cmd()
            .args([
                "sync",
                "--force",
                "--no-tailor",
                "--runner",
                "claude-cli",
                "--api-url",
                &env.api_url,
            ])
            .assert()
            .success();

        let content = env.read_file("CLAUDE.md");
        // All 3 ADR titles
        assert!(content.contains("## Error Handling"));
        assert!(content.contains("## Testing Policy"));
        assert!(content.contains("## Logging Standards"));
        // All 3 IDs in adr-ids comment
        assert!(content.contains("adr-001"));
        assert!(content.contains("adr-002"));
        assert!(content.contains("adr-003"));
        assert!(content.contains("<!-- adr-ids:"));
    }

    #[test]
    fn monorepo_creates_multiple_claude_md_files() {
        let mut server = mockito::Server::new();
        let adr_web = make_adr_json(
            "adr-web",
            "Web Standards",
            &["Use React Server Components"],
            &[],
            &["apps/web"],
        );
        let adr_api = make_adr_json(
            "adr-api",
            "API Guidelines",
            &["Use axum for HTTP"],
            &[],
            &["apps/api"],
        );
        let adr_shared = make_adr_json(
            "adr-shared",
            "Shared Code Rules",
            &["Keep dependencies minimal"],
            &[],
            &["apps/web", "apps/api"],
        );
        let adr_general = make_adr_json(
            "adr-general",
            "General Practices",
            &["Document all public APIs"],
            &[],
            &[],
        );
        let response = make_match_response_json(&format!(
            "[{},{},{},{}]",
            adr_web, adr_api, adr_shared, adr_general
        ));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_MONOREPO);
        env.setup_monorepo();
        env.cmd()
            .args([
                "sync",
                "--force",
                "--no-tailor",
                "--runner",
                "claude-cli",
                "--api-url",
                &env.api_url,
            ])
            .assert()
            .success();

        // Root CLAUDE.md should contain adr-general (no matched_projects -> root)
        assert!(env.file_exists("CLAUDE.md"));
        let root = env.read_file("CLAUDE.md");
        assert!(root.contains("## General Practices"));
        assert!(root.contains("- Document all public APIs"));

        // apps/web/CLAUDE.md should have adr-web AND adr-shared
        assert!(env.file_exists("apps/web/CLAUDE.md"));
        let web = env.read_file("apps/web/CLAUDE.md");
        assert!(web.contains("## Web Standards"));
        assert!(web.contains("- Use React Server Components"));
        assert!(web.contains("## Shared Code Rules"));
        assert!(web.contains("- Keep dependencies minimal"));

        // apps/api/CLAUDE.md should have adr-api AND adr-shared
        assert!(env.file_exists("apps/api/CLAUDE.md"));
        let api = env.read_file("apps/api/CLAUDE.md");
        assert!(api.contains("## API Guidelines"));
        assert!(api.contains("- Use axum for HTTP"));
        assert!(api.contains("## Shared Code Rules"));

        // libs/shared should NOT have a CLAUDE.md (no ADRs matched to it)
        assert!(!env.file_exists("libs/shared/CLAUDE.md"));
    }

    #[test]
    fn adr_with_policies_and_instructions() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json(
            "adr-mixed",
            "Code Review Policy",
            &[
                "Policy A: All PRs need review",
                "Policy B: Use conventional commits",
            ],
            &["Instruction X: Run clippy before pushing"],
            &["."],
        );
        let response = make_match_response_json(&format!("[{}]", adr));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);
        env.cmd()
            .args([
                "sync",
                "--force",
                "--no-tailor",
                "--runner",
                "claude-cli",
                "--api-url",
                &env.api_url,
            ])
            .assert()
            .success();

        let content = env.read_file("CLAUDE.md");
        assert!(content.contains("- Policy A: All PRs need review"));
        assert!(content.contains("- Policy B: Use conventional commits"));
        assert!(content.contains("- Instruction X: Run clippy before pushing"));
    }

    #[test]
    fn verbose_shows_api_details_on_stderr() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json("adr-001", "Test ADR", &["policy"], &[], &["."]);
        let response = make_match_response_json(&format!("[{}]", adr));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);
        // Write a config with recognizable fake API key values to verify they
        // are not leaked into verbose output.
        std::fs::write(
            &env.config_path,
            "anthropic_api_key: sk-ant-test12345\nopenai_api_key: sk-test-openai-67890\n",
        )
        .unwrap();
        env.cmd()
            .args([
                "adr-bot",
                "--force",
                "--no-tailor",
                "--verbose",
                "--runner",
                "claude-cli",
                "--api-url",
                &env.api_url,
            ])
            .env("ANTHROPIC_API_KEY", "sk-ant-test12345")
            .env("OPENAI_API_KEY", "sk-test-openai-67890")
            .assert()
            .success()
            // Existing positive assertions (keep these):
            .stderr(predicate::str::contains("API request to:"))
            .stderr(predicate::str::contains("projects:"))
            .stderr(predicate::str::contains("matched:"))
            // Credential values must not appear in stderr
            .stderr(predicate::str::contains("sk-ant-test12345").not())
            .stderr(predicate::str::contains("sk-test-openai-67890").not())
            // Credential values must not appear in stdout
            .stdout(predicate::str::contains("sk-ant-test12345").not())
            .stdout(predicate::str::contains("sk-test-openai-67890").not());
    }

    #[test]
    fn verbose_does_not_leak_api_keys_in_output() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json("adr-001", "Test ADR", &["policy"], &[], &["."]);
        let response = make_match_response_json(&format!("[{}]", adr));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);
        // Write a config with recognizable fake API key values to ensure they
        // never appear in any output, even in verbose mode.
        std::fs::write(
            &env.config_path,
            "anthropic_api_key: sk-ant-test12345\nopenai_api_key: sk-test-openai-67890\n",
        )
        .unwrap();
        env.cmd()
            .args([
                "adr-bot",
                "--force",
                "--no-tailor",
                "--verbose",
                "--runner",
                "claude-cli",
                "--api-url",
                &env.api_url,
            ])
            .env("ANTHROPIC_API_KEY", "sk-ant-test12345")
            .env("OPENAI_API_KEY", "sk-test-openai-67890")
            .assert()
            .success()
            // API key values must never appear in stderr
            .stderr(predicate::str::contains("sk-ant-test12345").not())
            .stderr(predicate::str::contains("sk-test-openai-67890").not())
            // API key values must never appear in stdout
            .stdout(predicate::str::contains("sk-ant-test12345").not())
            .stdout(predicate::str::contains("sk-test-openai-67890").not());
    }

    #[test]
    fn project_filter_narrows_scope() {
        let mut server = mockito::Server::new();
        // When --project apps/web is used, only apps/web is sent to the API
        // So the mock response would only contain web-relevant ADRs
        let adr_web = make_adr_json(
            "adr-web",
            "Web Only Rule",
            &["Use Next.js app router"],
            &[],
            &["apps/web"],
        );
        let response = make_match_response_json(&format!("[{}]", adr_web));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_MONOREPO);
        env.setup_monorepo();
        env.cmd()
            .args([
                "adr-bot",
                "--force",
                "--no-tailor",
                "--project",
                "apps/web",
                "--runner",
                "claude-cli",
                "--api-url",
                &env.api_url,
            ])
            .assert()
            .success();

        // Only apps/web should have a CLAUDE.md
        assert!(env.file_exists("apps/web/CLAUDE.md"));
        let web = env.read_file("apps/web/CLAUDE.md");
        assert!(web.contains("## Web Only Rule"));
        assert!(web.contains("- Use Next.js app router"));

        // No root CLAUDE.md or other project files
        assert!(!env.file_exists("CLAUDE.md"));
        assert!(!env.file_exists("apps/api/CLAUDE.md"));
    }

    #[test]
    fn subdirectory_claude_md_has_no_project_guidelines_header() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json("adr-sub", "Sub Rule", &["Sub policy"], &[], &["apps/web"]);
        let response = make_match_response_json(&format!("[{}]", adr));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_MONOREPO);
        env.setup_monorepo();
        env.cmd()
            .args([
                "sync",
                "--force",
                "--no-tailor",
                "--runner",
                "claude-cli",
                "--api-url",
                &env.api_url,
            ])
            .assert()
            .success();

        let web = env.read_file("apps/web/CLAUDE.md");
        // Subdirectory files should NOT have the "# Project Guidelines" header
        assert!(!web.contains("# Project Guidelines"));
        // But should still have managed markers
        assert!(web.contains("<!-- managed:actual-start -->"));
        assert!(web.contains("<!-- managed:actual-end -->"));
    }
}
