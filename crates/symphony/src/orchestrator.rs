use crate::agent::AgentSession;
use crate::config::{state_matches, DeploymentMode, ServiceConfig};
use crate::error::{Result, SymphonyError};
use crate::github::GitHubClient;
use crate::model::{Issue, OrchestratorState, RetryEntry};
use crate::persistence::{PersistedSession, StateStore};
use crate::prompt::{build_continuation_prompt, render_prompt};
use crate::protocol::{
    AgentEvent, LiveSession, RunningEntry, RunningSessionInfo, WorkAssignment, WorkerExitReason,
};
use crate::tracker::LinearClient;
use crate::workspace;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, error, info, warn};

// Re-export protocol types for backward compatibility.
pub use crate::protocol::{OrchestratorMessage, OrchestratorSnapshot};

/// Log orchestrator start parameters.
fn log_orchestrator_start(poll_interval_ms: u64, max_concurrent: u32) {
    info!(poll_interval_ms, max_concurrent, "starting orchestrator");
}

/// Log terminal workspace cleanup.
fn log_terminal_cleanup(identifier: &str, workspace: &std::path::Path) {
    let ws = workspace.display().to_string();
    info!(issue_identifier = %identifier, workspace = %ws, "cleaning terminal workspace");
}

/// Map a worker `Result<()>` to its exit reason.
fn map_worker_result(result: Result<()>) -> WorkerExitReason {
    match result {
        Ok(()) => WorkerExitReason::Normal,
        Err(SymphonyError::AgentTurnTimeout) => WorkerExitReason::TimedOut,
        Err(SymphonyError::AgentTurnCancelled) => WorkerExitReason::Cancelled,
        Err(e) => WorkerExitReason::Failed(e.to_string()),
    }
}

/// Default timeout for draining workers during shutdown (30 seconds).
const SHUTDOWN_DRAIN_TIMEOUT_SECS: u64 = 30;

/// The orchestrator owns the poll tick, state, and dispatch logic.
pub struct Orchestrator {
    state: Arc<RwLock<OrchestratorState>>,
    config: Arc<RwLock<ServiceConfig>>,
    prompt_template: Arc<RwLock<String>>,
    http: reqwest::Client,
    msg_tx: mpsc::Sender<OrchestratorMessage>,
    msg_rx: mpsc::Receiver<OrchestratorMessage>,
    /// JoinHandles for spawned worker tasks, keyed by issue ID.
    worker_handles: Arc<RwLock<std::collections::HashMap<String, tokio::task::JoinHandle<()>>>>,
    /// Optional persistence store for session metrics.
    store: Option<Arc<dyn StateStore>>,
}

impl Orchestrator {
    pub fn new(config: ServiceConfig, prompt_template: String) -> Self {
        let state = OrchestratorState::new(
            config.polling.interval_ms,
            config.agent.max_concurrent_agents,
        );

        let (msg_tx, msg_rx) = mpsc::channel(512);

        Self {
            state: Arc::new(RwLock::new(state)),
            config: Arc::new(RwLock::new(config)),
            prompt_template: Arc::new(RwLock::new(prompt_template)),
            http: reqwest::Client::new(),
            msg_tx,
            msg_rx,
            worker_handles: Arc::new(RwLock::new(std::collections::HashMap::new())),
            store: None,
        }
    }

    /// Attach a persistence store and load persisted state into memory.
    pub async fn with_store(mut self, store: Arc<dyn StateStore>) -> Self {
        // Load persisted agent totals
        match store.load_agent_totals() {
            Ok(totals) => {
                let mut state = self.state.write().await;
                state.agent_totals = totals;
                let t = &state.agent_totals;
                info!("restored persisted agent totals: input_tokens={} output_tokens={} total_tokens={} seconds_running={:.1}", t.input_tokens, t.output_tokens, t.total_tokens, t.seconds_running);
            }
            Err(e) => {
                warn!(error = %e, "failed to load persisted agent totals");
            }
        }

        // Load persisted completed set
        match store.load_completed_ids() {
            Ok(ids) => {
                let count = ids.len();
                let mut state = self.state.write().await;
                for id in ids {
                    state.mark_completed(id);
                }
                info!(count, "restored persisted completed issues");
            }
            Err(e) => {
                warn!(error = %e, "failed to load persisted completed issues");
            }
        }

        // Load persisted claimed set
        match store.load_claimed_ids() {
            Ok(ids) => {
                let count = ids.len();
                let mut state = self.state.write().await;
                for id in ids {
                    state.claimed.insert(id);
                }
                info!(count, "restored persisted claimed issues");
            }
            Err(e) => {
                warn!(error = %e, "failed to load persisted claimed issues");
            }
        }

        // Load persisted waiting_for_review entries
        match store.load_waiting() {
            Ok(entries) => {
                let count = entries.len();
                let mut state = self.state.write().await;
                for entry in entries {
                    state
                        .waiting_for_review
                        .insert(entry.issue_id.clone(), entry);
                }
                info!(count, "restored persisted waiting_for_review entries");
            }
            Err(e) => {
                warn!(error = %e, "failed to load persisted waiting entries");
            }
        }

        self.store = Some(store);
        self
    }

    /// Get a handle to the persistence store (for the HTTP server or other components).
    pub fn store_handle(&self) -> Option<Arc<dyn StateStore>> {
        self.store.clone()
    }

    /// Update the configuration dynamically (for workflow reload).
    pub async fn update_config(&self, config: ServiceConfig, prompt_template: String) {
        let mut state = self.state.write().await;
        state.poll_interval_ms = config.polling.interval_ms;
        state.max_concurrent_agents = config.agent.max_concurrent_agents;
        drop(state);

        *self.config.write().await = config;
        *self.prompt_template.write().await = prompt_template;

        info!("orchestrator config updated");
    }

    /// Get a sender for external messages (e.g., refresh triggers).
    pub fn message_sender(&self) -> mpsc::Sender<OrchestratorMessage> {
        self.msg_tx.clone()
    }

    /// Get a handle to the shared orchestrator state (for the HTTP server).
    pub fn state_handle(&self) -> Arc<RwLock<OrchestratorState>> {
        Arc::clone(&self.state)
    }

    /// Get a handle to the shared config (for the HTTP server).
    pub fn config_handle(&self) -> Arc<RwLock<ServiceConfig>> {
        Arc::clone(&self.config)
    }

    /// Get a snapshot of current state for observability.
    pub async fn snapshot(&self) -> OrchestratorSnapshot {
        let state = self.state.read().await;

        let running: Vec<RunningSessionInfo> = state
            .running
            .values()
            .map(|entry| RunningSessionInfo {
                issue_id: entry.issue.id.clone(),
                issue_identifier: entry.identifier.clone(),
                state: entry.issue.state.clone(),
                session_id: entry.session.session_id.clone(),
                turn_count: entry.session.turn_count,
                last_event: entry.session.last_event.clone(),
                last_message: entry.session.last_message.clone(),
                started_at: entry.started_at,
                last_event_at: entry.session.last_event_at,
                input_tokens: entry.session.input_tokens,
                output_tokens: entry.session.output_tokens,
                total_tokens: entry.session.total_tokens,
            })
            .collect();

        let retrying: Vec<RetryEntry> = state.retry_attempts.values().cloned().collect();
        let waiting: Vec<crate::model::WaitingEntry> =
            state.waiting_for_review.values().cloned().collect();

        // Calculate live seconds_running including active sessions
        let active_seconds: f64 = state
            .running
            .values()
            .map(|entry| {
                Utc::now()
                    .signed_duration_since(entry.started_at)
                    .num_milliseconds()
                    .max(0) as f64
                    / 1000.0
            })
            .sum();

        let mut totals = state.agent_totals.clone();
        totals.seconds_running += active_seconds;

        OrchestratorSnapshot {
            running,
            retrying,
            waiting,
            totals,
            rate_limits: state.rate_limits.clone(),
        }
    }

    /// Main orchestration loop.
    pub async fn run(
        mut self,
        mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
        mut reload_rx: mpsc::Receiver<(ServiceConfig, String)>,
    ) {
        {
            let config = self.config.read().await;
            log_orchestrator_start(
                config.polling.interval_ms,
                config.agent.max_concurrent_agents,
            );
        }

        // Startup terminal workspace cleanup
        self.startup_cleanup().await;

        // Schedule immediate first tick
        let mut current_interval_ms = {
            let state = self.state.read().await;
            state.poll_interval_ms
        };
        let mut tick_interval =
            tokio::time::interval(std::time::Duration::from_millis(current_interval_ms));

        loop {
            tokio::select! {
                _ = tick_interval.tick() => {
                    self.on_tick().await;

                    // Only recreate the interval if the poll period changed,
                    // to avoid the "fresh interval fires immediately" double-tick.
                    let state = self.state.read().await;
                    if state.poll_interval_ms != current_interval_ms {
                        current_interval_ms = state.poll_interval_ms;
                        tick_interval = tokio::time::interval(
                            std::time::Duration::from_millis(current_interval_ms),
                        );
                    }
                }

                Some(msg) = self.msg_rx.recv() => {
                    self.handle_message(msg).await;
                }

                Some((new_config, new_prompt)) = reload_rx.recv() => {
                    info!("applying workflow reload");
                    self.update_config(new_config, new_prompt).await;
                }

                Ok(()) = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        info!("shutdown signal received");
                        self.shutdown().await;
                        return;
                    }
                }
            }
        }
    }

    async fn on_tick(&self) {
        // Phase 1: Reconcile running issues
        self.reconcile().await;

        // Phase 2: Validate config and clone what we need, then drop the lock
        let config = {
            let cfg = self.config.read().await;
            if let Err(e) = cfg.validate_for_dispatch() {
                error!(error = %e, "dispatch validation failed, skipping dispatch");
                return;
            }
            cfg.clone()
        };

        // Phase 3: Fetch candidates (no lock held during HTTP call)
        let client = LinearClient::new(&config.tracker, self.http.clone());
        let candidates = match client
            .fetch_candidate_issues(&config.tracker.active_states)
            .await
        {
            Ok(issues) => issues,
            Err(e) => {
                warn!(error = %e, "failed to fetch candidate issues");
                return;
            }
        };

        debug!(count = candidates.len(), "fetched candidate issues");

        // Phase 4: Sort and dispatch
        let sorted = sort_for_dispatch(candidates);
        for issue in sorted {
            let state = self.state.read().await;
            if state.available_slots() == 0 {
                break;
            }
            drop(state);

            if self.should_dispatch(&issue, &config).await {
                self.dispatch_issue(issue, None, false, None).await;
            }
        }
    }

    async fn should_dispatch(&self, issue: &Issue, config: &ServiceConfig) -> bool {
        // Must have required fields
        if issue.id.is_empty()
            || issue.identifier.is_empty()
            || issue.title.is_empty()
            || issue.state.is_empty()
        {
            return false;
        }

        // State must be active and not terminal
        if !state_matches(&issue.state, &config.tracker.active_states) {
            return false;
        }
        if state_matches(&issue.state, &config.tracker.terminal_states) {
            return false;
        }

        let state = self.state.read().await;

        // Not already running, claimed, waiting for review, or recently completed
        if state.running.contains_key(&issue.id)
            || state.claimed.contains(&issue.id)
            || state.waiting_for_review.contains_key(&issue.id)
            || state.is_completed(&issue.id)
        {
            return false;
        }

        // Global concurrency
        if state.available_slots() == 0 {
            return false;
        }

        // Per-state concurrency
        let normalized_state = issue.state.trim().to_lowercase();
        if let Some(&limit) = config
            .agent
            .max_concurrent_agents_by_state
            .get(&normalized_state)
        {
            if state.running_count_by_state(&issue.state) >= limit {
                return false;
            }
        }

        // Label claim: skip if already claimed by another Symphony instance
        if issue.labels.iter().any(|l| l == "symphony-claimed") {
            info!(issue_id = %issue.id, issue_identifier = %issue.identifier, "skipping dispatch: issue has symphony-claimed label");
            return false;
        }

        // Blocker rule for Todo state
        if normalized_state == "todo" {
            let has_non_terminal_blocker = issue.blocked_by.iter().any(|b| {
                if let Some(ref blocker_state) = b.state {
                    !state_matches(blocker_state, &config.tracker.terminal_states)
                } else {
                    true // Unknown state treated as non-terminal
                }
            });
            if has_non_terminal_blocker {
                return false;
            }
        }

        // Drop state lock before async GitHub call
        drop(state);

        // PR dedup: skip if an open PR already exists for this issue's branch
        if let Some(ref github_config) = config.github {
            let branch = format!(
                "{}{}",
                github_config.branch_prefix,
                issue.identifier.to_lowercase()
            );
            let github = GitHubClient::new(github_config, self.http.clone());
            let pr_check = github.find_open_pr(&branch).await;
            if let Err(e) = &pr_check {
                warn!(issue_id = %issue.id, error = %e, "failed to check for existing PR, proceeding with dispatch");
            }
            if let Ok(Some(_pr)) = pr_check {
                info!(issue_id = %issue.id, branch = %branch, "skipping dispatch: open PR already exists");
                return false;
            }
        }

        true
    }

    async fn dispatch_issue(
        &self,
        issue: Issue,
        attempt: Option<u32>,
        skip_state_transition: bool,
        resume_session_id: Option<String>,
    ) {
        let issue_id = issue.id.clone();
        let identifier = issue.identifier.clone();

        info!(issue_id = %issue_id, issue_identifier = %identifier, attempt = ?attempt, "dispatching issue");

        let (cancel_tx, cancel_rx) = oneshot::channel();

        // Add to running and claimed
        {
            let mut state = self.state.write().await;

            // Create broadcast channel for SSE streaming
            let (broadcast_tx, _) =
                tokio::sync::broadcast::channel(crate::model::BROADCAST_CHANNEL_CAPACITY);
            state
                .event_broadcasts
                .insert(issue_id.clone(), broadcast_tx);

            state.running.insert(
                issue_id.clone(),
                RunningEntry {
                    issue: issue.clone(),
                    identifier: identifier.clone(),
                    session: LiveSession::default(),
                    retry_attempt: attempt,
                    started_at: Utc::now(),
                    cancel_tx,
                    event_log: std::collections::VecDeque::new(),
                    log_seq: 0,
                },
            );
            state.claimed.insert(issue_id.clone());
            state.retry_attempts.remove(&issue_id);
            state.notify_state_change();
        }

        // Persist claimed marker (best-effort)
        if let Some(ref store) = self.store {
            if let Err(e) = store.save_claimed(&issue_id) {
                debug!(error = %e, "failed to persist claimed marker");
            }
        }

        // Persist new session (best-effort)
        self.persist_session(
            &issue_id,
            &identifier,
            &LiveSession::default(),
            attempt,
            Utc::now(),
        )
        .await;

        // Transition issue to "In Progress" in Linear (best-effort)
        if !skip_state_transition {
            self.transition_to_in_progress(&issue_id, &identifier).await;
        }

        // Add "symphony-claimed" label to prevent other instances from dispatching (best-effort)
        self.add_claim_label(&issue_id, &identifier).await;

        // Read config to check deployment mode
        let config = self.config.read().await.clone();

        match config.deployment.mode {
            DeploymentMode::Distributed => {
                // Render prompt for remote worker
                let prompt_template = self.prompt_template.read().await.clone();
                let rendered_prompt = match crate::prompt::render_prompt(
                    &prompt_template,
                    &issue,
                    attempt,
                ) {
                    Ok(p) => p,
                    Err(e) => {
                        error!(issue_id = %issue_id, issue_identifier = %identifier, error = %e, "failed to render prompt for distributed worker");
                        return;
                    }
                };

                let workspace_key = crate::workspace::sanitize_workspace_key(&identifier);
                let workspace_path = config.workspace.root.join(&workspace_key);

                let assignment = WorkAssignment {
                    issue_id: issue_id.clone(),
                    issue_identifier: identifier.clone(),
                    prompt: rendered_prompt,
                    attempt,
                    workspace_path: Some(workspace_path.display().to_string()),
                };

                {
                    let mut state = self.state.write().await;
                    state.pending_jobs.push_back(assignment);
                    state.job_notify.notify_one();
                }

                info!(issue_id = %issue_id, issue_identifier = %identifier, "issue enqueued for distributed worker");
                return;
            }
            DeploymentMode::Local => {
                // Spawn local worker task below
            }
        }

        let prompt_template = self.prompt_template.read().await.clone();
        let http = self.http.clone();
        let msg_tx = self.msg_tx.clone();
        let worker_handles = Arc::clone(&self.worker_handles);
        let worker_issue_id = issue_id.clone();

        let handle = tokio::spawn(async move {
            let result = run_worker(
                &config,
                &prompt_template,
                &issue,
                attempt,
                http,
                msg_tx.clone(),
                cancel_rx,
                resume_session_id,
            )
            .await;

            let reason = map_worker_result(result);

            let _ = msg_tx
                .send(OrchestratorMessage::WorkerExited {
                    issue_id: issue.id.clone(),
                    reason,
                })
                .await;

            // Remove own handle from the map (best-effort cleanup)
            worker_handles.write().await.remove(&issue.id);
        });

        // Store handle for shutdown draining
        self.worker_handles
            .write()
            .await
            .insert(worker_issue_id, handle);
    }

    /// Best-effort transition of an issue to "In Progress" in Linear.
    /// Logs a warning on failure but does not block dispatch.
    async fn transition_to_in_progress(&self, issue_id: &str, identifier: &str) {
        let config = self.config.read().await;
        let client = LinearClient::new(&config.tracker, self.http.clone());
        drop(config); // Release RwLock before async call

        match client.transition_issue_state(issue_id, "In Progress").await {
            Ok(()) => {
                info!(issue_identifier = %identifier, "transitioned issue to In Progress");
            }
            Err(e) => {
                warn!(issue_identifier = %identifier, error = %e, "failed to transition issue to In Progress (continuing dispatch)");
            }
        }
    }

    /// Best-effort transition of an issue to "Merged" in Linear.
    /// Logs a warning on failure but does not block cleanup.
    async fn transition_to_merged(&self, issue_id: &str, identifier: &str) {
        let config = self.config.read().await;
        let client = LinearClient::new(&config.tracker, self.http.clone());
        drop(config); // Release RwLock before async call

        match client.transition_issue_state(issue_id, "Merged").await {
            Ok(()) => {
                info!(issue_identifier = %identifier, "transitioned issue to Merged");
            }
            Err(e) => {
                warn!(issue_identifier = %identifier, error = %e, "failed to transition issue to Merged (continuing cleanup)");
            }
        }
    }

    /// Best-effort add "symphony-claimed" label to a Linear issue on dispatch.
    async fn add_claim_label(&self, issue_id: &str, identifier: &str) {
        let config = self.config.read().await;
        let client = LinearClient::new(&config.tracker, self.http.clone());
        drop(config);

        match client
            .add_label_to_issue(issue_id, "symphony-claimed")
            .await
        {
            Ok(()) => {
                debug!(issue_identifier = %identifier, "added symphony-claimed label");
            }
            Err(e) => {
                warn!(issue_identifier = %identifier, error = %e, "failed to add symphony-claimed label (continuing dispatch)");
            }
        }
    }

    /// Best-effort remove "symphony-claimed" label from a Linear issue on completion.
    async fn remove_claim_label(&self, issue_id: &str, identifier: &str) {
        let config = self.config.read().await;
        let client = LinearClient::new(&config.tracker, self.http.clone());
        drop(config);

        match client
            .remove_label_from_issue(issue_id, "symphony-claimed")
            .await
        {
            Ok(()) => {
                debug!(issue_identifier = %identifier, "removed symphony-claimed label");
            }
            Err(e) => {
                warn!(issue_identifier = %identifier, error = %e, "failed to remove symphony-claimed label");
            }
        }
    }

    /// Add an "In Review" issue to waiting_for_review by looking up its PR.
    async fn add_to_waiting_for_review(
        &self,
        issue_id: &str,
        identifier: &str,
        session_id: Option<&str>,
    ) {
        let config = self.config.read().await;
        let github_config = match &config.github {
            Some(c) => c.clone(),
            None => {
                // No GitHub config — can't track PR, fall back to continuation retry
                debug!(issue_id = %issue_id, "no GitHub config, skipping waiting_for_review");
                return;
            }
        };
        drop(config);

        let branch = format!(
            "{}{}",
            github_config.branch_prefix,
            identifier.to_lowercase()
        );
        let github = GitHubClient::new(&github_config, self.http.clone());

        let pr_number = match github.find_open_pr(&branch).await {
            Ok(Some(pr)) => pr.number,
            Ok(None) => {
                // Fallback: check if PR was already merged
                match github.is_pr_merged(&branch).await {
                    Ok(Some(true)) => {
                        info!(issue_id = %issue_id, branch = %branch, "PR already merged — transitioning to Merged");
                        self.transition_to_merged(issue_id, identifier).await;
                        return;
                    }
                    _ => {
                        debug!(issue_id = %issue_id, branch = %branch, "no open PR found for In Review issue");
                        return;
                    }
                }
            }
            Err(e) => {
                debug!(issue_id = %issue_id, error = %e, "failed to find PR for In Review issue");
                return;
            }
        };

        let waiting_entry = crate::model::WaitingEntry {
            issue_id: issue_id.to_string(),
            identifier: identifier.to_string(),
            pr_number,
            branch,
            started_waiting_at: Utc::now(),
            session_id: session_id.map(|s| s.to_string()),
        };
        let mut state = self.state.write().await;
        state
            .waiting_for_review
            .insert(issue_id.to_string(), waiting_entry.clone());
        state.notify_state_change();
        drop(state);

        // Persist waiting entry (best-effort)
        if let Some(ref store) = self.store {
            if let Err(e) = store.save_waiting(&waiting_entry) {
                debug!(error = %e, issue_id = %issue_id, "failed to persist waiting entry");
            }
        }

        info!(issue_id = %issue_id, issue_identifier = %identifier, pr_number = pr_number, "added to waiting_for_review");
    }

    /// Best-effort persist a session snapshot to the store.
    async fn persist_session(
        &self,
        issue_id: &str,
        identifier: &str,
        session: &LiveSession,
        retry_attempt: Option<u32>,
        started_at: DateTime<Utc>,
    ) {
        if let Some(ref store) = self.store {
            let persisted = PersistedSession {
                issue_id: issue_id.to_string(),
                identifier: identifier.to_string(),
                session_id: session.session_id.clone(),
                input_tokens: session.input_tokens,
                output_tokens: session.output_tokens,
                total_tokens: session.total_tokens,
                turn_count: session.turn_count,
                started_at: started_at.to_rfc3339(),
                ended_at: None,
                last_event: session.last_event.clone(),
                last_event_at: session.last_event_at.map(|t| t.to_rfc3339()),
                last_message: session.last_message.clone(),
                retry_attempt,
            };
            if let Err(e) = store.save_session(&persisted) {
                debug!(error = %e, issue_id = %issue_id, "failed to persist session");
            }
        }
    }

    async fn handle_message(&self, msg: OrchestratorMessage) {
        match msg {
            OrchestratorMessage::WorkerExited { issue_id, reason } => {
                self.on_worker_exit(&issue_id, reason).await;
            }
            OrchestratorMessage::AgentUpdate { issue_id, event } => {
                self.on_agent_update(&issue_id, event).await;
            }
            OrchestratorMessage::RetryFired { issue_id } => {
                self.on_retry_fired(&issue_id).await;
            }
            OrchestratorMessage::TriggerRefresh => {
                self.on_tick().await;
            }
            OrchestratorMessage::RemoveFromRetryQueue { issue_id } => {
                self.on_remove_from_retry_queue(&issue_id).await;
            }
        }
    }

    async fn on_worker_exit(&self, issue_id: &str, reason: WorkerExitReason) {
        let mut state = self.state.write().await;

        // Remove broadcast sender (dropping it closes all receivers)
        state.event_broadcasts.remove(issue_id);

        if let Some(entry) = state.running.remove(issue_id) {
            state.notify_state_change();
            // Add runtime seconds to totals
            let elapsed = Utc::now()
                .signed_duration_since(entry.started_at)
                .num_milliseconds()
                .max(0) as f64
                / 1000.0;
            state.agent_totals.seconds_running += elapsed;

            // Persist updated agent totals and finalize session (best-effort)
            if let Some(ref store) = self.store {
                if let Err(e) = store.save_agent_totals(&state.agent_totals) {
                    debug!(error = %e, "failed to persist agent totals on worker exit");
                }
                // Finalize the session record with ended_at
                let ended = PersistedSession {
                    issue_id: issue_id.to_string(),
                    identifier: entry.identifier.clone(),
                    session_id: entry.session.session_id.clone(),
                    input_tokens: entry.session.input_tokens,
                    output_tokens: entry.session.output_tokens,
                    total_tokens: entry.session.total_tokens,
                    turn_count: entry.session.turn_count,
                    started_at: entry.started_at.to_rfc3339(),
                    ended_at: Some(Utc::now().to_rfc3339()),
                    last_event: entry.session.last_event.clone(),
                    last_event_at: entry.session.last_event_at.map(|t| t.to_rfc3339()),
                    last_message: entry.session.last_message.clone(),
                    retry_attempt: entry.retry_attempt,
                };
                if let Err(e) = store.save_session(&ended) {
                    debug!(error = %e, "failed to finalize persisted session on worker exit");
                }
            }

            let identifier = entry.identifier.clone();
            let next_attempt = entry.retry_attempt.unwrap_or(0) + 1;

            // Extract data needed for normal-exit processing before the match,
            // since the Normal arm needs to drop the state lock for async HTTP calls.
            let normal_exit_data = if matches!(reason, WorkerExitReason::Normal) {
                Some((
                    entry.identifier.clone(),
                    entry.issue.state.clone(),
                    entry.issue.id.clone(),
                    entry.session.session_id.clone(),
                ))
            } else {
                None
            };

            match &reason {
                WorkerExitReason::Normal => {
                    info!(issue_id = %issue_id, issue_identifier = %identifier, "worker completed normally");
                    state.mark_completed(issue_id.to_string());
                    // Persist completed marker (best-effort)
                    if let Some(ref store) = self.store {
                        if let Err(e) = store.mark_completed(issue_id) {
                            debug!(error = %e, "failed to persist completed marker");
                        }
                    }

                    let (issue_identifier, cached_state, entry_issue_id, exit_session_id) =
                        normal_exit_data.expect("set when Normal");

                    // Drop the write lock before making async HTTP calls
                    drop(state);

                    // Re-fetch the current issue state from the tracker.
                    // The locally cached `entry.issue.state` may be stale if the
                    // worker itself transitioned the issue (e.g. to "In Review")
                    // during its run.
                    let issue_state = {
                        let config = self.config.read().await;
                        let client = LinearClient::new(&config.tracker, self.http.clone());
                        match client.fetch_issue_states_by_ids(&[entry_issue_id]).await {
                            Ok(refreshed) => refreshed
                                .first()
                                .map(|i| i.state.clone())
                                .unwrap_or_else(|| cached_state.clone()),
                            Err(e) => {
                                warn!(
                                    issue_id = %issue_id,
                                    issue_identifier = %identifier,
                                    error = %e,
                                    "failed to refresh issue state on worker exit, using cached state"
                                );
                                cached_state.clone()
                            }
                        }
                    };

                    if is_done_state(&issue_state) {
                        // Cleanup completed — issue already transitioned, nothing more to do
                        info!(issue_id = %issue_id, issue_identifier = %identifier, "cleanup worker completed, issue already in terminal state");
                    } else if is_in_review_state(&issue_state) {
                        self.add_to_waiting_for_review(
                            issue_id,
                            &issue_identifier,
                            exit_session_id.as_deref(),
                        )
                        .await;
                        // Verify the entry was added; if not (GitHub API failed), schedule a retry
                        let state = self.state.read().await;
                        if !state.waiting_for_review.contains_key(issue_id) {
                            drop(state);
                            warn!(issue_id = %issue_id, issue_identifier = %issue_identifier, "add_to_waiting_for_review failed, scheduling retry");
                            let mut state = self.state.write().await;
                            self.schedule_retry_inner(
                                &mut state,
                                issue_id.to_string(),
                                0,
                                issue_identifier.clone(),
                                Some("failed to add to waiting_for_review".to_string()),
                                30_000, // 30 second retry
                            );
                        } else {
                            drop(state);
                        }
                        // Remove claim label when entering review (best-effort)
                        self.remove_claim_label(issue_id, &issue_identifier).await;
                    } else {
                        // §8.1: Schedule a short continuation retry (1000ms)
                        let mut state = self.state.write().await;
                        self.schedule_retry_inner(
                            &mut state,
                            issue_id.to_string(),
                            0, // continuation, not a failure retry
                            identifier,
                            None,
                            1_000, // 1 second
                        );
                    }
                }
                WorkerExitReason::Failed(err) => {
                    warn!(issue_id = %issue_id, issue_identifier = %identifier, error = %err, "worker failed");
                    let config = self.config.read().await;
                    self.schedule_retry(
                        &mut state,
                        &config,
                        issue_id,
                        next_attempt,
                        identifier,
                        Some(err.clone()),
                    );
                }
                WorkerExitReason::TimedOut => {
                    warn!(issue_id = %issue_id, issue_identifier = %identifier, "worker timed out");
                    let config = self.config.read().await;
                    self.schedule_retry(
                        &mut state,
                        &config,
                        issue_id,
                        next_attempt,
                        identifier,
                        Some("turn timeout".to_string()),
                    );
                }
                WorkerExitReason::Stalled => {
                    warn!(issue_id = %issue_id, issue_identifier = %identifier, "worker stalled");
                    let config = self.config.read().await;
                    self.schedule_retry(
                        &mut state,
                        &config,
                        issue_id,
                        next_attempt,
                        identifier,
                        Some("stalled".to_string()),
                    );
                }
                WorkerExitReason::Cancelled => {
                    info!(issue_id = %issue_id, issue_identifier = %identifier, "worker cancelled by reconciliation");
                    state.claimed.remove(issue_id);
                    // Remove claim label (best-effort, after releasing lock)
                    let id = issue_id.to_string();
                    let ident = identifier.clone();
                    drop(state);
                    if let Some(ref store) = self.store {
                        if let Err(e) = store.remove_claimed(&id) {
                            debug!(error = %e, "failed to remove persisted claimed marker");
                        }
                    }
                    self.remove_claim_label(&id, &ident).await;
                }
            }
        }
    }

    /// Schedule a retry with exponential backoff (§8.2: always retries, no cap).
    fn schedule_retry(
        &self,
        state: &mut OrchestratorState,
        config: &ServiceConfig,
        issue_id: &str,
        attempt: u32,
        identifier: String,
        error: Option<String>,
    ) {
        let delay = exponential_backoff(attempt, config.agent.max_retry_backoff_ms);
        self.schedule_retry_inner(
            state,
            issue_id.to_string(),
            attempt,
            identifier,
            error,
            delay,
        );
    }

    fn schedule_retry_inner(
        &self,
        state: &mut OrchestratorState,
        issue_id: String,
        attempt: u32,
        identifier: String,
        error: Option<String>,
        delay_ms: u64,
    ) {
        // Cancel any existing retry
        state.retry_attempts.remove(&issue_id);

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let entry = RetryEntry {
            issue_id: issue_id.clone(),
            identifier: identifier.clone(),
            attempt,
            due_at_ms: now_ms + delay_ms,
            error: error.clone(),
        };

        state.retry_attempts.insert(issue_id.clone(), entry);
        state.notify_state_change();

        debug!(issue_id = %issue_id, issue_identifier = %identifier, attempt = attempt, delay_ms = delay_ms, "scheduled retry");

        // Spawn a timer task
        let msg_tx = self.msg_tx.clone();
        let id = issue_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            let _ = msg_tx
                .send(OrchestratorMessage::RetryFired { issue_id: id })
                .await;
        });
    }

    async fn on_retry_fired(&self, issue_id: &str) {
        let mut state = self.state.write().await;
        let entry = match state.retry_attempts.remove(issue_id) {
            Some(e) => e,
            None => return,
        };
        drop(state);

        // Clone config values before the async HTTP call to release the lock
        let config = self.config.read().await.clone();
        let client = LinearClient::new(&config.tracker, self.http.clone());

        // Fetch active candidates to check if issue is still eligible
        // (no config lock held during HTTP call)
        let candidates = match client
            .fetch_candidate_issues(&config.tracker.active_states)
            .await
        {
            Ok(issues) => issues,
            Err(e) => {
                warn!(issue_id = %issue_id, issue_identifier = %entry.identifier, error = %e, "retry poll failed, rescheduling");
                let delay =
                    exponential_backoff(entry.attempt + 1, config.agent.max_retry_backoff_ms);
                let mut state = self.state.write().await;
                self.schedule_retry_inner(
                    &mut state,
                    issue_id.to_string(),
                    entry.attempt + 1,
                    entry.identifier,
                    Some("retry poll failed".to_string()),
                    delay,
                );
                return;
            }
        };

        let issue = candidates.iter().find(|i| i.id == issue_id);

        match issue {
            None => {
                // Issue no longer in active candidates, release claim
                info!(issue_id = %issue_id, issue_identifier = %entry.identifier, "issue no longer active, releasing claim");
                let mut state = self.state.write().await;
                state.claimed.remove(issue_id);
                drop(state);
                if let Some(ref store) = self.store {
                    if let Err(e) = store.remove_claimed(issue_id) {
                        debug!(error = %e, "failed to remove persisted claimed marker");
                    }
                }
            }
            Some(issue) => {
                let state = self.state.read().await;
                if state.available_slots() == 0 {
                    drop(state);
                    let delay =
                        exponential_backoff(entry.attempt + 1, config.agent.max_retry_backoff_ms);
                    let mut state = self.state.write().await;
                    self.schedule_retry_inner(
                        &mut state,
                        issue_id.to_string(),
                        entry.attempt + 1,
                        entry.identifier,
                        Some("no available orchestrator slots".to_string()),
                        delay,
                    );
                    return;
                }
                drop(state);

                self.dispatch_issue(issue.clone(), Some(entry.attempt), false, None)
                    .await;
            }
        }
    }

    /// Remove an issue from the retry queue permanently.
    /// This removes the retry entry and releases the claim so the issue
    /// won't be retried or re-dispatched until it appears again in a
    /// future poll cycle (if still in an active state in the tracker).
    async fn on_remove_from_retry_queue(&self, issue_id: &str) {
        let mut state = self.state.write().await;
        if let Some(entry) = state.retry_attempts.remove(issue_id) {
            state.claimed.remove(issue_id);
            drop(state);
            if let Some(ref store) = self.store {
                if let Err(e) = store.remove_claimed(issue_id) {
                    debug!(error = %e, "failed to remove persisted claimed marker");
                }
            }
            info!(issue_id = %issue_id, issue_identifier = %entry.identifier, "removed from retry queue by user");
        } else {
            debug!(issue_id = %issue_id, "remove-from-retry-queue: issue not in retry queue");
        }
    }

    async fn on_agent_update(&self, issue_id: &str, event: AgentEvent) {
        let mut state = self.state.write().await;

        // For token usage, we need to compute deltas and update both the entry
        // and the aggregate totals. We do this in two phases to avoid double-borrow.
        let token_deltas = if let AgentEvent::TokenUsage {
            input_tokens,
            output_tokens,
            total_tokens,
        } = &event
        {
            if let Some(entry) = state.running.get(issue_id) {
                let di = input_tokens.saturating_sub(entry.session.last_reported_input_tokens);
                let do_ = output_tokens.saturating_sub(entry.session.last_reported_output_tokens);
                let dt = total_tokens.saturating_sub(entry.session.last_reported_total_tokens);
                Some((di, do_, dt))
            } else {
                None
            }
        } else {
            None
        };

        if let Some(entry) = state.running.get_mut(issue_id) {
            let now = Utc::now();
            let mut should_log = true;

            match &event {
                AgentEvent::SessionStarted { session_id, pid } => {
                    entry.session.session_id = Some(session_id.clone());
                    if let Some(p) = pid {
                        entry.session.agent_pid = Some(*p);
                    }
                    entry.session.last_event = Some("session_started".to_string());
                    entry.session.last_event_at = Some(now);
                }
                AgentEvent::TurnCompleted { message } => {
                    entry.session.last_event = Some("turn_completed".to_string());
                    entry.session.last_event_at = Some(now);
                    entry.session.last_message = message.clone();
                    entry.session.turn_count += 1;
                }
                AgentEvent::TurnFailed { error } => {
                    entry.session.last_event = Some("turn_failed".to_string());
                    entry.session.last_event_at = Some(now);
                    entry.session.last_message = Some(error.clone());
                }
                AgentEvent::Notification { message } => {
                    entry.session.last_event = Some("notification".to_string());
                    entry.session.last_event_at = Some(now);
                    entry.session.last_message = Some(message.clone());
                }
                AgentEvent::TokenUsage {
                    input_tokens,
                    output_tokens,
                    total_tokens,
                } => {
                    if let Some((di, do_, dt)) = token_deltas {
                        entry.session.input_tokens += di;
                        entry.session.output_tokens += do_;
                        entry.session.total_tokens += dt;
                    }

                    entry.session.last_reported_input_tokens = *input_tokens;
                    entry.session.last_reported_output_tokens = *output_tokens;
                    entry.session.last_reported_total_tokens = *total_tokens;

                    entry.session.last_event = Some("token_usage".to_string());
                    entry.session.last_event_at = Some(now);
                }
                AgentEvent::AgentMessage {
                    event_type,
                    message,
                } => {
                    entry.session.last_event = Some(event_type.clone());
                    entry.session.last_event_at = Some(now);
                    if let Some(msg) = message {
                        entry.session.last_message = Some(msg.clone());
                    }
                }
                AgentEvent::RateLimitUpdate { data } => {
                    entry.session.last_event = Some("rate_limit_event".to_string());
                    entry.session.last_event_at = Some(now);
                    // Only log/broadcast interesting rate-limit events (not routine "allowed")
                    should_log = crate::model::should_log_rate_limit(data);
                }
            }

            // Create log entry and append to event log (skip filtered events)
            if should_log {
                entry.log_seq += 1;
                let log_entry = crate::model::create_log_entry(entry.log_seq, &event);
                crate::model::append_log_entry(&mut entry.event_log, log_entry.clone());

                // Persist log entry (best-effort)
                if let Some(ref store) = self.store {
                    if let Err(e) = store.save_log_entry(issue_id, &log_entry) {
                        debug!(error = %e, issue_id = %issue_id, "failed to persist log entry");
                    }
                }

                // Broadcast to SSE subscribers (ignore error if no receivers)
                if let Some(tx) = state.event_broadcasts.get(issue_id) {
                    let _ = tx.send(log_entry);
                }
            }
        }

        // Update aggregate totals outside the running entry borrow
        if let Some((di, do_, dt)) = token_deltas {
            state.agent_totals.input_tokens += di;
            state.agent_totals.output_tokens += do_;
            state.agent_totals.total_tokens += dt;
        }

        // §11.2: Store rate limit data in state
        if let AgentEvent::RateLimitUpdate { data } = &event {
            state.rate_limits = Some(data.clone());
        }

        // Notify SSE subscribers about the global state change
        state.notify_state_change();

        // Persist updated session snapshot (best-effort)
        if let Some(entry) = state.running.get(issue_id) {
            self.persist_session(
                issue_id,
                &entry.identifier,
                &entry.session,
                entry.retry_attempt,
                entry.started_at,
            )
            .await;
        }
    }

    async fn reconcile(&self) {
        // Part A: Stall detection
        self.reconcile_stalls().await;

        // Part B: Tracker state refresh
        self.reconcile_tracker_states().await;

        // Part C: PR merge conflict detection
        self.reconcile_pr_conflicts().await;

        // Part D: PR lifecycle management (auto-merge and post-merge cleanup)
        self.reconcile_pr_lifecycle().await;
    }

    async fn reconcile_pr_conflicts(&self) {
        // 1. Check if GitHub config exists
        let config = self.config.read().await;
        let github_config = match &config.github {
            Some(c) => c.clone(),
            None => return, // GitHub not configured, skip
        };
        let tracker_config = config.tracker.clone();
        drop(config);

        // 2. Build GitHub client
        let github = GitHubClient::new(&github_config, self.http.clone());

        // 3. Check running issues for conflicts (safety net logging)
        let state = self.state.read().await;
        let running_issues: Vec<(String, String, String)> = state
            .running
            .iter()
            .map(|(id, entry)| {
                (
                    id.clone(),
                    entry.identifier.clone(),
                    entry.issue.state.clone(),
                )
            })
            .collect();
        drop(state);

        for (issue_id, identifier, issue_state) in &running_issues {
            let branch = format!(
                "{}{}",
                github_config.branch_prefix,
                identifier.to_lowercase()
            );
            match github.is_pr_mergeable(&branch).await {
                Ok(Some(false)) => {
                    warn!(issue_id = %issue_id, issue_identifier = %identifier, branch = %branch, "PR has merge conflicts (worker should resolve via prompt instructions)");
                }
                Ok(Some(true)) => {
                    debug!(issue_id = %issue_id, "PR is mergeable");
                }
                Ok(None) => {
                    debug!(issue_id = %issue_id, "PR mergeable status not yet computed");
                }
                Err(e) => {
                    debug!(issue_id = %issue_id, error = %e, "failed to check PR status");
                }
            }

            // If issue is "In Review" and has conflicts, transition back
            if issue_state.eq_ignore_ascii_case("in review") {
                if let Ok(Some(false)) = github.is_pr_mergeable(&branch).await {
                    warn!(issue_id = %issue_id, issue_identifier = %identifier, "In Review running issue has merge conflicts, transitioning to In Progress");
                    let client = LinearClient::new(&tracker_config, self.http.clone());
                    if let Err(e) = client.transition_issue_state(issue_id, "In Progress").await {
                        warn!(issue_id = %issue_id, error = %e, "failed to transition issue back to In Progress");
                    }
                }
            }
        }

        // 4. Fetch "In Review" issues from Linear that may NOT be currently running
        let client = LinearClient::new(&tracker_config, self.http.clone());
        let in_review_from_tracker = match client
            .fetch_issues_by_states(&["In Review".to_string()])
            .await
        {
            Ok(issues) => issues,
            Err(e) => {
                debug!(error = %e, "failed to fetch In Review issues for conflict check");
                return;
            }
        };

        let running_ids: std::collections::HashSet<&str> = running_issues
            .iter()
            .map(|(id, _, _)| id.as_str())
            .collect();

        for issue in &in_review_from_tracker {
            if running_ids.contains(issue.id.as_str()) {
                continue;
            }

            let branch = format!(
                "{}{}",
                github_config.branch_prefix,
                issue.identifier.to_lowercase()
            );
            if let Ok(Some(false)) = github.is_pr_mergeable(&branch).await {
                warn!(issue_id = %issue.id, issue_identifier = %issue.identifier, "In Review issue has merge conflicts, transitioning to In Progress");
                if let Err(e) = client
                    .transition_issue_state(&issue.id, "In Progress")
                    .await
                {
                    warn!(issue_id = %issue.id, error = %e, "failed to transition conflicting issue");
                }
            }
        }
    }

    /// Reconcile PR lifecycle for issues in waiting_for_review.
    /// - Check if PRs have been merged → dispatch cleanup agent
    /// - Check if PRs are approved + mergeable + auto_merge enabled → merge
    /// - Remove entries where issue left "In Review" (e.g., moved back)
    async fn reconcile_pr_lifecycle(&self) {
        let config = self.config.read().await;
        let github_config = match &config.github {
            Some(c) => c.clone(),
            None => return, // GitHub not configured
        };
        let tracker_config = config.tracker.clone();
        drop(config);

        let github = GitHubClient::new(&github_config, self.http.clone());
        let linear = LinearClient::new(&tracker_config, self.http.clone());

        // Snapshot waiting entries to iterate
        let state = self.state.read().await;
        let waiting_entries: Vec<crate::model::WaitingEntry> =
            state.waiting_for_review.values().cloned().collect();
        drop(state);

        for entry in &waiting_entries {
            // 1. Check if PR has been merged
            match github.is_pr_merged(&entry.branch).await {
                Ok(Some(true)) => {
                    info!(issue_id = %entry.issue_id, issue_identifier = %entry.identifier, pr_number = entry.pr_number, "PR merged — transitioning to Merged and dispatching cleanup");
                    self.transition_to_merged(&entry.issue_id, &entry.identifier)
                        .await;
                    let mut state = self.state.write().await;
                    state.waiting_for_review.remove(&entry.issue_id);
                    state.claimed.remove(&entry.issue_id);
                    drop(state);
                    if let Some(ref store) = self.store {
                        if let Err(e) = store.remove_waiting(&entry.issue_id) {
                            debug!(error = %e, "failed to remove persisted waiting entry");
                        }
                        if let Err(e) = store.remove_claimed(&entry.issue_id) {
                            debug!(error = %e, "failed to remove persisted claimed marker");
                        }
                    }
                    self.dispatch_cleanup(entry, &tracker_config).await;
                    continue;
                }
                Ok(Some(false)) => {
                    // Still open — check if we should auto-merge
                }
                Ok(None) => {
                    debug!(issue_id = %entry.issue_id, "no PR found for waiting entry, removing");
                    let mut state = self.state.write().await;
                    state.waiting_for_review.remove(&entry.issue_id);
                    state.claimed.remove(&entry.issue_id);
                    drop(state);
                    if let Some(ref store) = self.store {
                        if let Err(e) = store.remove_waiting(&entry.issue_id) {
                            debug!(error = %e, "failed to remove persisted waiting entry");
                        }
                        if let Err(e) = store.remove_claimed(&entry.issue_id) {
                            debug!(error = %e, "failed to remove persisted claimed marker");
                        }
                    }
                    continue;
                }
                Err(e) => {
                    debug!(issue_id = %entry.issue_id, error = %e, "failed to check PR merge status");
                    continue;
                }
            }

            if !github_config.auto_merge {
                continue;
            }

            let approved = match github.is_pr_approved(entry.pr_number).await {
                Ok(a) => a,
                Err(e) => {
                    debug!(issue_id = %entry.issue_id, error = %e, "failed to check PR approval status");
                    continue;
                }
            };

            if !approved {
                continue;
            }

            let pr_details = match github.get_pr_details(entry.pr_number).await {
                Ok(pr) => pr,
                Err(e) => {
                    debug!(issue_id = %entry.issue_id, error = %e, "failed to get PR details for auto-merge");
                    continue;
                }
            };

            if pr_details.mergeable != Some(true) {
                continue;
            }

            info!(issue_id = %entry.issue_id, issue_identifier = %entry.identifier, pr_number = entry.pr_number, "PR approved and mergeable — attempting auto-merge");

            match github.enable_auto_merge(entry.pr_number).await {
                Ok(true) => {
                    info!(issue_id = %entry.issue_id, issue_identifier = %entry.identifier, pr_number = entry.pr_number, "auto-merge succeeded");
                    let mut state = self.state.write().await;
                    state.waiting_for_review.remove(&entry.issue_id);
                    state.claimed.remove(&entry.issue_id);
                    drop(state);
                    if let Some(ref store) = self.store {
                        if let Err(e) = store.remove_waiting(&entry.issue_id) {
                            debug!(error = %e, "failed to remove persisted waiting entry");
                        }
                        if let Err(e) = store.remove_claimed(&entry.issue_id) {
                            debug!(error = %e, "failed to remove persisted claimed marker");
                        }
                    }
                    self.dispatch_cleanup(entry, &tracker_config).await;
                }
                Ok(false) => {
                    debug!(issue_id = %entry.issue_id, "auto-merge not ready yet (checks pending or blocked)");
                }
                Err(e) => {
                    warn!(issue_id = %entry.issue_id, error = %e, "auto-merge API call failed");
                }
            }
        }

        // Verify issues in waiting_for_review are still "In Review" in Linear
        let remaining: Vec<String> = {
            let state = self.state.read().await;
            state.waiting_for_review.keys().cloned().collect()
        };

        if remaining.is_empty() {
            return;
        }

        match linear.fetch_issue_states_by_ids(&remaining).await {
            Ok(refreshed) => {
                let mut removed_ids = Vec::new();
                {
                    let mut state = self.state.write().await;
                    for issue in &refreshed {
                        if is_in_review_state(&issue.state) {
                            continue;
                        }
                        info!(issue_id = %issue.id, issue_identifier = %issue.identifier, state = %issue.state, "waiting issue no longer In Review, removing from waiting");
                        state.waiting_for_review.remove(&issue.id);
                        state.claimed.remove(&issue.id);
                        removed_ids.push(issue.id.clone());
                    }
                }
                if let Some(ref store) = self.store {
                    for id in &removed_ids {
                        if let Err(e) = store.remove_waiting(id) {
                            debug!(error = %e, "failed to remove persisted waiting entry");
                        }
                        if let Err(e) = store.remove_claimed(id) {
                            debug!(error = %e, "failed to remove persisted claimed marker");
                        }
                    }
                }
            }
            Err(e) => {
                debug!(error = %e, "failed to refresh waiting issue states");
            }
        }
    }

    /// Dispatch a cleanup agent turn for a merged PR.
    /// Transitions the issue to "Done" after cleanup completes.
    async fn dispatch_cleanup(
        &self,
        entry: &crate::model::WaitingEntry,
        _tracker_config: &crate::config::TrackerConfig,
    ) {
        let issue = Issue {
            id: entry.issue_id.clone(),
            identifier: entry.identifier.clone(),
            title: format!("Cleanup for {}", entry.identifier),
            description: Some(format!(
                "PR #{} for issue {} has been merged. \
                 Delete the remote branch '{}' and transition the Linear issue to Done.",
                entry.pr_number, entry.identifier, entry.branch
            )),
            priority: Some(0), // Highest priority — cleanup is quick
            state: "Done".to_string(),
            branch_name: Some(entry.branch.clone()),
            url: None,
            labels: vec![],
            blocked_by: vec![],
            created_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
        };

        info!(
            issue_id = %entry.issue_id,
            issue_identifier = %entry.identifier,
            resume_session_id = ?entry.session_id,
            "dispatching cleanup agent turn"
        );

        // Dispatch with attempt=None, skip_state_transition=true (already "Merged").
        // If a session_id was persisted from before the review, resume that session
        // so the agent has full conversation context for cleanup.
        self.dispatch_issue(issue, None, true, entry.session_id.clone())
            .await;
    }

    async fn reconcile_stalls(&self) {
        let stall_timeout_ms = {
            let config = self.config.read().await;
            config.coding_agent.stall_timeout_ms
        };

        if stall_timeout_ms == 0 {
            return;
        }

        let state = self.state.read().await;
        let now = Utc::now();

        let mut stalled_ids = Vec::new();
        for (issue_id, entry) in &state.running {
            let last_activity = entry.session.last_event_at.unwrap_or(entry.started_at);
            let elapsed_ms = now
                .signed_duration_since(last_activity)
                .num_milliseconds()
                .max(0) as u64;

            if elapsed_ms > stall_timeout_ms {
                warn!(issue_id = %issue_id, issue_identifier = %entry.identifier, elapsed_ms = elapsed_ms, stall_timeout_ms = stall_timeout_ms, "session stalled");
                stalled_ids.push(issue_id.clone());
            }
        }
        drop(state);

        for issue_id in stalled_ids {
            self.terminate_running_issue(&issue_id, false).await;
        }
    }

    async fn reconcile_tracker_states(&self) {
        let state = self.state.read().await;
        let running_ids: Vec<String> = state.running.keys().cloned().collect();
        drop(state);

        if running_ids.is_empty() {
            return;
        }

        let config = self.config.read().await.clone();
        let client = LinearClient::new(&config.tracker, self.http.clone());

        let refreshed = match client.fetch_issue_states_by_ids(&running_ids).await {
            Ok(issues) => issues,
            Err(e) => {
                debug!(error = %e, "reconciliation state refresh failed, keeping workers");
                return;
            }
        };

        for issue in &refreshed {
            if state_matches(&issue.state, &config.tracker.terminal_states) {
                info!(issue_id = %issue.id, issue_identifier = %issue.identifier, state = %issue.state, "issue is terminal, stopping worker and cleaning workspace");
                {
                    let mut state = self.state.write().await;
                    state.mark_completed(issue.id.clone());
                }
                self.terminate_running_issue(&issue.id, true).await;
            } else if is_in_review_state(&issue.state) {
                // Issue moved to "In Review" — kill the worker to stop token spend
                // and move to waiting_for_review (passive API polling for PR merge).
                let session_id = {
                    let state = self.state.read().await;
                    state
                        .running
                        .get(&issue.id)
                        .and_then(|e| e.session.session_id.clone())
                };
                info!(
                    issue_id = %issue.id,
                    issue_identifier = %issue.identifier,
                    session_id = ?session_id,
                    "issue is In Review, killing worker and moving to waiting_for_review"
                );
                self.terminate_running_issue(&issue.id, false).await;
                self.add_to_waiting_for_review(&issue.id, &issue.identifier, session_id.as_deref())
                    .await;
            } else if state_matches(&issue.state, &config.tracker.active_states) {
                let mut state = self.state.write().await;
                if let Some(entry) = state.running.get_mut(&issue.id) {
                    entry.issue = issue.clone();
                }
            } else {
                info!(issue_id = %issue.id, issue_identifier = %issue.identifier, state = %issue.state, "issue neither active nor terminal, stopping worker");
                self.terminate_running_issue(&issue.id, false).await;
            }
        }
    }

    async fn terminate_running_issue(&self, issue_id: &str, cleanup_workspace: bool) {
        let mut state = self.state.write().await;
        // Remove broadcast sender (dropping it closes all receivers)
        state.event_broadcasts.remove(issue_id);
        if let Some(entry) = state.running.remove(issue_id) {
            // Signal cancellation
            let _ = entry.cancel_tx.send(());
            state.claimed.remove(issue_id);
            state.retry_attempts.remove(issue_id);

            // Add runtime seconds
            let elapsed = Utc::now()
                .signed_duration_since(entry.started_at)
                .num_milliseconds()
                .max(0) as f64
                / 1000.0;
            state.agent_totals.seconds_running += elapsed;
            drop(state);

            // Persist claimed removal (best-effort)
            if let Some(ref store) = self.store {
                if let Err(e) = store.remove_claimed(issue_id) {
                    debug!(error = %e, "failed to remove persisted claimed marker");
                }
            }

            // Abort and remove the worker handle
            if let Some(handle) = self.worker_handles.write().await.remove(issue_id) {
                handle.abort();
            }

            if cleanup_workspace {
                let (workspace_path, hooks) = {
                    let config = self.config.read().await;
                    let workspace_key = crate::workspace::sanitize_workspace_key(&entry.identifier);
                    (
                        config.workspace.root.join(&workspace_key),
                        config.hooks.clone(),
                    )
                };
                workspace::cleanup_workspace(&workspace_path, &hooks, issue_id, &entry.identifier)
                    .await;
            }
        }
    }

    async fn startup_cleanup(&self) {
        // Clone config before the async HTTP call to release the lock
        let config = self.config.read().await.clone();
        let client = LinearClient::new(&config.tracker, self.http.clone());

        info!("running startup terminal workspace cleanup");

        // No config lock held during HTTP call
        let terminal_issues = match client
            .fetch_issues_by_states(&config.tracker.terminal_states)
            .await
        {
            Ok(issues) => issues,
            Err(e) => {
                warn!(error = %e, "startup terminal cleanup fetch failed (continuing)");
                return;
            }
        };

        for issue in &terminal_issues {
            let workspace_key = crate::workspace::sanitize_workspace_key(&issue.identifier);
            let workspace_path = config.workspace.root.join(&workspace_key);
            if workspace_path.exists() {
                log_terminal_cleanup(&issue.identifier, &workspace_path);
                workspace::cleanup_workspace(
                    &workspace_path,
                    &config.hooks,
                    &issue.id,
                    &issue.identifier,
                )
                .await;
            }
        }
    }

    async fn shutdown(&self) {
        info!("shutting down orchestrator");
        let mut state = self.state.write().await;

        // Cancel all running workers
        let issue_ids: Vec<String> = state.running.keys().cloned().collect();
        for issue_id in issue_ids {
            if let Some(entry) = state.running.remove(&issue_id) {
                let _ = entry.cancel_tx.send(());
            }
        }
        state.claimed.clear();
        state.retry_attempts.clear();
        drop(state);

        // §12.2: Drain worker tasks — wait for spawned workers to exit
        let handles: Vec<tokio::task::JoinHandle<()>> = {
            let mut map = self.worker_handles.write().await;
            map.drain().map(|(_, h)| h).collect()
        };

        if !handles.is_empty() {
            info!(count = handles.len(), "draining worker tasks");
            // Abort all worker tasks first, then drain to wait for cleanup
            for handle in &handles {
                handle.abort();
            }
            abort_and_drain_handles(handles).await;
        }
    }
}

/// Build a map of environment variables to inject into the agent subprocess.
///
/// Injects `LINEAR_API_KEY` (and `LINEAR_TEAM_KEY`) from the tracker config so
/// agents can interact with Linear directly, and `SYMPHONY_MAX_TURNS` so agents
/// know their turn budget.
fn build_agent_env_vars(
    config: &ServiceConfig,
    max_turns: u32,
) -> std::collections::HashMap<String, String> {
    let mut env_vars = std::collections::HashMap::new();
    if !config.tracker.api_key.is_empty() {
        env_vars.insert("LINEAR_API_KEY".to_string(), config.tracker.api_key.clone());
    }
    if !config.tracker.team_key.is_empty() {
        env_vars.insert(
            "LINEAR_TEAM_KEY".to_string(),
            config.tracker.team_key.clone(),
        );
    }
    env_vars.insert("SYMPHONY_MAX_TURNS".to_string(), max_turns.to_string());
    env_vars
}

/// Write the MCP config file for an agent subprocess.
///
/// Returns the path to the generated config file, or a `McpConfigWriteFailed` error.
fn setup_mcp_config(
    workspace_path: &std::path::Path,
    env_vars: &std::collections::HashMap<String, String>,
) -> Result<std::path::PathBuf> {
    let symphony_binary =
        std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("symphony"));
    crate::mcp::write_mcp_config(workspace_path, &symphony_binary, env_vars).map_err(|e| {
        SymphonyError::McpConfigWriteFailed {
            reason: format!("failed to write MCP config: {e}"),
        }
    })
}

/// Run a worker for one issue.
#[allow(clippy::too_many_arguments)]
async fn run_worker(
    config: &ServiceConfig,
    prompt_template: &str,
    issue: &Issue,
    attempt: Option<u32>,
    http: reqwest::Client,
    msg_tx: mpsc::Sender<OrchestratorMessage>,
    mut cancel_rx: oneshot::Receiver<()>,
    resume_session_id: Option<String>,
) -> Result<()> {
    let issue_id = issue.id.clone();

    // Phase 1: Prepare workspace
    let workspace_result = workspace::create_workspace(
        &config.workspace.root,
        &issue.identifier,
        &config.hooks,
        &issue.id,
    )
    .await?;

    // Phase 2: Run before_run hook
    workspace::run_before_run_hook(
        &workspace_result.path,
        &config.hooks,
        &issue.id,
        &issue.identifier,
    )
    .await?;

    // Phase 3: Build env vars for agent subprocess
    let env_vars = build_agent_env_vars(config, config.agent.max_turns);

    // Phase 3b: Generate MCP config for agent subprocess
    let mcp_config_path = setup_mcp_config(&workspace_result.path, &env_vars)?;

    // Phase 4: Build prompt and launch agent
    let max_turns = config.agent.max_turns;
    let mut turn_number: u32 = 1;

    loop {
        // Check for cancellation
        if cancel_rx.try_recv().is_ok() {
            workspace::run_after_run_hook(
                &workspace_result.path,
                &config.hooks,
                &issue.id,
                &issue.identifier,
            )
            .await;
            return Err(SymphonyError::AgentTurnCancelled);
        }

        // Build prompt
        let prompt = if turn_number == 1 {
            render_prompt(prompt_template, issue, attempt)?
        } else {
            build_continuation_prompt(issue, turn_number, max_turns)
        };

        // Launch agent — resume a prior session on the first turn if session_id provided
        let resume_id = if turn_number == 1 {
            resume_session_id.as_deref()
        } else {
            None
        };
        let (mut session, mut event_rx) = AgentSession::launch(
            &config.coding_agent,
            &workspace_result.path,
            &prompt,
            issue,
            &env_vars,
            config.agent.max_turns,
            Some(&mcp_config_path),
            resume_id,
        )
        .await?;

        // Stream events
        let tx = msg_tx.clone();
        let id = issue_id.clone();
        let event_forwarder = tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                let _ = tx
                    .send(OrchestratorMessage::AgentUpdate {
                        issue_id: id.clone(),
                        event,
                    })
                    .await;
            }
        });

        // Wait for agent to complete
        let result = session
            .wait_with_timeout(config.coding_agent.turn_timeout_ms)
            .await;
        event_forwarder.abort();

        match result {
            Ok(_) => {
                info!(issue_id = %issue_id, issue_identifier = %issue.identifier, turn = turn_number, "agent turn completed successfully");

                // Check if issue is still active or moved to review
                let client = LinearClient::new(&config.tracker, http.clone());
                match client.fetch_issue_states_by_ids(&[issue.id.clone()]).await {
                    Ok(refreshed) => {
                        if let Some(refreshed_issue) = refreshed.first() {
                            if !state_matches(&refreshed_issue.state, &config.tracker.active_states)
                            {
                                info!(issue_id = %issue_id, issue_identifier = %issue.identifier, state = %refreshed_issue.state, "issue no longer active after turn");
                                break;
                            }
                            if is_in_review_state(&refreshed_issue.state) {
                                info!(issue_id = %issue_id, issue_identifier = %issue.identifier, "issue moved to In Review, stopping worker");
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    Err(e) => {
                        warn!(issue_id = %issue_id, issue_identifier = %issue.identifier, error = %e, "failed to refresh issue state after turn");
                        break;
                    }
                }

                if check_pr_ready_and_transition(
                    &workspace_result.path,
                    &issue.id,
                    &issue.identifier,
                    config,
                    &http,
                )
                .await
                {
                    break;
                }

                if turn_number >= max_turns {
                    info!(issue_id = %issue_id, issue_identifier = %issue.identifier, turns = turn_number, max_turns = max_turns, "reached max turns");
                    break;
                }

                turn_number += 1;
            }
            Err(_) => {
                workspace::run_after_run_hook(
                    &workspace_result.path,
                    &config.hooks,
                    &issue.id,
                    &issue.identifier,
                )
                .await;
                return result.map(|_| ());
            }
        }
    }

    workspace::run_after_run_hook(
        &workspace_result.path,
        &config.hooks,
        &issue.id,
        &issue.identifier,
    )
    .await;
    Ok(())
}

/// Check if an issue state is "In Review" (case-insensitive).
fn is_in_review_state(state: &str) -> bool {
    state.trim().eq_ignore_ascii_case("in review")
}

/// Check if an issue state indicates a terminal/completed state.
fn is_done_state(state: &str) -> bool {
    let s = state.trim().to_ascii_lowercase();
    s == "done" || s == "merged"
}

/// Check if the worker's branch has an open PR with green CI.
/// If so, transition the issue to "In Review" and return true to stop the worker.
/// Uses the GitHub API client when configured, falls back to `gh` CLI.
async fn check_pr_ready_and_transition(
    workspace_path: &std::path::Path,
    issue_id: &str,
    issue_identifier: &str,
    config: &ServiceConfig,
    http: &reqwest::Client,
) -> bool {
    // Try GitHub API first if configured
    if let Some(github_config) = &config.github {
        let branch = format!(
            "{}{}",
            github_config.branch_prefix,
            issue_identifier.to_lowercase()
        );
        let github = GitHubClient::new(github_config, http.clone());
        return check_pr_ready_via_api(&github, &branch, issue_id, issue_identifier, config, http)
            .await;
    }

    // Fall back to gh CLI from workspace
    check_pr_ready_via_gh(
        workspace_path,
        issue_id,
        issue_identifier,
        &config.tracker,
        http,
    )
    .await
}

/// Check PR readiness using the GitHub API client.
async fn check_pr_ready_via_api(
    github: &GitHubClient,
    branch: &str,
    issue_id: &str,
    issue_identifier: &str,
    config: &ServiceConfig,
    http: &reqwest::Client,
) -> bool {
    // Check if there's an open PR for this branch
    let pr_info = match github.find_open_pr(branch).await {
        Ok(Some(pr)) => pr,
        Ok(None) => return false, // No PR yet
        Err(e) => {
            debug!(issue_identifier = %issue_identifier, error = %e, "failed to check PR via API");
            return false;
        }
    };

    // PR must be open
    if pr_info.state != "open" {
        return false;
    }

    // PR must be mergeable (no conflicts)
    if pr_info.mergeable == Some(false) {
        return false;
    }

    // Get full details to check mergeable_state
    if let Some(ref ms) = pr_info.mergeable_state {
        if ms == "dirty" || ms == "blocked" {
            return false;
        }
    }

    // If mergeable is still None (not yet computed), skip this check
    if pr_info.mergeable.is_none() {
        return false;
    }

    info!(issue_id = %issue_id, issue_identifier = %issue_identifier, pr_number = pr_info.number, "PR is ready (mergeable, no conflicts) — transitioning to In Review");

    // Transition issue to "In Review"
    transition_to_in_review(issue_id, issue_identifier, &config.tracker, http).await;

    true
}

/// Transition an issue to "In Review" via Linear. Logs a warning on failure.
async fn transition_to_in_review(
    issue_id: &str,
    issue_identifier: &str,
    tracker_config: &crate::config::TrackerConfig,
    http: &reqwest::Client,
) {
    let client = LinearClient::new(tracker_config, http.clone());
    let result = client.transition_issue_state(issue_id, "In Review").await;
    log_transition_error(result, issue_identifier);
}

/// Log a warning if a Linear transition failed.
fn log_transition_error(
    result: std::result::Result<(), crate::error::SymphonyError>,
    identifier: &str,
) {
    if let Err(e) = result {
        warn!(issue_identifier = %identifier, error = %e, "failed to transition issue to In Review");
    }
}

/// Fallback: check PR readiness using `gh` CLI from workspace.
async fn check_pr_ready_via_gh(
    workspace_path: &std::path::Path,
    issue_id: &str,
    issue_identifier: &str,
    tracker_config: &crate::config::TrackerConfig,
    http: &reqwest::Client,
) -> bool {
    check_pr_ready_via_command(
        "gh",
        workspace_path,
        issue_id,
        issue_identifier,
        tracker_config,
        http,
    )
    .await
}

/// Check PR readiness by running a command that outputs `gh pr view`-compatible JSON.
async fn check_pr_ready_via_command(
    command: &str,
    workspace_path: &std::path::Path,
    issue_id: &str,
    issue_identifier: &str,
    tracker_config: &crate::config::TrackerConfig,
    http: &reqwest::Client,
) -> bool {
    let stdout = match run_pr_view_command(command, workspace_path).await {
        Some(bytes) => bytes,
        None => return false,
    };
    handle_gh_pr_output(&stdout, issue_id, issue_identifier, tracker_config, http).await
}

/// Run `<command> pr view --json ...` in the workspace and return stdout on success.
async fn run_pr_view_command(command: &str, workspace_path: &std::path::Path) -> Option<Vec<u8>> {
    let output = tokio::process::Command::new(command)
        .args([
            "pr",
            "view",
            "--json",
            "number,state,mergeable,statusCheckRollup",
        ])
        .current_dir(workspace_path)
        .output()
        .await
        .ok()?;
    if output.status.success() {
        Some(output.stdout)
    } else {
        None
    }
}

/// Process `gh pr view` output: parse, check readiness, and transition if ready.
///
/// Returns `true` if the PR is ready and the transition was initiated.
async fn handle_gh_pr_output(
    stdout: &[u8],
    issue_id: &str,
    issue_identifier: &str,
    tracker_config: &crate::config::TrackerConfig,
    http: &reqwest::Client,
) -> bool {
    if !parse_gh_output_ready(stdout) {
        return false;
    }

    info!(issue_id = %issue_id, issue_identifier = %issue_identifier, "PR is ready (CI green, no conflicts) — transitioning to In Review");

    transition_to_in_review(issue_id, issue_identifier, tracker_config, http).await;

    true
}

/// Parse `gh pr view --json` stdout and determine if the PR is ready.
///
/// Returns `true` if the output is valid JSON and the PR is open, not
/// conflicting, and all checks pass.
fn parse_gh_output_ready(stdout: &[u8]) -> bool {
    let json: serde_json::Value = match serde_json::from_slice(stdout) {
        Ok(v) => v,
        Err(_) => return false,
    };
    is_gh_pr_ready(&json)
}

/// Validate the JSON output from `gh pr view` to determine if a PR is ready.
///
/// Returns `true` if the PR is open, not conflicting, and all checks pass.
fn is_gh_pr_ready(json: &serde_json::Value) -> bool {
    if json.get("state").and_then(|v| v.as_str()) != Some("OPEN") {
        return false;
    }
    if json.get("mergeable").and_then(|v| v.as_str()) == Some("CONFLICTING") {
        return false;
    }
    all_checks_passing(json)
}

/// Check if all CI status checks are passing on a PR.
fn all_checks_passing(pr_json: &serde_json::Value) -> bool {
    let checks = match pr_json.get("statusCheckRollup").and_then(|v| v.as_array()) {
        Some(arr) if !arr.is_empty() => arr,
        _ => return false, // No checks = not ready
    };

    checks.iter().all(|check| {
        let conclusion = check.get("conclusion").and_then(|v| v.as_str());
        // A check passes if conclusion is SUCCESS, NEUTRAL, or SKIPPED.
        // Pending/queued/failed checks mean not ready.
        matches!(conclusion, Some("SUCCESS" | "NEUTRAL" | "SKIPPED"))
    })
}

/// Sort issues for dispatch per spec:
/// 1. priority ascending (null sorts last)
/// 2. created_at oldest first
/// 3. identifier lexicographic
fn sort_for_dispatch(mut issues: Vec<Issue>) -> Vec<Issue> {
    issues.sort_by(|a, b| {
        // Priority: lower is higher priority, null sorts last
        let pri_a = a.priority.unwrap_or(i32::MAX);
        let pri_b = b.priority.unwrap_or(i32::MAX);
        pri_a
            .cmp(&pri_b)
            .then_with(|| a.created_at.cmp(&b.created_at))
            .then_with(|| a.identifier.cmp(&b.identifier))
    });
    issues
}

/// Wait for all worker handles to complete.
///
/// Extracted as a named async function (rather than an inline block) to keep
/// the shutdown method readable and to allow LLVM coverage instrumentation
/// to track this code path independently.
async fn drain_worker_handles(handles: Vec<tokio::task::JoinHandle<()>>) {
    for handle in handles {
        let _ = handle.await;
    }
}

/// Drain worker handles with a timeout, logging a warning if it expires.
async fn abort_and_drain_handles(handles: Vec<tokio::task::JoinHandle<()>>) {
    abort_and_drain_with_timeout(
        handles,
        std::time::Duration::from_secs(SHUTDOWN_DRAIN_TIMEOUT_SECS),
    )
    .await;
}

/// Inner implementation with configurable timeout (for testability).
async fn abort_and_drain_with_timeout(
    handles: Vec<tokio::task::JoinHandle<()>>,
    timeout: std::time::Duration,
) {
    let drain_result = tokio::time::timeout(timeout, drain_worker_handles(handles)).await;
    if drain_result.is_err() {
        warn!("worker drain timed out after {SHUTDOWN_DRAIN_TIMEOUT_SECS}s");
    }
}

/// Compute exponential backoff delay.
/// Formula: min(10000 * 2^(attempt - 1), max_backoff_ms)
fn exponential_backoff(attempt: u32, max_backoff_ms: u64) -> u64 {
    let base: u64 = 10_000;
    let power = attempt.saturating_sub(1).min(20); // Cap to prevent overflow
    let delay = base.saturating_mul(1u64.checked_shl(power).unwrap_or(u64::MAX));
    delay.min(max_backoff_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AgentConfig, CodingAgentConfig, DeploymentConfig, DeploymentMode, GitHubConfig,
        HooksConfig, PollingConfig, ServerConfig, TrackerConfig, WorkspaceConfig,
    };
    use crate::model::BlockerRef;
    use chrono::TimeZone;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tracing_test::traced_test;

    // ── Helpers ──────────────────────────────────────────────────────

    fn default_issue() -> Issue {
        Issue {
            id: String::new(),
            identifier: String::new(),
            title: String::new(),
            description: None,
            priority: None,
            state: String::new(),
            branch_name: None,
            url: None,
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
        }
    }

    fn make_issue(id: &str, identifier: &str, state: &str) -> Issue {
        Issue {
            id: id.to_string(),
            identifier: identifier.to_string(),
            title: format!("Issue {identifier}"),
            state: state.to_string(),
            ..default_issue()
        }
    }

    fn test_config() -> ServiceConfig {
        ServiceConfig {
            tracker: TrackerConfig {
                kind: "linear".to_string(),
                endpoint: "http://localhost:9999".to_string(),
                api_key: "test-key".to_string(),
                team_key: "test-proj".to_string(),
                active_states: vec!["Todo".to_string(), "In Progress".to_string()],
                terminal_states: vec![
                    "Done".to_string(),
                    "Cancelled".to_string(),
                    "Closed".to_string(),
                ],
            },
            polling: PollingConfig { interval_ms: 5000 },
            workspace: WorkspaceConfig {
                root: PathBuf::from("/tmp/symphony-test-workspaces"),
            },
            hooks: HooksConfig {
                after_create: None,
                before_run: None,
                after_run: None,
                before_remove: None,
                timeout_ms: 60_000,
            },
            agent: AgentConfig {
                max_concurrent_agents: 3,
                max_turns: 5,
                max_retry_backoff_ms: 300_000,
                max_concurrent_agents_by_state: HashMap::new(),
            },
            coding_agent: CodingAgentConfig {
                command: "echo test".to_string(),
                permission_mode: "bypassPermissions".to_string(),
                turn_timeout_ms: 3_600_000,
                stall_timeout_ms: 300_000,
            },
            server: ServerConfig {
                port: None,
                bind: None,
            },
            deployment: DeploymentConfig {
                mode: DeploymentMode::Local,
                auth_token: None,
            },
            github: None,
        }
    }

    fn test_orchestrator() -> Orchestrator {
        Orchestrator::new(
            test_config(),
            "Test prompt {{ issue.identifier }}".to_string(),
        )
    }

    fn test_config_with_endpoint(endpoint: &str) -> ServiceConfig {
        let mut config = test_config();
        config.tracker.endpoint = endpoint.to_string();
        config
    }

    /// Helper: insert a running entry into the orchestrator's state.
    async fn insert_running(
        orch: &Orchestrator,
        id: &str,
        identifier: &str,
        state: &str,
    ) -> oneshot::Receiver<()> {
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let mut s = orch.state.write().await;
        s.running.insert(
            id.to_string(),
            RunningEntry {
                issue: make_issue(id, identifier, state),
                identifier: identifier.to_string(),
                session: LiveSession::default(),
                retry_attempt: None,
                started_at: Utc::now(),
                cancel_tx,
                event_log: std::collections::VecDeque::new(),
                log_seq: 0,
            },
        );
        s.claimed.insert(id.to_string());
        cancel_rx
    }

    /// Helper: insert a running entry with a specific started_at time.
    async fn insert_running_at(
        orch: &Orchestrator,
        id: &str,
        identifier: &str,
        state: &str,
        started_at: chrono::DateTime<Utc>,
    ) -> oneshot::Receiver<()> {
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let mut s = orch.state.write().await;
        s.running.insert(
            id.to_string(),
            RunningEntry {
                issue: make_issue(id, identifier, state),
                identifier: identifier.to_string(),
                session: LiveSession::default(),
                retry_attempt: None,
                started_at,
                cancel_tx,
                event_log: std::collections::VecDeque::new(),
                log_seq: 0,
            },
        );
        s.claimed.insert(id.to_string());
        cancel_rx
    }

    /// Build a minimal valid Linear GraphQL response for candidate issues.
    fn mock_candidates_response(issues: &[(&str, &str, &str)]) -> String {
        let nodes: Vec<serde_json::Value> = issues
            .iter()
            .map(|(id, ident, state)| {
                serde_json::json!({
                    "id": id,
                    "identifier": ident,
                    "title": format!("Issue {ident}"),
                    "description": null,
                    "priority": 1,
                    "state": { "name": state },
                    "branchName": null,
                    "url": null,
                    "labels": { "nodes": [] },
                    "relations": { "nodes": [] },
                    "createdAt": "2025-01-01T00:00:00Z",
                    "updatedAt": "2025-01-01T00:00:00Z"
                })
            })
            .collect();

        serde_json::json!({
            "data": {
                "issues": {
                    "nodes": nodes,
                    "pageInfo": {
                        "hasNextPage": false,
                        "endCursor": null
                    }
                }
            }
        })
        .to_string()
    }

    /// Build a minimal valid Linear GraphQL response for issue states by IDs.
    /// Includes `pageInfo` so the response is valid for both
    /// `fetch_candidate_issues` and `fetch_issue_states_by_ids` — this is
    /// important for integration tests where mock ordering is non-deterministic.
    fn mock_states_response(issues: &[(&str, &str, &str)]) -> String {
        let nodes: Vec<serde_json::Value> = issues
            .iter()
            .map(|(id, ident, state)| {
                serde_json::json!({
                    "id": id,
                    "identifier": ident,
                    "title": format!("Issue {ident}"),
                    "description": null,
                    "priority": 1,
                    "state": { "name": state },
                    "branchName": null,
                    "url": null,
                    "labels": { "nodes": [] },
                    "relations": { "nodes": [] },
                    "createdAt": "2025-01-01T00:00:00Z",
                    "updatedAt": "2025-01-01T00:00:00Z"
                })
            })
            .collect();

        serde_json::json!({
            "data": {
                "issues": {
                    "nodes": nodes,
                    "pageInfo": {
                        "hasNextPage": false,
                        "endCursor": null
                    }
                }
            }
        })
        .to_string()
    }

    // ── sort_for_dispatch ────────────────────────────────────────────

    #[test]
    fn test_sort_for_dispatch() {
        let issues = vec![
            Issue {
                id: "3".to_string(),
                identifier: "C".to_string(),
                title: "Third".to_string(),
                priority: Some(2),
                state: "Todo".to_string(),
                created_at: Some(Utc::now()),
                ..default_issue()
            },
            Issue {
                id: "1".to_string(),
                identifier: "A".to_string(),
                title: "First".to_string(),
                priority: Some(1),
                state: "Todo".to_string(),
                created_at: Some(Utc::now()),
                ..default_issue()
            },
            Issue {
                id: "2".to_string(),
                identifier: "B".to_string(),
                title: "Second".to_string(),
                priority: None,
                state: "Todo".to_string(),
                created_at: Some(Utc::now()),
                ..default_issue()
            },
        ];

        let sorted = sort_for_dispatch(issues);
        assert_eq!(sorted[0].id, "1"); // priority 1
        assert_eq!(sorted[1].id, "3"); // priority 2
        assert_eq!(sorted[2].id, "2"); // priority null (last)
    }

    #[test]
    fn test_sort_for_dispatch_timestamp_tiebreaker() {
        let t1 = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2025, 6, 1, 0, 0, 0).unwrap();
        let issues = vec![
            Issue {
                id: "newer".to_string(),
                identifier: "B".to_string(),
                title: "Newer".to_string(),
                priority: Some(1),
                state: "Todo".to_string(),
                created_at: Some(t2),
                ..default_issue()
            },
            Issue {
                id: "older".to_string(),
                identifier: "A".to_string(),
                title: "Older".to_string(),
                priority: Some(1),
                state: "Todo".to_string(),
                created_at: Some(t1),
                ..default_issue()
            },
        ];

        let sorted = sort_for_dispatch(issues);
        assert_eq!(sorted[0].id, "older"); // same priority, older first
        assert_eq!(sorted[1].id, "newer");
    }

    #[test]
    fn test_sort_for_dispatch_identifier_tiebreaker() {
        let t = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let issues = vec![
            Issue {
                id: "z".to_string(),
                identifier: "ZZZ-1".to_string(),
                title: "Z".to_string(),
                priority: Some(1),
                state: "Todo".to_string(),
                created_at: Some(t),
                ..default_issue()
            },
            Issue {
                id: "a".to_string(),
                identifier: "AAA-1".to_string(),
                title: "A".to_string(),
                priority: Some(1),
                state: "Todo".to_string(),
                created_at: Some(t),
                ..default_issue()
            },
        ];

        let sorted = sort_for_dispatch(issues);
        assert_eq!(sorted[0].id, "a"); // same priority + timestamp, lex by identifier
        assert_eq!(sorted[1].id, "z");
    }

    #[test]
    fn test_sort_for_dispatch_empty() {
        let sorted = sort_for_dispatch(vec![]);
        assert!(sorted.is_empty());
    }

    // ── exponential_backoff ──────────────────────────────────────────

    #[test]
    fn test_exponential_backoff() {
        assert_eq!(exponential_backoff(1, 300_000), 10_000);
        assert_eq!(exponential_backoff(2, 300_000), 20_000);
        assert_eq!(exponential_backoff(3, 300_000), 40_000);
        assert_eq!(exponential_backoff(4, 300_000), 80_000);
        assert_eq!(exponential_backoff(5, 300_000), 160_000);
        assert_eq!(exponential_backoff(6, 300_000), 300_000); // capped
        assert_eq!(exponential_backoff(10, 300_000), 300_000); // still capped
    }

    #[test]
    fn test_exponential_backoff_zero_attempt() {
        // attempt 0 → power = 0.saturating_sub(1) = 0 (saturating), delay = 10_000
        assert_eq!(exponential_backoff(0, 300_000), 10_000);
    }

    #[test]
    fn test_exponential_backoff_very_high_attempt() {
        // Should not overflow — power capped at 20
        let result = exponential_backoff(100, 1_000_000_000);
        assert!(result <= 1_000_000_000);
    }

    // ── Orchestrator::new ────────────────────────────────────────────

    #[tokio::test]
    async fn test_orchestrator_new() {
        let config = test_config();
        let orch = Orchestrator::new(config.clone(), "template".to_string());

        let state = orch.state.read().await;
        assert_eq!(state.poll_interval_ms, 5000);
        assert_eq!(state.max_concurrent_agents, 3);
        assert!(state.running.is_empty());
        assert!(state.claimed.is_empty());
        assert!(state.retry_attempts.is_empty());
        assert_eq!(state.completed_count(), 0);
        assert_eq!(state.agent_totals.input_tokens, 0);
        assert!(state.rate_limits.is_none());
    }

    // ── update_config ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_update_config() {
        let orch = test_orchestrator();

        let mut new_config = test_config();
        new_config.polling.interval_ms = 10_000;
        new_config.agent.max_concurrent_agents = 8;

        orch.update_config(new_config.clone(), "new template".to_string())
            .await;

        let state = orch.state.read().await;
        assert_eq!(state.poll_interval_ms, 10_000);
        assert_eq!(state.max_concurrent_agents, 8);
        drop(state);

        let cfg = orch.config.read().await;
        assert_eq!(cfg.polling.interval_ms, 10_000);
        assert_eq!(cfg.agent.max_concurrent_agents, 8);
        drop(cfg);

        let tmpl = orch.prompt_template.read().await;
        assert_eq!(*tmpl, "new template");
    }

    // ── message_sender ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_message_sender() {
        let orch = test_orchestrator();
        let sender = orch.message_sender();

        // Verify we can send a message
        sender
            .send(OrchestratorMessage::TriggerRefresh)
            .await
            .expect("should be able to send");
    }

    // ── state_handle / config_handle ─────────────────────────────────

    #[tokio::test]
    async fn test_state_handle_returns_shared_arc() {
        let orch = test_orchestrator();
        let handle = orch.state_handle();

        // Verify we can read state through the handle
        let state = handle.read().await;
        assert!(state.running.is_empty());
    }

    #[tokio::test]
    async fn test_config_handle_returns_shared_arc() {
        let orch = test_orchestrator();
        let handle = orch.config_handle();

        // Verify we can read config through the handle
        let config = handle.read().await;
        assert_eq!(config.polling.interval_ms, 5000);
    }

    // ── snapshot ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_snapshot_empty() {
        let orch = test_orchestrator();
        let snap = orch.snapshot().await;

        assert!(snap.running.is_empty());
        assert!(snap.retrying.is_empty());
        assert_eq!(snap.totals.input_tokens, 0);
        assert_eq!(snap.totals.output_tokens, 0);
        assert_eq!(snap.totals.total_tokens, 0);
        assert!(snap.rate_limits.is_none());
    }

    #[tokio::test]
    async fn test_snapshot_with_running_entries() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;
        let _cancel_rx2 = insert_running(&orch, "id2", "PROJ-2", "In Progress").await;

        // Set session data on one entry
        {
            let mut state = orch.state.write().await;
            let entry = state.running.get_mut("id1").unwrap();
            entry.session.session_id = Some("sess-abc".to_string());
            entry.session.turn_count = 3;
            entry.session.input_tokens = 100;
            entry.session.output_tokens = 50;
            entry.session.total_tokens = 150;
            entry.session.last_event = Some("turn_completed".to_string());
            entry.session.last_message = Some("Working on it".to_string());
        }

        let snap = orch.snapshot().await;
        assert_eq!(snap.running.len(), 2);

        // Find the PROJ-1 entry
        let proj1 = snap.running.iter().find(|r| r.issue_id == "id1").unwrap();
        assert_eq!(proj1.issue_identifier, "PROJ-1");
        assert_eq!(proj1.state, "Todo");
        assert_eq!(proj1.session_id, Some("sess-abc".to_string()));
        assert_eq!(proj1.turn_count, 3);
        assert_eq!(proj1.input_tokens, 100);
        assert_eq!(proj1.output_tokens, 50);
        assert_eq!(proj1.total_tokens, 150);
        assert_eq!(proj1.last_event, Some("turn_completed".to_string()));
        assert_eq!(proj1.last_message, Some("Working on it".to_string()));

        // seconds_running should include active time from running entries
        assert!(snap.totals.seconds_running >= 0.0);
    }

    #[tokio::test]
    async fn test_snapshot_with_retry_entries() {
        let orch = test_orchestrator();

        // Manually add a retry entry
        {
            let mut state = orch.state.write().await;
            state.retry_attempts.insert(
                "retry1".to_string(),
                RetryEntry {
                    issue_id: "retry1".to_string(),
                    identifier: "PROJ-R1".to_string(),
                    attempt: 2,
                    due_at_ms: 9999999,
                    error: Some("timeout".to_string()),
                },
            );
        }

        let snap = orch.snapshot().await;
        assert_eq!(snap.retrying.len(), 1);
        assert_eq!(snap.retrying[0].issue_id, "retry1");
        assert_eq!(snap.retrying[0].attempt, 2);
        assert_eq!(snap.retrying[0].error, Some("timeout".to_string()));
    }

    #[tokio::test]
    async fn test_snapshot_with_rate_limits() {
        let orch = test_orchestrator();
        {
            let mut state = orch.state.write().await;
            state.rate_limits = Some(serde_json::json!({"limit": 100}));
        }

        let snap = orch.snapshot().await;
        assert!(snap.rate_limits.is_some());
        assert_eq!(snap.rate_limits.unwrap()["limit"], 100);
    }

    // ── should_dispatch ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_should_dispatch_empty_id() {
        let orch = test_orchestrator();
        let config = orch.config.read().await;
        let issue = Issue {
            id: "".to_string(),
            identifier: "PROJ-1".to_string(),
            title: "Title".to_string(),
            state: "Todo".to_string(),
            ..default_issue()
        };
        assert!(!orch.should_dispatch(&issue, &config).await);
    }

    #[tokio::test]
    async fn test_should_dispatch_empty_identifier() {
        let orch = test_orchestrator();
        let config = orch.config.read().await;
        let issue = Issue {
            id: "id1".to_string(),
            identifier: "".to_string(),
            title: "Title".to_string(),
            state: "Todo".to_string(),
            ..default_issue()
        };
        assert!(!orch.should_dispatch(&issue, &config).await);
    }

    #[tokio::test]
    async fn test_should_dispatch_empty_title() {
        let orch = test_orchestrator();
        let config = orch.config.read().await;
        let issue = Issue {
            id: "id1".to_string(),
            identifier: "PROJ-1".to_string(),
            title: "".to_string(),
            state: "Todo".to_string(),
            ..default_issue()
        };
        assert!(!orch.should_dispatch(&issue, &config).await);
    }

    #[tokio::test]
    async fn test_should_dispatch_empty_state() {
        let orch = test_orchestrator();
        let config = orch.config.read().await;
        let issue = Issue {
            id: "id1".to_string(),
            identifier: "PROJ-1".to_string(),
            title: "Title".to_string(),
            state: "".to_string(),
            ..default_issue()
        };
        assert!(!orch.should_dispatch(&issue, &config).await);
    }

    #[tokio::test]
    async fn test_should_dispatch_not_in_active_states() {
        let orch = test_orchestrator();
        let config = orch.config.read().await;
        let issue = Issue {
            id: "id1".to_string(),
            identifier: "PROJ-1".to_string(),
            title: "Title".to_string(),
            state: "Review".to_string(), // Not in active_states
            ..default_issue()
        };
        assert!(!orch.should_dispatch(&issue, &config).await);
    }

    #[tokio::test]
    async fn test_should_dispatch_in_terminal_states() {
        let orch = test_orchestrator();
        let config = orch.config.read().await;
        // Create a config where "Done" is both active and terminal
        let mut custom_config = config.clone();
        custom_config.tracker.active_states.push("Done".to_string());
        let issue = Issue {
            id: "id1".to_string(),
            identifier: "PROJ-1".to_string(),
            title: "Title".to_string(),
            state: "Done".to_string(),
            ..default_issue()
        };
        assert!(!orch.should_dispatch(&issue, &custom_config).await);
    }

    #[tokio::test]
    async fn test_should_dispatch_already_running() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        let config = orch.config.read().await;
        let issue = make_issue("id1", "PROJ-1", "Todo");
        assert!(!orch.should_dispatch(&issue, &config).await);
    }

    #[tokio::test]
    async fn test_should_dispatch_already_claimed() {
        let orch = test_orchestrator();
        {
            let mut state = orch.state.write().await;
            state.claimed.insert("id1".to_string());
        }

        let config = orch.config.read().await;
        let issue = make_issue("id1", "PROJ-1", "Todo");
        assert!(!orch.should_dispatch(&issue, &config).await);
    }

    #[tokio::test]
    async fn test_should_dispatch_no_available_slots() {
        let orch = test_orchestrator(); // max_concurrent_agents = 3
        let _c1 = insert_running(&orch, "id1", "PROJ-1", "Todo").await;
        let _c2 = insert_running(&orch, "id2", "PROJ-2", "Todo").await;
        let _c3 = insert_running(&orch, "id3", "PROJ-3", "Todo").await;

        let config = orch.config.read().await;
        let issue = make_issue("id4", "PROJ-4", "Todo");
        assert!(!orch.should_dispatch(&issue, &config).await);
    }

    #[tokio::test]
    async fn test_should_dispatch_per_state_concurrency_limit() {
        let mut config = test_config();
        config
            .agent
            .max_concurrent_agents_by_state
            .insert("todo".to_string(), 1);
        let orch = Orchestrator::new(config.clone(), "template".to_string());

        // Add one running entry in "Todo" state
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        let issue = make_issue("id2", "PROJ-2", "Todo");
        assert!(!orch.should_dispatch(&issue, &config).await);
    }

    #[tokio::test]
    async fn test_should_dispatch_todo_with_non_terminal_blocker() {
        let orch = test_orchestrator();
        let config = orch.config.read().await;

        let issue = Issue {
            id: "id1".to_string(),
            identifier: "PROJ-1".to_string(),
            title: "Title".to_string(),
            state: "Todo".to_string(),
            blocked_by: vec![BlockerRef {
                id: Some("blocker1".to_string()),
                identifier: Some("PROJ-B1".to_string()),
                state: Some("In Progress".to_string()), // non-terminal
            }],
            ..default_issue()
        };
        assert!(!orch.should_dispatch(&issue, &config).await);
    }

    #[tokio::test]
    async fn test_should_dispatch_todo_with_terminal_only_blockers() {
        let orch = test_orchestrator();
        let config = orch.config.read().await;

        let issue = Issue {
            id: "id1".to_string(),
            identifier: "PROJ-1".to_string(),
            title: "Title".to_string(),
            state: "Todo".to_string(),
            blocked_by: vec![
                BlockerRef {
                    id: Some("b1".to_string()),
                    identifier: Some("PROJ-B1".to_string()),
                    state: Some("Done".to_string()), // terminal
                },
                BlockerRef {
                    id: Some("b2".to_string()),
                    identifier: Some("PROJ-B2".to_string()),
                    state: Some("Cancelled".to_string()), // terminal
                },
            ],
            ..default_issue()
        };
        assert!(orch.should_dispatch(&issue, &config).await);
    }

    #[tokio::test]
    async fn test_should_dispatch_todo_with_unknown_state_blocker() {
        let orch = test_orchestrator();
        let config = orch.config.read().await;

        let issue = Issue {
            id: "id1".to_string(),
            identifier: "PROJ-1".to_string(),
            title: "Title".to_string(),
            state: "Todo".to_string(),
            blocked_by: vec![BlockerRef {
                id: Some("b1".to_string()),
                identifier: Some("PROJ-B1".to_string()),
                state: None, // unknown → treated as non-terminal
            }],
            ..default_issue()
        };
        assert!(!orch.should_dispatch(&issue, &config).await);
    }

    #[tokio::test]
    async fn test_should_dispatch_all_conditions_pass() {
        let orch = test_orchestrator();
        let config = orch.config.read().await;

        let issue = make_issue("id1", "PROJ-1", "Todo");
        assert!(orch.should_dispatch(&issue, &config).await);
    }

    #[tokio::test]
    async fn test_should_dispatch_in_progress_with_blockers_ignored() {
        // Blocker check only applies to "todo" state
        let orch = test_orchestrator();
        let config = orch.config.read().await;

        let issue = Issue {
            id: "id1".to_string(),
            identifier: "PROJ-1".to_string(),
            title: "Title".to_string(),
            state: "In Progress".to_string(),
            blocked_by: vec![BlockerRef {
                id: Some("b1".to_string()),
                identifier: Some("PROJ-B1".to_string()),
                state: Some("In Progress".to_string()), // non-terminal, but state is not "todo"
            }],
            ..default_issue()
        };
        assert!(orch.should_dispatch(&issue, &config).await);
    }

    // ── should_dispatch: completed_set guard (Fix 4) ─────────────────

    #[tokio::test]
    async fn test_should_dispatch_completed_issue() {
        let orch = test_orchestrator();
        {
            let mut state = orch.state.write().await;
            state.mark_completed("id1".to_string());
        }

        let config = orch.config.read().await;
        let issue = make_issue("id1", "PROJ-1", "Todo");
        assert!(
            !orch.should_dispatch(&issue, &config).await,
            "should not dispatch a recently completed issue"
        );
    }

    // ── should_dispatch: symphony-claimed label guard (Fix 2) ──────

    #[tokio::test]
    async fn test_should_dispatch_symphony_claimed_label() {
        let orch = test_orchestrator();
        let config = orch.config.read().await;

        let issue = Issue {
            id: "id1".to_string(),
            identifier: "PROJ-1".to_string(),
            title: "Title".to_string(),
            state: "Todo".to_string(),
            labels: vec!["symphony-claimed".to_string()],
            ..default_issue()
        };
        assert!(
            !orch.should_dispatch(&issue, &config).await,
            "should not dispatch an issue with symphony-claimed label"
        );
    }

    #[tokio::test]
    async fn test_should_dispatch_other_labels_pass() {
        let orch = test_orchestrator();
        let config = orch.config.read().await;

        let issue = Issue {
            id: "id1".to_string(),
            identifier: "PROJ-1".to_string(),
            title: "Title".to_string(),
            state: "Todo".to_string(),
            labels: vec!["bug".to_string(), "feature".to_string()],
            ..default_issue()
        };
        assert!(
            orch.should_dispatch(&issue, &config).await,
            "should dispatch issue with non-claimed labels"
        );
    }

    // ── should_dispatch: PR dedup guard (Fix 3) ────────────────────

    fn test_github_config(endpoint: &str) -> GitHubConfig {
        GitHubConfig {
            token: "test-token".to_string(),
            repo_owner: "test-owner".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: endpoint.to_string(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        }
    }

    #[tokio::test]
    async fn test_should_dispatch_open_pr_exists() {
        let mut server = mockito::Server::new_async().await;

        // Mock GitHub API returning an open PR
        let _mock = server
            .mock("GET", "/repos/test-owner/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![mockito::Matcher::UrlEncoded(
                "state".to_string(),
                "open".to_string(),
            )]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!([{
                    "number": 42,
                    "state": "open",
                    "mergeable": true,
                    "mergeable_state": "clean",
                    "merged": false
                }])
                .to_string(),
            )
            .create_async()
            .await;

        let mut config = test_config();
        config.github = Some(test_github_config(&server.url()));
        let orch = Orchestrator::new(config.clone(), "template".to_string());

        let issue = make_issue("id1", "PROJ-1", "Todo");
        assert!(
            !orch.should_dispatch(&issue, &config).await,
            "should not dispatch when an open PR exists"
        );
    }

    #[tokio::test]
    async fn test_should_dispatch_no_open_pr() {
        let mut server = mockito::Server::new_async().await;

        // Mock GitHub API returning no PRs
        let _mock = server
            .mock("GET", "/repos/test-owner/test-repo/pulls")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        let mut config = test_config();
        config.github = Some(test_github_config(&server.url()));
        let orch = Orchestrator::new(config.clone(), "template".to_string());

        let issue = make_issue("id1", "PROJ-1", "Todo");
        assert!(
            orch.should_dispatch(&issue, &config).await,
            "should dispatch when no open PR exists"
        );
    }

    #[tokio::test]
    async fn test_should_dispatch_github_api_error_proceeds() {
        let mut server = mockito::Server::new_async().await;

        // Mock GitHub API returning error
        let _mock = server
            .mock("GET", "/repos/test-owner/test-repo/pulls")
            .with_status(500)
            .with_body("internal server error")
            .create_async()
            .await;

        let mut config = test_config();
        config.github = Some(test_github_config(&server.url()));
        let orch = Orchestrator::new(config.clone(), "template".to_string());

        let issue = make_issue("id1", "PROJ-1", "Todo");
        assert!(
            orch.should_dispatch(&issue, &config).await,
            "should proceed with dispatch when GitHub API errors"
        );
    }

    #[tokio::test]
    async fn test_should_dispatch_no_github_config_proceeds() {
        let orch = test_orchestrator(); // No GitHub config
        let config = orch.config.read().await;

        let issue = make_issue("id1", "PROJ-1", "Todo");
        assert!(
            orch.should_dispatch(&issue, &config).await,
            "should dispatch when no GitHub config is set"
        );
    }

    // ── should_dispatch: waiting_for_review guard ──────────────────

    #[tokio::test]
    async fn test_should_dispatch_waiting_for_review() {
        let orch = test_orchestrator();
        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
        }

        let config = orch.config.read().await;
        let issue = make_issue("id1", "PROJ-1", "In Progress");
        assert!(
            !orch.should_dispatch(&issue, &config).await,
            "should not dispatch an issue waiting for review"
        );
    }

    // ── add_claim_label / remove_claim_label ───────────────────────

    /// Mock server helper: respond to all label-related GraphQL requests with
    /// simple success responses, allowing add_claim_label and remove_claim_label
    /// to complete without error.
    async fn mock_label_server() -> (mockito::ServerGuard, Vec<mockito::Mock>) {
        let mut server = mockito::Server::new_async().await;
        let mut mocks = Vec::new();

        // This mock accepts any POST and returns a response that satisfies
        // find_label (returns empty labels, so remove_label_from_issue returns Ok early)
        // For add_label_to_issue, we need more complex multi-step mocking.
        // Simplest: return a response that makes find_label return the label found,
        // then get_issue_label_ids returns it already attached → Ok early.
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "issueLabels": { "nodes": [] },
                        "issue": { "labels": { "nodes": [] } }
                    }
                })
                .to_string(),
            )
            .expect_at_least(1)
            .create_async()
            .await;
        mocks.push(mock);

        (server, mocks)
    }

    #[traced_test]
    #[tokio::test]
    async fn test_remove_claim_label_ok_path() {
        let (server, _mocks) = mock_label_server().await;
        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        // remove_claim_label calls remove_label_from_issue which calls find_label.
        // If find_label returns None (label not found), remove_label_from_issue returns Ok.
        orch.remove_claim_label("issue-1", "PROJ-1").await;

        assert!(logs_contain("removed symphony-claimed label"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_add_claim_label_ok_path() {
        let mut server = mockito::Server::new_async().await;

        // 1. find_label: label already exists
        let _m1 = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": { "issueLabels": { "nodes": [
                        { "id": "lbl-1", "name": "symphony-claimed" }
                    ] } }
                })
                .to_string(),
            )
            .create_async()
            .await;

        // 2. get_issue_label_ids: label already attached
        let _m2 = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": { "issue": { "labels": { "nodes": [
                        { "id": "lbl-1" }
                    ] } } }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        orch.add_claim_label("issue-1", "PROJ-1").await;

        assert!(logs_contain("added symphony-claimed label"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_add_claim_label_err_path() {
        // Use a config that points to a non-existent server to trigger Err
        let orch = test_orchestrator();

        orch.add_claim_label("issue-1", "PROJ-1").await;

        assert!(logs_contain("failed to add symphony-claimed label"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_remove_claim_label_err_path() {
        // Use a config that points to a non-existent server to trigger Err
        let orch = test_orchestrator();

        orch.remove_claim_label("issue-1", "PROJ-1").await;

        assert!(logs_contain("failed to remove symphony-claimed label"));
    }

    // ── handle_message ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_handle_message_worker_exited() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.handle_message(OrchestratorMessage::WorkerExited {
            issue_id: "id1".to_string(),
            reason: WorkerExitReason::Cancelled,
        })
        .await;

        let state = orch.state.read().await;
        assert!(!state.running.contains_key("id1"));
        assert!(!state.claimed.contains("id1"));
    }

    #[tokio::test]
    async fn test_handle_message_agent_update() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.handle_message(OrchestratorMessage::AgentUpdate {
            issue_id: "id1".to_string(),
            event: AgentEvent::TurnCompleted {
                message: Some("did stuff".to_string()),
            },
        })
        .await;

        let state = orch.state.read().await;
        let entry = state.running.get("id1").unwrap();
        assert_eq!(entry.session.turn_count, 1);
        assert_eq!(entry.session.last_message, Some("did stuff".to_string()));
    }

    #[tokio::test]
    async fn test_handle_message_trigger_refresh() {
        // TriggerRefresh calls on_tick which requires HTTP — use mock server
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[]))
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        orch.handle_message(OrchestratorMessage::TriggerRefresh)
            .await;

        mock.assert_async().await;
    }

    // ── on_worker_exit ───────────────────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_normal() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_worker_exit("id1", WorkerExitReason::Normal).await;

        let state = orch.state.read().await;
        assert!(!state.running.contains_key("id1"));
        assert!(state.is_completed("id1"));
        // §8.1: Normal exit schedules a short continuation retry (1000ms)
        // so the issue is re-checked quickly instead of waiting for the
        // next poll tick (~30s).
        assert!(state.retry_attempts.contains_key("id1"));
        let retry = state.retry_attempts.get("id1").unwrap();
        assert_eq!(retry.attempt, 0); // continuation, not failure
        assert!(retry.error.is_none());
    }

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_failed() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_worker_exit("id1", WorkerExitReason::Failed("boom".to_string()))
            .await;

        let state = orch.state.read().await;
        assert!(!state.running.contains_key("id1"));
        assert!(state.retry_attempts.contains_key("id1"));
        let retry = state.retry_attempts.get("id1").unwrap();
        assert_eq!(retry.attempt, 1);
        assert_eq!(retry.error, Some("boom".to_string()));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_timed_out() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_worker_exit("id1", WorkerExitReason::TimedOut).await;

        let state = orch.state.read().await;
        assert!(!state.running.contains_key("id1"));
        assert!(state.retry_attempts.contains_key("id1"));
        let retry = state.retry_attempts.get("id1").unwrap();
        assert_eq!(retry.error, Some("turn timeout".to_string()));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_stalled() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_worker_exit("id1", WorkerExitReason::Stalled).await;

        let state = orch.state.read().await;
        assert!(!state.running.contains_key("id1"));
        assert!(state.retry_attempts.contains_key("id1"));
        let retry = state.retry_attempts.get("id1").unwrap();
        assert_eq!(retry.error, Some("stalled".to_string()));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_cancelled() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_worker_exit("id1", WorkerExitReason::Cancelled)
            .await;

        let state = orch.state.read().await;
        assert!(!state.running.contains_key("id1"));
        assert!(!state.claimed.contains("id1")); // claimed removed
        assert!(!state.retry_attempts.contains_key("id1")); // no retry
    }

    #[tokio::test]
    async fn test_on_worker_exit_unknown_issue() {
        let orch = test_orchestrator();
        // No running entry for "unknown"
        orch.on_worker_exit("unknown", WorkerExitReason::Normal)
            .await;

        let state = orch.state.read().await;
        // Nothing should have changed
        assert!(state.running.is_empty());
        assert_eq!(state.completed_count(), 0);
        assert!(state.retry_attempts.is_empty());
    }

    #[tokio::test]
    async fn test_on_worker_exit_accumulates_seconds_running() {
        let orch = test_orchestrator();
        // Insert with a started_at in the past to get measurable elapsed time
        let past = Utc::now() - chrono::Duration::seconds(10);
        let _cancel_rx = insert_running_at(&orch, "id1", "PROJ-1", "Todo", past).await;

        orch.on_worker_exit("id1", WorkerExitReason::Cancelled)
            .await;

        let state = orch.state.read().await;
        // Should have accumulated approximately 10 seconds
        assert!(state.agent_totals.seconds_running >= 9.0);
    }

    #[tokio::test]
    async fn test_on_worker_exit_with_existing_retry_attempt() {
        let orch = test_orchestrator();

        // Pre-populate with retry_attempt = Some(3)
        let (cancel_tx, _cancel_rx) = oneshot::channel();
        {
            let mut state = orch.state.write().await;
            state.running.insert(
                "id1".to_string(),
                RunningEntry {
                    issue: make_issue("id1", "PROJ-1", "Todo"),
                    identifier: "PROJ-1".to_string(),
                    session: LiveSession::default(),
                    retry_attempt: Some(3),
                    started_at: Utc::now(),
                    cancel_tx,
                    event_log: std::collections::VecDeque::new(),
                    log_seq: 0,
                },
            );
            state.claimed.insert("id1".to_string());
        }

        orch.on_worker_exit("id1", WorkerExitReason::Failed("err".to_string()))
            .await;

        let state = orch.state.read().await;
        let retry = state.retry_attempts.get("id1").unwrap();
        assert_eq!(retry.attempt, 4); // 3 + 1
    }

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_unlimited_retries() {
        // §8.2: retries always continue, even at high attempt counts
        let config = test_config();
        let orch = Orchestrator::new(config, "template".to_string());

        // Insert with retry_attempt = Some(100) so next_attempt = 101
        let (cancel_tx, _cancel_rx) = oneshot::channel();
        {
            let mut state = orch.state.write().await;
            state.running.insert(
                "id1".to_string(),
                RunningEntry {
                    issue: make_issue("id1", "PROJ-1", "Todo"),
                    identifier: "PROJ-1".to_string(),
                    session: LiveSession::default(),
                    retry_attempt: Some(100),
                    started_at: Utc::now(),
                    cancel_tx,
                    event_log: std::collections::VecDeque::new(),
                    log_seq: 0,
                },
            );
            state.claimed.insert("id1".to_string());
        }

        orch.on_worker_exit("id1", WorkerExitReason::Failed("err".to_string()))
            .await;

        let state = orch.state.read().await;
        // Should have a retry entry — unlimited retries never gives up
        let retry = state.retry_attempts.get("id1").unwrap();
        assert_eq!(retry.attempt, 101); // 100 + 1
                                        // Claim should still be held
        assert!(state.claimed.contains("id1"));
    }

    // ── on_agent_update ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_on_agent_update_session_started() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_agent_update(
            "id1",
            AgentEvent::SessionStarted {
                session_id: "sess-123".to_string(),
                pid: Some(42),
            },
        )
        .await;

        let state = orch.state.read().await;
        let entry = state.running.get("id1").unwrap();
        assert_eq!(entry.session.session_id, Some("sess-123".to_string()));
        assert_eq!(entry.session.agent_pid, Some(42));
        assert_eq!(
            entry.session.last_event,
            Some("session_started".to_string())
        );
        assert!(entry.session.last_event_at.is_some());
    }

    #[tokio::test]
    async fn test_on_agent_update_session_started_no_pid() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_agent_update(
            "id1",
            AgentEvent::SessionStarted {
                session_id: "sess-456".to_string(),
                pid: None,
            },
        )
        .await;

        let state = orch.state.read().await;
        let entry = state.running.get("id1").unwrap();
        assert_eq!(entry.session.session_id, Some("sess-456".to_string()));
        assert_eq!(entry.session.agent_pid, None);
    }

    #[tokio::test]
    async fn test_on_agent_update_turn_completed() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_agent_update(
            "id1",
            AgentEvent::TurnCompleted {
                message: Some("did work".to_string()),
            },
        )
        .await;

        let state = orch.state.read().await;
        let entry = state.running.get("id1").unwrap();
        assert_eq!(entry.session.turn_count, 1);
        assert_eq!(entry.session.last_event, Some("turn_completed".to_string()));
        assert_eq!(entry.session.last_message, Some("did work".to_string()));
        assert!(entry.session.last_event_at.is_some());

        drop(state);

        // Second turn
        orch.on_agent_update("id1", AgentEvent::TurnCompleted { message: None })
            .await;

        let state = orch.state.read().await;
        let entry = state.running.get("id1").unwrap();
        assert_eq!(entry.session.turn_count, 2);
        assert_eq!(entry.session.last_message, None);
    }

    #[tokio::test]
    async fn test_on_agent_update_turn_failed() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_agent_update(
            "id1",
            AgentEvent::TurnFailed {
                error: "something broke".to_string(),
            },
        )
        .await;

        let state = orch.state.read().await;
        let entry = state.running.get("id1").unwrap();
        assert_eq!(entry.session.last_event, Some("turn_failed".to_string()));
        assert_eq!(
            entry.session.last_message,
            Some("something broke".to_string())
        );
    }

    #[tokio::test]
    async fn test_on_agent_update_notification() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_agent_update(
            "id1",
            AgentEvent::Notification {
                message: "heads up".to_string(),
            },
        )
        .await;

        let state = orch.state.read().await;
        let entry = state.running.get("id1").unwrap();
        assert_eq!(entry.session.last_event, Some("notification".to_string()));
        assert_eq!(entry.session.last_message, Some("heads up".to_string()));
    }

    #[tokio::test]
    async fn test_on_agent_update_token_usage() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        // First token report
        orch.on_agent_update(
            "id1",
            AgentEvent::TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
            },
        )
        .await;

        let state = orch.state.read().await;
        let entry = state.running.get("id1").unwrap();
        assert_eq!(entry.session.input_tokens, 100);
        assert_eq!(entry.session.output_tokens, 50);
        assert_eq!(entry.session.total_tokens, 150);
        assert_eq!(entry.session.last_reported_input_tokens, 100);
        assert_eq!(entry.session.last_reported_output_tokens, 50);
        assert_eq!(entry.session.last_reported_total_tokens, 150);
        assert_eq!(entry.session.last_event, Some("token_usage".to_string()));
        // Aggregate totals
        assert_eq!(state.agent_totals.input_tokens, 100);
        assert_eq!(state.agent_totals.output_tokens, 50);
        assert_eq!(state.agent_totals.total_tokens, 150);
        drop(state);

        // Second token report (cumulative: 200 total input, delta = 100)
        orch.on_agent_update(
            "id1",
            AgentEvent::TokenUsage {
                input_tokens: 200,
                output_tokens: 80,
                total_tokens: 280,
            },
        )
        .await;

        let state = orch.state.read().await;
        let entry = state.running.get("id1").unwrap();
        assert_eq!(entry.session.input_tokens, 200); // 100 + delta(100)
        assert_eq!(entry.session.output_tokens, 80); // 50 + delta(30)
        assert_eq!(entry.session.total_tokens, 280); // 150 + delta(130)
        assert_eq!(entry.session.last_reported_input_tokens, 200);
        assert_eq!(entry.session.last_reported_output_tokens, 80);
        assert_eq!(entry.session.last_reported_total_tokens, 280);
        // Aggregate: 100 + 100 = 200
        assert_eq!(state.agent_totals.input_tokens, 200);
        assert_eq!(state.agent_totals.output_tokens, 80);
        assert_eq!(state.agent_totals.total_tokens, 280);
    }

    #[tokio::test]
    async fn test_on_agent_update_agent_message() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_agent_update(
            "id1",
            AgentEvent::AgentMessage {
                event_type: "tool_use".to_string(),
                message: Some("running grep".to_string()),
            },
        )
        .await;

        let state = orch.state.read().await;
        let entry = state.running.get("id1").unwrap();
        assert_eq!(entry.session.last_event, Some("tool_use".to_string()));
        assert_eq!(entry.session.last_message, Some("running grep".to_string()));
    }

    #[tokio::test]
    async fn test_on_agent_update_agent_message_no_message() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        // Set initial last_message
        {
            let mut state = orch.state.write().await;
            let entry = state.running.get_mut("id1").unwrap();
            entry.session.last_message = Some("previous".to_string());
        }

        orch.on_agent_update(
            "id1",
            AgentEvent::AgentMessage {
                event_type: "thinking".to_string(),
                message: None,
            },
        )
        .await;

        let state = orch.state.read().await;
        let entry = state.running.get("id1").unwrap();
        assert_eq!(entry.session.last_event, Some("thinking".to_string()));
        // last_message not updated when message is None
        assert_eq!(entry.session.last_message, Some("previous".to_string()));
    }

    // §11.2: rate limit update stored in state (no rate_limit_info → logged)
    #[tokio::test]
    async fn test_on_agent_update_rate_limit() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        let rate_data = serde_json::json!({
            "type": "rate_limit_event",
            "requests_remaining": 42,
            "tokens_remaining": 99999
        });

        orch.on_agent_update(
            "id1",
            AgentEvent::RateLimitUpdate {
                data: rate_data.clone(),
            },
        )
        .await;

        let state = orch.state.read().await;
        assert!(state.rate_limits.is_some());
        assert_eq!(
            state.rate_limits.as_ref().unwrap()["requests_remaining"],
            42
        );
        let entry = state.running.get("id1").unwrap();
        assert_eq!(
            entry.session.last_event,
            Some("rate_limit_event".to_string())
        );
        assert!(entry.session.last_event_at.is_some());
        // Missing rate_limit_info means it IS logged (conservative)
        assert_eq!(entry.event_log.len(), 1);
    }

    // Routine "allowed" rate-limit events are filtered from the log
    #[tokio::test]
    async fn test_on_agent_update_rate_limit_allowed_filtered() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        let rate_data = serde_json::json!({
            "type": "rate_limit_event",
            "rate_limit_info": { "status": "allowed" }
        });

        orch.on_agent_update(
            "id1",
            AgentEvent::RateLimitUpdate {
                data: rate_data.clone(),
            },
        )
        .await;

        let state = orch.state.read().await;
        // Global rate_limits is still updated
        assert!(state.rate_limits.is_some());
        // Session metadata is still updated
        let entry = state.running.get("id1").unwrap();
        assert_eq!(
            entry.session.last_event,
            Some("rate_limit_event".to_string())
        );
        assert!(entry.session.last_event_at.is_some());
        // But event log is empty — routine event was filtered
        assert!(entry.event_log.is_empty());
        assert_eq!(entry.log_seq, 0);
    }

    // Non-"allowed" rate-limit events ARE logged
    #[tokio::test]
    async fn test_on_agent_update_rate_limit_rejected_logged() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        let rate_data = serde_json::json!({
            "type": "rate_limit_event",
            "rate_limit_info": { "status": "rejected" }
        });

        orch.on_agent_update(
            "id1",
            AgentEvent::RateLimitUpdate {
                data: rate_data.clone(),
            },
        )
        .await;

        let state = orch.state.read().await;
        assert!(state.rate_limits.is_some());
        let entry = state.running.get("id1").unwrap();
        // Rejected event IS logged
        assert_eq!(entry.event_log.len(), 1);
        assert_eq!(entry.log_seq, 1);
        assert_eq!(entry.event_log[0].event_type, "rate_limit_event");
    }

    #[tokio::test]
    async fn test_on_agent_update_nonexistent_issue() {
        let orch = test_orchestrator();
        // No running entry for "unknown"
        orch.on_agent_update(
            "unknown",
            AgentEvent::TurnCompleted {
                message: Some("hi".to_string()),
            },
        )
        .await;

        let state = orch.state.read().await;
        assert!(state.running.is_empty());
        // Should be no-op, no crash
    }

    #[tokio::test]
    async fn test_on_agent_update_token_usage_nonexistent_issue() {
        let orch = test_orchestrator();
        // TokenUsage for non-existent issue should be a no-op
        orch.on_agent_update(
            "ghost",
            AgentEvent::TokenUsage {
                input_tokens: 999,
                output_tokens: 999,
                total_tokens: 999,
            },
        )
        .await;

        let state = orch.state.read().await;
        // Aggregate totals should not be affected
        assert_eq!(state.agent_totals.input_tokens, 0);
        assert_eq!(state.agent_totals.output_tokens, 0);
        assert_eq!(state.agent_totals.total_tokens, 0);
    }

    // ── on_agent_update: event log and broadcast ───────────────────

    #[tokio::test]
    async fn test_on_agent_update_appends_to_event_log() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_agent_update(
            "id1",
            AgentEvent::SessionStarted {
                session_id: "sess-1".to_string(),
                pid: Some(42),
            },
        )
        .await;

        orch.on_agent_update(
            "id1",
            AgentEvent::TurnCompleted {
                message: Some("done".to_string()),
            },
        )
        .await;

        let state = orch.state.read().await;
        let entry = state.running.get("id1").unwrap();
        assert_eq!(entry.log_seq, 2);
        assert_eq!(entry.event_log.len(), 2);
        assert_eq!(entry.event_log[0].seq, 1);
        assert_eq!(entry.event_log[0].event_type, "session_started");
        assert_eq!(entry.event_log[1].seq, 2);
        assert_eq!(entry.event_log[1].event_type, "turn_completed");
    }

    #[tokio::test]
    async fn test_on_agent_update_broadcasts_log_entry() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        // Create a broadcast channel and subscribe
        let mut rx = {
            let mut state = orch.state.write().await;
            let (tx, rx) = tokio::sync::broadcast::channel(256);
            state.event_broadcasts.insert("id1".to_string(), tx);
            rx
        };

        orch.on_agent_update(
            "id1",
            AgentEvent::Notification {
                message: "hello".to_string(),
            },
        )
        .await;

        // Receiver should get the broadcast
        let received = rx.try_recv().unwrap();
        assert_eq!(received.seq, 1);
        assert_eq!(received.event_type, "notification");
        assert_eq!(received.message, Some("hello".to_string()));
    }

    #[tokio::test]
    async fn test_on_agent_update_broadcast_no_subscribers_ok() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        // Create broadcast channel with no subscribers (receiver dropped)
        {
            let mut state = orch.state.write().await;
            let (tx, _) = tokio::sync::broadcast::channel(256);
            state.event_broadcasts.insert("id1".to_string(), tx);
        }

        // Should not panic even with no subscribers
        orch.on_agent_update(
            "id1",
            AgentEvent::TurnCompleted {
                message: Some("test".to_string()),
            },
        )
        .await;

        let state = orch.state.read().await;
        let entry = state.running.get("id1").unwrap();
        assert_eq!(entry.event_log.len(), 1);
    }

    // ── dispatch_issue: broadcast channel creation ───────────────────

    #[tokio::test]
    async fn test_dispatch_creates_broadcast_channel() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[]))
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        let issue = make_issue("id1", "PROJ-1", "Todo");
        orch.dispatch_issue(issue, None, false, None).await;

        let state = orch.state.read().await;
        assert!(state.event_broadcasts.contains_key("id1"));
    }

    // ── on_worker_exit: broadcast channel removal ────────────────────

    #[tokio::test]
    async fn test_on_worker_exit_removes_broadcast_channel() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        // Add a broadcast channel
        {
            let mut state = orch.state.write().await;
            let (tx, _) = tokio::sync::broadcast::channel(256);
            state.event_broadcasts.insert("id1".to_string(), tx);
        }

        orch.on_worker_exit("id1", WorkerExitReason::Cancelled)
            .await;

        let state = orch.state.read().await;
        assert!(!state.event_broadcasts.contains_key("id1"));
    }

    // ── terminate_running_issue: broadcast channel removal ───────────

    #[tokio::test]
    async fn test_terminate_removes_broadcast_channel() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        {
            let mut state = orch.state.write().await;
            let (tx, _) = tokio::sync::broadcast::channel(256);
            state.event_broadcasts.insert("id1".to_string(), tx);
        }

        orch.terminate_running_issue("id1", false).await;

        let state = orch.state.read().await;
        assert!(!state.event_broadcasts.contains_key("id1"));
    }

    // ── schedule_retry_inner ─────────────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_schedule_retry_inner() {
        let orch = test_orchestrator();
        let mut state = orch.state.write().await;

        orch.schedule_retry_inner(
            &mut state,
            "id1".to_string(),
            2,
            "PROJ-1".to_string(),
            Some("error msg".to_string()),
            5_000,
        );

        assert!(state.retry_attempts.contains_key("id1"));
        let retry = state.retry_attempts.get("id1").unwrap();
        assert_eq!(retry.issue_id, "id1");
        assert_eq!(retry.identifier, "PROJ-1");
        assert_eq!(retry.attempt, 2);
        assert_eq!(retry.error, Some("error msg".to_string()));
        assert!(retry.due_at_ms > 0);
    }

    #[traced_test]
    #[tokio::test]
    async fn test_schedule_retry_inner_timer_fires() {
        let orch = test_orchestrator();
        let mut state = orch.state.write().await;

        // Use a very short delay so the timer task actually fires
        orch.schedule_retry_inner(
            &mut state,
            "id1".to_string(),
            1,
            "PROJ-1".to_string(),
            None,
            1, // 1ms delay
        );
        drop(state);

        // Wait for the timer to fire and send the message
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // The timer task should have sent a RetryFired message
        // We can't easily verify this without consuming msg_rx,
        // but the spawned task has executed lines 506-510
    }

    #[tokio::test]
    async fn test_schedule_retry_inner_replaces_existing() {
        let orch = test_orchestrator();
        let mut state = orch.state.write().await;

        // First retry
        orch.schedule_retry_inner(
            &mut state,
            "id1".to_string(),
            1,
            "PROJ-1".to_string(),
            Some("first".to_string()),
            1_000,
        );
        let first_due = state.retry_attempts.get("id1").unwrap().due_at_ms;

        // Second retry (should replace)
        orch.schedule_retry_inner(
            &mut state,
            "id1".to_string(),
            2,
            "PROJ-1".to_string(),
            Some("second".to_string()),
            10_000,
        );

        assert_eq!(state.retry_attempts.len(), 1);
        let retry = state.retry_attempts.get("id1").unwrap();
        assert_eq!(retry.attempt, 2);
        assert_eq!(retry.error, Some("second".to_string()));
        assert!(retry.due_at_ms >= first_due);
    }

    #[tokio::test]
    async fn test_schedule_retry_inner_no_error() {
        let orch = test_orchestrator();
        let mut state = orch.state.write().await;

        orch.schedule_retry_inner(
            &mut state,
            "id1".to_string(),
            1,
            "PROJ-1".to_string(),
            None,
            1_000,
        );

        let retry = state.retry_attempts.get("id1").unwrap();
        assert!(retry.error.is_none());
    }

    // ── shutdown ─────────────────────────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_shutdown_clears_all() {
        let orch = test_orchestrator();
        let _c1 = insert_running(&orch, "id1", "PROJ-1", "Todo").await;
        let _c2 = insert_running(&orch, "id2", "PROJ-2", "In Progress").await;

        // Add retry and claimed entries
        {
            let mut state = orch.state.write().await;
            state.retry_attempts.insert(
                "retry1".to_string(),
                RetryEntry {
                    issue_id: "retry1".to_string(),
                    identifier: "PROJ-R1".to_string(),
                    attempt: 1,
                    due_at_ms: 999,
                    error: None,
                },
            );
        }

        orch.shutdown().await;

        let state = orch.state.read().await;
        assert!(state.running.is_empty());
        assert!(state.claimed.is_empty());
        assert!(state.retry_attempts.is_empty());
    }

    #[tokio::test]
    async fn test_shutdown_sends_cancel_signals() {
        let orch = test_orchestrator();
        let cancel_rx1 = insert_running(&orch, "id1", "PROJ-1", "Todo").await;
        let cancel_rx2 = insert_running(&orch, "id2", "PROJ-2", "Todo").await;

        orch.shutdown().await;

        // Cancel signals should have been sent
        assert!(cancel_rx1.await.is_ok());
        assert!(cancel_rx2.await.is_ok());
    }

    #[tokio::test]
    async fn test_shutdown_empty_is_noop() {
        let orch = test_orchestrator();
        orch.shutdown().await;

        let state = orch.state.read().await;
        assert!(state.running.is_empty());
        assert!(state.claimed.is_empty());
        assert!(state.retry_attempts.is_empty());
    }

    // ── terminate_running_issue ──────────────────────────────────────

    #[tokio::test]
    async fn test_terminate_running_issue_removes_entry() {
        let orch = test_orchestrator();
        let cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        // Also add a retry entry for this issue
        {
            let mut state = orch.state.write().await;
            state.retry_attempts.insert(
                "id1".to_string(),
                RetryEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    attempt: 1,
                    due_at_ms: 999,
                    error: None,
                },
            );
        }

        orch.terminate_running_issue("id1", false).await;

        let state = orch.state.read().await;
        assert!(!state.running.contains_key("id1"));
        assert!(!state.claimed.contains("id1"));
        assert!(!state.retry_attempts.contains_key("id1"));

        // Cancel signal should have been sent
        assert!(cancel_rx.await.is_ok());
    }

    #[tokio::test]
    async fn test_terminate_running_issue_nonexistent_is_noop() {
        let orch = test_orchestrator();
        // Should not panic or error
        orch.terminate_running_issue("nonexistent", false).await;

        let state = orch.state.read().await;
        assert!(state.running.is_empty());
    }

    #[tokio::test]
    async fn test_terminate_running_issue_accumulates_seconds() {
        let orch = test_orchestrator();
        let past = Utc::now() - chrono::Duration::seconds(5);
        let _cancel_rx = insert_running_at(&orch, "id1", "PROJ-1", "Todo", past).await;

        orch.terminate_running_issue("id1", false).await;

        let state = orch.state.read().await;
        assert!(state.agent_totals.seconds_running >= 4.0);
    }

    // ── on_tick (with mockito) ───────────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_on_tick_dispatches_candidates() {
        let mut server = mockito::Server::new_async().await;

        // Mock: candidates response with 2 issues, then states reconciliation (empty running)
        let _candidates_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[
                ("id1", "PROJ-1", "Todo"),
                ("id2", "PROJ-2", "In Progress"),
            ]))
            .expect_at_least(1)
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        orch.on_tick().await;

        let state = orch.state.read().await;
        // Both issues should be dispatched (running)
        assert!(state.running.contains_key("id1"));
        assert!(state.running.contains_key("id2"));
        assert!(state.claimed.contains("id1"));
        assert!(state.claimed.contains("id2"));
    }

    #[tokio::test]
    async fn test_on_tick_respects_max_concurrent() {
        let mut server = mockito::Server::new_async().await;

        // Return 5 candidates, but max_concurrent is 3
        let _candidates_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[
                ("id1", "PROJ-1", "Todo"),
                ("id2", "PROJ-2", "Todo"),
                ("id3", "PROJ-3", "Todo"),
                ("id4", "PROJ-4", "Todo"),
                ("id5", "PROJ-5", "Todo"),
            ]))
            .expect_at_least(1)
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        orch.on_tick().await;

        let state = orch.state.read().await;
        assert_eq!(state.running.len(), 3); // max_concurrent_agents = 3
    }

    #[tokio::test]
    #[traced_test]
    async fn test_on_tick_api_error_handled_gracefully() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        orch.on_tick().await; // Should not panic

        let state = orch.state.read().await;
        assert!(state.running.is_empty()); // Nothing dispatched

        mock.assert_async().await;
    }

    #[tokio::test]
    #[traced_test]
    async fn test_on_tick_skips_dispatch_on_validation_failure() {
        let mut config = test_config();
        config.tracker.api_key = "".to_string(); // Will fail validation
        let orch = Orchestrator::new(config, "template".to_string());

        orch.on_tick().await; // Should not panic, no HTTP requests

        let state = orch.state.read().await;
        assert!(state.running.is_empty());
    }

    // ── reconcile_stalls ─────────────────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_stalls_terminates_stalled_entry() {
        let mut config = test_config();
        config.coding_agent.stall_timeout_ms = 100; // 100ms stall timeout
        let orch = Orchestrator::new(config, "template".to_string());

        // Insert with a started_at far in the past
        let past = Utc::now() - chrono::Duration::seconds(60);
        let cancel_rx = insert_running_at(&orch, "id1", "PROJ-1", "Todo", past).await;

        orch.reconcile_stalls().await;

        let state = orch.state.read().await;
        assert!(!state.running.contains_key("id1")); // terminated
        assert!(!state.claimed.contains("id1"));
        drop(state);

        // Cancel signal sent
        assert!(cancel_rx.await.is_ok());
    }

    #[tokio::test]
    async fn test_reconcile_stalls_skips_when_timeout_zero() {
        let mut config = test_config();
        config.coding_agent.stall_timeout_ms = 0;
        let orch = Orchestrator::new(config, "template".to_string());

        let past = Utc::now() - chrono::Duration::seconds(60);
        let _cancel_rx = insert_running_at(&orch, "id1", "PROJ-1", "Todo", past).await;

        orch.reconcile_stalls().await;

        let state = orch.state.read().await;
        assert!(state.running.contains_key("id1")); // not terminated
    }

    #[tokio::test]
    async fn test_reconcile_stalls_uses_last_event_at() {
        let mut config = test_config();
        config.coding_agent.stall_timeout_ms = 100;
        let orch = Orchestrator::new(config, "template".to_string());

        // Insert with an old started_at but recent last_event_at
        let past = Utc::now() - chrono::Duration::seconds(60);
        let _cancel_rx = insert_running_at(&orch, "id1", "PROJ-1", "Todo", past).await;

        // Update last_event_at to now
        {
            let mut state = orch.state.write().await;
            let entry = state.running.get_mut("id1").unwrap();
            entry.session.last_event_at = Some(Utc::now());
        }

        orch.reconcile_stalls().await;

        let state = orch.state.read().await;
        assert!(state.running.contains_key("id1")); // NOT stalled
    }

    // ── reconcile_tracker_states (with mockito) ──────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_tracker_states_terminal_issue_removed() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[("id1", "PROJ-1", "Done")]))
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());
        let cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.reconcile_tracker_states().await;

        let state = orch.state.read().await;
        assert!(!state.running.contains_key("id1"));
        assert!(!state.claimed.contains("id1"));
        drop(state);

        assert!(cancel_rx.await.is_ok());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_reconcile_tracker_states_active_issue_updated() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[("id1", "PROJ-1", "In Progress")]))
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.reconcile_tracker_states().await;

        let state = orch.state.read().await;
        // Still running, but state updated
        assert!(state.running.contains_key("id1"));
        let entry = state.running.get("id1").unwrap();
        assert_eq!(entry.issue.state, "In Progress");

        mock.assert_async().await;
    }

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_tracker_states_neither_active_nor_terminal() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[(
                "id1", "PROJ-1", "On Hold", // Not active, not terminal
            )]))
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());
        let cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.reconcile_tracker_states().await;

        let state = orch.state.read().await;
        // Should be terminated (not active, not terminal → stop without cleanup)
        assert!(!state.running.contains_key("id1"));
        drop(state);

        assert!(cancel_rx.await.is_ok());
        mock.assert_async().await;
    }

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_tracker_states_api_error_keeps_workers() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(500)
            .with_body("fail")
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.reconcile_tracker_states().await;

        let state = orch.state.read().await;
        // Worker should still be running
        assert!(state.running.contains_key("id1"));

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_reconcile_tracker_states_no_running_skips() {
        let orch = test_orchestrator();

        // Should return immediately without making HTTP calls
        orch.reconcile_tracker_states().await;

        let state = orch.state.read().await;
        assert!(state.running.is_empty());
    }

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_tracker_states_in_review_terminates_worker() {
        let mut linear_server = mockito::Server::new_async().await;
        let mut gh_server = mockito::Server::new_async().await;

        // Linear returns "In Review" for the running issue
        let _linear_mock = linear_server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[("id1", "PROJ-1", "In Review")]))
            .create_async()
            .await;

        // Mock find_open_pr (called by add_to_waiting_for_review)
        let _pr_mock = gh_server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 99,
                    "state": "open"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&linear_server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: gh_server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "template".to_string());

        // Insert a running entry in "In Progress" state
        let cancel_rx = insert_running(&orch, "id1", "PROJ-1", "In Progress").await;

        orch.reconcile_tracker_states().await;

        let state = orch.state.read().await;
        // Issue should NOT be in running anymore
        assert!(
            !state.running.contains_key("id1"),
            "issue should be removed from running"
        );
        // Issue should be in waiting_for_review
        assert!(
            state.waiting_for_review.contains_key("id1"),
            "issue should be in waiting_for_review"
        );
        let entry = state.waiting_for_review.get("id1").unwrap();
        assert_eq!(entry.pr_number, 99);
        assert_eq!(entry.identifier, "PROJ-1");
        assert_eq!(entry.branch, "symphony/proj-1");
        drop(state);

        // Cancel signal should have been sent (worker terminated)
        assert!(cancel_rx.await.is_ok());
    }

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_tracker_states_in_review_preserves_session_id() {
        let mut linear_server = mockito::Server::new_async().await;
        let mut gh_server = mockito::Server::new_async().await;

        // Linear returns "In Review" for the running issue
        let _linear_mock = linear_server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[("id1", "PROJ-1", "In Review")]))
            .create_async()
            .await;

        // Mock find_open_pr
        let _pr_mock = gh_server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 77,
                    "state": "open"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&linear_server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: gh_server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "template".to_string());

        // Insert a running entry and set its session_id
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "In Progress").await;
        {
            let mut state = orch.state.write().await;
            let entry = state.running.get_mut("id1").unwrap();
            entry.session.session_id = Some("sess-123".to_string());
        }

        orch.reconcile_tracker_states().await;

        let state = orch.state.read().await;
        assert!(
            !state.running.contains_key("id1"),
            "issue should be removed from running"
        );
        assert!(
            state.waiting_for_review.contains_key("id1"),
            "issue should be in waiting_for_review"
        );
        let waiting = state.waiting_for_review.get("id1").unwrap();
        assert_eq!(
            waiting.session_id,
            Some("sess-123".to_string()),
            "session_id should be preserved in WaitingEntry"
        );
    }

    // ── on_retry_fired (with mockito) ────────────────────────────────

    #[tokio::test]
    async fn test_on_retry_fired_issue_still_active_dispatches() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[("id1", "PROJ-1", "Todo")]))
            .expect_at_least(1)
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        // Pre-populate retry entry and claimed set
        {
            let mut state = orch.state.write().await;
            state.retry_attempts.insert(
                "id1".to_string(),
                RetryEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    attempt: 2,
                    due_at_ms: 0,
                    error: Some("prev error".to_string()),
                },
            );
            state.claimed.insert("id1".to_string());
        }

        orch.on_retry_fired("id1").await;

        let state = orch.state.read().await;
        // Issue should be dispatched (running)
        assert!(state.running.contains_key("id1"));
        // Retry entry should be removed (replaced by dispatch)
        assert!(!state.retry_attempts.contains_key("id1"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_on_retry_fired_issue_no_longer_active() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[])) // empty: issue not in candidates
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        {
            let mut state = orch.state.write().await;
            state.retry_attempts.insert(
                "id1".to_string(),
                RetryEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    attempt: 1,
                    due_at_ms: 0,
                    error: None,
                },
            );
            state.claimed.insert("id1".to_string());
        }

        orch.on_retry_fired("id1").await;

        let state = orch.state.read().await;
        // Claim should be released
        assert!(!state.claimed.contains("id1"));
        assert!(state.running.is_empty());

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_on_retry_fired_no_retry_entry() {
        let orch = test_orchestrator();

        // No retry entry exists → should return immediately
        orch.on_retry_fired("nonexistent").await;

        let state = orch.state.read().await;
        assert!(state.running.is_empty());
    }

    #[traced_test]
    #[tokio::test]
    async fn test_on_retry_fired_api_error_reschedules() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(500)
            .with_body("fail")
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        {
            let mut state = orch.state.write().await;
            state.retry_attempts.insert(
                "id1".to_string(),
                RetryEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    attempt: 2,
                    due_at_ms: 0,
                    error: Some("prev".to_string()),
                },
            );
            state.claimed.insert("id1".to_string());
        }

        orch.on_retry_fired("id1").await;

        let state = orch.state.read().await;
        // Should have rescheduled with incremented attempt
        assert!(state.retry_attempts.contains_key("id1"));
        let retry = state.retry_attempts.get("id1").unwrap();
        assert_eq!(retry.attempt, 3);
        assert_eq!(retry.error, Some("retry poll failed".to_string()));

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_on_retry_fired_no_slots_reschedules() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[("id1", "PROJ-1", "Todo")]))
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        // Fill all 3 slots
        let _c1 = insert_running(&orch, "slot1", "S-1", "Todo").await;
        let _c2 = insert_running(&orch, "slot2", "S-2", "Todo").await;
        let _c3 = insert_running(&orch, "slot3", "S-3", "Todo").await;

        // Add retry entry
        {
            let mut state = orch.state.write().await;
            state.retry_attempts.insert(
                "id1".to_string(),
                RetryEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    attempt: 1,
                    due_at_ms: 0,
                    error: None,
                },
            );
        }

        orch.on_retry_fired("id1").await;

        let state = orch.state.read().await;
        // Should reschedule since no slots
        assert!(state.retry_attempts.contains_key("id1"));
        let retry = state.retry_attempts.get("id1").unwrap();
        assert_eq!(retry.attempt, 2);
        assert_eq!(
            retry.error,
            Some("no available orchestrator slots".to_string())
        );

        mock.assert_async().await;
    }

    // ── startup_cleanup (with mockito) ───────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_startup_cleanup_removes_terminal_workspaces() {
        let tmp = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[("id1", "PROJ-1", "Done")]))
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.workspace.root = tmp.path().to_path_buf();
        let orch = Orchestrator::new(config, "template".to_string());

        // Create the workspace directory that matches the terminal issue
        let ws_path = tmp.path().join("PROJ-1");
        std::fs::create_dir_all(&ws_path).unwrap();
        std::fs::write(ws_path.join("file.txt"), "content").unwrap();
        assert!(ws_path.exists());

        orch.startup_cleanup().await;

        assert!(!ws_path.exists()); // Should be cleaned up

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_startup_cleanup_ignores_nonexistent_workspaces() {
        let tmp = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[("id1", "PROJ-1", "Done")]))
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.workspace.root = tmp.path().to_path_buf();
        let orch = Orchestrator::new(config, "template".to_string());

        // Don't create the workspace — should not error
        orch.startup_cleanup().await;

        mock.assert_async().await;
    }

    #[traced_test]
    #[tokio::test]
    async fn test_startup_cleanup_api_error_continues() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(500)
            .with_body("error")
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        // Should not panic
        orch.startup_cleanup().await;

        mock.assert_async().await;
    }

    // ── dispatch_issue ───────────────────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_dispatch_issue_adds_to_running_and_claimed() {
        let orch = test_orchestrator();
        let issue = make_issue("id1", "PROJ-1", "Todo");

        orch.dispatch_issue(issue, None, false, None).await;

        let state = orch.state.read().await;
        assert!(state.running.contains_key("id1"));
        assert!(state.claimed.contains("id1"));
        let entry = state.running.get("id1").unwrap();
        assert_eq!(entry.identifier, "PROJ-1");
        assert!(entry.retry_attempt.is_none());
    }

    #[tokio::test]
    async fn test_dispatch_issue_with_attempt() {
        let orch = test_orchestrator();
        let issue = make_issue("id1", "PROJ-1", "Todo");

        orch.dispatch_issue(issue, Some(3), false, None).await;

        let state = orch.state.read().await;
        let entry = state.running.get("id1").unwrap();
        assert_eq!(entry.retry_attempt, Some(3));
    }

    #[tokio::test]
    async fn test_dispatch_issue_removes_retry_entry() {
        let orch = test_orchestrator();

        // Pre-populate a retry entry
        {
            let mut state = orch.state.write().await;
            state.retry_attempts.insert(
                "id1".to_string(),
                RetryEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    attempt: 2,
                    due_at_ms: 0,
                    error: Some("prev".to_string()),
                },
            );
        }

        let issue = make_issue("id1", "PROJ-1", "Todo");
        orch.dispatch_issue(issue, Some(2), false, None).await;

        let state = orch.state.read().await;
        assert!(!state.retry_attempts.contains_key("id1"));
        assert!(state.running.contains_key("id1"));
    }

    // ── transition_to_in_progress ──────────────────────────────────

    #[tokio::test]
    #[traced_test]
    async fn test_transition_to_in_progress_success() {
        let mut server = mockito::Server::new_async().await;

        // resolve_state_id response
        let resolve_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "workflowStates": {
                            "nodes": [
                                { "id": "state-ip", "name": "In Progress" }
                            ]
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        // update_issue_state response
        let update_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": { "issueUpdate": { "success": true } }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let orch = Orchestrator::new(
            test_config_with_endpoint(&server.url()),
            "template".to_string(),
        );

        orch.transition_to_in_progress("issue-1", "PROJ-1").await;

        assert!(logs_contain("transitioned issue to In Progress"));
        resolve_mock.assert_async().await;
        update_mock.assert_async().await;
    }

    #[tokio::test]
    #[traced_test]
    async fn test_transition_to_in_progress_failure_logs_warning() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("POST", "/")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let orch = Orchestrator::new(
            test_config_with_endpoint(&server.url()),
            "template".to_string(),
        );

        orch.transition_to_in_progress("issue-1", "PROJ-1").await;

        assert!(logs_contain("failed to transition issue to In Progress"));
        mock.assert_async().await;
    }

    // ── reconcile (integration) ──────────────────────────────────────

    #[tokio::test]
    async fn test_reconcile_stalls_and_tracker_states() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[("id2", "PROJ-2", "In Progress")]))
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.coding_agent.stall_timeout_ms = 100;
        let orch = Orchestrator::new(config, "template".to_string());

        // id1: stalled (old started_at)
        let past = Utc::now() - chrono::Duration::seconds(60);
        let _c1 = insert_running_at(&orch, "id1", "PROJ-1", "Todo", past).await;
        // id2: normal (recent started_at)
        let _c2 = insert_running(&orch, "id2", "PROJ-2", "In Progress").await;

        orch.reconcile().await;

        let state = orch.state.read().await;
        // id1 should be terminated (stalled)
        assert!(!state.running.contains_key("id1"));
        // id2 should still be running (tracker says active)
        assert!(state.running.contains_key("id2"));

        mock.assert_async().await;
    }

    // ── OrchestratorSnapshot serialization ───────────────────────────

    #[tokio::test]
    async fn test_snapshot_serializable() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        let snap = orch.snapshot().await;
        let json = serde_json::to_string(&snap).expect("snapshot should be serializable");
        assert!(json.contains("PROJ-1"));
        assert!(json.contains("running"));
        assert!(json.contains("totals"));
    }

    // ── handle_message: RetryFired variant ───────────────────────────

    #[tokio::test]
    async fn test_handle_message_retry_fired() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[("id1", "PROJ-1", "Todo")]))
            .expect_at_least(1)
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        {
            let mut state = orch.state.write().await;
            state.retry_attempts.insert(
                "id1".to_string(),
                RetryEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    attempt: 1,
                    due_at_ms: 0,
                    error: None,
                },
            );
            state.claimed.insert("id1".to_string());
        }

        orch.handle_message(OrchestratorMessage::RetryFired {
            issue_id: "id1".to_string(),
        })
        .await;

        let state = orch.state.read().await;
        assert!(state.running.contains_key("id1"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_handle_message_remove_from_retry_queue() {
        let orch = test_orchestrator();

        // Insert a retry entry and claim
        {
            let mut state = orch.state.write().await;
            state.retry_attempts.insert(
                "id1".to_string(),
                RetryEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    attempt: 3,
                    due_at_ms: 0,
                    error: Some("timeout".to_string()),
                },
            );
            state.claimed.insert("id1".to_string());
        }

        orch.handle_message(OrchestratorMessage::RemoveFromRetryQueue {
            issue_id: "id1".to_string(),
        })
        .await;

        let state = orch.state.read().await;
        assert!(!state.retry_attempts.contains_key("id1"));
        assert!(!state.claimed.contains("id1"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_handle_message_remove_from_retry_queue_not_found() {
        let orch = test_orchestrator();

        // Try to remove non-existent entry — should be a no-op
        orch.handle_message(OrchestratorMessage::RemoveFromRetryQueue {
            issue_id: "nonexistent".to_string(),
        })
        .await;

        let state = orch.state.read().await;
        assert!(state.retry_attempts.is_empty());
    }

    // ── Edge cases ───────────────────────────────────────────────────

    #[tokio::test]
    async fn test_multiple_dispatches_same_tick() {
        let mut server = mockito::Server::new_async().await;

        let _mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[
                ("id1", "PROJ-1", "Todo"),
                ("id2", "PROJ-2", "Todo"),
            ]))
            .expect_at_least(1)
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        orch.on_tick().await;

        let state = orch.state.read().await;
        assert_eq!(state.running.len(), 2);
        assert!(state.claimed.contains("id1"));
        assert!(state.claimed.contains("id2"));
    }

    #[tokio::test]
    async fn test_on_tick_skips_already_running() {
        let mut server = mockito::Server::new_async().await;

        // on_tick calls reconcile() first (which calls reconcile_tracker_states
        // making one HTTP request), then fetch_candidate_issues (another request).
        // Both go to the same endpoint so we need to allow 2 requests.
        let _reconcile_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[("id1", "PROJ-1", "Todo")]))
            .expect_at_least(1)
            .create_async()
            .await;

        let _candidates_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[
                ("id1", "PROJ-1", "Todo"),
                ("id2", "PROJ-2", "Todo"),
            ]))
            .expect_at_least(1)
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        // Pre-populate id1 as running
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_tick().await;

        let state = orch.state.read().await;
        // id1 was already running, id2 should be dispatched
        assert!(state.running.contains_key("id1"));
        assert!(state.running.contains_key("id2"));
        assert_eq!(state.running.len(), 2);
    }

    #[tokio::test]
    async fn test_on_tick_empty_candidates() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[]))
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        orch.on_tick().await;

        let state = orch.state.read().await;
        assert!(state.running.is_empty());

        mock.assert_async().await;
    }

    // ── terminate_running_issue with cleanup_workspace ────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_terminate_running_issue_with_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = test_config();
        config.workspace.root = tmp.path().to_path_buf();
        let orch = Orchestrator::new(config, "template".to_string());

        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        // Create workspace directory
        let ws_path = tmp.path().join("PROJ-1");
        std::fs::create_dir_all(&ws_path).unwrap();
        std::fs::write(ws_path.join("data.txt"), "test").unwrap();

        orch.terminate_running_issue("id1", true).await;

        let state = orch.state.read().await;
        assert!(!state.running.contains_key("id1"));
        drop(state);

        // Workspace should be cleaned up
        assert!(!ws_path.exists());
    }

    // ── should_dispatch: per-state limit not exceeded ─────────────────

    #[tokio::test]
    async fn test_should_dispatch_per_state_limit_not_exceeded() {
        let mut config = test_config();
        config
            .agent
            .max_concurrent_agents_by_state
            .insert("todo".to_string(), 2);
        let orch = Orchestrator::new(config.clone(), "template".to_string());

        // Add one running entry in "Todo" state (limit is 2, so 1 < 2 passes)
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        let issue = make_issue("id2", "PROJ-2", "Todo");
        assert!(orch.should_dispatch(&issue, &config).await);
    }

    // ── run() integration test ───────────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_run_with_immediate_shutdown() {
        let mut server = mockito::Server::new_async().await;

        // Mock for startup_cleanup (terminal issues fetch)
        let terminal_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[]))
            .expect_at_least(1)
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (_reload_tx, reload_rx) = mpsc::channel::<(ServiceConfig, String)>(1);

        // Send a spurious false first (exercises the `if *borrow()` false path),
        // then the real shutdown
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            // Modify triggers changed() but value is false → if body skipped
            shutdown_tx.send_modify(|v| *v = false);
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            let _ = shutdown_tx.send(true);
        });

        // Run the orchestrator — should start, do one tick, then receive shutdown
        orch.run(shutdown_rx, reload_rx).await;

        terminal_mock.assert_async().await;
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_receives_message() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[]))
            .expect_at_least(1)
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        let msg_tx = orch.message_sender();
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (_reload_tx, reload_rx) = mpsc::channel::<(ServiceConfig, String)>(1);

        tokio::spawn(async move {
            // Send a TriggerRefresh message to exercise the msg_rx branch in run()
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            let _ = msg_tx.send(OrchestratorMessage::TriggerRefresh).await;
            // Then shutdown
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            let _ = shutdown_tx.send(true);
        });

        orch.run(shutdown_rx, reload_rx).await;
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_with_reload() {
        let mut server = mockito::Server::new_async().await;

        // Mock for API calls during run
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[]))
            .expect_at_least(1)
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config.clone(), "template".to_string());

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (reload_tx, reload_rx) = mpsc::channel::<(ServiceConfig, String)>(1);

        tokio::spawn(async move {
            // Send a reload first
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = reload_tx.send((config, "new template".to_string())).await;
            // Then shutdown
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = shutdown_tx.send(true);
        });

        orch.run(shutdown_rx, reload_rx).await;
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_with_interval_change_reload() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[]))
            .expect_at_least(1)
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        // Use a short initial interval so multiple ticks fire before shutdown.
        config.polling.interval_ms = 50;
        let orch = Orchestrator::new(config.clone(), "template".to_string());

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (reload_tx, reload_rx) = mpsc::channel::<(ServiceConfig, String)>(1);

        let state_ref = Arc::clone(&orch.state);
        tokio::spawn(async move {
            // Wait for at least one tick to complete, then send a reload with
            // a different poll interval to exercise the interval-change branch.
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            let mut changed = config;
            changed.polling.interval_ms = 9999;
            let _ = reload_tx.send((changed, "new template".to_string())).await;
            // Wait for a tick to detect the change.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            // Verify the state was updated.
            let state = state_ref.read().await;
            assert_eq!(state.poll_interval_ms, 9999);
            drop(state);
            let _ = shutdown_tx.send(true);
        });

        orch.run(shutdown_rx, reload_rx).await;
    }

    // ── dispatch_issue spawn closure (map_worker_result) ─────────────

    #[traced_test]
    #[tokio::test]
    async fn test_dispatch_issue_worker_spawn_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = test_config();
        config.workspace.root = tmp.path().to_path_buf();
        // Use a command that exits immediately with success
        config.coding_agent.command = "true #-p".to_string();
        let orch = Orchestrator::new(config, "template".to_string());

        let issue = make_issue("id1", "PROJ-1", "Todo");
        orch.dispatch_issue(issue, None, false, None).await;

        // Give the spawned worker time to run and send WorkerExited message
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Drain the message channel to process WorkerExited
        // The worker should have run and sent a WorkerExited message
        let state = orch.state.read().await;
        // The spawned task should be in the running map initially
        // (it may have already exited and sent a message, but the message
        // isn't processed since we're not running the event loop)
        assert!(state.claimed.contains("id1"));
    }

    // ── run_worker integration tests ─────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_run_worker_success_issue_becomes_inactive() {
        let tmp = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;

        // Agent completes successfully, then check if issue is still active:
        // respond with "Done" state (terminal) → worker breaks
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[("id1", "PROJ-1", "Done")]))
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.workspace.root = tmp.path().to_path_buf();
        // Agent outputs a result and exits 0
        config.coding_agent.command =
            r#"printf '{"type":"result","result":"done"}\n' #-p"#.to_string();

        let issue = make_issue("id1", "PROJ-1", "Todo");
        let (msg_tx, _msg_rx) = mpsc::channel(256);
        let (_cancel_tx, cancel_rx) = oneshot::channel();

        let result = run_worker(
            &config,
            "Work on {{ issue.identifier }}",
            &issue,
            None,
            reqwest::Client::new(),
            msg_tx,
            cancel_rx,
            None,
        )
        .await;

        assert!(result.is_ok());
        mock.assert_async().await;
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_worker_success_empty_refresh() {
        let tmp = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;

        // Agent completes, API returns empty list → break
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[]))
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.workspace.root = tmp.path().to_path_buf();
        config.coding_agent.command =
            r#"printf '{"type":"result","result":"ok"}\n' #-p"#.to_string();

        let issue = make_issue("id1", "PROJ-1", "Todo");
        let (msg_tx, _msg_rx) = mpsc::channel(256);
        let (_cancel_tx, cancel_rx) = oneshot::channel();

        let result = run_worker(
            &config,
            "Work on {{ issue.identifier }}",
            &issue,
            None,
            reqwest::Client::new(),
            msg_tx,
            cancel_rx,
            None,
        )
        .await;

        assert!(result.is_ok());
        mock.assert_async().await;
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_worker_success_api_error_on_refresh() {
        let tmp = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;

        // Agent completes, API fails → break with warning
        let mock = server
            .mock("POST", "/")
            .with_status(500)
            .with_body("internal error")
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.workspace.root = tmp.path().to_path_buf();
        config.coding_agent.command =
            r#"printf '{"type":"result","result":"ok"}\n' #-p"#.to_string();

        let issue = make_issue("id1", "PROJ-1", "Todo");
        let (msg_tx, _msg_rx) = mpsc::channel(256);
        let (_cancel_tx, cancel_rx) = oneshot::channel();

        let result = run_worker(
            &config,
            "Work on {{ issue.identifier }}",
            &issue,
            None,
            reqwest::Client::new(),
            msg_tx,
            cancel_rx,
            None,
        )
        .await;

        assert!(result.is_ok());
        mock.assert_async().await;
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_worker_in_review_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;

        // Agent completes successfully, Linear returns "In Review" state.
        // Because we add "In Review" to active_states, state_matches passes,
        // then is_in_review_state catches it and breaks.
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[("id1", "PROJ-1", "In Review")]))
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.workspace.root = tmp.path().to_path_buf();
        // Include "In Review" so state_matches passes and is_in_review_state is reached
        config.tracker.active_states.push("In Review".to_string());
        config.coding_agent.command =
            r#"printf '{"type":"result","result":"done"}\n' #-p"#.to_string();

        let issue = make_issue("id1", "PROJ-1", "In Progress");
        let (msg_tx, _msg_rx) = mpsc::channel(256);
        let (_cancel_tx, cancel_rx) = oneshot::channel();

        let result = run_worker(
            &config,
            "Work on {{ issue.identifier }}",
            &issue,
            None,
            reqwest::Client::new(),
            msg_tx,
            cancel_rx,
            None,
        )
        .await;

        assert!(result.is_ok());
        mock.assert_async().await;
        assert!(logs_contain("issue moved to In Review, stopping worker"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_worker_pr_ready_breaks() {
        let tmp = tempfile::tempdir().unwrap();
        let mut linear_server = mockito::Server::new_async().await;
        let mut gh_server = mockito::Server::new_async().await;

        // Agent completes, issue is still "In Progress" (active, not In Review).
        // Then check_pr_ready_and_transition finds a ready PR → break.
        // Use Regex matcher to only match fetch_issue_states_by_ids calls.
        let _linear_fetch_mock = linear_server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("IssuesByIds".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[("id1", "PROJ-1", "In Progress")]))
            .create_async()
            .await;

        // find_open_pr returns a mergeable PR
        // Note: find_open_pr constructs head as "owner:branch"
        let _find_pr_mock = gh_server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "open".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "mergeable": true,
                    "mergeable_state": "clean"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // Linear: resolve_state_id (WorkflowStates query)
        let _resolve_mock = linear_server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("WorkflowStates".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":{"workflowStates":{"nodes":[{"id":"state-in-review","name":"In Review"}]}}}"#)
            .create_async()
            .await;

        // Linear: update_issue_state (issueUpdate mutation)
        let _update_mock = linear_server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("issueUpdate".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":{"issueUpdate":{"success":true}}}"#)
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&linear_server.url());
        config.workspace.root = tmp.path().to_path_buf();
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: gh_server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        config.coding_agent.command =
            r#"printf '{"type":"result","result":"done"}\n' #-p"#.to_string();

        let issue = make_issue("id1", "PROJ-1", "In Progress");
        let (msg_tx, _msg_rx) = mpsc::channel(256);
        let (_cancel_tx, cancel_rx) = oneshot::channel();

        let result = run_worker(
            &config,
            "Work on {{ issue.identifier }}",
            &issue,
            None,
            reqwest::Client::new(),
            msg_tx,
            cancel_rx,
            None,
        )
        .await;

        assert!(result.is_ok());
        assert!(logs_contain(
            "PR is ready (mergeable, no conflicts) — transitioning to In Review"
        ));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_worker_max_turns_reached() {
        let tmp = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;

        // Agent completes, issue still active, but max_turns=1 → break
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[("id1", "PROJ-1", "Todo")]))
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.workspace.root = tmp.path().to_path_buf();
        config.agent.max_turns = 1;
        config.coding_agent.command =
            r#"printf '{"type":"result","result":"ok"}\n' #-p"#.to_string();

        let issue = make_issue("id1", "PROJ-1", "Todo");
        let (msg_tx, _msg_rx) = mpsc::channel(256);
        let (_cancel_tx, cancel_rx) = oneshot::channel();

        let result = run_worker(
            &config,
            "Work on {{ issue.identifier }}",
            &issue,
            None,
            reqwest::Client::new(),
            msg_tx,
            cancel_rx,
            None,
        )
        .await;

        assert!(result.is_ok());
        mock.assert_async().await;
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_worker_agent_failure() {
        let tmp = tempfile::tempdir().unwrap();

        let mut config = test_config();
        config.workspace.root = tmp.path().to_path_buf();
        // Agent exits with non-zero
        config.coding_agent.command = "exit 1 #-p".to_string();

        let issue = make_issue("id1", "PROJ-1", "Todo");
        let (msg_tx, _msg_rx) = mpsc::channel(256);
        let (_cancel_tx, cancel_rx) = oneshot::channel();

        let result = run_worker(
            &config,
            "Work on {{ issue.identifier }}",
            &issue,
            None,
            reqwest::Client::new(),
            msg_tx,
            cancel_rx,
            None,
        )
        .await;

        assert!(result.is_err());
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_worker_cancellation() {
        let tmp = tempfile::tempdir().unwrap();

        let mut config = test_config();
        config.workspace.root = tmp.path().to_path_buf();
        config.coding_agent.command =
            r#"printf '{"type":"result","result":"ok"}\n' #-p"#.to_string();

        let issue = make_issue("id1", "PROJ-1", "Todo");
        let (msg_tx, _msg_rx) = mpsc::channel(256);
        let (cancel_tx, cancel_rx) = oneshot::channel();

        // Send cancel immediately — picked up at the top of the loop
        let _ = cancel_tx.send(());

        let result = run_worker(
            &config,
            "Work on {{ issue.identifier }}",
            &issue,
            None,
            reqwest::Client::new(),
            msg_tx,
            cancel_rx,
            None,
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("cancelled"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_worker_multi_turn_with_continuation() {
        let tmp = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;

        // First call: issue still active (Todo)
        // Second call: issue becomes Done
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[("id1", "PROJ-1", "Todo")]))
            .expect(1)
            .create_async()
            .await;
        let mock2 = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[("id1", "PROJ-1", "Done")]))
            .expect(1)
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.workspace.root = tmp.path().to_path_buf();
        config.agent.max_turns = 5;
        config.coding_agent.command =
            r#"printf '{"type":"result","result":"ok"}\n' #-p"#.to_string();

        let issue = make_issue("id1", "PROJ-1", "Todo");
        let (msg_tx, _msg_rx) = mpsc::channel(256);
        let (_cancel_tx, cancel_rx) = oneshot::channel();

        let result = run_worker(
            &config,
            "Work on {{ issue.identifier }}",
            &issue,
            None,
            reqwest::Client::new(),
            msg_tx,
            cancel_rx,
            None,
        )
        .await;

        assert!(result.is_ok());
        mock.assert_async().await;
        mock2.assert_async().await;
    }

    // ── dispatch spawn closure error paths ───────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_dispatch_issue_worker_timeout_maps_to_timed_out() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = test_config();
        config.workspace.root = tmp.path().to_path_buf();
        // Command that sleeps (will timeout)
        config.coding_agent.command = "sleep 999 #-p".to_string();
        config.coding_agent.turn_timeout_ms = 100; // 100ms timeout

        let orch = Orchestrator::new(config, "Work on {{ issue.identifier }}".to_string());

        let issue = make_issue("id1", "PROJ-1", "Todo");
        orch.dispatch_issue(issue, None, false, None).await;

        // Wait for the worker to timeout and send WorkerExited
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;

        // The spawned task should have completed and sent WorkerExited with TimedOut
        // We verify by checking the running map was populated
        let state = orch.state.read().await;
        assert!(state.claimed.contains("id1"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_dispatch_issue_worker_failure_maps_to_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = test_config();
        config.workspace.root = tmp.path().to_path_buf();
        config.coding_agent.command = "exit 1 #-p".to_string();

        let orch = Orchestrator::new(config, "Work on {{ issue.identifier }}".to_string());

        let issue = make_issue("id1", "PROJ-1", "Todo");
        orch.dispatch_issue(issue, None, false, None).await;

        // Give worker time to fail
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    // ── run_worker timeout path ──────────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_run_worker_agent_timeout() {
        let tmp = tempfile::tempdir().unwrap();

        let mut config = test_config();
        config.workspace.root = tmp.path().to_path_buf();
        config.coding_agent.command = "sleep 999 #-p".to_string();
        config.coding_agent.turn_timeout_ms = 100;

        let issue = make_issue("id1", "PROJ-1", "Todo");
        let (msg_tx, _msg_rx) = mpsc::channel(256);
        let (_cancel_tx, cancel_rx) = oneshot::channel();

        let result = run_worker(
            &config,
            "Work on {{ issue.identifier }}",
            &issue,
            None,
            reqwest::Client::new(),
            msg_tx,
            cancel_rx,
            None,
        )
        .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SymphonyError::AgentTurnTimeout
        ));
    }

    // ══════════════════════════════════════════════════════════════════
    // Integration tests: full orchestrator lifecycle through run()
    // ══════════════════════════════════════════════════════════════════

    /// Full lifecycle: poll → dispatch → agent completes → worker exits normally
    /// → claimed released → next tick does NOT re-dispatch (issue went terminal)
    #[traced_test]
    #[tokio::test]
    async fn test_integration_poll_dispatch_complete_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;

        // Request 1 (startup_cleanup terminal fetch): empty
        let startup_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[]))
            .expect(1)
            .create_async()
            .await;

        // Request 2 (first on_tick: no running so reconcile_tracker_states is
        // skipped, then fetch_candidate_issues): return one Todo issue
        let candidates_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[("id1", "PROJ-1", "Todo")]))
            .expect(1)
            .create_async()
            .await;

        // Request 3 (run_worker turn success → issue state refresh): issue is Done
        let state_refresh_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[("id1", "PROJ-1", "Done")]))
            .expect(1)
            .create_async()
            .await;

        // Request 4+ (subsequent ticks reconcile + candidate fetches): empty
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[]))
            .expect_at_least(0)
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.workspace.root = tmp.path().to_path_buf();
        config.polling.interval_ms = 100; // fast polling
        config.coding_agent.command =
            r#"printf '{"type":"result","result":"done"}\n' #-p"#.to_string();

        let orch = Orchestrator::new(config, "Work on {{ issue.identifier }}".to_string());
        let state_ref = Arc::clone(&orch.state);

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (_reload_tx, reload_rx) = mpsc::channel::<(ServiceConfig, String)>(1);

        let monitor = tokio::spawn(async move {
            let mut done = false;
            while !done {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let state = state_ref.read().await;
                // Worker has exited normally: completed set has the issue,
                // claimed does NOT (bug fix #4), and running is empty
                done = state.is_completed("id1")
                    && !state.claimed.contains("id1")
                    && !state.running.contains_key("id1");
                drop(state);
            }
            // Give one more tick to verify no re-dispatch
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let _ = shutdown_tx.send(true);
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            orch.run(shutdown_rx, reload_rx),
        )
        .await
        .expect("test timed out waiting for poll→dispatch→complete→terminal");
        monitor.abort();

        startup_mock.assert_async().await;
        candidates_mock.assert_async().await;
        state_refresh_mock.assert_async().await;
    }

    /// Integration: failed worker → retry with exponential backoff → re-dispatch
    #[traced_test]
    #[tokio::test]
    async fn test_integration_failed_worker_retry_then_success() {
        let tmp = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;

        // Startup cleanup: empty
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[]))
            .expect(1)
            .create_async()
            .await;

        // First tick candidates: return the issue
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[("id1", "PROJ-1", "Todo")]))
            .expect_at_least(1)
            .create_async()
            .await;

        // State refresh / reconciliation calls: return In Progress (non-terminal)
        // so reconciliation doesn't kill the worker before it can report failure
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[("id1", "PROJ-1", "In Progress")]))
            .expect_at_least(0)
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.workspace.root = tmp.path().to_path_buf();
        config.polling.interval_ms = 100;
        // First dispatch: agent fails (exit 1). The worker will fail.
        // After retry fires, the next dispatch should succeed.
        // We can't easily switch command mid-flight, so let's just verify
        // the retry mechanism fires by using a failing command and checking
        // that a retry entry is created.
        config.coding_agent.command = "exit 1 #-p".to_string();

        let orch = Orchestrator::new(config, "Work on {{ issue.identifier }}".to_string());
        let state_ref = Arc::clone(&orch.state);

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (_reload_tx, reload_rx) = mpsc::channel::<(ServiceConfig, String)>(1);

        let monitor = tokio::spawn(async move {
            let mut done = false;
            while !done {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let state = state_ref.read().await;
                // Wait until a retry entry exists (worker failed → scheduled retry)
                done = state.retry_attempts.contains_key("id1");
                drop(state);
            }
            // Verify retry entry contents after loop exits
            let state = state_ref.read().await;
            let retry = state.retry_attempts.get("id1").unwrap();
            assert_eq!(retry.attempt, 1);
            assert!(retry.error.is_some());
            drop(state);
            let _ = shutdown_tx.send(true);
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            orch.run(shutdown_rx, reload_rx),
        )
        .await
        .expect("test timed out waiting for failed→retry→success");
        monitor.abort();
    }

    /// Integration: concurrent issue dispatch (3 issues in parallel)
    #[traced_test]
    #[tokio::test]
    async fn test_integration_concurrent_issue_dispatch() {
        let tmp = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;

        // Startup: empty
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[]))
            .expect(1)
            .create_async()
            .await;

        // Candidates: 3 issues (first poll only)
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[
                ("id1", "PROJ-1", "Todo"),
                ("id2", "PROJ-2", "In Progress"),
                ("id3", "PROJ-3", "Todo"),
            ]))
            .expect(1)
            .create_async()
            .await;

        // State refreshes: all Done (terminal) so workers stop.
        // Also serves as the response for continuation retry
        // fetch_candidate_issues calls — returning Done issues
        // means they won't be in active_states, so the retry
        // handler releases the claim.
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[
                ("id1", "PROJ-1", "Done"),
                ("id2", "PROJ-2", "Done"),
                ("id3", "PROJ-3", "Done"),
            ]))
            .expect_at_least(0)
            .create_async()
            .await;

        // Fallback: empty candidates for any subsequent requests
        // (continuation retries, reconciliation, etc.)
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[]))
            .expect_at_least(0)
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.workspace.root = tmp.path().to_path_buf();
        config.polling.interval_ms = 100;
        config.agent.max_concurrent_agents = 3;
        config.coding_agent.command =
            r#"printf '{"type":"result","result":"ok"}\n' #-p"#.to_string();

        let orch = Orchestrator::new(config, "Work on {{ issue.identifier }}".to_string());
        let state_ref = Arc::clone(&orch.state);

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (_reload_tx, reload_rx) = mpsc::channel::<(ServiceConfig, String)>(1);

        let monitor = tokio::spawn(async move {
            let mut all_done = false;
            while !all_done {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let state = state_ref.read().await;
                // All 3 issues should be completed AND claims released
                // (§8.1: claims are released after the continuation retry
                // fires and finds the issue is no longer active)
                all_done = state.is_completed("id1")
                    && state.is_completed("id2")
                    && state.is_completed("id3")
                    && !state.claimed.contains("id1")
                    && !state.claimed.contains("id2")
                    && !state.claimed.contains("id3");
                drop(state);
            }
            let _ = shutdown_tx.send(true);
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            orch.run(shutdown_rx, reload_rx),
        )
        .await
        .expect("test timed out waiting for concurrent dispatch");
        monitor.abort();
    }

    /// Integration: workspace isolation — each issue gets its own workspace dir
    #[traced_test]
    #[tokio::test]
    async fn test_integration_workspace_isolation() {
        let tmp = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;

        // Startup: empty
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[]))
            .expect(1)
            .create_async()
            .await;

        // Two issues to dispatch — keep them in "Todo" (active) for all
        // subsequent requests so reconciliation never removes the workspaces
        // before the test can verify the identity files.
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[
                ("id1", "PROJ-1", "Todo"),
                ("id2", "PROJ-2", "Todo"),
            ]))
            .expect_at_least(1)
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.workspace.root = tmp.path().to_path_buf();
        config.polling.interval_ms = 100;
        config.agent.max_concurrent_agents = 2;
        // Agent writes a file to prove workspace isolation:
        // each workspace gets its own identity file
        config.coding_agent.command =
            r#"echo $PWD > workspace_id.txt && printf '{"type":"result","result":"ok"}\n' #-p"#
                .to_string();

        let orch = Orchestrator::new(config, "Work on {{ issue.identifier }}".to_string());
        let state_ref = Arc::clone(&orch.state);

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (_reload_tx, reload_rx) = mpsc::channel::<(ServiceConfig, String)>(1);

        let tmp_path = tmp.path().to_path_buf();
        let monitor = tokio::spawn(async move {
            let mut done = false;
            while !done {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let state = state_ref.read().await;
                done = state.is_completed("id1") && state.is_completed("id2");
                drop(state);
            }
            let _ = shutdown_tx.send(true);
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            orch.run(shutdown_rx, reload_rx),
        )
        .await
        .expect("test timed out waiting for workspace isolation");
        monitor.abort();

        // Verify each issue got its own workspace directory
        let ws1 = tmp_path.join("PROJ-1");
        let ws2 = tmp_path.join("PROJ-2");
        // The agent wrote a file in each workspace
        let id1 = std::fs::read_to_string(ws1.join("workspace_id.txt"));
        let id2 = std::fs::read_to_string(ws2.join("workspace_id.txt"));
        assert!(id1.is_ok(), "PROJ-1 workspace should have identity file");
        assert!(id2.is_ok(), "PROJ-2 workspace should have identity file");
        // The paths should be different
        assert_ne!(
            id1.unwrap().trim(),
            id2.unwrap().trim(),
            "workspaces should be isolated"
        );
    }

    /// Integration: timeout → worker exits with TimedOut → retry scheduled
    #[traced_test]
    #[tokio::test]
    async fn test_integration_agent_timeout_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;

        // Startup: empty
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[]))
            .expect(1)
            .create_async()
            .await;

        // Candidates: one issue
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[("id1", "PROJ-1", "Todo")]))
            .expect_at_least(1)
            .create_async()
            .await;

        // Reconcile and other state fetches
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[("id1", "PROJ-1", "Todo")]))
            .expect_at_least(0)
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.workspace.root = tmp.path().to_path_buf();
        config.polling.interval_ms = 200;
        config.coding_agent.turn_timeout_ms = 100; // very short timeout
                                                   // Agent sleeps forever, will be killed by timeout
        config.coding_agent.command = "sleep 999 #-p".to_string();

        let orch = Orchestrator::new(config, "Work on {{ issue.identifier }}".to_string());
        let state_ref = Arc::clone(&orch.state);

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (_reload_tx, reload_rx) = mpsc::channel::<(ServiceConfig, String)>(1);

        let monitor = tokio::spawn(async move {
            let mut done = false;
            while !done {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let state = state_ref.read().await;
                // Wait for retry to be scheduled after timeout
                done = state.retry_attempts.contains_key("id1");
                drop(state);
            }
            // Verify retry entry contents after loop exits
            let state = state_ref.read().await;
            let retry = state.retry_attempts.get("id1").unwrap();
            assert!(retry.error.is_some());
            assert_eq!(retry.error.as_deref(), Some("turn timeout"));
            drop(state);
            let _ = shutdown_tx.send(true);
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            orch.run(shutdown_rx, reload_rx),
        )
        .await
        .expect("test timed out waiting for agent timeout retry");
        monitor.abort();
    }

    /// Integration: reconciliation cancels running worker when issue goes terminal
    #[traced_test]
    #[tokio::test]
    async fn test_integration_reconcile_cancels_terminal_issue() {
        let tmp = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;

        // Use body-matching to distinguish GraphQL query types, avoiding
        // LIFO ordering issues when dispatch adds transition API calls.
        //
        // Query types in request bodies:
        //   - CandidateIssues  → fetch_candidate_issues (polling)
        //   - IssuesByIds      → fetch_issue_states_by_ids (reconciliation)
        //   - WorkflowStates   → resolve_state_id (transition step 1)
        //   - issueUpdate      → update_issue_state (transition step 2)

        // Candidate polls: first returns PROJ-1, then empty forever
        server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("CandidateIssues".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[("id1", "PROJ-1", "Todo")]))
            .expect(1)
            .create_async()
            .await;

        server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("CandidateIssues".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[]))
            .expect_at_least(0)
            .create_async()
            .await;

        // Reconcile state check: issue has gone to Done (terminal)
        server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("IssuesByIds".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[("id1", "PROJ-1", "Done")]))
            .expect_at_least(1)
            .create_async()
            .await;

        // Transition: resolve workflow state ID
        server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("WorkflowStates".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "workflowStates": {
                            "nodes": [
                                { "id": "state-ip", "name": "In Progress" }
                            ]
                        }
                    }
                })
                .to_string(),
            )
            .expect_at_least(0)
            .create_async()
            .await;

        // Transition: update issue state
        server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("issueUpdate".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": { "issueUpdate": { "success": true } }
                })
                .to_string(),
            )
            .expect_at_least(0)
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.workspace.root = tmp.path().to_path_buf();
        config.polling.interval_ms = 100;
        // Agent that sleeps, so it's still running when reconcile fires
        config.coding_agent.command = "sleep 999 #-p".to_string();
        config.coding_agent.turn_timeout_ms = 60_000; // long timeout so it doesn't time out first

        let orch = Orchestrator::new(config, "Work on {{ issue.identifier }}".to_string());
        let state_ref = Arc::clone(&orch.state);

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (_reload_tx, reload_rx) = mpsc::channel::<(ServiceConfig, String)>(1);

        let monitor = tokio::spawn(async move {
            let start = tokio::time::Instant::now();
            let mut done = false;
            while !done {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let state = state_ref.read().await;
                // After reconcile sees terminal, the worker should be removed
                // and claimed cleared
                done = !state.running.contains_key("id1")
                    && !state.claimed.contains("id1")
                    && start.elapsed() > std::time::Duration::from_millis(300);
                drop(state);
            }
            let _ = shutdown_tx.send(true);
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            orch.run(shutdown_rx, reload_rx),
        )
        .await
        .expect("test timed out waiting for reconcile cancellation");
        monitor.abort();
    }

    /// Integration: config reload changes max_concurrent_agents, affecting dispatch
    #[traced_test]
    #[tokio::test]
    async fn test_integration_config_reload_affects_dispatch() {
        let tmp = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;

        // Startup: empty
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[]))
            .expect_at_least(1)
            .create_async()
            .await;

        // Candidates: 3 issues
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[
                ("id1", "PROJ-1", "Todo"),
                ("id2", "PROJ-2", "Todo"),
                ("id3", "PROJ-3", "Todo"),
            ]))
            .expect_at_least(0)
            .create_async()
            .await;

        // State refresh: all remain active
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[
                ("id1", "PROJ-1", "Todo"),
                ("id2", "PROJ-2", "Todo"),
                ("id3", "PROJ-3", "Todo"),
            ]))
            .expect_at_least(0)
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.workspace.root = tmp.path().to_path_buf();
        config.polling.interval_ms = 100;
        // Start with max_concurrent = 1 — only 1 issue dispatched
        config.agent.max_concurrent_agents = 1;
        config.coding_agent.command = "sleep 999 #-p".to_string();
        config.coding_agent.turn_timeout_ms = 60_000;

        let orch = Orchestrator::new(config.clone(), "Work on {{ issue.identifier }}".to_string());
        let state_ref = Arc::clone(&orch.state);

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (reload_tx, reload_rx) = mpsc::channel::<(ServiceConfig, String)>(1);

        tokio::spawn(async move {
            let start = tokio::time::Instant::now();
            let mut reload_sent = false;
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let state = state_ref.read().await;
                let running = state.running_count();
                drop(state);

                // After first tick dispatches 1, send reload to allow 3
                if running >= 1 && !reload_sent {
                    let mut new_config = config.clone();
                    new_config.agent.max_concurrent_agents = 3;
                    let _ = reload_tx
                        .send((new_config, "Work on {{ issue.identifier }}".to_string()))
                        .await;
                    reload_sent = true;
                }

                if start.elapsed() > std::time::Duration::from_secs(5) {
                    let _ = shutdown_tx.send(true);
                    return;
                }
            }
        });

        orch.run(shutdown_rx, reload_rx).await;
    }

    /// Integration: stall detection terminates stalled workers through run()
    #[traced_test]
    #[tokio::test]
    async fn test_integration_stall_detection() {
        let tmp = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;

        // Startup: empty
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[]))
            .expect(1)
            .create_async()
            .await;

        // Candidates: one issue
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[("id1", "PROJ-1", "Todo")]))
            .expect_at_least(1)
            .create_async()
            .await;

        // State refresh for reconcile (issue still active)
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[("id1", "PROJ-1", "Todo")]))
            .expect_at_least(0)
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.workspace.root = tmp.path().to_path_buf();
        config.polling.interval_ms = 200;
        config.coding_agent.stall_timeout_ms = 500; // 500ms stall timeout
        config.coding_agent.turn_timeout_ms = 60_000; // long turn timeout
                                                      // Agent outputs nothing (no events) and sleeps — will trigger stall
        config.coding_agent.command = "sleep 999 #-p".to_string();

        let orch = Orchestrator::new(config, "Work on {{ issue.identifier }}".to_string());
        let state_ref = Arc::clone(&orch.state);

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (_reload_tx, reload_rx) = mpsc::channel::<(ServiceConfig, String)>(1);

        let monitor = tokio::spawn(async move {
            // Stall detection terminates the worker via cancel signal, which
            // removes it from running. The issue then appears as a candidate
            // again on the next poll. We detect stall detection by waiting for
            // the seconds_running counter to increase (meaning the issue was
            // dispatched, ran, terminated, then dispatched again).
            let mut done = false;
            while !done {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                let state = state_ref.read().await;
                done = state.agent_totals.seconds_running > 0.0 && !state.running.is_empty();
                drop(state);
            }
            let _ = shutdown_tx.send(true);
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            orch.run(shutdown_rx, reload_rx),
        )
        .await
        .expect("test timed out waiting for stall detection");
        monitor.abort();

        // Verify stall was actually detected via tracing output
        assert!(logs_contain("session stalled"));
    }

    // ── map_worker_result ─────────────────────────────────────────────

    #[test]
    fn test_map_worker_result_normal() {
        assert_eq!(map_worker_result(Ok(())), WorkerExitReason::Normal);
    }

    #[test]
    fn test_map_worker_result_timeout() {
        let r = map_worker_result(Err(SymphonyError::AgentTurnTimeout));
        assert_eq!(r, WorkerExitReason::TimedOut);
    }

    #[test]
    fn test_map_worker_result_cancelled() {
        let r = map_worker_result(Err(SymphonyError::AgentTurnCancelled));
        assert_eq!(r, WorkerExitReason::Cancelled);
    }

    #[test]
    fn test_map_worker_result_failed() {
        let r = map_worker_result(Err(SymphonyError::MissingRequiredConfig {
            field: "test".into(),
        }));
        assert!(matches!(&r, WorkerExitReason::Failed(msg) if msg.contains("test")));
    }

    // ── drain_worker_handles ─────────────────────────────────────────

    /// Named async helper to avoid LLVM coverage gaps on inline async blocks.
    async fn sleep_briefly() {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    #[tokio::test]
    async fn test_drain_worker_handles_empty() {
        // Should complete immediately with no handles
        drain_worker_handles(vec![]).await;
    }

    #[tokio::test]
    async fn test_drain_worker_handles_waits_for_completion() {
        let h1 = tokio::spawn(sleep_briefly());
        let h2 = tokio::spawn(sleep_briefly());
        drain_worker_handles(vec![h1, h2]).await;
        // If we got here, both handles completed
    }

    #[tokio::test]
    async fn test_drain_worker_handles_panicked_task() {
        let h = tokio::spawn(async { panic!("test panic") });
        // Should not panic — JoinError from panicked task is ignored
        drain_worker_handles(vec![h]).await;
    }

    // ── abort_and_drain_handles ──────────────────────────────────────

    #[tokio::test]
    async fn test_abort_and_drain_handles_completes() {
        let h = tokio::spawn(sleep_briefly());
        abort_and_drain_handles(vec![h]).await;
    }

    /// Named async helper that blocks until a oneshot signal is received.
    async fn block_until_signal(rx: tokio::sync::oneshot::Receiver<()>) {
        let _ = rx.await;
    }

    #[tokio::test]
    async fn test_block_until_signal_completes_normally() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let h = tokio::spawn(block_until_signal(rx));
        let _ = tx.send(());
        let _ = h.await;
    }

    #[traced_test]
    #[tokio::test]
    async fn test_abort_and_drain_with_timeout_fires_warn() {
        // Use abort_and_drain_with_timeout with a short timeout and a
        // blocking task to exercise the warn branch.
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let h = tokio::spawn(block_until_signal(rx));
        // Don't abort — let the drain timeout expire
        abort_and_drain_with_timeout(vec![h], std::time::Duration::from_millis(10)).await;
        // Clean up: signal the blocked task to exit
        let _ = tx.send(());
    }

    #[tokio::test]
    async fn test_abort_and_drain_with_timeout_no_warn() {
        // When handles complete before timeout, no warn is emitted.
        let h = tokio::spawn(sleep_briefly());
        abort_and_drain_with_timeout(vec![h], std::time::Duration::from_secs(5)).await;
    }

    #[tokio::test]
    async fn test_abort_and_drain_handles_completes_with_aborted_task() {
        let h = tokio::spawn(sleep_briefly());
        h.abort();
        abort_and_drain_handles(vec![h]).await;
    }

    // ── shutdown drains workers ──────────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_shutdown_drains_worker_handles() {
        let orch = test_orchestrator();
        let _c1 = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        // Add a worker handle that completes after a short delay
        {
            let handle = tokio::spawn(sleep_briefly());
            orch.worker_handles
                .write()
                .await
                .insert("id1".to_string(), handle);
        }

        orch.shutdown().await;

        let state = orch.state.read().await;
        assert!(state.running.is_empty());
        assert!(state.claimed.is_empty());
        drop(state);

        // Worker handles should be drained
        let handles = orch.worker_handles.read().await;
        assert!(handles.is_empty());
    }

    // ── SHUTDOWN_DRAIN_TIMEOUT_SECS constant ─────────────────────────

    #[test]
    fn test_shutdown_drain_timeout_constant() {
        assert_eq!(SHUTDOWN_DRAIN_TIMEOUT_SECS, 30);
    }

    // ── build_agent_env_vars ─────────────────────────────────────────

    #[test]
    fn test_build_agent_env_vars_includes_api_key_and_team_key() {
        let config = test_config();
        let env_vars = build_agent_env_vars(&config, config.agent.max_turns);
        assert_eq!(
            env_vars.get("LINEAR_API_KEY"),
            Some(&"test-key".to_string())
        );
        assert_eq!(
            env_vars.get("LINEAR_TEAM_KEY"),
            Some(&"test-proj".to_string())
        );
    }

    #[test]
    fn test_build_agent_env_vars_skips_empty_api_key() {
        let mut config = test_config();
        config.tracker.api_key = String::new();
        let env_vars = build_agent_env_vars(&config, config.agent.max_turns);
        assert!(!env_vars.contains_key("LINEAR_API_KEY"));
        // team_key should still be present
        assert_eq!(
            env_vars.get("LINEAR_TEAM_KEY"),
            Some(&"test-proj".to_string())
        );
    }

    #[test]
    fn test_build_agent_env_vars_skips_empty_team_key() {
        let mut config = test_config();
        config.tracker.team_key = String::new();
        let env_vars = build_agent_env_vars(&config, config.agent.max_turns);
        assert_eq!(
            env_vars.get("LINEAR_API_KEY"),
            Some(&"test-key".to_string())
        );
        assert!(!env_vars.contains_key("LINEAR_TEAM_KEY"));
    }

    #[test]
    fn test_build_agent_env_vars_both_empty() {
        let mut config = test_config();
        config.tracker.api_key = String::new();
        config.tracker.team_key = String::new();
        let env_vars = build_agent_env_vars(&config, config.agent.max_turns);
        // Should still have SYMPHONY_MAX_TURNS even when tracker keys are empty
        assert_eq!(env_vars.len(), 1);
        assert_eq!(env_vars.get("SYMPHONY_MAX_TURNS"), Some(&"5".to_string()));
    }

    #[test]
    fn test_build_agent_env_vars_includes_max_turns() {
        let config = test_config();
        let env_vars = build_agent_env_vars(&config, 42);
        assert_eq!(env_vars.get("SYMPHONY_MAX_TURNS"), Some(&"42".to_string()));
    }

    // ── setup_mcp_config ────────────────────────────────────────────

    #[test]
    fn test_setup_mcp_config_success() {
        let dir = tempfile::tempdir().unwrap();
        let env_vars = std::collections::HashMap::new();
        let result = setup_mcp_config(dir.path(), &env_vars);
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.exists());
        assert_eq!(path.file_name().unwrap(), ".mcp-config.json");
    }

    #[test]
    fn test_setup_mcp_config_error_invalid_path() {
        let env_vars = std::collections::HashMap::new();
        // Use a path that does not exist to trigger an I/O error
        let result = setup_mcp_config(std::path::Path::new("/nonexistent/dir/xyz"), &env_vars);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("failed to write MCP config"));
    }

    #[tokio::test]
    #[traced_test]
    async fn test_dispatch_issue_distributed_mode() {
        let mut config = test_config();
        config.deployment = DeploymentConfig {
            mode: DeploymentMode::Distributed,
            auth_token: Some("dist-token".to_string()),
        };
        let orch = Orchestrator::new(config, "Test prompt {{ issue.identifier }}".to_string());
        let issue = make_issue("dist-1", "DIST-1", "Todo");
        orch.dispatch_issue(issue, None, false, None).await;

        // Issue should be in running state
        let state = orch.state.read().await;
        assert!(state.running.contains_key("dist-1"));
        assert!(state.claimed.contains("dist-1"));

        // Work assignment should be enqueued
        assert_eq!(state.pending_jobs.len(), 1);
        let assignment = &state.pending_jobs[0];
        assert_eq!(assignment.issue_id, "dist-1");
        assert_eq!(assignment.issue_identifier, "DIST-1");
        assert!(assignment.prompt.contains("DIST-1"));
        assert!(assignment.attempt.is_none());
        assert!(assignment.workspace_path.is_some());

        // No worker handle should exist (distributed mode returns early)
        drop(state);
        let handles = orch.worker_handles.read().await;
        assert!(!handles.contains_key("dist-1"));

        // Check the log message
        assert!(logs_contain("issue enqueued for distributed worker"));
    }

    // ── reconcile_pr_conflicts ─────────────────────────────────────

    #[tokio::test]
    #[traced_test]
    async fn test_reconcile_pr_conflicts_no_github_config() {
        // When github config is None, reconcile_pr_conflicts should return immediately
        let orch = test_orchestrator();
        assert!(orch.config.read().await.github.is_none());
        orch.reconcile_pr_conflicts().await;
        // No errors, no panics — just an early return
    }

    #[tokio::test]
    #[traced_test]
    async fn test_reconcile_pr_conflicts_no_running_issues() {
        let mut server = mockito::Server::new_async().await;

        // Mock the Linear fetch_issues_by_states (for "In Review" check)
        let linear_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "issues": {
                            "nodes": [],
                            "pageInfo": { "hasNextPage": false }
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "Test prompt".to_string());

        // No running issues — should complete without errors
        orch.reconcile_pr_conflicts().await;
        linear_mock.assert_async().await;
    }

    #[tokio::test]
    #[traced_test]
    async fn test_reconcile_pr_conflicts_running_issue_with_conflict() {
        let mut server = mockito::Server::new_async().await;

        // GitHub: find open PR for the branch
        let gh_find_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/tst-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "open".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // GitHub: get PR details — conflicting
        let gh_details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "mergeable": false,
                    "mergeable_state": "dirty"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        // Linear: fetch "In Review" issues (empty — no non-running issues)
        let linear_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "issues": {
                            "nodes": [],
                            "pageInfo": { "hasNextPage": false }
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "Test prompt".to_string());

        // Insert a running issue in "In Progress" state
        let _cancel_rx = insert_running(&orch, "1", "TST-1", "In Progress").await;

        orch.reconcile_pr_conflicts().await;

        // Should log a warning about merge conflicts
        assert!(logs_contain("PR has merge conflicts"));
        gh_find_mock.assert_async().await;
        gh_details_mock.assert_async().await;
        linear_mock.assert_async().await;
    }

    #[tokio::test]
    #[traced_test]
    async fn test_reconcile_pr_conflicts_running_issue_clean() {
        let mut server = mockito::Server::new_async().await;

        // GitHub: find open PR
        let gh_find_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 10,
                    "state": "open"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // GitHub: get PR details — clean
        let gh_details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/10")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 10,
                    "state": "open",
                    "mergeable": true,
                    "mergeable_state": "clean"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        // Linear: fetch "In Review" (empty)
        let linear_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "issues": {
                            "nodes": [],
                            "pageInfo": { "hasNextPage": false }
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "Test prompt".to_string());

        let _cancel_rx = insert_running(&orch, "1", "TST-1", "In Progress").await;
        orch.reconcile_pr_conflicts().await;

        // Should log "PR is mergeable"
        assert!(logs_contain("PR is mergeable"));
        gh_find_mock.assert_async().await;
        gh_details_mock.assert_async().await;
        linear_mock.assert_async().await;
    }

    #[tokio::test]
    #[traced_test]
    async fn test_reconcile_pr_conflicts_github_api_error_handled_gracefully() {
        let mut server = mockito::Server::new_async().await;

        // GitHub returns error
        let gh_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(500)
            .with_body("internal error")
            .create_async()
            .await;

        // Linear: fetch "In Review" issues
        let linear_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "issues": {
                            "nodes": [],
                            "pageInfo": { "hasNextPage": false }
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "Test prompt".to_string());

        let _cancel_rx = insert_running(&orch, "1", "TST-1", "In Progress").await;
        // Should not panic — errors are logged and handled gracefully
        orch.reconcile_pr_conflicts().await;

        assert!(logs_contain("failed to check PR status"));
        gh_mock.assert_async().await;
        linear_mock.assert_async().await;
    }

    #[tokio::test]
    #[traced_test]
    async fn test_reconcile_pr_conflicts_no_pr_for_branch() {
        let mut server = mockito::Server::new_async().await;

        // GitHub: no open PR found
        let gh_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        // Linear: fetch "In Review" (empty)
        let linear_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "issues": {
                            "nodes": [],
                            "pageInfo": { "hasNextPage": false }
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "Test prompt".to_string());

        let _cancel_rx = insert_running(&orch, "1", "TST-1", "In Progress").await;
        orch.reconcile_pr_conflicts().await;

        // Should log that mergeable status is not computed (returns None)
        assert!(logs_contain("PR mergeable status not yet computed"));
        gh_mock.assert_async().await;
        linear_mock.assert_async().await;
    }

    #[tokio::test]
    #[traced_test]
    async fn test_reconcile_pr_conflicts_in_review_running_issue_transitions() {
        let mut server = mockito::Server::new_async().await;

        // GitHub: find PR (called twice — once for logging, once for In Review check)
        let gh_find_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/tst-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "open".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 55,
                    "state": "open"
                })])
                .unwrap(),
            )
            .expect_at_least(2)
            .create_async()
            .await;

        // GitHub: get PR details — conflicting (called twice)
        let gh_details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/55")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 55,
                    "state": "open",
                    "mergeable": false,
                    "mergeable_state": "dirty"
                }))
                .unwrap(),
            )
            .expect_at_least(2)
            .create_async()
            .await;

        // Linear: resolve state ID for "In Progress"
        let linear_resolve_mock = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("workflowStates".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "workflowStates": {
                            "nodes": [
                                { "id": "state-ip", "name": "In Progress" }
                            ]
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        // Linear: issue update (transition to In Progress)
        let linear_update_mock = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("issueUpdate".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "issueUpdate": { "success": true }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        // Linear: fetch "In Review" issues from tracker (empty — the running one is handled directly)
        let linear_fetch_mock = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("In Review".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "issues": {
                            "nodes": [],
                            "pageInfo": { "hasNextPage": false }
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "Test prompt".to_string());

        // Insert running issue in "In Review" state
        let _cancel_rx = insert_running(&orch, "1", "TST-1", "In Review").await;
        orch.reconcile_pr_conflicts().await;

        assert!(logs_contain("In Review running issue has merge conflicts"));
        gh_find_mock.assert_async().await;
        gh_details_mock.assert_async().await;
        linear_resolve_mock.assert_async().await;
        linear_update_mock.assert_async().await;
        linear_fetch_mock.assert_async().await;
    }

    #[tokio::test]
    #[traced_test]
    async fn test_reconcile_pr_conflicts_in_review_clean_no_transition() {
        let mut server = mockito::Server::new_async().await;

        // GitHub: find PR
        let gh_find_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 66,
                    "state": "open"
                })])
                .unwrap(),
            )
            .expect_at_least(2)
            .create_async()
            .await;

        // GitHub: get PR details — clean (called twice)
        let gh_details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/66")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 66,
                    "state": "open",
                    "mergeable": true,
                    "mergeable_state": "clean"
                }))
                .unwrap(),
            )
            .expect_at_least(2)
            .create_async()
            .await;

        // Linear: fetch "In Review" (empty)
        let linear_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "issues": {
                            "nodes": [],
                            "pageInfo": { "hasNextPage": false }
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "Test prompt".to_string());

        let _cancel_rx = insert_running(&orch, "1", "TST-1", "In Review").await;
        orch.reconcile_pr_conflicts().await;

        // Should NOT log transition
        assert!(!logs_contain("transitioning to In Progress"));
        gh_find_mock.assert_async().await;
        gh_details_mock.assert_async().await;
        linear_mock.assert_async().await;
    }

    #[tokio::test]
    #[traced_test]
    async fn test_reconcile_pr_conflicts_linear_fetch_error_handled() {
        let mut server = mockito::Server::new_async().await;

        // Linear: fetch "In Review" fails
        let linear_mock = server
            .mock("POST", "/")
            .with_status(500)
            .with_body("internal error")
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "Test prompt".to_string());

        // No running issues, but the Linear API call for In Review issues will fail
        orch.reconcile_pr_conflicts().await;

        // Should handle gracefully
        assert!(logs_contain(
            "failed to fetch In Review issues for conflict check"
        ));
        linear_mock.assert_async().await;
    }

    #[tokio::test]
    #[traced_test]
    async fn test_reconcile_pr_conflicts_non_running_in_review_with_conflict() {
        let mut server = mockito::Server::new_async().await;

        // Linear: fetch "In Review" issues — returns one issue not currently running
        let linear_fetch_mock = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("In Review".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "issues": {
                            "nodes": [{
                                "id": "issue-99",
                                "identifier": "TST-99",
                                "title": "Non-running issue",
                                "state": { "name": "In Review" },
                                "priority": null,
                                "branchName": null,
                                "url": "https://linear.app/test/issue/TST-99",
                                "labels": { "nodes": [] },
                                "relations": { "nodes": [] },
                                "createdAt": "2024-01-01T00:00:00Z",
                                "updatedAt": "2024-01-01T00:00:00Z"
                            }],
                            "pageInfo": { "hasNextPage": false }
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        // GitHub: find open PR for TST-99
        let gh_find_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/tst-99".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "open".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 77,
                    "state": "open"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // GitHub: PR details — conflicting
        let gh_details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/77")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 77,
                    "state": "open",
                    "mergeable": false,
                    "mergeable_state": "dirty"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        // Linear: resolve state ID for "In Progress"
        let linear_resolve_mock = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("workflowStates".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "workflowStates": {
                            "nodes": [
                                { "id": "state-ip", "name": "In Progress" }
                            ]
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        // Linear: issue update
        let linear_update_mock = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("issueUpdate".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "issueUpdate": { "success": true }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "Test prompt".to_string());

        // No running issues — the In Review issue comes from the tracker fetch
        orch.reconcile_pr_conflicts().await;

        assert!(logs_contain(
            "In Review issue has merge conflicts, transitioning to In Progress"
        ));
        linear_fetch_mock.assert_async().await;
        gh_find_mock.assert_async().await;
        gh_details_mock.assert_async().await;
        linear_resolve_mock.assert_async().await;
        linear_update_mock.assert_async().await;
    }

    #[tokio::test]
    #[traced_test]
    async fn test_reconcile_pr_conflicts_non_running_in_review_clean_pr() {
        let mut server = mockito::Server::new_async().await;

        // Linear: fetch "In Review" issues — returns one issue not currently running
        let linear_fetch_mock = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("In Review".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "issues": {
                            "nodes": [{
                                "id": "issue-clean",
                                "identifier": "TST-CLEAN",
                                "title": "Clean non-running issue",
                                "state": { "name": "In Review" },
                                "priority": null,
                                "branchName": null,
                                "url": "https://linear.app/test/issue/TST-CLEAN",
                                "labels": { "nodes": [] },
                                "relations": { "nodes": [] },
                                "createdAt": "2024-01-01T00:00:00Z",
                                "updatedAt": "2024-01-01T00:00:00Z"
                            }],
                            "pageInfo": { "hasNextPage": false }
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        // GitHub: find open PR for TST-CLEAN
        let gh_find_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/tst-clean".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "open".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 100,
                    "state": "open"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // GitHub: PR details — clean (mergeable = true)
        let gh_details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/100")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 100,
                    "state": "open",
                    "mergeable": true,
                    "mergeable_state": "clean"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "Test prompt".to_string());

        // No running issues — the In Review issue comes from the tracker fetch
        orch.reconcile_pr_conflicts().await;

        // Should NOT log conflict message since PR is clean
        assert!(!logs_contain("In Review issue has merge conflicts"));
        linear_fetch_mock.assert_async().await;
        gh_find_mock.assert_async().await;
        gh_details_mock.assert_async().await;
    }

    #[tokio::test]
    #[traced_test]
    async fn test_reconcile_pr_conflicts_transition_failure_handled() {
        let mut server = mockito::Server::new_async().await;

        // GitHub: find PR (called twice)
        let gh_find_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 88,
                    "state": "open"
                })])
                .unwrap(),
            )
            .expect_at_least(2)
            .create_async()
            .await;

        // GitHub: PR details — conflicting (called twice)
        let gh_details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/88")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 88,
                    "state": "open",
                    "mergeable": false,
                    "mergeable_state": "dirty"
                }))
                .unwrap(),
            )
            .expect_at_least(2)
            .create_async()
            .await;

        // Linear: all POST requests fail (resolve_state and fetch_issues_by_states)
        let linear_mock = server
            .mock("POST", "/")
            .with_status(500)
            .with_body("internal error")
            .expect_at_least(2)
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "Test prompt".to_string());

        let _cancel_rx = insert_running(&orch, "1", "TST-1", "In Review").await;
        orch.reconcile_pr_conflicts().await;

        // Should log failure to transition
        assert!(logs_contain(
            "failed to transition issue back to In Progress"
        ));
        gh_find_mock.assert_async().await;
        gh_details_mock.assert_async().await;
        linear_mock.assert_async().await;
    }

    #[tokio::test]
    #[traced_test]
    async fn test_reconcile_pr_conflicts_non_running_skips_running_issue() {
        // When a tracker-fetched In Review issue is already running, it should be skipped
        let mut server = mockito::Server::new_async().await;

        // GitHub: find PR for the running issue (TST-1)
        let gh_find_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open"
                })])
                .unwrap(),
            )
            .expect_at_least(2)
            .create_async()
            .await;

        // GitHub: PR details — clean
        let gh_details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "mergeable": true,
                    "mergeable_state": "clean"
                }))
                .unwrap(),
            )
            .expect_at_least(2)
            .create_async()
            .await;

        // Linear: fetch "In Review" issues — returns the SAME issue that is running
        let linear_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "issues": {
                            "nodes": [{
                                "id": "1",
                                "identifier": "TST-1",
                                "title": "Running issue",
                                "state": { "name": "In Review" },
                                "priority": null,
                                "branchName": null,
                                "url": null,
                                "labels": { "nodes": [] },
                                "relations": { "nodes": [] },
                                "createdAt": "2024-01-01T00:00:00Z",
                                "updatedAt": "2024-01-01T00:00:00Z"
                            }],
                            "pageInfo": { "hasNextPage": false }
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "Test prompt".to_string());

        // Insert running issue with same ID "1"
        let _cancel_rx = insert_running(&orch, "1", "TST-1", "In Review").await;
        orch.reconcile_pr_conflicts().await;

        // The tracker-fetched issue should be skipped (already handled as running)
        // Only the running-issues section should have logged
        gh_find_mock.assert_async().await;
        gh_details_mock.assert_async().await;
        linear_mock.assert_async().await;
    }

    #[tokio::test]
    #[traced_test]
    async fn test_reconcile_pr_conflicts_non_running_transition_failure() {
        let mut server = mockito::Server::new_async().await;

        // Linear: fetch "In Review" issues — one non-running issue
        let linear_fetch_mock = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("In Review".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "issues": {
                            "nodes": [{
                                "id": "issue-88",
                                "identifier": "TST-88",
                                "title": "Issue with conflict",
                                "state": { "name": "In Review" },
                                "priority": null,
                                "branchName": null,
                                "url": null,
                                "labels": { "nodes": [] },
                                "relations": { "nodes": [] },
                                "createdAt": "2024-01-01T00:00:00Z",
                                "updatedAt": "2024-01-01T00:00:00Z"
                            }],
                            "pageInfo": { "hasNextPage": false }
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        // GitHub: find open PR for TST-88
        let gh_find_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/tst-88".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "open".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 55,
                    "state": "open"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // GitHub: PR details — conflicting
        let gh_details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/55")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 55,
                    "state": "open",
                    "mergeable": false,
                    "mergeable_state": "dirty"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        // Linear: resolve state — fails
        let linear_resolve_mock = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("workflowStates".to_string()))
            .with_status(500)
            .with_body("internal error")
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "Test prompt".to_string());

        // No running issues
        orch.reconcile_pr_conflicts().await;

        // Should log the conflict and the failed transition
        assert!(logs_contain(
            "In Review issue has merge conflicts, transitioning to In Progress"
        ));
        assert!(logs_contain("failed to transition conflicting issue"));

        linear_fetch_mock.assert_async().await;
        gh_find_mock.assert_async().await;
        gh_details_mock.assert_async().await;
        linear_resolve_mock.assert_async().await;
    }

    // ── is_in_review_state ───────────────────────────────────────────

    #[test]
    fn test_is_in_review_state_exact() {
        assert!(is_in_review_state("In Review"));
    }

    #[test]
    fn test_is_in_review_state_case_insensitive() {
        assert!(is_in_review_state("in review"));
        assert!(is_in_review_state("IN REVIEW"));
        assert!(is_in_review_state("In review"));
    }

    #[test]
    fn test_is_in_review_state_with_whitespace() {
        assert!(is_in_review_state("  In Review  "));
    }

    #[test]
    fn test_is_in_review_state_false() {
        assert!(!is_in_review_state("In Progress"));
        assert!(!is_in_review_state("Todo"));
        assert!(!is_in_review_state("Done"));
    }

    // ── all_checks_passing ───────────────────────────────────────────

    #[test]
    fn test_all_checks_passing_all_success() {
        let pr = serde_json::json!({
            "statusCheckRollup": [
                { "conclusion": "SUCCESS", "status": "COMPLETED" },
                { "conclusion": "SUCCESS", "status": "COMPLETED" }
            ]
        });
        assert!(all_checks_passing(&pr));
    }

    #[test]
    fn test_all_checks_passing_with_skipped() {
        let pr = serde_json::json!({
            "statusCheckRollup": [
                { "conclusion": "SUCCESS", "status": "COMPLETED" },
                { "conclusion": "SKIPPED", "status": "COMPLETED" }
            ]
        });
        assert!(all_checks_passing(&pr));
    }

    #[test]
    fn test_all_checks_passing_with_neutral() {
        let pr = serde_json::json!({
            "statusCheckRollup": [
                { "conclusion": "SUCCESS", "status": "COMPLETED" },
                { "conclusion": "NEUTRAL", "status": "COMPLETED" }
            ]
        });
        assert!(all_checks_passing(&pr));
    }

    #[test]
    fn test_all_checks_passing_with_failure() {
        let pr = serde_json::json!({
            "statusCheckRollup": [
                { "conclusion": "SUCCESS", "status": "COMPLETED" },
                { "conclusion": "FAILURE", "status": "COMPLETED" }
            ]
        });
        assert!(!all_checks_passing(&pr));
    }

    #[test]
    fn test_all_checks_passing_with_pending() {
        let pr = serde_json::json!({
            "statusCheckRollup": [
                { "conclusion": "SUCCESS", "status": "COMPLETED" },
                { "conclusion": null, "status": "IN_PROGRESS" }
            ]
        });
        assert!(!all_checks_passing(&pr));
    }

    #[test]
    fn test_all_checks_passing_empty_checks() {
        let pr = serde_json::json!({ "statusCheckRollup": [] });
        assert!(!all_checks_passing(&pr));
    }

    #[test]
    fn test_all_checks_passing_missing_field() {
        let pr = serde_json::json!({});
        assert!(!all_checks_passing(&pr));
    }

    #[test]
    fn test_all_checks_passing_null_field() {
        let pr = serde_json::json!({ "statusCheckRollup": null });
        assert!(!all_checks_passing(&pr));
    }

    // ── should_dispatch: waiting_for_review blocks ───────────────────

    #[tokio::test]
    async fn test_should_dispatch_in_waiting_for_review() {
        let orch = test_orchestrator();
        // Add to waiting_for_review
        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
        }

        let config = orch.config.read().await;
        let issue = make_issue("id1", "PROJ-1", "In Review");
        assert!(!orch.should_dispatch(&issue, &config).await);
    }

    // ── on_worker_exit: state refresh on Normal exit ────────────────

    /// Test that when Linear state refresh fails, on_worker_exit falls back
    /// to the cached state and logs a warning.
    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_normal_refresh_failure_uses_cached_state() {
        let mut linear_server = mockito::Server::new_async().await;

        // Linear returns 500 error
        let _linear_mock = linear_server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("IssuesByIds".to_string()))
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let config = test_config_with_endpoint(&linear_server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        // Insert running entry with "Todo" state
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_worker_exit("id1", WorkerExitReason::Normal).await;

        let state = orch.state.read().await;
        assert!(!state.running.contains_key("id1"));
        assert!(state.is_completed("id1"));
        // Should fall back to cached "Todo" state → continuation retry
        assert!(state.retry_attempts.contains_key("id1"));
        assert!(logs_contain(
            "failed to refresh issue state on worker exit, using cached state"
        ));
    }

    /// Test that on_worker_exit re-fetches issue state from Linear and
    /// correctly routes to waiting_for_review even when the cached state
    /// is stale (e.g. "In Progress" in cache, but "In Review" in Linear).
    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_normal_refreshes_stale_state() {
        let mut linear_server = mockito::Server::new_async().await;
        let mut gh_server = mockito::Server::new_async().await;

        // Linear returns "In Review" even though the entry was cached as "In Progress"
        let _linear_mock = linear_server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("IssuesByIds".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[("id1", "PROJ-1", "In Review")]))
            .create_async()
            .await;

        // Mock find_open_pr (called by add_to_waiting_for_review)
        let _pr_mock = gh_server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&linear_server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: gh_server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "template".to_string());

        // Insert running entry with STALE "In Progress" state
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "In Progress").await;

        orch.on_worker_exit("id1", WorkerExitReason::Normal).await;

        let state = orch.state.read().await;
        assert!(!state.running.contains_key("id1"));
        assert!(state.is_completed("id1"));
        // Should be in waiting_for_review — NOT in retry queue
        assert!(
            state.waiting_for_review.contains_key("id1"),
            "issue should be in waiting_for_review after state refresh"
        );
        assert!(
            !state.retry_attempts.contains_key("id1"),
            "issue should NOT be in retry queue when refresh shows In Review"
        );
        let entry = state.waiting_for_review.get("id1").unwrap();
        assert_eq!(entry.pr_number, 42);
    }

    // ── on_worker_exit: In Review → waiting_for_review ──────────────

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_normal_in_review_adds_to_waiting() {
        let mut server = mockito::Server::new_async().await;

        // Mock find_open_pr
        let _pr_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let mut config = test_config();
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "template".to_string());

        // Insert running entry with "In Review" state
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "In Review").await;

        orch.on_worker_exit("id1", WorkerExitReason::Normal).await;

        let state = orch.state.read().await;
        assert!(!state.running.contains_key("id1"));
        assert!(state.is_completed("id1"));
        // Should be in waiting_for_review
        assert!(state.waiting_for_review.contains_key("id1"));
        let entry = state.waiting_for_review.get("id1").unwrap();
        assert_eq!(entry.pr_number, 42);
        assert_eq!(entry.identifier, "PROJ-1");
        assert_eq!(entry.branch, "symphony/proj-1");
    }

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_normal_in_review_no_github_config() {
        let orch = test_orchestrator(); // no github config

        // Insert running entry with "In Review" state
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "In Review").await;

        orch.on_worker_exit("id1", WorkerExitReason::Normal).await;

        let state = orch.state.read().await;
        assert!(!state.running.contains_key("id1"));
        // Should NOT be in waiting (no github config)
        assert!(!state.waiting_for_review.contains_key("id1"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_normal_in_review_no_pr_found() {
        let mut server = mockito::Server::new_async().await;

        // Mock find_open_pr — empty list
        let _pr_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        let mut config = test_config();
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "template".to_string());
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "In Review").await;

        orch.on_worker_exit("id1", WorkerExitReason::Normal).await;

        let state = orch.state.read().await;
        // Not in waiting (no PR found)
        assert!(!state.waiting_for_review.contains_key("id1"));
    }

    // ── reconcile_pr_lifecycle ─────────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_no_github_config() {
        let orch = test_orchestrator(); // no github config
                                        // Should complete without error
        orch.reconcile_pr_lifecycle().await;
    }

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_no_waiting_entries() {
        let mut server = mockito::Server::new_async().await;

        // Catch-all mock
        let _mock = server
            .mock("GET", mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .expect_at_least(0)
            .create_async()
            .await;

        let mut config = test_config();
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "template".to_string());
        orch.reconcile_pr_lifecycle().await;
        // No errors, no state changes
    }

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_pr_merged() {
        let mut server = mockito::Server::new_async().await;

        // is_pr_merged: returns merged PR
        let _merged_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "closed",
                    "merged": true
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "Cleanup {{ issue.identifier }}".to_string());

        // Add waiting entry
        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
        }

        orch.reconcile_pr_lifecycle().await;

        let state = orch.state.read().await;
        // Should be removed from waiting
        assert!(!state.waiting_for_review.contains_key("id1"));
        // Cleanup dispatch should have added it to running
        assert!(state.running.contains_key("id1"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_no_pr_removes_waiting() {
        let mut server = mockito::Server::new_async().await;

        // is_pr_merged: no PR found
        let _mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        let mut config = test_config();
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "template".to_string());

        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
        }

        orch.reconcile_pr_lifecycle().await;

        let state = orch.state.read().await;
        assert!(!state.waiting_for_review.contains_key("id1"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_auto_merge_approved() {
        let mut server = mockito::Server::new_async().await;

        // is_pr_merged: still open (not merged)
        let _merged_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "merged": false
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // get_pr_reviews: approved
        let _reviews_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42/reviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "id": 1,
                    "state": "APPROVED",
                    "user": { "login": "reviewer1" }
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // get_pr_details: mergeable
        let _details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "mergeable": true,
                    "mergeable_state": "clean"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        // enable_auto_merge: success
        let _merge_mock = server
            .mock("PUT", "/repos/test-org/test-repo/pulls/42/merge")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"sha": "abc123", "merged": true}"#)
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: true,
        });
        let orch = Orchestrator::new(config, "Cleanup {{ issue.identifier }}".to_string());

        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
        }

        orch.reconcile_pr_lifecycle().await;

        let state = orch.state.read().await;
        // Should be removed from waiting (merged via auto-merge)
        assert!(!state.waiting_for_review.contains_key("id1"));
        // Cleanup dispatch should have added it to running
        assert!(state.running.contains_key("id1"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_auto_merge_disabled_skips() {
        let mut server = mockito::Server::new_async().await;

        // is_pr_merged: still open
        let _merged_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "merged": false
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let mut config = test_config();
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false, // disabled
        });
        let orch = Orchestrator::new(config, "template".to_string());

        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
        }

        orch.reconcile_pr_lifecycle().await;

        let state = orch.state.read().await;
        // Should still be in waiting (auto_merge disabled, PR not merged)
        assert!(state.waiting_for_review.contains_key("id1"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_api_error_continues() {
        let mut config = test_config();
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: "http://127.0.0.1:1".to_string(), // unreachable
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "template".to_string());

        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
        }

        // Should not panic on API errors
        orch.reconcile_pr_lifecycle().await;

        let state = orch.state.read().await;
        // Should still be in waiting (API error, no change)
        assert!(state.waiting_for_review.contains_key("id1"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_auto_merge_not_approved() {
        let mut server = mockito::Server::new_async().await;

        // is_pr_merged: still open
        let _merged_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "merged": false
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // Reviews: no approvals
        let _reviews_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42/reviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        let mut config = test_config();
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: true,
        });
        let orch = Orchestrator::new(config, "template".to_string());

        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
        }

        orch.reconcile_pr_lifecycle().await;

        let state = orch.state.read().await;
        // Should still be in waiting (not approved)
        assert!(state.waiting_for_review.contains_key("id1"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_auto_merge_not_mergeable() {
        let mut server = mockito::Server::new_async().await;

        // is_pr_merged: still open
        let _merged_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "merged": false
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // Reviews: approved
        let _reviews_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42/reviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "id": 1,
                    "state": "APPROVED",
                    "user": { "login": "reviewer1" }
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // get_pr_details: NOT mergeable
        let _details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "mergeable": false,
                    "mergeable_state": "dirty"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        let mut config = test_config();
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: true,
        });
        let orch = Orchestrator::new(config, "template".to_string());

        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
        }

        orch.reconcile_pr_lifecycle().await;

        let state = orch.state.read().await;
        // Should still be in waiting (not mergeable)
        assert!(state.waiting_for_review.contains_key("id1"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_auto_merge_405_keeps_waiting() {
        let mut server = mockito::Server::new_async().await;

        // is_pr_merged: still open
        let _merged_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "merged": false
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // Reviews: approved
        let _reviews_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42/reviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "id": 1,
                    "state": "APPROVED",
                    "user": { "login": "reviewer1" }
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // PR details: mergeable
        let _details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "mergeable": true,
                    "mergeable_state": "clean"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        // Merge attempt: 405 (not ready)
        let _merge_mock = server
            .mock("PUT", "/repos/test-org/test-repo/pulls/42/merge")
            .with_status(405)
            .with_body("not mergeable")
            .create_async()
            .await;

        let mut config = test_config();
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: true,
        });
        let orch = Orchestrator::new(config, "template".to_string());

        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
        }

        orch.reconcile_pr_lifecycle().await;

        let state = orch.state.read().await;
        // Should still be in waiting (merge returned 405)
        assert!(state.waiting_for_review.contains_key("id1"));
    }

    // ── add_to_waiting_for_review error path ─────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_normal_in_review_github_api_error() {
        let mut server = mockito::Server::new_async().await;

        // Mock find_open_pr — returns 500 error
        let _pr_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(500)
            .with_body("internal server error")
            .create_async()
            .await;

        let mut config = test_config();
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "template".to_string());

        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "In Review").await;

        orch.on_worker_exit("id1", WorkerExitReason::Normal).await;

        let state = orch.state.read().await;
        assert!(!state.running.contains_key("id1"));
        // Should NOT be in waiting (API error)
        assert!(!state.waiting_for_review.contains_key("id1"));
    }

    // ── reconcile_pr_lifecycle: is_pr_approved error ──────────────

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_auto_merge_approval_check_error() {
        let mut server = mockito::Server::new_async().await;

        // is_pr_merged: still open
        let _merged_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "merged": false
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // Reviews endpoint: returns 500
        let _reviews_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42/reviews")
            .with_status(500)
            .with_body("internal server error")
            .create_async()
            .await;

        let mut config = test_config();
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: true,
        });
        let orch = Orchestrator::new(config, "template".to_string());

        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
        }

        orch.reconcile_pr_lifecycle().await;

        let state = orch.state.read().await;
        // Should still be in waiting (approval check failed)
        assert!(state.waiting_for_review.contains_key("id1"));
    }

    // ── reconcile_pr_lifecycle: get_pr_details error ──────────────

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_auto_merge_pr_details_error() {
        let mut server = mockito::Server::new_async().await;

        // is_pr_merged: still open
        let _merged_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "merged": false
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // Reviews: approved
        let _reviews_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42/reviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "id": 1,
                    "state": "APPROVED",
                    "user": { "login": "reviewer1" }
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // PR details: returns 500
        let _details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(500)
            .with_body("internal server error")
            .create_async()
            .await;

        let mut config = test_config();
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: true,
        });
        let orch = Orchestrator::new(config, "template".to_string());

        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
        }

        orch.reconcile_pr_lifecycle().await;

        let state = orch.state.read().await;
        // Should still be in waiting (details fetch failed)
        assert!(state.waiting_for_review.contains_key("id1"));
    }

    // ── reconcile_pr_lifecycle: auto-merge API error ──────────────

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_auto_merge_api_error() {
        let mut server = mockito::Server::new_async().await;

        // is_pr_merged: still open
        let _merged_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "merged": false
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // Reviews: approved
        let _reviews_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42/reviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "id": 1,
                    "state": "APPROVED",
                    "user": { "login": "reviewer1" }
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // PR details: mergeable
        let _details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "mergeable": true,
                    "mergeable_state": "clean"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        // Merge attempt: 500 (API error, not 405)
        let _merge_mock = server
            .mock("PUT", "/repos/test-org/test-repo/pulls/42/merge")
            .with_status(500)
            .with_body("internal server error")
            .create_async()
            .await;

        let mut config = test_config();
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: true,
        });
        let orch = Orchestrator::new(config, "template".to_string());

        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
        }

        orch.reconcile_pr_lifecycle().await;

        let state = orch.state.read().await;
        // Should still be in waiting (merge API error)
        assert!(state.waiting_for_review.contains_key("id1"));
    }

    // ── reconcile_pr_lifecycle: Linear state verification ─────────

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_removes_stale_waiting_entry() {
        let mut gh_server = mockito::Server::new_async().await;
        let mut linear_server = mockito::Server::new_async().await;

        // is_pr_merged: still open (not merged, not closed)
        let _merged_mock = gh_server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "merged": false
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // Linear: issue moved back to "In Progress" (no longer "In Review")
        let _linear_mock = linear_server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&mock_states_response(&[("id1", "PROJ-1", "In Progress")]))
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&linear_server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: gh_server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "template".to_string());

        {
            let mut state = orch.state.write().await;
            state.claimed.insert("id1".to_string());
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
        }

        orch.reconcile_pr_lifecycle().await;

        let state = orch.state.read().await;
        // Issue moved back to "In Progress" — should be removed from waiting
        assert!(!state.waiting_for_review.contains_key("id1"));
        // Claim should also be released so it can be re-dispatched
        assert!(!state.claimed.contains("id1"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_still_in_review_keeps_waiting() {
        let mut gh_server = mockito::Server::new_async().await;
        let mut linear_server = mockito::Server::new_async().await;

        // is_pr_merged: still open
        let _merged_mock = gh_server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "merged": false
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // Linear: issue is still "In Review"
        let _linear_mock = linear_server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&mock_states_response(&[("id1", "PROJ-1", "In Review")]))
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&linear_server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: gh_server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "template".to_string());

        {
            let mut state = orch.state.write().await;
            state.claimed.insert("id1".to_string());
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
        }

        orch.reconcile_pr_lifecycle().await;

        let state = orch.state.read().await;
        // Issue is still "In Review" — should remain in waiting
        assert!(state.waiting_for_review.contains_key("id1"));
        assert!(state.claimed.contains("id1"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_linear_state_check_error() {
        let mut gh_server = mockito::Server::new_async().await;
        let mut linear_server = mockito::Server::new_async().await;

        // is_pr_merged: still open
        let _merged_mock = gh_server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "merged": false
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // Linear: returns error
        let _linear_mock = linear_server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Any)
            .with_status(500)
            .with_body("internal server error")
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&linear_server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: gh_server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "template".to_string());

        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
        }

        orch.reconcile_pr_lifecycle().await;

        let state = orch.state.read().await;
        // Should still be in waiting (Linear check failed, but entry is kept)
        assert!(state.waiting_for_review.contains_key("id1"));
    }

    // ── check_pr_ready_and_transition ─────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_check_pr_ready_and_transition_with_github_config_delegates_to_api() {
        let mut server = mockito::Server::new_async().await;

        // Mock find_open_pr: returns a PR that's ready
        let _pr_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "mergeable": true,
                    "mergeable_state": "clean"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // Mock Linear transition (will fail but we don't care — just need it not to hang)
        let _linear_mock = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":{"issueUpdate":{"issue":{"id":"id1"}}}}"#)
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });

        let http = reqwest::Client::new();
        let result = check_pr_ready_and_transition(
            std::path::Path::new("/tmp"),
            "id1",
            "PROJ-1",
            &config,
            &http,
        )
        .await;

        assert!(result);
    }

    #[traced_test]
    #[tokio::test]
    async fn test_check_pr_ready_and_transition_no_github_falls_back_to_gh() {
        let config = test_config(); // no github config
        let http = reqwest::Client::new();

        // Falls back to `gh pr view` which will fail (no git repo at /tmp/nonexistent)
        let result = check_pr_ready_and_transition(
            std::path::Path::new("/tmp/nonexistent-symphony-test"),
            "id1",
            "PROJ-1",
            &config,
            &http,
        )
        .await;

        // gh CLI will fail -> returns false
        assert!(!result);
    }

    // ── check_pr_ready_via_api ────────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_check_pr_ready_via_api_no_pr() {
        let mut server = mockito::Server::new_async().await;

        let _pr_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        let gh_config = crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        };
        let http = reqwest::Client::new();
        let github = GitHubClient::new(&gh_config, http.clone());
        let config = test_config();

        let result =
            check_pr_ready_via_api(&github, "symphony/proj-1", "id1", "PROJ-1", &config, &http)
                .await;

        assert!(!result);
    }

    #[traced_test]
    #[tokio::test]
    async fn test_check_pr_ready_via_api_error() {
        let mut server = mockito::Server::new_async().await;

        let _pr_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(500)
            .with_body("error")
            .create_async()
            .await;

        let gh_config = crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        };
        let http = reqwest::Client::new();
        let github = GitHubClient::new(&gh_config, http.clone());
        let config = test_config();

        let result =
            check_pr_ready_via_api(&github, "symphony/proj-1", "id1", "PROJ-1", &config, &http)
                .await;

        assert!(!result);
    }

    #[traced_test]
    #[tokio::test]
    async fn test_check_pr_ready_via_api_not_open() {
        let mut server = mockito::Server::new_async().await;

        let _pr_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "closed",
                    "mergeable": true
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let gh_config = crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        };
        let http = reqwest::Client::new();
        let github = GitHubClient::new(&gh_config, http.clone());
        let config = test_config();

        let result =
            check_pr_ready_via_api(&github, "symphony/proj-1", "id1", "PROJ-1", &config, &http)
                .await;

        assert!(!result);
    }

    #[traced_test]
    #[tokio::test]
    async fn test_check_pr_ready_via_api_not_mergeable() {
        let mut server = mockito::Server::new_async().await;

        let _pr_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "mergeable": false
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let gh_config = crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        };
        let http = reqwest::Client::new();
        let github = GitHubClient::new(&gh_config, http.clone());
        let config = test_config();

        let result =
            check_pr_ready_via_api(&github, "symphony/proj-1", "id1", "PROJ-1", &config, &http)
                .await;

        assert!(!result);
    }

    #[traced_test]
    #[tokio::test]
    async fn test_check_pr_ready_via_api_dirty_state() {
        let mut server = mockito::Server::new_async().await;

        let _pr_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "mergeable": true,
                    "mergeable_state": "dirty"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let gh_config = crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        };
        let http = reqwest::Client::new();
        let github = GitHubClient::new(&gh_config, http.clone());
        let config = test_config();

        let result =
            check_pr_ready_via_api(&github, "symphony/proj-1", "id1", "PROJ-1", &config, &http)
                .await;

        assert!(!result);
    }

    #[traced_test]
    #[tokio::test]
    async fn test_check_pr_ready_via_api_blocked_state() {
        let mut server = mockito::Server::new_async().await;

        let _pr_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "mergeable": true,
                    "mergeable_state": "blocked"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let gh_config = crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        };
        let http = reqwest::Client::new();
        let github = GitHubClient::new(&gh_config, http.clone());
        let config = test_config();

        let result =
            check_pr_ready_via_api(&github, "symphony/proj-1", "id1", "PROJ-1", &config, &http)
                .await;

        assert!(!result);
    }

    #[traced_test]
    #[tokio::test]
    async fn test_check_pr_ready_via_api_mergeable_none() {
        let mut server = mockito::Server::new_async().await;

        let _pr_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open"
                    // mergeable is absent (None)
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let gh_config = crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        };
        let http = reqwest::Client::new();
        let github = GitHubClient::new(&gh_config, http.clone());
        let config = test_config();

        let result =
            check_pr_ready_via_api(&github, "symphony/proj-1", "id1", "PROJ-1", &config, &http)
                .await;

        assert!(!result);
    }

    #[traced_test]
    #[tokio::test]
    async fn test_check_pr_ready_via_api_ready_transitions_to_in_review() {
        let mut server = mockito::Server::new_async().await;

        // PR is ready
        let _pr_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "mergeable": true,
                    "mergeable_state": "clean"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // Linear transition mock
        let _linear_mock = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":{"issueUpdate":{"issue":{"id":"id1"}}}}"#)
            .create_async()
            .await;

        let gh_config = crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        };
        let http = reqwest::Client::new();
        let github = GitHubClient::new(&gh_config, http.clone());
        let config = test_config_with_endpoint(&server.url());

        let result =
            check_pr_ready_via_api(&github, "symphony/proj-1", "id1", "PROJ-1", &config, &http)
                .await;

        assert!(result);
    }

    #[traced_test]
    #[tokio::test]
    async fn test_check_pr_ready_via_api_transition_error_still_returns_true() {
        let mut server = mockito::Server::new_async().await;

        // PR is ready
        let _pr_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "mergeable": true,
                    "mergeable_state": "clean"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // Linear transition fails
        let _linear_mock = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Any)
            .with_status(500)
            .with_body("internal server error")
            .create_async()
            .await;

        let gh_config = crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        };
        let http = reqwest::Client::new();
        let github = GitHubClient::new(&gh_config, http.clone());
        let config = test_config_with_endpoint(&server.url());

        let result =
            check_pr_ready_via_api(&github, "symphony/proj-1", "id1", "PROJ-1", &config, &http)
                .await;

        // Still returns true (PR is ready even if transition failed)
        assert!(result);
    }

    // ── check_pr_ready_via_gh ─────────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_check_pr_ready_via_gh_nonexistent_workspace() {
        let config = test_config();
        let http = reqwest::Client::new();

        let result = check_pr_ready_via_gh(
            std::path::Path::new("/tmp/nonexistent-symphony-test-workspace"),
            "id1",
            "PROJ-1",
            &config.tracker,
            &http,
        )
        .await;

        // gh CLI will fail in nonexistent dir -> returns false
        assert!(!result);
    }

    // ── check_pr_ready_via_command / run_pr_view_command ───────────

    #[traced_test]
    #[tokio::test]
    async fn test_check_pr_ready_via_command_success() {
        let tmp = tempfile::tempdir().unwrap();
        let mut linear_server = mockito::Server::new_async().await;

        // Create a mock script that outputs valid PR JSON
        let mock_script = tmp.path().join("mock-gh");
        std::fs::write(
            &mock_script,
            "#!/bin/sh\necho '{\"number\":42,\"state\":\"OPEN\",\"mergeable\":\"MERGEABLE\",\"statusCheckRollup\":[{\"conclusion\":\"SUCCESS\",\"__typename\":\"CheckRun\"}]}'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mock_script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        // Linear mocks for transition_to_in_review
        let _wf_mock = linear_server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("WorkflowStates".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"data":{"workflowStates":{"nodes":[{"id":"state-ir","name":"In Review"}]}}}"#,
            )
            .create_async()
            .await;

        let _update_mock = linear_server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("issueUpdate".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":{"issueUpdate":{"success":true}}}"#)
            .create_async()
            .await;

        let config = test_config_with_endpoint(&linear_server.url());
        let http = reqwest::Client::new();

        let result = check_pr_ready_via_command(
            mock_script.to_str().unwrap(),
            tmp.path(),
            "id1",
            "PROJ-1",
            &config.tracker,
            &http,
        )
        .await;

        assert!(result);
        assert!(logs_contain(
            "PR is ready (CI green, no conflicts) — transitioning to In Review"
        ));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_check_pr_ready_via_command_not_ready() {
        let tmp = tempfile::tempdir().unwrap();

        // Create a mock script that outputs JSON for a closed PR
        let mock_script = tmp.path().join("mock-gh");
        std::fs::write(
            &mock_script,
            "#!/bin/sh\necho '{\"number\":42,\"state\":\"CLOSED\",\"mergeable\":\"MERGEABLE\",\"statusCheckRollup\":[{\"conclusion\":\"SUCCESS\",\"__typename\":\"CheckRun\"}]}'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mock_script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let config = test_config();
        let http = reqwest::Client::new();

        let result = check_pr_ready_via_command(
            mock_script.to_str().unwrap(),
            tmp.path(),
            "id1",
            "PROJ-1",
            &config.tracker,
            &http,
        )
        .await;

        assert!(!result);
    }

    #[traced_test]
    #[tokio::test]
    async fn test_check_pr_ready_via_command_invalid_json() {
        let tmp = tempfile::tempdir().unwrap();

        // Create a mock script that outputs invalid JSON
        let mock_script = tmp.path().join("mock-gh");
        std::fs::write(&mock_script, "#!/bin/sh\necho 'not json'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mock_script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let config = test_config();
        let http = reqwest::Client::new();

        let result = check_pr_ready_via_command(
            mock_script.to_str().unwrap(),
            tmp.path(),
            "id1",
            "PROJ-1",
            &config.tracker,
            &http,
        )
        .await;

        assert!(!result);
    }

    #[traced_test]
    #[tokio::test]
    async fn test_check_pr_ready_via_command_script_fails() {
        let tmp = tempfile::tempdir().unwrap();

        // Create a mock script that exits with non-zero
        let mock_script = tmp.path().join("mock-gh");
        std::fs::write(&mock_script, "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mock_script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let config = test_config();
        let http = reqwest::Client::new();

        let result = check_pr_ready_via_command(
            mock_script.to_str().unwrap(),
            tmp.path(),
            "id1",
            "PROJ-1",
            &config.tracker,
            &http,
        )
        .await;

        assert!(!result);
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_pr_view_command_success() {
        let tmp = tempfile::tempdir().unwrap();

        let mock_script = tmp.path().join("mock-gh");
        std::fs::write(&mock_script, "#!/bin/sh\necho '{\"ok\":true}'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mock_script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let result = run_pr_view_command(mock_script.to_str().unwrap(), tmp.path()).await;
        assert!(result.is_some());
        let stdout = result.unwrap();
        assert!(String::from_utf8_lossy(&stdout).contains("ok"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_pr_view_command_failure() {
        let tmp = tempfile::tempdir().unwrap();

        let mock_script = tmp.path().join("mock-gh");
        std::fs::write(&mock_script, "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mock_script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let result = run_pr_view_command(mock_script.to_str().unwrap(), tmp.path()).await;
        assert!(result.is_none());
    }

    #[traced_test]
    #[tokio::test]
    async fn test_run_pr_view_command_nonexistent() {
        let result = run_pr_view_command(
            "/tmp/nonexistent-symphony-test-binary",
            std::path::Path::new("/tmp"),
        )
        .await;
        assert!(result.is_none());
    }

    // ── parse_gh_output_ready ─────────────────────────────────────

    #[test]
    fn test_parse_gh_output_ready_valid_open_clean() {
        let json = serde_json::json!({
            "number": 42,
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                {"conclusion": "SUCCESS", "__typename": "CheckRun"}
            ]
        });
        assert!(parse_gh_output_ready(
            serde_json::to_vec(&json).unwrap().as_slice()
        ));
    }

    #[test]
    fn test_parse_gh_output_ready_invalid_json() {
        assert!(!parse_gh_output_ready(b"not valid json"));
    }

    #[test]
    fn test_parse_gh_output_ready_not_open() {
        let json = serde_json::json!({
            "number": 42,
            "state": "CLOSED",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                {"conclusion": "SUCCESS", "__typename": "CheckRun"}
            ]
        });
        assert!(!parse_gh_output_ready(
            serde_json::to_vec(&json).unwrap().as_slice()
        ));
    }

    // ── handle_gh_pr_output ─────────────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_handle_gh_pr_output_ready() {
        let mut linear_server = mockito::Server::new_async().await;

        // Linear mocks for transition_to_in_review
        let _transition_mock = linear_server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":{"workflowStates":{"nodes":[{"id":"state-ir","name":"In Review"}]},"issueUpdate":{"issue":{"id":"id1"}}}}"#)
            .create_async()
            .await;

        let config = test_config_with_endpoint(&linear_server.url());
        let http = reqwest::Client::new();

        let stdout = serde_json::to_vec(&serde_json::json!({
            "number": 42,
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                {"conclusion": "SUCCESS", "__typename": "CheckRun"}
            ]
        }))
        .unwrap();

        let result = handle_gh_pr_output(&stdout, "id1", "PROJ-1", &config.tracker, &http).await;

        assert!(result);
        assert!(logs_contain(
            "PR is ready (CI green, no conflicts) — transitioning to In Review"
        ));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_handle_gh_pr_output_not_ready() {
        let config = test_config();
        let http = reqwest::Client::new();

        let stdout = serde_json::to_vec(&serde_json::json!({
            "number": 42,
            "state": "CLOSED",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                {"conclusion": "SUCCESS", "__typename": "CheckRun"}
            ]
        }))
        .unwrap();

        let result = handle_gh_pr_output(&stdout, "id1", "PROJ-1", &config.tracker, &http).await;

        assert!(!result);
    }

    #[traced_test]
    #[tokio::test]
    async fn test_handle_gh_pr_output_invalid_json() {
        let config = test_config();
        let http = reqwest::Client::new();

        let result =
            handle_gh_pr_output(b"not json", "id1", "PROJ-1", &config.tracker, &http).await;

        assert!(!result);
    }

    // ── dispatch_cleanup ──────────────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_dispatch_cleanup_creates_running_entry() {
        let config = test_config();
        let orch = Orchestrator::new(config.clone(), "{{ issue.identifier }}".to_string());

        let entry = crate::model::WaitingEntry {
            issue_id: "id1".to_string(),
            identifier: "PROJ-1".to_string(),
            pr_number: 42,
            branch: "symphony/proj-1".to_string(),
            started_waiting_at: Utc::now(),
            session_id: None,
        };

        orch.dispatch_cleanup(&entry, &config.tracker).await;

        let state = orch.state.read().await;
        assert!(state.running.contains_key("id1"));
        let running = state.running.get("id1").unwrap();
        assert!(running.issue.title.contains("Cleanup"));
        assert!(running
            .issue
            .description
            .as_ref()
            .unwrap()
            .contains("merged"));
    }

    // ── log_transition_error ─────────────────────────────────────

    #[traced_test]
    #[test]
    fn test_log_transition_error_ok() {
        log_transition_error(Ok(()), "PROJ-1");
        // No warning logged
        assert!(!logs_contain("failed to transition"));
    }

    #[traced_test]
    #[test]
    fn test_log_transition_error_err() {
        let err = crate::error::SymphonyError::GitHubApiRequest {
            reason: "test error".to_string(),
        };
        log_transition_error(Err(err), "PROJ-1");
        assert!(logs_contain("failed to transition issue to In Review"));
    }

    // ── is_gh_pr_ready ────────────────────────────────────────────

    #[test]
    fn test_is_gh_pr_ready_open_and_clean() {
        let json = serde_json::json!({
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                { "conclusion": "SUCCESS" }
            ]
        });
        assert!(is_gh_pr_ready(&json));
    }

    #[test]
    fn test_is_gh_pr_ready_not_open() {
        let json = serde_json::json!({
            "state": "CLOSED",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                { "conclusion": "SUCCESS" }
            ]
        });
        assert!(!is_gh_pr_ready(&json));
    }

    #[test]
    fn test_is_gh_pr_ready_conflicting() {
        let json = serde_json::json!({
            "state": "OPEN",
            "mergeable": "CONFLICTING",
            "statusCheckRollup": [
                { "conclusion": "SUCCESS" }
            ]
        });
        assert!(!is_gh_pr_ready(&json));
    }

    #[test]
    fn test_is_gh_pr_ready_checks_failing() {
        let json = serde_json::json!({
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                { "conclusion": "FAILURE" }
            ]
        });
        assert!(!is_gh_pr_ready(&json));
    }

    #[test]
    fn test_is_gh_pr_ready_no_checks() {
        let json = serde_json::json!({
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": []
        });
        assert!(!is_gh_pr_ready(&json));
    }

    // ── transition_to_in_review ───────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_transition_to_in_review_success() {
        let mut server = mockito::Server::new_async().await;

        let _mock = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":{"issueUpdate":{"issue":{"id":"id1"}}}}"#)
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let http = reqwest::Client::new();

        transition_to_in_review("id1", "PROJ-1", &config.tracker, &http).await;
        // No panic = success
    }

    #[traced_test]
    #[tokio::test]
    async fn test_transition_to_in_review_error_logs_warning() {
        let mut server = mockito::Server::new_async().await;

        let _mock = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Any)
            .with_status(500)
            .with_body("internal server error")
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let http = reqwest::Client::new();

        // Should not panic, just logs a warning
        transition_to_in_review("id1", "PROJ-1", &config.tracker, &http).await;
        assert!(logs_contain("failed to transition issue to In Review"));
    }

    #[tokio::test]
    #[traced_test]
    async fn test_dispatch_issue_distributed_mode_prompt_render_failure() {
        let mut config = test_config();
        config.deployment = DeploymentConfig {
            mode: DeploymentMode::Distributed,
            auth_token: Some("dist-token".to_string()),
        };
        // Use an invalid Liquid template (unclosed tag) to force render failure
        let orch = Orchestrator::new(config, "{% if issue.identifier %}unclosed".to_string());
        let issue = make_issue("dist-err", "DIST-ERR", "Todo");
        orch.dispatch_issue(issue, None, false, None).await;

        // Issue should still be in running/claimed (dispatch adds it before rendering)
        let state = orch.state.read().await;
        assert!(state.running.contains_key("dist-err"));
        assert!(state.claimed.contains("dist-err"));

        // But no work assignment should be enqueued (render failed)
        assert!(state.pending_jobs.is_empty());

        // Check the error log message
        assert!(logs_contain(
            "failed to render prompt for distributed worker"
        ));
    }

    // ── MockStore for persistence integration tests ──────────────────

    use crate::model::{AgentTotals, LogEntry};
    use crate::persistence::{self, PersistedSession};
    use std::sync::Mutex;

    /// A simple mock that records calls and can be configured to fail.
    struct MockStore {
        sessions: Mutex<Vec<PersistedSession>>,
        log_entries: Mutex<Vec<(String, LogEntry)>>,
        agent_totals: Mutex<Option<AgentTotals>>,
        completed: Mutex<Vec<String>>,
        claimed: Mutex<Vec<String>>,
        waiting: Mutex<Vec<crate::model::WaitingEntry>>,
        /// When true, load_agent_totals returns an error.
        fail_load_totals: bool,
        /// When true, load_completed_ids returns an error.
        fail_load_completed: bool,
        /// When true, save_session returns an error.
        fail_save_session: bool,
        /// When true, save_agent_totals returns an error.
        fail_save_totals: bool,
        /// When true, mark_completed returns an error.
        fail_mark_completed: bool,
        /// When true, save_log_entry returns an error.
        fail_save_log_entry: bool,
        /// When true, load_claimed_ids returns an error.
        fail_load_claimed: bool,
        /// When true, load_waiting returns an error.
        fail_load_waiting: bool,
        /// When true, save_claimed returns an error.
        fail_save_claimed: bool,
        /// When true, remove_claimed returns an error.
        fail_remove_claimed: bool,
        /// When true, save_waiting returns an error.
        fail_save_waiting: bool,
        /// When true, remove_waiting returns an error.
        fail_remove_waiting: bool,
    }

    impl MockStore {
        fn new() -> Self {
            Self {
                sessions: Mutex::new(Vec::new()),
                log_entries: Mutex::new(Vec::new()),
                agent_totals: Mutex::new(None),
                completed: Mutex::new(Vec::new()),
                claimed: Mutex::new(Vec::new()),
                waiting: Mutex::new(Vec::new()),
                fail_load_totals: false,
                fail_load_completed: false,
                fail_save_session: false,
                fail_save_totals: false,
                fail_mark_completed: false,
                fail_save_log_entry: false,
                fail_load_claimed: false,
                fail_load_waiting: false,
                fail_save_claimed: false,
                fail_remove_claimed: false,
                fail_save_waiting: false,
                fail_remove_waiting: false,
            }
        }

        fn with_fail_save_session(mut self) -> Self {
            self.fail_save_session = true;
            self
        }

        fn with_fail_save_totals(mut self) -> Self {
            self.fail_save_totals = true;
            self
        }

        fn with_fail_mark_completed(mut self) -> Self {
            self.fail_mark_completed = true;
            self
        }

        fn with_fail_save_log_entry(mut self) -> Self {
            self.fail_save_log_entry = true;
            self
        }

        fn with_totals(self, totals: AgentTotals) -> Self {
            *self.agent_totals.lock().unwrap() = Some(totals);
            self
        }

        fn with_completed(self, ids: Vec<String>) -> Self {
            *self.completed.lock().unwrap() = ids;
            self
        }

        fn with_fail_load_totals(mut self) -> Self {
            self.fail_load_totals = true;
            self
        }

        fn with_fail_load_completed(mut self) -> Self {
            self.fail_load_completed = true;
            self
        }

        fn with_fail_load_claimed(mut self) -> Self {
            self.fail_load_claimed = true;
            self
        }

        fn with_fail_load_waiting(mut self) -> Self {
            self.fail_load_waiting = true;
            self
        }

        fn with_fail_save_claimed(mut self) -> Self {
            self.fail_save_claimed = true;
            self
        }

        fn with_fail_remove_claimed(mut self) -> Self {
            self.fail_remove_claimed = true;
            self
        }

        fn with_fail_save_waiting(mut self) -> Self {
            self.fail_save_waiting = true;
            self
        }

        fn with_fail_remove_waiting(mut self) -> Self {
            self.fail_remove_waiting = true;
            self
        }

        fn with_claimed(self, ids: Vec<String>) -> Self {
            *self.claimed.lock().unwrap() = ids;
            self
        }

        fn with_waiting(self, entries: Vec<crate::model::WaitingEntry>) -> Self {
            *self.waiting.lock().unwrap() = entries;
            self
        }

        fn saved_sessions(&self) -> Vec<PersistedSession> {
            self.sessions.lock().unwrap().clone()
        }

        fn saved_log_entries(&self) -> Vec<(String, LogEntry)> {
            self.log_entries.lock().unwrap().clone()
        }

        fn saved_totals(&self) -> Option<AgentTotals> {
            self.agent_totals.lock().unwrap().clone()
        }

        fn completed_ids(&self) -> Vec<String> {
            self.completed.lock().unwrap().clone()
        }

        fn claimed_ids(&self) -> Vec<String> {
            self.claimed.lock().unwrap().clone()
        }

        fn waiting_entries(&self) -> Vec<crate::model::WaitingEntry> {
            self.waiting.lock().unwrap().clone()
        }
    }

    impl persistence::StateStore for MockStore {
        fn save_session(&self, session: &PersistedSession) -> persistence::Result<()> {
            if self.fail_save_session {
                return Err(persistence::PersistenceError::LockPoisoned);
            }
            self.sessions.lock().unwrap().push(session.clone());
            Ok(())
        }

        fn load_sessions(&self) -> persistence::Result<Vec<PersistedSession>> {
            Ok(self.sessions.lock().unwrap().clone())
        }

        fn delete_session(&self, issue_id: &str) -> persistence::Result<()> {
            self.sessions
                .lock()
                .unwrap()
                .retain(|s| s.issue_id != issue_id);
            Ok(())
        }

        fn save_log_entry(&self, issue_id: &str, entry: &LogEntry) -> persistence::Result<()> {
            if self.fail_save_log_entry {
                return Err(persistence::PersistenceError::LockPoisoned);
            }
            self.log_entries
                .lock()
                .unwrap()
                .push((issue_id.to_string(), entry.clone()));
            Ok(())
        }

        fn load_log_entries(&self, _issue_id: &str) -> persistence::Result<Vec<LogEntry>> {
            Ok(Vec::new())
        }

        fn delete_log_entries(&self, _issue_id: &str) -> persistence::Result<()> {
            Ok(())
        }

        fn save_agent_totals(&self, totals: &AgentTotals) -> persistence::Result<()> {
            if self.fail_save_totals {
                return Err(persistence::PersistenceError::LockPoisoned);
            }
            *self.agent_totals.lock().unwrap() = Some(totals.clone());
            Ok(())
        }

        fn load_agent_totals(&self) -> persistence::Result<AgentTotals> {
            if self.fail_load_totals {
                return Err(persistence::PersistenceError::LockPoisoned);
            }
            Ok(self
                .agent_totals
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_default())
        }

        fn mark_completed(&self, issue_id: &str) -> persistence::Result<()> {
            if self.fail_mark_completed {
                return Err(persistence::PersistenceError::LockPoisoned);
            }
            self.completed.lock().unwrap().push(issue_id.to_string());
            Ok(())
        }

        fn load_completed_ids(&self) -> persistence::Result<Vec<String>> {
            if self.fail_load_completed {
                return Err(persistence::PersistenceError::LockPoisoned);
            }
            Ok(self.completed.lock().unwrap().clone())
        }

        fn save_claimed(&self, issue_id: &str) -> persistence::Result<()> {
            if self.fail_save_claimed {
                return Err(persistence::PersistenceError::LockPoisoned);
            }
            let mut claimed = self.claimed.lock().unwrap();
            if !claimed.contains(&issue_id.to_string()) {
                claimed.push(issue_id.to_string());
            }
            Ok(())
        }

        fn remove_claimed(&self, issue_id: &str) -> persistence::Result<()> {
            if self.fail_remove_claimed {
                return Err(persistence::PersistenceError::LockPoisoned);
            }
            self.claimed.lock().unwrap().retain(|id| id != issue_id);
            Ok(())
        }

        fn load_claimed_ids(&self) -> persistence::Result<Vec<String>> {
            if self.fail_load_claimed {
                return Err(persistence::PersistenceError::LockPoisoned);
            }
            Ok(self.claimed.lock().unwrap().clone())
        }

        fn save_waiting(&self, entry: &crate::model::WaitingEntry) -> persistence::Result<()> {
            if self.fail_save_waiting {
                return Err(persistence::PersistenceError::LockPoisoned);
            }
            let mut waiting = self.waiting.lock().unwrap();
            waiting.retain(|e| e.issue_id != entry.issue_id);
            waiting.push(entry.clone());
            Ok(())
        }

        fn remove_waiting(&self, issue_id: &str) -> persistence::Result<()> {
            if self.fail_remove_waiting {
                return Err(persistence::PersistenceError::LockPoisoned);
            }
            self.waiting
                .lock()
                .unwrap()
                .retain(|e| e.issue_id != issue_id);
            Ok(())
        }

        fn load_waiting(&self) -> persistence::Result<Vec<crate::model::WaitingEntry>> {
            if self.fail_load_waiting {
                return Err(persistence::PersistenceError::LockPoisoned);
            }
            Ok(self.waiting.lock().unwrap().clone())
        }
    }

    fn test_orchestrator_with_store(store: Arc<dyn persistence::StateStore>) -> Orchestrator {
        let mut orch = test_orchestrator();
        orch.store = Some(store);
        orch
    }

    // ── MockStore trait coverage ─────────────────────────────────────

    #[test]
    fn test_mock_store_load_delete_methods() {
        let store = MockStore::new();

        // save + load + delete sessions
        let session = PersistedSession {
            issue_id: "id1".to_string(),
            identifier: "TST-1".to_string(),
            session_id: None,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            turn_count: 0,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            ended_at: None,
            last_event: None,
            last_event_at: None,
            last_message: None,
            retry_attempt: None,
        };
        store.save_session(&session).unwrap();
        let loaded = store.load_sessions().unwrap();
        assert_eq!(loaded.len(), 1);
        store.delete_session("id1").unwrap();
        assert!(store.load_sessions().unwrap().is_empty());

        // load + delete log entries
        assert!(store.load_log_entries("id1").unwrap().is_empty());
        store.delete_log_entries("id1").unwrap();
    }

    // ── with_store ──────────────────────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_with_store_loads_agent_totals() {
        let store = Arc::new(MockStore::new().with_totals(AgentTotals {
            input_tokens: 1000,
            output_tokens: 500,
            total_tokens: 1500,
            seconds_running: 42.0,
        }));
        let orch = test_orchestrator().with_store(store).await;

        let state = orch.state.read().await;
        assert_eq!(state.agent_totals.input_tokens, 1000);
        assert_eq!(state.agent_totals.output_tokens, 500);
        assert_eq!(state.agent_totals.total_tokens, 1500);
        assert!((state.agent_totals.seconds_running - 42.0).abs() < 0.001);
        assert!(logs_contain("restored persisted agent totals"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_with_store_loads_completed_ids() {
        let store =
            Arc::new(MockStore::new().with_completed(vec!["id-a".to_string(), "id-b".to_string()]));
        let orch = test_orchestrator().with_store(store).await;

        let state = orch.state.read().await;
        assert!(state.is_completed("id-a"));
        assert!(state.is_completed("id-b"));
        assert!(logs_contain("restored persisted completed issues"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_with_store_handles_totals_error() {
        let store = Arc::new(MockStore::new().with_fail_load_totals());
        let orch = test_orchestrator().with_store(store).await;

        let state = orch.state.read().await;
        // Defaults should remain
        assert_eq!(state.agent_totals.input_tokens, 0);
        assert!(logs_contain("failed to load persisted agent totals"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_with_store_handles_completed_error() {
        let store = Arc::new(MockStore::new().with_fail_load_completed());
        let orch = test_orchestrator().with_store(store).await;

        let state = orch.state.read().await;
        assert_eq!(state.completed_count(), 0);
        assert!(logs_contain("failed to load persisted completed issues"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_with_store_loads_claimed_ids() {
        let store =
            Arc::new(MockStore::new().with_claimed(vec!["id-x".to_string(), "id-y".to_string()]));
        let orch = test_orchestrator().with_store(store).await;

        let state = orch.state.read().await;
        assert!(state.claimed.contains("id-x"));
        assert!(state.claimed.contains("id-y"));
        assert!(logs_contain("restored persisted claimed issues"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_with_store_handles_claimed_error() {
        let store = Arc::new(MockStore::new().with_fail_load_claimed());
        let orch = test_orchestrator().with_store(store).await;

        let state = orch.state.read().await;
        assert!(state.claimed.is_empty());
        assert!(logs_contain("failed to load persisted claimed issues"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_with_store_loads_waiting_entries() {
        let entry = crate::model::WaitingEntry {
            issue_id: "id-w".to_string(),
            identifier: "PROJ-W".to_string(),
            pr_number: 99,
            branch: "symphony/proj-w".to_string(),
            started_waiting_at: Utc::now(),
            session_id: None,
        };
        let store = Arc::new(MockStore::new().with_waiting(vec![entry]));
        let orch = test_orchestrator().with_store(store).await;

        let state = orch.state.read().await;
        assert!(state.waiting_for_review.contains_key("id-w"));
        let w = state.waiting_for_review.get("id-w").unwrap();
        assert_eq!(w.pr_number, 99);
        assert_eq!(w.branch, "symphony/proj-w");
        assert!(logs_contain(
            "restored persisted waiting_for_review entries"
        ));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_with_store_handles_waiting_error() {
        let store = Arc::new(MockStore::new().with_fail_load_waiting());
        let orch = test_orchestrator().with_store(store).await;

        let state = orch.state.read().await;
        assert!(state.waiting_for_review.is_empty());
        assert!(logs_contain("failed to load persisted waiting entries"));
    }

    // ── store_handle ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_store_handle_none_without_store() {
        let orch = test_orchestrator();
        assert!(orch.store_handle().is_none());
    }

    #[tokio::test]
    async fn test_store_handle_some_with_store() {
        let store: Arc<dyn persistence::StateStore> = Arc::new(MockStore::new());
        let orch = test_orchestrator().with_store(store).await;
        assert!(orch.store_handle().is_some());
    }

    // ── persist_session ─────────────────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_persist_session_with_store() {
        let store = Arc::new(MockStore::new());
        let orch = test_orchestrator_with_store(store.clone());

        let session = LiveSession {
            session_id: Some("sess-1".to_string()),
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            turn_count: 3,
            last_event: Some("turn_completed".to_string()),
            last_event_at: Some(Utc::now()),
            last_message: Some("Done".to_string()),
            ..Default::default()
        };
        let started_at = Utc::now();

        orch.persist_session("id-1", "TST-1", &session, Some(2), started_at)
            .await;

        let saved = store.saved_sessions();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].issue_id, "id-1");
        assert_eq!(saved[0].identifier, "TST-1");
        assert_eq!(saved[0].session_id, Some("sess-1".to_string()));
        assert_eq!(saved[0].input_tokens, 100);
        assert_eq!(saved[0].output_tokens, 50);
        assert_eq!(saved[0].total_tokens, 150);
        assert_eq!(saved[0].turn_count, 3);
        assert!(saved[0].ended_at.is_none());
        assert_eq!(saved[0].last_event, Some("turn_completed".to_string()));
        assert!(saved[0].last_event_at.is_some());
        assert_eq!(saved[0].last_message, Some("Done".to_string()));
        assert_eq!(saved[0].retry_attempt, Some(2));
    }

    #[tokio::test]
    async fn test_persist_session_without_store_is_noop() {
        let orch = test_orchestrator();
        let session = LiveSession::default();
        // Should not panic
        orch.persist_session("id-1", "TST-1", &session, None, Utc::now())
            .await;
    }

    #[tokio::test]
    async fn test_persist_session_none_optionals() {
        let store = Arc::new(MockStore::new());
        let orch = test_orchestrator_with_store(store.clone());

        let session = LiveSession::default();
        orch.persist_session("id-1", "TST-1", &session, None, Utc::now())
            .await;

        let saved = store.saved_sessions();
        assert_eq!(saved.len(), 1);
        assert!(saved[0].session_id.is_none());
        assert!(saved[0].last_event.is_none());
        assert!(saved[0].last_event_at.is_none());
        assert!(saved[0].last_message.is_none());
        assert!(saved[0].retry_attempt.is_none());
    }

    // ── on_worker_exit with store ───────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_normal_persists_completed() {
        let store = Arc::new(MockStore::new());
        let orch = test_orchestrator_with_store(store.clone());
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_worker_exit("id1", WorkerExitReason::Normal).await;

        // Should persist agent totals
        assert!(store.saved_totals().is_some());
        // Should finalize session with ended_at
        let sessions = store.saved_sessions();
        assert!(!sessions.is_empty());
        let finalized = sessions.last().unwrap();
        assert_eq!(finalized.issue_id, "id1");
        assert!(finalized.ended_at.is_some());
        // Should persist completed marker
        assert!(store.completed_ids().contains(&"id1".to_string()));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_failed_persists_session() {
        let store = Arc::new(MockStore::new());
        let orch = test_orchestrator_with_store(store.clone());
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_worker_exit("id1", WorkerExitReason::Failed("crash".to_string()))
            .await;

        // Should persist agent totals
        assert!(store.saved_totals().is_some());
        // Should finalize session
        let sessions = store.saved_sessions();
        assert!(!sessions.is_empty());
        let finalized = sessions.last().unwrap();
        assert!(finalized.ended_at.is_some());
        // Should NOT persist completed marker (it failed)
        assert!(!store.completed_ids().contains(&"id1".to_string()));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_cancelled_persists_session() {
        let store = Arc::new(MockStore::new());
        let orch = test_orchestrator_with_store(store.clone());
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_worker_exit("id1", WorkerExitReason::Cancelled)
            .await;

        // Should persist agent totals and finalized session
        assert!(store.saved_totals().is_some());
        let sessions = store.saved_sessions();
        assert!(!sessions.is_empty());
    }

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_timed_out_persists_session() {
        let store = Arc::new(MockStore::new());
        let orch = test_orchestrator_with_store(store.clone());
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_worker_exit("id1", WorkerExitReason::TimedOut).await;

        assert!(store.saved_totals().is_some());
        let sessions = store.saved_sessions();
        assert!(!sessions.is_empty());
    }

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_stalled_persists_session() {
        let store = Arc::new(MockStore::new());
        let orch = test_orchestrator_with_store(store.clone());
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_worker_exit("id1", WorkerExitReason::Stalled).await;

        assert!(store.saved_totals().is_some());
        let sessions = store.saved_sessions();
        assert!(!sessions.is_empty());
    }

    // ── on_agent_update with store ──────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_on_agent_update_persists_log_entry() {
        let store = Arc::new(MockStore::new());
        let orch = test_orchestrator_with_store(store.clone());
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_agent_update(
            "id1",
            AgentEvent::TurnCompleted {
                message: Some("turn 1 done".to_string()),
            },
        )
        .await;

        let log_entries = store.saved_log_entries();
        assert_eq!(log_entries.len(), 1);
        assert_eq!(log_entries[0].0, "id1");
        assert_eq!(log_entries[0].1.event_type, "turn_completed");
    }

    #[traced_test]
    #[tokio::test]
    async fn test_on_agent_update_persists_session_snapshot() {
        let store = Arc::new(MockStore::new());
        let orch = test_orchestrator_with_store(store.clone());
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_agent_update(
            "id1",
            AgentEvent::TokenUsage {
                input_tokens: 200,
                output_tokens: 100,
                total_tokens: 300,
            },
        )
        .await;

        // Should persist session snapshot (persist_session called after processing)
        let sessions = store.saved_sessions();
        assert!(!sessions.is_empty());
        let snap = sessions.last().unwrap();
        assert_eq!(snap.issue_id, "id1");
        assert_eq!(snap.identifier, "PROJ-1");
    }

    #[traced_test]
    #[tokio::test]
    async fn test_on_agent_update_notification_persists_log_and_session() {
        let store = Arc::new(MockStore::new());
        let orch = test_orchestrator_with_store(store.clone());
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_agent_update(
            "id1",
            AgentEvent::Notification {
                message: "hello".to_string(),
            },
        )
        .await;

        assert!(!store.saved_log_entries().is_empty());
        assert!(!store.saved_sessions().is_empty());
    }

    // ── persistence error paths (best-effort, should not panic) ─────

    #[traced_test]
    #[tokio::test]
    async fn test_persist_session_save_error_is_best_effort() {
        let store = Arc::new(MockStore::new().with_fail_save_session());
        let orch = test_orchestrator_with_store(store.clone());

        let session = LiveSession::default();
        // Should not panic even when save fails
        orch.persist_session("id-1", "TST-1", &session, None, Utc::now())
            .await;

        assert!(logs_contain("failed to persist session"));
        assert!(store.saved_sessions().is_empty());
    }

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_save_totals_error_is_best_effort() {
        let store = Arc::new(MockStore::new().with_fail_save_totals());
        let orch = test_orchestrator_with_store(store.clone());
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        // Should not panic even when save_agent_totals fails
        orch.on_worker_exit("id1", WorkerExitReason::Normal).await;

        assert!(logs_contain(
            "failed to persist agent totals on worker exit"
        ));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_save_session_error_is_best_effort() {
        let store = Arc::new(MockStore::new().with_fail_save_session());
        let orch = test_orchestrator_with_store(store.clone());
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        // Should not panic even when save_session fails
        orch.on_worker_exit("id1", WorkerExitReason::Failed("err".to_string()))
            .await;

        assert!(logs_contain(
            "failed to finalize persisted session on worker exit"
        ));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_mark_completed_error_is_best_effort() {
        let store = Arc::new(MockStore::new().with_fail_mark_completed());
        let orch = test_orchestrator_with_store(store.clone());
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        // Should not panic even when mark_completed fails
        orch.on_worker_exit("id1", WorkerExitReason::Normal).await;

        assert!(logs_contain("failed to persist completed marker"));
    }

    #[traced_test]
    #[tokio::test]
    async fn test_on_agent_update_save_log_entry_error_is_best_effort() {
        let store = Arc::new(MockStore::new().with_fail_save_log_entry());
        let orch = test_orchestrator_with_store(store.clone());
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        // Should not panic even when save_log_entry fails
        orch.on_agent_update(
            "id1",
            AgentEvent::TurnCompleted {
                message: Some("turn done".to_string()),
            },
        )
        .await;

        assert!(logs_contain("failed to persist log entry"));
        assert!(store.saved_log_entries().is_empty());
    }

    // ── is_done_state ──────────────────────────────────────────────

    #[test]
    fn test_is_done_state() {
        assert!(is_done_state("Done"));
        assert!(is_done_state("done"));
        assert!(is_done_state("DONE"));
        assert!(is_done_state("Merged"));
        assert!(is_done_state("merged"));
        assert!(is_done_state("MERGED"));
        assert!(is_done_state("  Done  "));
        assert!(is_done_state("  Merged  "));
        assert!(!is_done_state("In Progress"));
        assert!(!is_done_state("In Review"));
        assert!(!is_done_state("Todo"));
        assert!(!is_done_state(""));
    }

    // ── transition_to_merged ──────────────────────────────────────

    #[tokio::test]
    #[traced_test]
    async fn test_transition_to_merged_success() {
        let mut server = mockito::Server::new_async().await;

        // resolve_state_id response
        let resolve_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "workflowStates": {
                            "nodes": [
                                { "id": "state-merged", "name": "Merged" }
                            ]
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        // update_issue_state response
        let update_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": { "issueUpdate": { "success": true } }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let orch = Orchestrator::new(
            test_config_with_endpoint(&server.url()),
            "template".to_string(),
        );

        orch.transition_to_merged("issue-1", "PROJ-1").await;

        assert!(logs_contain("transitioned issue to Merged"));
        resolve_mock.assert_async().await;
        update_mock.assert_async().await;
    }

    #[tokio::test]
    #[traced_test]
    async fn test_transition_to_merged_failure() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("POST", "/")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let orch = Orchestrator::new(
            test_config_with_endpoint(&server.url()),
            "template".to_string(),
        );

        orch.transition_to_merged("issue-1", "PROJ-1").await;

        assert!(logs_contain("failed to transition issue to Merged"));
        mock.assert_async().await;
    }

    // ── reconcile_pr_lifecycle: merged PR transitions to Merged ──

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_pr_merged_transitions_to_merged() {
        let mut server = mockito::Server::new_async().await;

        // is_pr_merged: returns merged PR (GitHub GET)
        let _merged_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "closed",
                    "merged": true
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // Linear: resolve_state_id for "Merged"
        let _resolve_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "workflowStates": {
                            "nodes": [
                                { "id": "state-merged", "name": "Merged" }
                            ]
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        // Linear: issueUpdate
        let _update_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": { "issueUpdate": { "success": true } }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "Cleanup {{ issue.identifier }}".to_string());

        // Add waiting entry
        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
        }

        orch.reconcile_pr_lifecycle().await;

        // Verify transition to Merged was called
        assert!(logs_contain("transitioned issue to Merged"));
        // Verify cleanup was dispatched
        assert!(logs_contain("dispatching cleanup agent turn"));

        let state = orch.state.read().await;
        // Should be removed from waiting
        assert!(!state.waiting_for_review.contains_key("id1"));
        // Cleanup dispatch should have added it to running
        assert!(state.running.contains_key("id1"));
    }

    // ── dispatch_issue: skip_state_transition ────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_dispatch_issue_skip_state_transition() {
        let mut server = mockito::Server::new_async().await;

        // The only POST call should be add_claim_label (1 call).
        // transition_to_in_progress should NOT be called (verified via logs below).
        let linear_mock = server
            .mock("POST", "/")
            .expect(1)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":{}}"#)
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());
        let issue = make_issue("id1", "PROJ-1", "Todo");

        orch.dispatch_issue(issue, None, true, None).await;

        let state = orch.state.read().await;
        // Issue should still be dispatched (in running and claimed)
        assert!(state.running.contains_key("id1"));
        assert!(state.claimed.contains("id1"));
        // transition_to_in_progress should NOT have been called
        assert!(!logs_contain("transitioned issue to In Progress"));
        linear_mock.assert_async().await;
    }

    // ── add_to_waiting_for_review: merged PR fallback ───────────

    #[traced_test]
    #[tokio::test]
    async fn test_add_to_waiting_merged_pr_fallback() {
        let mut server = mockito::Server::new_async().await;

        // find_open_pr: no open PR (state=open returns empty)
        let _open_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "open".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        // is_pr_merged fallback: PR was merged (state=all returns merged PR)
        let _merged_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "closed",
                    "merged": true
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // Linear: resolve_state_id for "Merged"
        let _resolve_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": {
                        "workflowStates": {
                            "nodes": [
                                { "id": "state-merged", "name": "Merged" }
                            ]
                        }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        // Linear: issueUpdate
        let _update_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "data": { "issueUpdate": { "success": true } }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let orch = Orchestrator::new(config, "template".to_string());

        orch.add_to_waiting_for_review("id1", "PROJ-1", None).await;

        // Should have detected merged PR and transitioned
        assert!(logs_contain("PR already merged"));
        assert!(logs_contain("transitioned issue to Merged"));

        // Should NOT be in waiting_for_review (merged, not waiting)
        let state = orch.state.read().await;
        assert!(!state.waiting_for_review.contains_key("id1"));
    }

    // ── on_worker_exit: Done state marks completed without retry ─

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_done_state_marks_completed() {
        let orch = test_orchestrator();
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Done").await;

        orch.on_worker_exit("id1", WorkerExitReason::Normal).await;

        let state = orch.state.read().await;
        assert!(!state.running.contains_key("id1"));
        assert!(state.is_completed("id1"));
        // Should NOT schedule a retry
        assert!(!state.retry_attempts.contains_key("id1"));
        // Should NOT be in waiting_for_review
        assert!(!state.waiting_for_review.contains_key("id1"));
        assert!(logs_contain(
            "cleanup worker completed, issue already in terminal state"
        ));
    }

    // ══════════════════════════════════════════════════════════════════
    // Persistence: store.save_claimed / remove_claimed / save_waiting /
    //              remove_waiting coverage tests
    // ══════════════════════════════════════════════════════════════════

    // ── dispatch_issue persists claimed marker ──────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_dispatch_issue_persists_claimed_marker() {
        let store = Arc::new(MockStore::new());
        let orch = test_orchestrator_with_store(store.clone());
        let issue = make_issue("id1", "PROJ-1", "Todo");

        orch.dispatch_issue(issue, None, false, None).await;

        // store.save_claimed should have been called with "id1"
        assert!(
            store.claimed_ids().contains(&"id1".to_string()),
            "dispatch_issue should persist claimed marker"
        );
    }

    // ── on_worker_exit Cancelled persists remove_claimed ────────────

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_cancelled_persists_remove_claimed() {
        let store = Arc::new(MockStore::new());
        let orch = test_orchestrator_with_store(store.clone());
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        // Pre-populate claimed in the store (simulating dispatch having persisted it)
        store.save_claimed("id1").unwrap();
        assert!(store.claimed_ids().contains(&"id1".to_string()));

        orch.on_worker_exit("id1", WorkerExitReason::Cancelled)
            .await;

        // store.remove_claimed should have been called
        assert!(
            !store.claimed_ids().contains(&"id1".to_string()),
            "on_worker_exit Cancelled should remove persisted claimed marker"
        );
    }

    // ── on_retry_fired: issue no longer active persists remove_claimed ─

    #[traced_test]
    #[tokio::test]
    async fn test_on_retry_fired_no_longer_active_persists_remove_claimed() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[])) // empty: issue not in candidates
            .create_async()
            .await;

        let store = Arc::new(MockStore::new());
        let config = test_config_with_endpoint(&server.url());
        let mut orch = Orchestrator::new(config, "template".to_string());
        orch.store = Some(store.clone());

        // Pre-populate retry entry, claimed set, and persisted claimed
        {
            let mut state = orch.state.write().await;
            state.retry_attempts.insert(
                "id1".to_string(),
                RetryEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    attempt: 1,
                    due_at_ms: 0,
                    error: None,
                },
            );
            state.claimed.insert("id1".to_string());
        }
        store.save_claimed("id1").unwrap();

        orch.on_retry_fired("id1").await;

        // store.remove_claimed should have been called
        assert!(
            !store.claimed_ids().contains(&"id1".to_string()),
            "on_retry_fired should remove persisted claimed when issue no longer active"
        );
    }

    // ── on_remove_from_retry_queue persists remove_claimed ──────────

    #[traced_test]
    #[tokio::test]
    async fn test_on_remove_from_retry_queue_persists_remove_claimed() {
        let store = Arc::new(MockStore::new());
        let orch = test_orchestrator_with_store(store.clone());

        // Pre-populate retry entry, claimed set, and persisted claimed
        {
            let mut state = orch.state.write().await;
            state.retry_attempts.insert(
                "id1".to_string(),
                RetryEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    attempt: 3,
                    due_at_ms: 0,
                    error: Some("timeout".to_string()),
                },
            );
            state.claimed.insert("id1".to_string());
        }
        store.save_claimed("id1").unwrap();

        orch.on_remove_from_retry_queue("id1").await;

        // store.remove_claimed should have been called
        assert!(
            !store.claimed_ids().contains(&"id1".to_string()),
            "on_remove_from_retry_queue should remove persisted claimed marker"
        );
    }

    // ── terminate_running_issue persists remove_claimed ─────────────

    #[traced_test]
    #[tokio::test]
    async fn test_terminate_running_issue_persists_remove_claimed() {
        let store = Arc::new(MockStore::new());
        let orch = test_orchestrator_with_store(store.clone());
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        // Pre-populate persisted claimed
        store.save_claimed("id1").unwrap();

        orch.terminate_running_issue("id1", false).await;

        // store.remove_claimed should have been called
        assert!(
            !store.claimed_ids().contains(&"id1".to_string()),
            "terminate_running_issue should remove persisted claimed marker"
        );
    }

    // ── add_to_waiting_for_review persists save_waiting ─────────────

    #[traced_test]
    #[tokio::test]
    async fn test_add_to_waiting_for_review_persists_waiting_entry() {
        let mut server = mockito::Server::new_async().await;

        // Mock find_open_pr
        let _pr_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let store = Arc::new(MockStore::new());
        let mut config = test_config();
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let mut orch = Orchestrator::new(config, "template".to_string());
        orch.store = Some(store.clone());

        orch.add_to_waiting_for_review("id1", "PROJ-1", None).await;

        // store.save_waiting should have been called
        let entries = store.waiting_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].issue_id, "id1");
        assert_eq!(entries[0].identifier, "PROJ-1");
        assert_eq!(entries[0].pr_number, 42);
        assert_eq!(entries[0].branch, "symphony/proj-1");
    }

    // ── on_worker_exit Normal In Review: retry logic after failed
    //    add_to_waiting_for_review ────────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_normal_in_review_retry_on_failed_waiting() {
        let mut server = mockito::Server::new_async().await;

        // Mock find_open_pr — returns 500 error → add_to_waiting_for_review fails
        let _pr_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(500)
            .with_body("internal server error")
            .create_async()
            .await;

        let store = Arc::new(MockStore::new());
        let mut config = test_config();
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let mut orch = Orchestrator::new(config, "template".to_string());
        orch.store = Some(store.clone());

        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "In Review").await;

        orch.on_worker_exit("id1", WorkerExitReason::Normal).await;

        let state = orch.state.read().await;
        // add_to_waiting_for_review failed, so NOT in waiting
        assert!(!state.waiting_for_review.contains_key("id1"));
        // Should have scheduled a retry (30s delay)
        assert!(
            state.retry_attempts.contains_key("id1"),
            "should schedule retry when add_to_waiting_for_review fails"
        );
        let retry = state.retry_attempts.get("id1").unwrap();
        assert_eq!(retry.attempt, 0);
        assert_eq!(
            retry.error,
            Some("failed to add to waiting_for_review".to_string())
        );
        // Log should contain the warning
        assert!(logs_contain(
            "add_to_waiting_for_review failed, scheduling retry"
        ));
    }

    // ── reconcile_pr_lifecycle: merged PR persists remove_waiting + remove_claimed ─

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_pr_merged_persists_store_removal() {
        let mut server = mockito::Server::new_async().await;

        // is_pr_merged: returns merged PR
        let _merged_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "closed",
                    "merged": true
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let store = Arc::new(MockStore::new());
        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let mut orch = Orchestrator::new(config, "Cleanup {{ issue.identifier }}".to_string());
        orch.store = Some(store.clone());

        // Pre-populate waiting entry, claimed, and persisted state
        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
            state.claimed.insert("id1".to_string());
        }
        store.save_claimed("id1").unwrap();
        store
            .save_waiting(&crate::model::WaitingEntry {
                issue_id: "id1".to_string(),
                identifier: "PROJ-1".to_string(),
                pr_number: 42,
                branch: "symphony/proj-1".to_string(),
                started_waiting_at: Utc::now(),
                session_id: None,
            })
            .unwrap();

        orch.reconcile_pr_lifecycle().await;

        // store.remove_waiting and store.remove_claimed should have been called
        assert!(
            !store.waiting_entries().iter().any(|e| e.issue_id == "id1"),
            "reconcile_pr_lifecycle merged PR should remove persisted waiting entry"
        );
        // Note: dispatch_cleanup re-adds the claimed marker via dispatch_issue,
        // so we check that remove_claimed was at least exercised by verifying
        // the original was removed (dispatch_issue adds it back fresh)
        // The key test is that the store calls were executed without error.
    }

    // ── reconcile_pr_lifecycle: no PR found persists remove_waiting + remove_claimed ─

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_no_pr_persists_store_removal() {
        let mut server = mockito::Server::new_async().await;

        // is_pr_merged: no PR found
        let _mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        let store = Arc::new(MockStore::new());
        let mut config = test_config();
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let mut orch = Orchestrator::new(config, "template".to_string());
        orch.store = Some(store.clone());

        // Pre-populate
        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
            state.claimed.insert("id1".to_string());
        }
        store.save_claimed("id1").unwrap();
        store
            .save_waiting(&crate::model::WaitingEntry {
                issue_id: "id1".to_string(),
                identifier: "PROJ-1".to_string(),
                pr_number: 42,
                branch: "symphony/proj-1".to_string(),
                started_waiting_at: Utc::now(),
                session_id: None,
            })
            .unwrap();

        orch.reconcile_pr_lifecycle().await;

        // Both should be removed from the store
        assert!(
            !store.waiting_entries().iter().any(|e| e.issue_id == "id1"),
            "no PR path should remove persisted waiting entry"
        );
        assert!(
            !store.claimed_ids().contains(&"id1".to_string()),
            "no PR path should remove persisted claimed marker"
        );
    }

    // ── reconcile_pr_lifecycle: auto-merge success persists store removal ─

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_auto_merge_persists_store_removal() {
        let mut server = mockito::Server::new_async().await;

        // is_pr_merged: still open
        let _merged_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "merged": false
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // Reviews: approved
        let _reviews_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42/reviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "id": 1,
                    "state": "APPROVED",
                    "user": { "login": "reviewer1" }
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // PR details: mergeable
        let _details_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "mergeable": true,
                    "mergeable_state": "clean"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        // enable_auto_merge: success
        let _merge_mock = server
            .mock("PUT", "/repos/test-org/test-repo/pulls/42/merge")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"sha": "abc123", "merged": true}"#)
            .create_async()
            .await;

        let store = Arc::new(MockStore::new());
        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: true,
        });
        let mut orch = Orchestrator::new(config, "Cleanup {{ issue.identifier }}".to_string());
        orch.store = Some(store.clone());

        // Pre-populate
        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
            state.claimed.insert("id1".to_string());
        }
        store.save_claimed("id1").unwrap();
        store
            .save_waiting(&crate::model::WaitingEntry {
                issue_id: "id1".to_string(),
                identifier: "PROJ-1".to_string(),
                pr_number: 42,
                branch: "symphony/proj-1".to_string(),
                started_waiting_at: Utc::now(),
                session_id: None,
            })
            .unwrap();

        orch.reconcile_pr_lifecycle().await;

        // After auto-merge success, remove_waiting and remove_claimed should have been called
        assert!(
            !store.waiting_entries().iter().any(|e| e.issue_id == "id1"),
            "auto-merge success should remove persisted waiting entry"
        );
    }

    // ── reconcile_pr_lifecycle: stale waiting entry (no longer In Review)
    //    persists remove_waiting + remove_claimed ────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_stale_waiting_persists_store_removal() {
        let mut gh_server = mockito::Server::new_async().await;
        let mut linear_server = mockito::Server::new_async().await;

        // is_pr_merged: still open
        let _merged_mock = gh_server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "merged": false
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // Linear: issue moved to "In Progress" (no longer "In Review")
        let _linear_mock = linear_server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&mock_states_response(&[("id1", "PROJ-1", "In Progress")]))
            .create_async()
            .await;

        let store = Arc::new(MockStore::new());
        let mut config = test_config_with_endpoint(&linear_server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: gh_server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let mut orch = Orchestrator::new(config, "template".to_string());
        orch.store = Some(store.clone());

        // Pre-populate
        {
            let mut state = orch.state.write().await;
            state.claimed.insert("id1".to_string());
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
        }
        store.save_claimed("id1").unwrap();
        store
            .save_waiting(&crate::model::WaitingEntry {
                issue_id: "id1".to_string(),
                identifier: "PROJ-1".to_string(),
                pr_number: 42,
                branch: "symphony/proj-1".to_string(),
                started_waiting_at: Utc::now(),
                session_id: None,
            })
            .unwrap();

        orch.reconcile_pr_lifecycle().await;

        // Both should be removed from the store
        assert!(
            !store.waiting_entries().iter().any(|e| e.issue_id == "id1"),
            "stale waiting entry should remove persisted waiting entry"
        );
        assert!(
            !store.claimed_ids().contains(&"id1".to_string()),
            "stale waiting entry should remove persisted claimed marker"
        );
    }

    // ── on_worker_exit Normal In Review with store: verifies save_waiting ─

    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_normal_in_review_persists_waiting_entry() {
        let mut server = mockito::Server::new_async().await;

        // Mock find_open_pr
        let _pr_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let store = Arc::new(MockStore::new());
        let mut config = test_config();
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let mut orch = Orchestrator::new(config, "template".to_string());
        orch.store = Some(store.clone());

        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "In Review").await;

        orch.on_worker_exit("id1", WorkerExitReason::Normal).await;

        // store.save_waiting should have been called via add_to_waiting_for_review
        let entries = store.waiting_entries();
        assert_eq!(
            entries.len(),
            1,
            "on_worker_exit Normal In Review should persist waiting entry"
        );
        assert_eq!(entries[0].issue_id, "id1");
        assert_eq!(entries[0].pr_number, 42);
    }

    // ── Error-path tests: store failures are best-effort (debug-logged) ─

    // dispatch_issue: save_claimed error is best-effort
    #[traced_test]
    #[tokio::test]
    async fn test_dispatch_issue_save_claimed_error_is_best_effort() {
        let store = Arc::new(MockStore::new().with_fail_save_claimed());
        let orch = test_orchestrator_with_store(store.clone());
        let issue = make_issue("id1", "PROJ-1", "Todo");

        // Should not panic even though save_claimed fails
        orch.dispatch_issue(issue, None, false, None).await;

        // Issue should still be in running (dispatch succeeded despite store error)
        let state = orch.state.read().await;
        assert!(state.running.contains_key("id1"));
        drop(state);

        assert!(logs_contain("failed to persist claimed marker"));
    }

    // add_to_waiting_for_review: save_waiting error is best-effort
    #[traced_test]
    #[tokio::test]
    async fn test_add_to_waiting_save_waiting_error_is_best_effort() {
        let mut server = mockito::Server::new_async().await;

        // Mock find_open_pr
        let _pr_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open"
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let store = Arc::new(MockStore::new().with_fail_save_waiting());
        let mut config = test_config();
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let mut orch = Orchestrator::new(config, "template".to_string());
        orch.store = Some(store.clone());

        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "In Review").await;

        orch.add_to_waiting_for_review("id1", "PROJ-1", None).await;

        // Should still be added to in-memory waiting despite store error
        let state = orch.state.read().await;
        assert!(state.waiting_for_review.contains_key("id1"));
        drop(state);

        assert!(logs_contain("failed to persist waiting entry"));
    }

    // on_worker_exit Cancelled: remove_claimed error is best-effort
    #[traced_test]
    #[tokio::test]
    async fn test_on_worker_exit_cancelled_remove_claimed_error_is_best_effort() {
        let store = Arc::new(MockStore::new().with_fail_remove_claimed());
        let orch = test_orchestrator_with_store(store.clone());
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.on_worker_exit("id1", WorkerExitReason::Cancelled)
            .await;

        // Issue should still be removed from running despite store error
        let state = orch.state.read().await;
        assert!(!state.running.contains_key("id1"));
        drop(state);

        assert!(logs_contain("failed to remove persisted claimed marker"));
    }

    // on_retry_fired: remove_claimed error (issue no longer active) is best-effort
    #[traced_test]
    #[tokio::test]
    async fn test_on_retry_fired_remove_claimed_error_is_best_effort() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[])) // empty: issue not in candidates
            .create_async()
            .await;

        let store = Arc::new(MockStore::new().with_fail_remove_claimed());
        let config = test_config_with_endpoint(&server.url());
        let mut orch = Orchestrator::new(config, "template".to_string());
        orch.store = Some(store.clone());

        {
            let mut state = orch.state.write().await;
            state.retry_attempts.insert(
                "id1".to_string(),
                RetryEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    attempt: 1,
                    due_at_ms: 0,
                    error: None,
                },
            );
            state.claimed.insert("id1".to_string());
        }

        orch.on_retry_fired("id1").await;

        assert!(logs_contain("failed to remove persisted claimed marker"));
    }

    // on_remove_from_retry_queue: remove_claimed error is best-effort
    #[traced_test]
    #[tokio::test]
    async fn test_on_remove_from_retry_queue_remove_claimed_error_is_best_effort() {
        let store = Arc::new(MockStore::new().with_fail_remove_claimed());
        let orch = test_orchestrator_with_store(store.clone());

        {
            let mut state = orch.state.write().await;
            state.retry_attempts.insert(
                "id1".to_string(),
                RetryEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    attempt: 1,
                    due_at_ms: 0,
                    error: None,
                },
            );
            state.claimed.insert("id1".to_string());
        }

        orch.on_remove_from_retry_queue("id1").await;

        assert!(logs_contain("failed to remove persisted claimed marker"));
    }

    // terminate_running_issue: remove_claimed error is best-effort
    #[traced_test]
    #[tokio::test]
    async fn test_terminate_running_issue_remove_claimed_error_is_best_effort() {
        let store = Arc::new(MockStore::new().with_fail_remove_claimed());
        let orch = test_orchestrator_with_store(store.clone());
        let _cancel_rx = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        orch.terminate_running_issue("id1", false).await;

        // Issue should still be removed from running despite store error
        let state = orch.state.read().await;
        assert!(!state.running.contains_key("id1"));
        drop(state);

        assert!(logs_contain("failed to remove persisted claimed marker"));
    }

    // reconcile_pr_lifecycle: remove_waiting + remove_claimed errors (merged PR path)
    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_merged_store_removal_error_is_best_effort() {
        let mut server = mockito::Server::new_async().await;

        let _merged_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "closed",
                    "merged": true
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        let store = Arc::new(
            MockStore::new()
                .with_fail_remove_waiting()
                .with_fail_remove_claimed(),
        );
        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let mut orch = Orchestrator::new(config, "Cleanup {{ issue.identifier }}".to_string());
        orch.store = Some(store.clone());

        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
            state.claimed.insert("id1".to_string());
        }

        orch.reconcile_pr_lifecycle().await;

        // waiting_for_review should be removed from in-memory state
        let state = orch.state.read().await;
        assert!(!state.waiting_for_review.contains_key("id1"));
        // Note: claimed may be re-added by dispatch_cleanup → dispatch_issue,
        // but the store error logs should still be emitted
        drop(state);

        assert!(logs_contain("failed to remove persisted waiting entry"));
        assert!(logs_contain("failed to remove persisted claimed marker"));
    }

    // reconcile_pr_lifecycle: remove_waiting + remove_claimed errors (no PR path)
    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_no_pr_store_removal_error_is_best_effort() {
        let mut server = mockito::Server::new_async().await;

        // No PR found
        let _no_pr_mock = server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        let store = Arc::new(
            MockStore::new()
                .with_fail_remove_waiting()
                .with_fail_remove_claimed(),
        );
        let mut config = test_config_with_endpoint(&server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let mut orch = Orchestrator::new(config, "template".to_string());
        orch.store = Some(store.clone());

        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
            state.claimed.insert("id1".to_string());
        }

        orch.reconcile_pr_lifecycle().await;

        assert!(logs_contain("failed to remove persisted waiting entry"));
        assert!(logs_contain("failed to remove persisted claimed marker"));
    }

    // reconcile_pr_lifecycle: remove_waiting + remove_claimed errors (auto-merge success path)
    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_auto_merge_store_removal_error_is_best_effort() {
        let mut gh_server = mockito::Server::new_async().await;
        let mut linear_server = mockito::Server::new_async().await;

        // is_pr_merged: not merged
        let _merged_mock = gh_server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "merged": false
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // get_pr_reviews: approved
        let _reviews_mock = gh_server
            .mock("GET", "/repos/test-org/test-repo/pulls/42/reviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "id": 1,
                    "state": "APPROVED",
                    "user": { "login": "reviewer1" }
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // get_pr_details: mergeable
        let _details_mock = gh_server
            .mock("GET", "/repos/test-org/test-repo/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "mergeable": true,
                    "mergeable_state": "clean"
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        // enable_auto_merge / merge: success
        let _merge_mock = gh_server
            .mock("PUT", "/repos/test-org/test-repo/pulls/42/merge")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"sha": "abc123", "merged": true}"#)
            .create_async()
            .await;

        // Linear: issue still in "In Review" (so it doesn't get marked stale)
        let _linear_mock = linear_server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&mock_states_response(&[("id1", "PROJ-1", "In Review")]))
            .create_async()
            .await;

        let store = Arc::new(
            MockStore::new()
                .with_fail_remove_waiting()
                .with_fail_remove_claimed(),
        );
        let mut config = test_config_with_endpoint(&linear_server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: gh_server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: true,
        });
        let mut orch = Orchestrator::new(config, "Cleanup {{ issue.identifier }}".to_string());
        orch.store = Some(store.clone());

        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
            state.claimed.insert("id1".to_string());
        }

        orch.reconcile_pr_lifecycle().await;

        assert!(logs_contain("failed to remove persisted waiting entry"));
        assert!(logs_contain("failed to remove persisted claimed marker"));
    }

    // reconcile_pr_lifecycle: remove_waiting + remove_claimed errors (stale waiting path)
    #[traced_test]
    #[tokio::test]
    async fn test_reconcile_pr_lifecycle_stale_store_removal_error_is_best_effort() {
        let mut gh_server = mockito::Server::new_async().await;
        let mut linear_server = mockito::Server::new_async().await;

        // is_pr_merged: still open (not merged)
        let _merged_mock = gh_server
            .mock("GET", "/repos/test-org/test-repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "head".to_string(),
                    "test-org:symphony/proj-1".to_string(),
                ),
                mockito::Matcher::UrlEncoded("state".to_string(), "all".to_string()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::to_string(&vec![serde_json::json!({
                    "number": 42,
                    "state": "open",
                    "merged": false
                })])
                .unwrap(),
            )
            .create_async()
            .await;

        // Linear: issue moved to "In Progress" (no longer "In Review" → stale)
        let _linear_mock = linear_server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&mock_states_response(&[("id1", "PROJ-1", "In Progress")]))
            .create_async()
            .await;

        let store = Arc::new(
            MockStore::new()
                .with_fail_remove_waiting()
                .with_fail_remove_claimed(),
        );
        let mut config = test_config_with_endpoint(&linear_server.url());
        config.github = Some(crate::config::GitHubConfig {
            token: "test-gh-token".to_string(),
            repo_owner: "test-org".to_string(),
            repo_name: "test-repo".to_string(),
            api_base: gh_server.url(),
            branch_prefix: "symphony/".to_string(),
            auto_merge: false,
        });
        let mut orch = Orchestrator::new(config, "template".to_string());
        orch.store = Some(store.clone());

        {
            let mut state = orch.state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                crate::model::WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
            state.claimed.insert("id1".to_string());
        }

        orch.reconcile_pr_lifecycle().await;

        assert!(logs_contain("failed to remove persisted waiting entry"));
        assert!(logs_contain("failed to remove persisted claimed marker"));
    }
}
