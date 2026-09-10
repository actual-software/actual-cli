//! Plan-governance telemetry (AK-678): emits anonymous events describing
//! `plan-check` outcomes to the `/plan-governance/record` proxy route
//! (AK-679), which forwards them to PostHog. Every property sent here stays
//! inside the shape sprintreview's `PlanGovernanceEventProperties` schema
//! already enforces — rule ids/slugs, enums, counts, hashed identifiers.
//! Plan text, matched rule file paths, and conflicting plan spans are never
//! sent; see `PRIVACY.md`'s "Plan-governance events" section.

use std::time::Duration;

use crate::api::client::ActualApiClient;
use crate::api::types::{
    PlanGovernanceDecision, PlanGovernanceEvent, PlanGovernanceEventName,
    PlanGovernanceEventProperties, PlanGovernanceEventRequest,
};
use crate::config::types::Config;
use crate::rules::check::Verdict;
use crate::telemetry::opt_out;

/// Public write-only telemetry key — see `reporter::SERVICE_KEY`'s doc
/// comment for the security rationale (safe to embed; scoped only to
/// telemetry ingest routes, no other access).
const SERVICE_KEY: &str = "ak_telemetry_prod_actual_cli";

/// Filename under the config dir (sibling to `plan-check-overrides.log`)
/// holding this installation's persisted anonymous telemetry id.
const DISTINCT_ID_FILE: &str = "telemetry-id";

/// A short, dedicated send timeout — deliberately much shorter than
/// `ActualApiClient::new`'s default 30s. `plan-check --claude-hook` shares a
/// hard 120s `PreToolUse` budget with the judge call, already measured at
/// 58-82s on its own; a 30s worst-case telemetry send on top of that risks
/// blowing the hook's budget. This bounds plan-governance telemetry's own
/// worst case to a few seconds instead, in service of AK-678's "telemetry
/// failure never fails or slows a plan check" acceptance criterion.
const SEND_TIMEOUT: Duration = Duration::from_secs(2);
const SEND_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// The proxy's `PlanGovernanceEventRequest` schema caps a single batch at
/// `z.array(PlanGovernanceEvent).min(1).max(100)` — see
/// `ActualApiClient::post_plan_governance_events`'s doc comment. A normal
/// `plan-check`/`--claude-hook` run never gets close (started + completed +
/// at most `MAX_RULES_JUDGED` violations, well under 100), but
/// `plan-check-override` emits one event per `--rule` flag with no cap of
/// its own, so its caller must chunk into batches of at most this many
/// events rather than sending one oversized batch the proxy would reject
/// whole.
pub const MAX_EVENTS_PER_BATCH: usize = 100;

/// Get or create this installation's opaque, per-install telemetry
/// identifier: a random UUIDv4, generated once, persisted under the config
/// dir, and reused forever after.
///
/// Never derived from a username, email, hostname, MAC address, or any
/// other identifying material — purely a random token, the same category as
/// a PostHog/Sentry anonymous device id. Best-effort like every other file
/// under the config dir: an unreadable or unwritable config dir degrades to
/// a fresh random id for this process only (never blocks, never errors)
/// rather than disabling telemetry entirely.
///
/// Validates that the file's contents parse as a UUID before trusting them
/// — a truncated write, manual edit, or filesystem corruption could leave
/// behind non-UUID bytes that would otherwise be sent as `distinct_id`
/// forever after and might fail the proxy's schema. An invalid file is
/// treated the same as a missing one: a fresh id is generated and written
/// over it.
pub fn distinct_id() -> String {
    let Ok(dir) = crate::config::paths::config_dir() else {
        return uuid::Uuid::new_v4().to_string();
    };
    let path = dir.join(DISTINCT_ID_FILE);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if uuid::Uuid::parse_str(trimmed).is_ok() {
            return trimmed.to_string();
        }
    }
    let fresh = uuid::Uuid::new_v4().to_string();
    let _ = std::fs::create_dir_all(&dir);
    let _ = crate::config::paths::write_secure(&path, fresh.as_bytes());
    fresh
}

/// The `decision` reported for one rule's outcome this round.
///
/// Takes `blocked` — whether the tool call was actually denied over this
/// rule after override/round-limit accounting — as a second input rather
/// than mapping `Verdict` 1:1, because [`Verdict::RequiresDecision`] blocks
/// exactly like [`Verdict::Conflicting`] in the hook's revision loop until
/// an override or round-limit pass says otherwise, but is never blocking on
/// its own in direct/CLI mode. A verdict that found a real problem but did
/// not end up blocking this round (override applied, round-limit exhausted,
/// or direct-mode's `RequiresDecision` surfacing) is reported as `Warn`,
/// distinct from a clean `Allow` — since this stream is, per AK-662, the
/// only record governance activity leaves behind.
pub fn rule_decision(verdict: Verdict, blocked: bool) -> PlanGovernanceDecision {
    match verdict {
        Verdict::Conforming => PlanGovernanceDecision::Allow,
        Verdict::Conflicting | Verdict::RequiresDecision => {
            if blocked {
                PlanGovernanceDecision::Block
            } else {
                PlanGovernanceDecision::Warn
            }
        }
    }
}

/// Shared, non-identifying context stamped onto every event in a batch.
pub struct EventContext {
    pub distinct_id: String,
    pub cli_version: String,
    pub command: String,
    pub repo_hash: Option<String>,
    pub repo_url_hash: Option<String>,
}

impl EventContext {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            distinct_id: distinct_id(),
            cli_version: env!("CARGO_PKG_VERSION").to_string(),
            command: command.into(),
            repo_hash: None,
            repo_url_hash: None,
        }
    }

    pub fn with_repo_hashes(mut self, repo_hash: String, repo_url_hash: String) -> Self {
        self.repo_hash = Some(repo_hash);
        self.repo_url_hash = Some(repo_url_hash);
        self
    }

    fn base_properties(&self) -> PlanGovernanceEventProperties {
        PlanGovernanceEventProperties {
            cli_version: Some(self.cli_version.clone()),
            command: Some(self.command.clone()),
            repo_hash: self.repo_hash.clone(),
            repo_url_hash: self.repo_url_hash.clone(),
            ..Default::default()
        }
    }

    fn timestamp() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    pub fn started_event(&self) -> PlanGovernanceEvent {
        PlanGovernanceEvent {
            event: PlanGovernanceEventName::PlanGovernanceCheckStarted,
            distinct_id: self.distinct_id.clone(),
            properties: Some(self.base_properties()),
            timestamp: Some(Self::timestamp()),
            insert_id: None,
        }
    }

    pub fn completed_event(
        &self,
        decision: PlanGovernanceDecision,
        duration_ms: f64,
        exit_code: i32,
    ) -> PlanGovernanceEvent {
        let mut properties = self.base_properties();
        properties.decision = Some(decision);
        properties.duration_ms = Some(duration_ms);
        properties.exit_code = Some(exit_code);
        PlanGovernanceEvent {
            event: PlanGovernanceEventName::PlanGovernanceCheckCompleted,
            distinct_id: self.distinct_id.clone(),
            properties: Some(properties),
            timestamp: Some(Self::timestamp()),
            insert_id: None,
        }
    }

    /// One event for a single rule that did not conform. `rule_id` and
    /// `rule_source` are the rule's internal id and document slug — an
    /// identifier, never a filesystem path or rule text.
    pub fn violation_event(
        &self,
        rule_id: &str,
        rule_source: &str,
        decision: PlanGovernanceDecision,
    ) -> PlanGovernanceEvent {
        let mut properties = self.base_properties();
        properties.rule_id = Some(rule_id.to_string());
        properties.rule_source = Some(rule_source.to_string());
        properties.decision = Some(decision);
        PlanGovernanceEvent {
            event: PlanGovernanceEventName::PlanGovernanceRuleViolation,
            distinct_id: self.distinct_id.clone(),
            properties: Some(properties),
            timestamp: Some(Self::timestamp()),
            insert_id: None,
        }
    }
}

/// Send a batch of plan-governance events, fire-and-forget.
///
/// Honors both runtime opt-outs (env var, config) identically to the sync
/// counters. Never returns an error to the caller — a slow or failing send
/// is logged at debug level and discarded, so it can never fail or slow a
/// plan-check run. No-op on an empty batch.
pub async fn send_events(events: Vec<PlanGovernanceEvent>, config: &Config, api_url: &str) {
    if events.is_empty() || opt_out::is_disabled(config) {
        return;
    }

    let Ok(client) = ActualApiClient::new_with_timeout(api_url, SEND_TIMEOUT, SEND_CONNECT_TIMEOUT)
    else {
        return;
    };
    let request = PlanGovernanceEventRequest { events };
    match client
        .post_plan_governance_events(&request, SERVICE_KEY)
        .await
    {
        Ok(resp) if resp.failed > 0 => {
            tracing::debug!(
                "plan-governance telemetry: {} of {} events failed server-side",
                resp.failed,
                resp.recorded + resp.failed
            );
        }
        Err(e) => tracing::debug!("plan-governance telemetry: {e}"),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::TelemetryConfig;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

    // --- rule_decision ---

    #[test]
    fn test_rule_decision_conforming_is_always_allow() {
        assert_eq!(
            rule_decision(Verdict::Conforming, false),
            PlanGovernanceDecision::Allow
        );
        assert_eq!(
            rule_decision(Verdict::Conforming, true),
            PlanGovernanceDecision::Allow
        );
    }

    #[test]
    fn test_rule_decision_conflicting_blocked_is_block() {
        assert_eq!(
            rule_decision(Verdict::Conflicting, true),
            PlanGovernanceDecision::Block
        );
    }

    #[test]
    fn test_rule_decision_conflicting_not_blocked_is_warn() {
        assert_eq!(
            rule_decision(Verdict::Conflicting, false),
            PlanGovernanceDecision::Warn
        );
    }

    #[test]
    fn test_rule_decision_requires_decision_blocked_is_block() {
        assert_eq!(
            rule_decision(Verdict::RequiresDecision, true),
            PlanGovernanceDecision::Block
        );
    }

    #[test]
    fn test_rule_decision_requires_decision_not_blocked_is_warn() {
        assert_eq!(
            rule_decision(Verdict::RequiresDecision, false),
            PlanGovernanceDecision::Warn
        );
    }

    // --- distinct_id ---

    #[test]
    fn test_distinct_id_is_stable_across_calls() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        let first = distinct_id();
        let second = distinct_id();
        assert_eq!(first, second, "distinct_id must be stable across calls");
        assert!(
            uuid::Uuid::parse_str(&first).is_ok(),
            "distinct_id must be a well-formed UUID, got: {first}"
        );
    }

    #[test]
    fn test_distinct_id_recovers_from_empty_file() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        std::fs::create_dir_all(tmp.path()).unwrap();
        std::fs::write(tmp.path().join(DISTINCT_ID_FILE), "").unwrap();

        let id = distinct_id();
        assert!(uuid::Uuid::parse_str(&id).is_ok());
    }

    #[test]
    fn test_distinct_id_recovers_from_corrupt_file() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", tmp.path().to_str().unwrap());

        std::fs::create_dir_all(tmp.path()).unwrap();
        std::fs::write(tmp.path().join(DISTINCT_ID_FILE), "not-a-uuid\0garbage").unwrap();

        let id = distinct_id();
        assert!(
            uuid::Uuid::parse_str(&id).is_ok(),
            "a non-UUID file must not be trusted as-is"
        );

        // The regenerated id must also have been persisted, not just
        // returned for this one call.
        let on_disk = std::fs::read_to_string(tmp.path().join(DISTINCT_ID_FILE)).unwrap();
        assert_eq!(on_disk.trim(), id);
    }

    #[test]
    fn test_distinct_id_falls_back_without_config_dir() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _config_guard = EnvGuard::remove("ACTUAL_CONFIG");
        // An empty (non-absolute) ACTUAL_CONFIG_DIR makes `config_dir()`
        // fail, so `distinct_id` must still return a well-formed id rather
        // than panicking or blocking.
        let _dir_guard = EnvGuard::set("ACTUAL_CONFIG_DIR", "");
        let id = distinct_id();
        assert!(uuid::Uuid::parse_str(&id).is_ok());
    }

    // --- EventContext builders ---

    fn context() -> EventContext {
        EventContext {
            distinct_id: "install-abc123".to_string(),
            cli_version: "1.2.3".to_string(),
            command: "plan-check".to_string(),
            repo_hash: Some("a".repeat(64)),
            repo_url_hash: Some("b".repeat(64)),
        }
    }

    #[test]
    fn test_started_event_carries_context_no_decision() {
        let ctx = context();
        let event = ctx.started_event();
        assert_eq!(
            event.event,
            PlanGovernanceEventName::PlanGovernanceCheckStarted
        );
        assert_eq!(event.distinct_id, "install-abc123");
        let props = event.properties.unwrap();
        assert_eq!(props.cli_version.as_deref(), Some("1.2.3"));
        assert_eq!(props.command.as_deref(), Some("plan-check"));
        assert_eq!(props.repo_hash.as_deref(), Some("a".repeat(64).as_str()));
        assert!(props.decision.is_none());
    }

    #[test]
    fn test_completed_event_carries_decision_duration_exit_code() {
        let ctx = context();
        let event = ctx.completed_event(PlanGovernanceDecision::Block, 42.5, 1);
        assert_eq!(
            event.event,
            PlanGovernanceEventName::PlanGovernanceCheckCompleted
        );
        let props = event.properties.unwrap();
        assert_eq!(props.decision, Some(PlanGovernanceDecision::Block));
        assert_eq!(props.duration_ms, Some(42.5));
        assert_eq!(props.exit_code, Some(1));
    }

    #[test]
    fn test_violation_event_carries_rule_id_and_source_never_span() {
        let ctx = context();
        let event = ctx.violation_event(
            "R-A-002",
            "cross-cutting-tokens",
            PlanGovernanceDecision::Warn,
        );
        assert_eq!(
            event.event,
            PlanGovernanceEventName::PlanGovernanceRuleViolation
        );
        let props = event.properties.unwrap();
        assert_eq!(props.rule_id.as_deref(), Some("R-A-002"));
        assert_eq!(props.rule_source.as_deref(), Some("cross-cutting-tokens"));
        assert_eq!(props.decision, Some(PlanGovernanceDecision::Warn));
    }

    // --- send_events ---

    #[tokio::test]
    async fn test_send_events_noop_on_empty_batch() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("ACTUAL_NO_TELEMETRY");
        // No mock server set up; an empty batch must never attempt a send.
        send_events(vec![], &Config::default(), "http://127.0.0.1:1").await;
    }

    #[tokio::test]
    async fn test_send_events_opt_out_via_env_var_skips_network() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
        let ctx = context();
        // Unreachable address: if this were attempted, the call would hang
        // or error out slowly rather than returning immediately.
        send_events(
            vec![ctx.started_event()],
            &Config::default(),
            "http://127.0.0.1:1",
        )
        .await;
    }

    #[tokio::test]
    async fn test_send_events_opt_out_via_config_skips_network() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("ACTUAL_NO_TELEMETRY");
        let ctx = context();
        let config = Config {
            telemetry: Some(TelemetryConfig {
                enabled: Some(false),
            }),
            ..Default::default()
        };
        send_events(vec![ctx.started_event()], &config, "http://127.0.0.1:1").await;
    }

    #[tokio::test]
    async fn test_send_events_posts_batch_on_success() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("ACTUAL_NO_TELEMETRY");

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/plan-governance/record")
            .match_header("authorization", mockito::Matcher::Regex("Bearer .+".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"recorded": 1, "failed": 0}"#)
            .create_async()
            .await;

        let ctx = context();
        send_events(vec![ctx.started_event()], &Config::default(), &server.url()).await;
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_send_events_network_failure_does_not_panic() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::remove("ACTUAL_NO_TELEMETRY");
        let ctx = context();
        // Unreachable address — must swallow the error, never panic or
        // propagate it to the caller.
        send_events(
            vec![ctx.started_event()],
            &Config::default(),
            "http://127.0.0.1:1",
        )
        .await;
    }
}
