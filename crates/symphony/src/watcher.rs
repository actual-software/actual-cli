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

/// Handle a single file-watcher event.
///
/// Extracted from the watcher callback to allow direct testing.
fn handle_watcher_event(
    res: std::result::Result<Event, notify::Error>,
    watch_path: &Path,
    tx: &mpsc::Sender<(ServiceConfig, String)>,
) {
    match res {
        Ok(event) => {
            let dominated_by_modify =
                matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_));
            if !dominated_by_modify {
                return;
            }

            debug!(event = ?event.kind, "workflow file change detected");

            match reload_workflow(watch_path) {
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
        handle_watcher_event(res, &watch_path, &tx);
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
    use crate::orchestrator::OrchestratorMessage;
    use std::fs;
    use std::time::Duration;

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

    /// Helper: write a valid WORKFLOW.md and return its ServiceConfig + prompt.
    fn write_workflow(dir: &Path, slug: &str, prompt_body: &str) -> PathBuf {
        let path = dir.join("WORKFLOW.md");
        let content = format!(
            "---\ntracker:\n  kind: linear\n  project_slug: {slug}\n  api_key: key123\n---\n{prompt_body}\n"
        );
        fs::write(&path, content).unwrap();
        path
    }

    // ---- spawn_reload_handler tests ----

    #[tokio::test]
    async fn test_spawn_reload_handler_updates_state_and_sends_trigger() {
        // Build an initial config from a workflow
        let tmp = tempfile::tempdir().unwrap();
        let path = write_workflow(tmp.path(), "initial", "Old prompt.");
        let (initial_config, _) = reload_workflow(&path).unwrap();

        // Shared state
        let config = Arc::new(RwLock::new(initial_config));
        let prompt = Arc::new(RwLock::new("old prompt".to_string()));

        // Channels
        let (reload_tx, reload_rx) = mpsc::channel::<(ServiceConfig, String)>(16);
        let (orch_tx, mut orch_rx) = mpsc::channel::<OrchestratorMessage>(16);

        // Spawn handler
        let handle = spawn_reload_handler(reload_rx, config.clone(), prompt.clone(), orch_tx);

        // Build a new config
        let path2 = write_workflow(tmp.path(), "updated", "New prompt.");
        let (new_config, new_prompt) = reload_workflow(&path2).unwrap();

        // Send the update
        reload_tx.send((new_config, new_prompt)).await.unwrap();

        // Wait for processing
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Verify shared state was updated
        assert_eq!(config.read().await.tracker.project_slug, "updated");
        assert_eq!(*prompt.read().await, "New prompt.");

        // Verify TriggerRefresh was sent
        assert!(matches!(
            orch_rx.try_recv(),
            Ok(OrchestratorMessage::TriggerRefresh)
        ));

        // Drop sender so the handler loop exits
        drop(reload_tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_spawn_reload_handler_exits_when_sender_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_workflow(tmp.path(), "test", "Prompt.");
        let (initial_config, _) = reload_workflow(&path).unwrap();

        let config = Arc::new(RwLock::new(initial_config));
        let prompt = Arc::new(RwLock::new(String::new()));

        let (reload_tx, reload_rx) = mpsc::channel::<(ServiceConfig, String)>(16);
        let (orch_tx, _orch_rx) = mpsc::channel::<OrchestratorMessage>(16);

        let handle = spawn_reload_handler(reload_rx, config, prompt, orch_tx);

        // Drop sender immediately — handler should exit cleanly
        drop(reload_tx);

        let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(result.is_ok(), "handler should exit when sender is dropped");
    }

    // ---- start_workflow_watch tests ----

    #[tokio::test]
    async fn test_start_workflow_watch_detects_modification() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_workflow(tmp.path(), "original", "Do original work.");

        let (watcher, mut rx) = start_workflow_watch(&path).unwrap();

        // Give the watcher time to start. Use a longer delay under instrumented
        // builds (llvm-cov) where startup may be slower.
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Drain any spurious initial events from the watcher starting
        while rx.try_recv().is_ok() {}

        // Modify the file
        let updated_content =
            "---\ntracker:\n  kind: linear\n  project_slug: modified\n  api_key: key123\n---\nDo modified work.\n";
        fs::write(&path, updated_content).unwrap();

        // Wait for the reload event
        let result = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await;
        assert!(result.is_ok());

        let (config, prompt) = result.unwrap().unwrap();
        assert_eq!(config.tracker.project_slug, "modified");
        assert!(prompt.contains("modified work"));

        // Keep watcher alive until assertions are done
        drop(watcher);
    }

    #[tokio::test]
    async fn test_start_workflow_watch_ignores_invalid_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_workflow(tmp.path(), "valid", "Valid prompt.");

        let (watcher, mut rx) = start_workflow_watch(&path).unwrap();

        // Give the watcher time to start, then drain any spurious initial
        // events (some platforms deliver a Create event for the file that
        // existed before the watcher started).
        tokio::time::sleep(Duration::from_millis(300)).await;
        while rx.try_recv().is_ok() {}

        // Write invalid YAML — should log error but not emit event
        let invalid_content = "---\n: :\n  invalid: [unclosed\n---\nBroken.";
        fs::write(&path, invalid_content).unwrap();

        // Should NOT receive an event (error path logs, doesn't send).
        // Use a generous timeout so slow CI runners have time to (not) deliver.
        let result = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        assert!(result.is_err(), "should not receive event for invalid YAML");

        // Now write valid YAML to confirm the watcher is still alive
        let valid_content =
            "---\ntracker:\n  kind: linear\n  project_slug: recovered\n  api_key: key123\n---\nRecovered.\n";
        fs::write(&path, valid_content).unwrap();

        let result = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
        assert!(result.is_ok());

        let (config, _prompt) = result.unwrap().unwrap();
        assert_eq!(config.tracker.project_slug, "recovered");

        drop(watcher);
    }

    // ---- handle_watcher_event tests ----

    #[tokio::test]
    async fn test_handle_watcher_event_non_modify_event_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_workflow(tmp.path(), "test", "Prompt.");
        let (tx, mut rx) = mpsc::channel::<(ServiceConfig, String)>(16);

        // Send a Remove event — should be ignored (not Modify/Create)
        let event = Event {
            kind: EventKind::Remove(notify::event::RemoveKind::File),
            paths: vec![path.clone()],
            attrs: Default::default(),
        };
        handle_watcher_event(Ok(event), &path, &tx);

        // No message should be sent
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_handle_watcher_event_error_result_logged() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_workflow(tmp.path(), "test", "Prompt.");
        let (tx, mut rx) = mpsc::channel::<(ServiceConfig, String)>(16);

        // Send a watcher error
        let err = notify::Error::generic("test watcher error");
        handle_watcher_event(Err(err), &path, &tx);

        // No message should be sent
        assert!(rx.try_recv().is_err());
    }
}
