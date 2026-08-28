use super::types::HookType;

const MIN_PROMPT_LENGTH: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolAction {
    Free,
    LeaseGated,
    AdvisorGated,
}

pub fn classify_tool_action(tool_name: Option<&str>, payload: &serde_json::Value) -> ToolAction {
    match tool_name {
        Some("Edit" | "Write") => ToolAction::LeaseGated,
        Some("Bash") => {
            if is_mutating_bash(payload) {
                ToolAction::AdvisorGated
            } else {
                ToolAction::Free
            }
        }
        Some("Agent") => {
            if is_agent_launch(payload) {
                ToolAction::AdvisorGated
            } else {
                ToolAction::Free
            }
        }
        Some("AskUserQuestion") => ToolAction::AdvisorGated,
        _ => ToolAction::Free,
    }
}

pub fn is_evaluation_boundary(
    hook_type: HookType,
    tool_name: Option<&str>,
    payload: &serde_json::Value,
) -> bool {
    match hook_type {
        HookType::PreToolUse => match tool_name {
            Some("Edit" | "Write") => true,
            Some("AskUserQuestion") => true,
            Some("Bash") => is_mutating_bash(payload),
            Some("Agent") => is_agent_launch(payload),
            _ => false,
        },
        HookType::PostToolUse => matches!(tool_name, Some("Agent")),
        HookType::PostToolUseFailure => true,
        HookType::Stop => true,
        HookType::UserPromptSubmit => is_substantial_prompt(payload),
        _ => false,
    }
}

fn is_substantial_prompt(payload: &serde_json::Value) -> bool {
    let prompt = payload
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    prompt.len() >= MIN_PROMPT_LENGTH
}

const MUTATING_BASH_PATTERNS: &[&str] = &[
    "rm ", "rm\t", "rmdir",
    "mv ", "mv\t",
    "cp ", "cp\t",
    "chmod", "chown",
    "git commit", "git push", "git reset", "git checkout", "git merge", "git rebase",
    "git stash", "git cherry-pick", "git revert",
    "npm install", "npm uninstall", "npm ci",
    "pnpm install", "pnpm add", "pnpm remove",
    "yarn add", "yarn remove",
    "pip install", "pip uninstall",
    "uv add", "uv remove", "uv sync",
    "cargo add", "cargo install",
    "docker build", "docker push", "docker run",
    "kubectl apply", "kubectl delete",
    "make ",
    "curl -x post", "curl -x put", "curl -x delete", "curl -x patch",
];

fn is_mutating_bash(payload: &serde_json::Value) -> bool {
    let command = payload
        .get("tool_input")
        .and_then(|v| v.get("command"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if command.is_empty() {
        return false;
    }
    let lower = command.to_lowercase();
    for pattern in MUTATING_BASH_PATTERNS {
        if lower.contains(pattern) {
            return true;
        }
    }
    if has_output_redirect(command) {
        return true;
    }
    false
}

fn has_output_redirect(command: &str) -> bool {
    if command.contains("| tee") || command.contains(">>") {
        return true;
    }
    for (i, ch) in command.char_indices() {
        if ch == '>' {
            if i > 0 {
                let prev = command.as_bytes()[i - 1];
                if prev == b'2' || prev == b'&' {
                    continue;
                }
            }
            return true;
        }
    }
    false
}

fn is_agent_launch(payload: &serde_json::Value) -> bool {
    let tool_input = match payload.get("tool_input") {
        Some(v) => v,
        None => return false,
    };
    let prompt = tool_input
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let description = tool_input
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    !prompt.is_empty() || !description.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn empty() -> serde_json::Value {
        json!({})
    }

    fn prompt_payload(text: &str) -> serde_json::Value {
        json!({"prompt": text, "session_id": "test-session"})
    }

    fn agent_pre_tool(description: &str, prompt: &str) -> serde_json::Value {
        json!({
            "tool_name": "Agent",
            "tool_input": {
                "description": description,
                "prompt": prompt,
                "subagent_type": "general-purpose"
            }
        })
    }

    fn agent_post_tool(description: &str, content: &str) -> serde_json::Value {
        json!({
            "tool_name": "Agent",
            "tool_input": {"description": description},
            "tool_response": {
                "content": content,
                "status": "completed",
                "agentType": "general-purpose"
            }
        })
    }

    // ── Edit / Write ──

    #[test]
    fn test_edit_is_boundary() {
        assert!(is_evaluation_boundary(
            HookType::PreToolUse,
            Some("Edit"),
            &empty()
        ));
    }

    #[test]
    fn test_write_is_boundary() {
        assert!(is_evaluation_boundary(
            HookType::PreToolUse,
            Some("Write"),
            &empty()
        ));
    }

    #[test]
    fn test_bash_readonly_is_not_boundary() {
        let payload = json!({"tool_input": {"command": "ls -la src/"}});
        assert!(!is_evaluation_boundary(
            HookType::PreToolUse,
            Some("Bash"),
            &payload
        ));
    }

    #[test]
    fn test_bash_empty_command_is_not_boundary() {
        assert!(!is_evaluation_boundary(
            HookType::PreToolUse,
            Some("Bash"),
            &empty()
        ));
    }

    #[test]
    fn test_bash_grep_is_not_boundary() {
        let payload = json!({"tool_input": {"command": "grep -rn 'pattern' src/"}});
        assert!(!is_evaluation_boundary(
            HookType::PreToolUse,
            Some("Bash"),
            &payload
        ));
    }

    #[test]
    fn test_bash_rm_is_boundary() {
        let payload = json!({"tool_input": {"command": "rm -rf dist/"}});
        assert!(is_evaluation_boundary(
            HookType::PreToolUse,
            Some("Bash"),
            &payload
        ));
    }

    #[test]
    fn test_bash_git_commit_is_boundary() {
        let payload = json!({"tool_input": {"command": "git commit -m 'fix bug'"}});
        assert!(is_evaluation_boundary(
            HookType::PreToolUse,
            Some("Bash"),
            &payload
        ));
    }

    #[test]
    fn test_bash_git_push_is_boundary() {
        let payload = json!({"tool_input": {"command": "git push origin main"}});
        assert!(is_evaluation_boundary(
            HookType::PreToolUse,
            Some("Bash"),
            &payload
        ));
    }

    #[test]
    fn test_bash_redirect_is_boundary() {
        let payload = json!({"tool_input": {"command": "echo 'data' > output.txt"}});
        assert!(is_evaluation_boundary(
            HookType::PreToolUse,
            Some("Bash"),
            &payload
        ));
    }

    #[test]
    fn test_bash_pnpm_install_is_boundary() {
        let payload = json!({"tool_input": {"command": "pnpm install lodash"}});
        assert!(is_evaluation_boundary(
            HookType::PreToolUse,
            Some("Bash"),
            &payload
        ));
    }

    #[test]
    fn test_bash_npm_ci_is_boundary() {
        let payload = json!({"tool_input": {"command": "npm ci"}});
        assert!(is_evaluation_boundary(
            HookType::PreToolUse,
            Some("Bash"),
            &payload
        ));
    }

    #[test]
    fn test_bash_git_status_is_not_boundary() {
        let payload = json!({"tool_input": {"command": "git status"}});
        assert!(!is_evaluation_boundary(
            HookType::PreToolUse,
            Some("Bash"),
            &payload
        ));
    }

    #[test]
    fn test_bash_git_log_is_not_boundary() {
        let payload = json!({"tool_input": {"command": "git log --oneline -5"}});
        assert!(!is_evaluation_boundary(
            HookType::PreToolUse,
            Some("Bash"),
            &payload
        ));
    }

    #[test]
    fn test_bash_git_diff_is_not_boundary() {
        let payload = json!({"tool_input": {"command": "git diff HEAD"}});
        assert!(!is_evaluation_boundary(
            HookType::PreToolUse,
            Some("Bash"),
            &payload
        ));
    }

    #[test]
    fn test_read_is_not_boundary() {
        assert!(!is_evaluation_boundary(
            HookType::PreToolUse,
            Some("Read"),
            &empty()
        ));
    }

    // ── Failure / Stop ──

    #[test]
    fn test_post_tool_failure_always_boundary() {
        assert!(is_evaluation_boundary(
            HookType::PostToolUseFailure,
            None,
            &empty()
        ));
        assert!(is_evaluation_boundary(
            HookType::PostToolUseFailure,
            Some("Edit"),
            &empty()
        ));
    }

    #[test]
    fn test_stop_always_boundary() {
        assert!(is_evaluation_boundary(HookType::Stop, None, &empty()));
    }

    // ── Session lifecycle (not boundaries) ──

    #[test]
    fn test_session_start_not_boundary() {
        assert!(!is_evaluation_boundary(
            HookType::SessionStart,
            None,
            &empty()
        ));
    }

    #[test]
    fn test_session_end_not_boundary() {
        assert!(!is_evaluation_boundary(
            HookType::SessionEnd,
            None,
            &empty()
        ));
    }

    #[test]
    fn test_pre_compact_not_boundary() {
        assert!(!is_evaluation_boundary(
            HookType::PreCompact,
            None,
            &empty()
        ));
    }

    #[test]
    fn test_pre_tool_no_tool_name_not_boundary() {
        assert!(!is_evaluation_boundary(
            HookType::PreToolUse,
            None,
            &empty()
        ));
    }

    // ── UserPromptSubmit ──

    #[test]
    fn test_short_prompt_not_boundary() {
        assert!(!is_evaluation_boundary(
            HookType::UserPromptSubmit,
            None,
            &prompt_payload("ok"),
        ));
    }

    #[test]
    fn test_empty_prompt_not_boundary() {
        assert!(!is_evaluation_boundary(
            HookType::UserPromptSubmit,
            None,
            &empty(),
        ));
    }

    #[test]
    fn test_substantial_prompt_is_boundary() {
        assert!(is_evaluation_boundary(
            HookType::UserPromptSubmit,
            None,
            &prompt_payload("make a plan to add a mortgage calculator"),
        ));
    }

    #[test]
    fn test_yes_not_boundary() {
        assert!(!is_evaluation_boundary(
            HookType::UserPromptSubmit,
            None,
            &prompt_payload("yes"),
        ));
    }

    #[test]
    fn test_continue_not_boundary() {
        assert!(!is_evaluation_boundary(
            HookType::UserPromptSubmit,
            None,
            &prompt_payload("continue"),
        ));
    }

    #[test]
    fn test_do_it_not_boundary() {
        assert!(!is_evaluation_boundary(
            HookType::UserPromptSubmit,
            None,
            &prompt_payload("do it"),
        ));
    }

    #[test]
    fn test_exactly_threshold_is_boundary() {
        let text = "a".repeat(MIN_PROMPT_LENGTH);
        assert!(is_evaluation_boundary(
            HookType::UserPromptSubmit,
            None,
            &prompt_payload(&text),
        ));
    }

    #[test]
    fn test_below_threshold_not_boundary() {
        let text = "a".repeat(MIN_PROMPT_LENGTH - 1);
        assert!(!is_evaluation_boundary(
            HookType::UserPromptSubmit,
            None,
            &prompt_payload(&text),
        ));
    }

    // ── Agent PreToolUse (launch) ──

    #[test]
    fn test_agent_launch_with_prompt_is_boundary() {
        assert!(is_evaluation_boundary(
            HookType::PreToolUse,
            Some("Agent"),
            &agent_pre_tool("Explore calculator structure", "Find all calculator modules"),
        ));
    }

    #[test]
    fn test_agent_launch_description_only_is_boundary() {
        assert!(is_evaluation_boundary(
            HookType::PreToolUse,
            Some("Agent"),
            &agent_pre_tool("Design lease calculator plan", ""),
        ));
    }

    #[test]
    fn test_agent_launch_empty_input_not_boundary() {
        assert!(!is_evaluation_boundary(
            HookType::PreToolUse,
            Some("Agent"),
            &empty(),
        ));
    }

    #[test]
    fn test_agent_launch_no_prompt_or_desc_not_boundary() {
        assert!(!is_evaluation_boundary(
            HookType::PreToolUse,
            Some("Agent"),
            &json!({"tool_input": {"subagent_type": "general-purpose"}}),
        ));
    }

    // ── Agent PostToolUse (return) ──

    #[test]
    fn test_agent_return_is_boundary() {
        assert!(is_evaluation_boundary(
            HookType::PostToolUse,
            Some("Agent"),
            &agent_post_tool("Explore calculator structure", "Found 3 calculator modules..."),
        ));
    }

    #[test]
    fn test_agent_return_empty_payload_still_boundary() {
        assert!(is_evaluation_boundary(
            HookType::PostToolUse,
            Some("Agent"),
            &empty(),
        ));
    }

    #[test]
    fn test_non_agent_post_tool_not_boundary() {
        assert!(!is_evaluation_boundary(
            HookType::PostToolUse,
            Some("Read"),
            &empty()
        ));
    }

    #[test]
    fn test_post_tool_no_tool_not_boundary() {
        assert!(!is_evaluation_boundary(
            HookType::PostToolUse,
            None,
            &empty()
        ));
    }

    // ── AskUserQuestion ──

    #[test]
    fn test_ask_user_question_is_boundary() {
        assert!(is_evaluation_boundary(
            HookType::PreToolUse,
            Some("AskUserQuestion"),
            &empty(),
        ));
    }

    #[test]
    fn test_ask_user_question_is_advisor_gated() {
        assert_eq!(
            classify_tool_action(Some("AskUserQuestion"), &empty()),
            ToolAction::AdvisorGated,
        );
    }
}
