use std::io::{self, Read};

use chrono::{Duration as ChronoDuration, Utc};

use crate::api::client::{ActualApiClient, DEFAULT_API_URL};
use crate::api::types::{InterventionEvent, InterventionRequest, InterventionResponse};
use crate::auth::{oauth, store};
use crate::auth::store::StoredCredentials;
use crate::cli::args::{ObserveArgs, ObserveCommand};
use crate::error::ActualError;
use crate::observe::boundary::is_evaluation_boundary;
use crate::observe::canonicalize;
use crate::observe::hook_output::build_block_output;
use crate::observe::journal::SessionJournal;
use crate::observe::setup;
use crate::observe::types::HookType;

pub fn exec(args: &ObserveArgs) -> Result<(), ActualError> {
    match &args.command {
        ObserveCommand::Setup => exec_setup(),
        ObserveCommand::Status => exec_status(),
        _ => exec_hook(args),
    }
}

fn exec_setup() -> Result<(), ActualError> {
    let settings_path = setup::default_settings_path();
    setup::install_hooks(&settings_path)?;
    eprintln!("Observer hooks installed in {}", settings_path.display());
    Ok(())
}

fn exec_status() -> Result<(), ActualError> {
    let settings_path = setup::default_settings_path();
    if settings_path.exists() {
        eprintln!("hooks: installed");
    } else {
        eprintln!("hooks: not installed");
    }
    eprintln!("observer: ready");
    Ok(())
}

fn exec_hook(args: &ObserveArgs) -> Result<(), ActualError> {
    let hook_type = HookType::from_subcommand(args.command.hook_name()).ok_or_else(|| {
        ActualError::ConfigError(format!("unknown hook type: {}", args.command.hook_name()))
    })?;

    let mut stdin_buf = String::new();
    io::stdin().read_to_string(&mut stdin_buf).map_err(|e| {
        ActualError::ConfigError(format!("failed to read stdin: {e}"))
    })?;

    let raw_payload: serde_json::Value = if stdin_buf.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&stdin_buf).map_err(|e| {
            ActualError::ConfigError(format!("invalid JSON on stdin: {e}"))
        })?
    };

    let tool_name = raw_payload.get("tool_name").and_then(|v| v.as_str());

    let aewo_code = canonicalize::canonicalize(hook_type, tool_name);

    let session_id = raw_payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let journal = SessionJournal::new()?;
    journal.append(session_id, &raw_payload, &aewo_code)?;

    if is_evaluation_boundary(hook_type, tool_name, &raw_payload) {
        let mut hook_output = evaluate_at_boundary(session_id, &journal);

        let is_gating_hook = matches!(hook_type, HookType::PreToolUse | HookType::Stop);
        let merged_disposition = extract_disposition(&hook_output);

        if is_gating_hook && merged_disposition == "block" {
            let reason = hook_output
                .get("hookSpecificOutput")
                .and_then(|h| h.get("additionalContext"))
                .and_then(|c| c.as_str())
                .unwrap_or("Architecture violation detected by Actual Advisor.");
            println!("{}", build_block_output(reason, Some(reason), hook_type.as_str()));
        } else {
            inject_hook_event_name(&mut hook_output, hook_type.as_str());
            println!("{}", serde_json::to_string(&hook_output).unwrap_or_else(|_| "{}".to_string()));
        }
    } else {
        println!("{{}}");
    }

    Ok(())
}

/// At an evaluation boundary, load credentials, read all journal events,
/// POST to the advisor intervention API, and return the hook_output.
/// Any error degrades to empty JSON (silent disposition).
fn evaluate_at_boundary(
    session_id: &str,
    journal: &SessionJournal,
) -> serde_json::Value {
    match try_evaluate_at_boundary(session_id, journal) {
        Ok(output) => output,
        Err(e) => {
            eprintln!("advisor: boundary evaluation failed: {e}");
            serde_json::json!({})
        }
    }
}

const CHUNK_SIZE: usize = 50;

fn disposition_severity(disposition: &str) -> u8 {
    match disposition {
        "block" => 3,
        "warn" => 2,
        "inform" => 1,
        _ => 0,
    }
}

fn try_evaluate_at_boundary(
    session_id: &str,
    journal: &SessionJournal,
) -> Result<serde_json::Value, ActualError> {
    let creds = store::load()?.ok_or(ActualError::NotLoggedIn)?;

    let api_url = std::env::var("ACTUAL_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_string());

    let cursor = journal.read_cursor(session_id);
    let (new_events, new_cursor) = journal.read_session_from(session_id, cursor)?;
    if new_events.is_empty() {
        return Ok(serde_json::json!({}));
    }

    let rt = tokio::runtime::Runtime::new().map_err(|e| {
        ActualError::ConfigError(format!("failed to create runtime: {e}"))
    })?;

    let creds = rt.block_on(ensure_fresh(creds))?;

    let all_events: Vec<InterventionEvent> = new_events
        .iter()
        .enumerate()
        .map(|(idx, event)| InterventionEvent {
            hook_type: event
                .get("hook_type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
            tool_name: event.get("tool_name").and_then(|v| v.as_str()).map(|s| s.to_string()),
            session_id: session_id.to_string(),
            sequence_no: event
                .get("sequence_no")
                .and_then(|v| v.as_u64())
                .unwrap_or((cursor + idx) as u64) as usize,
            payload: Some(event.clone()),
        })
        .collect();

    let chunks: Vec<&[InterventionEvent]> = all_events.chunks(CHUNK_SIZE).collect();
    let total_chunks = chunks.len();

    let mut responses: Vec<InterventionResponse> = Vec::new();

    for (chunk_idx, chunk) in chunks.iter().enumerate() {
        let request = InterventionRequest {
            org_id: creds.organization_id.clone(),
            repo_unique_id: None,
            session_id: session_id.to_string(),
            events: chunk.to_vec(),
        };

        let response = rt.block_on(async {
            let client = ActualApiClient::new(&api_url)?
                .with_bearer(&creds.access_token);
            client.post_intervention(&request).await
        });

        match response {
            Ok(resp) => {
                if total_chunks > 1 {
                    eprintln!(
                        "advisor: chunk {}/{} complete (disposition={})",
                        chunk_idx + 1,
                        total_chunks,
                        resp.disposition,
                    );
                }
                responses.push(resp);
            }
            Err(e) => {
                eprintln!(
                    "advisor: chunk {}/{} failed: {e}, degrading to silent (AD-22)",
                    chunk_idx + 1,
                    total_chunks,
                );
                journal.write_cursor(session_id, new_cursor)?;
                return Ok(serde_json::json!({}));
            }
        }
    }

    journal.write_cursor(session_id, new_cursor)?;

    Ok(merge_chunk_responses(responses))
}

/// Extract the highest disposition from a merged hook output.
/// The merged output carries a `_disposition` field set by `merge_chunk_responses`.
fn extract_disposition(output: &serde_json::Value) -> String {
    output
        .get("_disposition")
        .and_then(|v| v.as_str())
        .unwrap_or("silent")
        .to_string()
}

/// Inject hookEventName into hookSpecificOutput so Claude Code validates the output.
/// Also strips internal fields (prefixed with `_`) before output.
fn inject_hook_event_name(output: &mut serde_json::Value, event_name: &str) {
    if let Some(obj) = output.as_object_mut() {
        obj.remove("_disposition");
    }
    if let Some(hook_specific) = output
        .get_mut("hookSpecificOutput")
        .and_then(|h| h.as_object_mut())
    {
        hook_specific.insert(
            "hookEventName".to_string(),
            serde_json::Value::String(event_name.to_string()),
        );
    }
}

/// Merge all chunk responses into a single hook_output. Non-silent responses
/// are ranked by severity (block > warn > inform), and their additionalContext
/// sections are concatenated so Claude Code sees every finding.
fn merge_chunk_responses(mut responses: Vec<InterventionResponse>) -> serde_json::Value {
    responses.sort_by(|a, b| {
        disposition_severity(&b.disposition).cmp(&disposition_severity(&a.disposition))
    });

    let non_silent: Vec<&InterventionResponse> = responses
        .iter()
        .filter(|r| r.disposition != "silent")
        .collect();

    if non_silent.is_empty() {
        return serde_json::json!({});
    }

    let highest_disposition = &non_silent[0].disposition;

    if non_silent.len() == 1 {
        let mut output = non_silent[0].hook_output.clone();
        if let Some(obj) = output.as_object_mut() {
            obj.insert("_disposition".to_string(), serde_json::json!(highest_disposition));
        }
        return output;
    }

    let mut combined_sections: Vec<String> = Vec::new();

    for (idx, resp) in non_silent.iter().enumerate() {
        if let Some(context) = resp
            .hook_output
            .get("hookSpecificOutput")
            .and_then(|h| h.get("additionalContext"))
            .and_then(|c| c.as_str())
        {
            if idx > 0 {
                combined_sections.push("---".to_string());
            }
            combined_sections.push(context.to_string());
        }
    }

    if combined_sections.is_empty() {
        return serde_json::json!({});
    }

    let header = format!(
        "📊 ACTUAL ADVISOR — {} FINDINGS ACROSS {} CHUNKS\n",
        non_silent.len(),
        responses.len(),
    );

    serde_json::json!({
        "_disposition": highest_disposition,
        "hookSpecificOutput": {
            "additionalContext": format!("{}{}", header, combined_sections.join("\n")),
        }
    })
}

async fn ensure_fresh(creds: StoredCredentials) -> Result<StoredCredentials, ActualError> {
    if !creds.expires_within(Utc::now(), ChronoDuration::seconds(60)) {
        return Ok(creds);
    }
    if creds.refresh_token.is_empty() {
        return Err(ActualError::NotLoggedIn);
    }
    let refreshed = oauth::refresh(&creds)
        .await
        .map_err(|_| ActualError::NotLoggedIn)?;
    store::save(&refreshed)?;
    Ok(refreshed)
}
