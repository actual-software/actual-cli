use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

// Re-export protocol types for backward compatibility.
pub use crate::protocol::{AgentEvent, LiveSession, RunningEntry, WorkerExitReason};

/// Maximum number of log entries to retain per running issue.
/// Older entries are evicted in FIFO order.
pub const MAX_LOG_ENTRIES: usize = 500;

/// Broadcast channel capacity for per-issue event streaming.
pub const BROADCAST_CHANNEL_CAPACITY: usize = 256;

/// A timestamped log entry for per-issue event history.
#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub seq: u64,
    pub timestamp: DateTime<Utc>,
    pub event_type: String,
    pub message: Option<String>,
    pub tokens: Option<TokenSnapshot>,
}

/// Snapshot of token usage at a point in time.
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
pub struct RetryEntry {
    pub issue_id: String,
    pub identifier: String,
    pub attempt: u32,
    pub due_at_ms: u64,
    pub error: Option<String>,
}

/// Aggregate token totals.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub seconds_running: f64,
}

/// Maximum number of completed issue IDs to retain for observability.
/// Older entries are evicted in FIFO order to prevent unbounded growth.
const MAX_COMPLETED_ENTRIES: usize = 1000;

/// Orchestrator runtime state.
pub struct OrchestratorState {
    pub poll_interval_ms: u64,
    pub max_concurrent_agents: u32,
    pub running: HashMap<String, RunningEntry>,
    pub claimed: HashSet<String>,
    pub retry_attempts: HashMap<String, RetryEntry>,
    /// Bounded set of recently-completed issue IDs (FIFO eviction).
    completed_set: HashSet<String>,
    completed_order: VecDeque<String>,
    pub agent_totals: AgentTotals,
    pub rate_limits: Option<serde_json::Value>,
    /// Per-issue broadcast channels for SSE event streaming.
    pub event_broadcasts: HashMap<String, tokio::sync::broadcast::Sender<LogEntry>>,
}

impl std::fmt::Debug for OrchestratorState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrchestratorState")
            .field("poll_interval_ms", &self.poll_interval_ms)
            .field("max_concurrent_agents", &self.max_concurrent_agents)
            .field("running", &self.running)
            .field("claimed", &self.claimed)
            .field("retry_attempts", &self.retry_attempts)
            .field("agent_totals", &self.agent_totals)
            .field("rate_limits", &self.rate_limits)
            .field(
                "event_broadcasts",
                &format!("<{} channels>", self.event_broadcasts.len()),
            )
            .finish()
    }
}

impl OrchestratorState {
    pub fn new(poll_interval_ms: u64, max_concurrent_agents: u32) -> Self {
        Self {
            poll_interval_ms,
            max_concurrent_agents,
            running: HashMap::new(),
            claimed: HashSet::new(),
            retry_attempts: HashMap::new(),
            completed_set: HashSet::new(),
            completed_order: VecDeque::new(),
            agent_totals: AgentTotals::default(),
            rate_limits: None,
            event_broadcasts: HashMap::new(),
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

    pub fn running_count(&self) -> usize {
        self.running.len()
    }

    pub fn available_slots(&self) -> u32 {
        let running = self.running_count() as u32;
        self.max_concurrent_agents.saturating_sub(running)
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

    // ── OrchestratorState Debug impl ─────────────────────────────────

    #[test]
    fn orchestrator_state_debug_impl() {
        let state = OrchestratorState::new(5000, 4);
        let debug = format!("{:?}", state);
        assert!(debug.contains("OrchestratorState"));
        assert!(debug.contains("event_broadcasts"));
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
}
