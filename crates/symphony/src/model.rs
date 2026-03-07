use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

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
    pub config: serde_yaml::Value,
    /// Markdown body after front matter, trimmed.
    pub prompt_template: String,
}

/// Live session metadata for a running coding agent.
#[derive(Debug, Clone, Default, Serialize)]
pub struct LiveSession {
    pub session_id: Option<String>,
    pub agent_pid: Option<u32>,
    pub last_event: Option<String>,
    pub last_event_at: Option<DateTime<Utc>>,
    pub last_message: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub last_reported_input_tokens: u64,
    pub last_reported_output_tokens: u64,
    pub last_reported_total_tokens: u64,
    pub turn_count: u32,
}

/// Entry in the running map, tracking one active worker.
#[derive(Debug)]
pub struct RunningEntry {
    pub issue: Issue,
    pub identifier: String,
    pub session: LiveSession,
    pub retry_attempt: Option<u32>,
    pub started_at: DateTime<Utc>,
    pub cancel_tx: tokio::sync::oneshot::Sender<()>,
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
#[derive(Debug)]
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

/// Events emitted by the agent runner to the orchestrator.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    SessionStarted {
        session_id: String,
        pid: Option<u32>,
    },
    TurnCompleted {
        message: Option<String>,
    },
    TurnFailed {
        error: String,
    },
    Notification {
        message: String,
    },
    TokenUsage {
        input_tokens: u64,
        output_tokens: u64,
        total_tokens: u64,
    },
    AgentMessage {
        event_type: String,
        message: Option<String>,
    },
    /// §11.2: Rate limit information from Claude CLI.
    RateLimitUpdate {
        data: serde_json::Value,
    },
}

/// Worker exit reason reported back to orchestrator.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerExitReason {
    Normal,
    Failed(String),
    TimedOut,
    Stalled,
    Cancelled,
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
            },
        );
    }
}
