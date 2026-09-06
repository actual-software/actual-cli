//! The Claude Code `PreToolUse` hook envelope: plan resolution and the hook's
//! own JSON output contract, for `actual plan-check --claude-hook`.
//!
//! # Design
//!
//! `hooks/plan-gate.sh` deliberately never parses JSON — see its own
//! comments — so this is the one place the hook envelope is actually read.
//! Everything here follows the contract fixed in `skills/actual/SKILL.md` and
//! exercised by the fixtures under `hooks/tests/fixtures/` in the
//! `actual-skill` plugin repository:
//!
//! 1. `tool_input.plan`, if non-empty. Current Claude Code injects the plan
//!    here before hooks run, even when the model's literal call omitted it.
//! 2. `tool_input.planFilePath`, if set and readable. Same injection.
//! 3. The transcript fallback: `prompt_id` + `transcript_path` locate the
//!    `plan_mode` attachment's `planFilePath`. Only reached when both of the
//!    above are absent — reading the transcript pulls conversation prose off
//!    disk that injected `tool_input` avoids, so it is the last resort, not
//!    the first choice (see AK-680).
//! 4. None of those: the caller fails open. There is no fifth step.
//!
//! Every read here is capped at [`MAX_READ_BYTES`] and best-effort: a missing
//! file, a malformed line, or an oversized transcript degrades to "no plan
//! resolved" rather than an error, because the caller's whole reason for
//! being here is that fail-open is mandatory.

use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Cap on any single file this module reads: a plan file, or the bytes
/// scanned out of a transcript. Mirrors
/// `crate::rules::discover::MAX_RULE_FILE_SIZE` — a file this large is a data
/// problem the hook's latency budget cannot absorb, not something to buffer
/// in full and hope.
pub const MAX_READ_BYTES: u64 = 1024 * 1024;

/// The fields this module reads out of a `PreToolUse` hook envelope. Every
/// other field Claude Code sends (`cwd`, `permission_mode`, `tool_name`, ...)
/// is ignored by construction: an unrecognized field is simply absent from
/// this struct rather than rejected, so a newer hook envelope with additional
/// fields still deserializes.
///
/// `session_id` is read (unlike the rest of the ignored fields) because it is
/// the identity the revision loop keys its state on — see
/// `crate::cli::commands::plan_check_session`. It is optional: an envelope
/// that omits it (an older Claude Code build) simply never engages the
/// loop/override/round-limit features, the same fail-open posture as every
/// other hook-only behavior in this module.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct HookEnvelope {
    pub session_id: Option<String>,
    pub prompt_id: Option<String>,
    pub transcript_path: Option<String>,
    pub tool_input: Option<ToolInput>,
}

/// `tool_input` uses `planFilePath` because that key comes from the
/// `ExitPlanMode` tool's own input schema, not the envelope's snake_case
/// convention — a legitimate single exception, not grounds for a per-field
/// rename on every field.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct ToolInput {
    pub plan: Option<String>,
    #[serde(rename = "planFilePath")]
    pub plan_file_path: Option<String>,
}

/// One line of the transcript JSONL this module cares about. Transcript
/// entries have several shapes (`user`, `assistant`, `attachment`, ...);
/// this covers only the two that matter for locating the plan.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct TranscriptEntry {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "promptId")]
    prompt_id: Option<String>,
    attachment: Option<TranscriptAttachment>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct TranscriptAttachment {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "planFilePath")]
    plan_file_path: Option<String>,
    #[serde(rename = "planExists")]
    plan_exists: Option<bool>,
}

/// Which step of the priority order produced the plan text. Exposed for
/// tests and diagnostics; the caller does not otherwise branch on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanSource {
    ToolInputPlan,
    ToolInputFile,
    Transcript,
}

/// Resolve plan text from a hook envelope, trying each step of the priority
/// order in turn. `None` means every step came up empty — the fail-open case,
/// which the caller must never treat as a violation.
///
/// INVARIANT: every fallible step here degrades to `None` or falls through to
/// the next step. Nothing in this function can return an `Err`, by
/// construction, because there is nothing a `--claude-hook` caller could do
/// with one other than fail open anyway.
pub fn resolve_plan(envelope: &HookEnvelope) -> Option<(String, PlanSource)> {
    if let Some(tool_input) = &envelope.tool_input {
        if let Some(plan) = &tool_input.plan {
            if !plan.trim().is_empty() {
                return Some((plan.clone(), PlanSource::ToolInputPlan));
            }
        }
        if let Some(path) = &tool_input.plan_file_path {
            if let Some(text) = read_capped(Path::new(path)) {
                return Some((text, PlanSource::ToolInputFile));
            }
        }
    }
    if let (Some(prompt_id), Some(transcript_path)) =
        (&envelope.prompt_id, &envelope.transcript_path)
    {
        if let Some(plan_file) = find_plan_file_in_transcript(Path::new(transcript_path), prompt_id)
        {
            if let Some(text) = read_capped(&plan_file) {
                return Some((text, PlanSource::Transcript));
            }
        }
    }
    None
}

/// Read a file, refusing anything empty or over [`MAX_READ_BYTES`].
///
/// The read itself is capped at one byte past [`MAX_READ_BYTES`] via
/// `Read::take`, the same pattern `crate::rules::discover::read_rule_file`
/// uses, rather than checking `metadata().len()` first and then reading the
/// whole file: a stat-then-read has a gap between the two — a file that
/// grows after the check (or a sparse file whose reported length understates
/// what a read produces) would still be buffered in full despite the check
/// having passed.
fn read_capped(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_READ_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_READ_BYTES {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Scan a transcript JSONL for the `plan_mode` attachment's `planFilePath`.
///
/// The attachment entry carries no `promptId` of its own in the observed
/// fixtures — only the `user` entry that precedes it does — so correlation is
/// done by tracking the most recently seen user `promptId` while scanning.
/// An attachment seen right after a `user` entry whose id matches
/// `target_prompt_id` is preferred; if none ever matches, the last
/// `plan_mode` attachment in the transcript is used, since the transcript is
/// append-only and the latest plan is the one this `ExitPlanMode` call is for.
///
/// Streamed line by line and capped at [`MAX_READ_BYTES`] scanned, so a
/// large transcript is never buffered whole — this is also why only the
/// `promptId` and attachment fields are ever read out of a line: the
/// message prose itself is never inspected (see AK-680).
fn find_plan_file_in_transcript(path: &Path, target_prompt_id: &str) -> Option<PathBuf> {
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);

    let mut last_user_prompt_id: Option<String> = None;
    let mut correlated: Option<String> = None;
    let mut any: Option<String> = None;
    let mut bytes_scanned: u64 = 0;

    for line in reader.lines() {
        let Ok(line) = line else { break };
        bytes_scanned += line.len() as u64 + 1;
        if bytes_scanned > MAX_READ_BYTES {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<TranscriptEntry>(line) else {
            continue;
        };
        if entry.kind.as_deref() == Some("user") {
            if let Some(id) = entry.prompt_id {
                last_user_prompt_id = Some(id);
            }
            continue;
        }
        if entry.kind.as_deref() != Some("attachment") {
            continue;
        }
        let Some(attachment) = entry.attachment else {
            continue;
        };
        if attachment.kind.as_deref() != Some("plan_mode") {
            continue;
        }
        if attachment.plan_exists == Some(false) {
            continue;
        }
        let Some(plan_file_path) = attachment.plan_file_path else {
            continue;
        };
        any = Some(plan_file_path.clone());
        if last_user_prompt_id.as_deref() == Some(target_prompt_id) {
            correlated = Some(plan_file_path);
        }
    }

    correlated.or(any).map(PathBuf::from)
}

// ── the hook's own JSON output contract ─────────────────────────────────

/// The name Claude Code's hook protocol expects on every `PreToolUse`
/// response this module produces.
const HOOK_EVENT_NAME: &str = "PreToolUse";

#[derive(Debug, Serialize)]
struct HookSpecificDeny {
    #[serde(rename = "hookEventName")]
    hook_event_name: &'static str,
    #[serde(rename = "permissionDecision")]
    permission_decision: &'static str,
    #[serde(rename = "permissionDecisionReason")]
    permission_decision_reason: String,
}

#[derive(Debug, Serialize)]
struct DenyOutput {
    #[serde(rename = "hookSpecificOutput")]
    hook_specific_output: HookSpecificDeny,
}

/// Render the hook's one blocking shape: `permissionDecision: "deny"`.
///
/// `allow` is never rendered anywhere in this module — there is no function
/// that produces it — which is the structural guarantee behind "never emit
/// `permissionDecision: allow`": the wrong value cannot be a typo away.
pub fn render_deny(reason: &str) -> String {
    let payload = DenyOutput {
        hook_specific_output: HookSpecificDeny {
            hook_event_name: HOOK_EVENT_NAME,
            permission_decision: "deny",
            permission_decision_reason: reason.to_string(),
        },
    };
    serde_json::to_string(&payload).expect("deny payload is serializable")
}

/// Render a non-blocking advisory: a `systemMessage` with no
/// `permissionDecision`, so the normal approval flow stays intact. Mirrors
/// `emit_pretooluse_notice` in `hooks/lib/bootstrap.sh` exactly, including
/// the duplicated top-level and nested `systemMessage` — that plugin's own
/// comment explains why: the documented location has moved between Claude
/// Code versions, and unknown fields are ignored, so carrying both is strictly
/// safer than picking one.
pub fn render_notice(message: &str) -> String {
    serde_json::json!({
        "systemMessage": message,
        "hookSpecificOutput": {
            "hookEventName": HOOK_EVENT_NAME,
            "systemMessage": message,
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::tempdir;

    fn envelope(json: &str) -> HookEnvelope {
        serde_json::from_str(json).unwrap()
    }

    // ── resolve_plan: tool_input.plan ───────────────────────────────────

    #[test]
    fn test_resolve_plan_prefers_tool_input_plan() {
        let env = envelope(
            r##"{"tool_input":{"plan":"# Do the thing","planFilePath":"/does/not/exist"}}"##,
        );
        let (plan, source) = resolve_plan(&env).unwrap();
        assert_eq!(plan, "# Do the thing");
        assert_eq!(source, PlanSource::ToolInputPlan);
    }

    #[test]
    fn test_resolve_plan_ignores_a_blank_tool_input_plan() {
        let dir = tempdir().unwrap();
        let plan_file = dir.path().join("plan.md");
        std::fs::write(&plan_file, "# From file").unwrap();
        let env = envelope(&format!(
            r#"{{"tool_input":{{"plan":"   ","planFilePath":"{}"}}}}"#,
            plan_file.display()
        ));
        let (plan, source) = resolve_plan(&env).unwrap();
        assert_eq!(plan, "# From file");
        assert_eq!(source, PlanSource::ToolInputFile);
    }

    // ── resolve_plan: tool_input.planFilePath ───────────────────────────

    #[test]
    fn test_resolve_plan_falls_back_to_plan_file_path() {
        let dir = tempdir().unwrap();
        let plan_file = dir.path().join("plan.md");
        std::fs::write(&plan_file, "# Add caching").unwrap();
        let env = envelope(&format!(
            r#"{{"tool_input":{{"planFilePath":"{}"}}}}"#,
            plan_file.display()
        ));
        let (plan, source) = resolve_plan(&env).unwrap();
        assert_eq!(plan, "# Add caching");
        assert_eq!(source, PlanSource::ToolInputFile);
    }

    #[test]
    fn test_resolve_plan_none_when_plan_file_path_is_unreadable() {
        let env = envelope(r#"{"tool_input":{"planFilePath":"/no/such/file.md"}}"#);
        assert!(resolve_plan(&env).is_none());
    }

    /// `read_capped` must actually refuse an oversized file rather than
    /// buffering it in full: it is capped via `Read::take`, not a
    /// stat-then-read, precisely so a file that exceeds `MAX_READ_BYTES`
    /// never gets fully read into memory before being rejected.
    #[test]
    fn test_resolve_plan_none_when_plan_file_path_exceeds_max_read_bytes() {
        let dir = tempdir().unwrap();
        let plan_file = dir.path().join("huge.md");
        let oversized = vec![b'x'; (MAX_READ_BYTES + 1) as usize];
        std::fs::write(&plan_file, &oversized).unwrap();
        let env = envelope(&format!(
            r#"{{"tool_input":{{"planFilePath":"{}"}}}}"#,
            plan_file.display()
        ));
        assert!(resolve_plan(&env).is_none());
    }

    // ── resolve_plan: transcript fallback ───────────────────────────────

    #[test]
    fn test_resolve_plan_falls_back_to_the_transcript() {
        let dir = tempdir().unwrap();
        let plan_file = dir.path().join("plan.md");
        std::fs::write(&plan_file, "# From transcript").unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        std::fs::write(
            &transcript,
            format!(
                "{{\"type\":\"user\",\"promptId\":\"abc-123\"}}\n\
                 {{\"type\":\"attachment\",\"attachment\":{{\"type\":\"plan_mode\",\"planFilePath\":\"{}\",\"planExists\":true}}}}\n",
                plan_file.display()
            ),
        )
        .unwrap();
        let env = envelope(&format!(
            r#"{{"prompt_id":"abc-123","transcript_path":"{}","tool_input":{{}}}}"#,
            transcript.display()
        ));
        let (plan, source) = resolve_plan(&env).unwrap();
        assert_eq!(plan, "# From transcript");
        assert_eq!(source, PlanSource::Transcript);
    }

    /// The transcript fallback is only reached when both `tool_input` sources
    /// are absent — reading it otherwise would pull conversation prose off
    /// disk for no reason (AK-680).
    #[test]
    fn test_resolve_plan_does_not_read_the_transcript_when_tool_input_plan_is_present() {
        let dir = tempdir().unwrap();
        // A transcript that would resolve to something different, to prove it
        // was never consulted.
        let transcript = dir.path().join("transcript.jsonl");
        std::fs::write(&transcript, "not even valid JSONL, and that's fine\n").unwrap();
        let env = envelope(&format!(
            r##"{{"prompt_id":"abc-123","transcript_path":"{}","tool_input":{{"plan":"# Inline plan"}}}}"##,
            transcript.display()
        ));
        let (plan, source) = resolve_plan(&env).unwrap();
        assert_eq!(plan, "# Inline plan");
        assert_eq!(source, PlanSource::ToolInputPlan);
    }

    #[test]
    fn test_resolve_plan_transcript_prefers_the_entry_correlated_by_prompt_id() {
        let dir = tempdir().unwrap();
        let wrong = dir.path().join("wrong.md");
        std::fs::write(&wrong, "# Wrong plan").unwrap();
        let right = dir.path().join("right.md");
        std::fs::write(&right, "# Right plan").unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        std::fs::write(
            &transcript,
            format!(
                "{{\"type\":\"user\",\"promptId\":\"other-prompt\"}}\n\
                 {{\"type\":\"attachment\",\"attachment\":{{\"type\":\"plan_mode\",\"planFilePath\":\"{}\",\"planExists\":true}}}}\n\
                 {{\"type\":\"user\",\"promptId\":\"target-prompt\"}}\n\
                 {{\"type\":\"attachment\",\"attachment\":{{\"type\":\"plan_mode\",\"planFilePath\":\"{}\",\"planExists\":true}}}}\n",
                wrong.display(),
                right.display(),
            ),
        )
        .unwrap();
        let env = envelope(&format!(
            r#"{{"prompt_id":"target-prompt","transcript_path":"{}","tool_input":{{}}}}"#,
            transcript.display()
        ));
        let (plan, _) = resolve_plan(&env).unwrap();
        assert_eq!(plan, "# Right plan");
    }

    #[test]
    fn test_resolve_plan_transcript_falls_back_to_the_last_attachment_with_no_correlation() {
        let dir = tempdir().unwrap();
        let first = dir.path().join("first.md");
        std::fs::write(&first, "# First").unwrap();
        let last = dir.path().join("last.md");
        std::fs::write(&last, "# Last").unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        std::fs::write(
            &transcript,
            format!(
                "{{\"type\":\"attachment\",\"attachment\":{{\"type\":\"plan_mode\",\"planFilePath\":\"{}\",\"planExists\":true}}}}\n\
                 {{\"type\":\"attachment\",\"attachment\":{{\"type\":\"plan_mode\",\"planFilePath\":\"{}\",\"planExists\":true}}}}\n",
                first.display(),
                last.display(),
            ),
        )
        .unwrap();
        // No promptId anywhere in the transcript ever matches; the last
        // attachment wins.
        let env = envelope(&format!(
            r#"{{"prompt_id":"never-seen","transcript_path":"{}","tool_input":{{}}}}"#,
            transcript.display()
        ));
        let (plan, _) = resolve_plan(&env).unwrap();
        assert_eq!(plan, "# Last");
    }

    #[test]
    fn test_resolve_plan_transcript_skips_an_attachment_whose_plan_does_not_exist() {
        let dir = tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        std::fs::write(
            &transcript,
            "{\"type\":\"attachment\",\"attachment\":{\"type\":\"plan_mode\",\"planFilePath\":\"/no/such/file\",\"planExists\":false}}\n",
        )
        .unwrap();
        let env = envelope(&format!(
            r#"{{"prompt_id":"x","transcript_path":"{}","tool_input":{{}}}}"#,
            transcript.display()
        ));
        assert!(resolve_plan(&env).is_none());
    }

    #[test]
    fn test_resolve_plan_transcript_ignores_malformed_lines() {
        let dir = tempdir().unwrap();
        let plan_file = dir.path().join("plan.md");
        std::fs::write(&plan_file, "# Ok plan").unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        std::fs::write(
            &transcript,
            format!(
                "not json at all\n\n{{\"type\":\"attachment\",\"attachment\":{{\"type\":\"plan_mode\",\"planFilePath\":\"{}\",\"planExists\":true}}}}\n",
                plan_file.display()
            ),
        )
        .unwrap();
        let env = envelope(&format!(
            r#"{{"prompt_id":"x","transcript_path":"{}","tool_input":{{}}}}"#,
            transcript.display()
        ));
        let (plan, _) = resolve_plan(&env).unwrap();
        assert_eq!(plan, "# Ok plan");
    }

    /// The transcript correlates to a real `planFilePath`, but the file at
    /// that path is empty — `find_plan_file_in_transcript` succeeds while
    /// `read_capped` on its result still fails, which are two different
    /// failure points that must both degrade to `None`.
    #[test]
    fn test_resolve_plan_transcript_found_a_path_but_could_not_read_it() {
        let dir = tempdir().unwrap();
        let empty_plan = dir.path().join("empty-plan.md");
        std::fs::write(&empty_plan, "   \n").unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        std::fs::write(
            &transcript,
            format!(
                "{{\"type\":\"attachment\",\"attachment\":{{\"type\":\"plan_mode\",\"planFilePath\":\"{}\",\"planExists\":true}}}}\n",
                empty_plan.display()
            ),
        )
        .unwrap();
        let env = envelope(&format!(
            r#"{{"prompt_id":"x","transcript_path":"{}","tool_input":{{}}}}"#,
            transcript.display()
        ));
        assert!(resolve_plan(&env).is_none());
    }

    #[test]
    fn test_read_capped_none_for_a_whitespace_only_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("blank.md");
        std::fs::write(&file, "   \n\t\n").unwrap();
        assert!(read_capped(&file).is_none());
    }

    /// The scan must actually stop at [`MAX_READ_BYTES`] rather than only
    /// documenting that it does: a transcript this large is streamed, never
    /// buffered whole.
    #[test]
    fn test_find_plan_file_in_transcript_stops_scanning_past_max_read_bytes() {
        let dir = tempdir().unwrap();
        let plan_file = dir.path().join("plan.md");
        std::fs::write(&plan_file, "# Never reached").unwrap();
        let transcript = dir.path().join("transcript.jsonl");

        // Pad the transcript past the scan cap with an oversized, otherwise
        // harmless line before the real attachment, so the attachment is
        // never reached if the cap is honored.
        let mut contents = String::new();
        let filler = "{\"type\":\"user\",\"promptId\":\"filler\"}".to_string();
        let padding_line = format!("{filler}{}\n", " ".repeat(MAX_READ_BYTES as usize));
        contents.push_str(&padding_line);
        contents.push_str(&format!(
            "{{\"type\":\"attachment\",\"attachment\":{{\"type\":\"plan_mode\",\"planFilePath\":\"{}\",\"planExists\":true}}}}\n",
            plan_file.display()
        ));
        std::fs::write(&transcript, contents).unwrap();

        let env = envelope(&format!(
            r#"{{"prompt_id":"x","transcript_path":"{}","tool_input":{{}}}}"#,
            transcript.display()
        ));
        assert!(resolve_plan(&env).is_none());
    }

    #[test]
    fn test_find_plan_file_in_transcript_ignores_a_non_user_non_attachment_entry() {
        let dir = tempdir().unwrap();
        let plan_file = dir.path().join("plan.md");
        std::fs::write(&plan_file, "# Ok plan").unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        std::fs::write(
            &transcript,
            format!(
                "{{\"type\":\"assistant\"}}\n{{\"type\":\"attachment\",\"attachment\":{{\"type\":\"plan_mode\",\"planFilePath\":\"{}\",\"planExists\":true}}}}\n",
                plan_file.display()
            ),
        )
        .unwrap();
        let env = envelope(&format!(
            r#"{{"prompt_id":"x","transcript_path":"{}","tool_input":{{}}}}"#,
            transcript.display()
        ));
        let (plan, _) = resolve_plan(&env).unwrap();
        assert_eq!(plan, "# Ok plan");
    }

    #[test]
    fn test_find_plan_file_in_transcript_ignores_an_attachment_entry_with_no_attachment_field() {
        let dir = tempdir().unwrap();
        let plan_file = dir.path().join("plan.md");
        std::fs::write(&plan_file, "# Ok plan").unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        std::fs::write(
            &transcript,
            format!(
                "{{\"type\":\"attachment\"}}\n{{\"type\":\"attachment\",\"attachment\":{{\"type\":\"plan_mode\",\"planFilePath\":\"{}\",\"planExists\":true}}}}\n",
                plan_file.display()
            ),
        )
        .unwrap();
        let env = envelope(&format!(
            r#"{{"prompt_id":"x","transcript_path":"{}","tool_input":{{}}}}"#,
            transcript.display()
        ));
        let (plan, _) = resolve_plan(&env).unwrap();
        assert_eq!(plan, "# Ok plan");
    }

    #[test]
    fn test_find_plan_file_in_transcript_ignores_a_non_plan_mode_attachment() {
        let dir = tempdir().unwrap();
        let plan_file = dir.path().join("plan.md");
        std::fs::write(&plan_file, "# Ok plan").unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        std::fs::write(
            &transcript,
            format!(
                "{{\"type\":\"attachment\",\"attachment\":{{\"type\":\"other_kind\"}}}}\n{{\"type\":\"attachment\",\"attachment\":{{\"type\":\"plan_mode\",\"planFilePath\":\"{}\",\"planExists\":true}}}}\n",
                plan_file.display()
            ),
        )
        .unwrap();
        let env = envelope(&format!(
            r#"{{"prompt_id":"x","transcript_path":"{}","tool_input":{{}}}}"#,
            transcript.display()
        ));
        let (plan, _) = resolve_plan(&env).unwrap();
        assert_eq!(plan, "# Ok plan");
    }

    #[test]
    fn test_find_plan_file_in_transcript_ignores_a_plan_mode_attachment_with_no_path() {
        let dir = tempdir().unwrap();
        let plan_file = dir.path().join("plan.md");
        std::fs::write(&plan_file, "# Ok plan").unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        std::fs::write(
            &transcript,
            format!(
                "{{\"type\":\"attachment\",\"attachment\":{{\"type\":\"plan_mode\",\"planExists\":true}}}}\n{{\"type\":\"attachment\",\"attachment\":{{\"type\":\"plan_mode\",\"planFilePath\":\"{}\",\"planExists\":true}}}}\n",
                plan_file.display()
            ),
        )
        .unwrap();
        let env = envelope(&format!(
            r#"{{"prompt_id":"x","transcript_path":"{}","tool_input":{{}}}}"#,
            transcript.display()
        ));
        let (plan, _) = resolve_plan(&env).unwrap();
        assert_eq!(plan, "# Ok plan");
    }

    // ── resolve_plan: nothing works ──────────────────────────────────────

    #[test]
    fn test_resolve_plan_none_with_an_empty_envelope() {
        assert!(resolve_plan(&HookEnvelope::default()).is_none());
    }

    #[test]
    fn test_resolve_plan_none_when_transcript_path_is_missing() {
        let env = envelope(r#"{"prompt_id":"x","tool_input":{}}"#);
        assert!(resolve_plan(&env).is_none());
    }

    #[test]
    fn test_resolve_plan_none_when_transcript_file_does_not_exist() {
        let env = envelope(
            r#"{"prompt_id":"x","transcript_path":"/no/such/transcript.jsonl","tool_input":{}}"#,
        );
        assert!(resolve_plan(&env).is_none());
    }

    // ── envelope deserialization tolerates unknown fields ────────────────

    #[test]
    fn test_envelope_ignores_fields_it_does_not_model() {
        let env = envelope(
            r##"{"session_id":"s1","cwd":"/repo","permission_mode":"plan","hook_event_name":"PreToolUse","tool_name":"ExitPlanMode","tool_use_id":"t1","tool_input":{"plan":"# Plan","planFilePath":"/x"}}"##,
        );
        assert_eq!(env.tool_input.unwrap().plan.as_deref(), Some("# Plan"));
    }

    #[test]
    fn test_envelope_reads_session_id() {
        let env = envelope(r##"{"session_id":"abc-123","tool_input":{"plan":"# Plan"}}"##);
        assert_eq!(env.session_id.as_deref(), Some("abc-123"));
    }

    #[test]
    fn test_envelope_session_id_absent_is_none() {
        let env = envelope(r##"{"tool_input":{"plan":"# Plan"}}"##);
        assert_eq!(env.session_id, None);
    }

    // ── output rendering ─────────────────────────────────────────────────

    #[test]
    fn test_render_deny_shape() {
        let json = render_deny("R-001: sign with RS256 — \"use HS256 for signing\"");
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(value["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(value["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains("R-001"));
        // Never allow, structurally: the rendered JSON contains the literal
        // string "deny" and nowhere contains "allow".
        assert!(!json.contains("allow"));
    }

    #[test]
    fn test_render_notice_matches_the_shell_hooks_own_shape() {
        let json = render_notice("upgrade actual");
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["systemMessage"], "upgrade actual");
        assert_eq!(
            value["hookSpecificOutput"]["systemMessage"],
            "upgrade actual"
        );
        assert_eq!(value["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert!(value.get("permissionDecision").is_none());
        assert!(value["hookSpecificOutput"]
            .get("permissionDecision")
            .is_none());
    }

    #[test]
    fn test_render_deny_is_exactly_one_json_object_on_one_line() {
        let json = render_deny("R-001: reason");
        assert_eq!(json.matches('\n').count(), 0);
        assert!(serde_json::from_str::<serde_json::Value>(&json).is_ok());
    }
}
