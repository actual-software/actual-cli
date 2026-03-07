use clap::Parser;
use std::path::PathBuf;
use symphony::config::ServiceConfig;
use symphony::error::SymphonyError;
use symphony::orchestrator::Orchestrator;
use symphony::server;
use symphony::watcher::start_workflow_watch;
use symphony::workflow::load_workflow;
use tracing::{error, info};

#[derive(Parser, Debug)]
#[command(
    name = "symphony",
    about = "Long-running service that orchestrates coding agents to work on issues from Linear"
)]
struct Cli {
    /// Path to WORKFLOW.md (defaults to ./WORKFLOW.md)
    #[arg(default_value = "WORKFLOW.md")]
    workflow_path: PathBuf,

    /// Start HTTP dashboard on this port (overrides server.port in WORKFLOW.md)
    #[arg(long)]
    port: Option<u16>,
}

#[tokio::main]
async fn main() {
    // Load .env.local / .env files (best-effort — missing files are fine)
    let _ = dotenvy::from_filename(".env.local");
    let _ = dotenvy::dotenv();

    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .with_thread_ids(true)
        .json()
        .init();

    let cli = Cli::parse();

    match run(cli).await {
        Ok(()) => {
            info!("symphony shutdown cleanly");
        }
        Err(e) => {
            error!(error = %e, "symphony startup failed");
            std::process::exit(1);
        }
    }
}

async fn run(cli: Cli) -> Result<(), SymphonyError> {
    let workflow_path = &cli.workflow_path;

    // Verify workflow file exists
    if !workflow_path.exists() {
        return Err(SymphonyError::MissingWorkflowFile {
            path: workflow_path.display().to_string(),
        });
    }

    let workflow_path =
        std::fs::canonicalize(workflow_path).map_err(|e| SymphonyError::MissingWorkflowFile {
            path: format!("{}: {e}", cli.workflow_path.display()),
        })?;

    // Load and validate workflow
    let workflow = load_workflow(&workflow_path)?;
    let config = ServiceConfig::from_workflow(&workflow)?;
    config.validate_for_dispatch()?;

    // Resolve HTTP server port: CLI flag overrides config
    let server_port = cli.port.or(config.server.port);

    info!(
        workflow = %workflow_path.display(),
        tracker_kind = %config.tracker.kind,
        team_key = %config.tracker.team_key,
        poll_interval_ms = config.polling.interval_ms,
        max_concurrent = config.agent.max_concurrent_agents,
        workspace_root = %config.workspace.root.display(),
        server_port = ?server_port,
        "symphony starting"
    );

    // Start workflow file watcher
    let (_watcher, reload_rx) = start_workflow_watch(&workflow_path)?;

    // Create orchestrator
    let orchestrator = Orchestrator::new(config, workflow.prompt_template);

    // Setup shutdown signal
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // Start HTTP server if port is configured
    if let Some(port) = server_port {
        let state = orchestrator.state_handle();
        let config = orchestrator.config_handle();
        let msg_tx = orchestrator.message_sender();

        let job_notify = {
            let orch_state = state.read().await;
            std::sync::Arc::clone(&orch_state.job_notify)
        };

        let server_shutdown_rx = _shutdown_tx.subscribe();
        let server_shutdown = async move {
            let mut rx = server_shutdown_rx;
            let _ = rx.changed().await;
        };
        tokio::spawn(async move {
            if let Err(e) =
                server::start_server(port, state, config, msg_tx, job_notify, server_shutdown).await
            {
                error!(error = %e, "HTTP server failed");
            }
        });
    }

    // Handle SIGINT/SIGTERM (§12.1: Docker/K8s send SIGTERM)
    let shutdown_tx_signal = _shutdown_tx.clone();
    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        {
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("failed to register SIGTERM handler");
            tokio::select! {
                _ = ctrl_c => { info!("received SIGINT"); }
                _ = sigterm.recv() => { info!("received SIGTERM"); }
            }
        }
        #[cfg(not(unix))]
        {
            ctrl_c.await.expect("failed to listen for ctrl_c");
            info!("received SIGINT");
        }
        let _ = shutdown_tx_signal.send(true);
    });

    // Run orchestrator (blocks until shutdown)
    // The orchestrator now also receives reload events
    orchestrator.run(shutdown_rx, reload_rx).await;

    Ok(())
}
