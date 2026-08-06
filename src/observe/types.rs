use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookType {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    Stop,
    SessionEnd,
    PreCompact,
}

impl HookType {
    pub fn from_subcommand(name: &str) -> Option<Self> {
        match name {
            "session-start" => Some(Self::SessionStart),
            "prompt" => Some(Self::UserPromptSubmit),
            "pre-tool" => Some(Self::PreToolUse),
            "post-tool" => Some(Self::PostToolUse),
            "post-tool-failure" => Some(Self::PostToolUseFailure),
            "stop" => Some(Self::Stop),
            "session-end" => Some(Self::SessionEnd),
            "pre-compact" => Some(Self::PreCompact),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::PostToolUseFailure => "PostToolUseFailure",
            Self::Stop => "Stop",
            Self::SessionEnd => "SessionEnd",
            Self::PreCompact => "PreCompact",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookPayload {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<Value>,
    #[serde(default)]
    pub tool_output: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_type_from_subcommand() {
        assert_eq!(
            HookType::from_subcommand("session-start"),
            Some(HookType::SessionStart)
        );
        assert_eq!(
            HookType::from_subcommand("pre-tool"),
            Some(HookType::PreToolUse)
        );
        assert_eq!(
            HookType::from_subcommand("post-tool-failure"),
            Some(HookType::PostToolUseFailure)
        );
        assert_eq!(HookType::from_subcommand("invalid"), None);
    }

    #[test]
    fn test_deserialize_session_start() {
        let json = r#"{"session_id": "s1", "cwd": "/tmp/project", "model": "opus"}"#;
        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.session_id, Some("s1".to_string()));
        assert_eq!(payload.cwd, Some("/tmp/project".to_string()));
        assert_eq!(payload.model, Some("opus".to_string()));
    }

    #[test]
    fn test_deserialize_pre_tool_use() {
        let json = r#"{"tool_name": "Edit", "tool_input": {"file_path": "/src/main.rs"}}"#;
        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.tool_name, Some("Edit".to_string()));
        assert!(payload.tool_input.is_some());
    }

    #[test]
    fn test_deserialize_post_tool_failure() {
        let json = r#"{"tool_name": "Bash", "error": "exit code 1"}"#;
        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.tool_name, Some("Bash".to_string()));
        assert_eq!(payload.error, Some("exit code 1".to_string()));
    }

    #[test]
    fn test_deserialize_extra_fields_preserved() {
        let json = r#"{"session_id": "s1", "custom_field": "value"}"#;
        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(
            payload.extra.get("custom_field").unwrap().as_str(),
            Some("value")
        );
    }
}
