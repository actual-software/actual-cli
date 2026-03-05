use crate::config::ServiceConfig;
use crate::error::{Result, SymphonyError};
use crate::workflow::load_workflow;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

/// Workflow file watcher that detects changes and triggers reload.
pub struct WorkflowWatcher {
    _workflow_path: PathBuf,
    _watcher: RecommendedWatcher,
}

/// Start watching a workflow file for changes.
///
/// Returns a receiver that emits (ServiceConfig, prompt_template) on each successful reload.
pub fn start_workflow_watch(
    workflow_path: &Path,
) -> Result<(WorkflowWatcher, mpsc::Receiver<(ServiceConfig, String)>)> {
    let (tx, rx) = mpsc::channel(16);
    let path = workflow_path.to_path_buf();
    let watch_path = path.clone();

    let mut watcher = notify::recommended_watcher(move |res: std::result::Result<Event, _>| {
        match res {
            Ok(event) => {
                let dominated_by_modify = matches!(
                    event.kind,
                    EventKind::Modify(_) | EventKind::Create(_)
                );
                if !dominated_by_modify {
                    return;
                }

                debug!(event = ?event.kind, "workflow file change detected");

                match reload_workflow(&watch_path) {
                    Ok((config, prompt)) => {
                        info!("workflow reloaded successfully");
                        // Best-effort send; if the channel is full, drop the oldest
                        let _ = tx.try_send((config, prompt));
                    }
                    Err(e) => {
                        error!(error = %e, "workflow reload failed, keeping last known good config");
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "workflow watcher error");
            }
        }
    })
    .map_err(|e| SymphonyError::Other(format!("failed to create file watcher: {e}")))?;

    // Watch the parent directory (to catch file replacements)
    let watch_dir = workflow_path.parent().unwrap_or_else(|| Path::new("."));

    watcher
        .watch(watch_dir, RecursiveMode::NonRecursive)
        .map_err(|e| SymphonyError::Other(format!("failed to watch directory: {e}")))?;

    info!(path = %workflow_path.display(), "watching workflow file for changes");

    Ok((
        WorkflowWatcher {
            _workflow_path: path,
            _watcher: watcher,
        },
        rx,
    ))
}

fn reload_workflow(path: &Path) -> Result<(ServiceConfig, String)> {
    let workflow = load_workflow(path)?;
    let config = ServiceConfig::from_workflow(&workflow)?;
    Ok((config, workflow.prompt_template))
}

/// Spawn a task that processes workflow reload events and updates the orchestrator.
pub fn spawn_reload_handler(
    mut rx: mpsc::Receiver<(ServiceConfig, String)>,
    config: Arc<RwLock<ServiceConfig>>,
    prompt_template: Arc<RwLock<String>>,
    orchestrator_tx: mpsc::Sender<crate::orchestrator::OrchestratorMessage>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some((new_config, new_prompt)) = rx.recv().await {
            *config.write().await = new_config;
            *prompt_template.write().await = new_prompt;
            // Optionally trigger an immediate poll
            let _ = orchestrator_tx
                .send(crate::orchestrator::OrchestratorMessage::TriggerRefresh)
                .await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn test_reload_workflow() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("WORKFLOW.md");
        fs::write(
            &path,
            r#"---
tracker:
  kind: linear
  project_slug: test
  api_key: key123
---
Do the work on {{ issue.identifier }}.
"#,
        )
        .unwrap();

        let (config, prompt) = reload_workflow(&path).unwrap();
        assert_eq!(config.tracker.kind, "linear");
        assert!(prompt.contains("issue.identifier"));
    }
}
