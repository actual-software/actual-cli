#[cfg(unix)]
mod common;

#[cfg(unix)]
mod tests {
    use super::common::*;
    use std::time::Duration;
    use tui_test::{Key, TuiSession};

    /// Return the path to the compiled `actual` binary.
    fn actual_binary_path() -> String {
        let bin = assert_cmd::cargo::cargo_bin!("actual");
        bin.to_str().unwrap().to_string()
    }

    /// Spawn an `actual adr-bot` TuiSession with the given TestEnv and extra CLI args.
    fn spawn_sync_session(env: &TestEnv, extra_args: &[&str]) -> TuiSession {
        let bin = actual_binary_path();
        let mut cmd = format!(
            "{} sync --force --no-tailor --runner claude-cli --api-url {}",
            bin, env.api_url
        );
        for arg in extra_args {
            cmd.push(' ');
            cmd.push_str(arg);
        }
        TuiSession::new(&cmd)
            .size(120, 40)
            .timeout(Duration::from_secs(30))
            .env("CLAUDE_BINARY", env.binary_path.to_str().unwrap())
            .env("ACTUAL_CONFIG", env.config_path.to_str().unwrap())
            .env("NO_COLOR", "1")
            .workdir(env.dir.path())
            .spawn()
            .expect("Failed to spawn actual adr-bot")
    }

    /// Set up a mockito server with an empty ADR response (zero matched).
    fn setup_empty_adr_server(server: &mut mockito::Server) -> mockito::Mock {
        let empty_response = make_match_response_json("[]");
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&empty_response)
            .create()
    }

    /// Set up a mockito server that returns the given ADRs.
    fn setup_adr_server(server: &mut mockito::Server, adrs_json: &str) -> mockito::Mock {
        let response = make_match_response_json(adrs_json);
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&response)
            .create()
    }

    /// Wait for the TUI to enter review mode (post-sync).
    ///
    /// In review mode, `selected_step` is set and the Steps pane shows a `▶`
    /// prefix on the selected step. This character only appears once
    /// `wait_for_keypress` starts, which is after all sync phases complete.
    fn wait_for_review_mode(session: &mut TuiSession) {
        session
            .wait_for_text("\u{25b6}")
            .expect("Should enter review mode (▶ visible in Steps pane)");
    }

    // ── Category 1: Basic TUI rendering ──────────────────────────────

    #[test]
    fn test_tui_shows_steps_pane() {
        let mut server = mockito::Server::new();
        setup_empty_adr_server(&mut server);
        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);

        let mut session = spawn_sync_session(&env, &[]);

        // The TUI step labels appear in the Steps box (left pane)
        session
            .wait_for_text("Environment")
            .expect("Should show Environment step label");
        session
            .wait_for_text("Analysis")
            .expect("Should show Analysis step label");
        session
            .wait_for_text("Fetch ADRs")
            .expect("Should show Fetch ADRs step label");

        // Wait for sync to finish, then quit
        wait_for_review_mode(&mut session);
        session.send_key(Key::Char('q')).unwrap();
        let exit_code = session.wait_for_exit().expect("Should exit");
        assert_eq!(exit_code, 0);
    }

    #[test]
    fn test_tui_shows_output_pane() {
        let mut server = mockito::Server::new();
        setup_empty_adr_server(&mut server);
        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);

        let mut session = spawn_sync_session(&env, &[]);

        // The TUI renders box titles "Steps" and "Output" in the borders
        session
            .wait_for_text("Steps")
            .expect("Should show Steps box title");
        session
            .wait_for_text("Output")
            .expect("Should show Output box title");

        wait_for_review_mode(&mut session);
        session.send_key(Key::Char('q')).unwrap();
        let exit_code = session.wait_for_exit().expect("Should exit");
        assert_eq!(exit_code, 0);
    }

    #[test]
    fn test_tui_banner_shows_version() {
        let mut server = mockito::Server::new();
        setup_empty_adr_server(&mut server);
        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);

        let mut session = spawn_sync_session(&env, &[]);

        // The banner box title includes the version string
        session
            .wait_for_text("actual v")
            .expect("Should show version in banner title");

        wait_for_review_mode(&mut session);
        session.send_key(Key::Char('q')).unwrap();
        let exit_code = session.wait_for_exit().expect("Should exit");
        assert_eq!(exit_code, 0);
    }

    // ── Category 2: Sync flow (happy path) ──────────────────────────

    #[test]
    fn test_tui_sync_zero_adrs_shows_no_files() {
        let mut server = mockito::Server::new();
        setup_empty_adr_server(&mut server);
        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);

        let mut session = spawn_sync_session(&env, &[]);

        // Wait for review mode (all phases complete)
        wait_for_review_mode(&mut session);

        // In the Steps pane, all step labels should be visible with final status.
        assert!(
            session.screen_contains("Write Files"),
            "Should show Write Files step label"
        );

        session.send_key(Key::Char('q')).unwrap();
        let exit_code = session.wait_for_exit().expect("Should exit");
        assert_eq!(exit_code, 0);
    }

    #[test]
    fn test_tui_sync_with_adrs_writes_file() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json(
            "adr-tui-001",
            "TUI Test Rule",
            &["Always write TUI tests"],
            &[],
            &["."],
        );
        setup_adr_server(&mut server, &format!("[{}]", adr));
        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);

        let mut session = spawn_sync_session(&env, &[]);

        // Wait for review mode (all phases complete, files written)
        wait_for_review_mode(&mut session);

        // The Write step's log should contain the sync summary (visible in Output
        // pane since Write Files is the last/active step and default selected).
        assert!(
            session.screen_contains("Sync complete"),
            "Should show sync complete summary in Write step log"
        );

        session.send_key(Key::Char('q')).unwrap();
        let exit_code = session.wait_for_exit().expect("Should exit");
        assert_eq!(exit_code, 0);

        // Verify the CLAUDE.md file was created with the ADR content
        assert!(env.file_exists("CLAUDE.md"), "CLAUDE.md should be created");
        let content = env.read_file("CLAUDE.md");
        assert!(
            content.contains("TUI Test Rule"),
            "CLAUDE.md should contain ADR title"
        );
    }

    #[test]
    fn test_tui_sync_no_tailor_shows_skipped() {
        let mut server = mockito::Server::new();
        setup_empty_adr_server(&mut server);
        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);

        let mut session = spawn_sync_session(&env, &[]);

        // Wait for review mode
        wait_for_review_mode(&mut session);

        // Navigate to the Tailoring step to see its log content.
        // Default selected_step is the last active step (Write Files, index 4).
        // Press Up to move to Tailoring (index 3).
        session.send_key(Key::Up).unwrap();

        // The Tailoring step's log should show "Tailoring skipped"
        session
            .wait_for_text("Tailoring skipped")
            .expect("Should show tailoring skipped in Tailoring step log");

        session.send_key(Key::Char('q')).unwrap();
        let exit_code = session.wait_for_exit().expect("Should exit");
        assert_eq!(exit_code, 0);
    }

    // ── Category 3: Plain mode ──────────────────────────────────────

    #[test]
    fn test_plain_mode_sync_completes() {
        let mut server = mockito::Server::new();
        setup_empty_adr_server(&mut server);
        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);

        let mut session = spawn_sync_session(&env, &["--no-tui"]);

        // In plain mode with zero ADRs, the last output is "No files to write"
        // (the process exits on its own — no keypress needed).
        session
            .wait_for_text("No files to write")
            .expect("Should show no files message in plain mode");
        let exit_code = session.wait_for_exit().expect("Should exit");
        assert_eq!(exit_code, 0);
    }

    #[test]
    fn test_plain_mode_shows_phases() {
        let mut server = mockito::Server::new();
        setup_empty_adr_server(&mut server);
        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);

        let mut session = spawn_sync_session(&env, &["--no-tui"]);

        // Plain mode shows phase messages with prefix symbols
        session
            .wait_for_text("Analysis complete")
            .expect("Should show analysis complete");

        // The "Fetched 0 ADRs" message appears in plain mode
        session
            .wait_for_text("Fetched")
            .expect("Should show fetched message");

        let exit_code = session.wait_for_exit().expect("Should exit");
        assert_eq!(exit_code, 0);
    }

    // ── Category 4: Error handling ──────────────────────────────────

    #[test]
    fn test_auth_failure_shows_error() {
        let server = mockito::Server::new();
        let env = TestEnv::new(&server, AUTH_FAIL, ANALYSIS_SINGLE_PROJECT);

        // Use --no-tui so the process exits on its own after the error.
        // spawn_sync_session already sets --runner claude-cli so the auth check
        // runs (the default is codex-cli which would fail with NoRunnerAvailable
        // before reaching the auth step).
        let mut session = spawn_sync_session(&env, &["--no-tui"]);

        // Auth failure shows "not authenticated" in the error panel
        session
            .wait_for_text("not authenticated")
            .expect("Should show auth failure message");
        let exit_code = session.wait_for_exit().expect("Should exit");
        assert_eq!(exit_code, 2);
    }

    #[test]
    fn test_api_unreachable_shows_error() {
        let server = mockito::Server::new();
        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);

        // Override API URL to an unreachable address; use --no-tui for clean exit
        let bin = actual_binary_path();
        let cmd = format!(
            "{} adr-bot --force --no-tailor --no-tui --api-url http://127.0.0.1:1",
            bin
        );
        let mut session = TuiSession::new(&cmd)
            .size(120, 40)
            .timeout(Duration::from_secs(30))
            .env("CLAUDE_BINARY", env.binary_path.to_str().unwrap())
            .env("ACTUAL_CONFIG", env.config_path.to_str().unwrap())
            .env("NO_COLOR", "1")
            .workdir(env.dir.path())
            .spawn()
            .expect("Failed to spawn actual adr-bot");

        // Should show API error and exit with code 3
        session
            .wait_for_text("API request failed")
            .expect("Should show API connection error");
        let exit_code = session.wait_for_exit().expect("Should exit");
        assert_eq!(exit_code, 3);
    }

    // ── Category 5: Keyboard interaction ────────────────────────────

    #[test]
    fn test_tui_quit_with_q() {
        let mut server = mockito::Server::new();
        setup_empty_adr_server(&mut server);
        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);

        let mut session = spawn_sync_session(&env, &[]);

        // Wait for review mode
        wait_for_review_mode(&mut session);

        // Press 'q' to quit
        session.send_key(Key::Char('q')).unwrap();
        let exit_code = session.wait_for_exit().expect("Should exit after q");
        assert_eq!(exit_code, 0);
    }

    #[test]
    fn test_tui_quit_with_esc() {
        let mut server = mockito::Server::new();
        setup_empty_adr_server(&mut server);
        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);

        let mut session = spawn_sync_session(&env, &[]);

        // Wait for review mode
        wait_for_review_mode(&mut session);

        // Press Esc to quit
        session.send_key(Key::Escape).unwrap();
        let exit_code = session.wait_for_exit().expect("Should exit after Esc");
        assert_eq!(exit_code, 0);
    }

    #[test]
    fn test_tui_step_navigation() {
        let mut server = mockito::Server::new();
        setup_empty_adr_server(&mut server);
        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);

        let mut session = spawn_sync_session(&env, &[]);

        // Wait for review mode
        wait_for_review_mode(&mut session);

        // Navigate up from Write (index 4) to Fetch (index 2)
        session.send_key(Key::Up).unwrap(); // → Tailoring (3)
        session.send_key(Key::Up).unwrap(); // → Fetch (2)
                                            // Fetch step's log should contain "Fetched"
        session
            .wait_for_text("Fetched")
            .expect("Should show Fetched in Fetch step log");

        // Navigate to Environment step (index 0)
        session.send_key(Key::Up).unwrap(); // → Analysis (1)
        session.send_key(Key::Up).unwrap(); // → Environment (0)
        session
            .wait_for_text("Checking environment")
            .expect("Should show environment check in Environment step log");

        session.send_key(Key::Char('q')).unwrap();
        let exit_code = session.wait_for_exit().expect("Should exit");
        assert_eq!(exit_code, 0);
    }

    // ── Category 6: Capture integration ─────────────────────────────

    #[test]
    fn test_tui_capture_produces_artifacts() {
        use tui_test::CaptureConfig;

        let mut server = mockito::Server::new();
        let adr = make_adr_json(
            "adr-cap-001",
            "Capture Test Rule",
            &["Test captures work"],
            &[],
            &["."],
        );
        setup_adr_server(&mut server, &format!("[{}]", adr));
        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);

        // Use a fixed capture directory so CI can collect the artifacts.
        // The tui-test-report action looks for **/tui-test-captures/timeline.json.
        let capture_dir = std::path::PathBuf::from("tui-test-captures");
        let capture_config = CaptureConfig {
            output_dir: capture_dir.clone(),
            auto_capture_on_wait: true,
            generation_debounce: 1,
            min_interval_ms: 0,
            save_on_drop: true,
            capture_on_drop: true,
        };

        let bin = actual_binary_path();
        let cmd = format!(
            "{} adr-bot --force --no-tailor --api-url {}",
            bin, env.api_url
        );
        let mut session = TuiSession::new(&cmd)
            .size(120, 40)
            .timeout(Duration::from_secs(30))
            .env("CLAUDE_BINARY", env.binary_path.to_str().unwrap())
            .env("ACTUAL_CONFIG", env.config_path.to_str().unwrap())
            .env("NO_COLOR", "1")
            .workdir(env.dir.path())
            .capture_config(capture_config)
            .spawn()
            .expect("Failed to spawn actual adr-bot with capture");

        // Wait for all phases to complete
        wait_for_review_mode(&mut session);

        // Take an explicit screenshot at the final review state
        let screenshot_name = session.capture().expect("Should capture screenshot");
        assert!(
            screenshot_name.ends_with(".png"),
            "Screenshot should be a PNG"
        );

        // Save timeline before quitting
        let timeline_path = session.save_captures().expect("Should save captures");
        assert!(timeline_path.exists(), "timeline.json should exist");

        // Verify timeline.json is valid and has expected structure
        let content = std::fs::read_to_string(&timeline_path).unwrap();
        let timeline: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(
            timeline["command"].as_str().unwrap().contains("adr-bot"),
            "Timeline command should contain 'sync'"
        );
        assert_eq!(timeline["terminal_size"][0], 120);
        assert_eq!(timeline["terminal_size"][1], 40);
        assert!(
            !timeline["entries"].as_array().unwrap().is_empty(),
            "Timeline should have entries"
        );

        // Verify at least one screenshot PNG was created in the capture dir
        let png_count = std::fs::read_dir(&capture_dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "png")
            })
            .count();
        assert!(png_count > 0, "Should have at least one PNG screenshot");

        session.send_key(Key::Char('q')).unwrap();
        let exit_code = session.wait_for_exit().expect("Should exit");
        assert_eq!(exit_code, 0);

        // Leave capture directory in place for the tui-test-report CI action
        // to collect and upload as artifacts / PR comment images.
        // The directory is in .gitignore so it won't be committed.
    }
}
