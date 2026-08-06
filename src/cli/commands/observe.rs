use std::io::{self, Read};

use crate::api::client::{ActualApiClient, DEFAULT_API_URL};
use crate::api::types::{InterventionEvent, InterventionRequest};
use crate::auth::store;
use crate::cli::args::{ObserveArgs, ObserveCommand};
use crate::error::ActualError;
use crate::observe::boundary::is_evaluation_boundary;
use crate::observe::canonicalize;
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

    if is_evaluation_boundary(hook_type, tool_name) {
        let hook_output = evaluate_at_boundary(session_id, &journal);
        println!("{}", serde_json::to_string(&hook_output).unwrap_or_else(|_| "{}".to_string()));
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

fn try_evaluate_at_boundary(
    session_id: &str,
    journal: &SessionJournal,
) -> Result<serde_json::Value, ActualError> {
    let creds = store::load()?.ok_or(ActualError::NotLoggedIn)?;

    let api_url = std::env::var("ACTUAL_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_string());

    let journal_events = journal.read_session(session_id)?;
    if journal_events.is_empty() {
        return Ok(serde_json::json!({}));
    }

    let events: Vec<InterventionEvent> = journal_events
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
                .unwrap_or(idx as u64) as usize,
            payload: Some(event.clone()),
        })
        .collect();

    let request = InterventionRequest {
        org_id: creds.organization_id.clone(),
        repo_unique_id: None,
        session_id: session_id.to_string(),
        events,
    };

    let rt = tokio::runtime::Runtime::new().map_err(|e| {
        ActualError::ConfigError(format!("failed to create runtime: {e}"))
    })?;

    let response = rt.block_on(async {
        let client = ActualApiClient::new(&api_url)?
            .with_bearer(&creds.access_token);
        client.post_intervention(&request).await
    })?;

    Ok(response.hook_output)
}
