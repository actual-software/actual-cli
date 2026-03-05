use crate::agent::AgentSession;
use crate::config::{state_matches, ServiceConfig};
use crate::error::{Result, SymphonyError};
use crate::model::{
    AgentEvent, AgentTotals, Issue, LiveSession, OrchestratorState, RetryEntry, RunningEntry,
    WorkerExitReason,
};
use crate::prompt::{build_continuation_prompt, render_prompt};
use crate::tracker::LinearClient;
use crate::workspace;
use chrono::Utc;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, error, info, warn};

/// Messages from workers back to the orchestrator.
#[derive(Debug)]
pub enum OrchestratorMessage {
    WorkerExited {
        issue_id: String,
        reason: WorkerExitReason,
    },
    AgentUpdate {
        issue_id: String,
        event: AgentEvent,
    },
    RetryFired {
        issue_id: String,
    },
    TriggerRefresh,
}

/// The orchestrator owns the poll tick, state, and dispatch logic.
pub struct Orchestrator {
    state: Arc<RwLock<OrchestratorState>>,
    config: Arc<RwLock<ServiceConfig>>,
    prompt_template: Arc<RwLock<String>>,
    msg_tx: mpsc::Sender<OrchestratorMessage>,
    msg_rx: mpsc::Receiver<OrchestratorMessage>,
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
            msg_tx,
            msg_rx,
        }
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
            info!(
                poll_interval_ms = config.polling.interval_ms,
                max_concurrent = config.agent.max_concurrent_agents,
                "starting orchestrator"
            );
        }

        // Startup terminal workspace cleanup
        self.startup_cleanup().await;

        // Schedule immediate first tick
        let mut tick_interval = {
            let state = self.state.read().await;
            tokio::time::interval(std::time::Duration::from_millis(state.poll_interval_ms))
        };

        loop {
            tokio::select! {
                _ = tick_interval.tick() => {
                    self.on_tick().await;

                    // Re-read interval in case it changed
                    let state = self.state.read().await;
                    let new_interval = std::time::Duration::from_millis(state.poll_interval_ms);
                    tick_interval = tokio::time::interval(new_interval);
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

        // Phase 2: Validate config
        let config = self.config.read().await;
        if let Err(e) = config.validate_for_dispatch() {
            error!(error = %e, "dispatch validation failed, skipping dispatch");
            return;
        }

        // Phase 3: Fetch candidates
        let client = LinearClient::new(&config.tracker);
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
                self.dispatch_issue(issue, None).await;
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

        // Not already running or claimed
        if state.running.contains_key(&issue.id) || state.claimed.contains(&issue.id) {
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

        true
    }

    async fn dispatch_issue(&self, issue: Issue, attempt: Option<u32>) {
        let issue_id = issue.id.clone();
        let identifier = issue.identifier.clone();

        info!(
            issue_id = %issue_id,
            issue_identifier = %identifier,
            attempt = ?attempt,
            "dispatching issue"
        );

        let (cancel_tx, cancel_rx) = oneshot::channel();

        // Add to running and claimed
        {
            let mut state = self.state.write().await;

            state.running.insert(
                issue_id.clone(),
                RunningEntry {
                    issue: issue.clone(),
                    identifier: identifier.clone(),
                    session: LiveSession::default(),
                    retry_attempt: attempt,
                    started_at: Utc::now(),
                    cancel_tx,
                },
            );
            state.claimed.insert(issue_id.clone());
            state.retry_attempts.remove(&issue_id);
        }

        // Spawn worker task
        let config = self.config.read().await.clone();
        let prompt_template = self.prompt_template.read().await.clone();
        let msg_tx = self.msg_tx.clone();

        tokio::spawn(async move {
            let result = run_worker(
                &config,
                &prompt_template,
                &issue,
                attempt,
                msg_tx.clone(),
                cancel_rx,
            )
            .await;

            let reason = match result {
                Ok(()) => WorkerExitReason::Normal,
                Err(SymphonyError::AgentTurnTimeout) => WorkerExitReason::TimedOut,
                Err(SymphonyError::AgentStalled { .. }) => WorkerExitReason::Stalled,
                Err(e) => WorkerExitReason::Failed(e.to_string()),
            };

            let _ = msg_tx
                .send(OrchestratorMessage::WorkerExited {
                    issue_id: issue.id.clone(),
                    reason,
                })
                .await;
        });
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
        }
    }

    async fn on_worker_exit(&self, issue_id: &str, reason: WorkerExitReason) {
        let mut state = self.state.write().await;

        if let Some(entry) = state.running.remove(issue_id) {
            // Add runtime seconds to totals
            let elapsed = Utc::now()
                .signed_duration_since(entry.started_at)
                .num_milliseconds()
                .max(0) as f64
                / 1000.0;
            state.agent_totals.seconds_running += elapsed;

            let identifier = entry.identifier.clone();
            let next_attempt = entry.retry_attempt.unwrap_or(0) + 1;

            match &reason {
                WorkerExitReason::Normal => {
                    info!(
                        issue_id = %issue_id,
                        issue_identifier = %identifier,
                        "worker completed normally"
                    );
                    state.completed.insert(issue_id.to_string());

                    // Schedule continuation retry after 1s
                    self.schedule_retry_inner(
                        &mut state,
                        issue_id.to_string(),
                        1,
                        identifier,
                        None,
                        1_000,
                    );
                }
                WorkerExitReason::Failed(err) => {
                    warn!(
                        issue_id = %issue_id,
                        issue_identifier = %identifier,
                        error = %err,
                        "worker failed"
                    );
                    let config = self.config.read().await;
                    let delay =
                        exponential_backoff(next_attempt, config.agent.max_retry_backoff_ms);
                    drop(config);
                    self.schedule_retry_inner(
                        &mut state,
                        issue_id.to_string(),
                        next_attempt,
                        identifier,
                        Some(err.clone()),
                        delay,
                    );
                }
                WorkerExitReason::TimedOut => {
                    warn!(
                        issue_id = %issue_id,
                        issue_identifier = %identifier,
                        "worker timed out"
                    );
                    let config = self.config.read().await;
                    let delay =
                        exponential_backoff(next_attempt, config.agent.max_retry_backoff_ms);
                    drop(config);
                    self.schedule_retry_inner(
                        &mut state,
                        issue_id.to_string(),
                        next_attempt,
                        identifier,
                        Some("turn timeout".to_string()),
                        delay,
                    );
                }
                WorkerExitReason::Stalled => {
                    warn!(
                        issue_id = %issue_id,
                        issue_identifier = %identifier,
                        "worker stalled"
                    );
                    let config = self.config.read().await;
                    let delay =
                        exponential_backoff(next_attempt, config.agent.max_retry_backoff_ms);
                    drop(config);
                    self.schedule_retry_inner(
                        &mut state,
                        issue_id.to_string(),
                        next_attempt,
                        identifier,
                        Some("stalled".to_string()),
                        delay,
                    );
                }
                WorkerExitReason::Cancelled => {
                    info!(
                        issue_id = %issue_id,
                        issue_identifier = %identifier,
                        "worker cancelled by reconciliation"
                    );
                    state.claimed.remove(issue_id);
                }
            }
        }
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

        debug!(
            issue_id = %issue_id,
            issue_identifier = %identifier,
            attempt = attempt,
            delay_ms = delay_ms,
            "scheduled retry"
        );

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

        let config = self.config.read().await;
        let client = LinearClient::new(&config.tracker);

        // Fetch active candidates to check if issue is still eligible
        let candidates = match client
            .fetch_candidate_issues(&config.tracker.active_states)
            .await
        {
            Ok(issues) => issues,
            Err(e) => {
                warn!(
                    issue_id = %issue_id,
                    error = %e,
                    "retry poll failed, rescheduling"
                );
                let max_backoff = config.agent.max_retry_backoff_ms;
                drop(config);
                let delay = exponential_backoff(entry.attempt + 1, max_backoff);
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
                info!(
                    issue_id = %issue_id,
                    issue_identifier = %entry.identifier,
                    "issue no longer active, releasing claim"
                );
                let mut state = self.state.write().await;
                state.claimed.remove(issue_id);
            }
            Some(issue) => {
                let state = self.state.read().await;
                if state.available_slots() == 0 {
                    let max_backoff = config.agent.max_retry_backoff_ms;
                    drop(state);
                    drop(config);
                    let delay = exponential_backoff(entry.attempt + 1, max_backoff);
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
                drop(config);

                self.dispatch_issue(issue.clone(), Some(entry.attempt))
                    .await;
            }
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
            }
        }

        // Update aggregate totals outside the running entry borrow
        if let Some((di, do_, dt)) = token_deltas {
            state.agent_totals.input_tokens += di;
            state.agent_totals.output_tokens += do_;
            state.agent_totals.total_tokens += dt;
        }
    }

    async fn reconcile(&self) {
        // Part A: Stall detection
        self.reconcile_stalls().await;

        // Part B: Tracker state refresh
        self.reconcile_tracker_states().await;
    }

    async fn reconcile_stalls(&self) {
        let config = self.config.read().await;
        let stall_timeout_ms = config.codex.stall_timeout_ms;

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
                warn!(
                    issue_id = %issue_id,
                    issue_identifier = %entry.identifier,
                    elapsed_ms = elapsed_ms,
                    stall_timeout_ms = stall_timeout_ms,
                    "session stalled"
                );
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

        let config = self.config.read().await;
        let client = LinearClient::new(&config.tracker);

        let refreshed = match client.fetch_issue_states_by_ids(&running_ids).await {
            Ok(issues) => issues,
            Err(e) => {
                debug!(error = %e, "reconciliation state refresh failed, keeping workers");
                return;
            }
        };

        for issue in &refreshed {
            if state_matches(&issue.state, &config.tracker.terminal_states) {
                info!(
                    issue_id = %issue.id,
                    issue_identifier = %issue.identifier,
                    state = %issue.state,
                    "issue is terminal, stopping worker and cleaning workspace"
                );
                self.terminate_running_issue(&issue.id, true).await;
            } else if state_matches(&issue.state, &config.tracker.active_states) {
                // Update the issue snapshot
                let mut state = self.state.write().await;
                if let Some(entry) = state.running.get_mut(&issue.id) {
                    entry.issue = issue.clone();
                }
            } else {
                // Neither active nor terminal - stop without cleanup
                info!(
                    issue_id = %issue.id,
                    issue_identifier = %issue.identifier,
                    state = %issue.state,
                    "issue neither active nor terminal, stopping worker"
                );
                self.terminate_running_issue(&issue.id, false).await;
            }
        }
    }

    async fn terminate_running_issue(&self, issue_id: &str, cleanup_workspace: bool) {
        let mut state = self.state.write().await;
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

            if cleanup_workspace {
                let config = self.config.read().await;
                let workspace_key = crate::workspace::sanitize_workspace_key(&entry.identifier);
                let workspace_path = config.workspace.root.join(&workspace_key);
                workspace::cleanup_workspace(&workspace_path, &config.hooks).await;
            }
        }
    }

    async fn startup_cleanup(&self) {
        let config = self.config.read().await;
        let client = LinearClient::new(&config.tracker);

        info!("running startup terminal workspace cleanup");

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
                info!(
                    issue_identifier = %issue.identifier,
                    workspace = %workspace_path.display(),
                    "cleaning terminal workspace"
                );
                workspace::cleanup_workspace(&workspace_path, &config.hooks).await;
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
    }
}

/// Run a worker for one issue.
async fn run_worker(
    config: &ServiceConfig,
    prompt_template: &str,
    issue: &Issue,
    attempt: Option<u32>,
    msg_tx: mpsc::Sender<OrchestratorMessage>,
    mut cancel_rx: oneshot::Receiver<()>,
) -> Result<()> {
    let issue_id = issue.id.clone();

    // Phase 1: Prepare workspace
    let workspace_result =
        workspace::create_workspace(&config.workspace.root, &issue.identifier, &config.hooks)
            .await?;

    // Phase 2: Run before_run hook
    workspace::run_before_run_hook(&workspace_result.path, &config.hooks).await?;

    // Phase 3: Build prompt and launch agent
    let max_turns = config.agent.max_turns;
    let mut turn_number: u32 = 1;

    loop {
        // Check for cancellation
        if cancel_rx.try_recv().is_ok() {
            workspace::run_after_run_hook(&workspace_result.path, &config.hooks).await;
            return Err(SymphonyError::AgentTurnCancelled);
        }

        // Build prompt
        let prompt = if turn_number == 1 {
            render_prompt(prompt_template, issue, attempt)?
        } else {
            build_continuation_prompt(issue, turn_number, max_turns)
        };

        // Launch agent
        let (mut session, mut event_rx) =
            AgentSession::launch(&config.codex, &workspace_result.path, &prompt, issue).await?;

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
            .wait_with_timeout(config.codex.turn_timeout_ms)
            .await;
        event_forwarder.abort();

        match result {
            Ok(true) => {
                info!(
                    issue_id = %issue_id,
                    turn = turn_number,
                    "agent turn completed successfully"
                );

                // Check if issue is still active
                let client = LinearClient::new(&config.tracker);
                match client.fetch_issue_states_by_ids(&[issue.id.clone()]).await {
                    Ok(refreshed) => {
                        if let Some(refreshed_issue) = refreshed.first() {
                            if !state_matches(&refreshed_issue.state, &config.tracker.active_states)
                            {
                                info!(
                                    issue_id = %issue_id,
                                    state = %refreshed_issue.state,
                                    "issue no longer active after turn"
                                );
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    Err(e) => {
                        warn!(
                            issue_id = %issue_id,
                            error = %e,
                            "failed to refresh issue state after turn"
                        );
                        break;
                    }
                }

                if turn_number >= max_turns {
                    info!(
                        issue_id = %issue_id,
                        turns = turn_number,
                        max_turns = max_turns,
                        "reached max turns"
                    );
                    break;
                }

                turn_number += 1;
            }
            Ok(false) | Err(_) => {
                workspace::run_after_run_hook(&workspace_result.path, &config.hooks).await;
                result?;
                return Err(SymphonyError::AgentTurnFailed {
                    reason: "agent exited with failure".to_string(),
                });
            }
        }
    }

    workspace::run_after_run_hook(&workspace_result.path, &config.hooks).await;
    Ok(())
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

/// Compute exponential backoff delay.
/// Formula: min(10000 * 2^(attempt - 1), max_backoff_ms)
fn exponential_backoff(attempt: u32, max_backoff_ms: u64) -> u64 {
    let base: u64 = 10_000;
    let power = attempt.saturating_sub(1).min(20); // Cap to prevent overflow
    let delay = base.saturating_mul(1u64.checked_shl(power).unwrap_or(u64::MAX));
    delay.min(max_backoff_ms)
}

/// Snapshot of orchestrator state for observability.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OrchestratorSnapshot {
    pub running: Vec<RunningSessionInfo>,
    pub retrying: Vec<RetryEntry>,
    pub totals: AgentTotals,
    pub rate_limits: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RunningSessionInfo {
    pub issue_id: String,
    pub issue_identifier: String,
    pub state: String,
    pub session_id: Option<String>,
    pub turn_count: u32,
    pub last_event: Option<String>,
    pub last_message: Option<String>,
    pub started_at: chrono::DateTime<Utc>,
    pub last_event_at: Option<chrono::DateTime<Utc>>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_exponential_backoff() {
        assert_eq!(exponential_backoff(1, 300_000), 10_000);
        assert_eq!(exponential_backoff(2, 300_000), 20_000);
        assert_eq!(exponential_backoff(3, 300_000), 40_000);
        assert_eq!(exponential_backoff(4, 300_000), 80_000);
        assert_eq!(exponential_backoff(5, 300_000), 160_000);
        assert_eq!(exponential_backoff(6, 300_000), 300_000); // capped
        assert_eq!(exponential_backoff(10, 300_000), 300_000); // still capped
    }

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
}
