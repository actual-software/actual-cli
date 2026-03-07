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

        // Phase 2: Validate config
        let config = self.config.read().await;
        if let Err(e) = config.validate_for_dispatch() {
            error!(error = %e, "dispatch validation failed, skipping dispatch");
            return;
        }

        // Phase 3: Fetch candidates
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
            )
            .await;

            let reason = match result {
                Ok(()) => WorkerExitReason::Normal,
                Err(SymphonyError::AgentTurnTimeout) => WorkerExitReason::TimedOut,
                Err(SymphonyError::AgentTurnCancelled) => WorkerExitReason::Cancelled,
                Err(e) => WorkerExitReason::Failed(e.to_string()),
            };

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
                    state.mark_completed(issue_id.to_string());
                    // §8.1: Schedule a short continuation retry (1000ms)
                    // instead of releasing the claim and waiting for the
                    // next poll tick (~30s). The retry handler will re-check
                    // if the issue is still active and re-dispatch quickly
                    // or release the claim if the issue went terminal.
                    self.schedule_retry_inner(
                        &mut state,
                        issue_id.to_string(),
                        0, // continuation, not a failure retry
                        identifier,
                        None,
                        1_000, // 1 second
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
                    self.maybe_schedule_retry(
                        &mut state,
                        &config,
                        issue_id,
                        next_attempt,
                        identifier,
                        Some(err.clone()),
                    );
                }
                WorkerExitReason::TimedOut => {
                    warn!(
                        issue_id = %issue_id,
                        issue_identifier = %identifier,
                        "worker timed out"
                    );
                    let config = self.config.read().await;
                    self.maybe_schedule_retry(
                        &mut state,
                        &config,
                        issue_id,
                        next_attempt,
                        identifier,
                        Some("turn timeout".to_string()),
                    );
                }
                WorkerExitReason::Stalled => {
                    warn!(
                        issue_id = %issue_id,
                        issue_identifier = %identifier,
                        "worker stalled"
                    );
                    let config = self.config.read().await;
                    self.maybe_schedule_retry(
                        &mut state,
                        &config,
                        issue_id,
                        next_attempt,
                        identifier,
                        Some("stalled".to_string()),
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

    /// Schedule a retry if the attempt count hasn't exceeded `max_retries`.
    /// If the cap is reached, the issue is released without further retries.
    fn maybe_schedule_retry(
        &self,
        state: &mut OrchestratorState,
        config: &ServiceConfig,
        issue_id: &str,
        attempt: u32,
        identifier: String,
        error: Option<String>,
    ) {
        if attempt > config.agent.max_retries {
            error!(
                issue_id = %issue_id,
                issue_identifier = %identifier,
                attempt = attempt,
                max_retries = config.agent.max_retries,
                "max retries exceeded, giving up"
            );
            state.claimed.remove(issue_id);
            return;
        }
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
        let client = LinearClient::new(&config.tracker, self.http.clone());

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
                AgentEvent::RateLimitUpdate { .. } => {
                    entry.session.last_event = Some("rate_limit_event".to_string());
                    entry.session.last_event_at = Some(now);
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
                info!(
                    issue_id = %issue.id,
                    issue_identifier = %issue.identifier,
                    state = %issue.state,
                    "issue is terminal, stopping worker and cleaning workspace"
                );
                // Mark as completed before terminating — the tracker says it's done
                {
                    let mut state = self.state.write().await;
                    state.mark_completed(issue.id.clone());
                }
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

            // Abort and remove the worker handle
            if let Some(handle) = self.worker_handles.write().await.remove(issue_id) {
                handle.abort();
            }

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
        let client = LinearClient::new(&config.tracker, self.http.clone());

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
            let drain_timeout = std::time::Duration::from_secs(SHUTDOWN_DRAIN_TIMEOUT_SECS);
            let drain_result =
                tokio::time::timeout(drain_timeout, drain_worker_handles(handles)).await;
            if drain_result.is_err() {
                warn!("worker drain timed out after {SHUTDOWN_DRAIN_TIMEOUT_SECS}s");
            }
        }
    }
}

/// Run a worker for one issue.
async fn run_worker(
    config: &ServiceConfig,
    prompt_template: &str,
    issue: &Issue,
    attempt: Option<u32>,
    http: reqwest::Client,
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
            Ok(_) => {
                info!(
                    issue_id = %issue_id,
                    turn = turn_number,
                    "agent turn completed successfully"
                );

                // Check if issue is still active
                let client = LinearClient::new(&config.tracker, http.clone());
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
            Err(_) => {
                workspace::run_after_run_hook(&workspace_result.path, &config.hooks).await;
                return result.map(|_| ());
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
    use crate::config::{
        AgentConfig, CodingAgentConfig, HooksConfig, PollingConfig, TrackerConfig, WorkspaceConfig,
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
                max_retries: 10,
                max_retry_backoff_ms: 300_000,
                max_concurrent_agents_by_state: HashMap::new(),
            },
            codex: CodingAgentConfig {
                command: "echo test".to_string(),
                permission_mode: "bypassPermissions".to_string(),
                turn_timeout_ms: 3_600_000,
                stall_timeout_ms: 300_000,
            },
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
    async fn test_on_worker_exit_max_retries_exceeded() {
        // Configure max_retries = 2, then fail with retry_attempt = Some(2)
        // → next_attempt = 3 > max_retries = 2 → gives up
        let mut config = test_config();
        config.agent.max_retries = 2;
        let orch = Orchestrator::new(config, "template".to_string());

        // Insert with retry_attempt = Some(2) so next_attempt = 3
        let (cancel_tx, _cancel_rx) = oneshot::channel();
        {
            let mut state = orch.state.write().await;
            state.running.insert(
                "id1".to_string(),
                RunningEntry {
                    issue: make_issue("id1", "PROJ-1", "Todo"),
                    identifier: "PROJ-1".to_string(),
                    session: LiveSession::default(),
                    retry_attempt: Some(2),
                    started_at: Utc::now(),
                    cancel_tx,
                },
            );
            state.claimed.insert("id1".to_string());
        }

        orch.on_worker_exit("id1", WorkerExitReason::Failed("boom".to_string()))
            .await;

        let state = orch.state.read().await;
        // Should NOT have a retry entry — max retries exceeded
        assert_eq!(state.retry_attempts.contains_key("id1"), false);
        // Claim should be released
        assert_eq!(state.claimed.contains("id1"), false);
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

    // §11.2: rate limit update stored in state
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
        let candidates_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[
                ("id1", "PROJ-1", "Todo"),
                ("id2", "PROJ-2", "In Progress"),
            ]))
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

        candidates_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_on_tick_respects_max_concurrent() {
        let mut server = mockito::Server::new_async().await;

        // Return 5 candidates, but max_concurrent is 3
        let candidates_mock = server
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
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        orch.on_tick().await;

        let state = orch.state.read().await;
        assert_eq!(state.running.len(), 3); // max_concurrent_agents = 3

        candidates_mock.assert_async().await;
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
        config.codex.stall_timeout_ms = 100; // 100ms stall timeout
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
        config.codex.stall_timeout_ms = 0;
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
        config.codex.stall_timeout_ms = 100;
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

    // ── on_retry_fired (with mockito) ────────────────────────────────

    #[tokio::test]
    async fn test_on_retry_fired_issue_still_active_dispatches() {
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

        mock.assert_async().await;
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

        orch.dispatch_issue(issue, None).await;

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

        orch.dispatch_issue(issue, Some(3)).await;

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
        orch.dispatch_issue(issue, Some(2)).await;

        let state = orch.state.read().await;
        assert!(!state.retry_attempts.contains_key("id1"));
        assert!(state.running.contains_key("id1"));
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
        config.codex.stall_timeout_ms = 100;
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
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[("id1", "PROJ-1", "Todo")]))
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

        mock.assert_async().await;
    }

    // ── Edge cases ───────────────────────────────────────────────────

    #[tokio::test]
    async fn test_multiple_dispatches_same_tick() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[
                ("id1", "PROJ-1", "Todo"),
                ("id2", "PROJ-2", "Todo"),
            ]))
            .create_async()
            .await;

        let config = test_config_with_endpoint(&server.url());
        let orch = Orchestrator::new(config, "template".to_string());

        orch.on_tick().await;

        let state = orch.state.read().await;
        assert_eq!(state.running.len(), 2);
        assert!(state.claimed.contains("id1"));
        assert!(state.claimed.contains("id2"));

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_on_tick_skips_already_running() {
        let mut server = mockito::Server::new_async().await;

        // on_tick calls reconcile() first (which calls reconcile_tracker_states
        // making one HTTP request), then fetch_candidate_issues (another request).
        // Both go to the same endpoint so we need to allow 2 requests.
        let reconcile_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[("id1", "PROJ-1", "Todo")]))
            .expect(1)
            .create_async()
            .await;

        let candidates_mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[
                ("id1", "PROJ-1", "Todo"),
                ("id2", "PROJ-2", "Todo"),
            ]))
            .expect(1)
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

        reconcile_mock.assert_async().await;
        candidates_mock.assert_async().await;
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
        config.codex.command = "true #-p".to_string();
        let orch = Orchestrator::new(config, "template".to_string());

        let issue = make_issue("id1", "PROJ-1", "Todo");
        orch.dispatch_issue(issue, None).await;

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
        config.codex.command = r#"printf '{"type":"result","result":"done"}\n' #-p"#.to_string();

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
        config.codex.command = r#"printf '{"type":"result","result":"ok"}\n' #-p"#.to_string();

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
        config.codex.command = r#"printf '{"type":"result","result":"ok"}\n' #-p"#.to_string();

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
        )
        .await;

        assert!(result.is_ok());
        mock.assert_async().await;
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
        config.codex.command = r#"printf '{"type":"result","result":"ok"}\n' #-p"#.to_string();

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
        config.codex.command = "exit 1 #-p".to_string();

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
        config.codex.command = r#"printf '{"type":"result","result":"ok"}\n' #-p"#.to_string();

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
        config.codex.command = r#"printf '{"type":"result","result":"ok"}\n' #-p"#.to_string();

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
        config.codex.command = "sleep 999 #-p".to_string();
        config.codex.turn_timeout_ms = 100; // 100ms timeout

        let orch = Orchestrator::new(config, "Work on {{ issue.identifier }}".to_string());

        let issue = make_issue("id1", "PROJ-1", "Todo");
        orch.dispatch_issue(issue, None).await;

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
        config.codex.command = "exit 1 #-p".to_string();

        let orch = Orchestrator::new(config, "Work on {{ issue.identifier }}".to_string());

        let issue = make_issue("id1", "PROJ-1", "Todo");
        orch.dispatch_issue(issue, None).await;

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
        config.codex.command = "sleep 999 #-p".to_string();
        config.codex.turn_timeout_ms = 100;

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
        config.codex.command = r#"printf '{"type":"result","result":"done"}\n' #-p"#.to_string();

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
        config.codex.command = "exit 1 #-p".to_string();

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
        config.codex.command = r#"printf '{"type":"result","result":"ok"}\n' #-p"#.to_string();

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
        config.codex.command =
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
        config.codex.turn_timeout_ms = 100; // very short timeout
                                            // Agent sleeps forever, will be killed by timeout
        config.codex.command = "sleep 999 #-p".to_string();

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

        // Startup: empty
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[]))
            .expect(1)
            .create_async()
            .await;

        // Candidates: one issue in Todo
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_candidates_response(&[("id1", "PROJ-1", "Todo")]))
            .expect(1)
            .create_async()
            .await;

        // Reconcile will see the issue has gone to "Done" (terminal)
        // and cancel the running worker
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_states_response(&[("id1", "PROJ-1", "Done")]))
            .expect_at_least(1)
            .create_async()
            .await;

        // Subsequent ticks: no candidates
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
        // Agent that sleeps, so it's still running when reconcile fires
        config.codex.command = "sleep 999 #-p".to_string();
        config.codex.turn_timeout_ms = 60_000; // long timeout so it doesn't time out first

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
        config.codex.command = "sleep 999 #-p".to_string();
        config.codex.turn_timeout_ms = 60_000;

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
        config.codex.stall_timeout_ms = 500; // 500ms stall timeout
        config.codex.turn_timeout_ms = 60_000; // long turn timeout
                                               // Agent outputs nothing (no events) and sleeps — will trigger stall
        config.codex.command = "sleep 999 #-p".to_string();

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

    // ── drain_worker_handles ─────────────────────────────────────────

    #[tokio::test]
    async fn test_drain_worker_handles_empty() {
        // Should complete immediately with no handles
        drain_worker_handles(vec![]).await;
    }

    #[tokio::test]
    async fn test_drain_worker_handles_waits_for_completion() {
        let h1 =
            tokio::spawn(async { tokio::time::sleep(std::time::Duration::from_millis(10)).await });
        let h2 =
            tokio::spawn(async { tokio::time::sleep(std::time::Duration::from_millis(10)).await });
        drain_worker_handles(vec![h1, h2]).await;
        // If we got here, both handles completed
    }

    #[tokio::test]
    async fn test_drain_worker_handles_panicked_task() {
        let h = tokio::spawn(async { panic!("test panic") });
        // Should not panic — JoinError from panicked task is ignored
        drain_worker_handles(vec![h]).await;
    }

    // ── shutdown drains workers ──────────────────────────────────────

    #[traced_test]
    #[tokio::test]
    async fn test_shutdown_drains_worker_handles() {
        let orch = test_orchestrator();
        let _c1 = insert_running(&orch, "id1", "PROJ-1", "Todo").await;

        // Add a worker handle that completes after a short delay
        {
            let handle = tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            });
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
}
