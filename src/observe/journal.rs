use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use serde_json::Value;

use crate::config::paths;
use crate::error::ActualError;

use super::canonicalize::AewoCode;

pub struct SessionJournal {
    dir: PathBuf,
}

impl SessionJournal {
    pub fn new() -> Result<Self, ActualError> {
        let dir = paths::config_dir()?.join("sessions");
        Ok(Self { dir })
    }

    #[cfg(test)]
    pub fn with_dir(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn append(
        &self,
        session_id: &str,
        raw_payload: &Value,
        aewo_code: &AewoCode,
    ) -> Result<(), ActualError> {
        fs::create_dir_all(&self.dir).map_err(|e| {
            ActualError::ConfigError(format!("failed to create sessions dir: {e}"))
        })?;

        let path = self.session_path(session_id);

        let mut entry = raw_payload.clone();
        if let Some(obj) = entry.as_object_mut() {
            obj.insert(
                "aewo_code".to_string(),
                Value::String(aewo_code.event_code.to_string()),
            );
            obj.insert(
                "aewo_mapping_rule".to_string(),
                Value::String(aewo_code.mapping_rule.to_string()),
            );
        }

        let line =
            serde_json::to_string(&entry).map_err(|e| ActualError::ConfigError(e.to_string()))?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| {
                ActualError::ConfigError(format!("failed to open journal {}: {e}", path.display()))
            })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&path, perms).ok();
        }

        writeln!(file, "{line}").map_err(|e| {
            ActualError::ConfigError(format!(
                "failed to write journal {}: {e}",
                path.display()
            ))
        })?;

        Ok(())
    }

    /// Read back all events for a session as a list of JSON values.
    /// Returns an empty vec if the session file does not exist.
    pub fn read_session(&self, session_id: &str) -> Result<Vec<serde_json::Value>, ActualError> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Ok(vec![]);
        }
        let content = std::fs::read_to_string(&path).map_err(|e| {
            ActualError::ConfigError(format!("failed to read journal {}: {e}", path.display()))
        })?;
        let events: Vec<serde_json::Value> = content
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
            .filter_map(|(idx, line)| {
                match serde_json::from_str(line) {
                    Ok(val) => Some(val),
                    Err(e) => {
                        eprintln!("advisor: journal line {} corrupt, skipping: {e}", idx + 1);
                        None
                    }
                }
            })
            .collect();
        Ok(events)
    }

    /// Read events starting from a line offset (0-based). Returns events and the
    /// new cursor (total non-empty lines seen so far).
    pub fn read_session_from(
        &self,
        session_id: &str,
        from_line: usize,
    ) -> Result<(Vec<serde_json::Value>, usize), ActualError> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Ok((vec![], 0));
        }
        let content = std::fs::read_to_string(&path).map_err(|e| {
            ActualError::ConfigError(format!("failed to read journal {}: {e}", path.display()))
        })?;
        let mut events = Vec::new();
        let mut valid_count: usize = 0;
        for (idx, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            valid_count += 1;
            if valid_count <= from_line {
                continue;
            }
            match serde_json::from_str(line) {
                Ok(val) => events.push(val),
                Err(e) => {
                    eprintln!("advisor: journal line {} corrupt, skipping: {e}", idx + 1);
                }
            }
        }
        Ok((events, valid_count))
    }

    /// Read the boundary cursor (line offset) for a session.
    pub fn read_cursor(&self, session_id: &str) -> usize {
        let path = self.cursor_path(session_id);
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    /// Write the boundary cursor after a successful evaluation.
    pub fn write_cursor(&self, session_id: &str, offset: usize) -> Result<(), ActualError> {
        let path = self.cursor_path(session_id);
        std::fs::write(&path, offset.to_string()).map_err(|e| {
            ActualError::ConfigError(format!(
                "failed to write cursor {}: {e}",
                path.display()
            ))
        })
    }

    pub fn is_stop_acknowledged(&self, session_id: &str) -> bool {
        self.stop_ack_path(session_id).exists()
    }

    pub fn set_stop_acknowledged(&self, session_id: &str) {
        let path = self.stop_ack_path(session_id);
        fs::write(&path, "1").ok();
    }

    pub fn clear_stop_acknowledged(&self, session_id: &str) {
        let path = self.stop_ack_path(session_id);
        fs::remove_file(&path).ok();
    }

    fn session_path(&self, session_id: &str) -> PathBuf {
        self.dir.join(format!("{}.jsonl", Self::safe_id(session_id)))
    }

    fn cursor_path(&self, session_id: &str) -> PathBuf {
        self.dir.join(format!("{}.cursor", Self::safe_id(session_id)))
    }

    fn stop_ack_path(&self, session_id: &str) -> PathBuf {
        self.dir.join(format!("{}.stop_ack", Self::safe_id(session_id)))
    }

    fn safe_id(session_id: &str) -> String {
        session_id
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_code() -> AewoCode {
        AewoCode {
            event_code: "actual.event.session.started",
            tool_class: None,
            artifact_type: None,
            assignment_method: "deterministic-mapping",
            mapping_rule: "claude-code.SessionStart",
        }
    }

    #[test]
    fn test_append_creates_session_file() {
        let dir = tempdir().unwrap();
        let journal = SessionJournal::with_dir(dir.path().to_path_buf());
        let payload = serde_json::json!({"session_id": "s1", "cwd": "/tmp"});

        journal.append("s1", &payload, &test_code()).unwrap();

        let path = dir.path().join("s1.jsonl");
        assert!(path.exists(), "journal file should exist");

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1, "should have exactly one line");

        let parsed: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(
            parsed["aewo_code"].as_str(),
            Some("actual.event.session.started")
        );
    }

    #[test]
    fn test_append_multiple_events() {
        let dir = tempdir().unwrap();
        let journal = SessionJournal::with_dir(dir.path().to_path_buf());
        let payload1 = serde_json::json!({"event": 1});
        let payload2 = serde_json::json!({"event": 2});

        journal.append("s1", &payload1, &test_code()).unwrap();
        journal.append("s1", &payload2, &test_code()).unwrap();

        let content = fs::read_to_string(dir.path().join("s1.jsonl")).unwrap();
        assert_eq!(content.lines().count(), 2);
    }

    #[test]
    fn test_separate_sessions_separate_files() {
        let dir = tempdir().unwrap();
        let journal = SessionJournal::with_dir(dir.path().to_path_buf());
        let payload = serde_json::json!({"data": true});

        journal.append("sess-a", &payload, &test_code()).unwrap();
        journal.append("sess-b", &payload, &test_code()).unwrap();

        assert!(dir.path().join("sess-a.jsonl").exists());
        assert!(dir.path().join("sess-b.jsonl").exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_journal_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let journal = SessionJournal::with_dir(dir.path().to_path_buf());
        let payload = serde_json::json!({"session_id": "s1"});

        journal.append("s1", &payload, &test_code()).unwrap();

        let path = dir.path().join("s1.jsonl");
        let perms = fs::metadata(&path).unwrap().permissions();
        assert_eq!(
            perms.mode() & 0o777,
            0o600,
            "journal file should have 0600 permissions"
        );
    }

    #[test]
    fn test_read_session_returns_empty_for_missing_session() {
        let dir = tempdir().unwrap();
        let journal = SessionJournal::with_dir(dir.path().to_path_buf());

        let events = journal.read_session("nonexistent").unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_read_session_returns_appended_events() {
        let dir = tempdir().unwrap();
        let journal = SessionJournal::with_dir(dir.path().to_path_buf());
        let payload1 = serde_json::json!({"event": "first"});
        let payload2 = serde_json::json!({"event": "second"});

        journal.append("s1", &payload1, &test_code()).unwrap();
        journal.append("s1", &payload2, &test_code()).unwrap();

        let events = journal.read_session("s1").unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["event"], "first");
        assert_eq!(events[1]["event"], "second");
        // Verify aewo_code was injected
        assert_eq!(
            events[0]["aewo_code"].as_str(),
            Some("actual.event.session.started")
        );
    }

    #[test]
    fn test_read_session_skips_empty_lines() {
        let dir = tempdir().unwrap();
        let journal = SessionJournal::with_dir(dir.path().to_path_buf());
        let payload = serde_json::json!({"event": "one"});

        journal.append("s1", &payload, &test_code()).unwrap();

        // Manually append a blank line to the file
        let path = dir.path().join("s1.jsonl");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        use std::io::Write;
        writeln!(file, "").unwrap();
        writeln!(file, "  ").unwrap();

        let events = journal.read_session("s1").unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_read_session_skips_invalid_json_lines() {
        let dir = tempdir().unwrap();
        let journal = SessionJournal::with_dir(dir.path().to_path_buf());
        let payload = serde_json::json!({"event": "valid"});

        journal.append("s1", &payload, &test_code()).unwrap();

        // Manually append invalid JSON
        let path = dir.path().join("s1.jsonl");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        use std::io::Write;
        writeln!(file, "not valid json").unwrap();

        let events = journal.read_session("s1").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event"], "valid");
    }

    #[test]
    fn test_stop_ack_lifecycle() {
        let dir = tempdir().unwrap();
        let journal = SessionJournal::with_dir(dir.path().to_path_buf());
        fs::create_dir_all(dir.path()).unwrap();

        assert!(!journal.is_stop_acknowledged("s1"));

        journal.set_stop_acknowledged("s1");
        assert!(journal.is_stop_acknowledged("s1"));

        journal.clear_stop_acknowledged("s1");
        assert!(!journal.is_stop_acknowledged("s1"));
    }

    #[test]
    fn test_stop_ack_clear_is_idempotent() {
        let dir = tempdir().unwrap();
        let journal = SessionJournal::with_dir(dir.path().to_path_buf());
        fs::create_dir_all(dir.path()).unwrap();

        journal.clear_stop_acknowledged("nonexistent");
        assert!(!journal.is_stop_acknowledged("nonexistent"));
    }

    #[test]
    fn test_sanitizes_session_id() {
        let dir = tempdir().unwrap();
        let journal = SessionJournal::with_dir(dir.path().to_path_buf());
        let payload = serde_json::json!({});

        journal
            .append("../../../etc/passwd", &payload, &test_code())
            .unwrap();

        let files: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(files.len(), 1);
        let name = files[0].file_name().to_string_lossy().to_string();
        assert!(!name.contains(".."), "session id should be sanitized");
        assert!(name.ends_with(".jsonl"));
    }
}
