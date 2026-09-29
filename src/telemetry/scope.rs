//! Scope-selection telemetry (narrative 6): one `plan_governance_scope_select`
//! event per `rules select` / signals-workflow run, describing how the local-ADR
//! selection was reached — how many rules were scanned, the stage-1 candidate
//! count, whether stage 2 (the LLM rank) ran or degraded, how many rules were
//! selected, cache hit/miss, and the latency.
//!
//! It routes through the same `/plan-governance/record` PostHog proxy path as the
//! governance events (not the warehouse counters), and carries a random
//! `scope_run_id` so a selection can later be joined to the governance decision it
//! produced. Only counts, enums, ids and hashes leave the machine — never rule
//! text, plan text, or file paths (see `PRIVACY.md`).

use crate::api::types::{
    PlanGovernanceEvent, PlanGovernanceEventName, PlanGovernanceEventProperties,
};
use crate::telemetry::plan_governance::new_insert_id;

/// The metrics for one scope-selection run. Built by the caller from the
/// library `Selection`/`ResolvedIndex` so this stays free of scope-pipeline types.
#[derive(Debug, Clone, PartialEq)]
pub struct ScopeMetrics {
    /// Random per-run correlation id (shared with the governance decision).
    pub scope_run_id: String,
    /// Local rule documents scanned (index size).
    pub rules_scanned: u32,
    /// Stage-1 lexical prefilter candidate count.
    pub stage1_candidates: u32,
    /// Whether stage 2 (the LLM rank) actually shaped this selection.
    pub stage2_invoked: bool,
    /// Stage-2 status / degradation reason.
    pub stage2_status: String,
    /// How many rules were ultimately selected.
    pub selected: u32,
    /// Whether the scope index came from cache.
    pub cache_hit: bool,
    /// Selection latency in milliseconds.
    pub duration_ms: f64,
    /// Rank runner label, when stage 2 ran.
    pub runner: Option<String>,
    /// SHA-256 repo hashes, same shape as the governance events'.
    pub repo_hash: Option<String>,
    pub repo_url_hash: Option<String>,
}

impl ScopeMetrics {
    /// Build the `scope_select` event for this run.
    pub fn to_event(&self) -> PlanGovernanceEvent {
        let properties = PlanGovernanceEventProperties {
            cli_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            command: Some("rules select".to_string()),
            duration_ms: Some(self.duration_ms),
            repo_hash: self.repo_hash.clone(),
            repo_url_hash: self.repo_url_hash.clone(),
            scope_run_id: Some(self.scope_run_id.clone()),
            rules_scanned: Some(self.rules_scanned),
            stage1_candidates: Some(self.stage1_candidates),
            stage2_invoked: Some(self.stage2_invoked),
            stage2_status: Some(self.stage2_status.clone()),
            selected: Some(self.selected),
            cache_hit: Some(self.cache_hit),
            runner: self.runner.clone(),
            ..Default::default()
        };
        PlanGovernanceEvent {
            event: PlanGovernanceEventName::PlanGovernanceScopeSelect,
            distinct_id: crate::telemetry::plan_governance::distinct_id(),
            properties: Some(properties),
            timestamp: Some(chrono::Utc::now().to_rfc3339()),
            insert_id: new_insert_id(),
        }
    }
}

/// A fresh random correlation id for one scope run. Not derived from any
/// customer data, so it is safe to send raw and to share with the governance
/// decision the selection feeds.
pub fn new_scope_run_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> ScopeMetrics {
        ScopeMetrics {
            scope_run_id: "11111111-1111-4111-8111-111111111111".to_string(),
            rules_scanned: 425,
            stage1_candidates: 30,
            stage2_invoked: true,
            stage2_status: "applied".to_string(),
            selected: 8,
            cache_hit: true,
            duration_ms: 1234.5,
            runner: Some("claude-cli (sonnet)".to_string()),
            repo_hash: Some("a".repeat(64)),
            repo_url_hash: Some("b".repeat(64)),
        }
    }

    #[test]
    fn test_to_event_is_a_scope_select_with_metrics() {
        let event = metrics().to_event();
        assert_eq!(
            event.event,
            PlanGovernanceEventName::PlanGovernanceScopeSelect
        );
        let props = event.properties.unwrap();
        assert_eq!(props.command.as_deref(), Some("rules select"));
        assert_eq!(
            props.scope_run_id.as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );
        assert_eq!(props.rules_scanned, Some(425));
        assert_eq!(props.stage1_candidates, Some(30));
        assert_eq!(props.stage2_invoked, Some(true));
        assert_eq!(props.stage2_status.as_deref(), Some("applied"));
        assert_eq!(props.selected, Some(8));
        assert_eq!(props.cache_hit, Some(true));
        assert_eq!(props.duration_ms, Some(1234.5));
        assert_eq!(props.runner.as_deref(), Some("claude-cli (sonnet)"));
    }

    #[test]
    fn test_new_scope_run_id_is_a_uuid_and_unique() {
        let a = new_scope_run_id();
        let b = new_scope_run_id();
        assert!(uuid::Uuid::parse_str(&a).is_ok());
        assert_ne!(a, b);
    }

    #[test]
    fn test_event_carries_no_plan_or_rule_text() {
        // The payload is counts/enums/ids only -- a selection over a plan that
        // mentioned "secret-plan-text" never carries that text.
        let event = metrics().to_event();
        let serialized = serde_json::to_string(&event).unwrap();
        assert!(!serialized.contains("secret-plan-text"));
        // Sanity: it does carry the numeric signals.
        assert!(serialized.contains("rules_scanned"));
    }
}
