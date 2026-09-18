//! Minimal Claude Code `PreToolUse` hook envelope for
//! `actual impl-check --claude-hook`.
//!
//! Unlike `plan-check`'s hook envelope (`plan_check_hook::HookEnvelope`),
//! there is no `tool_input.plan`-equivalent field carrying the material to
//! judge: the diff is always resolved via `working_tree_diff` (working tree
//! vs `HEAD`, including untracked non-ignored files; see
//! `impl_check::working_tree_diff`), never from the envelope itself, so this
//! envelope only needs to carry enough to key the revision-loop session.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_envelope_reads_session_id() {
        let env: HookEnvelope = serde_json::from_str(r#"{"session_id":"abc-123"}"#).unwrap();
        assert_eq!(env.session_id.as_deref(), Some("abc-123"));
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
