#[cfg(unix)]
mod common;

#[cfg(unix)]
mod tests {
    use super::common::*;
    use predicates::prelude::*;

    // ── Dry-run mode ────────────────────────────────────────────────────

    #[test]
    fn dry_run_writes_nothing() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json(
            "adr-001",
            "Use Error Types",
            &["Always use thiserror for errors"],
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
                "--dry-run",
                "--api-url",
                &env.api_url,
            ])
            .assert()
            .success();

        assert!(
            !env.file_exists("CLAUDE.md"),
            "dry-run should NOT create CLAUDE.md on disk"
        );
    }

    #[test]
    fn dry_run_full_shows_content() {
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
                "--dry-run",
                "--full",
                "--api-url",
                &env.api_url,
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("## Use Error Types"))
            .stdout(predicate::str::contains(
                "- Always use thiserror for errors",
            ))
            .stdout(predicate::str::contains(
                "- Never use unwrap in production code",
            ));

        assert!(
            !env.file_exists("CLAUDE.md"),
            "dry-run --full should NOT create CLAUDE.md on disk"
        );
    }

    #[test]
    fn dry_run_shows_diff_summary() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json("adr-001", "Test Rule", &["policy1"], &[], &["."]);
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
                "--dry-run",
                "--api-url",
                &env.api_url,
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("new file"));
    }

    // ── Re-sync (updating existing CLAUDE.md) ───────────────────────────

    #[test]
    fn user_content_preserved_on_resync() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json(
            "adr-002",
            "New Rule",
            &["New policy from second sync"],
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

        // Pre-populate CLAUDE.md with user content surrounding a managed section
        let existing = "\
# My Custom Header

User notes

<!-- managed:actual-start -->
<!-- last-synced: 2025-01-01T00:00:00Z -->
<!-- version: 1 -->
<!-- adr-ids: adr-001 -->

Old content

<!-- managed:actual-end -->

User footer";
        env.write_file("CLAUDE.md", existing);

        env.cmd()
            .args(["sync", "--force", "--no-tailor", "--api-url", &env.api_url])
            .assert()
            .success();

        let content = env.read_file("CLAUDE.md");

        // User content before and after managed section is preserved
        assert!(
            content.contains("# My Custom Header"),
            "expected user header preserved, got:\n{content}"
        );
        assert!(
            content.contains("User notes"),
            "expected user notes preserved, got:\n{content}"
        );
        assert!(
            content.contains("User footer"),
            "expected user footer preserved, got:\n{content}"
        );

        // Managed section replaced with new content
        assert!(
            !content.contains("Old content"),
            "expected old managed content to be replaced, got:\n{content}"
        );
        assert!(
            content.contains("## New Rule"),
            "expected new ADR title in managed section, got:\n{content}"
        );
        assert!(
            content.contains("- New policy from second sync"),
            "expected new policy in managed section, got:\n{content}"
        );

        // Version incremented
        assert!(
            content.contains("<!-- version: 2 -->"),
            "expected version 2 after re-sync, got:\n{content}"
        );

        // ADR IDs updated
        assert!(
            content.contains("<!-- adr-ids: adr-002 -->"),
            "expected adr-ids updated to adr-002, got:\n{content}"
        );
    }

    #[test]
    fn resync_is_idempotent() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json(
            "adr-001",
            "Stable Rule",
            &["Consistent policy"],
            &[],
            &["."],
        );
        let response = make_match_response_json(&format!("[{}]", adr));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .expect(2)
            .create();

        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);

        // Pre-populate with user content and a managed section
        let existing = "\
# My Custom Header

User notes

<!-- managed:actual-start -->
<!-- last-synced: 2025-01-01T00:00:00Z -->
<!-- version: 1 -->
<!-- adr-ids: adr-001 -->

Old content

<!-- managed:actual-end -->

User footer";
        env.write_file("CLAUDE.md", existing);

        // First sync
        env.cmd()
            .args(["sync", "--force", "--no-tailor", "--api-url", &env.api_url])
            .assert()
            .success();

        // Second sync
        env.cmd()
            .args(["sync", "--force", "--no-tailor", "--api-url", &env.api_url])
            .assert()
            .success();

        let content = env.read_file("CLAUDE.md");

        // User content still intact after two syncs
        assert!(
            content.contains("# My Custom Header"),
            "expected user header after 2 syncs, got:\n{content}"
        );
        assert!(
            content.contains("User notes"),
            "expected user notes after 2 syncs, got:\n{content}"
        );
        assert!(
            content.contains("User footer"),
            "expected user footer after 2 syncs, got:\n{content}"
        );

        // Managed content is present
        assert!(
            content.contains("## Stable Rule"),
            "expected ADR title after 2 syncs, got:\n{content}"
        );
        assert!(
            content.contains("- Consistent policy"),
            "expected policy after 2 syncs, got:\n{content}"
        );
    }

    // ── Rejection memory ────────────────────────────────────────────────

    #[test]
    fn rejected_adrs_filtered() {
        let mut server = mockito::Server::new();
        let adr1 = make_adr_json(
            "adr-001",
            "Rejected Rule",
            &["Should be filtered"],
            &[],
            &["."],
        );
        let adr2 = make_adr_json("adr-002", "Accepted Rule", &["Should appear"], &[], &["."]);
        let response = make_match_response_json(&format!("[{},{}]", adr1, adr2));
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);

        // Compute the repo key the same way compute_repo_key does for non-git dirs:
        // SHA-256 of the canonicalized tempdir path string.
        // On macOS, std::env::current_dir() resolves symlinks (e.g. /var -> /private/var),
        // so we must canonicalize the path to match what the binary sees.
        use sha2::{Digest, Sha256};
        let canonical_path = env.dir.path().canonicalize().unwrap();
        let mut hasher = Sha256::new();
        hasher.update(canonical_path.to_string_lossy().as_bytes());
        let repo_key = format!("{:x}", hasher.finalize());

        // Pre-populate config with rejection for adr-001
        let config_yaml = format!("rejected_adrs:\n  {}:\n  - adr-001\n", repo_key);
        std::fs::write(&env.config_path, &config_yaml).unwrap();

        env.cmd()
            .args(["sync", "--force", "--no-tailor", "--api-url", &env.api_url])
            .assert()
            .success();

        let content = env.read_file("CLAUDE.md");

        // adr-001 should be filtered out
        assert!(
            !content.contains("Rejected Rule"),
            "expected adr-001 to be filtered by rejection memory, got:\n{content}"
        );
        assert!(
            !content.contains("Should be filtered"),
            "expected adr-001 policy to be filtered, got:\n{content}"
        );

        // adr-002 should be present
        assert!(
            content.contains("## Accepted Rule"),
            "expected adr-002 title in output, got:\n{content}"
        );
        assert!(
            content.contains("- Should appear"),
            "expected adr-002 policy in output, got:\n{content}"
        );
    }

    #[test]
    fn reset_rejections_clears_memory() {
        let mut server = mockito::Server::new();
        let adr1 = make_adr_json(
            "adr-001",
            "Previously Rejected",
            &["Now included"],
            &[],
            &["."],
        );
        let adr2 = make_adr_json(
            "adr-002",
            "Always Accepted",
            &["Always present"],
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

        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);

        // Compute repo key (canonicalize to match what the binary sees via current_dir)
        use sha2::{Digest, Sha256};
        let canonical_path = env.dir.path().canonicalize().unwrap();
        let mut hasher = Sha256::new();
        hasher.update(canonical_path.to_string_lossy().as_bytes());
        let repo_key = format!("{:x}", hasher.finalize());

        // Pre-populate config with rejection for adr-001
        let config_yaml = format!("rejected_adrs:\n  {}:\n  - adr-001\n", repo_key);
        std::fs::write(&env.config_path, &config_yaml).unwrap();

        // Sync with --reset-rejections
        env.cmd()
            .args([
                "sync",
                "--force",
                "--no-tailor",
                "--reset-rejections",
                "--api-url",
                &env.api_url,
            ])
            .assert()
            .success();

        let content = env.read_file("CLAUDE.md");

        // Both ADRs should be present (rejections cleared)
        assert!(
            content.contains("## Previously Rejected"),
            "expected adr-001 after reset, got:\n{content}"
        );
        assert!(
            content.contains("- Now included"),
            "expected adr-001 policy after reset, got:\n{content}"
        );
        assert!(
            content.contains("## Always Accepted"),
            "expected adr-002 after reset, got:\n{content}"
        );
        assert!(
            content.contains("- Always present"),
            "expected adr-002 policy after reset, got:\n{content}"
        );

        // Config should no longer have rejected_adrs for this repo
        let config_content = std::fs::read_to_string(&env.config_path).unwrap();
        assert!(
            !config_content.contains("rejected_adrs"),
            "expected rejected_adrs cleared from config, got:\n{config_content}"
        );
    }

    // ── Edge cases ──────────────────────────────────────────────────────

    #[test]
    fn zero_adrs_shows_no_files() {
        let mut server = mockito::Server::new();
        let response = make_match_response_json("[]");
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create();

        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);
        env.cmd()
            .args(["sync", "--force", "--no-tailor", "--api-url", &env.api_url])
            .assert()
            .success()
            .stdout(predicate::str::contains("No files to write."));

        assert!(
            !env.file_exists("CLAUDE.md"),
            "zero ADRs should not create CLAUDE.md"
        );
    }

    #[test]
    fn non_git_directory_sync_still_succeeds() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json(
            "adr-001",
            "Non-Git Rule",
            &["Works without git"],
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

        // TestEnv creates a tempdir with no .git — confirm it's not a git repo
        assert!(
            !env.file_exists(".git"),
            "expected tempdir to NOT be a git repo"
        );

        // Spinner messages (like "Not a git repository") are written via indicatif
        // which may not appear in captured stderr (non-TTY). We verify the
        // functional behavior: sync succeeds and produces correct output.
        env.cmd()
            .args(["sync", "--force", "--no-tailor", "--api-url", &env.api_url])
            .assert()
            .success();

        // Sync should still succeed and create CLAUDE.md
        assert!(
            env.file_exists("CLAUDE.md"),
            "expected CLAUDE.md created even without git"
        );
        let content = env.read_file("CLAUDE.md");
        assert!(
            content.contains("## Non-Git Rule"),
            "expected ADR content in non-git sync, got:\n{content}"
        );
    }

    #[test]
    fn project_filter_with_no_tailor() {
        let mut server = mockito::Server::new();
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
                "sync",
                "--force",
                "--no-tailor",
                "--project",
                "apps/web",
                "--api-url",
                &env.api_url,
            ])
            .assert()
            .success();

        // Only apps/web should have a CLAUDE.md
        assert!(
            env.file_exists("apps/web/CLAUDE.md"),
            "expected apps/web/CLAUDE.md"
        );
        let web = env.read_file("apps/web/CLAUDE.md");
        assert!(
            web.contains("## Web Only Rule"),
            "expected web rule in apps/web/CLAUDE.md, got:\n{web}"
        );
        assert!(
            web.contains("- Use Next.js app router"),
            "expected web policy in apps/web/CLAUDE.md, got:\n{web}"
        );

        // No root or other project CLAUDE.md
        assert!(
            !env.file_exists("CLAUDE.md"),
            "expected no root CLAUDE.md with --project filter"
        );
        assert!(
            !env.file_exists("apps/api/CLAUDE.md"),
            "expected no apps/api/CLAUDE.md with --project filter"
        );
    }
}
