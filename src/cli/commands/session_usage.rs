//! A Claude Code or Codex session's token totals per model, read from its
//! transcript for `actual.session.ended`. Only counts and model names come out
//! of here; the transcript, its path and its text stay on the machine.
//!
//! Claude Code writes a `cost-state` line now and then with its own running
//! totals per model, which also cover subagents and the background calls the
//! transcript has no message for. The last one wins. A transcript without one
//! falls back to the `usage` of each assistant message, in the transcript and
//! in the subagent transcripts beside it. Claude Code repeats a message's
//! line once per content block with the same usage, so messages count once
//! per id.
//!
//! A Codex rollout has a `token_usage_record` line for each model response,
//! carrying its usage and turn but no model; the turn's `turn_context` line
//! names the model. A record whose turn has no `turn_context`, such as the
//! request that compacts the history, counts toward the latest turn's model.
//! Codex's `input_tokens` includes the cached ones, so input is sent less
//! `cached_input_tokens`, as Claude Code's is. A record naming another session
//! is history a fork copied, and is skipped. Observed on Codex CLI 0.166.0.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Deserialize;

use crate::telemetry::otlp::{ModelTokens, SessionUsage};

/// The `actual.usage.source` of totals taken from a `cost-state` line.
pub(super) const SESSION_TOTALS: &str = "session_totals";

/// The `actual.usage.source` of totals summed from assistant messages.
pub(super) const MESSAGES: &str = "messages";

/// The `actual.usage.source` of totals summed from a Codex rollout's
/// `token_usage_record` lines.
pub(super) const RESPONSES: &str = "responses";

/// The model Claude Code names on messages it made up itself, which no API
/// call produced.
const SYNTHETIC: &str = "<synthetic>";

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    uuid: Option<String>,
    message: Option<Message>,
    #[serde(rename = "modelUsage")]
    model_usage: Option<BTreeMap<String, CostState>>,
    payload: Option<Payload>,
}

/// The fields of a Codex `turn_context` or `token_usage_record` payload.
#[derive(Deserialize)]
struct Payload {
    turn_id: Option<String>,
    model: Option<String>,
    session_id: Option<String>,
    response_id: Option<String>,
    usage: Option<CodexUsage>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CodexUsage {
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct CostState {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
}

/// The totals for session `session_id`'s transcript at `transcript`, or
/// `None` when it can't be read, holds no usage, or the scan passes
/// `deadline`, since a partial sum would undercount.
pub(super) fn read(transcript: &Path, session_id: &str, deadline: Instant) -> Option<SessionUsage> {
    let mut scan = Scan {
        session_id,
        ..Scan::default()
    };
    scan.file(transcript, deadline)?;
    if scan.codex {
        let mut totals: BTreeMap<String, ModelTokens> = BTreeMap::new();
        let first_model = scan.first_model;
        let unturned = scan
            .unturned
            .into_iter()
            .filter_map(|u| Some((first_model.clone()?, u)));
        for (model, u) in scan.responses.into_iter().chain(unturned) {
            let total = totals
                .entry(model.clone())
                .or_insert_with(|| tokens(model, 0, 0, 0, 0));
            total.input = total
                .input
                .saturating_add(u.input_tokens.saturating_sub(u.cached_input_tokens));
            total.output = total.output.saturating_add(u.output_tokens);
            total.cache_read = total.cache_read.saturating_add(u.cached_input_tokens);
            total.cache_write = total.cache_write.saturating_add(u.cache_write_input_tokens);
        }
        return usage(RESPONSES, totals.into_values().collect());
    }
    if let Some(totals) = scan.cost_state.take() {
        let models = totals
            .into_iter()
            .map(|(model, c)| {
                tokens(
                    model,
                    c.input_tokens,
                    c.output_tokens,
                    c.cache_read_input_tokens,
                    c.cache_creation_input_tokens,
                )
            })
            .collect();
        return usage(SESSION_TOTALS, models);
    }
    for subagent in subagent_transcripts(transcript) {
        scan.file(&subagent, deadline)?;
    }
    let mut totals: BTreeMap<String, ModelTokens> = BTreeMap::new();
    for (model, u) in scan.messages.into_values().chain(scan.unkeyed) {
        let total = totals
            .entry(model.clone())
            .or_insert_with(|| tokens(model, 0, 0, 0, 0));
        total.input = total.input.saturating_add(u.input_tokens);
        total.output = total.output.saturating_add(u.output_tokens);
        total.cache_read = total.cache_read.saturating_add(u.cache_read_input_tokens);
        total.cache_write = total
            .cache_write
            .saturating_add(u.cache_creation_input_tokens);
    }
    usage(MESSAGES, totals.into_values().collect())
}

#[derive(Default)]
struct Scan<'a> {
    session_id: &'a str,
    cost_state: Option<BTreeMap<String, CostState>>,
    messages: HashMap<String, (String, Usage)>,
    unkeyed: Vec<(String, Usage)>,
    /// Whether a Codex `token_usage_record` line was read.
    codex: bool,
    turn_models: HashMap<String, String>,
    first_model: Option<String>,
    latest_model: Option<String>,
    response_ids: HashSet<String>,
    responses: Vec<(String, CodexUsage)>,
    /// Records read before any `turn_context`, for the first model named.
    unturned: Vec<CodexUsage>,
}

impl Scan<'_> {
    /// Fold `path`'s lines in, or `None` when it can't be opened or the
    /// deadline passes. Lines that aren't JSON are skipped.
    fn file(&mut self, path: &Path, deadline: Instant) -> Option<()> {
        let mut reader = BufReader::new(std::fs::File::open(path).ok()?);
        let mut line = Vec::new();
        loop {
            if Instant::now() >= deadline {
                return None;
            }
            line.clear();
            if reader.read_until(b'\n', &mut line).ok()? == 0 {
                return Some(());
            }
            if let Ok(entry) = serde_json::from_slice::<Line>(&line) {
                self.entry(entry);
            }
        }
    }

    fn entry(&mut self, entry: Line) {
        match entry.kind.as_deref() {
            Some("cost-state") => {
                if let Some(totals) = entry.model_usage {
                    self.cost_state = Some(totals);
                }
            }
            Some("assistant") => {
                let Some(Message {
                    id,
                    model: Some(model),
                    usage: Some(usage),
                }) = entry.message
                else {
                    return;
                };
                if model == SYNTHETIC {
                    return;
                }
                match id.or(entry.uuid) {
                    Some(id) => {
                        self.messages.insert(id, (model, usage));
                    }
                    None => self.unkeyed.push((model, usage)),
                }
            }
            Some("turn_context") => {
                let Some(Payload {
                    turn_id: Some(turn),
                    model: Some(model),
                    ..
                }) = entry.payload
                else {
                    return;
                };
                self.first_model.get_or_insert_with(|| model.clone());
                self.latest_model = Some(model.clone());
                self.turn_models.insert(turn, model);
            }
            Some("token_usage_record") => {
                let Some(Payload {
                    turn_id,
                    session_id,
                    response_id,
                    usage: Some(usage),
                    ..
                }) = entry.payload
                else {
                    return;
                };
                self.codex = true;
                if session_id.is_some_and(|id| id != self.session_id) {
                    return;
                }
                if let Some(id) = response_id {
                    if !self.response_ids.insert(id) {
                        return;
                    }
                }
                let model = turn_id
                    .and_then(|turn| self.turn_models.get(&turn))
                    .or(self.latest_model.as_ref());
                match model {
                    Some(model) => self.responses.push((model.clone(), usage)),
                    None => self.unturned.push(usage),
                }
            }
            _ => {}
        }
    }
}

/// The subagent transcripts Claude Code keeps beside `transcript`, in
/// `<session>/subagents/*.jsonl`, sorted.
fn subagent_transcripts(transcript: &Path) -> Vec<PathBuf> {
    let dir = transcript.with_extension("").join("subagents");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl") && path.is_file())
        .collect();
    paths.sort();
    paths
}

fn tokens(
    model: String,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
) -> ModelTokens {
    ModelTokens {
        model,
        input,
        output,
        cache_read,
        cache_write,
    }
}

/// `models` from `source`, less any model that used no tokens, or `None`
/// when that leaves none.
fn usage(source: &'static str, mut models: Vec<ModelTokens>) -> Option<SessionUsage> {
    models.retain(|m| [m.input, m.output, m.cache_read, m.cache_write] != [0; 4]);
    (!models.is_empty()).then_some(SessionUsage { source, models })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::tempdir;

    fn later() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    fn assistant(id: &str, model: &str, input: u64, output: u64, read: u64, write: u64) -> String {
        serde_json::json!({
            "type": "assistant",
            "uuid": format!("uuid-{id}"),
            "message": {
                "id": id,
                "model": model,
                "content": [{"type": "text", "text": "private words"}],
                "usage": {
                    "input_tokens": input,
                    "output_tokens": output,
                    "cache_read_input_tokens": read,
                    "cache_creation_input_tokens": write,
                    "service_tier": "standard"
                }
            }
        })
        .to_string()
    }

    fn cost_state(models: serde_json::Value) -> String {
        serde_json::json!({"type": "cost-state", "totalCostUSD": 1.5, "modelUsage": models})
            .to_string()
    }

    fn write(path: &Path, lines: &[String]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, lines.join("\n") + "\n").unwrap();
    }

    fn summary(usage: &SessionUsage) -> Vec<(String, [u64; 4])> {
        usage
            .models
            .iter()
            .map(|m| {
                (
                    m.model.clone(),
                    [m.input, m.output, m.cache_read, m.cache_write],
                )
            })
            .collect()
    }

    /// Each message counts once however many lines repeat it, across the
    /// transcript and its subagents, per model and sorted by model; made-up
    /// messages, other lines and lines that aren't JSON count nothing.
    #[test]
    fn messages_count_once_per_id_with_their_subagents() {
        let dir = tempdir().unwrap();
        let transcript = dir.path().join("session-1.jsonl");
        write(
            &transcript,
            &[
                r#"{"type":"user","message":{"role":"user","content":"hi"}}"#.to_string(),
                assistant("m1", "claude-opus", 3, 10, 100, 20),
                assistant("m1", "claude-opus", 3, 10, 100, 20),
                "not json".to_string(),
                assistant("m2", "claude-opus", 1, 5, 50, 0),
                assistant("s1", SYNTHETIC, 9, 9, 9, 9),
                r#"{"type":"assistant","uuid":"u3","message":{"model":"claude-opus","usage":{"output_tokens":2}}}"#.to_string(),
                r#"{"type":"assistant","message":{"model":"claude-opus","usage":{"output_tokens":1}}}"#.to_string(),
                r#"{"type":"assistant","message":{"id":"m3","model":"claude-opus"}}"#.to_string(),
            ],
        );
        let subagents = dir.path().join("session-1/subagents");
        write(
            &subagents.join("agent-a.jsonl"),
            &[
                assistant("a1", "claude-haiku", 2, 7, 30, 40),
                assistant("a1", "claude-haiku", 2, 7, 30, 40),
            ],
        );
        write(
            &subagents.join("notes.txt"),
            &[assistant("x", "claude-haiku", 1, 1, 1, 1)],
        );

        let usage = read(&transcript, "s", later()).unwrap();

        assert_eq!(usage.source, MESSAGES);
        assert_eq!(
            summary(&usage),
            [
                ("claude-haiku".to_string(), [2, 7, 30, 40]),
                ("claude-opus".to_string(), [4, 18, 150, 20]),
            ]
        );
    }

    /// A `cost-state` line's totals stand for the whole session, so the last
    /// one wins over every message and the subagents aren't read.
    #[test]
    fn the_last_cost_state_wins() {
        let dir = tempdir().unwrap();
        let transcript = dir.path().join("session-2.jsonl");
        write(
            &transcript,
            &[
                cost_state(
                    serde_json::json!({"claude-opus": {"inputTokens": 1, "outputTokens": 1}}),
                ),
                assistant("m1", "claude-opus", 3, 10, 100, 20),
                cost_state(serde_json::json!({
                    "claude-opus": {"inputTokens": 5, "outputTokens": 11, "cacheReadInputTokens": 120,
                                    "cacheCreationInputTokens": 25, "costUSD": 0.4, "thinkingTokens": 3},
                    "claude-haiku": {"inputTokens": 900, "outputTokens": 4},
                    "claude-unused": {"inputTokens": 0},
                    "claude-max": {"inputTokens": u64::MAX, "outputTokens": u64::MAX}
                })),
                assistant("m2", "claude-opus", 1, 5, 50, 0),
            ],
        );
        write(
            &dir.path().join("session-2/subagents/agent-a.jsonl"),
            &[assistant("a1", "claude-sonnet", 2, 7, 30, 40)],
        );

        let usage = read(&transcript, "s", later()).unwrap();

        assert_eq!(usage.source, SESSION_TOTALS);
        assert_eq!(
            summary(&usage),
            [
                ("claude-haiku".to_string(), [900, 4, 0, 0]),
                ("claude-max".to_string(), [u64::MAX, u64::MAX, 0, 0]),
                ("claude-opus".to_string(), [5, 11, 120, 25]),
            ]
        );
    }

    /// A missing file, one with no usage, and a scan past its deadline give
    /// nothing rather than a partial count.
    #[test]
    fn nothing_when_unreadable_empty_or_late() {
        let dir = tempdir().unwrap();
        assert!(read(&dir.path().join("missing.jsonl"), "s", later()).is_none());
        let empty = dir.path().join("empty.jsonl");
        write(
            &empty,
            &[
                r#"{"type":"user"}"#.to_string(),
                assistant("s", SYNTHETIC, 1, 1, 1, 1),
            ],
        );
        assert!(read(&empty, "s", later()).is_none());
        let full = dir.path().join("full.jsonl");
        write(&full, &[assistant("m1", "claude-opus", 1, 1, 1, 1)]);
        assert!(read(&full, "s", Instant::now()).is_none());
        assert!(read(&full, "s", later()).is_some());
    }

    fn codex_turn(turn: &str, model: &str) -> String {
        serde_json::json!({
            "type": "turn_context",
            "payload": {"turn_id": turn, "cwd": "/private/dir", "model": model, "effort": "high",
                        "user_instructions": "private words"}
        })
        .to_string()
    }

    fn codex_record(
        session: &str,
        turn: &str,
        response: &str,
        [input, cached, write, output]: [u64; 4],
    ) -> String {
        let usage = serde_json::json!({
            "input_tokens": input, "cached_input_tokens": cached,
            "cache_write_input_tokens": write, "output_tokens": output,
            "reasoning_output_tokens": output / 2, "total_tokens": input + output
        });
        serde_json::json!({
            "type": "token_usage_record",
            "payload": {"thread_id": session, "session_id": session, "turn_id": turn,
                        "root_turn_id": turn, "response_id": response, "usage": usage,
                        "turn_token_usage": usage, "thread_token_usage": usage}
        })
        .to_string()
    }

    /// A Codex rollout's responses add up per model of their turn, input less
    /// the cached tokens. A response recorded twice counts once, records with
    /// no response id each count, one before any turn goes to the first model,
    /// one in a turn with no `turn_context` goes to the latest model, and
    /// another session's records, records with no usage and turns with no
    /// model count nothing.
    #[test]
    fn codex_responses_add_up_per_turn_model() {
        let dir = tempdir().unwrap();
        let rollout = dir.path().join("rollout-2026-11-24T10-00-00-codex-1.jsonl");
        write(
            &rollout,
            &[
                r#"{"type":"session_meta","payload":{"id":"codex-1","cwd":"/private/dir","cli_version":"0.166.0"}}"#.to_string(),
                codex_record("codex-1", "t0", "r0", [7, 0, 0, 1]),
                codex_turn("t1", "gpt-a"),
                r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"private words"}]}}"#.to_string(),
                codex_record("codex-1", "t1", "r1", [100, 60, 5, 10]),
                codex_record("codex-1", "t1", "r1", [100, 60, 5, 10]),
                r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":999}}}}"#.to_string(),
                "not json".to_string(),
                codex_turn("t2", "gpt-b"),
                codex_record("codex-1", "t2", "r2", [50, 40, 0, 3]),
                codex_record("codex-1", "t1", "r3", [20, 0, 0, 2]),
                codex_record("codex-1", "compaction", "r4", [30, 30, 0, 4]),
                codex_record("codex-0", "t1", "r5", [1000, 0, 0, 1000]),
                codex_record("codex-1", "t2", "r6", [5, 10, 0, 0]),
                r#"{"type":"turn_context","payload":{"turn_id":"t3"}}"#.to_string(),
                r#"{"type":"token_usage_record","payload":{"session_id":"codex-1","turn_id":"t1"}}"#.to_string(),
                r#"{"type":"token_usage_record","payload":{"turn_id":"t1","usage":{"input_tokens":4,"output_tokens":1}}}"#.to_string(),
                r#"{"type":"token_usage_record","payload":{"turn_id":"t1","usage":{"input_tokens":4,"output_tokens":1}}}"#.to_string(),
            ],
        );

        let usage = read(&rollout, "codex-1", later()).unwrap();

        assert_eq!(usage.source, RESPONSES);
        assert_eq!(
            summary(&usage),
            [
                ("gpt-a".to_string(), [75, 15, 60, 5]),
                ("gpt-b".to_string(), [10, 7, 80, 0]),
            ]
        );
    }

    /// A Codex rollout with no records of its own session, or none at all,
    /// gives nothing.
    #[test]
    fn nothing_from_a_codex_rollout_without_its_own_records() {
        let dir = tempdir().unwrap();
        let forked = dir.path().join("forked.jsonl");
        write(
            &forked,
            &[
                codex_turn("t1", "gpt-a"),
                codex_record("parent", "t1", "r1", [100, 0, 0, 10]),
            ],
        );
        assert!(read(&forked, "child", later()).is_none());
        let unrecorded = dir.path().join("unrecorded.jsonl");
        write(&unrecorded, &[codex_turn("t1", "gpt-a")]);
        assert!(read(&unrecorded, "child", later()).is_none());
    }
}
