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

    fn session_path(&self, session_id: &str) -> PathBuf {
        let safe_id: String = session_id
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        self.dir.join(format!("{safe_id}.jsonl"))
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
