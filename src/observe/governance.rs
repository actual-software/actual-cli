use serde::{Deserialize, Serialize};

// ── Enums ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GovernanceDecision {
    Approve,
    ApproveWithConstraints,
    Rework,
    Decompose,
    Escalate,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PolicyStrength {
    Must,
    MustNot,
    Should,
    ShouldNot,
    May,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingSeverity {
    Violation,
    Concern,
    Observation,
}

// ── Structs ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormativePolicyStatement {
    pub strength: PolicyStrength,
    pub statement: String,
    pub policy_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_adr_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConformanceFinding {
    pub policy_id: String,
    pub strength: PolicyStrength,
    pub statement: String,
    pub finding_text: String,
    pub severity: FindingSeverity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoState {
    pub sha: String,
    pub branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub proposal_id: String,
    pub task_id: String,
    pub session_id: String,
    pub plan_text: String,
    pub affected_paths: Vec<String>,
    pub architecture_surfaces: Vec<String>,
    pub repo_state: RepoState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoundedAuthorization {
    pub authorization_id: String,
    pub task_id: String,
    pub repo_state: String,
    pub allowed_scope: Vec<String>,
    pub protected_boundaries: Vec<String>,
    pub policies: Vec<String>,
    pub conditions: Vec<String>,
    pub issued_at: String,
    pub ttl_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdditionalContext {
    #[serde(rename = "type")]
    pub r#type: String,
    #[serde(rename = "ref")]
    pub ref_: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestedTask {
    pub title: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecompositionGuidance {
    pub reasons: Vec<String>,
    pub suggested_boundaries: Vec<String>,
    pub suggested_tasks: Vec<SuggestedTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernanceDecisionResponse {
    pub decision: GovernanceDecision,
    pub proposal_id: String,
    pub findings: Vec<ConformanceFinding>,
    pub constraints: Vec<String>,
    pub additional_context: Vec<AdditionalContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decomposition_guidance: Option<DecompositionGuidance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization: Option<BoundedAuthorization>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernanceState {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<GovernanceDecision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization: Option<BoundedAuthorization>,
    pub rework_iteration: u32,
}

impl GovernanceState {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            proposal_id: None,
            decision: None,
            authorization: None,
            rework_iteration: 0,
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // 1. GovernanceDecision — all 6 variants
    #[test]
    fn governance_decision_round_trip() {
        let variants = vec![
            (GovernanceDecision::Approve, "\"APPROVE\""),
            (GovernanceDecision::ApproveWithConstraints, "\"APPROVE_WITH_CONSTRAINTS\""),
            (GovernanceDecision::Rework, "\"REWORK\""),
            (GovernanceDecision::Decompose, "\"DECOMPOSE\""),
            (GovernanceDecision::Escalate, "\"ESCALATE\""),
            (GovernanceDecision::Deny, "\"DENY\""),
        ];
        for (variant, expected_json) in variants {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected_json, "serialize {:?}", variant);
            let back: GovernanceDecision = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    // 2. PolicyStrength — all 5 values
    #[test]
    fn policy_strength_round_trip() {
        let variants = vec![
            (PolicyStrength::Must, "\"MUST\""),
            (PolicyStrength::MustNot, "\"MUST_NOT\""),
            (PolicyStrength::Should, "\"SHOULD\""),
            (PolicyStrength::ShouldNot, "\"SHOULD_NOT\""),
            (PolicyStrength::May, "\"MAY\""),
        ];
        for (variant, expected_json) in variants {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected_json, "serialize {:?}", variant);
            let back: PolicyStrength = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    // 3. NormativePolicyStatement — with and without source_adr_id
    #[test]
    fn normative_policy_statement_with_source() {
        let stmt = NormativePolicyStatement {
            strength: PolicyStrength::Must,
            statement: "All tables must have RLS enabled".into(),
            policy_id: "pol-001".into(),
            source_adr_id: Some("adr-042".into()),
        };
        let json = serde_json::to_string(&stmt).unwrap();
        assert!(json.contains("\"source_adr_id\":\"adr-042\""));
        let back: NormativePolicyStatement = serde_json::from_str(&json).unwrap();
        assert_eq!(back.source_adr_id, Some("adr-042".into()));
    }

    #[test]
    fn normative_policy_statement_without_source() {
        let stmt = NormativePolicyStatement {
            strength: PolicyStrength::Should,
            statement: "Use CTEs for multi-stage queries".into(),
            policy_id: "pol-002".into(),
            source_adr_id: None,
        };
        let json = serde_json::to_string(&stmt).unwrap();
        assert!(!json.contains("source_adr_id"), "None field should be skipped");
        let back: NormativePolicyStatement = serde_json::from_str(&json).unwrap();
        assert_eq!(back.source_adr_id, None);
    }

    // 4. ConformanceFinding — with and without evidence_ref
    #[test]
    fn conformance_finding_with_evidence() {
        let finding = ConformanceFinding {
            policy_id: "pol-001".into(),
            strength: PolicyStrength::Must,
            statement: "RLS must be enabled".into(),
            finding_text: "Table users missing RLS".into(),
            severity: FindingSeverity::Violation,
            evidence_ref: Some("migration_042.sql:15".into()),
        };
        let json = serde_json::to_string(&finding).unwrap();
        assert!(json.contains("\"severity\":\"violation\""));
        assert!(json.contains("\"evidence_ref\""));
        let back: ConformanceFinding = serde_json::from_str(&json).unwrap();
        assert_eq!(back.severity, FindingSeverity::Violation);
        assert_eq!(back.evidence_ref, Some("migration_042.sql:15".into()));
    }

    #[test]
    fn conformance_finding_without_evidence() {
        let finding = ConformanceFinding {
            policy_id: "pol-003".into(),
            strength: PolicyStrength::Should,
            statement: "Use descriptive CTE names".into(),
            finding_text: "CTE named 'q1' is not descriptive".into(),
            severity: FindingSeverity::Observation,
            evidence_ref: None,
        };
        let json = serde_json::to_string(&finding).unwrap();
        assert!(!json.contains("evidence_ref"), "None field should be skipped");
        let back: ConformanceFinding = serde_json::from_str(&json).unwrap();
        assert_eq!(back.evidence_ref, None);
        assert_eq!(back.severity, FindingSeverity::Observation);
    }

    // 5. FindingSeverity lowercase serialization
    #[test]
    fn finding_severity_lowercase() {
        let cases = vec![
            (FindingSeverity::Violation, "\"violation\""),
            (FindingSeverity::Concern, "\"concern\""),
            (FindingSeverity::Observation, "\"observation\""),
        ];
        for (variant, expected) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected);
            let back: FindingSeverity = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    // 6. Proposal round-trip
    #[test]
    fn proposal_round_trip() {
        let proposal = Proposal {
            proposal_id: "prop-001".into(),
            task_id: "task-abc".into(),
            session_id: "sess-123".into(),
            plan_text: "Add user profiles table".into(),
            affected_paths: vec!["supabase/migrations/".into(), "src/models/".into()],
            architecture_surfaces: vec!["database".into(), "api".into()],
            repo_state: RepoState {
                sha: "abc1234".into(),
                branch: "feat/user-profiles".into(),
            },
        };
        let json = serde_json::to_string(&proposal).unwrap();
        let back: Proposal = serde_json::from_str(&json).unwrap();
        assert_eq!(back.proposal_id, "prop-001");
        assert_eq!(back.repo_state.sha, "abc1234");
        assert_eq!(back.affected_paths.len(), 2);
    }

    // 7. BoundedAuthorization round-trip
    #[test]
    fn bounded_authorization_round_trip() {
        let auth = BoundedAuthorization {
            authorization_id: "auth-001".into(),
            task_id: "task-xyz".into(),
            repo_state: "abc1234".into(),
            allowed_scope: vec!["src/models/**".into()],
            protected_boundaries: vec!["supabase/migrations/**".into()],
            policies: vec!["pol-001".into()],
            conditions: vec!["no schema changes without migration".into()],
            issued_at: "2026-08-11T00:00:00Z".into(),
            ttl_seconds: 3600,
        };
        let json = serde_json::to_string(&auth).unwrap();
        let back: BoundedAuthorization = serde_json::from_str(&json).unwrap();
        assert_eq!(back.authorization_id, "auth-001");
        assert_eq!(back.ttl_seconds, 3600);
        assert_eq!(back.allowed_scope.len(), 1);
    }

    // 8. AdditionalContext — "type" and "ref" rename
    #[test]
    fn additional_context_field_renames() {
        let ctx = AdditionalContext {
            r#type: "adr".into(),
            ref_: "adr-042".into(),
            content: Some("Full ADR text here".into()),
        };
        let json = serde_json::to_string(&ctx).unwrap();
        // Verify the JSON keys are "type" and "ref", not "r#type" or "ref_"
        assert!(json.contains("\"type\":\"adr\""), "field should serialize as 'type'");
        assert!(json.contains("\"ref\":\"adr-042\""), "field should serialize as 'ref'");
        let back: AdditionalContext = serde_json::from_str(&json).unwrap();
        assert_eq!(back.r#type, "adr");
        assert_eq!(back.ref_, "adr-042");
    }

    // 9. GovernanceDecisionResponse — APPROVE with authorization
    #[test]
    fn governance_response_approve_with_auth() {
        let resp = GovernanceDecisionResponse {
            decision: GovernanceDecision::Approve,
            proposal_id: "prop-001".into(),
            findings: vec![],
            constraints: vec![],
            additional_context: vec![],
            decomposition_guidance: None,
            authorization: Some(BoundedAuthorization {
                authorization_id: "auth-001".into(),
                task_id: "task-abc".into(),
                repo_state: "abc1234".into(),
                allowed_scope: vec!["src/**".into()],
                protected_boundaries: vec![],
                policies: vec![],
                conditions: vec![],
                issued_at: "2026-08-11T00:00:00Z".into(),
                ttl_seconds: 7200,
            }),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"APPROVE\""));
        assert!(!json.contains("decomposition_guidance"));
        let back: GovernanceDecisionResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.decision, GovernanceDecision::Approve);
        assert!(back.authorization.is_some());
        assert!(back.decomposition_guidance.is_none());
    }

    // 10. GovernanceDecisionResponse — REWORK with findings
    #[test]
    fn governance_response_rework_with_findings() {
        let resp = GovernanceDecisionResponse {
            decision: GovernanceDecision::Rework,
            proposal_id: "prop-002".into(),
            findings: vec![ConformanceFinding {
                policy_id: "pol-001".into(),
                strength: PolicyStrength::Must,
                statement: "RLS required".into(),
                finding_text: "Missing RLS on users table".into(),
                severity: FindingSeverity::Violation,
                evidence_ref: Some("migration.sql:10".into()),
            }],
            constraints: vec!["Add RLS before resubmitting".into()],
            additional_context: vec![AdditionalContext {
                r#type: "adr".into(),
                ref_: "adr-001".into(),
                content: None,
            }],
            decomposition_guidance: None,
            authorization: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"REWORK\""));
        assert!(json.contains("\"violation\""));
        let back: GovernanceDecisionResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.findings.len(), 1);
        assert!(back.authorization.is_none());
    }

    // 11. GovernanceDecisionResponse — DECOMPOSE with guidance
    #[test]
    fn governance_response_decompose_with_guidance() {
        let resp = GovernanceDecisionResponse {
            decision: GovernanceDecision::Decompose,
            proposal_id: "prop-003".into(),
            findings: vec![],
            constraints: vec![],
            additional_context: vec![],
            decomposition_guidance: Some(DecompositionGuidance {
                reasons: vec!["Scope too broad".into(), "Crosses service boundary".into()],
                suggested_boundaries: vec!["database layer".into(), "api layer".into()],
                suggested_tasks: vec![
                    SuggestedTask {
                        title: "Add migration".into(),
                        description: "Create schema migration for profiles".into(),
                    },
                    SuggestedTask {
                        title: "Add API endpoint".into(),
                        description: "Expose profile CRUD via REST".into(),
                    },
                ],
            }),
            authorization: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"DECOMPOSE\""));
        assert!(json.contains("suggested_tasks"));
        let back: GovernanceDecisionResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.decision, GovernanceDecision::Decompose);
        let guidance = back.decomposition_guidance.unwrap();
        assert_eq!(guidance.reasons.len(), 2);
        assert_eq!(guidance.suggested_tasks.len(), 2);
    }

    // 12. GovernanceState — round-trip and new() default
    #[test]
    fn governance_state_new_defaults() {
        let state = GovernanceState::new("sess-001".into());
        assert_eq!(state.session_id, "sess-001");
        assert_eq!(state.proposal_id, None);
        assert_eq!(state.decision, None);
        assert_eq!(state.authorization, None);
        assert_eq!(state.rework_iteration, 0);
    }

    #[test]
    fn governance_state_round_trip() {
        let state = GovernanceState {
            session_id: "sess-001".into(),
            proposal_id: Some("prop-001".into()),
            decision: Some(GovernanceDecision::ApproveWithConstraints),
            authorization: Some(BoundedAuthorization {
                authorization_id: "auth-002".into(),
                task_id: "task-xyz".into(),
                repo_state: "def5678".into(),
                allowed_scope: vec!["src/**".into()],
                protected_boundaries: vec!["migrations/**".into()],
                policies: vec!["pol-001".into(), "pol-002".into()],
                conditions: vec!["must pass CI".into()],
                issued_at: "2026-08-11T12:00:00Z".into(),
                ttl_seconds: 1800,
            }),
            rework_iteration: 2,
        };
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains("\"APPROVE_WITH_CONSTRAINTS\""));
        let back: GovernanceState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session_id, "sess-001");
        assert_eq!(back.rework_iteration, 2);
        assert!(back.decision.is_some());
    }

    // 13. GovernanceState — serialization skips None fields
    #[test]
    fn governance_state_skips_none_fields() {
        let state = GovernanceState::new("sess-002".into());
        let json = serde_json::to_string(&state).unwrap();
        assert!(!json.contains("proposal_id"), "None proposal_id should be skipped");
        assert!(!json.contains("decision"), "None decision should be skipped");
        assert!(!json.contains("authorization"), "None authorization should be skipped");
        // But session_id and rework_iteration should always be present
        assert!(json.contains("\"session_id\""));
        assert!(json.contains("\"rework_iteration\""));
    }
}
