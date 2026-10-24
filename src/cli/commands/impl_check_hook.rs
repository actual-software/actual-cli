//! Minimal Claude Code `PreToolUse` hook envelope for
//! `actual impl-check --claude-hook`.
//!
//! Unlike `plan-check`'s hook envelope (`plan_check_hook::HookEnvelope`),
//! there is no `tool_input.plan`-equivalent field carrying the material to
//! judge: the diff is always resolved from the working tree (including
//! untracked non-ignored files, against the commit the turn started from or
//! `HEAD`; see `impl_check::working_tree_diff_from`), never from the envelope
//! itself, so this envelope only needs to carry enough to key the
//! revision-loop session. A `UserPromptSubmit` envelope is read through the
//! same struct and, instead of a check, records the turn's starting commit.
//! Deliberately its own, smaller struct rather than reusing
//! `plan_check_hook::HookEnvelope`, which is coupled to `ToolInput` and other
//! plan-specific fields this command has no use for.

use serde::Deserialize;

/// The fields this module reads out of a `PreToolUse` hook envelope. Every
/// other field Claude Code sends (`cwd`, `permission_mode`, `tool_name`,
/// `tool_input`, `prompt_id`, ...) is ignored by construction — an
/// unrecognized field is simply absent from this struct rather than
/// rejected, so a newer hook envelope with additional fields still
/// deserializes, the same tolerance `plan_check_hook::HookEnvelope` gives.
///
/// `session_id` is the field the revision loop actually keys on (see
/// `crate::cli::commands::governance_session`, keyed on
/// `(session_id, rules_dir)`).
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct HookEnvelope {
    pub session_id: Option<String>,
    /// The subagent the envelope came from, absent for the main agent. Only
    /// the gate's telemetry reads it.
    pub agent_id: Option<String>,
    /// Which hook event delivered the envelope. `UserPromptSubmit` marks the
    /// start of a turn; anything else, or nothing, runs the check.
    pub hook_event_name: Option<String>,
}

impl HookEnvelope {
    /// Whether this envelope came from a `UserPromptSubmit` hook.
    pub fn is_turn_start(&self) -> bool {
        self.hook_event_name.as_deref() == Some("UserPromptSubmit")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_envelope_reads_session_id() {
        let env: HookEnvelope = serde_json::from_str(r#"{"session_id":"abc-123"}"#).unwrap();
        assert_eq!(env.session_id.as_deref(), Some("abc-123"));
    }

    /// A subagent's envelope names it, and the main agent's names none.
    #[test]
    fn test_envelope_reads_agent_id() {
        let env: HookEnvelope =
            serde_json::from_str(r#"{"session_id":"abc-123","agent_id":"sub-1"}"#).unwrap();
        assert_eq!(env.agent_id.as_deref(), Some("sub-1"));
        let env: HookEnvelope = serde_json::from_str(r#"{"session_id":"abc-123"}"#).unwrap();
        assert_eq!(env.agent_id, None);
    }

    #[test]
    fn test_envelope_is_turn_start_only_for_user_prompt_submit() {
        let parse = |json: &str| serde_json::from_str::<HookEnvelope>(json).unwrap();
        assert!(parse(r#"{"hook_event_name":"UserPromptSubmit"}"#).is_turn_start());
        assert!(!parse(r#"{"hook_event_name":"Stop"}"#).is_turn_start());
        assert!(!parse(r#"{"hook_event_name":"PreToolUse"}"#).is_turn_start());
        assert!(!parse("{}").is_turn_start());
    }

    #[test]
    fn test_envelope_session_id_absent_is_none() {
        let env: HookEnvelope = serde_json::from_str("{}").unwrap();
        assert_eq!(env.session_id, None);
    }

    /// The same tolerance `plan_check_hook::HookEnvelope` gives: fields this
    /// struct does not model (including `tool_input`, which this command has
    /// no use for at all) must not fail deserialization.
    #[test]
    fn test_envelope_ignores_fields_it_does_not_model() {
        let env: HookEnvelope = serde_json::from_str(
            r##"{"session_id":"s1","cwd":"/repo","permission_mode":"plan","hook_event_name":"PreToolUse","tool_name":"ExitPlanMode","tool_use_id":"t1","tool_input":{"plan":"# Plan"}}"##,
        )
        .unwrap();
        assert_eq!(env.session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn test_envelope_defaults_on_malformed_but_valid_json() {
        let env: HookEnvelope = serde_json::from_str(r#"{"unrelated": 1}"#).unwrap();
        assert_eq!(env.session_id, None);
    }
}
