//! Worker-orchestrator communication types.
//!
//! This module centralizes the types that define the communication contract
//! between agent workers and the orchestrator, making the protocol explicit.

use crate::model::{AgentTotals, Issue, LogEntry, RetryEntry, WaitingEntry};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Live session metadata for a running coding agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// Assignment payload sent to a remote worker via GET /api/v1/work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkAssignment {
    pub issue_id: String,
    pub issue_identifier: String,
    pub prompt: String,
    pub attempt: Option<u32>,
    pub workspace_path: Option<String>,
}

/// Payload posted by a worker to report an agent event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerEventPayload {
    pub event: AgentEvent,
}

/// Payload posted by a worker to signal completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkResult {
    pub reason: WorkerExitReason,
}

/// Snapshot of orchestrator state for observability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorSnapshot {
    pub running: Vec<RunningSessionInfo>,
    pub retrying: Vec<RetryEntry>,
    pub waiting: Vec<WaitingEntry>,
    pub totals: AgentTotals,
    pub rate_limits: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentTotals, RetryEntry};
    use std::collections::VecDeque;

    // ── LiveSession ──────────────────────────────────────────────────

    #[test]
    fn live_session_default() {
        let session = LiveSession::default();
        assert!(session.session_id.is_none());
        assert!(session.agent_pid.is_none());
        assert!(session.last_event.is_none());
        assert!(session.last_event_at.is_none());
        assert!(session.last_message.is_none());
        assert_eq!(session.input_tokens, 0);
        assert_eq!(session.output_tokens, 0);
        assert_eq!(session.total_tokens, 0);
        assert_eq!(session.last_reported_input_tokens, 0);
        assert_eq!(session.last_reported_output_tokens, 0);
        assert_eq!(session.last_reported_total_tokens, 0);
        assert_eq!(session.turn_count, 0);
    }

    #[test]
    fn live_session_clone() {
        let session = LiveSession {
            session_id: Some("sess-1".to_string()),
            agent_pid: Some(42),
            turn_count: 5,
            ..LiveSession::default()
        };
        let cloned = session.clone();
        assert_eq!(cloned.session_id, Some("sess-1".to_string()));
        assert_eq!(cloned.agent_pid, Some(42));
        assert_eq!(cloned.turn_count, 5);
    }

    #[test]
    fn live_session_debug() {
        let session = LiveSession::default();
        let debug = format!("{:?}", session);
        assert!(debug.contains("LiveSession"));
    }

    #[test]
    fn live_session_serialize_deserialize_roundtrip() {
        let session = LiveSession {
            session_id: Some("sess-abc".to_string()),
            agent_pid: Some(1234),
            last_event: Some("turn_completed".to_string()),
            last_event_at: Some(Utc::now()),
            last_message: Some("Done".to_string()),
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            last_reported_input_tokens: 80,
            last_reported_output_tokens: 40,
            last_reported_total_tokens: 120,
            turn_count: 3,
        };
        let json = serde_json::to_string(&session).unwrap();
        let deserialized: LiveSession = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.session_id, Some("sess-abc".to_string()));
        assert_eq!(deserialized.agent_pid, Some(1234));
        assert_eq!(deserialized.input_tokens, 100);
        assert_eq!(deserialized.output_tokens, 50);
        assert_eq!(deserialized.total_tokens, 150);
        assert_eq!(deserialized.turn_count, 3);
    }

    // ── RunningEntry ─────────────────────────────────────────────────

    #[test]
    fn running_entry_debug() {
        let (cancel_tx, _cancel_rx) = tokio::sync::oneshot::channel();
        let entry = RunningEntry {
            issue: Issue {
                id: "id1".to_string(),
                identifier: "TST-1".to_string(),
                title: "Test".to_string(),
                description: None,
                priority: None,
                state: "Todo".to_string(),
                branch_name: None,
                url: None,
                labels: vec![],
                blocked_by: vec![],
                created_at: None,
                updated_at: None,
            },
            identifier: "TST-1".to_string(),
            session: LiveSession::default(),
            retry_attempt: None,
            started_at: Utc::now(),
            cancel_tx,
            event_log: VecDeque::new(),
            log_seq: 0,
        };
        let debug = format!("{:?}", entry);
        assert!(debug.contains("RunningEntry"));
        assert!(debug.contains("TST-1"));
    }

    #[test]
    fn running_entry_fields_accessible() {
        let (cancel_tx, _cancel_rx) = tokio::sync::oneshot::channel();
        let entry = RunningEntry {
            issue: Issue {
                id: "id1".to_string(),
                identifier: "TST-1".to_string(),
                title: "Test".to_string(),
                description: None,
                priority: None,
                state: "In Progress".to_string(),
                branch_name: None,
                url: None,
                labels: vec![],
                blocked_by: vec![],
                created_at: None,
                updated_at: None,
            },
            identifier: "TST-1".to_string(),
            session: LiveSession::default(),
            retry_attempt: Some(2),
            started_at: Utc::now(),
            cancel_tx,
            event_log: VecDeque::new(),
            log_seq: 5,
        };
        assert_eq!(entry.identifier, "TST-1");
        assert_eq!(entry.retry_attempt, Some(2));
        assert_eq!(entry.log_seq, 5);
        assert_eq!(entry.issue.state, "In Progress");
    }

    // ── AgentEvent ───────────────────────────────────────────────────

    #[test]
    fn agent_event_session_started_roundtrip() {
        let event = AgentEvent::SessionStarted {
            session_id: "sess-1".to_string(),
            pid: Some(42),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            AgentEvent::SessionStarted { ref session_id, pid }
            if session_id == "sess-1" && pid == Some(42)
        ));
    }

    #[test]
    fn agent_event_session_started_no_pid() {
        let event = AgentEvent::SessionStarted {
            session_id: "s".to_string(),
            pid: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            AgentEvent::SessionStarted { pid: None, .. }
        ));
    }

    #[test]
    fn agent_event_turn_completed_roundtrip() {
        let event = AgentEvent::TurnCompleted {
            message: Some("done".to_string()),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            AgentEvent::TurnCompleted { ref message }
            if message.as_deref() == Some("done")
        ));
    }

    #[test]
    fn agent_event_turn_completed_no_message() {
        let event = AgentEvent::TurnCompleted { message: None };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            AgentEvent::TurnCompleted { message: None }
        ));
    }

    #[test]
    fn agent_event_turn_failed_roundtrip() {
        let event = AgentEvent::TurnFailed {
            error: "boom".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            AgentEvent::TurnFailed { ref error }
            if error == "boom"
        ));
    }

    #[test]
    fn agent_event_notification_roundtrip() {
        let event = AgentEvent::Notification {
            message: "heads up".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            AgentEvent::Notification { ref message }
            if message == "heads up"
        ));
    }

    #[test]
    fn agent_event_token_usage_roundtrip() {
        let event = AgentEvent::TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            AgentEvent::TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
            }
        ));
    }

    #[test]
    fn agent_event_agent_message_roundtrip() {
        let event = AgentEvent::AgentMessage {
            event_type: "tool_use".to_string(),
            message: Some("using grep".to_string()),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            AgentEvent::AgentMessage { ref event_type, ref message }
            if event_type == "tool_use" && message.as_deref() == Some("using grep")
        ));
    }

    #[test]
    fn agent_event_agent_message_no_message() {
        let event = AgentEvent::AgentMessage {
            event_type: "thinking".to_string(),
            message: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            AgentEvent::AgentMessage { message: None, .. }
        ));
    }

    #[test]
    fn agent_event_rate_limit_update_roundtrip() {
        let data = serde_json::json!({"requests_remaining": 42});
        let event = AgentEvent::RateLimitUpdate { data };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            AgentEvent::RateLimitUpdate { ref data }
            if data["requests_remaining"] == 42
        ));
    }

    #[test]
    fn agent_event_clone() {
        let event = AgentEvent::TokenUsage {
            input_tokens: 1,
            output_tokens: 2,
            total_tokens: 3,
        };
        let cloned = event.clone();
        let json_original = serde_json::to_string(&event).unwrap();
        let json_cloned = serde_json::to_string(&cloned).unwrap();
        assert_eq!(json_original, json_cloned);
    }

    #[test]
    fn agent_event_debug() {
        let event = AgentEvent::Notification {
            message: "test".to_string(),
        };
        let debug = format!("{:?}", event);
        assert!(debug.contains("Notification"));
        assert!(debug.contains("test"));
    }

    // ── WorkerExitReason ─────────────────────────────────────────────

    #[test]
    fn worker_exit_reason_normal_roundtrip() {
        let reason = WorkerExitReason::Normal;
        let json = serde_json::to_string(&reason).unwrap();
        let deserialized: WorkerExitReason = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, WorkerExitReason::Normal);
    }

    #[test]
    fn worker_exit_reason_failed_roundtrip() {
        let reason = WorkerExitReason::Failed("oops".to_string());
        let json = serde_json::to_string(&reason).unwrap();
        let deserialized: WorkerExitReason = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, WorkerExitReason::Failed("oops".to_string()));
    }

    #[test]
    fn worker_exit_reason_timed_out_roundtrip() {
        let reason = WorkerExitReason::TimedOut;
        let json = serde_json::to_string(&reason).unwrap();
        let deserialized: WorkerExitReason = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, WorkerExitReason::TimedOut);
    }

    #[test]
    fn worker_exit_reason_stalled_roundtrip() {
        let reason = WorkerExitReason::Stalled;
        let json = serde_json::to_string(&reason).unwrap();
        let deserialized: WorkerExitReason = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, WorkerExitReason::Stalled);
    }

    #[test]
    fn worker_exit_reason_cancelled_roundtrip() {
        let reason = WorkerExitReason::Cancelled;
        let json = serde_json::to_string(&reason).unwrap();
        let deserialized: WorkerExitReason = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, WorkerExitReason::Cancelled);
    }

    #[test]
    fn worker_exit_reason_partial_eq() {
        assert_eq!(WorkerExitReason::Normal, WorkerExitReason::Normal);
        assert_ne!(WorkerExitReason::Normal, WorkerExitReason::TimedOut);
        assert_ne!(
            WorkerExitReason::Failed("a".to_string()),
            WorkerExitReason::Failed("b".to_string())
        );
        assert_eq!(
            WorkerExitReason::Failed("same".to_string()),
            WorkerExitReason::Failed("same".to_string())
        );
    }

    #[test]
    fn worker_exit_reason_clone() {
        let reason = WorkerExitReason::Failed("err".to_string());
        let cloned = reason.clone();
        assert_eq!(reason, cloned);
    }

    #[test]
    fn worker_exit_reason_debug() {
        let reason = WorkerExitReason::Stalled;
        let debug = format!("{:?}", reason);
        assert!(debug.contains("Stalled"));
    }

    // ── OrchestratorMessage ──────────────────────────────────────────

    #[test]
    fn orchestrator_message_worker_exited_debug() {
        let msg = OrchestratorMessage::WorkerExited {
            issue_id: "id1".to_string(),
            reason: WorkerExitReason::Normal,
        };
        let debug = format!("{:?}", msg);
        assert!(debug.contains("WorkerExited"));
        assert!(debug.contains("id1"));
    }

    #[test]
    fn orchestrator_message_agent_update_debug() {
        let msg = OrchestratorMessage::AgentUpdate {
            issue_id: "id2".to_string(),
            event: AgentEvent::Notification {
                message: "hi".to_string(),
            },
        };
        let debug = format!("{:?}", msg);
        assert!(debug.contains("AgentUpdate"));
        assert!(debug.contains("id2"));
    }

    #[test]
    fn orchestrator_message_retry_fired_debug() {
        let msg = OrchestratorMessage::RetryFired {
            issue_id: "id3".to_string(),
        };
        let debug = format!("{:?}", msg);
        assert!(debug.contains("RetryFired"));
        assert!(debug.contains("id3"));
    }

    #[test]
    fn orchestrator_message_trigger_refresh_debug() {
        let msg = OrchestratorMessage::TriggerRefresh;
        let debug = format!("{:?}", msg);
        assert!(debug.contains("TriggerRefresh"));
    }

    // ── OrchestratorSnapshot ─────────────────────────────────────────

    #[test]
    fn orchestrator_snapshot_serialize_deserialize_roundtrip() {
        let snapshot = OrchestratorSnapshot {
            running: vec![RunningSessionInfo {
                issue_id: "id1".to_string(),
                issue_identifier: "TST-1".to_string(),
                state: "In Progress".to_string(),
                session_id: Some("sess-1".to_string()),
                turn_count: 3,
                last_event: Some("turn_completed".to_string()),
                last_message: Some("Done".to_string()),
                started_at: Utc::now(),
                last_event_at: Some(Utc::now()),
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
            }],
            retrying: vec![RetryEntry {
                issue_id: "id2".to_string(),
                identifier: "TST-2".to_string(),
                attempt: 1,
                due_at_ms: 12345,
                error: Some("timeout".to_string()),
            }],
            waiting: vec![WaitingEntry {
                issue_id: "id3".to_string(),
                identifier: "TST-3".to_string(),
                pr_number: 99,
                branch: "symphony/tst-3".to_string(),
                started_waiting_at: Utc::now(),
            }],
            totals: AgentTotals {
                input_tokens: 200,
                output_tokens: 100,
                total_tokens: 300,
                seconds_running: 60.5,
            },
            rate_limits: Some(serde_json::json!({"remaining": 10})),
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        let deserialized: OrchestratorSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.running.len(), 1);
        assert_eq!(deserialized.running[0].issue_id, "id1");
        assert_eq!(deserialized.retrying.len(), 1);
        assert_eq!(deserialized.retrying[0].issue_id, "id2");
        assert_eq!(deserialized.waiting.len(), 1);
        assert_eq!(deserialized.waiting[0].issue_id, "id3");
        assert_eq!(deserialized.waiting[0].pr_number, 99);
        assert_eq!(deserialized.totals.input_tokens, 200);
        assert_eq!(deserialized.rate_limits.unwrap()["remaining"], 10);
    }

    #[test]
    fn orchestrator_snapshot_empty() {
        let snapshot = OrchestratorSnapshot {
            running: vec![],
            retrying: vec![],
            waiting: vec![],
            totals: AgentTotals::default(),
            rate_limits: None,
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        let deserialized: OrchestratorSnapshot = serde_json::from_str(&json).unwrap();
        assert!(deserialized.running.is_empty());
        assert!(deserialized.retrying.is_empty());
        assert!(deserialized.waiting.is_empty());
        assert!(deserialized.rate_limits.is_none());
    }

    #[test]
    fn orchestrator_snapshot_clone() {
        let snapshot = OrchestratorSnapshot {
            running: vec![],
            retrying: vec![],
            waiting: vec![],
            totals: AgentTotals::default(),
            rate_limits: None,
        };
        let cloned = snapshot.clone();
        assert_eq!(
            serde_json::to_string(&snapshot).unwrap(),
            serde_json::to_string(&cloned).unwrap()
        );
    }

    #[test]
    fn orchestrator_snapshot_debug() {
        let snapshot = OrchestratorSnapshot {
            running: vec![],
            retrying: vec![],
            waiting: vec![],
            totals: AgentTotals::default(),
            rate_limits: None,
        };
        let debug = format!("{:?}", snapshot);
        assert!(debug.contains("OrchestratorSnapshot"));
    }

    // ── RunningSessionInfo ───────────────────────────────────────────

    #[test]
    fn running_session_info_serialize_deserialize_roundtrip() {
        let info = RunningSessionInfo {
            issue_id: "id1".to_string(),
            issue_identifier: "TST-1".to_string(),
            state: "In Progress".to_string(),
            session_id: Some("sess-1".to_string()),
            turn_count: 5,
            last_event: Some("notification".to_string()),
            last_message: Some("Working".to_string()),
            started_at: Utc::now(),
            last_event_at: Some(Utc::now()),
            input_tokens: 500,
            output_tokens: 250,
            total_tokens: 750,
        };
        let json = serde_json::to_string(&info).unwrap();
        let deserialized: RunningSessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.issue_id, "id1");
        assert_eq!(deserialized.issue_identifier, "TST-1");
        assert_eq!(deserialized.state, "In Progress");
        assert_eq!(deserialized.session_id, Some("sess-1".to_string()));
        assert_eq!(deserialized.turn_count, 5);
        assert_eq!(deserialized.input_tokens, 500);
        assert_eq!(deserialized.output_tokens, 250);
        assert_eq!(deserialized.total_tokens, 750);
    }

    #[test]
    fn running_session_info_optional_fields_none() {
        let info = RunningSessionInfo {
            issue_id: "id1".to_string(),
            issue_identifier: "TST-1".to_string(),
            state: "Todo".to_string(),
            session_id: None,
            turn_count: 0,
            last_event: None,
            last_message: None,
            started_at: Utc::now(),
            last_event_at: None,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
        };
        let json = serde_json::to_string(&info).unwrap();
        let deserialized: RunningSessionInfo = serde_json::from_str(&json).unwrap();
        assert!(deserialized.session_id.is_none());
        assert!(deserialized.last_event.is_none());
        assert!(deserialized.last_message.is_none());
        assert!(deserialized.last_event_at.is_none());
    }

    #[test]
    fn running_session_info_clone() {
        let info = RunningSessionInfo {
            issue_id: "id1".to_string(),
            issue_identifier: "TST-1".to_string(),
            state: "Done".to_string(),
            session_id: None,
            turn_count: 0,
            last_event: None,
            last_message: None,
            started_at: Utc::now(),
            last_event_at: None,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
        };
        let cloned = info.clone();
        assert_eq!(info.issue_id, cloned.issue_id);
    }

    #[test]
    fn running_session_info_debug() {
        let info = RunningSessionInfo {
            issue_id: "id1".to_string(),
            issue_identifier: "TST-1".to_string(),
            state: "Todo".to_string(),
            session_id: None,
            turn_count: 0,
            last_event: None,
            last_message: None,
            started_at: Utc::now(),
            last_event_at: None,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
        };
        let debug = format!("{:?}", info);
        assert!(debug.contains("RunningSessionInfo"));
    }

    // ── WorkAssignment ──────────────────────────────────────────────

    #[test]
    fn work_assignment_serialize_deserialize_roundtrip() {
        let assignment = WorkAssignment {
            issue_id: "id-abc".to_string(),
            issue_identifier: "TST-42".to_string(),
            prompt: "Fix the bug in main.rs".to_string(),
            attempt: Some(2),
            workspace_path: Some("/tmp/ws/TST-42".to_string()),
        };
        let json = serde_json::to_string(&assignment).unwrap();
        let deserialized: WorkAssignment = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.issue_id, "id-abc");
        assert_eq!(deserialized.issue_identifier, "TST-42");
        assert_eq!(deserialized.prompt, "Fix the bug in main.rs");
        assert_eq!(deserialized.attempt, Some(2));
        assert_eq!(
            deserialized.workspace_path,
            Some("/tmp/ws/TST-42".to_string())
        );
    }

    #[test]
    fn work_assignment_optional_fields_none() {
        let assignment = WorkAssignment {
            issue_id: "id1".to_string(),
            issue_identifier: "TST-1".to_string(),
            prompt: "do stuff".to_string(),
            attempt: None,
            workspace_path: None,
        };
        let json = serde_json::to_string(&assignment).unwrap();
        let deserialized: WorkAssignment = serde_json::from_str(&json).unwrap();
        assert!(deserialized.attempt.is_none());
        assert!(deserialized.workspace_path.is_none());
    }

    #[test]
    fn work_assignment_empty_prompt() {
        let assignment = WorkAssignment {
            issue_id: "id1".to_string(),
            issue_identifier: "TST-1".to_string(),
            prompt: String::new(),
            attempt: None,
            workspace_path: None,
        };
        let json = serde_json::to_string(&assignment).unwrap();
        let deserialized: WorkAssignment = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.prompt, "");
    }

    #[test]
    fn work_assignment_clone() {
        let assignment = WorkAssignment {
            issue_id: "id1".to_string(),
            issue_identifier: "TST-1".to_string(),
            prompt: "test".to_string(),
            attempt: Some(1),
            workspace_path: None,
        };
        let cloned = assignment.clone();
        assert_eq!(cloned.issue_id, assignment.issue_id);
        assert_eq!(cloned.issue_identifier, assignment.issue_identifier);
        assert_eq!(cloned.prompt, assignment.prompt);
        assert_eq!(cloned.attempt, assignment.attempt);
    }

    #[test]
    fn work_assignment_debug() {
        let assignment = WorkAssignment {
            issue_id: "id1".to_string(),
            issue_identifier: "TST-1".to_string(),
            prompt: "test".to_string(),
            attempt: None,
            workspace_path: None,
        };
        let debug = format!("{:?}", assignment);
        assert!(debug.contains("WorkAssignment"));
        assert!(debug.contains("TST-1"));
    }

    // ── WorkerEventPayload ──────────────────────────────────────────

    #[test]
    fn worker_event_payload_serialize_deserialize_roundtrip() {
        let payload = WorkerEventPayload {
            event: AgentEvent::Notification {
                message: "hello".to_string(),
            },
        };
        let json = serde_json::to_string(&payload).unwrap();
        let deserialized: WorkerEventPayload = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized.event,
            AgentEvent::Notification { ref message }
            if message == "hello"
        ));
    }

    #[test]
    fn worker_event_payload_with_token_usage() {
        let payload = WorkerEventPayload {
            event: AgentEvent::TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
            },
        };
        let json = serde_json::to_string(&payload).unwrap();
        let deserialized: WorkerEventPayload = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized.event,
            AgentEvent::TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
            }
        ));
    }

    #[test]
    fn worker_event_payload_clone() {
        let payload = WorkerEventPayload {
            event: AgentEvent::TurnCompleted {
                message: Some("done".to_string()),
            },
        };
        let cloned = payload.clone();
        let json_original = serde_json::to_string(&payload).unwrap();
        let json_cloned = serde_json::to_string(&cloned).unwrap();
        assert_eq!(json_original, json_cloned);
    }

    #[test]
    fn worker_event_payload_debug() {
        let payload = WorkerEventPayload {
            event: AgentEvent::Notification {
                message: "test".to_string(),
            },
        };
        let debug = format!("{:?}", payload);
        assert!(debug.contains("WorkerEventPayload"));
        assert!(debug.contains("Notification"));
    }

    // ── WorkResult ──────────────────────────────────────────────────

    #[test]
    fn work_result_normal_roundtrip() {
        let result = WorkResult {
            reason: WorkerExitReason::Normal,
        };
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: WorkResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.reason, WorkerExitReason::Normal);
    }

    #[test]
    fn work_result_failed_roundtrip() {
        let result = WorkResult {
            reason: WorkerExitReason::Failed("something broke".to_string()),
        };
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: WorkResult = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.reason,
            WorkerExitReason::Failed("something broke".to_string())
        );
    }

    #[test]
    fn work_result_timed_out_roundtrip() {
        let result = WorkResult {
            reason: WorkerExitReason::TimedOut,
        };
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: WorkResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.reason, WorkerExitReason::TimedOut);
    }

    #[test]
    fn work_result_stalled_roundtrip() {
        let result = WorkResult {
            reason: WorkerExitReason::Stalled,
        };
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: WorkResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.reason, WorkerExitReason::Stalled);
    }

    #[test]
    fn work_result_cancelled_roundtrip() {
        let result = WorkResult {
            reason: WorkerExitReason::Cancelled,
        };
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: WorkResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.reason, WorkerExitReason::Cancelled);
    }

    #[test]
    fn work_result_clone() {
        let result = WorkResult {
            reason: WorkerExitReason::Failed("err".to_string()),
        };
        let cloned = result.clone();
        assert_eq!(result.reason, cloned.reason);
    }

    #[test]
    fn work_result_debug() {
        let result = WorkResult {
            reason: WorkerExitReason::Normal,
        };
        let debug = format!("{:?}", result);
        assert!(debug.contains("WorkResult"));
        assert!(debug.contains("Normal"));
    }
}
