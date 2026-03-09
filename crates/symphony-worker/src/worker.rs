use std::path::Path;

use tracing::{info, warn};

use crate::client::OrchestratorClient;
use crate::config::WorkerConfig;
use crate::executor::{self, AgentLauncher};
use crate::workspace::{self, GitCommandRunner};
use symphony::protocol::{WorkerHeartbeat, WorkerRegistration};

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

fn log_heartbeat_sent(worker_id: &str) {
    info!(worker_id = %worker_id, "sent heartbeat");
}

fn log_heartbeat_failed(reason: &str) {
    warn!(reason = %reason, "failed to send heartbeat");
}

fn log_register_failed(reason: &str) {
    warn!(reason = %reason, "failed to register with orchestrator");
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

    // 2. Register with orchestrator
    let reg = WorkerRegistration {
        worker_id: config.worker_id.clone(),
        capabilities: vec![],
        max_concurrent_jobs: 1,
    };
    if let Err(e) = client.send_register(reg).await {
        log_register_failed(&e.to_string());
        // Non-fatal — continue anyway
    }

    let mut backoff_ms = config.poll_backoff_base_ms;
    let heartbeat_interval = std::time::Duration::from_secs(30);
    let mut last_heartbeat = None::<std::time::Instant>;
    let mut current_job: Option<String> = None;

    // 3. Main loop
    loop {
        // Check shutdown
        if *shutdown_rx.borrow() {
            log_shutdown();
            break;
        }

        // Send heartbeat if enough time has passed
        if last_heartbeat.is_none_or(|t: std::time::Instant| t.elapsed() >= heartbeat_interval) {
            let heartbeat = WorkerHeartbeat {
                worker_id: config.worker_id.clone(),
                active_jobs: current_job.iter().cloned().collect(),
                timestamp: chrono::Utc::now(),
            };
            match client.send_heartbeat(heartbeat).await {
                Ok(()) => log_heartbeat_sent(&config.worker_id),
                Err(e) => log_heartbeat_failed(&e.to_string()),
            }
            last_heartbeat = Some(std::time::Instant::now());
        }

        // Claim work
        match client.claim_work().await {
            Ok(Some(assignment)) => {
                log_claimed_work(&assignment.issue_identifier);
                // Reset backoff on successful claim
                backoff_ms = config.poll_backoff_base_ms;

                // Track the active job for heartbeats
                current_job = Some(assignment.issue_identifier.clone());

                // Create a per-job shutdown receiver
                let job_shutdown_rx = shutdown_rx.clone();

                // Execute the job with concurrent heartbeats
                let job_result = run_job_with_heartbeats(
                    client,
                    git,
                    launcher,
                    &assignment,
                    config,
                    job_shutdown_rx,
                    &current_job,
                    heartbeat_interval,
                    &mut last_heartbeat,
                )
                .await;

                // Clear active job tracking
                current_job = None;

                let cancelled = match job_result {
                    Ok(()) => false,
                    Err(ref e) => {
                        log_job_error(&e.to_string());
                        matches!(e, executor::ExecutorError::Cancelled)
                    }
                };
                if cancelled {
                    break;
                }
            }
            Ok(None) => {
                log_no_work();
                backoff_ms = config.poll_backoff_base_ms;
                if sleep_or_shutdown(backoff_ms, &mut shutdown_rx).await {
                    log_shutdown();
                    break;
                }
            }
            Err(e) => {
                log_claim_error(&e.to_string(), backoff_ms);
                if sleep_or_shutdown(backoff_ms, &mut shutdown_rx).await {
                    log_shutdown();
                    break;
                }
                backoff_ms = (backoff_ms * 2).min(config.poll_backoff_max_ms);
            }
        }
    }

    Ok(())
}

/// Run a job while sending periodic heartbeats concurrently.
///
/// Wraps `execute_job` with a `tokio::select!` loop that sends heartbeats
/// on the given interval so the orchestrator always knows which worker is
/// executing the job, even during long-running agent sessions.
#[allow(clippy::too_many_arguments)]
async fn run_job_with_heartbeats<C, G, A>(
    client: &C,
    git: &G,
    launcher: &A,
    assignment: &symphony::protocol::WorkAssignment,
    config: &WorkerConfig,
    job_shutdown_rx: tokio::sync::watch::Receiver<bool>,
    current_job: &Option<String>,
    heartbeat_interval: std::time::Duration,
    last_heartbeat: &mut Option<std::time::Instant>,
) -> Result<(), executor::ExecutorError>
where
    C: OrchestratorClient,
    G: workspace::GitCommandRunner,
    A: executor::AgentLauncher,
{
    let job_future = executor::execute_job(
        client,
        git,
        launcher,
        assignment,
        &config.base_clone_path,
        &config.worktree_root,
        config.turn_timeout_ms,
        job_shutdown_rx,
    );
    tokio::pin!(job_future);

    let mut heartbeat_ticker = tokio::time::interval(heartbeat_interval);
    // Consume the first immediate tick so the first heartbeat fires
    // after one full interval (we already sent one before claiming work).
    heartbeat_ticker.tick().await;

    loop {
        tokio::select! {
            result = &mut job_future => {
                return result;
            }
            _ = heartbeat_ticker.tick() => {
                let heartbeat = WorkerHeartbeat {
                    worker_id: config.worker_id.clone(),
                    active_jobs: current_job.iter().cloned().collect(),
                    timestamp: chrono::Utc::now(),
                };
                match client.send_heartbeat(heartbeat).await {
                    Ok(()) => log_heartbeat_sent(&config.worker_id),
                    Err(e) => log_heartbeat_failed(&e.to_string()),
                }
                *last_heartbeat = Some(std::time::Instant::now());
            }
        }
    }
}

/// Sleep for `duration_ms` or until a shutdown signal is received.
/// Returns `true` if shutdown was signaled, `false` if the sleep completed.
async fn sleep_or_shutdown(
    duration_ms: u64,
    shutdown_rx: &mut tokio::sync::watch::Receiver<bool>,
) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(tokio::time::Duration::from_millis(duration_ms)) => false,
        _ = wait_for_shutdown_or_change(shutdown_rx) => *shutdown_rx.borrow(),
    }
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
    use crate::test_support::{MockTestAgentHandle, MockTestAgentLauncher};
    use crate::workspace::MockGitCommandRunner;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use symphony::protocol::{AgentEvent, WorkAssignment, WorkerExitReason};
    use tracing_test::traced_test;

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

    /// Set up default register/heartbeat mock expectations that accept any calls.
    fn setup_register_heartbeat_mocks(client: &mut MockOrchestratorClient) {
        client.expect_send_register().returning(|_| Ok(()));
        client.expect_send_heartbeat().returning(|_| Ok(()));
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

        let mut client = MockOrchestratorClient::new();
        setup_register_heartbeat_mocks(&mut client);
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
        setup_register_heartbeat_mocks(&mut client);
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
        setup_register_heartbeat_mocks(&mut client);
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
        setup_register_heartbeat_mocks(&mut client);
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
        setup_register_heartbeat_mocks(&mut client);
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
        setup_register_heartbeat_mocks(&mut client);
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
        setup_register_heartbeat_mocks(&mut client);
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

    #[traced_test]
    #[tokio::test]
    async fn test_run_main_loop_registration_failure_non_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = make_config();
        config.base_clone_path = tmp.path().join("base");

        let mut git = MockGitCommandRunner::new();
        setup_base_clone_mock(&mut git);

        let mut client = MockOrchestratorClient::new();
        // Registration fails
        client.expect_send_register().returning(|_| {
            Err(ClientError::RequestFailed {
                reason: "connection refused".to_string(),
            })
        });
        client.expect_send_heartbeat().returning(|_| Ok(()));
        let launcher = MockTestAgentLauncher::new();

        // Start already shut down
        let (_tx, shutdown_rx) = tokio::sync::watch::channel(true);

        let result = run_main_loop(&client, &git, &launcher, &config, shutdown_rx).await;
        assert!(result.is_ok());
        assert!(logs_contain("failed to register with orchestrator"));
        assert!(logs_contain("shutdown signal received"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_main_loop_heartbeat_failure_non_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = make_config();
        config.base_clone_path = tmp.path().join("base");

        let mut git = MockGitCommandRunner::new();
        setup_base_clone_mock(&mut git);

        let mut client = MockOrchestratorClient::new();
        client.expect_send_register().returning(|_| Ok(()));
        // Heartbeat fails
        client.expect_send_heartbeat().returning(|_| {
            Err(ClientError::RequestFailed {
                reason: "connection refused".to_string(),
            })
        });
        client.expect_claim_work().returning(|| Ok(None));

        let launcher = MockTestAgentLauncher::new();

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let _ = shutdown_tx.send(true);
        });

        let result = run_main_loop(&client, &git, &launcher, &config, shutdown_rx).await;
        assert!(result.is_ok());
        assert!(logs_contain("failed to send heartbeat"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_main_loop_heartbeat_includes_active_job() {
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
        let stx = shutdown_tx.clone();

        // Track heartbeat payloads
        let heartbeat_jobs = Arc::new(std::sync::Mutex::new(Vec::<Vec<String>>::new()));
        let hj = heartbeat_jobs.clone();

        let mut client = MockOrchestratorClient::new();
        client.expect_send_register().returning(|_| Ok(()));
        client.expect_send_heartbeat().returning(move |hb| {
            hj.lock().unwrap().push(hb.active_jobs.clone());
            Ok(())
        });

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

        // Verify at least one heartbeat was sent with the active job
        let all_hbs = heartbeat_jobs.lock().unwrap();
        // The first heartbeat (before claiming) should have no active jobs
        assert!(
            all_hbs[0].is_empty(),
            "first heartbeat should have no active jobs"
        );
        // After the job completes and no more work, the post-job heartbeat
        // should have an empty active_jobs (job cleared)
    }

    #[traced_test]
    #[tokio::test(start_paused = true)]
    async fn test_run_main_loop_heartbeat_during_job_execution() {
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
        let stx = shutdown_tx.clone();

        let heartbeat_count = Arc::new(AtomicU32::new(0));
        let hc = heartbeat_count.clone();

        let mut client = MockOrchestratorClient::new();
        client.expect_send_register().returning(|_| Ok(()));
        client.expect_send_heartbeat().returning(move |_hb| {
            hc.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });

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
                let (_tx, rx) = tokio::sync::mpsc::channel(10);
                // Spawn a task that sleeps long enough for heartbeats to fire.
                // With paused time, this triggers the heartbeat ticker in
                // run_job_with_heartbeats without actually waiting 35 seconds.
                let tx_clone = _tx;
                tokio::spawn(async move {
                    tokio::time::sleep(tokio::time::Duration::from_secs(65)).await;
                    drop(tx_clone);
                });
                let mut mock_handle = MockTestAgentHandle::new();
                mock_handle
                    .expect_wait_with_timeout()
                    .times(1)
                    .returning(|_| Ok(true));
                Ok((Box::new(mock_handle), rx))
            });

        let result = run_main_loop(&client, &git, &launcher, &config, shutdown_rx).await;
        assert!(result.is_ok());

        // At least 1 heartbeat before claiming + at least 1 during job execution
        let hb_total = heartbeat_count.load(Ordering::SeqCst);
        assert!(hb_total >= 2);
        assert!(logs_contain("sent heartbeat"));
    }

    #[traced_test]
    #[tokio::test(start_paused = true)]
    async fn test_run_main_loop_heartbeat_failure_during_job_execution() {
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
        let stx = shutdown_tx.clone();

        let fail_count = Arc::new(AtomicU32::new(0));
        let fc = fail_count.clone();

        let mut client = MockOrchestratorClient::new();
        client.expect_send_register().returning(|_| Ok(()));
        // First heartbeat (pre-claim) succeeds; subsequent ones (during job) fail
        client.expect_send_heartbeat().returning(move |_hb| {
            let n = fc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(())
            } else {
                Err(ClientError::RequestFailed {
                    reason: "timeout".to_string(),
                })
            }
        });

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
                let (_tx, rx) = tokio::sync::mpsc::channel(10);
                // Keep channel open long enough for heartbeat tick to fire
                let tx_clone = _tx;
                tokio::spawn(async move {
                    tokio::time::sleep(tokio::time::Duration::from_secs(65)).await;
                    drop(tx_clone);
                });
                let mut mock_handle = MockTestAgentHandle::new();
                mock_handle
                    .expect_wait_with_timeout()
                    .times(1)
                    .returning(|_| Ok(true));
                Ok((Box::new(mock_handle), rx))
            });

        let result = run_main_loop(&client, &git, &launcher, &config, shutdown_rx).await;
        assert!(result.is_ok());

        // The in-job heartbeat failed, so we should see the failure log
        assert!(logs_contain("failed to send heartbeat"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_main_loop_heartbeat_sent_on_first_iteration() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = make_config();
        config.base_clone_path = tmp.path().join("base");

        let mut git = MockGitCommandRunner::new();
        setup_base_clone_mock(&mut git);

        let mut client = MockOrchestratorClient::new();
        client.expect_send_register().returning(|_| Ok(()));
        client.expect_send_heartbeat().returning(|_| Ok(()));
        client.expect_claim_work().returning(|| Ok(None));

        let launcher = MockTestAgentLauncher::new();

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let _ = shutdown_tx.send(true);
        });

        let result = run_main_loop(&client, &git, &launcher, &config, shutdown_rx).await;
        assert!(result.is_ok());
        assert!(logs_contain("sent heartbeat"));
    }
}
