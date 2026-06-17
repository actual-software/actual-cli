//! `actual advisor <query>` — ask the Advisor org-scoped architecture questions.
//!
//! Opens an SSE stream (`POST /v1/advisor/query/stream`), renders phase progress
//! to stderr as it arrives, and prints the final answer to stdout. Uses the
//! platform token from `actual login` as the bearer. There is no client-side
//! time cap, so a long ADR-backed answer runs to completion; a per-read idle
//! timeout (kept alive by server heartbeats) is the only liveness bound.

use chrono::{Duration as ChronoDuration, Utc};
use futures_util::StreamExt;

use crate::api::types::{AdvisorOutput, AdvisorStreamRequest};
use crate::api::{ActualApiClient, DEFAULT_API_URL};
use crate::auth::oauth;
use crate::auth::store::{self, StoredCredentials};
use crate::cli::args::AdvisorArgs;
use crate::cli::ui::theme;
use crate::error::ActualError;

pub fn exec(args: &AdvisorArgs) -> Result<(), ActualError> {
    build_runtime()?.block_on(run(args))
}

/// Single-threaded tokio runtime so the sync CLI dispatch path can drive the
/// async stream flow (mirrors `commands::login`).
fn build_runtime() -> Result<tokio::runtime::Runtime, ActualError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ActualError::InternalError(format!("failed to build tokio runtime: {e}")))
}

/// Resolve the Advisor API base URL: the `--api-url` flag wins, then the
/// `ACTUAL_API_URL` environment variable, else the api-service default. This
/// mirrors how `login` honors `ACTUAL_AUTH_URL`, so a single export steers both
/// the auth and advisor halves against a local stack. An empty env var is
/// treated as unset.
fn resolve_api_url(flag: Option<&str>) -> String {
    flag.map(|s| s.to_string())
        .or_else(|| {
            std::env::var("ACTUAL_API_URL")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_API_URL.to_string())
}

fn enrich_org_mismatch(
    err: ActualError,
    session_org: &str,
    target_org: &str,
    explicit_org: bool,
) -> ActualError {
    match err {
        ActualError::OrgMismatch { .. } => {
            let (message, hint) = org_mismatch_message(session_org, target_org, explicit_org);
            ActualError::OrgMismatch { message, hint }
        }
        other => other,
    }
}

fn org_mismatch_message(
    session_org: &str,
    target_org: &str,
    explicit_org: bool,
) -> (String, String) {
    if explicit_org && target_org != session_org {
        (
            format!(
                "Advisor request denied (HTTP 403): this session is scoped to organization \
                 {session_org}, but you requested organization {target_org}."
            ),
            format!("actual login --org {target_org}  (or drop --org to query {session_org})"),
        )
    } else {
        (
            format!(
                "Advisor request denied (HTTP 403): this session (organization {session_org}) \
                 was rejected as cross-organization, or your token carries no usable \
                 organization."
            ),
            "actual login  (optionally with --org <org-id>) to refresh your session".to_string(),
        )
    }
}

/// If the stored access token is expired (or about to be), refresh it with the
/// rotation primitive and re-persist. A refresh failure surfaces as
/// `NotLoggedIn` — the user must re-run `actual login`.
async fn ensure_fresh(creds: StoredCredentials) -> Result<StoredCredentials, ActualError> {
    if creds.refresh_token.is_empty() {
        return Ok(creds);
    }
    if creds.expires_within(Utc::now(), ChronoDuration::seconds(60)) {
        let refreshed = oauth::refresh(&creds)
            .await
            .map_err(|_| ActualError::NotLoggedIn)?;
        store::save(&refreshed)?;
        return Ok(refreshed);
    }
    Ok(creds)
}

async fn run(args: &AdvisorArgs) -> Result<(), ActualError> {
    let creds = store::load()?.ok_or(ActualError::NotLoggedIn)?;
    let creds = ensure_fresh(creds).await?;
    let org_id = args
        .org
        .clone()
        .unwrap_or_else(|| creds.organization_id.clone());
    // Captured for the cross-org 403 message: the session's own org, and
    // whether the caller targeted a different org via an explicit `--org`.
    let session_org = creds.organization_id.clone();
    let explicit_org = args.org.is_some();
    let base_url = resolve_api_url(args.api_url.as_deref());
    let client = ActualApiClient::new(&base_url)?.with_bearer(&creds.access_token);

    let request = AdvisorStreamRequest::new(org_id.clone(), args.query.clone(), args.repo.clone());

    let response = client
        .stream_advisor_query(&request)
        .await
        .map_err(|e| enrich_org_mismatch(e, &session_org, &org_id, explicit_org))?;

    let outcome = consume_stream(response)
        .await
        .map_err(|e| enrich_org_mismatch(e, &session_org, &org_id, explicit_org))?;

    match outcome {
        StreamOutcome::Final(data) => {
            print_final(&data, args.json)?;
            Ok(())
        }
        // The stream closed without a `final` frame and without an `error`. Per
        // the wire contract a graceful close is completion, not a failure — there
        // is simply no answer body to print.
        StreamOutcome::Closed => Ok(()),
    }
}

/// Terminal outcome of consuming the advisor stream. An `error` frame and a
/// transport failure are surfaced as `Err` by [`consume_stream`], so the only
/// success shapes are a `final` answer or a graceful close without one.
#[derive(Debug, PartialEq)]
enum StreamOutcome {
    Final(serde_json::Value),
    Closed,
}

/// Drive the SSE response to a terminal outcome: render each `phase` frame to
/// stderr, return the `data` of the `final` frame, and surface an `error` frame
/// as a non-zero-exit `ApiError`. Frames split across read chunks are reassembled
/// by [`SseDecoder`]; a graceful close without a `final` frame is completion.
async fn consume_stream(response: reqwest::Response) -> Result<StreamOutcome, ActualError> {
    let mut decoder = SseDecoder::default();
    let stream = response.bytes_stream();
    futures_util::pin_mut!(stream);
    while let Some(chunk) = stream.next().await {
        let bytes =
            chunk.map_err(|e| ActualError::ApiError(format!("advisor stream read failed: {e}")))?;
        for payload in decoder.push(bytes.as_ref()) {
            if let Some(outcome) = handle_payload(&payload)? {
                return Ok(outcome);
            }
        }
    }
    // Stream ended. Flush a trailing event the server closed without a blank-line
    // terminator, in case it carried the `final` frame.
    if let Some(payload) = decoder.flush() {
        if let Some(outcome) = handle_payload(&payload)? {
            return Ok(outcome);
        }
    }
    Ok(StreamOutcome::Closed)
}

/// Interpret one decoded SSE payload. Renders phase progress to stderr as a side
/// effect; returns `Some(outcome)` once a terminal `final` frame arrives, `Err`
/// on an `error` frame, and `None` for frames that do not end the stream
/// (phase, heartbeat, unrecognized type, or a non-JSON line).
fn handle_payload(payload: &str) -> Result<Option<StreamOutcome>, ActualError> {
    match parse_advisor_frame(payload) {
        Some(AdvisorStreamEvent::Phase(phase)) => {
            render_phase(&phase);
            Ok(None)
        }
        Some(AdvisorStreamEvent::Final(data)) => Ok(Some(StreamOutcome::Final(data))),
        Some(AdvisorStreamEvent::Error(message)) => Err(ActualError::ApiError(format!(
            "Advisor query failed: {message}"
        ))),
        // Heartbeats, unrecognized frame types, and non-JSON lines are ignored.
        Some(AdvisorStreamEvent::Heartbeat) | Some(AdvisorStreamEvent::Other(_)) | None => Ok(None),
    }
}

/// A decoded SSE frame from the advisor stream.
#[derive(Debug, Clone, PartialEq)]
enum AdvisorStreamEvent {
    /// `data.phase` progress (`fetching` | `interpreting` | `summarizing` | `done`).
    Phase(String),
    /// The terminal answer — the `data` payload (a serialized `AdvisorOutput`).
    Final(serde_json::Value),
    /// An `error` frame; its message ends the run with a non-zero exit.
    Error(String),
    /// A keep-alive; ignored.
    Heartbeat,
    /// A frame whose `type` is unrecognized; ignored.
    Other(String),
}

/// Parse one SSE `data:` payload (the JSON after `data: `) into an event.
/// Returns `None` when the payload is not a JSON object with a string `type`,
/// so a stray or non-conforming line is ignored rather than fatal.
fn parse_advisor_frame(payload: &str) -> Option<AdvisorStreamEvent> {
    let value: serde_json::Value = serde_json::from_str(payload.trim()).ok()?;
    let obj = value.as_object()?;
    let kind = obj.get("type")?.as_str()?;
    let event = match kind {
        "phase" => {
            let phase = obj
                .get("data")
                .and_then(|d| d.get("phase"))
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            AdvisorStreamEvent::Phase(phase)
        }
        "final" => {
            let data = obj.get("data").cloned().unwrap_or(serde_json::Value::Null);
            AdvisorStreamEvent::Final(data)
        }
        "error" => {
            let message = obj
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown error")
                .to_string();
            AdvisorStreamEvent::Error(message)
        }
        "heartbeat" => AdvisorStreamEvent::Heartbeat,
        other => AdvisorStreamEvent::Other(other.to_string()),
    };
    Some(event)
}

/// Render one phase as a single stderr line. `summarizing` is a one-shot
/// deterministic step, so it renders as one line (not a typing animation); the
/// `done` marker is silent because the `final` frame carries the answer.
fn render_phase(phase: &str) {
    let label = match phase {
        "fetching" => "fetching ADRs…",
        "interpreting" => "interpreting…",
        "summarizing" => "summarizing…",
        "" | "done" => return,
        other => {
            eprintln!("{} {other}…", theme::hint("advisor"));
            return;
        }
    };
    eprintln!("{} {label}", theme::hint("advisor"));
}

/// Print the final answer to stdout. With `--json`, echo the server's
/// `AdvisorOutput` object verbatim (pretty-printed) so machine consumers get the
/// canonical shape; otherwise render the human-readable form.
fn print_final(data: &serde_json::Value, json: bool) -> Result<(), ActualError> {
    if json {
        let rendered = serde_json::to_string_pretty(data)
            .map_err(|e| ActualError::ApiError(format!("failed to render advisor JSON: {e}")))?;
        println!("{rendered}");
        return Ok(());
    }
    let output: AdvisorOutput = serde_json::from_value(data.clone()).map_err(|e| {
        ActualError::ApiError(format!("advisor returned an unparseable answer: {e}"))
    })?;
    print_answer(&output);
    Ok(())
}

/// Render the advisor answer. **Never prints token material.**
fn print_answer(output: &AdvisorOutput) {
    println!("{}", output.summary);
    if !output.interpreter.related_adrs.is_empty() {
        println!("\n{}", theme::hint("Related ADRs:"));
        for adr in &output.interpreter.related_adrs {
            println!(
                "  • {} ({}, confidence {:.0}%)",
                adr.title,
                adr.scope,
                adr.confidence * 100.0
            );
        }
    }
}

/// Reassembles SSE frames from a byte stream. Bytes are buffered until a blank
/// line (`\n\n`, or the CRLF form `\n\r\n`) terminates an event; each complete
/// event's `data:` payload is then returned. A frame split across read chunks is
/// held in the buffer until its terminator arrives.
#[derive(Default)]
struct SseDecoder {
    buf: Vec<u8>,
}

impl SseDecoder {
    /// Append a chunk and return the `data:` payload of every event that is now
    /// complete. Events with no `data:` line (e.g. a comment-only keep-alive)
    /// yield nothing.
    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(end) = Self::event_boundary(&self.buf) {
            let block: Vec<u8> = self.buf.drain(..end).collect();
            if let Some(payload) = Self::extract_data(&block) {
                out.push(payload);
            }
        }
        out
    }

    /// Flush a trailing event the stream closed without a blank-line terminator.
    /// Returns its `data:` payload if it has one.
    fn flush(&mut self) -> Option<String> {
        if self.buf.is_empty() {
            return None;
        }
        let block = std::mem::take(&mut self.buf);
        Self::extract_data(&block)
    }

    /// Index just past the first blank-line event terminator in `buf`, if any.
    /// Recognizes both `\n\n` (LF) and `\n\r\n` (CRLF) blank lines. The boundary
    /// always falls on an ASCII byte, so a drained block never splits a UTF-8
    /// codepoint.
    fn event_boundary(buf: &[u8]) -> Option<usize> {
        for i in 0..buf.len() {
            if buf[i] == b'\n' {
                if buf.get(i + 1) == Some(&b'\n') {
                    return Some(i + 2);
                }
                if buf.get(i + 1) == Some(&b'\r') && buf.get(i + 2) == Some(&b'\n') {
                    return Some(i + 3);
                }
            }
        }
        None
    }

    /// Collect the `data:` lines of one event block, strip the `data:` prefix
    /// (and one optional leading space), and join multiple data lines with `\n`
    /// per the SSE spec. Returns `None` when the block has no data line.
    fn extract_data(block: &[u8]) -> Option<String> {
        let text = String::from_utf8_lossy(block);
        let mut data_lines = Vec::new();
        for raw_line in text.split('\n') {
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            if let Some(rest) = line.strip_prefix("data:") {
                data_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            }
        }
        if data_lines.is_empty() {
            None
        } else {
            Some(data_lines.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::store::StoredCredentials;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use tempfile::tempdir;

    const STREAM_PATH: &str = "/v1/advisor/query/stream";

    const PHASE_FETCHING: &str = r#"{"type":"phase","data":{"phase":"fetching"}}"#;
    const PHASE_INTERPRETING: &str = r#"{"type":"phase","data":{"phase":"interpreting"}}"#;
    const PHASE_SUMMARIZING: &str = r#"{"type":"phase","data":{"phase":"summarizing"}}"#;
    const HEARTBEAT: &str = r#"{"type":"heartbeat","data":{"timestamp":1}}"#;
    const FINAL_NO_ADRS: &str = r#"{"type":"final","data":{"summary":"Use the App Router.","interpreter":{"summary":"i","related_adrs":[]}}}"#;
    const ONE_ADR: &str = r#"{"id":"a1","name":"n","title":"Use the App Router","policy":"p","instructions":"i","scope":"frontend","relevance_reason":"r","confidence":0.92}"#;

    fn final_with_adr() -> String {
        format!(
            r#"{{"type":"final","data":{{"summary":"Use the App Router.","interpreter":{{"summary":"i","related_adrs":[{ONE_ADR}]}}}}}}"#
        )
    }

    /// Build an SSE body from a list of JSON frame payloads.
    fn sse_body(frames: &[&str]) -> String {
        frames.iter().map(|f| format!("data: {f}\n\n")).collect()
    }

    fn test_creds() -> StoredCredentials {
        StoredCredentials {
            access_token: "tok".to_string(),
            refresh_token: "r".to_string(),
            token_type: "Bearer".to_string(),
            expires_at: None,
            scope: None,
            organization_id: "11111111-1111-1111-1111-111111111111".to_string(),
            member_id: "m".to_string(),
            email: None,
            subject: None,
            auth_url: Some("http://localhost:4000".to_string()),
        }
    }

    fn args(api_url: &str, org: Option<&str>) -> AdvisorArgs {
        AdvisorArgs {
            query: "why app router?".to_string(),
            api_url: Some(api_url.to_string()),
            org: org.map(|s| s.to_string()),
            repo: None,
            json: false,
        }
    }

    #[test]
    fn test_resolve_api_url() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

        // The --api-url flag wins even when ACTUAL_API_URL is set.
        let g = EnvGuard::set("ACTUAL_API_URL", "http://env:9999");
        assert_eq!(
            resolve_api_url(Some("http://localhost:3099")),
            "http://localhost:3099"
        );
        drop(g);

        // No flag → the ACTUAL_API_URL env var is used when present.
        let g = EnvGuard::set("ACTUAL_API_URL", "http://env:9999");
        assert_eq!(resolve_api_url(None), "http://env:9999");
        drop(g);

        // No flag, empty env var → treated as unset, falls back to the default.
        let g = EnvGuard::set("ACTUAL_API_URL", "");
        assert_eq!(resolve_api_url(None), DEFAULT_API_URL);
        drop(g);

        // No flag, env unset → the api-service default.
        let g = EnvGuard::remove("ACTUAL_API_URL");
        assert_eq!(resolve_api_url(None), DEFAULT_API_URL);
        drop(g);
    }

    #[test]
    fn test_print_answer_with_and_without_adrs() {
        let with = AdvisorOutput {
            summary: "S".to_string(),
            interpreter: crate::api::types::AdvisorInterpreter {
                summary: "i".to_string(),
                related_adrs: vec![crate::api::types::RelatedAdr {
                    id: "a".to_string(),
                    name: "n".to_string(),
                    title: "T".to_string(),
                    policy: "p".to_string(),
                    instructions: "i".to_string(),
                    scope: "s".to_string(),
                    relevance_reason: "r".to_string(),
                    confidence: 0.5,
                }],
            },
        };
        print_answer(&with);
        let without = AdvisorOutput {
            summary: "S".to_string(),
            interpreter: crate::api::types::AdvisorInterpreter {
                summary: "i".to_string(),
                related_adrs: vec![],
            },
        };
        print_answer(&without);
    }

    #[test]
    fn test_render_phase_silent_and_fallback_arms() {
        // `done` and the empty phase are silent (the `final` frame carries the
        // answer, so there is nothing to print for them); an unrecognized phase
        // falls through to the generic one-line render. None of these panic —
        // exercising each arm locks the rendering contract in.
        render_phase("done");
        render_phase("");
        render_phase("compiling");
    }

    // --- SSE frame decoding (chunk reassembly) ---

    #[test]
    fn test_decoder_single_frame_one_push() {
        let mut d = SseDecoder::default();
        let out = d.push(b"data: {\"type\":\"heartbeat\"}\n\n");
        assert_eq!(out, vec!["{\"type\":\"heartbeat\"}".to_string()]);
    }

    #[test]
    fn test_decoder_multiple_frames_one_push() {
        let mut d = SseDecoder::default();
        let body = sse_body(&[PHASE_FETCHING, FINAL_NO_ADRS]);
        let out = d.push(body.as_bytes());
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], PHASE_FETCHING);
        assert_eq!(out[1], FINAL_NO_ADRS);
    }

    #[test]
    fn test_decoder_reassembles_frame_split_across_chunks() {
        let mut d = SseDecoder::default();
        let frame = format!("data: {FINAL_NO_ADRS}\n\n");
        let bytes = frame.as_bytes();
        let mid = bytes.len() / 2;
        // First half: no complete frame yet.
        assert!(d.push(&bytes[..mid]).is_empty());
        // Second half completes the frame.
        let out = d.push(&bytes[mid..]);
        assert_eq!(out, vec![FINAL_NO_ADRS.to_string()]);
    }

    #[test]
    fn test_decoder_reassembles_frame_split_byte_by_byte() {
        let mut d = SseDecoder::default();
        let frame = format!("data: {PHASE_FETCHING}\n\n");
        let mut collected = Vec::new();
        for b in frame.as_bytes() {
            collected.extend(d.push(&[*b]));
        }
        assert_eq!(collected, vec![PHASE_FETCHING.to_string()]);
    }

    #[test]
    fn test_decoder_handles_crlf_separators() {
        let mut d = SseDecoder::default();
        let out = d.push(b"data: {\"type\":\"heartbeat\"}\r\n\r\n");
        assert_eq!(out, vec!["{\"type\":\"heartbeat\"}".to_string()]);
    }

    #[test]
    fn test_decoder_ignores_comment_only_event() {
        let mut d = SseDecoder::default();
        // A comment-only keep-alive (no `data:` line) yields nothing.
        let out = d.push(b": keep-alive\n\n");
        assert!(out.is_empty());
    }

    #[test]
    fn test_decoder_flush_returns_unterminated_trailing_event() {
        let mut d = SseDecoder::default();
        // No trailing blank line — push yields nothing, flush recovers it.
        assert!(d.push(b"data: {\"type\":\"final\"}").is_empty());
        assert_eq!(d.flush(), Some("{\"type\":\"final\"}".to_string()));
        // Buffer is drained after flush.
        assert_eq!(d.flush(), None);
    }

    #[test]
    fn test_decoder_strips_optional_space_after_prefix() {
        let mut d = SseDecoder::default();
        // SSE allows `data:x` (no space) and `data: x` (one space) — both strip
        // to `x`.
        assert_eq!(d.push(b"data:no-space\n\n"), vec!["no-space".to_string()]);
        assert_eq!(
            d.push(b"data: one-space\n\n"),
            vec!["one-space".to_string()]
        );
    }

    // --- frame parsing ---

    #[test]
    fn test_parse_frame_phase() {
        assert_eq!(
            parse_advisor_frame(PHASE_FETCHING),
            Some(AdvisorStreamEvent::Phase("fetching".to_string()))
        );
    }

    #[test]
    fn test_parse_frame_final_carries_output() {
        // `final` parses to a Final event carrying the verbatim `data` object.
        // Asserting equality (rather than matching with a fallback arm) keeps the
        // test free of an untaken branch; the asserted-equal value is then what
        // deserializes into an AdvisorOutput.
        let event = parse_advisor_frame(FINAL_NO_ADRS).unwrap();
        let expected: serde_json::Value = serde_json::from_str(
            r#"{"summary":"Use the App Router.","interpreter":{"summary":"i","related_adrs":[]}}"#,
        )
        .unwrap();
        assert_eq!(event, AdvisorStreamEvent::Final(expected.clone()));
        let out: AdvisorOutput = serde_json::from_value(expected).unwrap();
        assert_eq!(out.summary, "Use the App Router.");
        assert!(out.interpreter.related_adrs.is_empty());
    }

    #[test]
    fn test_parse_frame_error() {
        let frame = r#"{"type":"error","error":"stream ended"}"#;
        assert_eq!(
            parse_advisor_frame(frame),
            Some(AdvisorStreamEvent::Error("stream ended".to_string()))
        );
    }

    #[test]
    fn test_parse_frame_heartbeat() {
        assert_eq!(
            parse_advisor_frame(HEARTBEAT),
            Some(AdvisorStreamEvent::Heartbeat)
        );
    }

    #[test]
    fn test_parse_frame_unknown_type_is_other() {
        let frame = r#"{"type":"surprise","data":{}}"#;
        assert_eq!(
            parse_advisor_frame(frame),
            Some(AdvisorStreamEvent::Other("surprise".to_string()))
        );
    }

    #[test]
    fn test_parse_frame_non_json_is_none() {
        assert_eq!(parse_advisor_frame("not json"), None);
        assert_eq!(parse_advisor_frame(""), None);
        // A JSON value that is not an object, or lacks a string `type`.
        assert_eq!(parse_advisor_frame("[1,2,3]"), None);
        assert_eq!(parse_advisor_frame(r#"{"no":"type"}"#), None);
    }

    // --- per-frame dispatch (handle_payload) ---

    #[test]
    fn test_handle_payload_phase_is_non_terminal() {
        assert_eq!(handle_payload(PHASE_FETCHING).unwrap(), None);
    }

    #[test]
    fn test_handle_payload_heartbeat_is_ignored() {
        assert_eq!(handle_payload(HEARTBEAT).unwrap(), None);
    }

    #[test]
    fn test_handle_payload_unknown_and_garbage_are_ignored() {
        assert_eq!(handle_payload(r#"{"type":"surprise"}"#).unwrap(), None);
        assert_eq!(handle_payload("not json").unwrap(), None);
    }

    #[test]
    fn test_handle_payload_final_is_terminal() {
        // A `final` payload is the one terminal success outcome; assert equality
        // rather than matching so there is no untaken arm left uncovered.
        let outcome = handle_payload(FINAL_NO_ADRS).unwrap();
        let expected: serde_json::Value = serde_json::from_str(
            r#"{"summary":"Use the App Router.","interpreter":{"summary":"i","related_adrs":[]}}"#,
        )
        .unwrap();
        assert_eq!(outcome, Some(StreamOutcome::Final(expected.clone())));
        let out: AdvisorOutput = serde_json::from_value(expected).unwrap();
        assert_eq!(out.summary, "Use the App Router.");
    }

    #[test]
    fn test_handle_payload_error_frame_is_error() {
        let frame = r#"{"type":"error","error":"boom"}"#;
        let err = handle_payload(frame).unwrap_err();
        assert!(matches!(err, ActualError::ApiError(ref m) if m.contains("boom")));
    }

    // --- print_final ---

    #[test]
    fn test_print_final_json_pretty_prints() {
        let data = serde_json::json!({
            "summary": "S",
            "interpreter": {"summary": "i", "related_adrs": []}
        });
        // --json echoes the object verbatim; valid input never errors.
        print_final(&data, true).unwrap();
    }

    #[test]
    fn test_print_final_human_renders_valid_output() {
        let data = serde_json::json!({
            "summary": "S",
            "interpreter": {"summary": "i", "related_adrs": []}
        });
        print_final(&data, false).unwrap();
    }

    #[test]
    fn test_print_final_human_rejects_unparseable_output() {
        // Missing `summary` → not a valid AdvisorOutput → error on the human path.
        let data = serde_json::json!({"interpreter": {"summary": "i"}});
        let err = print_final(&data, false).unwrap_err();
        assert!(matches!(err, ActualError::ApiError(ref m) if m.contains("unparseable")));
    }

    // --- end-to-end via a mocked stream ---

    #[test]
    fn test_exec_not_logged_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        let err = exec(&args("http://127.0.0.1:1", None)).unwrap_err();
        assert!(matches!(err, ActualError::NotLoggedIn));
    }

    #[tokio::test]
    async fn test_run_not_logged_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        let err = run(&args("http://127.0.0.1:1", None)).await.unwrap_err();
        assert!(matches!(err, ActualError::NotLoggedIn));
    }

    #[tokio::test]
    async fn test_run_streams_phases_then_final() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        let body = sse_body(&[
            PHASE_FETCHING,
            PHASE_INTERPRETING,
            PHASE_SUMMARIZING,
            &final_with_adr(),
        ]);
        let m = server
            .mock("POST", STREAM_PATH)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(body)
            .create_async()
            .await;

        run(&args(&server.url(), None)).await.unwrap();
        m.assert_async().await;
    }

    #[tokio::test]
    async fn test_run_ignores_interleaved_heartbeats() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        let body = sse_body(&[HEARTBEAT, PHASE_FETCHING, HEARTBEAT, FINAL_NO_ADRS]);
        let _m = server
            .mock("POST", STREAM_PATH)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(body)
            .create_async()
            .await;

        run(&args(&server.url(), None)).await.unwrap();
    }

    #[tokio::test]
    async fn test_run_json_output() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        let body = sse_body(&[PHASE_FETCHING, FINAL_NO_ADRS]);
        let _m = server
            .mock("POST", STREAM_PATH)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(body)
            .create_async()
            .await;

        let mut a = args(&server.url(), None);
        a.json = true;
        run(&a).await.unwrap();
    }

    #[tokio::test]
    async fn test_run_error_frame_is_non_zero_exit() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        let body = sse_body(&[
            PHASE_FETCHING,
            r#"{"type":"error","error":"model unavailable"}"#,
        ]);
        let _m = server
            .mock("POST", STREAM_PATH)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(body)
            .create_async()
            .await;

        let err = run(&args(&server.url(), None)).await.unwrap_err();
        assert!(matches!(err, ActualError::ApiError(ref m) if m.contains("model unavailable")));
        // ApiError is a non-zero exit.
        assert_ne!(err.exit_code(), 0);
    }

    #[tokio::test]
    async fn test_run_graceful_close_without_final_is_completion() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        // Phases only, then the server closes — no `final`, no `error`.
        let body = sse_body(&[PHASE_FETCHING, PHASE_INTERPRETING]);
        let _m = server
            .mock("POST", STREAM_PATH)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(body)
            .create_async()
            .await;

        // Graceful close with no final frame is completion, not an error.
        run(&args(&server.url(), None)).await.unwrap();
    }

    #[tokio::test]
    async fn test_run_final_recovered_via_flush_without_trailing_blank_line() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        // A blank-line-terminated phase frame (consumed via `push`) followed by a
        // `final` frame the server closes WITHOUT the terminating blank line, so
        // the answer is only recovered by the end-of-stream `flush`.
        let body = format!("data: {PHASE_FETCHING}\n\ndata: {FINAL_NO_ADRS}");
        let _m = server
            .mock("POST", STREAM_PATH)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(body)
            .create_async()
            .await;

        // The flush-recovered `final` frame is a successful terminal outcome.
        run(&args(&server.url(), None)).await.unwrap();
    }

    #[tokio::test]
    async fn test_run_flush_recovers_non_terminal_frame_then_closes() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        // The stream closes right after a phase frame, with no terminating blank
        // line: the phase is recovered by `flush` but is non-terminal, so the run
        // ends as a graceful close (no `final`, no `error`) rather than returning
        // a flushed answer.
        let body = format!("data: {PHASE_FETCHING}");
        let _m = server
            .mock("POST", STREAM_PATH)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(body)
            .create_async()
            .await;

        run(&args(&server.url(), None)).await.unwrap();
    }

    #[tokio::test]
    async fn test_run_non_2xx_is_non_zero_exit() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", STREAM_PATH)
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":{"code":"BAD_REQUEST","message":"bad","details":null}}"#)
            .create_async()
            .await;

        let err = run(&args(&server.url(), None)).await.unwrap_err();
        assert_ne!(err.exit_code(), 0);
    }

    #[tokio::test]
    async fn test_run_with_explicit_repo_scopes_request() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        let s = server
            .mock("POST", STREAM_PATH)
            .match_body(mockito::Matcher::PartialJsonString(
                r#"{"repo_unique_id":"33333333-3333-3333-3333-333333333333"}"#.to_string(),
            ))
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(sse_body(&[FINAL_NO_ADRS]))
            .create_async()
            .await;

        let mut a = args(&server.url(), None);
        a.repo = Some("33333333-3333-3333-3333-333333333333".to_string());
        run(&a).await.unwrap();
        s.assert_async().await;
    }

    #[tokio::test]
    async fn test_run_org_mismatch_403_surfaces_actionable_error() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());
        store::save(&test_creds()).unwrap();

        let mut server = mockito::Server::new_async().await;
        // api-service rejects the cross-org token with a fail-closed 403.
        let _s = server
            .mock("POST", STREAM_PATH)
            .with_status(403)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":{"code":"FORBIDDEN","message":"cross-org","details":null}}"#)
            .create_async()
            .await;

        // Explicit --org that differs from the session org (test_creds is 1111…).
        let target = "99999999-9999-9999-9999-999999999999";
        let err = run(&args(&server.url(), Some(target))).await.unwrap_err();

        match err {
            ActualError::OrgMismatch { message, hint } => {
                assert!(message.contains("403"), "expected 403 in: {message}");
                assert!(
                    message.contains(target),
                    "expected target org in: {message}"
                );
                assert!(
                    message.contains("11111111-1111-1111-1111-111111111111"),
                    "expected session org in: {message}"
                );
                assert!(
                    hint.contains("actual login --org"),
                    "expected actionable remediation in hint: {hint}"
                );
            }
            other => panic!("expected OrgMismatch, got {other:?}"),
        }
    }

    #[test]
    fn test_org_mismatch_message_explicit_org_names_both_and_remediation() {
        let (message, hint) = org_mismatch_message("org-A", "org-B", true);
        assert!(
            message.contains("org-A"),
            "expected session org in: {message}"
        );
        assert!(
            message.contains("org-B"),
            "expected target org in: {message}"
        );
        assert!(message.contains("403"), "expected 403 in: {message}");
        // The remediation now rides on the hint (the "Fix:" line), not Display.
        assert!(
            hint.contains("actual login --org org-B"),
            "expected targeted remediation in hint: {hint}"
        );
        assert!(
            !message.contains("actual login"),
            "remediation should not be in the message: {message}"
        );
    }

    #[test]
    fn test_org_mismatch_message_no_explicit_org_steers_to_relogin() {
        // No explicit --org → target == session; the generic re-login branch.
        let (message, hint) = org_mismatch_message("org-A", "org-A", false);
        assert!(
            message.contains("org-A"),
            "expected session org in: {message}"
        );
        assert!(message.contains("403"), "expected 403 in: {message}");
        // Remediation rides on the hint, not Display.
        assert!(
            hint.contains("actual login"),
            "expected re-login remediation in hint: {hint}"
        );
        assert!(
            !message.contains("actual login"),
            "remediation should not be in the message: {message}"
        );
    }

    // --- transparent refresh-on-expiry ---

    #[tokio::test]
    async fn test_ensure_fresh_no_refresh_token_returns_unchanged() {
        let mut c = test_creds();
        c.refresh_token = String::new();
        let out = ensure_fresh(c.clone()).await.unwrap();
        assert_eq!(out.access_token, c.access_token);
    }

    #[tokio::test]
    async fn test_ensure_fresh_not_expired_returns_unchanged() {
        let mut c = test_creds();
        c.expires_at = Some(Utc::now() + ChronoDuration::hours(1));
        let out = ensure_fresh(c).await.unwrap();
        assert_eq!(out.access_token, "tok");
    }

    #[tokio::test]
    async fn test_ensure_fresh_expired_refreshes_and_persists() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let tmp = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", "/api/oauth/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"access_token":"new-at","token_type":"Bearer","expires_in":3600,"refresh_token":"new-rt"}"#,
            )
            .create_async()
            .await;

        let mut c = test_creds();
        c.expires_at = Some(Utc::now() - ChronoDuration::seconds(1)); // expired
        c.auth_url = Some(server.url());

        let out = ensure_fresh(c).await.unwrap();
        assert_eq!(out.access_token, "new-at");
        // Rotated creds were re-persisted.
        assert_eq!(store::load().unwrap().unwrap().access_token, "new-at");
    }

    #[tokio::test]
    async fn test_ensure_fresh_refresh_failure_is_not_logged_in() {
        // Expired + unreachable auth server → refresh errors → NotLoggedIn.
        let mut c = test_creds();
        c.expires_at = Some(Utc::now() - ChronoDuration::seconds(1));
        c.auth_url = Some("http://127.0.0.1:1".to_string());
        let err = ensure_fresh(c).await.unwrap_err();
        assert!(matches!(err, ActualError::NotLoggedIn));
    }
}
