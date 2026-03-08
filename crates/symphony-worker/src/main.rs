use std::path::PathBuf;

use clap::Parser;
use tracing_subscriber::EnvFilter;

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
// Real AgentLauncher (placeholder — delegates to claude CLI)
// ---------------------------------------------------------------------------

struct RealAgentLauncher {
    _agent_command: String,
}

struct RealAgentHandle;

#[async_trait::async_trait]
impl AgentHandle for RealAgentHandle {
    async fn wait_with_timeout(&mut self, _timeout_ms: u64) -> Result<bool, String> {
        // TODO: implement real agent waiting
        Ok(true)
    }

    async fn kill(&mut self) {
        // TODO: implement real agent kill
    }
}

#[async_trait::async_trait]
impl AgentLauncher for RealAgentLauncher {
    async fn launch_agent(
        &self,
        _workspace_path: &std::path::Path,
        _prompt: &str,
        _issue_identifier: &str,
    ) -> Result<
        (
            Box<dyn AgentHandle>,
            tokio::sync::mpsc::Receiver<symphony::protocol::AgentEvent>,
        ),
        String,
    > {
        // TODO: implement real agent launching
        let (_tx, rx) = tokio::sync::mpsc::channel(1);
        Ok((Box::new(RealAgentHandle), rx))
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
    let launcher = RealAgentLauncher {
        _agent_command: cli.agent_command,
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
