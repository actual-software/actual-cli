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

/// Hook-path classifier. Same decisions as [`classify_tool_action`], except a complex Bash
/// command the deterministic patterns call Free is put to kev. Falls back to v1 when kev is
/// absent or unreachable.
pub async fn classify_tool_action_v2(
    tool_name: Option<&str>,
    payload: &serde_json::Value,
    kev: Option<&super::kev::KevClient>,
) -> ToolAction {
    match (tool_name, classify_tool_action(tool_name, payload)) {
        (Some("Bash"), ToolAction::Free) if is_mutating_bash_v2(payload, kev).await => {
            ToolAction::AdvisorGated
        }
        (_, action) => action,
    }
}

/// Hook-path boundary check. Same as [`is_evaluation_boundary`], except ambiguous-length
/// prompts and complex Bash commands are put to kev. Falls back to v1 without kev.
pub async fn is_evaluation_boundary_v2(
    hook_type: HookType,
    tool_name: Option<&str>,
    payload: &serde_json::Value,
    kev: Option<&super::kev::KevClient>,
) -> bool {
    match (hook_type, tool_name) {
        (HookType::UserPromptSubmit, _) => is_substantial_prompt_v2(payload, kev).await,
        (HookType::PreToolUse, Some("Bash")) => is_mutating_bash_v2(payload, kev).await,
        _ => is_evaluation_boundary(hook_type, tool_name, payload),
    }
}

fn is_substantial_prompt(payload: &serde_json::Value) -> bool {
    let prompt = payload.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    prompt.len() >= MIN_PROMPT_LENGTH
}

const MUTATING_BASH_PATTERNS: &[&str] = &[
    "rm ",
    "rm\t",
    "rmdir",
    "mv ",
    "mv\t",
    "cp ",
    "cp\t",
    "chmod",
    "chown",
    "git commit",
    "git push",
    "git reset",
    "git checkout",
    "git merge",
    "git rebase",
    "git stash",
    "git cherry-pick",
    "git revert",
    "npm install",
    "npm uninstall",
    "npm ci",
    "pnpm install",
    "pnpm add",
    "pnpm remove",
    "yarn add",
    "yarn remove",
    "pip install",
    "pip uninstall",
    "uv add",
    "uv remove",
    "uv sync",
    "cargo add",
    "cargo install",
    "docker build",
    "docker push",
    "docker run",
    "kubectl apply",
    "kubectl delete",
    "make ",
    "curl -x post",
    "curl -x put",
    "curl -x delete",
    "curl -x patch",
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

// ---------------------------------------------------------------------------
// Kev-augmented variants — consult kev only for ambiguous cases
// ---------------------------------------------------------------------------

const PROMPT_AMBIGUOUS_LOW: usize = 10;
const PROMPT_AMBIGUOUS_HIGH: usize = 50;
const BASH_COMPLEXITY_THRESHOLD: usize = 80;

/// Kev-augmented substantial prompt check.
///
/// Fast-paths short (<10 chars) to false and long (>50 chars) to true.
/// Only consults kev for the ambiguous middle range.
pub async fn is_substantial_prompt_v2(
    payload: &serde_json::Value,
    kev: Option<&super::kev::KevClient>,
) -> bool {
    let prompt = payload.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    let len = prompt.len();

    if len < PROMPT_AMBIGUOUS_LOW {
        return false;
    }
    if len > PROMPT_AMBIGUOUS_HIGH {
        return true;
    }

    // Ambiguous range — consult kev if available
    if let Some(kev) = kev {
        match kev
            .noul(
                prompt,
                "Is this a substantive coding instruction that requires architectural review?",
            )
            .await
        {
            Ok(resp) => {
                tracing::info!(target: "kev", decision = "prompt", probability = resp.probability, "kev decided an ambiguous prompt");
                return resp.probability >= 0.5;
            }
            Err(super::kev::KevError::Unavailable(e)) => {
                tracing::info!(target: "kev", error = %e, "kev unavailable; deterministic prompt rule used");
            }
        }
    }

    prompt.len() >= MIN_PROMPT_LENGTH
}

/// Kev-augmented bash mutation check.
///
/// Deterministic check runs first (fast path). Kev consulted only for
/// complex commands (pipes, semicolons, >80 chars) where deterministic
/// returned Free.
pub async fn is_mutating_bash_v2(
    payload: &serde_json::Value,
    kev: Option<&super::kev::KevClient>,
) -> bool {
    // Fast path: deterministic check
    if is_mutating_bash(payload) {
        return true;
    }

    let command = payload
        .get("tool_input")
        .and_then(|v| v.get("command"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if command.is_empty() {
        return false;
    }

    // Only consult kev for complex commands the deterministic check may miss
    let is_complex =
        command.len() > BASH_COMPLEXITY_THRESHOLD || command.contains('|') || command.contains(';');

    if !is_complex {
        return false;
    }

    if let Some(kev) = kev {
        match kev
            .choice(
                command,
                "Classify this bash command's filesystem/repository impact",
                &[
                    (
                        "read_only",
                        "Only reads data, lists files, or queries state",
                    ),
                    (
                        "mutating",
                        "Modifies files, repository, packages, or infrastructure",
                    ),
                    ("ambiguous", "Could be either depending on flags or context"),
                ],
            )
            .await
        {
            Ok(resp) => {
                tracing::info!(target: "kev", decision = "bash", selected = %resp.selected, p_mutating = resp.distribution.get("mutating").copied().unwrap_or(0.0), margin = resp.confidence, "kev classified a complex bash command");
                // `confidence` is kev's top-two margin, not P(choice): a 51% "mutating"
                // answer has confidence ~0.27. Gate on the probability itself.
                let p_mutating = resp.distribution.get("mutating").copied().unwrap_or(0.0);
                return p_mutating >= 0.5;
            }
            Err(super::kev::KevError::Unavailable(e)) => {
                // Deterministic check already said Free.
                tracing::info!(target: "kev", error = %e, "kev unavailable; deterministic bash rule used");
            }
        }
    }

    false
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
            &agent_pre_tool(
                "Explore calculator structure",
                "Find all calculator modules"
            ),
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
            &agent_post_tool(
                "Explore calculator structure",
                "Found 3 calculator modules..."
            ),
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

    // ── Kev v2 variants (no kev = deterministic fallback) ──

    #[tokio::test]
    async fn test_v2_substantial_prompt_short_fast_path() {
        // < 10 chars → false regardless of kev
        let payload = prompt_payload("yes");
        assert!(!is_substantial_prompt_v2(&payload, None).await);
    }

    #[tokio::test]
    async fn test_v2_substantial_prompt_long_fast_path() {
        // > 50 chars → true regardless of kev
        let text = "refactor the entire authentication module to use OAuth2 with PKCE";
        let payload = prompt_payload(text);
        assert!(is_substantial_prompt_v2(&payload, None).await);
    }

    #[tokio::test]
    async fn test_v2_substantial_prompt_ambiguous_no_kev() {
        // 10-50 chars, no kev → falls back to >= 20 check
        let payload = prompt_payload("fix the auth bug"); // 16 chars
        assert!(!is_substantial_prompt_v2(&payload, None).await);
    }

    #[tokio::test]
    async fn test_v2_substantial_prompt_ambiguous_at_threshold() {
        // 10-50 chars, no kev, exactly at 20 → true
        let payload = prompt_payload("fix the auth bug now"); // 20 chars
        assert!(is_substantial_prompt_v2(&payload, None).await);
    }

    #[tokio::test]
    async fn test_v2_mutating_bash_deterministic_catches() {
        // rm is caught by deterministic check — kev not needed
        let payload = json!({"tool_input": {"command": "rm -rf dist/"}});
        assert!(is_mutating_bash_v2(&payload, None).await);
    }

    #[tokio::test]
    async fn test_v2_mutating_bash_simple_readonly() {
        // ls is simple and short — no kev consultation
        let payload = json!({"tool_input": {"command": "ls -la"}});
        assert!(!is_mutating_bash_v2(&payload, None).await);
    }

    #[tokio::test]
    async fn test_v2_mutating_bash_complex_no_kev() {
        // Complex (has pipe) but deterministic says free, no kev → false
        let payload = json!({"tool_input": {"command": "cat file.txt | grep pattern"}});
        assert!(!is_mutating_bash_v2(&payload, None).await);
    }

    #[tokio::test]
    async fn test_v2_mutating_bash_empty() {
        assert!(!is_mutating_bash_v2(&empty(), None).await);
    }

    // ── Hook-path wiring: v2 classifiers consult kev only where v1 is unsure ──

    async fn kev_mock(
        answer: serde_json::Value,
    ) -> (mockito::ServerGuard, super::super::kev::KevClient) {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/systemone")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({"model": "kev-latest", "answers": {"q": answer}}).to_string(),
            )
            .create_async()
            .await;
        let kev = super::super::kev::KevClient::with_base_url(
            &server.url(),
            std::time::Duration::from_secs(2),
        )
        .expect("client");
        (server, kev)
    }

    fn bash(command: &str) -> serde_json::Value {
        json!({"tool_name": "Bash", "tool_input": {"command": command}})
    }

    // A pipeline the deterministic patterns miss (no rm/mv/git commit/...).
    const SNEAKY_MUTATION: &str =
        "cat package.json | jq '.version = \"2.0.0\"' | sponge package.json";

    #[tokio::test]
    async fn classify_v2_without_kev_matches_v1() {
        for (tool, payload) in [
            (Some("Bash"), bash("ls -la")),
            (Some("Bash"), bash("rm -rf dist")),
            (Some("Bash"), bash(SNEAKY_MUTATION)),
            (Some("Edit"), json!({})),
            (Some("Read"), json!({})),
            (Some("AskUserQuestion"), json!({})),
        ] {
            assert_eq!(
                classify_tool_action_v2(tool, &payload, None).await,
                classify_tool_action(tool, &payload)
            );
        }
    }

    #[tokio::test]
    async fn classify_v2_gates_a_mutation_only_kev_catches() {
        let (_srv, kev) = kev_mock(
            json!({"type": "choice", "choice": "mutating", "confidence": 0.9,
            "probabilities": {"mutating": 0.93, "read_only": 0.05, "ambiguous": 0.02}}),
        )
        .await;
        assert_eq!(
            classify_tool_action(Some("Bash"), &bash(SNEAKY_MUTATION)),
            ToolAction::Free
        );
        assert_eq!(
            classify_tool_action_v2(Some("Bash"), &bash(SNEAKY_MUTATION), Some(&kev)).await,
            ToolAction::AdvisorGated
        );
    }

    #[tokio::test]
    async fn classify_v2_gates_on_mutating_probability_not_the_margin() {
        // Real kev answer for the jq|sponge rewrite: `confidence` is the top-two margin
        // (0.27), while P(mutating) is 0.51. Gating on the margin let it through.
        let (_srv, kev) = kev_mock(json!({"type": "choice", "choice": "mutating", "confidence": 0.2663,
            "probabilities": {"read_only": 0.234, "mutating": 0.511, "ambiguous": 0.256}})).await;
        assert_eq!(classify_tool_action_v2(Some("Bash"), &bash(SNEAKY_MUTATION), Some(&kev)).await, ToolAction::AdvisorGated);
    }

    #[tokio::test]
    async fn classify_v2_does_not_gate_when_mutating_is_unlikely() {
        let (_srv, kev) = kev_mock(json!({"type": "choice", "choice": "ambiguous", "confidence": 0.1,
            "probabilities": {"read_only": 0.35, "mutating": 0.2, "ambiguous": 0.45}})).await;
        assert_eq!(classify_tool_action_v2(Some("Bash"), &bash(SNEAKY_MUTATION), Some(&kev)).await, ToolAction::Free);
    }

    #[tokio::test]
    async fn classify_v2_leaves_read_only_pipelines_free() {
        let (_srv, kev) = kev_mock(
            json!({"type": "choice", "choice": "read_only", "confidence": 0.9,
            "probabilities": {"mutating": 0.04, "read_only": 0.94, "ambiguous": 0.02}}),
        )
        .await;
        let payload = bash("git log --oneline | head -20 | grep fix");
        assert_eq!(
            classify_tool_action_v2(Some("Bash"), &payload, Some(&kev)).await,
            ToolAction::Free
        );
    }

    #[tokio::test]
    async fn boundary_v2_lets_kev_decide_ambiguous_prompts() {
        let ambiguous = prompt_payload("fix the login bug please"); // 10..=50 chars
        let (_yes, kev_yes) = kev_mock(json!({"type": "noul", "noul": 0.91})).await;
        assert!(
            is_evaluation_boundary_v2(HookType::UserPromptSubmit, None, &ambiguous, Some(&kev_yes))
                .await
        );
        let (_no, kev_no) = kev_mock(json!({"type": "noul", "noul": 0.08})).await;
        assert!(
            !is_evaluation_boundary_v2(HookType::UserPromptSubmit, None, &ambiguous, Some(&kev_no))
                .await
        );
    }

    #[tokio::test]
    async fn boundary_v2_without_kev_matches_v1() {
        for (hook, tool, payload) in [
            (HookType::UserPromptSubmit, None, prompt_payload("hi")),
            (
                HookType::UserPromptSubmit,
                None,
                prompt_payload("fix the login bug please"),
            ),
            (HookType::PreToolUse, Some("Bash"), bash("rm -rf dist")),
            (HookType::PostToolUse, Some("Agent"), json!({})),
            (HookType::Stop, None, json!({})),
        ] {
            assert_eq!(
                is_evaluation_boundary_v2(hook, tool, &payload, None).await,
                is_evaluation_boundary(hook, tool, &payload)
            );
        }
    }

    #[tokio::test]
    async fn kev_down_falls_back_to_deterministic() {
        // Nothing listens on port 9; the client must fail fast and v1 must decide.
        let kev = super::super::kev::KevClient::with_base_url(
            "http://127.0.0.1:9",
            std::time::Duration::from_millis(500),
        )
        .unwrap();
        assert_eq!(
            classify_tool_action_v2(Some("Bash"), &bash(SNEAKY_MUTATION), Some(&kev)).await,
            ToolAction::Free
        );
        assert!(
            is_evaluation_boundary_v2(
                HookType::UserPromptSubmit,
                None,
                &prompt_payload("fix the login bug please"),
                Some(&kev)
            )
            .await
        );
    }
}
