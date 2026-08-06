use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ActualError;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HookEntry {
    r#type: String,
    command: String,
}

const HOOK_COMMANDS: &[(&str, &str)] = &[
    ("SessionStart", "actual observe session-start"),
    ("UserPromptSubmit", "actual observe prompt"),
    ("PreToolUse", "actual observe pre-tool"),
    ("PostToolUse", "actual observe post-tool"),
    ("PostToolUseFailure", "actual observe post-tool-failure"),
    ("Stop", "actual observe stop"),
    ("SessionEnd", "actual observe session-end"),
    ("PreCompact", "actual observe pre-compact"),
];

pub fn install_hooks(settings_path: &Path) -> Result<(), ActualError> {
    let mut settings: Value = if settings_path.exists() {
        let content = fs::read_to_string(settings_path).map_err(|e| {
            ActualError::ConfigError(format!("failed to read {}: {e}", settings_path.display()))
        })?;
        serde_json::from_str(&content).map_err(|e| {
            ActualError::ConfigError(format!("invalid JSON in {}: {e}", settings_path.display()))
        })?
    } else {
        serde_json::json!({})
    };

    let hooks = settings
        .as_object_mut()
        .ok_or_else(|| ActualError::ConfigError("settings is not a JSON object".to_string()))?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));

    let hooks_obj = hooks.as_object_mut().ok_or_else(|| {
        ActualError::ConfigError("hooks is not a JSON object".to_string())
    })?;

    for (hook_name, command) in HOOK_COMMANDS {
        let entries = hooks_obj
            .entry(*hook_name)
            .or_insert_with(|| serde_json::json!([]));

        let arr = entries.as_array_mut().ok_or_else(|| {
            ActualError::ConfigError(format!("hooks.{hook_name} is not an array"))
        })?;

        let already_present = arr.iter().any(|entry| {
            entry.get("command").and_then(|c| c.as_str()) == Some(command)
        });

        if !already_present {
            arr.push(serde_json::json!({
                "type": "command",
                "command": command,
            }));
        }
    }

    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            ActualError::ConfigError(format!(
                "failed to create dir {}: {e}",
                parent.display()
            ))
        })?;
    }

    let tmp_path = settings_path.with_extension("tmp");
    let json = serde_json::to_string_pretty(&settings).map_err(|e| {
        ActualError::ConfigError(format!("failed to serialize settings: {e}"))
    })?;
    fs::write(&tmp_path, &json).map_err(|e| {
        ActualError::ConfigError(format!("failed to write {}: {e}", tmp_path.display()))
    })?;
    fs::rename(&tmp_path, settings_path).map_err(|e| {
        ActualError::ConfigError(format!("failed to rename to {}: {e}", settings_path.display()))
    })?;

    Ok(())
}

pub fn default_settings_path() -> PathBuf {
    PathBuf::from(".claude").join("settings.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_inserts_hooks_into_empty_settings() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks(&path).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let hooks = content["hooks"].as_object().unwrap();

        assert_eq!(hooks.len(), 8);
        assert_eq!(
            hooks["SessionStart"][0]["command"].as_str().unwrap(),
            "actual observe session-start"
        );
        assert_eq!(
            hooks["PreToolUse"][0]["command"].as_str().unwrap(),
            "actual observe pre-tool"
        );
    }

    #[test]
    fn test_preserves_existing_hooks() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let existing = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {"type": "command", "command": "beads observe start"}
                ]
            }
        });
        fs::write(&path, serde_json::to_string(&existing).unwrap()).unwrap();

        install_hooks(&path).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let session_start = content["hooks"]["SessionStart"].as_array().unwrap();

        assert_eq!(session_start.len(), 2);
        assert_eq!(
            session_start[0]["command"].as_str().unwrap(),
            "beads observe start"
        );
        assert_eq!(
            session_start[1]["command"].as_str().unwrap(),
            "actual observe session-start"
        );
    }

    #[test]
    fn test_idempotent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks(&path).unwrap();
        install_hooks(&path).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let session_start = content["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(
            session_start.len(),
            1,
            "should not duplicate hooks on re-run"
        );
    }

    #[test]
    fn test_preserves_non_hook_settings() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let existing = serde_json::json!({
            "model": "opus",
            "theme": "dark"
        });
        fs::write(&path, serde_json::to_string(&existing).unwrap()).unwrap();

        install_hooks(&path).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["model"].as_str().unwrap(), "opus");
        assert_eq!(content["theme"].as_str().unwrap(), "dark");
        assert!(content["hooks"].is_object());
    }

    #[test]
    fn test_creates_parent_dirs() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("deep").join("settings.json");

        install_hooks(&path).unwrap();

        assert!(path.exists());
    }

    #[test]
    fn test_all_eight_hooks_installed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks(&path).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let hooks = content["hooks"].as_object().unwrap();

        let expected = vec![
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "PostToolUseFailure",
            "Stop",
            "SessionEnd",
            "PreCompact",
        ];
        for hook in &expected {
            assert!(
                hooks.contains_key(*hook),
                "missing hook: {hook}"
            );
        }
    }

    #[test]
    fn test_hook_entries_have_correct_type() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks(&path).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let hooks = content["hooks"].as_object().unwrap();

        for (_, entries) in hooks {
            let arr = entries.as_array().unwrap();
            for entry in arr {
                assert_eq!(entry["type"].as_str().unwrap(), "command");
            }
        }
    }
}
