//! Worker-orchestrator communication types.
//!
//! This module centralizes the types that define the communication contract
//! between agent workers and the orchestrator, making the protocol explicit.

use crate::model::{AgentTotals, Issue, LogEntry, RetryEntry};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::VecDeque;

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
    /// Bounded event log for this issue (FIFO eviction at MAX_LOG_ENTRIES).
    pub event_log: VecDeque<LogEntry>,
    /// Monotonically increasing sequence number for log entries.
    pub log_seq: u64,
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

/// Snapshot of orchestrator state for observability.
#[derive(Debug, Clone, Serialize)]
pub struct OrchestratorSnapshot {
    pub running: Vec<RunningSessionInfo>,
    pub retrying: Vec<RetryEntry>,
    pub totals: AgentTotals,
    pub rate_limits: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunningSessionInfo {
    pub issue_id: String,
    pub issue_identifier: String,
    pub state: String,
    pub session_id: Option<String>,
    pub turn_count: u32,
    pub last_event: Option<String>,
    pub last_message: Option<String>,
    pub started_at: DateTime<Utc>,
    pub last_event_at: Option<DateTime<Utc>>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}
