//! What a Codex `Stop` hook needs from the session rollout: whether the turn
//! that just ended was in Plan mode, and the plan it proposed.
//!
//! # Why the rollout
//!
//! Codex has no `ExitPlanMode` tool and its `Stop` envelope does not say which
//! collaboration mode the turn ran in. Observed on Codex CLI 0.167.0, a
//! Plan-mode turn's `Stop` envelope carries `permission_mode: "default"` and
//! `last_assistant_message: null`. The rollout at the envelope's
//! `transcript_path` does say, and it is already written when the hook runs:
//!
//! - an `event_msg` whose payload is `task_started` names the turn
//!   (`turn_id`) and its `collaboration_mode_kind` (`"plan"` or `"default"`);
//! - each plan is an `event_msg` `item_completed` whose `item.type` is
//!   `"Plan"` and whose `item.text` is the plan without its
//!   `<proposed_plan>` tags. A revision in the same turn adds another one, so
//!   the last is the current plan;
//! - the assistant's `final_answer` message (a `response_item`, which carries
//!   no `turn_id`) holds the plan inside `<proposed_plan>…</proposed_plan>`.
//!   It is the fallback when no `Plan` item was written, and is attributed to
//!   the turn by position: everything after that turn's `task_started` and
//!   before the next one;
//! - a `Stop` hook's block starts a continuation in the same turn, which the
//!   rollout marks with an `item_completed` whose `item.type` is
//!   `"HookPrompt"`. The agent may answer it with a revised plan, or with a
//!   final answer that proposes none (declining to revise, say). A plan the
//!   continuation did not re-propose is withdrawn: checking it again would
//!   block the turn over a plan nobody is proposing, round after round, until
//!   the round limit (observed on Codex CLI 0.167.0).
//!
//! Codex documents the rollout as "not a stable interface", so everything here
//! fails open: an unreadable file, a compressed (`.zst`) rollout, an unknown
//! line shape or a turn that never appears all read as "unknown", and the
//! callers then do nothing rather than guess.
//!
//! # Cost
//!
//! A rollout grows with the session and can run to many megabytes. It is
//! streamed line by line, never buffered whole, and only lines that mention
//! one of the markers above are parsed. [`MAX_SCAN_BYTES`] bounds the scan so
//! a pathological file cannot outlast the hook's timeout.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use serde::Deserialize;

/// How much of a rollout is scanned before giving up on it.
pub const MAX_SCAN_BYTES: u64 = 256 * 1024 * 1024;

/// What the rollout says about one turn.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TurnFacts {
    /// `Some(true)` when the turn ran in Plan mode, `Some(false)` in any other
    /// mode, `None` when the rollout never named the turn's mode.
    pub plan_mode: Option<bool>,
    /// The plan the turn is proposing as it ends, without its tags: the
    /// latest one, unless a continuation withdrew it.
    pub plan: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    payload: Option<Payload>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct Payload {
    #[serde(rename = "type")]
    kind: Option<String>,
    turn_id: Option<String>,
    collaboration_mode_kind: Option<String>,
    item: Option<Item>,
    role: Option<String>,
    phase: Option<String>,
    content: Option<Vec<Content>>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct Item {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct Content {
    text: Option<String>,
}

/// Read what the rollout at `path` says about `turn_id`. Never fails: see the
/// module doc.
pub fn turn_facts(path: &Path, turn_id: &str) -> TurnFacts {
    if turn_id.is_empty() || path.extension().is_some_and(|ext| ext == "zst") {
        return TurnFacts::default();
    }
    match std::fs::File::open(path) {
        Ok(file) => scan(file, turn_id),
        Err(_) => TurnFacts::default(),
    }
}

/// [`turn_facts`] over any reader, so the scan is testable without a file.
fn scan(source: impl Read, turn_id: &str) -> TurnFacts {
    scan_capped(source, turn_id, MAX_SCAN_BYTES)
}

/// [`scan`] with the byte cap as a parameter, so the cap is testable without
/// a quarter-gigabyte fixture.
fn scan_capped(source: impl Read, turn_id: &str, cap: u64) -> TurnFacts {
    let mut facts = TurnFacts::default();
    let mut in_turn = false;
    let mut plan: Option<String> = None;
    // A `Plan` item since the last continuation began: the final answer that
    // follows repeats it, and the item's text wins.
    let mut item_since_hook = false;
    // A continuation began since the last plan was proposed: a final answer
    // with no plan in it now withdraws that plan.
    let mut hook_since_plan = false;
    let mut scanned: u64 = 0;

    for line in BufReader::new(source).lines() {
        let Ok(line) = line else { break };
        scanned += line.len() as u64 + 1;
        if scanned > cap {
            break;
        }
        // Cheap prefilter: a line that mentions none of the markers cannot
        // matter, and most of a rollout is tool output.
        let task_started = line.contains("\"task_started\"");
        let item = line.contains("\"item_completed\"")
            && (line.contains("\"Plan\"") || line.contains("\"HookPrompt\""));
        let answer = in_turn && line.contains("\"final_answer\"");
        if !(task_started || item || answer) {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<Line>(&line) else {
            continue;
        };
        let Some(payload) = parsed.payload else {
            continue;
        };
        let this_turn = payload.turn_id.as_deref() == Some(turn_id);
        match (parsed.kind.as_deref(), payload.kind.as_deref()) {
            (Some("event_msg"), Some("task_started")) => {
                in_turn = this_turn;
                if this_turn {
                    facts.plan_mode = payload
                        .collaboration_mode_kind
                        .as_deref()
                        .map(|mode| mode == "plan");
                }
            }
            (Some("event_msg"), Some("item_completed")) if this_turn => {
                let Some(item) = payload.item else { continue };
                match item.kind.as_deref() {
                    Some("Plan") => {
                        if let Some(text) = item.text.filter(|text| !text.trim().is_empty()) {
                            plan = Some(text);
                            item_since_hook = true;
                            hook_since_plan = false;
                        }
                    }
                    Some("HookPrompt") => {
                        item_since_hook = false;
                        hook_since_plan = true;
                    }
                    _ => {}
                }
            }
            (Some("response_item"), Some("message"))
                if payload.role.as_deref() == Some("assistant")
                    && payload.phase.as_deref() == Some("final_answer") =>
            {
                let text: String = payload
                    .content
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|c| c.text)
                    .collect();
                match proposed_plan(&text) {
                    Some(answer_plan) => {
                        if !item_since_hook {
                            plan = Some(answer_plan);
                        }
                        hook_since_plan = false;
                    }
                    None if hook_since_plan => plan = None,
                    None => {}
                }
            }
            _ => {}
        }
    }
    facts.plan = plan;
    facts
}

/// The last `<proposed_plan>` block in `text`, without its tags.
pub fn proposed_plan(text: &str) -> Option<String> {
    const OPEN: &str = "<proposed_plan>";
    const CLOSE: &str = "</proposed_plan>";
    let start = text.rfind(OPEN)? + OPEN.len();
    let end = start + text[start..].find(CLOSE)?;
    let plan = text[start..end].trim();
    (!plan.is_empty()).then(|| plan.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TURN: &str = "01a1c301-6ddc-7472-aaa8-1a3eb266f254";

    fn task_started(turn: &str, mode: &str) -> String {
        serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":turn,"collaboration_mode_kind":mode}}).to_string()
    }

    fn plan_item(turn: &str, text: &str) -> String {
        serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","turn_id":turn,"item":{"type":"Plan","id":"p","text":text}}}).to_string()
    }

    fn answer(text: &str) -> String {
        serde_json::json!({"type":"response_item","payload":{"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":text}]}}).to_string()
    }

    /// The line a `Stop` block's continuation writes, as Codex CLI 0.167.0
    /// writes it.
    fn hook_prompt(turn: &str) -> String {
        serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","turn_id":turn,"item":{"type":"HookPrompt","id":"h","fragments":[{"text":"CONFLICT R-D-001","hook_run_id":"stop:4"}]}}}).to_string()
    }

    fn facts(lines: &[String]) -> TurnFacts {
        scan(lines.join("\n").as_bytes(), TURN)
    }

    /// The rollout Phase 0 captured, trimmed: a Plan-mode turn whose plan
    /// was blocked once and revised.
    #[test]
    fn test_reads_the_captured_plan_turn() {
        let fixture = include_str!("../../../tests/fixtures/codex/transcript-plan.jsonl");
        let facts = scan(fixture.as_bytes(), TURN);
        assert_eq!(facts.plan_mode, Some(true));
        let plan = facts.plan.unwrap();
        assert!(plan.starts_with("## Add `getUser(id)`"), "{plan}");
        assert!(
            plan.contains("calling `query()` from `src/db.ts`"),
            "the revision wins: {plan}"
        );
        assert!(!plan.contains("<proposed_plan>"));
    }

    #[test]
    fn test_reads_the_captured_default_turn() {
        let fixture = include_str!("../../../tests/fixtures/codex/transcript-default.jsonl");
        let facts = scan(fixture.as_bytes(), "01a1c2fe-a95b-7861-9799-a1738d81c1c9");
        assert_eq!(facts.plan_mode, Some(false));
        assert_eq!(facts.plan, None);
    }

    #[test]
    fn test_the_last_plan_item_wins() {
        let f = facts(&[
            task_started(TURN, "plan"),
            plan_item(TURN, "first"),
            plan_item(TURN, "second"),
        ]);
        assert_eq!(f.plan.as_deref(), Some("second"));
    }

    /// A plan item from another turn is not this turn's plan.
    #[test]
    fn test_ignores_other_turns() {
        let f = facts(&[
            task_started("other", "plan"),
            plan_item("other", "theirs"),
            answer("<proposed_plan>\ntheirs too\n</proposed_plan>"),
            task_started(TURN, "default"),
        ]);
        assert_eq!(f.plan_mode, Some(false));
        assert_eq!(f.plan, None);
    }

    /// No `Plan` item: the final answer's block is used, by position.
    #[test]
    fn test_falls_back_to_the_final_answer() {
        let f = facts(&[
            task_started(TURN, "plan"),
            answer("Here it is.\n<proposed_plan>\n# Plan\n1. Do it\n</proposed_plan>"),
        ]);
        assert_eq!(f.plan.as_deref(), Some("# Plan\n1. Do it"));
    }

    #[test]
    fn test_a_plan_item_beats_the_final_answer() {
        let f = facts(&[
            task_started(TURN, "plan"),
            answer("<proposed_plan>\nfrom answer\n</proposed_plan>"),
            plan_item(TURN, "from item"),
        ]);
        assert_eq!(f.plan.as_deref(), Some("from item"));
    }

    /// Blocked, the agent answered the continuation without a plan (it
    /// declined to revise): the plan is withdrawn, so the gate has nothing to
    /// check again. The shape observed live on Codex CLI 0.167.0, where the
    /// stale plan was otherwise blocked until the round limit.
    #[test]
    fn test_a_continuation_without_a_plan_withdraws_it() {
        let f = facts(&[
            task_started(TURN, "plan"),
            plan_item(TURN, "draft"),
            answer("<proposed_plan>\ndraft\n</proposed_plan>"),
            hook_prompt(TURN),
            answer("That plan conflicts with the rules, so I won't propose it."),
        ]);
        assert_eq!(f.plan_mode, Some(true));
        assert_eq!(f.plan, None);
    }

    /// A continuation that revises the plan, as a `Plan` item or only in its
    /// final answer, is checked on the revision.
    #[test]
    fn test_a_continuation_that_revises_is_checked_on_the_revision() {
        let draft = [
            task_started(TURN, "plan"),
            plan_item(TURN, "draft"),
            answer("<proposed_plan>\ndraft\n</proposed_plan>"),
            hook_prompt(TURN),
        ];
        let mut by_item = draft.to_vec();
        by_item.push(plan_item(TURN, "revised"));
        by_item.push(answer("<proposed_plan>\nrevised\n</proposed_plan>"));
        assert_eq!(facts(&by_item).plan.as_deref(), Some("revised"));

        let mut by_answer = draft.to_vec();
        by_answer.push(answer(
            "<proposed_plan>\nrevised in the answer\n</proposed_plan>",
        ));
        assert_eq!(
            facts(&by_answer).plan.as_deref(),
            Some("revised in the answer")
        );

        // Withdrawn on one continuation, proposed again on the next.
        let mut again = draft.to_vec();
        again.push(answer("No plan this time."));
        again.push(hook_prompt(TURN));
        again.push(answer("<proposed_plan>\nback\n</proposed_plan>"));
        assert_eq!(facts(&again).plan.as_deref(), Some("back"));
    }

    /// Without a continuation, a final answer that drops the tags does not
    /// withdraw the turn's `Plan` item; and another turn's continuation is
    /// not this turn's.
    #[test]
    fn test_only_this_turns_continuation_withdraws_a_plan() {
        let f = facts(&[
            task_started(TURN, "plan"),
            plan_item(TURN, "kept"),
            answer("The plan is above."),
        ]);
        assert_eq!(f.plan.as_deref(), Some("kept"));

        let f = facts(&[
            task_started(TURN, "plan"),
            plan_item(TURN, "kept"),
            hook_prompt("other"),
            answer("Still the plan above."),
        ]);
        assert_eq!(f.plan.as_deref(), Some("kept"));
    }

    /// A blank `Plan` item and a final answer with no block leave no plan.
    #[test]
    fn test_blank_and_tagless_plans_are_no_plan() {
        let f = facts(&[
            task_started(TURN, "plan"),
            plan_item(TURN, "   "),
            answer("No block here."),
        ]);
        assert_eq!(f.plan_mode, Some(true));
        assert_eq!(f.plan, None);
    }

    /// A turn that never started in this rollout, or names no mode, is
    /// unknown rather than "not Plan mode".
    #[test]
    fn test_an_unnamed_mode_is_unknown() {
        assert_eq!(facts(&[]).plan_mode, None);
        let no_mode = serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":TURN}}).to_string();
        assert_eq!(facts(&[no_mode]).plan_mode, None);
    }

    /// Malformed lines, lines with no payload and unrelated markers are
    /// skipped without stopping the scan.
    #[test]
    fn test_skips_lines_it_cannot_use() {
        let f = facts(&[
            "{not json \"task_started\"".to_string(),
            r#"{"type":"event_msg","note":"task_started"}"#.to_string(),
            r#"{"type":"turn_context","payload":{"type":"task_started_elsewhere","x":"task_started"}}"#.to_string(),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","turn_id":TURN,"item":{"type":"AgentMessage","text":"Plan"}}}).to_string(),
            task_started(TURN, "plan"),
            plan_item(TURN, "kept"),
        ]);
        assert_eq!(f.plan_mode, Some(true));
        assert_eq!(f.plan.as_deref(), Some("kept"));
    }

    #[test]
    fn test_stops_at_the_scan_cap() {
        let lines = ["x".repeat(64), task_started(TURN, "plan")].join("\n");
        assert_eq!(
            scan_capped(lines.as_bytes(), TURN, 32),
            TurnFacts::default()
        );
        assert_eq!(
            scan_capped(lines.as_bytes(), TURN, 4096).plan_mode,
            Some(true)
        );
    }

    #[test]
    fn test_stops_at_an_unreadable_line() {
        // Invalid UTF-8 ends `lines()` with an error, which ends the scan.
        let mut bytes = task_started(TURN, "plan").into_bytes();
        bytes.extend_from_slice(b"\n\xff\xfe\n");
        bytes.extend_from_slice(plan_item(TURN, "after").as_bytes());
        let f = scan(bytes.as_slice(), TURN);
        assert_eq!(f.plan_mode, Some(true));
        assert_eq!(f.plan, None);
    }

    #[test]
    fn test_turn_facts_reads_a_file_and_fails_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        std::fs::write(
            &path,
            [task_started(TURN, "plan"), plan_item(TURN, "p")].join("\n"),
        )
        .unwrap();
        assert_eq!(turn_facts(&path, TURN).plan.as_deref(), Some("p"));

        assert_eq!(
            turn_facts(&dir.path().join("missing.jsonl"), TURN),
            TurnFacts::default()
        );
        assert_eq!(turn_facts(&path, ""), TurnFacts::default());
        let zst = dir.path().join("rollout.jsonl.zst");
        std::fs::write(&zst, "compressed").unwrap();
        assert_eq!(turn_facts(&zst, TURN), TurnFacts::default());
    }

    #[test]
    fn test_proposed_plan_takes_the_last_closed_block() {
        assert_eq!(
            proposed_plan("<proposed_plan>a</proposed_plan> <proposed_plan>\nb\n</proposed_plan>")
                .as_deref(),
            Some("b")
        );
        assert_eq!(proposed_plan("<proposed_plan>unclosed"), None);
        assert_eq!(proposed_plan("<proposed_plan>  </proposed_plan>"), None);
        assert_eq!(proposed_plan("none"), None);
    }
}
