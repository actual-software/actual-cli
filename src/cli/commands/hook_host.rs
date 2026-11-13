//! Which coding agent a hook-mode command is answering, and the output shapes
//! each one accepts.
//!
//! # Design
//!
//! `plan-check`, `impl-check`, `rules brief` and `session summary` each have a
//! hook mode that reads an envelope on stdin and prints at most one JSON line.
//! The checking itself is host-agnostic; what differs is the envelope and the
//! reply. This module owns the reply.
//!
//! **Claude Code** ignores unknown keys, so its notice carries `systemMessage`
//! both at the top level and inside `hookSpecificOutput` (see
//! [`plan_check_hook::render_notice`]), and its gates run on `PreToolUse`, so a
//! denial is `permissionDecision: "deny"`.
//!
//! **Codex** parses hook output strictly: an unknown key fails the hook run
//! ("hook returned invalid pre-tool-use JSON output", observed on Codex CLI
//! 0.167.0), so every Codex shape here carries exactly the keys Codex's own
//! schema allows. Both Codex gates run on `Stop` — Codex has no `ExitPlanMode`
//! tool, so the plan gate checks the plan when a Plan-mode turn ends — and a
//! `Stop` hook blocks with `{"decision":"block","reason":…}`, which Codex turns
//! into a continuation prompt carrying `reason`.
//!
//! **Status lines are Claude-only.** "No rule applies" and "nothing changed"
//! are information, not warnings. Claude Code shows a `systemMessage` dimly;
//! Codex shows it as a warning on every turn, so [`HookHost::status`] is
//! silence there. Anything that says a check did not run, or did not run in
//! full, stays a [`HookHost::notice`] on both hosts.

use super::plan_check_hook;

/// The coding agent whose hook envelope a command is reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookHost {
    Claude,
    Codex,
}

impl HookHost {
    /// The host named by a command's hook flags, or `None` in direct mode.
    /// The flags conflict in clap, so at most one is ever set; Claude wins the
    /// impossible tie only so the function is total.
    pub fn select(claude_hook: bool, codex_hook: bool) -> Option<Self> {
        match (claude_hook, codex_hook) {
            (true, _) => Some(Self::Claude),
            (false, true) => Some(Self::Codex),
            (false, false) => None,
        }
    }

    /// The gate's one blocking reply.
    ///
    /// No function in this module produces an approval: Claude Code's
    /// `allow` and Codex's `approve` have no constructor, so the gates can
    /// only block or stay out of the way.
    pub fn deny(self, reason: &str) -> String {
        match self {
            Self::Claude => plan_check_hook::render_deny(reason),
            Self::Codex => serde_json::json!({ "decision": "block", "reason": reason }).to_string(),
        }
    }

    /// A non-blocking message the user should see: a check that did not run,
    /// ran in part, or passed only because of an override or a round limit.
    pub fn notice(self, message: &str) -> String {
        match self {
            Self::Claude => plan_check_hook::render_notice(message),
            Self::Codex => serde_json::json!({ "systemMessage": message }).to_string(),
        }
    }

    /// A purely informational line, such as "no rule applies": a notice on
    /// Claude Code, silence on Codex. See the module doc.
    pub fn status(self, message: &str) -> Option<String> {
        match self {
            Self::Claude => Some(self.notice(message)),
            Self::Codex => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(json: &str) -> Vec<String> {
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        value.as_object().unwrap().keys().cloned().collect()
    }

    #[test]
    fn test_select_reads_the_flags() {
        assert_eq!(HookHost::select(true, false), Some(HookHost::Claude));
        assert_eq!(HookHost::select(false, true), Some(HookHost::Codex));
        assert_eq!(HookHost::select(false, false), None);
        assert_eq!(HookHost::select(true, true), Some(HookHost::Claude));
    }

    /// Claude Code keeps the shapes it has always had.
    #[test]
    fn test_claude_shapes_are_unchanged() {
        assert_eq!(
            HookHost::Claude.deny("no"),
            plan_check_hook::render_deny("no")
        );
        assert_eq!(
            HookHost::Claude.notice("hi"),
            plan_check_hook::render_notice("hi")
        );
        assert_eq!(
            HookHost::Claude.status("hi"),
            Some(plan_check_hook::render_notice("hi"))
        );
    }

    /// Codex's `Stop` block: exactly `decision` and `reason`.
    #[test]
    fn test_codex_deny_is_a_stop_block() {
        let out = HookHost::Codex.deny("Rule DA-1 conflicts.");
        assert_eq!(keys(&out), ["decision", "reason"]);
        let value: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["decision"], "block");
        assert_eq!(value["reason"], "Rule DA-1 conflicts.");
    }

    /// Codex rejects unknown keys, so its notice is `systemMessage` alone.
    #[test]
    fn test_codex_notice_has_only_system_message() {
        let out = HookHost::Codex.notice("did not run");
        assert_eq!(keys(&out), ["systemMessage"]);
        assert!(!out.contains("hookSpecificOutput"));
    }

    #[test]
    fn test_codex_status_is_silence() {
        assert_eq!(HookHost::Codex.status("No rule applies."), None);
    }
}
