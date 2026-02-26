#[cfg(unix)]
mod common;

#[cfg(unix)]
mod tests {
    use super::common::*;

    #[test]
    fn smoke_test_env_zero_adrs() {
        let mut server = mockito::Server::new();
        let empty_response = make_match_response_json("[]");
        server
            .mock("POST", "/adrs/match")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&empty_response)
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
    }

    #[test]
    fn smoke_make_adr_json_parses() {
        let adr = make_adr_json("adr-001", "Test Rule", &["policy1"], &["inst1"], &["."]);
        let parsed: serde_json::Value = serde_json::from_str(&adr).unwrap();
        assert_eq!(parsed["id"], "adr-001");
        assert_eq!(parsed["title"], "Test Rule");
    }

    #[test]
    fn smoke_make_match_response_json_parses() {
        let adr = make_adr_json("adr-001", "Test Rule", &["policy1"], &["inst1"], &["."]);
        let response = make_match_response_json(&format!("[{}]", adr));
        let parsed: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed["matched_adrs"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["metadata"]["total_matched"], 1);
    }

    #[test]
    fn smoke_test_env_write_and_read_file() {
        let server = mockito::Server::new();
        let env = TestEnv::new(&server, AUTH_OK, ANALYSIS_SINGLE_PROJECT);
        env.write_file("test.txt", "hello");
        assert!(env.file_exists("test.txt"));
        assert_eq!(env.read_file("test.txt"), "hello");
        assert!(!env.file_exists("nonexistent.txt"));
    }
}
