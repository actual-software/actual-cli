use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

// Re-export protocol types for backward compatibility.
pub use crate::protocol::{
    AgentEvent, LiveSession, RunningEntry, TrackedWorker, WorkAssignment, WorkerExitReason,
    WorkerHeartbeat, WorkerRegistration,
};

/// Maximum number of log entries to retain per running issue.
/// Older entries are evicted in FIFO order.
pub const MAX_LOG_ENTRIES: usize = 500;

/// Broadcast channel capacity for per-issue event streaming.
pub const BROADCAST_CHANNEL_CAPACITY: usize = 256;

/// Broadcast channel capacity for global state-change notifications.
/// State changes are less frequent than per-issue events, so a smaller
/// capacity suffices.
pub const STATE_BROADCAST_CAPACITY: usize = 64;

/// A timestamped log entry for per-issue event history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub seq: u64,
    pub timestamp: DateTime<Utc>,
    pub event_type: String,
    pub message: Option<String>,
    pub tokens: Option<TokenSnapshot>,
}

/// Snapshot of token usage at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSnapshot {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// Normalized issue record from the tracker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    /// Stable tracker-internal ID.
    pub id: String,
    /// Human-readable ticket key (e.g. "ABC-123").
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
    /// Lower numbers are higher priority. null sorts last.
    pub priority: Option<i32>,
    /// Current tracker state name.
    pub state: String,
    pub branch_name: Option<String>,
    pub url: Option<String>,
    /// Normalized to lowercase.
    pub labels: Vec<String>,
    pub blocked_by: Vec<BlockerRef>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

/// Blocker reference from issue relations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockerRef {
    pub id: Option<String>,
    pub identifier: Option<String>,
    pub state: Option<String>,
}

/// Parsed WORKFLOW.md payload.
#[derive(Debug, Clone)]
pub struct WorkflowDefinition {
    /// YAML front matter root object.
    pub config: serde_yml::Value,
    /// Markdown body after front matter, trimmed.
    pub prompt_template: String,
}

/// Scheduled retry state for an issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryEntry {
    pub issue_id: String,
    pub identifier: String,
    pub attempt: u32,
    pub due_at_ms: u64,
    pub error: Option<String>,
}

/// Aggregate token totals.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub seconds_running: f64,
}

/// An issue waiting for PR review/merge (worker stopped, no slot consumed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitingEntry {
    pub issue_id: String,
    pub identifier: String,
    pub pr_number: u64,
    pub branch: String,
    pub started_waiting_at: DateTime<Utc>,
    /// Claude Code session ID for resuming the session after PR merge.
    pub session_id: Option<String>,
}

/// Outcome of a completed agent run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CompletionOutcome {
    /// Worker exited normally and issue reached terminal state.
    Success,
    /// Worker exited normally and issue is in review.
    InReview,
    /// Worker exited normally but issue needs continuation retry.
    Retry,
    /// Worker failed, timed out, stalled, or was cancelled.
    Failed,
}

/// Record of a completed agent run, capturing session metrics and outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRecord {
    pub issue_id: String,
    pub identifier: String,
    pub outcome: CompletionOutcome,
    pub duration_seconds: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub turn_count: u32,
    pub completed_at: DateTime<Utc>,
}

/// Maximum number of completed issue IDs to retain for observability.
/// Older entries are evicted in FIFO order to prevent unbounded growth.
pub const MAX_COMPLETED_ENTRIES: usize = 1000;

/// Orchestrator runtime state.
pub struct OrchestratorState {
    pub poll_interval_ms: u64,
    pub max_concurrent_agents: u32,
    pub running: HashMap<String, RunningEntry>,
    pub claimed: HashSet<String>,
    pub retry_attempts: HashMap<String, RetryEntry>,
    /// Issues waiting for PR review/merge (worker stopped, no slot consumed).
    pub waiting_for_review: HashMap<String, WaitingEntry>,
    /// Bounded set of recently-completed issue IDs (FIFO eviction).
    completed_set: HashSet<String>,
    completed_order: VecDeque<String>,
    /// Bounded history of completion records (FIFO eviction at MAX_COMPLETED_ENTRIES).
    completion_history: VecDeque<CompletionRecord>,
    pub agent_totals: AgentTotals,
    pub rate_limits: Option<serde_json::Value>,
    /// Per-issue broadcast channels for SSE event streaming.
    pub event_broadcasts: HashMap<String, tokio::sync::broadcast::Sender<LogEntry>>,
    /// Tracked remote workers (heartbeats, registration).
    pub tracked_workers: HashMap<String, TrackedWorker>,
    /// Queue of work assignments awaiting pickup by distributed workers.
    pub pending_jobs: VecDeque<WorkAssignment>,
    /// Notifier to wake long-polling workers when new jobs are enqueued.
    pub job_notify: Arc<tokio::sync::Notify>,
    /// Broadcast channel for global state-change notifications (SSE).
    /// Sends `()` signals — clients read the full snapshot on each signal.
    pub state_broadcast: tokio::sync::broadcast::Sender<()>,
}

impl std::fmt::Debug for OrchestratorState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrchestratorState")
            .field("poll_interval_ms", &self.poll_interval_ms)
            .field("max_concurrent_agents", &self.max_concurrent_agents)
            .field("running", &self.running)
            .field("claimed", &self.claimed)
            .field("retry_attempts", &self.retry_attempts)
            .field("waiting_for_review", &self.waiting_for_review)
            .field(
                "completion_history",
                &format!("<{} records>", self.completion_history.len()),
            )
            .field("agent_totals", &self.agent_totals)
            .field("rate_limits", &self.rate_limits)
            .field(
                "event_broadcasts",
                &format!("<{} channels>", self.event_broadcasts.len()),
            )
            .field(
                "tracked_workers",
                &format!("<{} workers>", self.tracked_workers.len()),
            )
            .field(
                "pending_jobs",
                &format!("<{} pending>", self.pending_jobs.len()),
            )
            .field(
                "state_broadcast",
                &format!("<{} receivers>", self.state_broadcast.receiver_count()),
            )
            .finish()
    }
}

impl OrchestratorState {
    pub fn new(poll_interval_ms: u64, max_concurrent_agents: u32) -> Self {
        let (state_broadcast, _) = tokio::sync::broadcast::channel(STATE_BROADCAST_CAPACITY);
        Self {
            poll_interval_ms,
            max_concurrent_agents,
            running: HashMap::new(),
            claimed: HashSet::new(),
            retry_attempts: HashMap::new(),
            waiting_for_review: HashMap::new(),
            completed_set: HashSet::new(),
            completed_order: VecDeque::new(),
            completion_history: VecDeque::new(),
            agent_totals: AgentTotals::default(),
            rate_limits: None,
            event_broadcasts: HashMap::new(),
            tracked_workers: HashMap::new(),
            pending_jobs: VecDeque::new(),
            job_notify: Arc::new(tokio::sync::Notify::new()),
            state_broadcast,
        }
    }

    /// Mark an issue as completed. Evicts the oldest entry if the set exceeds
    /// `MAX_COMPLETED_ENTRIES` to prevent unbounded memory growth.
    pub fn mark_completed(&mut self, issue_id: String) {
        if self.completed_set.insert(issue_id.clone()) {
            self.completed_order.push_back(issue_id);
        }
        while self.completed_set.len() > MAX_COMPLETED_ENTRIES {
            if let Some(oldest) = self.completed_order.pop_front() {
                self.completed_set.remove(&oldest);
            }
        }
    }

    /// Check if an issue ID is in the completed set.
    pub fn is_completed(&self, issue_id: &str) -> bool {
        self.completed_set.contains(issue_id)
    }

    /// Number of completed entries.
    pub fn completed_count(&self) -> usize {
        self.completed_set.len()
    }

    /// Add a completion record to the history. Evicts the oldest record
    /// if the history exceeds `MAX_COMPLETED_ENTRIES`.
    pub fn add_completion(&mut self, record: CompletionRecord) {
        self.completion_history.push_back(record);
        while self.completion_history.len() > MAX_COMPLETED_ENTRIES {
            self.completion_history.pop_front();
        }
    }

    /// Returns a reference to the completion history.
    pub fn completion_history(&self) -> &VecDeque<CompletionRecord> {
        &self.completion_history
    }

    pub fn running_count(&self) -> usize {
        self.running.len()
    }

    pub fn available_slots(&self) -> u32 {
        let running = self.running_count() as u32;
        self.max_concurrent_agents.saturating_sub(running)
    }

    /// Signal all SSE subscribers that global state has changed.
    ///
    /// Sends a `()` signal on the broadcast channel. Subscribers receive
    /// the signal and then read the current snapshot. The send error is
    /// intentionally ignored — it simply means there are no active
    /// subscribers.
    pub fn notify_state_change(&self) {
        let _ = self.state_broadcast.send(());
    }

    /// Update or insert a tracked worker from a heartbeat.
    pub fn update_worker_heartbeat(&mut self, heartbeat: &WorkerHeartbeat) {
        let entry = self
            .tracked_workers
            .entry(heartbeat.worker_id.clone())
            .or_insert_with(|| TrackedWorker {
                worker_id: heartbeat.worker_id.clone(),
                last_heartbeat: heartbeat.timestamp,
                active_jobs: heartbeat.active_jobs.clone(),
                registered_at: heartbeat.timestamp,
                capabilities: vec![],
                max_concurrent_jobs: 1,
                alive: true,
            });
        entry.last_heartbeat = heartbeat.timestamp;
        entry.active_jobs = heartbeat.active_jobs.clone();
        entry.alive = true;
    }

    /// Register a worker (create or update its tracked entry).
    pub fn register_worker(&mut self, reg: &WorkerRegistration) {
        let now = Utc::now();
        let entry = self
            .tracked_workers
            .entry(reg.worker_id.clone())
            .or_insert_with(|| TrackedWorker {
                worker_id: reg.worker_id.clone(),
                last_heartbeat: now,
                active_jobs: vec![],
                registered_at: now,
                capabilities: reg.capabilities.clone(),
                max_concurrent_jobs: reg.max_concurrent_jobs,
                alive: true,
            });
        entry.capabilities = reg.capabilities.clone();
        entry.max_concurrent_jobs = reg.max_concurrent_jobs;
        entry.alive = true;
    }

    /// Record that a worker has claimed a job.
    ///
    /// Adds the issue identifier to the worker's `active_jobs` list so
    /// the dashboard can show which worker is executing each session.
    pub fn record_worker_claim(&mut self, worker_id: &str, issue_identifier: &str) {
        if let Some(worker) = self.tracked_workers.get_mut(worker_id) {
            if !worker.active_jobs.contains(&issue_identifier.to_string()) {
                worker.active_jobs.push(issue_identifier.to_string());
            }
        }
    }

    /// Clear a worker's claim on a job (e.g. when the job completes).
    ///
    /// Removes the issue identifier from every tracked worker's
    /// `active_jobs` list. Uses a linear scan since in practice each
    /// worker has very few active jobs.
    pub fn clear_worker_claim(&mut self, issue_identifier: &str) {
        for worker in self.tracked_workers.values_mut() {
            worker.active_jobs.retain(|j| j != issue_identifier);
        }
    }

    /// Mark a worker as dead.
    pub fn mark_worker_dead(&mut self, worker_id: &str) {
        if let Some(entry) = self.tracked_workers.get_mut(worker_id) {
            entry.alive = false;
        }
    }

    /// Return worker IDs whose last heartbeat is older than `threshold`.
    pub fn dead_workers(&self, threshold: std::time::Duration) -> Vec<String> {
        let cutoff = Utc::now() - chrono::Duration::from_std(threshold).unwrap_or_default();
        self.tracked_workers
            .values()
            .filter(|w| w.alive && w.last_heartbeat < cutoff)
            .map(|w| w.worker_id.clone())
            .collect()
    }

    /// Count running issues in a given state (case-insensitive).
    pub fn running_count_by_state(&self, state: &str) -> u32 {
        let normalized = state.trim().to_lowercase();
        self.running
            .values()
            .filter(|e| e.issue.state.trim().to_lowercase() == normalized)
            .count() as u32
    }
}

/// Create a `LogEntry` from an `AgentEvent` for the event log.
pub fn create_log_entry(seq: u64, event: &AgentEvent) -> LogEntry {
    let (event_type, message, tokens) = extract_log_fields(event);
    LogEntry {
        seq,
        timestamp: Utc::now(),
        event_type,
        message,
        tokens,
    }
}

/// Extract event type, message, and token snapshot from an AgentEvent.
fn extract_log_fields(event: &AgentEvent) -> (String, Option<String>, Option<TokenSnapshot>) {
    match event {
        AgentEvent::SessionStarted { session_id, .. } => (
            "session_started".to_string(),
            Some(format!("Session {session_id} started")),
            None,
        ),
        AgentEvent::TurnCompleted { message } => {
            ("turn_completed".to_string(), message.clone(), None)
        }
        AgentEvent::TurnFailed { error } => ("turn_failed".to_string(), Some(error.clone()), None),
        AgentEvent::Notification { message } => {
            ("notification".to_string(), Some(message.clone()), None)
        }
        AgentEvent::TokenUsage {
            input_tokens,
            output_tokens,
            total_tokens,
        } => (
            "token_usage".to_string(),
            None,
            Some(TokenSnapshot {
                input_tokens: *input_tokens,
                output_tokens: *output_tokens,
                total_tokens: *total_tokens,
            }),
        ),
        AgentEvent::AgentMessage {
            event_type,
            message,
        } => (event_type.clone(), message.clone(), None),
        AgentEvent::RateLimitUpdate { data } => {
            ("rate_limit_event".to_string(), Some(data.to_string()), None)
        }
    }
}

/// Check whether a rate-limit event should be logged to the event stream.
///
/// Returns `true` only when the event is interesting: the status is NOT
/// `"allowed"` (i.e. the agent is being rate-limited or throttled).
/// Routine "allowed" pings are filtered out to reduce dashboard noise.
pub fn should_log_rate_limit(data: &serde_json::Value) -> bool {
    let status = data
        .get("rate_limit_info")
        .and_then(|info| info.get("status"))
        .and_then(|s| s.as_str())
        .unwrap_or("");
    status != "allowed"
}

/// Append a log entry to the event log with bounded eviction.
pub fn append_log_entry(event_log: &mut VecDeque<LogEntry>, entry: LogEntry) {
    event_log.push_back(entry);
    while event_log.len() > MAX_LOG_ENTRIES {
        event_log.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── OrchestratorState::new ───────────────────────────────────────

    #[test]
    fn orchestrator_state_new_sets_fields() {
        let state = OrchestratorState::new(5000, 4);
        assert_eq!(state.poll_interval_ms, 5000);
        assert_eq!(state.max_concurrent_agents, 4);
        assert!(state.running.is_empty());
        assert!(state.claimed.is_empty());
        assert!(state.retry_attempts.is_empty());
        assert!(state.waiting_for_review.is_empty());
        assert_eq!(state.completed_count(), 0);
        assert_eq!(state.agent_totals.input_tokens, 0);
        assert_eq!(state.agent_totals.output_tokens, 0);
        assert_eq!(state.agent_totals.total_tokens, 0);
        assert!(state.rate_limits.is_none());
    }

    // ── running_count ────────────────────────────────────────────────

    #[test]
    fn running_count_empty() {
        let state = OrchestratorState::new(1000, 2);
        assert_eq!(state.running_count(), 0);
    }

    // ── available_slots ──────────────────────────────────────────────

    #[test]
    fn available_slots_all_free() {
        let state = OrchestratorState::new(1000, 3);
        assert_eq!(state.available_slots(), 3);
    }

    #[test]
    fn available_slots_some_used() {
        let mut state = OrchestratorState::new(1000, 3);
        insert_running_entry(&mut state, "a", "in_progress");
        assert_eq!(state.available_slots(), 2);
    }

    #[test]
    fn available_slots_saturates_at_zero() {
        let mut state = OrchestratorState::new(1000, 1);
        insert_running_entry(&mut state, "a", "in_progress");
        insert_running_entry(&mut state, "b", "in_progress");
        // running (2) > max (1) → saturating_sub returns 0
        assert_eq!(state.available_slots(), 0);
    }

    // ── running_count_by_state ───────────────────────────────────────

    #[test]
    fn running_count_by_state_no_matches() {
        let mut state = OrchestratorState::new(1000, 5);
        insert_running_entry(&mut state, "a", "in_progress");
        assert_eq!(state.running_count_by_state("done"), 0);
    }

    #[test]
    fn running_count_by_state_exact_match() {
        let mut state = OrchestratorState::new(1000, 5);
        insert_running_entry(&mut state, "a", "in_progress");
        insert_running_entry(&mut state, "b", "done");
        insert_running_entry(&mut state, "c", "in_progress");
        assert_eq!(state.running_count_by_state("in_progress"), 2);
        assert_eq!(state.running_count_by_state("done"), 1);
    }

    #[test]
    fn running_count_by_state_case_insensitive() {
        let mut state = OrchestratorState::new(1000, 5);
        insert_running_entry(&mut state, "a", "In_Progress");
        insert_running_entry(&mut state, "b", "IN_PROGRESS");
        assert_eq!(state.running_count_by_state("in_progress"), 2);
        assert_eq!(state.running_count_by_state("IN_PROGRESS"), 2);
    }

    #[test]
    fn running_count_by_state_trims_whitespace() {
        let mut state = OrchestratorState::new(1000, 5);
        insert_running_entry(&mut state, "a", "  review  ");
        assert_eq!(state.running_count_by_state(" review "), 1);
    }

    // ── mark_completed / bounded eviction ──────────────────────────

    #[test]
    fn mark_completed_basic() {
        let mut state = OrchestratorState::new(1000, 2);
        state.mark_completed("a".to_string());
        state.mark_completed("b".to_string());
        assert!(state.is_completed("a"));
        assert!(state.is_completed("b"));
        assert!(!state.is_completed("c"));
        assert_eq!(state.completed_count(), 2);
    }

    #[test]
    fn mark_completed_duplicate_is_idempotent() {
        let mut state = OrchestratorState::new(1000, 2);
        state.mark_completed("a".to_string());
        state.mark_completed("a".to_string());
        assert_eq!(state.completed_count(), 1);
    }

    #[test]
    fn mark_completed_evicts_oldest_beyond_limit() {
        let mut state = OrchestratorState::new(1000, 2);
        // Insert MAX_COMPLETED_ENTRIES + 10 items
        for i in 0..super::MAX_COMPLETED_ENTRIES + 10 {
            state.mark_completed(format!("issue-{i}"));
        }
        // Should be capped at MAX_COMPLETED_ENTRIES
        assert_eq!(state.completed_count(), super::MAX_COMPLETED_ENTRIES);
        // Oldest entries should be evicted
        assert!(!state.is_completed("issue-0"));
        assert!(!state.is_completed("issue-9"));
        // Recent entries should be present
        let last = super::MAX_COMPLETED_ENTRIES + 9;
        assert!(state.is_completed(&format!("issue-{last}")));
    }

    // ── LogEntry / TokenSnapshot ────────────────────────────────────

    #[test]
    fn log_entry_serializes_to_json() {
        let entry = LogEntry {
            seq: 1,
            timestamp: Utc::now(),
            event_type: "session_started".to_string(),
            message: Some("hello".to_string()),
            tokens: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"seq\":1"));
        assert!(json.contains("\"event_type\":\"session_started\""));
        assert!(json.contains("\"message\":\"hello\""));
    }

    #[test]
    fn log_entry_with_tokens_serializes() {
        let entry = LogEntry {
            seq: 5,
            timestamp: Utc::now(),
            event_type: "token_usage".to_string(),
            message: None,
            tokens: Some(TokenSnapshot {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
            }),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"input_tokens\":100"));
        assert!(json.contains("\"output_tokens\":50"));
        assert!(json.contains("\"total_tokens\":150"));
    }

    #[test]
    fn token_snapshot_serializes() {
        let snap = TokenSnapshot {
            input_tokens: 10,
            output_tokens: 20,
            total_tokens: 30,
        };
        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains("\"input_tokens\":10"));
    }

    // ── create_log_entry ─────────────────────────────────────────────

    #[test]
    fn create_log_entry_session_started() {
        let event = AgentEvent::SessionStarted {
            session_id: "sess-1".to_string(),
            pid: Some(42),
        };
        let entry = create_log_entry(1, &event);
        assert_eq!(entry.seq, 1);
        assert_eq!(entry.event_type, "session_started");
        assert!(entry.message.unwrap().contains("sess-1"));
        assert!(entry.tokens.is_none());
    }

    #[test]
    fn create_log_entry_turn_completed() {
        let event = AgentEvent::TurnCompleted {
            message: Some("done".to_string()),
        };
        let entry = create_log_entry(2, &event);
        assert_eq!(entry.event_type, "turn_completed");
        assert_eq!(entry.message, Some("done".to_string()));
        assert!(entry.tokens.is_none());
    }

    #[test]
    fn create_log_entry_turn_completed_no_message() {
        let event = AgentEvent::TurnCompleted { message: None };
        let entry = create_log_entry(3, &event);
        assert_eq!(entry.event_type, "turn_completed");
        assert!(entry.message.is_none());
    }

    #[test]
    fn create_log_entry_turn_failed() {
        let event = AgentEvent::TurnFailed {
            error: "boom".to_string(),
        };
        let entry = create_log_entry(4, &event);
        assert_eq!(entry.event_type, "turn_failed");
        assert_eq!(entry.message, Some("boom".to_string()));
    }

    #[test]
    fn create_log_entry_notification() {
        let event = AgentEvent::Notification {
            message: "heads up".to_string(),
        };
        let entry = create_log_entry(5, &event);
        assert_eq!(entry.event_type, "notification");
        assert_eq!(entry.message, Some("heads up".to_string()));
    }

    #[test]
    fn create_log_entry_token_usage() {
        let event = AgentEvent::TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
        };
        let entry = create_log_entry(6, &event);
        assert_eq!(entry.event_type, "token_usage");
        assert!(entry.message.is_none());
        let tokens = entry.tokens.unwrap();
        assert_eq!(tokens.input_tokens, 100);
        assert_eq!(tokens.output_tokens, 50);
        assert_eq!(tokens.total_tokens, 150);
    }

    #[test]
    fn create_log_entry_agent_message() {
        let event = AgentEvent::AgentMessage {
            event_type: "tool_use".to_string(),
            message: Some("using grep".to_string()),
        };
        let entry = create_log_entry(7, &event);
        assert_eq!(entry.event_type, "tool_use");
        assert_eq!(entry.message, Some("using grep".to_string()));
        assert!(entry.tokens.is_none());
    }

    #[test]
    fn create_log_entry_agent_message_no_message() {
        let event = AgentEvent::AgentMessage {
            event_type: "thinking".to_string(),
            message: None,
        };
        let entry = create_log_entry(8, &event);
        assert_eq!(entry.event_type, "thinking");
        assert!(entry.message.is_none());
    }

    #[test]
    fn create_log_entry_rate_limit_update() {
        let data = serde_json::json!({"requests_remaining": 42});
        let event = AgentEvent::RateLimitUpdate { data };
        let entry = create_log_entry(9, &event);
        assert_eq!(entry.event_type, "rate_limit_event");
        assert!(entry.message.unwrap().contains("42"));
        assert!(entry.tokens.is_none());
    }

    // ── should_log_rate_limit ───────────────────────────────────────

    #[test]
    fn rate_limit_allowed_is_not_logged() {
        let data = serde_json::json!({
            "rate_limit_info": { "status": "allowed" },
            "type": "rate_limit_event"
        });
        assert!(!should_log_rate_limit(&data));
    }

    #[test]
    fn rate_limit_rejected_is_logged() {
        let data = serde_json::json!({
            "rate_limit_info": { "status": "rejected" },
            "type": "rate_limit_event"
        });
        assert!(should_log_rate_limit(&data));
    }

    #[test]
    fn rate_limit_throttled_is_logged() {
        let data = serde_json::json!({
            "rate_limit_info": { "status": "throttled" },
            "type": "rate_limit_event"
        });
        assert!(should_log_rate_limit(&data));
    }

    #[test]
    fn rate_limit_missing_info_is_logged() {
        let data = serde_json::json!({"requests_remaining": 42});
        assert!(should_log_rate_limit(&data));
    }

    #[test]
    fn rate_limit_missing_status_is_logged() {
        let data = serde_json::json!({
            "rate_limit_info": { "isUsingOverage": false }
        });
        assert!(should_log_rate_limit(&data));
    }

    #[test]
    fn rate_limit_empty_status_is_logged() {
        let data = serde_json::json!({
            "rate_limit_info": { "status": "" }
        });
        assert!(should_log_rate_limit(&data));
    }

    // ── append_log_entry / bounded eviction ──────────────────────────

    #[test]
    fn append_log_entry_adds_entry() {
        let mut log = VecDeque::new();
        let entry = LogEntry {
            seq: 1,
            timestamp: Utc::now(),
            event_type: "test".to_string(),
            message: None,
            tokens: None,
        };
        append_log_entry(&mut log, entry);
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].seq, 1);
    }

    #[test]
    fn append_log_entry_evicts_oldest_beyond_limit() {
        let mut log = VecDeque::new();
        for i in 0..MAX_LOG_ENTRIES + 10 {
            let entry = LogEntry {
                seq: i as u64,
                timestamp: Utc::now(),
                event_type: "test".to_string(),
                message: None,
                tokens: None,
            };
            append_log_entry(&mut log, entry);
        }
        assert_eq!(log.len(), MAX_LOG_ENTRIES);
        // Oldest should be evicted
        assert_eq!(log.front().unwrap().seq, 10);
        let last = (MAX_LOG_ENTRIES + 9) as u64;
        assert_eq!(log.back().unwrap().seq, last);
    }

    // ── OrchestratorState::new includes event_broadcasts ─────────────

    #[test]
    fn orchestrator_state_new_has_empty_broadcasts() {
        let state = OrchestratorState::new(5000, 4);
        assert!(state.event_broadcasts.is_empty());
    }

    // ── pending_jobs and job_notify ─────────────────────────────────

    #[test]
    fn orchestrator_state_pending_jobs_empty_on_init() {
        let state = OrchestratorState::new(5000, 4);
        assert!(state.pending_jobs.is_empty());
    }

    #[test]
    fn orchestrator_state_job_notify_exists() {
        let state = OrchestratorState::new(5000, 4);
        // Notify should not block — just verifying it exists and is usable
        state.job_notify.notify_one();
    }

    #[test]
    fn orchestrator_state_pending_jobs_push_pop() {
        let mut state = OrchestratorState::new(5000, 4);
        let assignment = WorkAssignment {
            issue_id: "id1".to_string(),
            issue_identifier: "TST-1".to_string(),
            prompt: "test".to_string(),
            attempt: None,
            workspace_path: None,
        };
        state.pending_jobs.push_back(assignment);
        assert_eq!(state.pending_jobs.len(), 1);
        let popped = state.pending_jobs.pop_front().unwrap();
        assert_eq!(popped.issue_id, "id1");
        assert!(state.pending_jobs.is_empty());
    }

    // ── OrchestratorState Debug impl ─────────────────────────────────

    #[test]
    fn orchestrator_state_debug_impl() {
        let state = OrchestratorState::new(5000, 4);
        let debug = format!("{:?}", state);
        assert!(debug.contains("OrchestratorState"));
        assert!(debug.contains("event_broadcasts"));
        assert!(debug.contains("waiting_for_review"));
        assert!(debug.contains("pending_jobs"));
        assert!(debug.contains("<0 pending>"));
        assert!(debug.contains("state_broadcast"));
        assert!(debug.contains("receivers"));
    }

    // ── state_broadcast / notify_state_change ──────────────────────

    #[test]
    fn orchestrator_state_has_state_broadcast() {
        let state = OrchestratorState::new(5000, 4);
        // Should be able to subscribe
        let _rx = state.state_broadcast.subscribe();
        assert_eq!(state.state_broadcast.receiver_count(), 1);
    }

    #[test]
    fn notify_state_change_sends_signal() {
        let state = OrchestratorState::new(5000, 4);
        let mut rx = state.state_broadcast.subscribe();
        state.notify_state_change();
        // Should receive the signal synchronously
        let result = rx.try_recv();
        assert!(result.is_ok());
    }

    #[test]
    fn notify_state_change_no_receivers_does_not_panic() {
        let state = OrchestratorState::new(5000, 4);
        // No subscribers — should not panic
        state.notify_state_change();
    }

    #[test]
    fn state_broadcast_capacity_constant() {
        assert_eq!(STATE_BROADCAST_CAPACITY, 64);
    }

    // ── WaitingEntry ─────────────────────────────────────────────────

    #[test]
    fn waiting_entry_serializes() {
        let entry = WaitingEntry {
            issue_id: "id1".to_string(),
            identifier: "PROJ-1".to_string(),
            pr_number: 42,
            branch: "symphony/proj-1".to_string(),
            started_waiting_at: Utc::now(),
            session_id: Some("sess-abc".to_string()),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"issue_id\":\"id1\""));
        assert!(json.contains("\"pr_number\":42"));
        assert!(json.contains("\"branch\":\"symphony/proj-1\""));
    }

    #[test]
    fn waiting_entry_clone_and_debug() {
        let entry = WaitingEntry {
            issue_id: "id1".to_string(),
            identifier: "PROJ-1".to_string(),
            pr_number: 42,
            branch: "symphony/proj-1".to_string(),
            started_waiting_at: Utc::now(),
            session_id: None,
        };
        let cloned = entry.clone();
        assert_eq!(cloned.issue_id, "id1");
        assert_eq!(cloned.pr_number, 42);
        let debug = format!("{:?}", entry);
        assert!(debug.contains("WaitingEntry"));
    }

    #[test]
    fn waiting_for_review_operations() {
        let mut state = OrchestratorState::new(5000, 4);
        assert!(state.waiting_for_review.is_empty());

        state.waiting_for_review.insert(
            "id1".to_string(),
            WaitingEntry {
                issue_id: "id1".to_string(),
                identifier: "PROJ-1".to_string(),
                pr_number: 42,
                branch: "symphony/proj-1".to_string(),
                started_waiting_at: Utc::now(),
                session_id: None,
            },
        );
        assert_eq!(state.waiting_for_review.len(), 1);
        assert!(state.waiting_for_review.contains_key("id1"));

        state.waiting_for_review.remove("id1");
        assert!(state.waiting_for_review.is_empty());
    }

    // ── tracked_workers ─────────────────────────────────────────────

    #[test]
    fn orchestrator_state_tracked_workers_empty_on_init() {
        let state = OrchestratorState::new(5000, 4);
        assert!(state.tracked_workers.is_empty());
    }

    #[test]
    fn update_worker_heartbeat_inserts_new_worker() {
        let mut state = OrchestratorState::new(5000, 4);
        let hb = WorkerHeartbeat {
            worker_id: "w-1".to_string(),
            active_jobs: vec!["TST-1".to_string()],
            timestamp: Utc::now(),
        };
        state.update_worker_heartbeat(&hb);
        assert_eq!(state.tracked_workers.len(), 1);
        let w = state.tracked_workers.get("w-1").unwrap();
        assert_eq!(w.worker_id, "w-1");
        assert_eq!(w.active_jobs, vec!["TST-1".to_string()]);
        assert!(w.alive);
    }

    #[test]
    fn update_worker_heartbeat_updates_existing_worker() {
        let mut state = OrchestratorState::new(5000, 4);
        let hb1 = WorkerHeartbeat {
            worker_id: "w-1".to_string(),
            active_jobs: vec!["TST-1".to_string()],
            timestamp: Utc::now(),
        };
        state.update_worker_heartbeat(&hb1);

        let hb2 = WorkerHeartbeat {
            worker_id: "w-1".to_string(),
            active_jobs: vec!["TST-2".to_string(), "TST-3".to_string()],
            timestamp: Utc::now(),
        };
        state.update_worker_heartbeat(&hb2);
        assert_eq!(state.tracked_workers.len(), 1);
        let w = state.tracked_workers.get("w-1").unwrap();
        assert_eq!(w.active_jobs.len(), 2);
    }

    #[test]
    fn update_worker_heartbeat_revives_dead_worker() {
        let mut state = OrchestratorState::new(5000, 4);
        let hb = WorkerHeartbeat {
            worker_id: "w-1".to_string(),
            active_jobs: vec![],
            timestamp: Utc::now(),
        };
        state.update_worker_heartbeat(&hb);
        state.mark_worker_dead("w-1");
        assert!(!state.tracked_workers.get("w-1").unwrap().alive);

        // Heartbeat revives
        let hb2 = WorkerHeartbeat {
            worker_id: "w-1".to_string(),
            active_jobs: vec![],
            timestamp: Utc::now(),
        };
        state.update_worker_heartbeat(&hb2);
        assert!(state.tracked_workers.get("w-1").unwrap().alive);
    }

    #[test]
    fn register_worker_inserts_new_worker() {
        let mut state = OrchestratorState::new(5000, 4);
        let reg = WorkerRegistration {
            worker_id: "w-1".to_string(),
            capabilities: vec!["rust".to_string()],
            max_concurrent_jobs: 2,
        };
        state.register_worker(&reg);
        assert_eq!(state.tracked_workers.len(), 1);
        let w = state.tracked_workers.get("w-1").unwrap();
        assert_eq!(w.capabilities, vec!["rust".to_string()]);
        assert_eq!(w.max_concurrent_jobs, 2);
        assert!(w.alive);
    }

    #[test]
    fn register_worker_updates_existing_worker() {
        let mut state = OrchestratorState::new(5000, 4);
        let reg1 = WorkerRegistration {
            worker_id: "w-1".to_string(),
            capabilities: vec!["rust".to_string()],
            max_concurrent_jobs: 2,
        };
        state.register_worker(&reg1);

        let reg2 = WorkerRegistration {
            worker_id: "w-1".to_string(),
            capabilities: vec!["rust".to_string(), "python".to_string()],
            max_concurrent_jobs: 4,
        };
        state.register_worker(&reg2);
        assert_eq!(state.tracked_workers.len(), 1);
        let w = state.tracked_workers.get("w-1").unwrap();
        assert_eq!(w.capabilities.len(), 2);
        assert_eq!(w.max_concurrent_jobs, 4);
    }

    #[test]
    fn mark_worker_dead_sets_alive_false() {
        let mut state = OrchestratorState::new(5000, 4);
        let hb = WorkerHeartbeat {
            worker_id: "w-1".to_string(),
            active_jobs: vec![],
            timestamp: Utc::now(),
        };
        state.update_worker_heartbeat(&hb);
        state.mark_worker_dead("w-1");
        assert!(!state.tracked_workers.get("w-1").unwrap().alive);
    }

    #[test]
    fn mark_worker_dead_nonexistent_is_noop() {
        let mut state = OrchestratorState::new(5000, 4);
        state.mark_worker_dead("no-such-worker");
        assert!(state.tracked_workers.is_empty());
    }

    // ── record_worker_claim / clear_worker_claim ────────────────────

    #[test]
    fn record_worker_claim_adds_job() {
        let mut state = OrchestratorState::new(5000, 4);
        let reg = WorkerRegistration {
            worker_id: "w-1".to_string(),
            capabilities: vec![],
            max_concurrent_jobs: 1,
        };
        state.register_worker(&reg);
        state.record_worker_claim("w-1", "TST-42");
        let w = state.tracked_workers.get("w-1").unwrap();
        assert_eq!(w.active_jobs, vec!["TST-42".to_string()]);
    }

    #[test]
    fn record_worker_claim_no_duplicate() {
        let mut state = OrchestratorState::new(5000, 4);
        let reg = WorkerRegistration {
            worker_id: "w-1".to_string(),
            capabilities: vec![],
            max_concurrent_jobs: 1,
        };
        state.register_worker(&reg);
        state.record_worker_claim("w-1", "TST-42");
        state.record_worker_claim("w-1", "TST-42");
        let w = state.tracked_workers.get("w-1").unwrap();
        assert_eq!(w.active_jobs.len(), 1);
    }

    #[test]
    fn record_worker_claim_unknown_worker_is_noop() {
        let mut state = OrchestratorState::new(5000, 4);
        state.record_worker_claim("no-such-worker", "TST-42");
        assert!(state.tracked_workers.is_empty());
    }

    #[test]
    fn clear_worker_claim_removes_job() {
        let mut state = OrchestratorState::new(5000, 4);
        let reg = WorkerRegistration {
            worker_id: "w-1".to_string(),
            capabilities: vec![],
            max_concurrent_jobs: 2,
        };
        state.register_worker(&reg);
        state.record_worker_claim("w-1", "TST-42");
        state.record_worker_claim("w-1", "TST-43");
        state.clear_worker_claim("TST-42");
        let w = state.tracked_workers.get("w-1").unwrap();
        assert_eq!(w.active_jobs, vec!["TST-43".to_string()]);
    }

    #[test]
    fn clear_worker_claim_scans_all_workers() {
        let mut state = OrchestratorState::new(5000, 4);
        let reg1 = WorkerRegistration {
            worker_id: "w-1".to_string(),
            capabilities: vec![],
            max_concurrent_jobs: 1,
        };
        let reg2 = WorkerRegistration {
            worker_id: "w-2".to_string(),
            capabilities: vec![],
            max_concurrent_jobs: 1,
        };
        state.register_worker(&reg1);
        state.register_worker(&reg2);
        state.record_worker_claim("w-1", "TST-42");
        state.record_worker_claim("w-2", "TST-42");
        state.clear_worker_claim("TST-42");
        assert!(state
            .tracked_workers
            .get("w-1")
            .unwrap()
            .active_jobs
            .is_empty());
        assert!(state
            .tracked_workers
            .get("w-2")
            .unwrap()
            .active_jobs
            .is_empty());
    }

    #[test]
    fn clear_worker_claim_nonexistent_job_is_noop() {
        let mut state = OrchestratorState::new(5000, 4);
        let reg = WorkerRegistration {
            worker_id: "w-1".to_string(),
            capabilities: vec![],
            max_concurrent_jobs: 1,
        };
        state.register_worker(&reg);
        state.record_worker_claim("w-1", "TST-42");
        state.clear_worker_claim("TST-99");
        let w = state.tracked_workers.get("w-1").unwrap();
        assert_eq!(w.active_jobs, vec!["TST-42".to_string()]);
    }

    #[test]
    fn dead_workers_returns_stale_workers() {
        let mut state = OrchestratorState::new(5000, 4);
        let old_time = Utc::now() - chrono::Duration::seconds(300);
        let hb = WorkerHeartbeat {
            worker_id: "w-old".to_string(),
            active_jobs: vec![],
            timestamp: old_time,
        };
        state.update_worker_heartbeat(&hb);

        let recent_hb = WorkerHeartbeat {
            worker_id: "w-new".to_string(),
            active_jobs: vec![],
            timestamp: Utc::now(),
        };
        state.update_worker_heartbeat(&recent_hb);

        let dead = state.dead_workers(std::time::Duration::from_secs(120));
        assert_eq!(dead.len(), 1);
        assert_eq!(dead[0], "w-old");
    }

    #[test]
    fn dead_workers_excludes_already_dead() {
        let mut state = OrchestratorState::new(5000, 4);
        let old_time = Utc::now() - chrono::Duration::seconds(300);
        let hb = WorkerHeartbeat {
            worker_id: "w-old".to_string(),
            active_jobs: vec![],
            timestamp: old_time,
        };
        state.update_worker_heartbeat(&hb);
        state.mark_worker_dead("w-old");

        let dead = state.dead_workers(std::time::Duration::from_secs(120));
        assert!(dead.is_empty());
    }

    #[test]
    fn dead_workers_no_workers_returns_empty() {
        let state = OrchestratorState::new(5000, 4);
        let dead = state.dead_workers(std::time::Duration::from_secs(120));
        assert!(dead.is_empty());
    }

    #[test]
    fn orchestrator_state_debug_includes_tracked_workers() {
        let mut state = OrchestratorState::new(5000, 4);
        let hb = WorkerHeartbeat {
            worker_id: "w-1".to_string(),
            active_jobs: vec![],
            timestamp: Utc::now(),
        };
        state.update_worker_heartbeat(&hb);
        let debug = format!("{:?}", state);
        assert!(debug.contains("tracked_workers"));
        assert!(debug.contains("<1 workers>"));
    }

    // ── helpers ──────────────────────────────────────────────────────

    fn make_issue(id: &str, issue_state: &str) -> Issue {
        Issue {
            id: id.to_string(),
            identifier: format!("TST-{id}"),
            title: format!("Test issue {id}"),
            description: None,
            priority: None,
            state: issue_state.to_string(),
            branch_name: None,
            url: None,
            labels: vec![],
            blocked_by: vec![],
            created_at: None,
            updated_at: None,
        }
    }

    fn insert_running_entry(state: &mut OrchestratorState, id: &str, issue_state: &str) {
        let (cancel_tx, _cancel_rx) = tokio::sync::oneshot::channel();
        let issue = make_issue(id, issue_state);
        state.running.insert(
            id.to_string(),
            RunningEntry {
                identifier: issue.identifier.clone(),
                issue,
                session: LiveSession::default(),
                retry_attempt: None,
                started_at: Utc::now(),
                cancel_tx,
                event_log: VecDeque::new(),
                log_seq: 0,
            },
        );
    }

    fn make_completion_record(issue_id: &str, outcome: CompletionOutcome) -> CompletionRecord {
        CompletionRecord {
            issue_id: issue_id.to_string(),
            identifier: format!("TST-{issue_id}"),
            outcome,
            duration_seconds: 60.0,
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            turn_count: 3,
            completed_at: Utc::now(),
        }
    }

    // ── CompletionOutcome ───────────────────────────────────────────

    #[test]
    fn completion_outcome_serialization_roundtrip() {
        for outcome in [
            CompletionOutcome::Success,
            CompletionOutcome::InReview,
            CompletionOutcome::Retry,
            CompletionOutcome::Failed,
        ] {
            let json = serde_json::to_string(&outcome).unwrap();
            let deserialized: CompletionOutcome = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, outcome);
        }
    }

    #[test]
    fn completion_outcome_debug_and_clone() {
        let outcome = CompletionOutcome::Success;
        let cloned = outcome.clone();
        assert_eq!(outcome, cloned);
        let debug = format!("{:?}", outcome);
        assert!(debug.contains("Success"));
    }

    #[test]
    fn completion_outcome_partial_eq() {
        assert_eq!(CompletionOutcome::Success, CompletionOutcome::Success);
        assert_ne!(CompletionOutcome::Success, CompletionOutcome::Failed);
        assert_ne!(CompletionOutcome::InReview, CompletionOutcome::Retry);
    }

    // ── CompletionRecord ────────────────────────────────────────────

    #[test]
    fn completion_record_serialization_roundtrip() {
        let record = make_completion_record("id1", CompletionOutcome::Success);
        let json = serde_json::to_string(&record).unwrap();
        let deserialized: CompletionRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.issue_id, "id1");
        assert_eq!(deserialized.identifier, "TST-id1");
        assert_eq!(deserialized.outcome, CompletionOutcome::Success);
        assert!((deserialized.duration_seconds - 60.0).abs() < f64::EPSILON);
        assert_eq!(deserialized.input_tokens, 100);
        assert_eq!(deserialized.output_tokens, 50);
        assert_eq!(deserialized.total_tokens, 150);
        assert_eq!(deserialized.turn_count, 3);
    }

    #[test]
    fn completion_record_debug_and_clone() {
        let record = make_completion_record("id1", CompletionOutcome::InReview);
        let cloned = record.clone();
        assert_eq!(cloned.issue_id, "id1");
        assert_eq!(cloned.outcome, CompletionOutcome::InReview);
        let debug = format!("{:?}", record);
        assert!(debug.contains("CompletionRecord"));
        assert!(debug.contains("InReview"));
    }

    // ── add_completion / completion_history ──────────────────────────

    #[test]
    fn add_completion_record_basic() {
        let mut state = OrchestratorState::new(1000, 2);
        let record = make_completion_record("id1", CompletionOutcome::Success);
        state.add_completion(record);
        assert_eq!(state.completion_history().len(), 1);
        assert_eq!(state.completion_history()[0].issue_id, "id1");
    }

    #[test]
    fn add_completion_multiple_records() {
        let mut state = OrchestratorState::new(1000, 2);
        state.add_completion(make_completion_record("id1", CompletionOutcome::Success));
        state.add_completion(make_completion_record("id2", CompletionOutcome::Failed));
        state.add_completion(make_completion_record("id3", CompletionOutcome::InReview));
        assert_eq!(state.completion_history().len(), 3);
        assert_eq!(state.completion_history()[0].issue_id, "id1");
        assert_eq!(state.completion_history()[2].issue_id, "id3");
    }

    #[test]
    fn completion_history_bounded_eviction() {
        let mut state = OrchestratorState::new(1000, 2);
        for i in 0..MAX_COMPLETED_ENTRIES + 10 {
            state.add_completion(make_completion_record(
                &format!("id-{i}"),
                CompletionOutcome::Success,
            ));
        }
        assert_eq!(state.completion_history().len(), MAX_COMPLETED_ENTRIES);
        // Oldest entries should be evicted
        assert_eq!(state.completion_history()[0].issue_id, "id-10");
        let last = MAX_COMPLETED_ENTRIES + 9;
        assert_eq!(
            state.completion_history().back().unwrap().issue_id,
            format!("id-{last}")
        );
    }

    #[test]
    fn completion_history_empty_by_default() {
        let state = OrchestratorState::new(1000, 2);
        assert!(state.completion_history().is_empty());
    }

    // ── Debug impl includes completion_history ──────────────────────

    #[test]
    fn orchestrator_state_debug_includes_completion_history() {
        let mut state = OrchestratorState::new(5000, 4);
        state.add_completion(make_completion_record("id1", CompletionOutcome::Success));
        let debug = format!("{:?}", state);
        assert!(debug.contains("completion_history"));
        assert!(debug.contains("<1 records>"));
    }
}
