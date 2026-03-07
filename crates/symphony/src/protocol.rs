use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::model::AgentEvent;

/// Hooks to run before/after the agent session.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkAssignmentHooks {
    pub before_run: Option<String>,
    pub after_run: Option<String>,
}

/// Assignment issued by the orchestrator when a worker claims a job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkAssignment {
    pub issue_id: String,
    pub issue_identifier: String,
    pub issue_title: String,
    pub issue_description: Option<String>,
    pub issue_state: String,
    pub prompt: String,
    pub repo_url: String,
    pub branch_name: String,
    pub attempt: u32,
    pub max_turns: u32,
    pub agent_command: String,
    pub agent_flags: Vec<String>,
    pub env_vars: HashMap<String, String>,
    pub hooks: WorkAssignmentHooks,
}

/// Event sent from worker to orchestrator during execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerEvent {
    pub issue_id: String,
    pub event: AgentEvent,
    pub timestamp: DateTime<Utc>,
    pub worker_id: String,
}

/// Outcome of a completed work assignment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum WorkOutcome {
    Normal,
    Failed { error: String },
    TimedOut,
}

/// Result sent by worker on job completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkResult {
    pub issue_id: String,
    pub outcome: WorkOutcome,
    pub worker_id: String,
    pub duration_secs: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    // ── WorkAssignmentHooks ─────────────────────────────────────────

    #[test]
    fn hooks_default_has_none_fields() {
        let hooks = WorkAssignmentHooks::default();
        assert!(hooks.before_run.is_none());
        assert!(hooks.after_run.is_none());
    }

    #[test]
    fn hooks_serialization_roundtrip() {
        let hooks = WorkAssignmentHooks {
            before_run: Some("echo before".to_string()),
            after_run: Some("echo after".to_string()),
        };
        let json = serde_json::to_string(&hooks).unwrap();
        let deserialized: WorkAssignmentHooks = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.before_run, Some("echo before".to_string()));
        assert_eq!(deserialized.after_run, Some("echo after".to_string()));
    }

    #[test]
    fn hooks_default_serialization_roundtrip() {
        let hooks = WorkAssignmentHooks::default();
        let json = serde_json::to_string(&hooks).unwrap();
        let deserialized: WorkAssignmentHooks = serde_json::from_str(&json).unwrap();
        assert!(deserialized.before_run.is_none());
        assert!(deserialized.after_run.is_none());
    }

    #[test]
    fn hooks_debug_and_clone() {
        let hooks = WorkAssignmentHooks {
            before_run: Some("setup".to_string()),
            after_run: None,
        };
        let debug = format!("{:?}", hooks);
        assert!(debug.contains("WorkAssignmentHooks"));
        let cloned = hooks.clone();
        assert_eq!(cloned.before_run, hooks.before_run);
        assert_eq!(cloned.after_run, hooks.after_run);
    }

    // ── WorkAssignment ──────────────────────────────────────────────

    fn make_full_assignment() -> WorkAssignment {
        let mut env_vars = HashMap::new();
        env_vars.insert("KEY".to_string(), "value".to_string());
        env_vars.insert("OTHER".to_string(), "val2".to_string());

        WorkAssignment {
            issue_id: "issue-1".to_string(),
            issue_identifier: "ACTCLI-42".to_string(),
            issue_title: "Fix the bug".to_string(),
            issue_description: Some("A detailed description".to_string()),
            issue_state: "Todo".to_string(),
            prompt: "Please fix this issue".to_string(),
            repo_url: "https://github.com/org/repo.git".to_string(),
            branch_name: "fix/the-bug".to_string(),
            attempt: 1,
            max_turns: 50,
            agent_command: "claude".to_string(),
            agent_flags: vec!["--flag1".to_string(), "--flag2".to_string()],
            env_vars,
            hooks: WorkAssignmentHooks {
                before_run: Some("echo setup".to_string()),
                after_run: Some("echo teardown".to_string()),
            },
        }
    }

    #[test]
    fn assignment_serialization_roundtrip_all_fields() {
        let assignment = make_full_assignment();
        let json = serde_json::to_string(&assignment).unwrap();
        let deserialized: WorkAssignment = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.issue_id, "issue-1");
        assert_eq!(deserialized.issue_identifier, "ACTCLI-42");
        assert_eq!(deserialized.issue_title, "Fix the bug");
        assert_eq!(
            deserialized.issue_description,
            Some("A detailed description".to_string())
        );
        assert_eq!(deserialized.issue_state, "Todo");
        assert_eq!(deserialized.prompt, "Please fix this issue");
        assert_eq!(deserialized.repo_url, "https://github.com/org/repo.git");
        assert_eq!(deserialized.branch_name, "fix/the-bug");
        assert_eq!(deserialized.attempt, 1);
        assert_eq!(deserialized.max_turns, 50);
        assert_eq!(deserialized.agent_command, "claude");
        assert_eq!(deserialized.agent_flags, vec!["--flag1", "--flag2"]);
        assert_eq!(deserialized.env_vars.get("KEY").unwrap(), "value");
        assert_eq!(deserialized.env_vars.get("OTHER").unwrap(), "val2");
        assert_eq!(
            deserialized.hooks.before_run,
            Some("echo setup".to_string())
        );
        assert_eq!(
            deserialized.hooks.after_run,
            Some("echo teardown".to_string())
        );
    }

    #[test]
    fn assignment_with_optional_fields_none() {
        let assignment = WorkAssignment {
            issue_id: "issue-2".to_string(),
            issue_identifier: "ACTCLI-99".to_string(),
            issue_title: "Another issue".to_string(),
            issue_description: None,
            issue_state: "In Progress".to_string(),
            prompt: "Do the thing".to_string(),
            repo_url: "https://github.com/org/repo.git".to_string(),
            branch_name: "feat/thing".to_string(),
            attempt: 0,
            max_turns: 10,
            agent_command: "claude".to_string(),
            agent_flags: vec![],
            env_vars: HashMap::new(),
            hooks: WorkAssignmentHooks::default(),
        };
        let json = serde_json::to_string(&assignment).unwrap();
        let deserialized: WorkAssignment = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.issue_id, "issue-2");
        assert!(deserialized.issue_description.is_none());
        assert!(deserialized.agent_flags.is_empty());
        assert!(deserialized.env_vars.is_empty());
        assert!(deserialized.hooks.before_run.is_none());
        assert!(deserialized.hooks.after_run.is_none());
    }

    #[test]
    fn assignment_debug_and_clone() {
        let assignment = make_full_assignment();
        let debug = format!("{:?}", assignment);
        assert!(debug.contains("WorkAssignment"));
        assert!(debug.contains("issue-1"));

        let cloned = assignment.clone();
        assert_eq!(cloned.issue_id, assignment.issue_id);
        assert_eq!(cloned.issue_identifier, assignment.issue_identifier);
        assert_eq!(cloned.issue_title, assignment.issue_title);
        assert_eq!(cloned.issue_description, assignment.issue_description);
        assert_eq!(cloned.attempt, assignment.attempt);
    }

    // ── WorkerEvent ─────────────────────────────────────────────────

    #[test]
    fn worker_event_with_session_started() {
        let event = WorkerEvent {
            issue_id: "issue-1".to_string(),
            event: AgentEvent::SessionStarted {
                session_id: "sess-abc".to_string(),
                pid: Some(1234),
            },
            timestamp: Utc::now(),
            worker_id: "worker-1".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: WorkerEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.issue_id, "issue-1");
        assert_eq!(deserialized.worker_id, "worker-1");
        match &deserialized.event {
            AgentEvent::SessionStarted { session_id, pid } => {
                assert_eq!(session_id, "sess-abc");
                assert_eq!(*pid, Some(1234));
            }
            _ => panic!("Expected SessionStarted"),
        }
    }

    #[test]
    fn worker_event_with_turn_completed() {
        let event = WorkerEvent {
            issue_id: "issue-2".to_string(),
            event: AgentEvent::TurnCompleted {
                message: Some("finished step".to_string()),
            },
            timestamp: Utc::now(),
            worker_id: "worker-2".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: WorkerEvent = serde_json::from_str(&json).unwrap();
        match &deserialized.event {
            AgentEvent::TurnCompleted { message } => {
                assert_eq!(message.as_deref(), Some("finished step"));
            }
            _ => panic!("Expected TurnCompleted"),
        }
    }

    #[test]
    fn worker_event_with_turn_failed() {
        let event = WorkerEvent {
            issue_id: "issue-3".to_string(),
            event: AgentEvent::TurnFailed {
                error: "something broke".to_string(),
            },
            timestamp: Utc::now(),
            worker_id: "worker-3".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: WorkerEvent = serde_json::from_str(&json).unwrap();
        match &deserialized.event {
            AgentEvent::TurnFailed { error } => {
                assert_eq!(error, "something broke");
            }
            _ => panic!("Expected TurnFailed"),
        }
    }

    #[test]
    fn worker_event_with_notification() {
        let event = WorkerEvent {
            issue_id: "issue-4".to_string(),
            event: AgentEvent::Notification {
                message: "heads up".to_string(),
            },
            timestamp: Utc::now(),
            worker_id: "worker-4".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: WorkerEvent = serde_json::from_str(&json).unwrap();
        match &deserialized.event {
            AgentEvent::Notification { message } => {
                assert_eq!(message, "heads up");
            }
            _ => panic!("Expected Notification"),
        }
    }

    #[test]
    fn worker_event_with_token_usage() {
        let event = WorkerEvent {
            issue_id: "issue-5".to_string(),
            event: AgentEvent::TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
            },
            timestamp: Utc::now(),
            worker_id: "worker-5".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: WorkerEvent = serde_json::from_str(&json).unwrap();
        match &deserialized.event {
            AgentEvent::TokenUsage {
                input_tokens,
                output_tokens,
                total_tokens,
            } => {
                assert_eq!(*input_tokens, 100);
                assert_eq!(*output_tokens, 50);
                assert_eq!(*total_tokens, 150);
            }
            _ => panic!("Expected TokenUsage"),
        }
    }

    #[test]
    fn worker_event_with_agent_message() {
        let event = WorkerEvent {
            issue_id: "issue-6".to_string(),
            event: AgentEvent::AgentMessage {
                event_type: "tool_use".to_string(),
                message: Some("using grep".to_string()),
            },
            timestamp: Utc::now(),
            worker_id: "worker-6".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: WorkerEvent = serde_json::from_str(&json).unwrap();
        match &deserialized.event {
            AgentEvent::AgentMessage {
                event_type,
                message,
            } => {
                assert_eq!(event_type, "tool_use");
                assert_eq!(message.as_deref(), Some("using grep"));
            }
            _ => panic!("Expected AgentMessage"),
        }
    }

    #[test]
    fn worker_event_with_rate_limit_update() {
        let data = serde_json::json!({"requests_remaining": 42});
        let event = WorkerEvent {
            issue_id: "issue-7".to_string(),
            event: AgentEvent::RateLimitUpdate { data },
            timestamp: Utc::now(),
            worker_id: "worker-7".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: WorkerEvent = serde_json::from_str(&json).unwrap();
        match &deserialized.event {
            AgentEvent::RateLimitUpdate { data } => {
                assert_eq!(data["requests_remaining"], 42);
            }
            _ => panic!("Expected RateLimitUpdate"),
        }
    }

    #[test]
    fn worker_event_debug_and_clone() {
        let event = WorkerEvent {
            issue_id: "issue-1".to_string(),
            event: AgentEvent::Notification {
                message: "test".to_string(),
            },
            timestamp: Utc::now(),
            worker_id: "worker-1".to_string(),
        };
        let debug = format!("{:?}", event);
        assert!(debug.contains("WorkerEvent"));
        assert!(debug.contains("issue-1"));

        let cloned = event.clone();
        assert_eq!(cloned.issue_id, event.issue_id);
        assert_eq!(cloned.worker_id, event.worker_id);
    }

    // ── WorkOutcome ─────────────────────────────────────────────────

    #[test]
    fn outcome_normal_roundtrip() {
        let outcome = WorkOutcome::Normal;
        let json = serde_json::to_string(&outcome).unwrap();
        let deserialized: WorkOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, WorkOutcome::Normal);
    }

    #[test]
    fn outcome_failed_roundtrip() {
        let outcome = WorkOutcome::Failed {
            error: "something went wrong".to_string(),
        };
        let json = serde_json::to_string(&outcome).unwrap();
        let deserialized: WorkOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized,
            WorkOutcome::Failed {
                error: "something went wrong".to_string()
            }
        );
    }

    #[test]
    fn outcome_timed_out_roundtrip() {
        let outcome = WorkOutcome::TimedOut;
        let json = serde_json::to_string(&outcome).unwrap();
        let deserialized: WorkOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, WorkOutcome::TimedOut);
    }

    #[test]
    fn outcome_debug_and_clone() {
        let outcome = WorkOutcome::Failed {
            error: "err".to_string(),
        };
        let debug = format!("{:?}", outcome);
        assert!(debug.contains("Failed"));
        assert!(debug.contains("err"));

        let cloned = outcome.clone();
        assert_eq!(cloned, outcome);
    }

    #[test]
    fn outcome_partial_eq() {
        assert_eq!(WorkOutcome::Normal, WorkOutcome::Normal);
        assert_eq!(WorkOutcome::TimedOut, WorkOutcome::TimedOut);
        assert_ne!(WorkOutcome::Normal, WorkOutcome::TimedOut);
        assert_ne!(
            WorkOutcome::Normal,
            WorkOutcome::Failed {
                error: "x".to_string()
            }
        );
        assert_ne!(
            WorkOutcome::Failed {
                error: "a".to_string()
            },
            WorkOutcome::Failed {
                error: "b".to_string()
            }
        );
    }

    // ── WorkResult ──────────────────────────────────────────────────

    #[test]
    fn result_normal_roundtrip() {
        let result = WorkResult {
            issue_id: "issue-1".to_string(),
            outcome: WorkOutcome::Normal,
            worker_id: "worker-1".to_string(),
            duration_secs: 123.45,
        };
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: WorkResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.issue_id, "issue-1");
        assert_eq!(deserialized.outcome, WorkOutcome::Normal);
        assert_eq!(deserialized.worker_id, "worker-1");
        assert!((deserialized.duration_secs - 123.45).abs() < f64::EPSILON);
    }

    #[test]
    fn result_failed_roundtrip() {
        let result = WorkResult {
            issue_id: "issue-2".to_string(),
            outcome: WorkOutcome::Failed {
                error: "build failed".to_string(),
            },
            worker_id: "worker-2".to_string(),
            duration_secs: 60.0,
        };
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: WorkResult = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.outcome,
            WorkOutcome::Failed {
                error: "build failed".to_string()
            }
        );
    }

    #[test]
    fn result_timed_out_roundtrip() {
        let result = WorkResult {
            issue_id: "issue-3".to_string(),
            outcome: WorkOutcome::TimedOut,
            worker_id: "worker-3".to_string(),
            duration_secs: 300.0,
        };
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: WorkResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.outcome, WorkOutcome::TimedOut);
        assert!((deserialized.duration_secs - 300.0).abs() < f64::EPSILON);
    }

    #[test]
    fn result_debug_and_clone() {
        let result = WorkResult {
            issue_id: "issue-1".to_string(),
            outcome: WorkOutcome::Normal,
            worker_id: "worker-1".to_string(),
            duration_secs: 10.0,
        };
        let debug = format!("{:?}", result);
        assert!(debug.contains("WorkResult"));
        assert!(debug.contains("issue-1"));

        let cloned = result.clone();
        assert_eq!(cloned.issue_id, result.issue_id);
        assert_eq!(cloned.outcome, result.outcome);
        assert_eq!(cloned.worker_id, result.worker_id);
        assert!((cloned.duration_secs - result.duration_secs).abs() < f64::EPSILON);
    }

    // ── WorkerEvent timestamp preservation ──────────────────────────

    #[test]
    fn worker_event_preserves_timestamp() {
        let now = Utc::now();
        let event = WorkerEvent {
            issue_id: "issue-ts".to_string(),
            event: AgentEvent::Notification {
                message: "ts-test".to_string(),
            },
            timestamp: now,
            worker_id: "worker-ts".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: WorkerEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.timestamp, now);
    }
}
