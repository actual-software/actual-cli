use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    Silent,
    Inform,
    Warn,
    Block,
}

#[derive(Serialize)]
struct HookSpecificOutput {
    #[serde(rename = "hookEventName")]
    hook_event_name: String,
    #[serde(rename = "additionalContext")]
    additional_context: String,
}

#[derive(Serialize)]
struct HookOutputWithContext {
    #[serde(rename = "hookSpecificOutput")]
    hook_specific_output: HookSpecificOutput,
}

pub fn build_hook_output(disposition: Disposition, guidance: Option<&str>, hook_event_name: &str) -> String {
    match disposition {
        Disposition::Silent => "{}".to_string(),
        Disposition::Inform => {
            let context = guidance.unwrap_or("The advisor has informational guidance for this action.");
            serde_json::to_string(&HookOutputWithContext {
                hook_specific_output: HookSpecificOutput {
                    hook_event_name: hook_event_name.to_string(),
                    additional_context: context.to_string(),
                },
            })
            .unwrap_or_else(|_| "{}".to_string())
        }
        Disposition::Warn => {
            let context = guidance.unwrap_or(
                "WARNING: The advisor has identified architectural concerns with this action. Please review before proceeding.",
            );
            serde_json::to_string(&HookOutputWithContext {
                hook_specific_output: HookSpecificOutput {
                    hook_event_name: hook_event_name.to_string(),
                    additional_context: context.to_string(),
                },
            })
            .unwrap_or_else(|_| "{}".to_string())
        }
        Disposition::Block => {
            let base = guidance.unwrap_or(
                "ADVISORY BLOCK: The advisor recommends against this action based on normative architectural decisions.",
            );
            let context = format!(
                "{base}\n\nNote: This is an advisory block (v1). The action has not been prevented. Please consult with your team before proceeding."
            );
            serde_json::to_string(&HookOutputWithContext {
                hook_specific_output: HookSpecificOutput {
                    hook_event_name: hook_event_name.to_string(),
                    additional_context: context,
                },
            })
            .unwrap_or_else(|_| "{}".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_silent_returns_empty_json() {
        assert_eq!(build_hook_output(Disposition::Silent, None, "PreToolUse"), "{}");
    }

    #[test]
    fn test_silent_ignores_guidance() {
        assert_eq!(
            build_hook_output(Disposition::Silent, Some("ignored"), "PreToolUse"),
            "{}"
        );
    }

    #[test]
    fn test_inform_returns_additional_context() {
        let output = build_hook_output(Disposition::Inform, Some("Use JWT for auth"), "PostToolUse");
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            parsed["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap(),
            "Use JWT for auth"
        );
        assert_eq!(
            parsed["hookSpecificOutput"]["hookEventName"]
                .as_str()
                .unwrap(),
            "PostToolUse"
        );
    }

    #[test]
    fn test_inform_default_message() {
        let output = build_hook_output(Disposition::Inform, None, "UserPromptSubmit");
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert!(parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains("informational"));
        assert_eq!(
            parsed["hookSpecificOutput"]["hookEventName"]
                .as_str()
                .unwrap(),
            "UserPromptSubmit"
        );
    }

    #[test]
    fn test_warn_returns_additional_context() {
        let output = build_hook_output(Disposition::Warn, Some("Auth pattern violates ADR-7"), "PreToolUse");
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            parsed["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap(),
            "Auth pattern violates ADR-7"
        );
    }

    #[test]
    fn test_warn_default_message() {
        let output = build_hook_output(Disposition::Warn, None, "PreToolUse");
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert!(parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains("WARNING"));
    }

    #[test]
    fn test_block_returns_advisory_note() {
        let output =
            build_hook_output(Disposition::Block, Some("Direct DB access violates ADR-1"), "PreToolUse");
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let ctx = parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(ctx.contains("Direct DB access violates ADR-1"));
        assert!(ctx.contains("advisory block"));
    }

    #[test]
    fn test_block_default_message() {
        let output = build_hook_output(Disposition::Block, None, "PreToolUse");
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let ctx = parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(ctx.contains("ADVISORY BLOCK"));
        assert!(ctx.contains("advisory block"));
    }

    #[test]
    fn test_all_dispositions_produce_valid_json() {
        for disp in [
            Disposition::Silent,
            Disposition::Inform,
            Disposition::Warn,
            Disposition::Block,
        ] {
            let output = build_hook_output(disp, Some("test"), "PostToolUse");
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(&output);
            assert!(parsed.is_ok(), "disposition {:?} should produce valid JSON", disp);
        }
    }
}
