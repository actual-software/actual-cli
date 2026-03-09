use std::collections::HashMap;
use std::path::PathBuf;

use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

use symphony::agent_process;
use symphony::protocol::AgentEvent;
use symphony_worker::client::HttpOrchestratorClient;
use symphony_worker::config::WorkerConfig;
use symphony_worker::executor::{AgentHandle, AgentLauncher};
use symphony_worker::worker::run_main_loop;
use symphony_worker::workspace::RealGitCommandRunner;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(
    name = "symphony-worker",
    about = "Symphony distributed worker process"
)]
struct Cli {
    #[arg(long, env = "ORCHESTRATOR_URL")]
    orchestrator_url: String,

    #[arg(long, env = "AUTH_TOKEN")]
    auth_token: String,

    #[arg(long, env = "WORKER_ID", default_value = "")]
    worker_id: String,

    #[arg(long, env = "REPO_URL")]
    repo_url: String,

    #[arg(long, env = "BASE_CLONE_PATH")]
    base_clone_path: PathBuf,

    #[arg(long, env = "WORKTREE_ROOT")]
    worktree_root: PathBuf,

    #[arg(long, env = "AGENT_COMMAND", default_value = "claude")]
    agent_command: String,

    #[arg(long, env = "MAX_TURNS", default_value_t = 30)]
    max_turns: u32,

    #[arg(long, env = "TURN_TIMEOUT_MS", default_value_t = 1_800_000)]
    turn_timeout_ms: u64,

    #[arg(long, env = "POLL_BACKOFF_BASE_MS", default_value_t = 1_000)]
    poll_backoff_base_ms: u64,

    #[arg(long, env = "POLL_BACKOFF_MAX_MS", default_value_t = 60_000)]
    poll_backoff_max_ms: u64,
}

// ---------------------------------------------------------------------------
// ClaudeAgentLauncher — spawns real Claude CLI subprocesses
// ---------------------------------------------------------------------------

struct ClaudeAgentLauncher {
    agent_command: String,
    max_turns: u32,
}

struct ClaudeAgentHandle {
    child: tokio::process::Child,
}

impl Drop for ClaudeAgentHandle {
    fn drop(&mut self) {
        // Best-effort SIGKILL to prevent orphaned processes.
        // `start_kill` is non-async and sends SIGKILL immediately.
        // Returns Err if the child has already exited — that is fine.
        let _ = self.child.start_kill();
    }
}

/// Log agent launch details.
fn log_agent_launch(issue_identifier: &str, command: &str) {
    info!(issue = %issue_identifier, command = %command, "launching claude agent subprocess");
}

#[async_trait::async_trait]
impl AgentHandle for ClaudeAgentHandle {
    async fn wait_with_timeout(&mut self, timeout_ms: u64) -> Result<bool, String> {
        let timeout = std::time::Duration::from_millis(timeout_ms);
        match tokio::time::timeout(timeout, self.child.wait()).await {
            Ok(Ok(status)) => Ok(status.success()),
            Ok(Err(e)) => Err(format!("process wait error: {e}")),
            Err(_) => Ok(false), // Timeout — still running
        }
    }

    async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }
}

#[async_trait::async_trait]
impl AgentLauncher for ClaudeAgentLauncher {
    async fn launch_agent(
        &self,
        workspace_path: &std::path::Path,
        prompt: &str,
        issue_identifier: &str,
    ) -> Result<
        (
            Box<dyn AgentHandle>,
            tokio::sync::mpsc::Receiver<AgentEvent>,
        ),
        String,
    > {
        let full_command =
            agent_process::build_agent_command(&self.agent_command, self.max_turns, None, None);

        log_agent_launch(issue_identifier, &full_command);

        let mut child =
            agent_process::spawn_agent_process(&full_command, workspace_path, &HashMap::new())
                .map_err(|e| format!("failed to spawn agent: {e}"))?;

        // Write prompt to stdin
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "stdin was not piped".to_string())?;
        let prompt_bytes = prompt.as_bytes().to_vec();
        tokio::spawn(agent_process::write_prompt_to_stdin(stdin, prompt_bytes));

        let pid = child.id();

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "stdout was not piped".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "stderr was not piped".to_string())?;

        let (event_tx, event_rx) = tokio::sync::mpsc::channel(256);

        // Spawn stdout reader
        let tx_clone = event_tx.clone();
        tokio::spawn(agent_process::read_stdout_lines(stdout, tx_clone));

        // Spawn stderr reader (diagnostic only)
        tokio::spawn(agent_process::read_stderr_lines(stderr));

        // Emit initial SessionStarted event
        let _ = event_tx
            .send(AgentEvent::SessionStarted {
                session_id: format!("claude-{}", pid.unwrap_or(0)),
                pid,
            })
            .await;

        Ok((Box::new(ClaudeAgentHandle { child }), event_rx))
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    // Generate worker ID if not provided
    let worker_id = if cli.worker_id.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        cli.worker_id
    };

    let config = WorkerConfig {
        orchestrator_url: cli.orchestrator_url.clone(),
        auth_token: cli.auth_token.clone(),
        worker_id: worker_id.clone(),
        repo_url: cli.repo_url,
        base_clone_path: cli.base_clone_path,
        worktree_root: cli.worktree_root,
        agent_command: cli.agent_command.clone(),
        max_turns: cli.max_turns,
        turn_timeout_ms: cli.turn_timeout_ms,
        poll_backoff_base_ms: cli.poll_backoff_base_ms,
        poll_backoff_max_ms: cli.poll_backoff_max_ms,
    };

    config
        .validate()
        .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;

    let client = HttpOrchestratorClient::new(cli.orchestrator_url, cli.auth_token, worker_id);
    let git = RealGitCommandRunner;
    let launcher = ClaudeAgentLauncher {
        agent_command: cli.agent_command,
        max_turns: cli.max_turns,
    };

    // Set up shutdown signal
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // Handle SIGTERM/SIGINT
    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to listen for ctrl-c");
        let _ = shutdown_tx.send(true);
    });

    run_main_loop(&client, &git, &launcher, &config, shutdown_rx).await
}
