//! `actual reset` — clear all local state for a clean test.
//!
//! Clears persistent credentials, ephemeral session tokens, observer journals,
//! and pending operations. Does NOT revoke server-side sessions — use
//! `actual logout` for that.

use crate::auth::{ephemeral, pending, store};
use crate::cli::ui::theme;
use crate::error::ActualError;

pub fn exec() -> Result<(), ActualError> {
    let mut cleared: Vec<&str> = Vec::new();

    if store::load()?.is_some() {
        store::delete()?;
        cleared.push("credentials");
    }

    if ephemeral::load()?.is_some() {
        ephemeral::clear()?;
        cleared.push("ephemeral session");
    }

    if pending::load()?.is_some() {
        pending::clear()?;
        cleared.push("pending operation");
    }

    // Clear observer session journals
    if let Ok(sessions_dir) = sessions_dir() {
        if sessions_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
                let count = entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path()
                            .extension()
                            .map(|ext| ext == "jsonl")
                            .unwrap_or(false)
                    })
                    .count();
                if count > 0 {
                    std::fs::remove_dir_all(&sessions_dir).ok();
                    std::fs::create_dir_all(&sessions_dir).ok();
                    cleared.push("observer sessions");
                }
            }
        }
    }

    if cleared.is_empty() {
        println!("Nothing to clear.");
    } else {
        println!(
            "{} Cleared: {}",
            theme::success(&theme::SUCCESS),
            cleared.join(", "),
        );
    }
    Ok(())
}

fn sessions_dir() -> Result<std::path::PathBuf, ActualError> {
    Ok(crate::config::paths::config_dir()?.join("sessions"))
}
