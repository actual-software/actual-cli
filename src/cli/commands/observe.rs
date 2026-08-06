use std::io::{self, Read};

use crate::cli::args::{ObserveArgs, ObserveCommand};
use crate::error::ActualError;
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

    println!("{{}}");

    Ok(())
}
