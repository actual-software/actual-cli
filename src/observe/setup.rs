use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::ActualError;

struct HookEntry {
    hook_name: &'static str,
    command: &'static str,
    matcher: &'static str,
    timeout: u64,
}

const DEFAULT_HOOK_ENTRIES: &[HookEntry] = &[
    HookEntry { hook_name: "SessionStart", command: "actual observe session-start", matcher: "", timeout: 1200 },
    HookEntry { hook_name: "UserPromptSubmit", command: "actual observe prompt", matcher: "", timeout: 1200 },
    HookEntry { hook_name: "PreToolUse", command: "actual observe pre-tool", matcher: "ExitPlanMode", timeout: 30 },
    HookEntry { hook_name: "Stop", command: "actual observe stop", matcher: "", timeout: 1200 },
    HookEntry { hook_name: "SessionEnd", command: "actual observe session-end", matcher: "", timeout: 1200 },
];

const EXTRA_HOOK_ENTRIES: &[HookEntry] = &[
    HookEntry { hook_name: "PreToolUse", command: "actual observe pre-tool", matcher: "Edit|Write", timeout: 30 },
    HookEntry { hook_name: "PreToolUse", command: "actual observe pre-tool", matcher: "Bash", timeout: 30 },
    HookEntry { hook_name: "PreToolUse", command: "actual observe pre-tool", matcher: "Agent", timeout: 600 },
    HookEntry { hook_name: "PostToolUse", command: "actual observe post-tool", matcher: "", timeout: 1200 },
    HookEntry { hook_name: "PostToolUseFailure", command: "actual observe post-tool-failure", matcher: "", timeout: 1200 },
    HookEntry { hook_name: "PreCompact", command: "actual observe pre-compact", matcher: "", timeout: 1200 },
    HookEntry { hook_name: "SubagentStart", command: "actual observe subagent-tool", matcher: "", timeout: 1200 },
];

pub fn install_hooks(settings_path: &Path, localhost: bool, hook_all: bool) -> Result<(), ActualError> {
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

    let entries: Vec<&HookEntry> = if hook_all {
        DEFAULT_HOOK_ENTRIES.iter().chain(EXTRA_HOOK_ENTRIES.iter()).collect()
    } else {
        DEFAULT_HOOK_ENTRIES.iter().collect()
    };

    for entry in entries {
        let command = if localhost {
            format!(
                "ACTUAL_AUTH_URL=http://localhost:3000 ACTUAL_API_URL=http://localhost:3002 {}",
                entry.command
            )
        } else {
            entry.command.to_string()
        };

        let entries = hooks_obj
            .entry(entry.hook_name)
            .or_insert_with(|| serde_json::json!([]));

        let arr = entries.as_array_mut().ok_or_else(|| {
            ActualError::ConfigError(format!("hooks.{} is not an array", entry.hook_name))
        })?;

        let existing_idx = arr.iter().position(|matcher_group| {
            let matcher_matches = matcher_group
                .get("matcher")
                .and_then(|m| m.as_str())
                .unwrap_or("") == entry.matcher;
            let command_matches = matcher_group
                .get("hooks")
                .and_then(|h| h.as_array())
                .map(|hooks| {
                    hooks.iter().any(|hook| {
                        let cmd = hook.get("command").and_then(|c| c.as_str()).unwrap_or("");
                        cmd == command || cmd.ends_with(entry.command)
                    })
                })
                .unwrap_or(false);
            matcher_matches && command_matches
        });

        match existing_idx {
            Some(idx) => {
                arr[idx] = serde_json::json!({
                    "matcher": entry.matcher,
                    "hooks": [
                        {
                            "type": "command",
                            "command": command,
                            "timeout": entry.timeout
                        }
                    ]
                });
            }
            None => {
                arr.push(serde_json::json!({
                    "matcher": entry.matcher,
                    "hooks": [
                        {
                            "type": "command",
                            "command": command,
                            "timeout": entry.timeout
                        }
                    ]
                }));
            }
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
    fn test_inserts_default_hooks_into_empty_settings() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks(&path, false, false).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let hooks = content["hooks"].as_object().unwrap();

        assert_eq!(hooks.len(), 5, "default installs 5 hook types: SessionStart, UserPromptSubmit, PreToolUse, Stop, SessionEnd");
        let expected_default = vec!["SessionStart", "UserPromptSubmit", "PreToolUse", "Stop", "SessionEnd"];
        for hook in &expected_default {
            assert!(hooks.contains_key(*hook), "missing default hook: {hook}");
        }
        assert_eq!(
            hooks["SessionStart"][0]["hooks"][0]["command"].as_str().unwrap(),
            "actual observe session-start"
        );
        let pre_tool = hooks["PreToolUse"].as_array().unwrap();
        assert_eq!(pre_tool.len(), 1, "default has only ExitPlanMode matcher");
        assert_eq!(pre_tool[0]["matcher"].as_str().unwrap(), "ExitPlanMode");
    }

    #[test]
    fn test_hook_all_installs_all_hooks() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks(&path, false, true).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let hooks = content["hooks"].as_object().unwrap();

        let expected = vec![
            "SessionStart", "UserPromptSubmit", "PreToolUse",
            "PostToolUse", "PostToolUseFailure", "Stop",
            "SessionEnd", "PreCompact", "SubagentStart",
        ];
        for hook in &expected {
            assert!(hooks.contains_key(*hook), "missing hook: {hook}");
        }
        let pre_tool = hooks["PreToolUse"].as_array().unwrap();
        assert_eq!(pre_tool.len(), 4, "hook-all has 4 PreToolUse matchers: ExitPlanMode, Edit|Write, Bash, Agent");
        assert_eq!(pre_tool[0]["matcher"].as_str().unwrap(), "ExitPlanMode");
        assert_eq!(pre_tool[1]["matcher"].as_str().unwrap(), "Edit|Write");
        assert_eq!(pre_tool[2]["matcher"].as_str().unwrap(), "Bash");
        assert_eq!(pre_tool[3]["matcher"].as_str().unwrap(), "Agent");
        assert_eq!(pre_tool[3]["hooks"][0]["timeout"].as_u64().unwrap(), 600);
    }

    #[test]
    fn test_preserves_existing_hooks() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let existing = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {"matcher": "", "hooks": [{"type": "command", "command": "beads observe start"}]}
                ]
            }
        });
        fs::write(&path, serde_json::to_string(&existing).unwrap()).unwrap();

        install_hooks(&path, false, false).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let session_start = content["hooks"]["SessionStart"].as_array().unwrap();

        assert_eq!(session_start.len(), 2);
        assert_eq!(
            session_start[0]["hooks"][0]["command"].as_str().unwrap(),
            "beads observe start"
        );
        assert_eq!(
            session_start[1]["hooks"][0]["command"].as_str().unwrap(),
            "actual observe session-start"
        );
    }

    #[test]
    fn test_idempotent_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks(&path, false, false).unwrap();
        install_hooks(&path, false, false).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let session_start = content["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(session_start.len(), 1, "should not duplicate hooks on re-run");
        let pre_tool = content["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre_tool.len(), 1, "default PreToolUse should have exactly 1 matcher entry after re-run");
    }

    #[test]
    fn test_idempotent_hook_all() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks(&path, false, true).unwrap();
        install_hooks(&path, false, true).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let session_start = content["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(session_start.len(), 1, "should not duplicate hooks on re-run");
        let pre_tool = content["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre_tool.len(), 4, "hook-all PreToolUse should have exactly 4 matcher entries after re-run");
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

        install_hooks(&path, false, false).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["model"].as_str().unwrap(), "opus");
        assert_eq!(content["theme"].as_str().unwrap(), "dark");
        assert!(content["hooks"].is_object());
    }

    #[test]
    fn test_creates_parent_dirs() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("deep").join("settings.json");

        install_hooks(&path, false, false).unwrap();

        assert!(path.exists());
    }

    #[test]
    fn test_hook_entries_have_correct_type_and_valid_timeout() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks(&path, false, true).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let hooks = content["hooks"].as_object().unwrap();

        for (_, entries) in hooks {
            let arr = entries.as_array().unwrap();
            for matcher_group in arr {
                assert!(matcher_group.get("matcher").is_some());
                let inner_hooks = matcher_group["hooks"].as_array().unwrap();
                for hook in inner_hooks {
                    assert_eq!(hook["type"].as_str().unwrap(), "command");
                    let timeout = hook["timeout"].as_u64().unwrap();
                    assert!(timeout > 0 && timeout <= 1200, "timeout should be between 1 and 1200, got {timeout}");
                }
            }
        }
    }

    #[test]
    fn test_localhost_flag_prefixes_commands() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks(&path, true, false).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let hooks = content["hooks"].as_object().unwrap();

        let prefix = "ACTUAL_AUTH_URL=http://localhost:3000 ACTUAL_API_URL=http://localhost:3002 ";
        assert_eq!(
            hooks["SessionStart"][0]["hooks"][0]["command"].as_str().unwrap(),
            format!("{}actual observe session-start", prefix)
        );
        assert_eq!(
            hooks["PreToolUse"][0]["hooks"][0]["command"].as_str().unwrap(),
            format!("{}actual observe pre-tool", prefix)
        );
        assert_eq!(
            hooks["Stop"][0]["hooks"][0]["command"].as_str().unwrap(),
            format!("{}actual observe stop", prefix)
        );
    }

    #[test]
    fn test_localhost_to_production_replaces_commands() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks(&path, true, false).unwrap();
        install_hooks(&path, false, false).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let session_start = content["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(session_start.len(), 1);
        assert_eq!(
            session_start[0]["hooks"][0]["command"].as_str().unwrap(),
            "actual observe session-start"
        );
    }

    #[test]
    fn test_production_to_localhost_replaces_commands() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks(&path, false, false).unwrap();
        install_hooks(&path, true, false).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let session_start = content["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(session_start.len(), 1);
        let prefix = "ACTUAL_AUTH_URL=http://localhost:3000 ACTUAL_API_URL=http://localhost:3002 ";
        assert_eq!(
            session_start[0]["hooks"][0]["command"].as_str().unwrap(),
            format!("{}actual observe session-start", prefix)
        );
    }
}
