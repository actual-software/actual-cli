use super::types::HookType;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AewoCode {
    pub event_code: &'static str,
    pub tool_class: Option<&'static str>,
    pub artifact_type: Option<&'static str>,
    pub assignment_method: &'static str,
    pub mapping_rule: &'static str,
}

use serde::{Deserialize, Serialize};

pub fn canonicalize(hook_type: HookType, tool_name: Option<&str>) -> AewoCode {
    match (hook_type, tool_name) {
        (HookType::SessionStart, _) => AewoCode {
            event_code: "actual.event.session.started",
            tool_class: None,
            artifact_type: None,
            assignment_method: "deterministic-mapping",
            mapping_rule: "claude-code.SessionStart",
        },
        (HookType::UserPromptSubmit, _) => AewoCode {
            event_code: "actual.event.message.user",
            tool_class: None,
            artifact_type: None,
            assignment_method: "deterministic-mapping",
            mapping_rule: "claude-code.UserPromptSubmit",
        },
        (HookType::PreToolUse, Some("Read")) => AewoCode {
            event_code: "actual.event.file.read",
            tool_class: Some("filesystem"),
            artifact_type: Some("file"),
            assignment_method: "deterministic-mapping",
            mapping_rule: "claude-code.PreToolUse.Read",
        },
        (HookType::PreToolUse, Some("Edit")) => AewoCode {
            event_code: "actual.event.tool.requested",
            tool_class: Some("editor"),
            artifact_type: Some("file"),
            assignment_method: "deterministic-mapping",
            mapping_rule: "claude-code.PreToolUse.Edit",
        },
        (HookType::PreToolUse, Some("Write")) => AewoCode {
            event_code: "actual.event.tool.requested",
            tool_class: Some("editor"),
            artifact_type: Some("file"),
            assignment_method: "deterministic-mapping",
            mapping_rule: "claude-code.PreToolUse.Write",
        },
        (HookType::PreToolUse, Some("Grep")) => AewoCode {
            event_code: "actual.event.grep.executed",
            tool_class: Some("filesystem"),
            artifact_type: Some("file"),
            assignment_method: "deterministic-mapping",
            mapping_rule: "claude-code.PreToolUse.Grep",
        },
        (HookType::PreToolUse, Some("Bash")) => AewoCode {
            event_code: "actual.event.terminal.command",
            tool_class: Some("shell"),
            artifact_type: Some("terminal-output"),
            assignment_method: "deterministic-mapping",
            mapping_rule: "claude-code.PreToolUse.Bash",
        },
        (HookType::PreToolUse, Some("Agent")) => AewoCode {
            event_code: "actual.event.agent.delegated",
            tool_class: Some("orchestration"),
            artifact_type: Some("agent-task"),
            assignment_method: "deterministic-mapping",
            mapping_rule: "claude-code.PreToolUse.Agent",
        },
        (HookType::PostToolUse, Some("Agent")) => AewoCode {
            event_code: "actual.event.agent.returned",
            tool_class: Some("orchestration"),
            artifact_type: Some("agent-result"),
            assignment_method: "deterministic-mapping",
            mapping_rule: "claude-code.PostToolUse.Agent",
        },
        (HookType::PostToolUse, Some("Edit")) => AewoCode {
            event_code: "actual.event.file.modified",
            tool_class: Some("editor"),
            artifact_type: Some("file"),
            assignment_method: "deterministic-mapping",
            mapping_rule: "claude-code.PostToolUse.Edit",
        },
        (HookType::PostToolUse, Some("Write")) => AewoCode {
            event_code: "actual.event.file.created",
            tool_class: Some("editor"),
            artifact_type: Some("file"),
            assignment_method: "deterministic-mapping",
            mapping_rule: "claude-code.PostToolUse.Write",
        },
        (HookType::PostToolUse, Some("Bash")) => AewoCode {
            event_code: "actual.event.command.executed",
            tool_class: Some("shell"),
            artifact_type: Some("terminal-output"),
            assignment_method: "deterministic-mapping",
            mapping_rule: "claude-code.PostToolUse.Bash",
        },
        (HookType::PostToolUseFailure, _) => AewoCode {
            event_code: "actual.event.tool.failed",
            tool_class: tool_name.and_then(tool_name_to_class),
            artifact_type: None,
            assignment_method: "deterministic-mapping",
            mapping_rule: "claude-code.PostToolUseFailure",
        },
        (HookType::Stop, _) => AewoCode {
            event_code: "actual.event.message.agent",
            tool_class: None,
            artifact_type: None,
            assignment_method: "deterministic-mapping",
            mapping_rule: "claude-code.Stop",
        },
        (HookType::SessionEnd, _) => AewoCode {
            event_code: "actual.event.session.completed",
            tool_class: None,
            artifact_type: None,
            assignment_method: "deterministic-mapping",
            mapping_rule: "claude-code.SessionEnd",
        },
        (HookType::PreCompact, _) => AewoCode {
            event_code: "actual.event.session.resumed",
            tool_class: None,
            artifact_type: None,
            assignment_method: "deterministic-mapping",
            mapping_rule: "claude-code.PreCompact",
        },
        // Fallback for any unmapped (hook_type, tool_name) combination
        _ => AewoCode {
            event_code: "actual.event.other",
            tool_class: tool_name.and_then(tool_name_to_class),
            artifact_type: None,
            assignment_method: "deterministic-mapping",
            mapping_rule: "fallback.unmapped",
        },
    }
}

fn tool_name_to_class(name: &str) -> Option<&'static str> {
    match name {
        "Read" | "Grep" => Some("filesystem"),
        "Edit" | "Write" | "LSP" => Some("editor"),
        "Bash" => Some("shell"),
        "WebSearch" | "WebFetch" => Some("http"),
        _ => Some("other"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edit_maps_to_file_edit() {
        let code = canonicalize(HookType::PreToolUse, Some("Edit"));
        assert_eq!(code.event_code, "actual.event.tool.requested");
        assert_eq!(code.tool_class, Some("editor"));
        assert_eq!(code.artifact_type, Some("file"));
    }

    #[test]
    fn test_write_maps_to_tool_requested() {
        let code = canonicalize(HookType::PreToolUse, Some("Write"));
        assert_eq!(code.event_code, "actual.event.tool.requested");
    }

    #[test]
    fn test_bash_maps_to_terminal_command() {
        let code = canonicalize(HookType::PreToolUse, Some("Bash"));
        assert_eq!(code.event_code, "actual.event.terminal.command");
        assert_eq!(code.tool_class, Some("shell"));
    }

    #[test]
    fn test_read_maps_to_file_read() {
        let code = canonicalize(HookType::PreToolUse, Some("Read"));
        assert_eq!(code.event_code, "actual.event.file.read");
    }

    #[test]
    fn test_grep_maps_to_grep_executed() {
        let code = canonicalize(HookType::PreToolUse, Some("Grep"));
        assert_eq!(code.event_code, "actual.event.grep.executed");
    }

    #[test]
    fn test_agent_pre_tool_maps_to_agent_delegated() {
        let code = canonicalize(HookType::PreToolUse, Some("Agent"));
        assert_eq!(code.event_code, "actual.event.agent.delegated");
        assert_eq!(code.tool_class, Some("orchestration"));
        assert_eq!(code.artifact_type, Some("agent-task"));
    }

    #[test]
    fn test_agent_post_tool_maps_to_agent_returned() {
        let code = canonicalize(HookType::PostToolUse, Some("Agent"));
        assert_eq!(code.event_code, "actual.event.agent.returned");
        assert_eq!(code.tool_class, Some("orchestration"));
        assert_eq!(code.artifact_type, Some("agent-result"));
    }

    #[test]
    fn test_post_tool_edit_maps_to_file_modified() {
        let code = canonicalize(HookType::PostToolUse, Some("Edit"));
        assert_eq!(code.event_code, "actual.event.file.modified");
    }

    #[test]
    fn test_post_tool_failure_maps_to_tool_failed() {
        let code = canonicalize(HookType::PostToolUseFailure, Some("Bash"));
        assert_eq!(code.event_code, "actual.event.tool.failed");
        assert_eq!(code.tool_class, Some("shell"));
    }

    #[test]
    fn test_session_start_maps_to_session_started() {
        let code = canonicalize(HookType::SessionStart, None);
        assert_eq!(code.event_code, "actual.event.session.started");
        assert_eq!(code.mapping_rule, "claude-code.SessionStart");
    }

    #[test]
    fn test_stop_maps_to_message_agent() {
        let code = canonicalize(HookType::Stop, None);
        assert_eq!(code.event_code, "actual.event.message.agent");
    }

    #[test]
    fn test_session_end_maps_to_session_completed() {
        let code = canonicalize(HookType::SessionEnd, None);
        assert_eq!(code.event_code, "actual.event.session.completed");
    }

    #[test]
    fn test_unknown_tool_maps_to_fallback() {
        let code = canonicalize(HookType::PreToolUse, Some("SomeFutureTool"));
        assert_eq!(code.event_code, "actual.event.other");
        assert_eq!(code.mapping_rule, "fallback.unmapped");
    }

    #[test]
    fn test_all_codes_have_deterministic_assignment() {
        let code = canonicalize(HookType::PreToolUse, Some("Edit"));
        assert_eq!(code.assignment_method, "deterministic-mapping");
    }
}
