use super::types::HookType;

pub fn is_evaluation_boundary(hook_type: HookType, tool_name: Option<&str>) -> bool {
    match hook_type {
        HookType::PreToolUse => matches!(tool_name, Some("Edit" | "Write")),
        HookType::PostToolUseFailure => true,
        HookType::Stop => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edit_is_boundary() {
        assert!(is_evaluation_boundary(HookType::PreToolUse, Some("Edit")));
    }

    #[test]
    fn test_write_is_boundary() {
        assert!(is_evaluation_boundary(HookType::PreToolUse, Some("Write")));
    }

    #[test]
    fn test_bash_is_not_boundary() {
        assert!(!is_evaluation_boundary(HookType::PreToolUse, Some("Bash")));
    }

    #[test]
    fn test_read_is_not_boundary() {
        assert!(!is_evaluation_boundary(HookType::PreToolUse, Some("Read")));
    }

    #[test]
    fn test_post_tool_failure_always_boundary() {
        assert!(is_evaluation_boundary(HookType::PostToolUseFailure, None));
        assert!(is_evaluation_boundary(
            HookType::PostToolUseFailure,
            Some("Edit")
        ));
    }

    #[test]
    fn test_stop_always_boundary() {
        assert!(is_evaluation_boundary(HookType::Stop, None));
    }

    #[test]
    fn test_session_start_not_boundary() {
        assert!(!is_evaluation_boundary(HookType::SessionStart, None));
    }

    #[test]
    fn test_prompt_not_boundary() {
        assert!(!is_evaluation_boundary(HookType::UserPromptSubmit, None));
    }

    #[test]
    fn test_post_tool_not_boundary() {
        assert!(!is_evaluation_boundary(HookType::PostToolUse, None));
    }

    #[test]
    fn test_session_end_not_boundary() {
        assert!(!is_evaluation_boundary(HookType::SessionEnd, None));
    }

    #[test]
    fn test_pre_compact_not_boundary() {
        assert!(!is_evaluation_boundary(HookType::PreCompact, None));
    }

    #[test]
    fn test_pre_tool_no_tool_name_not_boundary() {
        assert!(!is_evaluation_boundary(HookType::PreToolUse, None));
    }
}
