//! A Claude Code session's token totals per model, read from its transcript
//! for `actual.session.ended`. Only counts and model names come out of here;
//! the transcript, its path and its text stay on the machine.
//!
//! Claude Code writes a `cost-state` line now and then with its own running
//! totals per model, which also cover subagents and the background calls the
//! transcript has no message for. The last one wins. A transcript without one
//! falls back to the `usage` of each assistant message, in the transcript and
//! in the subagent transcripts beside it. Claude Code repeats a message's
//! line once per content block with the same usage, so messages count once
//! per id.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Deserialize;

use crate::telemetry::otlp::{ModelTokens, SessionUsage};

/// The `actual.usage.source` of totals taken from a `cost-state` line.
pub(super) const SESSION_TOTALS: &str = "session_totals";

/// The `actual.usage.source` of totals summed from assistant messages.
pub(super) const MESSAGES: &str = "messages";

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

/// The totals for the transcript at `transcript`, or `None` when it can't be
/// read, holds no usage, or the scan passes `deadline`, since a partial sum
/// would undercount.
pub(super) fn read(transcript: &Path, deadline: Instant) -> Option<SessionUsage> {
    let mut scan = Scan::default();
    scan.file(transcript, deadline)?;
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
struct Scan {
    cost_state: Option<BTreeMap<String, CostState>>,
    messages: HashMap<String, (String, Usage)>,
    unkeyed: Vec<(String, Usage)>,
}

impl Scan {
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
            _ => {}
        }
    }
}

/// The subagent transcripts Claude Code keeps beside `transcript`, in
/// `<session>/subagents/*.jsonl`, sorted.
fn subagent_transcripts(transcript: &Path) -> Vec<PathBuf> {
    let Some(stem) = transcript.file_stem() else {
        return Vec::new();
    };
    let dir = transcript.with_file_name(stem).join("subagents");
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

        let usage = read(&transcript, later()).unwrap();

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

        let usage = read(&transcript, later()).unwrap();

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
        assert!(read(&dir.path().join("missing.jsonl"), later()).is_none());
        let empty = dir.path().join("empty.jsonl");
        write(
            &empty,
            &[
                r#"{"type":"user"}"#.to_string(),
                assistant("s", SYNTHETIC, 1, 1, 1, 1),
            ],
        );
        assert!(read(&empty, later()).is_none());
        let full = dir.path().join("full.jsonl");
        write(&full, &[assistant("m1", "claude-opus", 1, 1, 1, 1)]);
        assert!(read(&full, Instant::now()).is_none());
        assert!(read(&full, later()).is_some());
    }

    /// Times a scan of a real transcript, for the session-end hook's budget:
    /// `ACTUAL_USAGE_TRANSCRIPT=<path> cargo test --release scan_time -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn scan_time() {
        let path = PathBuf::from(std::env::var("ACTUAL_USAGE_TRANSCRIPT").unwrap());
        let size = std::fs::metadata(&path).unwrap().len();
        let start = Instant::now();
        let usage = read(&path, later());
        let elapsed = start.elapsed();
        println!(
            "scanned {size} bytes in {elapsed:?}: {:?}",
            usage.map(|u| (u.source, summary(&u)))
        );
    }
}
