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
            "{} adr-bot --force --no-tailor --runner claude-cli --api-url {}",
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

    /// A `codex` that exits 0 with no answer and a model error, the failure
    /// that sends `adr-bot` down its `openai-api` retry.
    fn create_fake_codex_with_model_error(dir: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("fake-codex");
        std::fs::write(
            &script,
            "#!/bin/sh\necho '[ts] ERROR: {\"detail\":\"The model is not supported\"}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    /// Answer a prompt the way a person does, some time after it appears. The
    /// nav poller stays up through the prompt and reads the same queue, so a
    /// key sent with the first frame gets lost or leaves the poller stuck in a
    /// read that the next step then waits on.
    fn answer_prompt(session: &mut TuiSession, key: Key) {
        std::thread::sleep(Duration::from_millis(500));
        session.send_key(key).unwrap();
    }

    /// The fallback's name also passes through the second run's Environment
    /// step, so this waits for it and the prompt on one screen. Waiting for
    /// the name alone would pass with the prompt showing neither.
    #[test]
    fn test_tui_retry_prompt_names_the_fallback_runner() {
        let mut server = mockito::Server::new();
        let adr = make_adr_json(
            "adr-tui-retry",
            "TUI Retry Rule",
            &["Name the runner"],
            &[],
            &["."],
        );
        setup_adr_server(&mut server, &format!("[{}]", adr));
        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);
        let fake_codex = create_fake_codex_with_model_error(env.dir.path());

        // A short terminal and a few projects, so the summary fills the pane and
        // the row only shows if it sits below the summary.
        let root = env.dir.path();
        std::fs::write(
            root.join("package.json"),
            r#"{"private":true,"workspaces":["packages/*"]}"#,
        )
        .unwrap();
        for i in 0..3 {
            let package = root.join(format!("packages/pkg{i}"));
            std::fs::create_dir_all(package.join("src")).unwrap();
            std::fs::write(
                package.join("package.json"),
                format!(r#"{{"name":"pkg{i}"}}"#),
            )
            .unwrap();
            std::fs::write(package.join("src/index.ts"), "export const a = 1;\n").unwrap();
        }

        let cmd = format!(
            "{} adr-bot --runner codex-cli --api-url {}",
            actual_binary_path(),
            env.api_url
        );
        let mut session = TuiSession::new(&cmd)
            .size(120, 14)
            .timeout(Duration::from_secs(30))
            .env("CODEX_BINARY", fake_codex.to_str().unwrap())
            .env("OPENAI_API_KEY", "sk-test")
            // A dead proxy, so a retry that stops asking can't reach OpenAI.
            .env("HTTPS_PROXY", "http://127.0.0.1:9")
            .env("ACTUAL_CONFIG", env.config_path.to_str().unwrap())
            .env("NO_COLOR", "1")
            .workdir(env.dir.path())
            .spawn()
            .expect("Failed to spawn actual adr-bot");

        session
            .wait_for(|screen| {
                screen.contains("Proceed with sync?") && screen.contains("codex-cli")
            })
            .expect("The first prompt should name codex-cli");
        answer_prompt(&mut session, Key::Enter);

        session
            .wait_for(|screen| {
                screen.contains("Proceed with sync?") && screen.contains("openai-api (fallback)")
            })
            .expect("The retry's prompt should name the fallback runner");
        answer_prompt(&mut session, Key::Char('n'));

        // 4 is the exit code for a user-cancelled run.
        assert_eq!(session.wait_for_exit().expect("Should exit"), 4);
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
            "{} adr-bot --force --no-tailor --runner claude-cli --no-tui --api-url http://127.0.0.1:1",
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
            "{} adr-bot --force --no-tailor --runner claude-cli --api-url {}",
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

    // ── Category 7: Update notice ───────────────────────────────────

    /// Run `actual <command>` from a shell at a terminal, with its stdout and
    /// stderr redirected to the files given for them, and return the screen
    /// once the shell reports that `actual` has exited and its exit status.
    fn run_at_a_terminal(
        config_dir: &std::path::Path,
        command: &str,
        stdout_file: Option<&std::path::Path>,
        stderr_file: Option<&std::path::Path>,
    ) -> String {
        let quote = |path: &std::path::Path| {
            format!("'{}'", shell_single_quote_escape(path.to_str().unwrap()))
        };
        let to = |fd: &str, file: Option<&std::path::Path>| {
            file.map_or(String::new(), |file| format!(" {fd}> {}", quote(file)))
        };
        let redirect = to("", stdout_file) + &to("2", stderr_file);
        let script = config_dir.join("run.sh");
        std::fs::write(
            &script,
            format!(
                "unset CI ACTUAL_CLI_SUBPROCESS ACTUAL_NO_UPDATE_CHECK ACTUAL_CONFIG\n\
                 ACTUAL_CONFIG_DIR={} {} {command}{redirect}\n\
                 echo run-finished with status $?\n",
                quote(config_dir),
                quote(std::path::Path::new(&actual_binary_path())),
            ),
        )
        .unwrap();
        let mut session = TuiSession::new(&format!("/bin/sh {}", script.display()))
            .size(120, 40)
            .spawn()
            .expect("Failed to spawn /bin/sh");
        session
            .wait_for_text("run-finished")
            .expect("the script should run to its end");
        session.get_screen_text()
    }

    /// With stdout or stderr redirected, as into a pager or a file, a run at a
    /// terminal prints no notice and leaves the day's notice for the next
    /// interactive run.
    #[test]
    fn test_update_notice_waits_for_a_run_with_both_streams_at_the_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("update-check.yaml");
        // Checked a moment ago, so no run starts a registry lookup.
        let seeded = format!(
            "checked_at: {}\nlatest_version: 999.0.0\n",
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
        );
        std::fs::write(&cache, &seeded).unwrap();

        let stdout_file = dir.path().join("stdout.txt");
        let screen = run_at_a_terminal(dir.path(), "runners", Some(&stdout_file), None);
        assert!(!screen.contains("is available"), "{screen}");
        assert!(std::fs::read_to_string(&stdout_file)
            .unwrap()
            .contains("claude-cli"));
        assert_eq!(std::fs::read_to_string(&cache).unwrap(), seeded);

        let stderr_file = dir.path().join("stderr.txt");
        let screen = run_at_a_terminal(dir.path(), "runners", None, Some(&stderr_file));
        assert!(screen.contains("claude-cli"), "{screen}");
        let stderr = std::fs::read_to_string(&stderr_file).unwrap();
        assert!(!stderr.contains("is available"), "{stderr}");
        assert_eq!(std::fs::read_to_string(&cache).unwrap(), seeded);

        let screen = run_at_a_terminal(dir.path(), "runners", None, None);
        assert!(screen.contains("actual 999.0.0 is available"), "{screen}");
        assert!(std::fs::read_to_string(&cache)
            .unwrap()
            .contains("notified_at"));
    }

    /// A command that fails prints no notice and leaves the day's notice for
    /// a command that succeeds.
    #[test]
    fn test_update_notice_waits_for_a_command_that_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("update-check.yaml");
        // Checked a moment ago, so no run starts a registry lookup.
        let seeded = format!(
            "checked_at: {}\nlatest_version: 999.0.0\n",
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
        );
        std::fs::write(&cache, &seeded).unwrap();

        // Signed out, `whoami` fails.
        let screen = run_at_a_terminal(dir.path(), "whoami", None, None);
        assert!(screen.contains("Not signed in to Actual AI"), "{screen}");
        assert!(screen.contains("run-finished with status 2"), "{screen}");
        assert!(!screen.contains("is available"), "{screen}");
        assert_eq!(std::fs::read_to_string(&cache).unwrap(), seeded);

        // Signed in, the same command succeeds and shows the day's notice.
        std::fs::write(
            dir.path().join("credentials.yaml"),
            "access_token: test-access\nrefresh_token: test-refresh\n\
             organization_id: test-org\nmember_id: test-member\n",
        )
        .unwrap();
        let screen = run_at_a_terminal(dir.path(), "whoami", None, None);
        assert!(screen.contains("Signed in to Actual AI"), "{screen}");
        assert!(screen.contains("run-finished with status 0"), "{screen}");
        assert!(screen.contains("actual 999.0.0 is available"), "{screen}");
        assert!(std::fs::read_to_string(&cache)
            .unwrap()
            .contains("notified_at"));
    }
}
