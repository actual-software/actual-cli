use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

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

/// Run attempt status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RunStatus {
    PreparingWorkspace,
    BuildingPrompt,
    LaunchingAgentProcess,
    InitializingSession,
    StreamingTurn,
    Finishing,
    Succeeded,
    Failed,
    TimedOut,
    Stalled,
    CanceledByReconciliation,
}

impl std::fmt::Display for RunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PreparingWorkspace => write!(f, "preparing_workspace"),
            Self::BuildingPrompt => write!(f, "building_prompt"),
            Self::LaunchingAgentProcess => write!(f, "launching_agent"),
            Self::InitializingSession => write!(f, "initializing_session"),
            Self::StreamingTurn => write!(f, "streaming_turn"),
            Self::Finishing => write!(f, "finishing"),
            Self::Succeeded => write!(f, "succeeded"),
            Self::Failed => write!(f, "failed"),
            Self::TimedOut => write!(f, "timed_out"),
            Self::Stalled => write!(f, "stalled"),
            Self::CanceledByReconciliation => write!(f, "canceled_by_reconciliation"),
        }
    }
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

/// Orchestrator runtime state.
#[derive(Debug)]
pub struct OrchestratorState {
    pub poll_interval_ms: u64,
    pub max_concurrent_agents: u32,
    pub running: HashMap<String, RunningEntry>,
    pub claimed: HashSet<String>,
    pub retry_attempts: HashMap<String, RetryEntry>,
    pub completed: HashSet<String>,
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
            completed: HashSet::new(),
            agent_totals: AgentTotals::default(),
            rate_limits: None,
        }
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
}

/// Worker exit reason reported back to orchestrator.
#[derive(Debug, Clone)]
pub enum WorkerExitReason {
    Normal,
    Failed(String),
    TimedOut,
    Stalled,
    Cancelled,
}
