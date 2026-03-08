use std::path::Path;

use tracing::{info, warn};

use crate::client::OrchestratorClient;
use crate::config::WorkerConfig;
use crate::executor::{self, AgentLauncher};
use crate::workspace::{self, GitCommandRunner};

// ---------------------------------------------------------------------------
// Logging helpers (avoid LLVM coverage splits in tracing macros)
// ---------------------------------------------------------------------------

fn log_starting_worker(worker_id: &str) {
    info!(worker_id = %worker_id, "starting worker main loop");
}

fn log_claimed_work(issue_identifier: &str) {
    info!(issue = %issue_identifier, "claimed work assignment");
}

fn log_no_work() {
    info!("no work available, will poll again");
}

fn log_claim_error(reason: &str, backoff_ms: u64) {
    warn!(reason = %reason, backoff_ms = %backoff_ms, "failed to claim work, backing off");
}

fn log_job_error(reason: &str) {
    warn!(reason = %reason, "job execution failed");
}

fn log_shutdown() {
    info!("shutdown signal received, exiting main loop");
}

fn log_ensure_base_clone(path: &Path) {
    let p = path.display().to_string();
    info!(path = %p, "ensuring base clone exists");
}

fn log_base_clone_error(reason: &str) {
    warn!(reason = %reason, "failed to ensure base clone");
}

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

pub async fn run_main_loop<C, G, A>(
    client: &C,
    git: &G,
    launcher: &A,
    config: &WorkerConfig,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error>>
where
    C: OrchestratorClient,
    G: GitCommandRunner,
    A: AgentLauncher,
{
    log_starting_worker(&config.worker_id);

    // 1. Ensure base clone exists
    log_ensure_base_clone(&config.base_clone_path);
    if let Err(e) =
        workspace::ensure_base_clone(git, &config.repo_url, &config.base_clone_path).await
    {
        log_base_clone_error(&e.to_string());
        return Err(Box::new(e));
    }

    let mut backoff_ms = config.poll_backoff_base_ms;

    // 2. Main loop
    loop {
        // Check shutdown
        if *shutdown_rx.borrow() {
            log_shutdown();
            break;
        }

        // Claim work
        match client.claim_work().await {
            Ok(Some(assignment)) => {
                log_claimed_work(&assignment.issue_identifier);
                // Reset backoff on successful claim
                backoff_ms = config.poll_backoff_base_ms;

                // Create a per-job shutdown receiver
                let job_shutdown_rx = shutdown_rx.clone();

                // Execute the job
                if let Err(e) = executor::execute_job(
                    client,
                    git,
                    launcher,
                    &assignment,
                    &config.base_clone_path,
                    &config.worktree_root,
                    config.turn_timeout_ms,
                    job_shutdown_rx,
                )
                .await
                {
                    log_job_error(&e.to_string());
                    // If it was cancelled, exit the loop
                    if matches!(e, executor::ExecutorError::Cancelled) {
                        break;
                    }
                }
            }
            Ok(None) => {
                log_no_work();
                // Reset backoff on successful poll (even if no work)
                backoff_ms = config.poll_backoff_base_ms;

                // Wait before polling again, but check shutdown
                tokio::select! {
                    _ = tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)) => {}
                    _ = wait_for_shutdown_or_change(&mut shutdown_rx) => {
                        if *shutdown_rx.borrow() {
                            log_shutdown();
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                log_claim_error(&e.to_string(), backoff_ms);

                // Wait with exponential backoff
                tokio::select! {
                    _ = tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)) => {}
                    _ = wait_for_shutdown_or_change(&mut shutdown_rx) => {
                        if *shutdown_rx.borrow() {
                            log_shutdown();
                            break;
                        }
                    }
                }

                // Double backoff, cap at max
                backoff_ms = (backoff_ms * 2).min(config.poll_backoff_max_ms);
            }
        }
    }

    Ok(())
}

/// Wait for shutdown signal change.
async fn wait_for_shutdown_or_change(rx: &mut tokio::sync::watch::Receiver<bool>) {
    let _ = rx.changed().await;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{ClientError, MockOrchestratorClient};
    use crate::executor::{AgentHandle, AgentLauncher};
    use crate::workspace::MockGitCommandRunner;
    use mockall::mock;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use symphony::protocol::{AgentEvent, WorkAssignment, WorkerExitReason};
    use tracing_test::traced_test;

    // Define mock types locally (can't import from executor::tests)
    mock! {
        pub TestAgentHandle {}

        #[async_trait::async_trait]
        impl AgentHandle for TestAgentHandle {
            async fn wait_with_timeout(&mut self, timeout_ms: u64) -> Result<bool, String>;
            async fn kill(&mut self);
        }
    }

    mock! {
        pub TestAgentLauncher {}

        #[async_trait::async_trait]
        impl AgentLauncher for TestAgentLauncher {
            async fn launch_agent(
                &self,
                workspace_path: &std::path::Path,
                prompt: &str,
                issue_identifier: &str,
            ) -> Result<(Box<dyn AgentHandle>, tokio::sync::mpsc::Receiver<AgentEvent>), String>;
        }
    }

    fn make_config() -> WorkerConfig {
        WorkerConfig {
            orchestrator_url: "http://localhost:8080".to_string(),
            auth_token: "secret".to_string(),
            worker_id: "w-1".to_string(),
            repo_url: "https://github.com/org/repo.git".to_string(),
            base_clone_path: PathBuf::from("/tmp/base"),
            worktree_root: PathBuf::from("/tmp/worktrees"),
            agent_command: "claude".to_string(),
            max_turns: 30,
            turn_timeout_ms: 30_000,
            poll_backoff_base_ms: 10, // Small for tests
            poll_backoff_max_ms: 100,
        }
    }

    fn setup_base_clone_mock(git: &mut MockGitCommandRunner) {
        // ensure_base_clone: fetch origin (if .git exists) or clone
        // For tests, we'll assume the path doesn't have .git, so it'll try to clone.
        // But the path doesn't have .git either, so we need to handle the parent dir case.
        // Actually let's just make it expect a clone call.
        git.expect_run_git()
            .withf(|_cwd, args| args.first().map(|s| s.as_str()) == Some("clone"))
            .times(1)
            .returning(|_, _| Ok(String::new()));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_main_loop_immediate_shutdown() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = make_config();
        config.base_clone_path = tmp.path().join("base");

        let mut git = MockGitCommandRunner::new();
        setup_base_clone_mock(&mut git);

        let client = MockOrchestratorClient::new();
        let launcher = MockTestAgentLauncher::new();

        // Start already shut down
        let (_tx, shutdown_rx) = tokio::sync::watch::channel(true);

        let result = run_main_loop(&client, &git, &launcher, &config, shutdown_rx).await;
        assert!(result.is_ok());
        assert!(logs_contain("shutdown signal received"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_main_loop_base_clone_fails() {
        let config = make_config();

        let mut git = MockGitCommandRunner::new();
        git.expect_run_git()
            .withf(|_cwd, args| args.first().map(|s| s.as_str()) == Some("clone"))
            .times(1)
            .returning(|_, _| Err("network error".to_string()));

        let client = MockOrchestratorClient::new();
        let launcher = MockTestAgentLauncher::new();

        let (_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let result = run_main_loop(&client, &git, &launcher, &config, shutdown_rx).await;
        assert!(result.is_err());
        assert!(logs_contain("failed to ensure base clone"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_main_loop_no_work_then_shutdown() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = make_config();
        config.base_clone_path = tmp.path().join("base");

        let mut git = MockGitCommandRunner::new();
        setup_base_clone_mock(&mut git);

        let mut client = MockOrchestratorClient::new();
        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();
        client.expect_claim_work().returning(move || {
            cc.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        });

        let launcher = MockTestAgentLauncher::new();

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        // Shut down after a short delay
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            let _ = shutdown_tx.send(true);
        });

        let result = run_main_loop(&client, &git, &launcher, &config, shutdown_rx).await;
        assert!(result.is_ok());
        assert!(call_count.load(Ordering::SeqCst) >= 1);
        assert!(logs_contain("no work available"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_main_loop_claim_error_backoff() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = make_config();
        config.base_clone_path = tmp.path().join("base");

        let mut git = MockGitCommandRunner::new();
        setup_base_clone_mock(&mut git);

        let mut client = MockOrchestratorClient::new();
        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();
        client.expect_claim_work().returning(move || {
            cc.fetch_add(1, Ordering::SeqCst);
            Err(ClientError::RequestFailed {
                reason: "connection refused".to_string(),
            })
        });

        let launcher = MockTestAgentLauncher::new();

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        // Shut down after a short delay
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
            let _ = shutdown_tx.send(true);
        });

        let result = run_main_loop(&client, &git, &launcher, &config, shutdown_rx).await;
        assert!(result.is_ok());
        assert!(call_count.load(Ordering::SeqCst) >= 1);
        assert!(logs_contain("failed to claim work"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_main_loop_one_job_then_shutdown() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = make_config();
        config.base_clone_path = tmp.path().join("base");
        config.worktree_root = tmp.path().join("worktrees");

        let mut git = MockGitCommandRunner::new();
        setup_base_clone_mock(&mut git);

        // Worktree creation: fetch origin main
        git.expect_run_git()
            .withf(|_cwd, args| args.len() == 3 && args[0] == "fetch" && args[2] == "main")
            .times(1)
            .returning(|_, _| Ok(String::new()));

        // Worktree add
        git.expect_run_git()
            .withf(|_cwd, args| args.len() >= 3 && args[0] == "worktree" && args[1] == "add")
            .times(1)
            .returning(|_, _| Ok(String::new()));

        // Cleanup: worktree remove
        git.expect_run_git()
            .withf(|_cwd, args| args.len() >= 3 && args[0] == "worktree" && args[1] == "remove")
            .times(1)
            .returning(|_, _| Ok(String::new()));

        // Cleanup: branch -D
        git.expect_run_git()
            .withf(|_cwd, args| args.len() >= 2 && args[0] == "branch" && args[1] == "-D")
            .times(1)
            .returning(|_, _| Ok(String::new()));

        // Cleanup: worktree prune
        git.expect_run_git()
            .withf(|_cwd, args| args.len() >= 2 && args[0] == "worktree" && args[1] == "prune")
            .times(1)
            .returning(|_, _| Ok(String::new()));

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let stx = shutdown_tx.clone();

        let mut client = MockOrchestratorClient::new();
        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();
        client.expect_claim_work().returning(move || {
            let count = cc.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                Ok(Some(WorkAssignment {
                    issue_id: "id-1".to_string(),
                    issue_identifier: "TST-42".to_string(),
                    prompt: "Fix the bug".to_string(),
                    attempt: None,
                    workspace_path: None,
                }))
            } else {
                // Shut down after first job
                let _ = stx.send(true);
                Ok(None)
            }
        });
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

        let result = run_main_loop(&client, &git, &launcher, &config, shutdown_rx).await;
        assert!(result.is_ok());
        assert!(logs_contain("claimed work assignment"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_main_loop_job_cancelled_exits() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = make_config();
        config.base_clone_path = tmp.path().join("base");
        config.worktree_root = tmp.path().join("worktrees");

        let mut git = MockGitCommandRunner::new();
        setup_base_clone_mock(&mut git);

        // Worktree creation
        git.expect_run_git()
            .withf(|_cwd, args| args.len() == 3 && args[0] == "fetch" && args[2] == "main")
            .times(1)
            .returning(|_, _| Ok(String::new()));
        git.expect_run_git()
            .withf(|_cwd, args| args.len() >= 3 && args[0] == "worktree" && args[1] == "add")
            .times(1)
            .returning(|_, _| Ok(String::new()));

        // Cleanup
        git.expect_run_git()
            .withf(|_cwd, args| args.len() >= 3 && args[0] == "worktree" && args[1] == "remove")
            .times(1)
            .returning(|_, _| Ok(String::new()));
        git.expect_run_git()
            .withf(|_cwd, args| args.len() >= 2 && args[0] == "branch" && args[1] == "-D")
            .times(1)
            .returning(|_, _| Ok(String::new()));
        git.expect_run_git()
            .withf(|_cwd, args| args.len() >= 2 && args[0] == "worktree" && args[1] == "prune")
            .times(1)
            .returning(|_, _| Ok(String::new()));

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let mut client = MockOrchestratorClient::new();
        client.expect_claim_work().times(1).returning(|| {
            Ok(Some(WorkAssignment {
                issue_id: "id-1".to_string(),
                issue_identifier: "TST-42".to_string(),
                prompt: "Fix the bug".to_string(),
                attempt: None,
                workspace_path: None,
            }))
        });
        client
            .expect_send_complete()
            .withf(|_, reason| *reason == WorkerExitReason::Cancelled)
            .times(1)
            .returning(|_, _| Ok(()));

        let stx = shutdown_tx.clone();
        let mut launcher = MockTestAgentLauncher::new();
        launcher
            .expect_launch_agent()
            .times(1)
            .returning(move |_, _, _| {
                let (tx, rx) = tokio::sync::mpsc::channel::<AgentEvent>(1);
                // Leak the sender so the channel stays open
                std::mem::forget(tx);
                // Send shutdown signal shortly after launch
                let stx2 = stx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                    let _ = stx2.send(true);
                });
                let mut mock_handle = MockTestAgentHandle::new();
                mock_handle
                    .expect_wait_with_timeout()
                    .times(0..=1)
                    .returning(|_| Ok(true));
                mock_handle.expect_kill().times(1).returning(|| ());
                Ok((Box::new(mock_handle), rx))
            });

        let result = run_main_loop(&client, &git, &launcher, &config, shutdown_rx).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_wait_for_shutdown_or_change() {
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            let _ = tx.send(true);
        });
        wait_for_shutdown_or_change(&mut rx).await;
        assert!(*rx.borrow());
    }

    #[tokio::test]
    async fn test_wait_for_shutdown_or_change_sender_dropped() {
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        drop(tx);
        // Should return because sender was dropped
        wait_for_shutdown_or_change(&mut rx).await;
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_main_loop_shutdown_during_idle_wait() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = make_config();
        config.base_clone_path = tmp.path().join("base");
        config.poll_backoff_base_ms = 600_000; // Very long so sleep won't complete

        let mut git = MockGitCommandRunner::new();
        setup_base_clone_mock(&mut git);

        let mut client = MockOrchestratorClient::new();
        client.expect_claim_work().times(1).returning(|| Ok(None));

        let launcher = MockTestAgentLauncher::new();

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let _ = shutdown_tx.send(true);
        });

        let result = run_main_loop(&client, &git, &launcher, &config, shutdown_rx).await;
        assert!(result.is_ok());
        assert!(logs_contain("no work available"));
        assert!(logs_contain("shutdown signal received"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_main_loop_shutdown_during_error_backoff() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = make_config();
        config.base_clone_path = tmp.path().join("base");
        config.poll_backoff_base_ms = 600_000; // Very long backoff

        let mut git = MockGitCommandRunner::new();
        setup_base_clone_mock(&mut git);

        let mut client = MockOrchestratorClient::new();
        client.expect_claim_work().times(1).returning(|| {
            Err(ClientError::RequestFailed {
                reason: "connection refused".to_string(),
            })
        });

        let launcher = MockTestAgentLauncher::new();

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let _ = shutdown_tx.send(true);
        });

        let result = run_main_loop(&client, &git, &launcher, &config, shutdown_rx).await;
        assert!(result.is_ok());
        assert!(logs_contain("failed to claim work"));
        assert!(logs_contain("shutdown signal received"));
    }
}
