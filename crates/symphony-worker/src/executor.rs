use std::path::Path;

use async_trait::async_trait;
use symphony::protocol::{AgentEvent, WorkAssignment, WorkerExitReason};
use tracing::{info, warn};

use crate::client::OrchestratorClient;
use crate::workspace::{self, GitCommandRunner};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    #[error("workspace error: {0}")]
    Workspace(#[from] workspace::WorkspaceError),
    #[error("agent launch failed: {reason}")]
    AgentLaunchFailed { reason: String },
    #[error("job cancelled by shutdown signal")]
    Cancelled,
}

// ---------------------------------------------------------------------------
// AgentLauncher / AgentHandle traits
// ---------------------------------------------------------------------------

#[async_trait]
pub trait AgentLauncher: Send + Sync {
    async fn launch_agent(
        &self,
        workspace_path: &Path,
        prompt: &str,
        issue_identifier: &str,
    ) -> Result<
        (
            Box<dyn AgentHandle>,
            tokio::sync::mpsc::Receiver<AgentEvent>,
        ),
        String,
    >;
}

#[async_trait]
pub trait AgentHandle: Send + Sync {
    async fn wait_with_timeout(&mut self, timeout_ms: u64) -> Result<bool, String>;
    async fn kill(&mut self);
}

// ---------------------------------------------------------------------------
// Logging helpers (avoid LLVM coverage splits in tracing macros)
// ---------------------------------------------------------------------------

fn log_creating_worktree(issue_identifier: &str) {
    info!(issue = %issue_identifier, "creating worktree for job");
}

fn log_launching_agent(issue_identifier: &str) {
    info!(issue = %issue_identifier, "launching agent");
}

fn log_agent_completed(issue_identifier: &str, completed: bool) {
    info!(issue = %issue_identifier, completed = %completed, "agent finished");
}

fn log_event_forward_failed(issue_identifier: &str, reason: &str) {
    warn!(issue = %issue_identifier, reason = %reason, "failed to forward event to orchestrator");
}

fn log_complete_send_failed(issue_identifier: &str, reason: &str) {
    warn!(issue = %issue_identifier, reason = %reason, "failed to send completion to orchestrator");
}

fn log_cleanup_failed(issue_identifier: &str, reason: &str) {
    warn!(issue = %issue_identifier, reason = %reason, "worktree cleanup failed");
}

fn log_shutdown_received(issue_identifier: &str) {
    info!(issue = %issue_identifier, "shutdown signal received, killing agent");
}

// ---------------------------------------------------------------------------
// execute_job
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub async fn execute_job<C, G, A>(
    client: &C,
    git: &G,
    launcher: &A,
    assignment: &WorkAssignment,
    base_clone_path: &Path,
    worktree_root: &Path,
    turn_timeout_ms: u64,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> Result<(), ExecutorError>
where
    C: OrchestratorClient,
    G: GitCommandRunner,
    A: AgentLauncher,
{
    let issue_identifier = &assignment.issue_identifier;
    let branch_name = format!("symphony/{}", issue_identifier.to_lowercase());

    // 1. Create worktree
    log_creating_worktree(issue_identifier);
    let worktree_path = workspace::create_worktree(
        git,
        base_clone_path,
        worktree_root,
        issue_identifier,
        &branch_name,
    )
    .await?;

    // Run the inner job logic, always cleaning up worktree afterwards
    let result = run_agent_loop(
        client,
        launcher,
        assignment,
        &worktree_path,
        turn_timeout_ms,
        &mut shutdown_rx,
    )
    .await;

    // Always clean up worktree
    if let Err(e) =
        workspace::remove_worktree(git, base_clone_path, &worktree_path, &branch_name).await
    {
        log_cleanup_failed(issue_identifier, &e.to_string());
    }

    result
}

/// Inner loop that launches the agent, forwards events inline, and reports
/// completion. Separated from `execute_job` so that worktree cleanup always
/// runs regardless of the outcome.
async fn run_agent_loop<C, A>(
    client: &C,
    launcher: &A,
    assignment: &WorkAssignment,
    worktree_path: &Path,
    turn_timeout_ms: u64,
    shutdown_rx: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<(), ExecutorError>
where
    C: OrchestratorClient,
    A: AgentLauncher,
{
    let issue_identifier = &assignment.issue_identifier;

    // 2. Launch agent
    log_launching_agent(issue_identifier);
    let (mut handle, mut event_rx) = launcher
        .launch_agent(worktree_path, &assignment.prompt, issue_identifier)
        .await
        .map_err(|reason| ExecutorError::AgentLaunchFailed { reason })?;

    // 3. Forward events inline while waiting for agent completion or shutdown.
    //    Events arrive on the mpsc channel. When the channel closes (sender
    //    dropped) the agent has finished producing output; we then wait for
    //    the actual exit status.
    let exit_reason = loop {
        tokio::select! {
            biased;
            event = event_rx.recv() => {
                match event {
                    Some(ev) => {
                        if let Err(e) = client.send_event(issue_identifier, ev).await {
                            log_event_forward_failed(issue_identifier, &e.to_string());
                        }
                    }
                    None => {
                        // Channel closed — agent finished producing output.
                        // Now wait for the exit status.
                        match handle.wait_with_timeout(turn_timeout_ms).await {
                            Ok(true) => {
                                log_agent_completed(issue_identifier, true);
                                break WorkerExitReason::Normal;
                            }
                            Ok(false) => {
                                log_agent_completed(issue_identifier, false);
                                break WorkerExitReason::TimedOut;
                            }
                            Err(e) => {
                                log_agent_completed(issue_identifier, false);
                                break WorkerExitReason::Failed(e);
                            }
                        }
                    }
                }
            }
            _ = wait_for_shutdown(shutdown_rx) => {
                log_shutdown_received(issue_identifier);
                handle.kill().await;
                break WorkerExitReason::Cancelled;
            }
        }
    };

    // 4. Send completion report
    if let Err(e) = client
        .send_complete(issue_identifier, exit_reason.clone())
        .await
    {
        log_complete_send_failed(issue_identifier, &e.to_string());
    }

    // 5. Return Cancelled as error so callers can distinguish it
    if matches!(exit_reason, WorkerExitReason::Cancelled) {
        return Err(ExecutorError::Cancelled);
    }

    Ok(())
}

/// Wait until the shutdown signal becomes true or the sender is dropped.
async fn wait_for_shutdown(shutdown_rx: &mut tokio::sync::watch::Receiver<bool>) {
    loop {
        if *shutdown_rx.borrow() {
            return;
        }
        if shutdown_rx.changed().await.is_err() {
            // Sender dropped — treat as shutdown
            return;
        }
        if *shutdown_rx.borrow() {
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{ClientError, MockOrchestratorClient};
    use crate::workspace::MockGitCommandRunner;
    use mockall::mock;
    use tracing_test::traced_test;

    // Mock AgentHandle
    mock! {
        pub TestAgentHandle {}

        #[async_trait]
        impl AgentHandle for TestAgentHandle {
            async fn wait_with_timeout(&mut self, timeout_ms: u64) -> Result<bool, String>;
            async fn kill(&mut self);
        }
    }

    // Mock AgentLauncher
    mock! {
        pub TestAgentLauncher {}

        #[async_trait]
        impl AgentLauncher for TestAgentLauncher {
            async fn launch_agent(
                &self,
                workspace_path: &Path,
                prompt: &str,
                issue_identifier: &str,
            ) -> Result<(Box<dyn AgentHandle>, tokio::sync::mpsc::Receiver<AgentEvent>), String>;
        }
    }

    fn make_assignment() -> WorkAssignment {
        WorkAssignment {
            issue_id: "id-1".to_string(),
            issue_identifier: "TST-42".to_string(),
            prompt: "Fix the bug".to_string(),
            attempt: None,
            workspace_path: None,
        }
    }

    fn setup_git_mock_for_worktree(mock: &mut MockGitCommandRunner) {
        // fetch origin main
        mock.expect_run_git()
            .withf(|_cwd, args| {
                args.len() == 3 && args[0] == "fetch" && args[1] == "origin" && args[2] == "main"
            })
            .times(1)
            .returning(|_, _| Ok(String::new()));

        // worktree add
        mock.expect_run_git()
            .withf(|_cwd, args| args.len() >= 3 && args[0] == "worktree" && args[1] == "add")
            .times(1)
            .returning(|_, _| Ok(String::new()));
    }

    fn setup_git_mock_for_cleanup(mock: &mut MockGitCommandRunner) {
        // worktree remove
        mock.expect_run_git()
            .withf(|_cwd, args| args.len() >= 3 && args[0] == "worktree" && args[1] == "remove")
            .times(1)
            .returning(|_, _| Ok(String::new()));

        // branch -D (best-effort)
        mock.expect_run_git()
            .withf(|_cwd, args| args.len() >= 2 && args[0] == "branch" && args[1] == "-D")
            .times(1)
            .returning(|_, _| Ok(String::new()));

        // worktree prune (best-effort)
        mock.expect_run_git()
            .withf(|_cwd, args| args.len() >= 2 && args[0] == "worktree" && args[1] == "prune")
            .times(1)
            .returning(|_, _| Ok(String::new()));
    }

    // ---- Error Display tests -----------------------------------------------

    #[test]
    fn test_error_display_workspace() {
        let err = ExecutorError::Workspace(workspace::WorkspaceError::FetchFailed {
            path: "/tmp".to_string(),
            reason: "timeout".to_string(),
        });
        let msg = err.to_string();
        assert!(msg.contains("workspace error"));
        assert!(msg.contains("failed to fetch"));
    }

    #[test]
    fn test_error_display_agent_launch_failed() {
        let err = ExecutorError::AgentLaunchFailed {
            reason: "no binary".to_string(),
        };
        assert_eq!(err.to_string(), "agent launch failed: no binary");
    }

    #[test]
    fn test_error_display_cancelled() {
        let err = ExecutorError::Cancelled;
        assert_eq!(err.to_string(), "job cancelled by shutdown signal");
    }

    #[test]
    fn test_error_is_debug() {
        let err = ExecutorError::Cancelled;
        let debug = format!("{:?}", err);
        assert!(debug.contains("Cancelled"));
    }

    #[test]
    fn test_workspace_error_converts_to_executor_error() {
        let ws_err = workspace::WorkspaceError::FetchFailed {
            path: "/tmp".to_string(),
            reason: "err".to_string(),
        };
        let exec_err: ExecutorError = ws_err.into();
        assert!(matches!(exec_err, ExecutorError::Workspace(_)));
    }

    // ---- execute_job tests -------------------------------------------------

    #[traced_test]
    #[tokio::test]
    async fn test_execute_job_success() {
        let tmp = tempfile::tempdir().unwrap();
        let base_clone = tmp.path().join("base");
        let wt_root = tmp.path().join("worktrees");
        std::fs::create_dir_all(&base_clone).unwrap();

        let mut git = MockGitCommandRunner::new();
        setup_git_mock_for_worktree(&mut git);
        setup_git_mock_for_cleanup(&mut git);

        let mut client = MockOrchestratorClient::new();
        client
            .expect_send_complete()
            .withf(|id, reason| id == "TST-42" && *reason == WorkerExitReason::Normal)
            .times(1)
            .returning(|_, _| Ok(()));

        let mut launcher = MockTestAgentLauncher::new();
        launcher
            .expect_launch_agent()
            .times(1)
            .returning(|_, _, _| {
                let (tx, rx) = tokio::sync::mpsc::channel(1);
                drop(tx); // Close immediately — no events
                let mut mock_handle = MockTestAgentHandle::new();
                mock_handle
                    .expect_wait_with_timeout()
                    .times(1)
                    .returning(|_| Ok(true));
                Ok((Box::new(mock_handle), rx))
            });

        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let assignment = make_assignment();

        let result = execute_job(
            &client,
            &git,
            &launcher,
            &assignment,
            &base_clone,
            &wt_root,
            30_000,
            shutdown_rx,
        )
        .await;

        assert!(result.is_ok());
    }

    #[traced_test]
    #[tokio::test]
    async fn test_execute_job_worktree_creation_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let base_clone = tmp.path().join("base");
        let wt_root = tmp.path().join("worktrees");
        std::fs::create_dir_all(&base_clone).unwrap();

        let mut git = MockGitCommandRunner::new();
        // fetch succeeds
        git.expect_run_git()
            .withf(|_cwd, args| args.len() == 3 && args[0] == "fetch")
            .times(1)
            .returning(|_, _| Ok(String::new()));
        // worktree add fails
        git.expect_run_git()
            .withf(|_cwd, args| args.len() >= 3 && args[0] == "worktree" && args[1] == "add")
            .times(1)
            .returning(|_, _| Err("worktree error".to_string()));

        let client = MockOrchestratorClient::new();
        let launcher = MockTestAgentLauncher::new();

        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let assignment = make_assignment();

        let result = execute_job(
            &client,
            &git,
            &launcher,
            &assignment,
            &base_clone,
            &wt_root,
            30_000,
            shutdown_rx,
        )
        .await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ExecutorError::Workspace(_)));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_execute_job_agent_launch_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let base_clone = tmp.path().join("base");
        let wt_root = tmp.path().join("worktrees");
        std::fs::create_dir_all(&base_clone).unwrap();

        let mut git = MockGitCommandRunner::new();
        setup_git_mock_for_worktree(&mut git);
        setup_git_mock_for_cleanup(&mut git);

        let client = MockOrchestratorClient::new();

        let mut launcher = MockTestAgentLauncher::new();
        launcher
            .expect_launch_agent()
            .times(1)
            .returning(|_, _, _| Err("no binary found".to_string()));

        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let assignment = make_assignment();

        let result = execute_job(
            &client,
            &git,
            &launcher,
            &assignment,
            &base_clone,
            &wt_root,
            30_000,
            shutdown_rx,
        )
        .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ExecutorError::AgentLaunchFailed { reason } => {
                assert_eq!(reason, "no binary found");
            }
            other => panic!("expected AgentLaunchFailed, got {:?}", other),
        }
    }

    #[traced_test]
    #[tokio::test]
    async fn test_execute_job_agent_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let base_clone = tmp.path().join("base");
        let wt_root = tmp.path().join("worktrees");
        std::fs::create_dir_all(&base_clone).unwrap();

        let mut git = MockGitCommandRunner::new();
        setup_git_mock_for_worktree(&mut git);
        setup_git_mock_for_cleanup(&mut git);

        let mut client = MockOrchestratorClient::new();
        client
            .expect_send_complete()
            .withf(|id, reason| id == "TST-42" && *reason == WorkerExitReason::TimedOut)
            .times(1)
            .returning(|_, _| Ok(()));

        let mut launcher = MockTestAgentLauncher::new();
        launcher
            .expect_launch_agent()
            .times(1)
            .returning(|_, _, _| {
                let (tx, rx) = tokio::sync::mpsc::channel(1);
                drop(tx);
                let mut mock_handle = MockTestAgentHandle::new();
                mock_handle
                    .expect_wait_with_timeout()
                    .times(1)
                    .returning(|_| Ok(false));
                Ok((Box::new(mock_handle), rx))
            });

        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let assignment = make_assignment();

        let result = execute_job(
            &client,
            &git,
            &launcher,
            &assignment,
            &base_clone,
            &wt_root,
            30_000,
            shutdown_rx,
        )
        .await;

        assert!(result.is_ok());
    }

    #[traced_test]
    #[tokio::test]
    async fn test_execute_job_agent_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let base_clone = tmp.path().join("base");
        let wt_root = tmp.path().join("worktrees");
        std::fs::create_dir_all(&base_clone).unwrap();

        let mut git = MockGitCommandRunner::new();
        setup_git_mock_for_worktree(&mut git);
        setup_git_mock_for_cleanup(&mut git);

        let mut client = MockOrchestratorClient::new();
        client
            .expect_send_complete()
            .withf(|id, reason| id == "TST-42" && matches!(reason, WorkerExitReason::Failed(_)))
            .times(1)
            .returning(|_, _| Ok(()));

        let mut launcher = MockTestAgentLauncher::new();
        launcher
            .expect_launch_agent()
            .times(1)
            .returning(|_, _, _| {
                let (tx, rx) = tokio::sync::mpsc::channel(1);
                drop(tx);
                let mut mock_handle = MockTestAgentHandle::new();
                mock_handle
                    .expect_wait_with_timeout()
                    .times(1)
                    .returning(|_| Err("process crashed".to_string()));
                Ok((Box::new(mock_handle), rx))
            });

        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let assignment = make_assignment();

        let result = execute_job(
            &client,
            &git,
            &launcher,
            &assignment,
            &base_clone,
            &wt_root,
            30_000,
            shutdown_rx,
        )
        .await;

        assert!(result.is_ok());
    }

    #[traced_test]
    #[tokio::test]
    async fn test_execute_job_shutdown_signal() {
        let tmp = tempfile::tempdir().unwrap();
        let base_clone = tmp.path().join("base");
        let wt_root = tmp.path().join("worktrees");
        std::fs::create_dir_all(&base_clone).unwrap();

        let mut git = MockGitCommandRunner::new();
        setup_git_mock_for_worktree(&mut git);
        setup_git_mock_for_cleanup(&mut git);

        let mut client = MockOrchestratorClient::new();
        client
            .expect_send_complete()
            .withf(|id, reason| id == "TST-42" && *reason == WorkerExitReason::Cancelled)
            .times(1)
            .returning(|_, _| Ok(()));

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let mut launcher = MockTestAgentLauncher::new();
        launcher
            .expect_launch_agent()
            .times(1)
            .returning(move |_, _, _| {
                let (tx, rx) = tokio::sync::mpsc::channel::<AgentEvent>(1);
                // Leak the sender so the channel stays open (agent won't
                // complete on its own — we rely on the shutdown signal).
                std::mem::forget(tx);
                let mut mock_handle = MockTestAgentHandle::new();
                mock_handle
                    .expect_wait_with_timeout()
                    .times(0..=1)
                    .returning(|_| Ok(true));
                mock_handle.expect_kill().times(1).returning(|| ());
                Ok((Box::new(mock_handle), rx))
            });

        let assignment = make_assignment();

        // Send shutdown signal shortly after start
        let shutdown_tx_clone = shutdown_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let _ = shutdown_tx_clone.send(true);
        });

        let result = execute_job(
            &client,
            &git,
            &launcher,
            &assignment,
            &base_clone,
            &wt_root,
            30_000,
            shutdown_rx,
        )
        .await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ExecutorError::Cancelled));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_execute_job_event_send_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let base_clone = tmp.path().join("base");
        let wt_root = tmp.path().join("worktrees");
        std::fs::create_dir_all(&base_clone).unwrap();

        let mut git = MockGitCommandRunner::new();
        setup_git_mock_for_worktree(&mut git);
        setup_git_mock_for_cleanup(&mut git);

        let mut client = MockOrchestratorClient::new();
        // Event forwarding fails
        client.expect_send_event().returning(|_, _| {
            Err(ClientError::BadStatus {
                status: 500,
                body: "error".to_string(),
            })
        });
        // Complete succeeds
        client
            .expect_send_complete()
            .times(1)
            .returning(|_, _| Ok(()));

        let mut launcher = MockTestAgentLauncher::new();
        launcher
            .expect_launch_agent()
            .times(1)
            .returning(|_, _, _| {
                let (tx, rx) = tokio::sync::mpsc::channel(10);
                // Send an event then close the channel
                tokio::spawn(async move {
                    let _ = tx
                        .send(AgentEvent::Notification {
                            message: "hello".to_string(),
                        })
                        .await;
                    // tx is dropped here, closing the channel
                });
                let mut mock_handle = MockTestAgentHandle::new();
                mock_handle
                    .expect_wait_with_timeout()
                    .times(1)
                    .returning(|_| Ok(true));
                Ok((Box::new(mock_handle), rx))
            });

        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let assignment = make_assignment();

        let result = execute_job(
            &client,
            &git,
            &launcher,
            &assignment,
            &base_clone,
            &wt_root,
            30_000,
            shutdown_rx,
        )
        .await;

        // Job should still succeed even though event forwarding failed
        assert!(result.is_ok());
        assert!(logs_contain("failed to forward event"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_execute_job_complete_send_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let base_clone = tmp.path().join("base");
        let wt_root = tmp.path().join("worktrees");
        std::fs::create_dir_all(&base_clone).unwrap();

        let mut git = MockGitCommandRunner::new();
        setup_git_mock_for_worktree(&mut git);
        setup_git_mock_for_cleanup(&mut git);

        let mut client = MockOrchestratorClient::new();
        client.expect_send_complete().times(1).returning(|_, _| {
            Err(ClientError::BadStatus {
                status: 500,
                body: "error".to_string(),
            })
        });

        let mut launcher = MockTestAgentLauncher::new();
        launcher
            .expect_launch_agent()
            .times(1)
            .returning(|_, _, _| {
                let (tx, rx) = tokio::sync::mpsc::channel(1);
                drop(tx);
                let mut mock_handle = MockTestAgentHandle::new();
                mock_handle
                    .expect_wait_with_timeout()
                    .times(1)
                    .returning(|_| Ok(true));
                Ok((Box::new(mock_handle), rx))
            });

        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let assignment = make_assignment();

        let result = execute_job(
            &client,
            &git,
            &launcher,
            &assignment,
            &base_clone,
            &wt_root,
            30_000,
            shutdown_rx,
        )
        .await;

        // Job still returns Ok even though completion report failed
        assert!(result.is_ok());
        assert!(logs_contain("failed to send completion"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_execute_job_cleanup_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let base_clone = tmp.path().join("base");
        let wt_root = tmp.path().join("worktrees");
        std::fs::create_dir_all(&base_clone).unwrap();

        let mut git = MockGitCommandRunner::new();
        setup_git_mock_for_worktree(&mut git);

        // Cleanup fails
        git.expect_run_git()
            .withf(|_cwd, args| args.len() >= 3 && args[0] == "worktree" && args[1] == "remove")
            .times(1)
            .returning(|_, _| Err("cleanup error".to_string()));

        let mut client = MockOrchestratorClient::new();
        client
            .expect_send_complete()
            .times(1)
            .returning(|_, _| Ok(()));

        let mut launcher = MockTestAgentLauncher::new();
        launcher
            .expect_launch_agent()
            .times(1)
            .returning(|_, _, _| {
                let (tx, rx) = tokio::sync::mpsc::channel(1);
                drop(tx);
                let mut mock_handle = MockTestAgentHandle::new();
                mock_handle
                    .expect_wait_with_timeout()
                    .times(1)
                    .returning(|_| Ok(true));
                Ok((Box::new(mock_handle), rx))
            });

        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let assignment = make_assignment();

        let result = execute_job(
            &client,
            &git,
            &launcher,
            &assignment,
            &base_clone,
            &wt_root,
            30_000,
            shutdown_rx,
        )
        .await;

        // Job still returns Ok even though cleanup failed
        assert!(result.is_ok());
        assert!(logs_contain("worktree cleanup failed"));
    }

    // ---- wait_for_shutdown tests -------------------------------------------

    #[tokio::test]
    async fn test_wait_for_shutdown_immediate_true() {
        let (_tx, mut rx) = tokio::sync::watch::channel(true);
        // Should return immediately since value is already true
        wait_for_shutdown(&mut rx).await;
    }

    #[tokio::test]
    async fn test_wait_for_shutdown_sender_dropped() {
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        drop(tx); // Sender dropped
                  // Should return because sender dropped
        wait_for_shutdown(&mut rx).await;
    }

    #[tokio::test]
    async fn test_wait_for_shutdown_signal_received() {
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            let _ = tx.send(true);
        });
        wait_for_shutdown(&mut rx).await;
    }

    #[tokio::test]
    async fn test_wait_for_shutdown_false_then_true() {
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            // Send false first — changed() fires but value is still false, loop continues
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            let _ = tx.send(false);
            // Then send true — changed() fires and value is true, returns
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            let _ = tx.send(true);
        });
        wait_for_shutdown(&mut rx).await;
    }

    // ---- run_agent_loop direct tests (without worktree management) ---------

    #[traced_test]
    #[tokio::test]
    async fn test_run_agent_loop_success_with_events() {
        let tmp = tempfile::tempdir().unwrap();
        let wt_path = tmp.path().join("worktree");
        std::fs::create_dir_all(&wt_path).unwrap();

        let mut client = MockOrchestratorClient::new();
        client.expect_send_event().times(1).returning(|_, _| Ok(()));
        client
            .expect_send_complete()
            .withf(|id, reason| id == "TST-42" && *reason == WorkerExitReason::Normal)
            .times(1)
            .returning(|_, _| Ok(()));

        let mut launcher = MockTestAgentLauncher::new();
        launcher
            .expect_launch_agent()
            .times(1)
            .returning(|_, _, _| {
                let (tx, rx) = tokio::sync::mpsc::channel(10);
                tokio::spawn(async move {
                    let _ = tx
                        .send(AgentEvent::Notification {
                            message: "working".to_string(),
                        })
                        .await;
                    // Drop tx to close channel
                });
                let mut mock_handle = MockTestAgentHandle::new();
                mock_handle
                    .expect_wait_with_timeout()
                    .times(1)
                    .returning(|_| Ok(true));
                Ok((Box::new(mock_handle), rx))
            });

        let (_shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
        let assignment = make_assignment();

        let result = run_agent_loop(
            &client,
            &launcher,
            &assignment,
            &wt_path,
            30_000,
            &mut shutdown_rx,
        )
        .await;

        assert!(result.is_ok());
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_agent_loop_launch_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let wt_path = tmp.path().join("worktree");
        std::fs::create_dir_all(&wt_path).unwrap();

        let client = MockOrchestratorClient::new();

        let mut launcher = MockTestAgentLauncher::new();
        launcher
            .expect_launch_agent()
            .times(1)
            .returning(|_, _, _| Err("no binary".to_string()));

        let (_shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
        let assignment = make_assignment();

        let result = run_agent_loop(
            &client,
            &launcher,
            &assignment,
            &wt_path,
            30_000,
            &mut shutdown_rx,
        )
        .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ExecutorError::AgentLaunchFailed { .. }
        ));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_agent_loop_shutdown() {
        let tmp = tempfile::tempdir().unwrap();
        let wt_path = tmp.path().join("worktree");
        std::fs::create_dir_all(&wt_path).unwrap();

        let mut client = MockOrchestratorClient::new();
        client
            .expect_send_complete()
            .withf(|id, reason| id == "TST-42" && *reason == WorkerExitReason::Cancelled)
            .times(1)
            .returning(|_, _| Ok(()));

        let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

        let mut launcher = MockTestAgentLauncher::new();
        launcher
            .expect_launch_agent()
            .times(1)
            .returning(move |_, _, _| {
                let (tx, rx) = tokio::sync::mpsc::channel::<AgentEvent>(10);
                // Leak the sender so the channel stays open
                std::mem::forget(tx);
                let mut mock_handle = MockTestAgentHandle::new();
                mock_handle
                    .expect_wait_with_timeout()
                    .times(0..=1)
                    .returning(|_| Ok(true));
                mock_handle.expect_kill().times(1).returning(|| ());
                Ok((Box::new(mock_handle), rx))
            });

        let assignment = make_assignment();

        // Send shutdown after a short delay
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let _ = shutdown_tx.send(true);
        });

        let result = run_agent_loop(
            &client,
            &launcher,
            &assignment,
            &wt_path,
            30_000,
            &mut shutdown_rx,
        )
        .await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ExecutorError::Cancelled));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_agent_loop_complete_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let wt_path = tmp.path().join("worktree");
        std::fs::create_dir_all(&wt_path).unwrap();

        let mut client = MockOrchestratorClient::new();
        client.expect_send_complete().times(1).returning(|_, _| {
            Err(ClientError::BadStatus {
                status: 500,
                body: "err".to_string(),
            })
        });

        let mut launcher = MockTestAgentLauncher::new();
        launcher
            .expect_launch_agent()
            .times(1)
            .returning(|_, _, _| {
                let (tx, rx) = tokio::sync::mpsc::channel(1);
                drop(tx);
                let mut mock_handle = MockTestAgentHandle::new();
                mock_handle
                    .expect_wait_with_timeout()
                    .times(1)
                    .returning(|_| Ok(true));
                Ok((Box::new(mock_handle), rx))
            });

        let (_shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
        let assignment = make_assignment();

        let result = run_agent_loop(
            &client,
            &launcher,
            &assignment,
            &wt_path,
            30_000,
            &mut shutdown_rx,
        )
        .await;

        assert!(result.is_ok());
        assert!(logs_contain("failed to send completion"));
    }
}
