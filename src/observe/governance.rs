use chrono::Utc;
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
pub struct GovernanceDecisionSummary {
    pub decision: GovernanceDecision,
    pub proposal_id: String,
    pub finding_count: usize,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernanceState {
    pub session_id: String,
    pub rework_iterations: u32,
    pub max_rework_iterations: u32,
    pub current_authorization: Option<BoundedAuthorization>,
    pub decision_history: Vec<GovernanceDecisionSummary>,
    pub touched_paths: Vec<String>,
    pub assurance_passed: bool,
}

impl GovernanceState {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            rework_iterations: 0,
            max_rework_iterations: 5,
            current_authorization: None,
            decision_history: Vec::new(),
            touched_paths: Vec::new(),
            assurance_passed: false,
        }
    }

    pub fn track_path(&mut self, path: &str) {
        if !self.touched_paths.contains(&path.to_string()) {
            self.touched_paths.push(path.to_string());
        }
    }

    pub fn needs_scope_review(&self) -> bool {
        self.current_authorization.is_some() && !self.touched_paths.is_empty()
    }

    pub fn needs_assurance(&self) -> bool {
        self.current_authorization.is_some() && !self.assurance_passed
    }

    pub fn record_decision(&mut self, response: &GovernanceDecisionResponse) {
        let summary = GovernanceDecisionSummary {
            decision: response.decision.clone(),
            proposal_id: response.proposal_id.clone(),
            finding_count: response.findings.len(),
            timestamp: Utc::now().to_rfc3339(),
        };
        self.decision_history.push(summary);

        match response.decision {
            GovernanceDecision::Rework => {
                self.rework_iterations += 1;
            }
            GovernanceDecision::Approve | GovernanceDecision::ApproveWithConstraints => {
                self.rework_iterations = 0;
                self.current_authorization = response.authorization.clone();
            }
            _ => {}
        }
    }

    pub fn should_escalate(&self) -> bool {
        self.rework_iterations >= self.max_rework_iterations
    }

    pub fn format_rework_feedback(&self, response: &GovernanceDecisionResponse) -> String {
        let mut out = String::new();

        out.push_str(&format!(
            "GOVERNANCE: REWORK (iteration {}/{})\n",
            self.rework_iterations, self.max_rework_iterations
        ));

        if !response.findings.is_empty() {
            out.push_str("\nFindings:\n");
            for f in &response.findings {
                out.push_str(&format!(
                    "  [{}] {} (policy: {})\n",
                    f.severity, f.finding_text, f.policy_id
                ));
            }
        }

        if !response.constraints.is_empty() {
            out.push_str("\nConstraints to address:\n");
            for c in &response.constraints {
                out.push_str(&format!("  - {}\n", c));
            }
        }

        out.push_str("\nRevise your plan to address the findings above and resubmit.\n");

        out
    }
}

// ── Enhanced Brief ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalAffordance {
    pub command: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancedBrief {
    pub normalized_intent: String,
    pub critical_policy: Vec<NormativePolicyStatement>,
    pub advisory_policy: Vec<NormativePolicyStatement>,
    pub unknowns: Vec<String>,
    pub retrieval_affordances: Vec<RetrievalAffordance>,
    pub governance_instruction: String,
}

impl std::fmt::Display for GovernanceDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GovernanceDecision::Approve => write!(f, "APPROVE"),
            GovernanceDecision::ApproveWithConstraints => write!(f, "APPROVE_WITH_CONSTRAINTS"),
            GovernanceDecision::Rework => write!(f, "REWORK"),
            GovernanceDecision::Decompose => write!(f, "DECOMPOSE"),
            GovernanceDecision::Escalate => write!(f, "ESCALATE"),
            GovernanceDecision::Deny => write!(f, "DENY"),
        }
    }
}

impl std::fmt::Display for FindingSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FindingSeverity::Violation => write!(f, "violation"),
            FindingSeverity::Concern => write!(f, "concern"),
            FindingSeverity::Observation => write!(f, "observation"),
        }
    }
}

impl std::fmt::Display for PolicyStrength {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PolicyStrength::Must => write!(f, "MUST"),
            PolicyStrength::MustNot => write!(f, "MUST_NOT"),
            PolicyStrength::Should => write!(f, "SHOULD"),
            PolicyStrength::ShouldNot => write!(f, "SHOULD_NOT"),
            PolicyStrength::May => write!(f, "MAY"),
        }
    }
}

pub fn format_brief_output(brief: &EnhancedBrief) -> String {
    let mut out = String::new();

    out.push_str(&format!("Intent: {}\n", brief.normalized_intent));

    if !brief.critical_policy.is_empty() {
        out.push_str("\nCritical Policies:\n");
        for p in &brief.critical_policy {
            out.push_str(&format!("  [{}] {}\n", p.strength, p.statement));
        }
    }

    if !brief.advisory_policy.is_empty() {
        out.push_str("\nAdvisory Policies:\n");
        for p in &brief.advisory_policy {
            out.push_str(&format!("  [{}] {}\n", p.strength, p.statement));
        }
    }

    if !brief.unknowns.is_empty() {
        out.push_str("\nUnknowns:\n");
        for u in &brief.unknowns {
            out.push_str(&format!("  - {}\n", u));
        }
    }

    if !brief.retrieval_affordances.is_empty() {
        out.push_str("\nRetrieval Affordances:\n");
        for a in &brief.retrieval_affordances {
            out.push_str(&format!("  {} \u{2014} {}\n", a.command, a.description));
        }
    }

    out.push_str(&format!("\nGovernance: {}\n", brief.governance_instruction));

    out
}

// ── Scope Monitoring ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeClassification {
    WithinScope,
    MinorExpansion,
    MaterialExpansion,
    BoundaryViolation,
}

impl std::fmt::Display for ScopeClassification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WithinScope => write!(f, "within_scope"),
            Self::MinorExpansion => write!(f, "minor_expansion"),
            Self::MaterialExpansion => write!(f, "material_expansion"),
            Self::BoundaryViolation => write!(f, "boundary_violation"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeReviewResponse {
    pub decision: GovernanceDecision,
    pub authorization_id: String,
    pub scope_review: ScopeReviewData,
    pub findings: Vec<ConformanceFinding>,
    pub constraints: Vec<String>,
    pub additional_context: Vec<AdditionalContext>,
    pub updated_authorization: Option<BoundedAuthorization>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeReviewData {
    pub classification: ScopeClassification,
    pub details: Vec<String>,
    pub out_of_scope_paths: Vec<String>,
    pub boundary_violations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssuranceResponse {
    pub decision: GovernanceDecision,
    pub authorization_id: String,
    pub assurance: AssuranceResult,
    pub findings: Vec<ConformanceFinding>,
    pub constraints: Vec<String>,
    pub additional_context: Vec<AdditionalContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssuranceConformance {
    Conformant,
    NonConformant,
    InsufficientEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssuranceResult {
    pub conformance: AssuranceConformance,
    pub findings: Vec<AssuranceFinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssuranceFinding {
    pub area: String,
    pub finding_text: String,
    pub severity: FindingSeverity,
    pub policy_id: Option<String>,
    pub strength: Option<PolicyStrength>,
    pub statement: Option<String>,
}

pub fn format_scope_review_output(response: &ScopeReviewResponse) -> String {
    let mut out = String::new();

    out.push_str(&format!("SCOPE REVIEW: {} ({})\n",
        response.decision,
        response.scope_review.classification,
    ));

    if !response.scope_review.boundary_violations.is_empty() {
        out.push_str("\nBoundary violations:\n");
        for v in &response.scope_review.boundary_violations {
            out.push_str(&format!("  - {}\n", v));
        }
    }

    if !response.scope_review.out_of_scope_paths.is_empty() {
        out.push_str("\nOut-of-scope paths:\n");
        for p in &response.scope_review.out_of_scope_paths {
            out.push_str(&format!("  - {}\n", p));
        }
    }

    if !response.findings.is_empty() {
        out.push_str("\nFindings:\n");
        for f in &response.findings {
            out.push_str(&format!("  [{}] {} (policy: {})\n",
                f.severity, f.finding_text, f.policy_id
            ));
        }
    }

    out
}

pub fn format_assurance_output(response: &AssuranceResponse) -> String {
    let mut out = String::new();

    let conformance_label = match response.assurance.conformance {
        AssuranceConformance::Conformant => "CONFORMANT",
        AssuranceConformance::NonConformant => "NON-CONFORMANT",
        AssuranceConformance::InsufficientEvidence => "INSUFFICIENT EVIDENCE",
    };

    out.push_str(&format!("FINAL ASSURANCE: {} ({})\n",
        response.decision, conformance_label,
    ));

    if !response.assurance.findings.is_empty() {
        out.push_str("\nAssurance findings:\n");
        for f in &response.assurance.findings {
            out.push_str(&format!("  [{}] {}\n", f.severity, f.finding_text));
        }
    }

    if response.decision == GovernanceDecision::Rework {
        out.push_str("\nAddress the findings above before completion.\n");
    }

    out
}

// ── Provenance ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProvenanceEventType {
    IntentReceived,
    PrePlanBrief,
    PlanProposed,
    PlanReviewed,
    AuthorizationIssued,
    ScopeChangeDetected,
    ScopeReviewed,
    ReworkRequested,
    DecompositionRequested,
    FinalAssurance,
    ConformanceDetermined,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceEvent {
    pub event_type: ProvenanceEventType,
    pub actor: String,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub policy_refs: Vec<String>,
    #[serde(default)]
    pub context: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceChainResponse {
    pub session_id: String,
    pub events: Vec<ProvenanceEvent>,
}

pub fn format_governance_history(response: &ProvenanceChainResponse) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "GOVERNANCE PROVENANCE — session {}",
        response.session_id
    ));
    lines.push(format!("{} events in chain\n", response.events.len()));

    for (i, event) in response.events.iter().enumerate() {
        let decision_str = event
            .decision
            .as_deref()
            .map(|d| format!(" → {d}"))
            .unwrap_or_default();

        lines.push(format!(
            "  {}. [{:?}]{} — {} at {}",
            i + 1,
            event.event_type,
            decision_str,
            event.actor,
            event.timestamp,
        ));

        if let Some(ref state) = event.repo_state {
            lines.push(format!("     repo: {state}"));
        }

        if !event.evidence_refs.is_empty() {
            lines.push(format!(
                "     evidence: {}",
                event.evidence_refs.join(", ")
            ));
        }

        if !event.policy_refs.is_empty() {
            lines.push(format!(
                "     policies: {}",
                event.policy_refs.join(", ")
            ));
        }
    }

    lines.join("\n")
}

// ── Plan Capture ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum PlanCaptureResult {
    Approved(GovernanceDecisionResponse),
    Blocked(GovernanceDecisionResponse),
    Error(String),
}

pub fn classify_decision_for_hook(response: &GovernanceDecisionResponse) -> PlanCaptureResult {
    match response.decision {
        GovernanceDecision::Approve | GovernanceDecision::ApproveWithConstraints => {
            PlanCaptureResult::Approved(response.clone())
        }
        GovernanceDecision::Rework
        | GovernanceDecision::Decompose
        | GovernanceDecision::Escalate
        | GovernanceDecision::Deny => PlanCaptureResult::Blocked(response.clone()),
    }
}

pub fn format_governance_block_output(response: &GovernanceDecisionResponse) -> String {
    let mut out = String::new();

    out.push_str(&format!("GOVERNANCE DECISION: {}\n", response.decision));

    if !response.findings.is_empty() {
        out.push_str("\nFindings:\n");
        for f in &response.findings {
            out.push_str(&format!(
                "  [{}] {} (policy: {})\n",
                f.severity, f.finding_text, f.policy_id
            ));
        }
    }

    match response.decision {
        GovernanceDecision::Decompose => {
            if let Some(ref guidance) = response.decomposition_guidance {
                out.push_str("\nDecomposition guidance:\n");
                if !guidance.reasons.is_empty() {
                    out.push_str("  Reasons:\n");
                    for r in &guidance.reasons {
                        out.push_str(&format!("    - {}\n", r));
                    }
                }
                if !guidance.suggested_boundaries.is_empty() {
                    out.push_str("  Suggested boundaries:\n");
                    for b in &guidance.suggested_boundaries {
                        out.push_str(&format!("    - {}\n", b));
                    }
                }
                if !guidance.suggested_tasks.is_empty() {
                    out.push_str("  Suggested tasks:\n");
                    for (i, t) in guidance.suggested_tasks.iter().enumerate() {
                        out.push_str(&format!(
                            "    {}. {} \u{2014} {}\n",
                            i + 1,
                            t.title,
                            t.description
                        ));
                    }
                }
                out.push_str("\nBreak this plan into the suggested tasks above and submit each separately with `actual governance submit-plan`.\n");
            }
        }
        GovernanceDecision::Rework => {
            if !response.constraints.is_empty() {
                out.push_str("\nConstraints to address:\n");
                for c in &response.constraints {
                    out.push_str(&format!("  - {}\n", c));
                }
            }
            out.push_str("\nRevise your plan to address the findings above and resubmit with `actual governance submit-plan`.\n");
        }
        GovernanceDecision::Escalate => {
            out.push_str("\nThis plan requires human architect review. Escalation has been filed.\n");
        }
        GovernanceDecision::Deny => {
            out.push_str("\nThis plan has been denied. Review the findings above and consider an alternative approach.\n");
        }
        GovernanceDecision::Approve | GovernanceDecision::ApproveWithConstraints => {}
    }

    out
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

    // 12. GovernanceState — new() defaults
    #[test]
    fn governance_state_new_defaults() {
        let state = GovernanceState::new("sess-001".into());
        assert_eq!(state.session_id, "sess-001");
        assert_eq!(state.rework_iterations, 0);
        assert_eq!(state.max_rework_iterations, 5);
        assert!(state.current_authorization.is_none());
        assert!(state.decision_history.is_empty());
    }

    // 13. GovernanceState — record_decision stores authorization on APPROVE
    #[test]
    fn governance_state_record_approve_stores_auth() {
        let mut state = GovernanceState::new("sess-001".into());
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
        state.record_decision(&resp);
        assert!(state.current_authorization.is_some());
        assert_eq!(
            state.current_authorization.as_ref().unwrap().authorization_id,
            "auth-001"
        );
        assert_eq!(state.rework_iterations, 0);
        assert_eq!(state.decision_history.len(), 1);
        assert_eq!(state.decision_history[0].proposal_id, "prop-001");
        assert_eq!(state.decision_history[0].finding_count, 0);
    }

    // 14. GovernanceState — record_decision increments rework_iterations on REWORK
    #[test]
    fn governance_state_record_rework_increments() {
        let mut state = GovernanceState::new("sess-001".into());
        let resp = GovernanceDecisionResponse {
            decision: GovernanceDecision::Rework,
            proposal_id: "prop-002".into(),
            findings: vec![ConformanceFinding {
                policy_id: "pol-001".into(),
                strength: PolicyStrength::Must,
                statement: "RLS required".into(),
                finding_text: "Missing RLS".into(),
                severity: FindingSeverity::Violation,
                evidence_ref: None,
            }],
            constraints: vec!["Add RLS".into()],
            additional_context: vec![],
            decomposition_guidance: None,
            authorization: None,
        };
        state.record_decision(&resp);
        assert_eq!(state.rework_iterations, 1);
        assert!(state.current_authorization.is_none());

        state.record_decision(&resp);
        assert_eq!(state.rework_iterations, 2);
        assert_eq!(state.decision_history.len(), 2);
    }

    // 15. GovernanceState — should_escalate returns true after max iterations
    #[test]
    fn governance_state_should_escalate_after_max() {
        let mut state = GovernanceState::new("sess-001".into());
        assert!(!state.should_escalate());

        let resp = GovernanceDecisionResponse {
            decision: GovernanceDecision::Rework,
            proposal_id: "prop-003".into(),
            findings: vec![],
            constraints: vec![],
            additional_context: vec![],
            decomposition_guidance: None,
            authorization: None,
        };

        for i in 0..5 {
            assert!(!state.should_escalate(), "should not escalate at iteration {}", i);
            state.record_decision(&resp);
        }
        assert!(state.should_escalate());
        assert_eq!(state.rework_iterations, 5);
    }

    // 16. GovernanceState — format_rework_feedback shows iteration count and findings
    #[test]
    fn governance_state_format_rework_shows_iteration() {
        let mut state = GovernanceState::new("sess-001".into());
        let resp = GovernanceDecisionResponse {
            decision: GovernanceDecision::Rework,
            proposal_id: "prop-004".into(),
            findings: vec![
                ConformanceFinding {
                    policy_id: "pol-001".into(),
                    strength: PolicyStrength::Must,
                    statement: "RLS required".into(),
                    finding_text: "Missing RLS on users table".into(),
                    severity: FindingSeverity::Violation,
                    evidence_ref: None,
                },
                ConformanceFinding {
                    policy_id: "pol-002".into(),
                    strength: PolicyStrength::Should,
                    statement: "Use CTEs".into(),
                    finding_text: "Nested subquery too deep".into(),
                    severity: FindingSeverity::Concern,
                    evidence_ref: None,
                },
            ],
            constraints: vec!["Add RLS to all tables".into(), "Refactor to use CTEs".into()],
            additional_context: vec![],
            decomposition_guidance: None,
            authorization: None,
        };

        // Record 2 rework decisions so iteration shows 2/5
        state.record_decision(&resp);
        state.record_decision(&resp);

        let output = state.format_rework_feedback(&resp);

        assert!(
            output.contains("GOVERNANCE: REWORK (iteration 2/5)"),
            "should show iteration count, got: {}",
            output
        );
        assert!(
            output.contains("[violation] Missing RLS on users table (policy: pol-001)"),
            "should show violation finding, got: {}",
            output
        );
        assert!(
            output.contains("[concern] Nested subquery too deep (policy: pol-002)"),
            "should show concern finding, got: {}",
            output
        );
        assert!(
            output.contains("- Add RLS to all tables"),
            "should show constraint, got: {}",
            output
        );
        assert!(
            output.contains("- Refactor to use CTEs"),
            "should show constraint, got: {}",
            output
        );
        assert!(
            output.contains("Revise your plan to address the findings above and resubmit."),
            "should show resubmit instruction, got: {}",
            output
        );
    }

    // 16b. GovernanceState — approval resets rework counter
    #[test]
    fn governance_state_approval_resets_rework_counter() {
        let mut state = GovernanceState::new("sess-001".into());
        let rework_resp = GovernanceDecisionResponse {
            decision: GovernanceDecision::Rework,
            proposal_id: "prop-001".into(),
            findings: vec![ConformanceFinding {
                policy_id: "pol-001".into(),
                strength: PolicyStrength::Must,
                statement: "RLS required".into(),
                finding_text: "Missing RLS".into(),
                severity: FindingSeverity::Violation,
                evidence_ref: None,
            }],
            constraints: vec![],
            additional_context: vec![],
            decomposition_guidance: None,
            authorization: None,
        };
        state.record_decision(&rework_resp);
        state.record_decision(&rework_resp);
        state.record_decision(&rework_resp);
        assert_eq!(state.rework_iterations, 3);

        let approve_resp = GovernanceDecisionResponse {
            decision: GovernanceDecision::Approve,
            proposal_id: "prop-001".into(),
            findings: vec![],
            constraints: vec![],
            additional_context: vec![],
            decomposition_guidance: None,
            authorization: Some(BoundedAuthorization {
                authorization_id: "auth-reset".into(),
                task_id: "task-001".into(),
                repo_state: "abc123".into(),
                allowed_scope: vec![],
                protected_boundaries: vec![],
                policies: vec![],
                conditions: vec![],
                issued_at: "2026-08-11T00:00:00Z".into(),
                ttl_seconds: 3600,
            }),
        };
        state.record_decision(&approve_resp);
        assert_eq!(state.rework_iterations, 0);

        // New rework cycle starts fresh
        state.record_decision(&rework_resp);
        assert_eq!(state.rework_iterations, 1);
        assert!(!state.should_escalate());
    }

    // 16c. GovernanceState — approval without authorization clears stale auth
    #[test]
    fn governance_state_approval_clears_stale_authorization() {
        let mut state = GovernanceState::new("sess-001".into());
        let approve_with_auth = GovernanceDecisionResponse {
            decision: GovernanceDecision::Approve,
            proposal_id: "prop-001".into(),
            findings: vec![],
            constraints: vec![],
            additional_context: vec![],
            decomposition_guidance: None,
            authorization: Some(BoundedAuthorization {
                authorization_id: "auth-old".into(),
                task_id: "task-001".into(),
                repo_state: "abc123".into(),
                allowed_scope: vec![],
                protected_boundaries: vec![],
                policies: vec![],
                conditions: vec![],
                issued_at: "2026-08-11T00:00:00Z".into(),
                ttl_seconds: 3600,
            }),
        };
        state.record_decision(&approve_with_auth);
        assert!(state.current_authorization.is_some());

        let approve_without_auth = GovernanceDecisionResponse {
            decision: GovernanceDecision::Approve,
            proposal_id: "prop-002".into(),
            findings: vec![],
            constraints: vec![],
            additional_context: vec![],
            decomposition_guidance: None,
            authorization: None,
        };
        state.record_decision(&approve_without_auth);
        assert!(state.current_authorization.is_none());
    }

    // 16d. GovernanceState — record_decision with Decompose/Escalate/Deny
    #[test]
    fn governance_state_record_other_decisions() {
        let mut state = GovernanceState::new("sess-001".into());
        for decision in [
            GovernanceDecision::Decompose,
            GovernanceDecision::Escalate,
            GovernanceDecision::Deny,
        ] {
            let resp = GovernanceDecisionResponse {
                decision,
                proposal_id: "prop-other".into(),
                findings: vec![],
                constraints: vec![],
                additional_context: vec![],
                decomposition_guidance: None,
                authorization: None,
            };
            state.record_decision(&resp);
        }
        assert_eq!(state.decision_history.len(), 3);
        assert_eq!(state.rework_iterations, 0);
        assert!(state.current_authorization.is_none());
    }

    // ── Enhanced Brief tests ──────────────────────────────────────────

    // 14. RetrievalAffordance — serde round-trip
    #[test]
    fn retrieval_affordance_round_trip() {
        let affordance = RetrievalAffordance {
            command: "actual rules list".into(),
            description: "List all ADR rules".into(),
        };
        let json = serde_json::to_string(&affordance).unwrap();
        assert!(json.contains("\"command\":\"actual rules list\""));
        assert!(json.contains("\"description\":\"List all ADR rules\""));
        let back: RetrievalAffordance = serde_json::from_str(&json).unwrap();
        assert_eq!(back.command, "actual rules list");
        assert_eq!(back.description, "List all ADR rules");
    }

    // 15. EnhancedBrief — serde round-trip with all fields populated
    #[test]
    fn enhanced_brief_round_trip() {
        let brief = EnhancedBrief {
            normalized_intent: "Add user profile table with RLS".into(),
            critical_policy: vec![NormativePolicyStatement {
                strength: PolicyStrength::Must,
                statement: "All tables must have RLS enabled".into(),
                policy_id: "pol-001".into(),
                source_adr_id: Some("adr-042".into()),
            }],
            advisory_policy: vec![NormativePolicyStatement {
                strength: PolicyStrength::Should,
                statement: "Use CTEs for multi-stage queries".into(),
                policy_id: "pol-002".into(),
                source_adr_id: None,
            }],
            unknowns: vec!["Does the profiles table need partitioning?".into()],
            retrieval_affordances: vec![RetrievalAffordance {
                command: "actual rules list".into(),
                description: "List all ADR rules".into(),
            }],
            governance_instruction: "Submit plan for review before implementing".into(),
        };
        let json = serde_json::to_string(&brief).unwrap();
        let back: EnhancedBrief = serde_json::from_str(&json).unwrap();
        assert_eq!(back.normalized_intent, "Add user profile table with RLS");
        assert_eq!(back.critical_policy.len(), 1);
        assert_eq!(back.critical_policy[0].strength, PolicyStrength::Must);
        assert_eq!(back.advisory_policy.len(), 1);
        assert_eq!(back.advisory_policy[0].strength, PolicyStrength::Should);
        assert_eq!(back.unknowns.len(), 1);
        assert_eq!(back.retrieval_affordances.len(), 1);
        assert_eq!(
            back.governance_instruction,
            "Submit plan for review before implementing"
        );
    }

    // 16. EnhancedBrief — empty optional collection fields
    #[test]
    fn enhanced_brief_empty_optional_fields() {
        let brief = EnhancedBrief {
            normalized_intent: "Simple refactor".into(),
            critical_policy: vec![],
            advisory_policy: vec![],
            unknowns: vec![],
            retrieval_affordances: vec![],
            governance_instruction: "Proceed".into(),
        };
        let json = serde_json::to_string(&brief).unwrap();
        let back: EnhancedBrief = serde_json::from_str(&json).unwrap();
        assert_eq!(back.normalized_intent, "Simple refactor");
        assert!(back.critical_policy.is_empty());
        assert!(back.advisory_policy.is_empty());
        assert!(back.unknowns.is_empty());
        assert!(back.retrieval_affordances.is_empty());
        assert_eq!(back.governance_instruction, "Proceed");
    }

    // 17. format_brief_output — displays MUST policies
    #[test]
    fn format_brief_displays_must_policies() {
        let brief = EnhancedBrief {
            normalized_intent: "Add table".into(),
            critical_policy: vec![
                NormativePolicyStatement {
                    strength: PolicyStrength::Must,
                    statement: "Enable RLS on all tables".into(),
                    policy_id: "pol-001".into(),
                    source_adr_id: None,
                },
                NormativePolicyStatement {
                    strength: PolicyStrength::MustNot,
                    statement: "Never expose admin client to frontend".into(),
                    policy_id: "pol-003".into(),
                    source_adr_id: None,
                },
            ],
            advisory_policy: vec![],
            unknowns: vec![],
            retrieval_affordances: vec![],
            governance_instruction: "Review required".into(),
        };
        let output = format_brief_output(&brief);
        assert!(output.contains("[MUST] Enable RLS on all tables"));
        assert!(output.contains("[MUST_NOT] Never expose admin client to frontend"));
    }

    // 18. format_brief_output — displays SHOULD / SHOULD_NOT / MAY policies
    #[test]
    fn format_brief_displays_should_policies() {
        let brief = EnhancedBrief {
            normalized_intent: "Refactor queries".into(),
            critical_policy: vec![],
            advisory_policy: vec![
                NormativePolicyStatement {
                    strength: PolicyStrength::Should,
                    statement: "Use CTEs for readability".into(),
                    policy_id: "pol-010".into(),
                    source_adr_id: None,
                },
                NormativePolicyStatement {
                    strength: PolicyStrength::ShouldNot,
                    statement: "Avoid nested subqueries beyond 2 levels".into(),
                    policy_id: "pol-011".into(),
                    source_adr_id: None,
                },
                NormativePolicyStatement {
                    strength: PolicyStrength::May,
                    statement: "Use materialized views for caching".into(),
                    policy_id: "pol-012".into(),
                    source_adr_id: None,
                },
            ],
            unknowns: vec![],
            retrieval_affordances: vec![],
            governance_instruction: "Proceed".into(),
        };
        let output = format_brief_output(&brief);
        assert!(output.contains("[SHOULD] Use CTEs for readability"));
        assert!(output.contains("[SHOULD_NOT] Avoid nested subqueries beyond 2 levels"));
        assert!(output.contains("[MAY] Use materialized views for caching"));
    }

    // 19. format_brief_output — displays unknowns as bulleted list
    #[test]
    fn format_brief_displays_unknowns() {
        let brief = EnhancedBrief {
            normalized_intent: "Add partitioning".into(),
            critical_policy: vec![],
            advisory_policy: vec![],
            unknowns: vec![
                "What is the expected table size?".into(),
                "Which partition key to use?".into(),
            ],
            retrieval_affordances: vec![],
            governance_instruction: "Investigate first".into(),
        };
        let output = format_brief_output(&brief);
        assert!(output.contains("- What is the expected table size?"));
        assert!(output.contains("- Which partition key to use?"));
    }

    // 20. format_brief_output — displays retrieval affordances
    #[test]
    fn format_brief_displays_affordances() {
        let brief = EnhancedBrief {
            normalized_intent: "Check rules".into(),
            critical_policy: vec![],
            advisory_policy: vec![],
            unknowns: vec![],
            retrieval_affordances: vec![
                RetrievalAffordance {
                    command: "actual rules list".into(),
                    description: "List all ADR rules".into(),
                },
                RetrievalAffordance {
                    command: "actual rules show pol-001".into(),
                    description: "Show details of policy pol-001".into(),
                },
            ],
            governance_instruction: "Proceed".into(),
        };
        let output = format_brief_output(&brief);
        assert!(output.contains("actual rules list \u{2014} List all ADR rules"));
        assert!(
            output.contains("actual rules show pol-001 \u{2014} Show details of policy pol-001")
        );
    }

    // 21. format_brief_output — displays governance instruction
    #[test]
    fn format_brief_displays_governance_instruction() {
        let brief = EnhancedBrief {
            normalized_intent: "Deploy change".into(),
            critical_policy: vec![],
            advisory_policy: vec![],
            unknowns: vec![],
            retrieval_affordances: vec![],
            governance_instruction: "Submit plan for architecture review before coding".into(),
        };
        let output = format_brief_output(&brief);
        assert!(output.contains("Governance: Submit plan for architecture review before coding"));
    }

    // ── Plan Capture tests ───────────────────────────────────────────

    fn make_response(decision: GovernanceDecision) -> GovernanceDecisionResponse {
        GovernanceDecisionResponse {
            decision,
            proposal_id: "prop-test".into(),
            findings: vec![],
            constraints: vec![],
            additional_context: vec![],
            decomposition_guidance: None,
            authorization: None,
        }
    }

    // 22. classify_decision_for_hook — APPROVE maps to Approved
    #[test]
    fn classify_approve_is_approved() {
        let resp = make_response(GovernanceDecision::Approve);
        let result = classify_decision_for_hook(&resp);
        assert!(matches!(result, PlanCaptureResult::Approved(_)));
    }

    // 23. classify_decision_for_hook — APPROVE_WITH_CONSTRAINTS maps to Approved
    #[test]
    fn classify_approve_with_constraints_is_approved() {
        let resp = make_response(GovernanceDecision::ApproveWithConstraints);
        let result = classify_decision_for_hook(&resp);
        assert!(matches!(result, PlanCaptureResult::Approved(_)));
    }

    // 24. classify_decision_for_hook — REWORK maps to Blocked
    #[test]
    fn classify_rework_is_blocked() {
        let resp = make_response(GovernanceDecision::Rework);
        let result = classify_decision_for_hook(&resp);
        assert!(matches!(result, PlanCaptureResult::Blocked(_)));
    }

    // 25. classify_decision_for_hook — DECOMPOSE maps to Blocked
    #[test]
    fn classify_decompose_is_blocked() {
        let resp = make_response(GovernanceDecision::Decompose);
        let result = classify_decision_for_hook(&resp);
        assert!(matches!(result, PlanCaptureResult::Blocked(_)));
    }

    // 26. classify_decision_for_hook — ESCALATE maps to Blocked
    #[test]
    fn classify_escalate_is_blocked() {
        let resp = make_response(GovernanceDecision::Escalate);
        let result = classify_decision_for_hook(&resp);
        assert!(matches!(result, PlanCaptureResult::Blocked(_)));
    }

    // 27. classify_decision_for_hook — DENY maps to Blocked
    #[test]
    fn classify_deny_is_blocked() {
        let resp = make_response(GovernanceDecision::Deny);
        let result = classify_decision_for_hook(&resp);
        assert!(matches!(result, PlanCaptureResult::Blocked(_)));
    }

    // 28. format_governance_block_output — output contains the decision string
    #[test]
    fn format_block_shows_decision_type() {
        let resp = make_response(GovernanceDecision::Rework);
        let output = format_governance_block_output(&resp);
        assert!(
            output.contains("REWORK"),
            "output should contain decision type, got: {}",
            output
        );
    }

    // 29. format_governance_block_output — output contains finding text and policy_id
    #[test]
    fn format_block_shows_findings() {
        let mut resp = make_response(GovernanceDecision::Rework);
        resp.findings = vec![ConformanceFinding {
            policy_id: "pol-001".into(),
            strength: PolicyStrength::Must,
            statement: "RLS required".into(),
            finding_text: "Missing RLS on users table".into(),
            severity: FindingSeverity::Violation,
            evidence_ref: None,
        }];
        let output = format_governance_block_output(&resp);
        assert!(
            output.contains("Missing RLS on users table"),
            "output should contain finding_text, got: {}",
            output
        );
        assert!(
            output.contains("pol-001"),
            "output should contain policy_id, got: {}",
            output
        );
    }

    // 30. format_governance_block_output — output contains suggested tasks when DECOMPOSE
    #[test]
    fn format_block_shows_decompose_guidance() {
        let mut resp = make_response(GovernanceDecision::Decompose);
        resp.decomposition_guidance = Some(DecompositionGuidance {
            reasons: vec!["Too broad".into()],
            suggested_boundaries: vec!["database".into()],
            suggested_tasks: vec![SuggestedTask {
                title: "Add migration".into(),
                description: "Create schema migration".into(),
            }],
        });
        let output = format_governance_block_output(&resp);
        assert!(
            output.contains("Add migration"),
            "output should contain suggested task title, got: {}",
            output
        );
        assert!(
            output.contains("Create schema migration"),
            "output should contain suggested task description, got: {}",
            output
        );
    }

    // 31. format_governance_block_output — tells agent to revise and resubmit
    #[test]
    fn format_block_shows_rework_instructions() {
        let mut resp = make_response(GovernanceDecision::Rework);
        resp.constraints = vec!["Add RLS".into()];
        let output = format_governance_block_output(&resp);
        assert!(
            output.contains("Revise"),
            "output should tell agent to revise, got: {}",
            output
        );
        assert!(
            output.contains("resubmit"),
            "output should tell agent to resubmit, got: {}",
            output
        );
    }

    // ── Scope Monitoring tests ───────────────────────────────────────

    // 20. ScopeClassification serde round-trip
    #[test]
    fn scope_classification_round_trip() {
        let variants = vec![
            (ScopeClassification::WithinScope, "\"within_scope\""),
            (ScopeClassification::MinorExpansion, "\"minor_expansion\""),
            (ScopeClassification::MaterialExpansion, "\"material_expansion\""),
            (ScopeClassification::BoundaryViolation, "\"boundary_violation\""),
        ];
        for (variant, expected_json) in variants {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected_json);
            let back: ScopeClassification = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    // 21. ScopeReviewResponse serde round-trip
    #[test]
    fn scope_review_response_round_trip() {
        let resp = ScopeReviewResponse {
            decision: GovernanceDecision::Escalate,
            authorization_id: "auth-001".into(),
            scope_review: ScopeReviewData {
                classification: ScopeClassification::BoundaryViolation,
                details: vec!["Crossed migration boundary".into()],
                out_of_scope_paths: vec![],
                boundary_violations: vec!["supabase/migrations/001.sql".into()],
            },
            findings: vec![ConformanceFinding {
                policy_id: "scope-boundary".into(),
                strength: PolicyStrength::MustNot,
                statement: "Must not cross protected boundary".into(),
                finding_text: "Boundary violation".into(),
                severity: FindingSeverity::Violation,
                evidence_ref: None,
            }],
            constraints: vec![],
            additional_context: vec![],
            updated_authorization: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: ScopeReviewResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.decision, GovernanceDecision::Escalate);
        assert_eq!(back.scope_review.classification, ScopeClassification::BoundaryViolation);
    }

    // 22. AssuranceConformance serde round-trip
    #[test]
    fn assurance_conformance_round_trip() {
        let variants = vec![
            (AssuranceConformance::Conformant, "\"conformant\""),
            (AssuranceConformance::NonConformant, "\"non_conformant\""),
            (AssuranceConformance::InsufficientEvidence, "\"insufficient_evidence\""),
        ];
        for (variant, expected_json) in variants {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected_json);
            let back: AssuranceConformance = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    // 23. AssuranceResponse serde round-trip
    #[test]
    fn assurance_response_round_trip() {
        let resp = AssuranceResponse {
            decision: GovernanceDecision::Approve,
            authorization_id: "auth-001".into(),
            assurance: AssuranceResult {
                conformance: AssuranceConformance::Conformant,
                findings: vec![],
            },
            findings: vec![],
            constraints: vec![],
            additional_context: vec![],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: AssuranceResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.decision, GovernanceDecision::Approve);
        assert_eq!(back.assurance.conformance, AssuranceConformance::Conformant);
    }

    // 24. GovernanceState — track_path deduplicates
    #[test]
    fn governance_state_track_path_deduplicates() {
        let mut state = GovernanceState::new("sess-001".into());
        state.track_path("src/handler.ts");
        state.track_path("src/handler.ts");
        state.track_path("src/utils.ts");
        assert_eq!(state.touched_paths.len(), 2);
    }

    // 25. GovernanceState — needs_scope_review
    #[test]
    fn governance_state_needs_scope_review() {
        let mut state = GovernanceState::new("sess-001".into());
        assert!(!state.needs_scope_review());

        state.current_authorization = Some(BoundedAuthorization {
            authorization_id: "auth-001".into(),
            task_id: "task-001".into(),
            repo_state: "abc123".into(),
            allowed_scope: vec!["src/**".into()],
            protected_boundaries: vec![],
            policies: vec![],
            conditions: vec![],
            issued_at: "2026-08-11T00:00:00Z".into(),
            ttl_seconds: 3600,
        });
        assert!(!state.needs_scope_review());

        state.track_path("src/handler.ts");
        assert!(state.needs_scope_review());
    }

    // 26. GovernanceState — needs_assurance
    #[test]
    fn governance_state_needs_assurance() {
        let mut state = GovernanceState::new("sess-001".into());
        assert!(!state.needs_assurance());

        state.current_authorization = Some(BoundedAuthorization {
            authorization_id: "auth-001".into(),
            task_id: "task-001".into(),
            repo_state: "abc123".into(),
            allowed_scope: vec![],
            protected_boundaries: vec![],
            policies: vec![],
            conditions: vec![],
            issued_at: "2026-08-11T00:00:00Z".into(),
            ttl_seconds: 3600,
        });
        assert!(state.needs_assurance());

        state.assurance_passed = true;
        assert!(!state.needs_assurance());
    }

    // 27. format_scope_review_output
    #[test]
    fn format_scope_review_shows_violations() {
        let resp = ScopeReviewResponse {
            decision: GovernanceDecision::Escalate,
            authorization_id: "auth-001".into(),
            scope_review: ScopeReviewData {
                classification: ScopeClassification::BoundaryViolation,
                details: vec![],
                out_of_scope_paths: vec![],
                boundary_violations: vec!["supabase/migrations/001.sql".into()],
            },
            findings: vec![ConformanceFinding {
                policy_id: "scope-boundary".into(),
                strength: PolicyStrength::MustNot,
                statement: "No migrations".into(),
                finding_text: "Boundary violation: migrations".into(),
                severity: FindingSeverity::Violation,
                evidence_ref: None,
            }],
            constraints: vec![],
            additional_context: vec![],
            updated_authorization: None,
        };
        let output = format_scope_review_output(&resp);
        assert!(output.contains("SCOPE REVIEW:"));
        assert!(output.contains("boundary_violation"));
        assert!(output.contains("supabase/migrations/001.sql"));
    }

    // 28. format_assurance_output
    #[test]
    fn format_assurance_output_shows_conformance() {
        let resp = AssuranceResponse {
            decision: GovernanceDecision::Approve,
            authorization_id: "auth-001".into(),
            assurance: AssuranceResult {
                conformance: AssuranceConformance::Conformant,
                findings: vec![],
            },
            findings: vec![],
            constraints: vec![],
            additional_context: vec![],
        };
        let output = format_assurance_output(&resp);
        assert!(output.contains("FINAL ASSURANCE:"));
        assert!(output.contains("CONFORMANT"));
    }

    // 29. format_assurance_output — rework shows instruction
    #[test]
    fn format_assurance_output_rework_shows_instruction() {
        let resp = AssuranceResponse {
            decision: GovernanceDecision::Rework,
            authorization_id: "auth-001".into(),
            assurance: AssuranceResult {
                conformance: AssuranceConformance::NonConformant,
                findings: vec![AssuranceFinding {
                    area: "tests".into(),
                    finding_text: "2 tests failing".into(),
                    severity: FindingSeverity::Violation,
                    policy_id: None,
                    strength: None,
                    statement: None,
                }],
            },
            findings: vec![],
            constraints: vec![],
            additional_context: vec![],
        };
        let output = format_assurance_output(&resp);
        assert!(output.contains("NON-CONFORMANT"));
        assert!(output.contains("2 tests failing"));
        assert!(output.contains("Address the findings"));
    }

    // ── Provenance tests ─────────────────────────────────────────────

    // 30. ProvenanceEventType serde round-trip
    #[test]
    fn provenance_event_type_round_trip() {
        let types = vec![
            ProvenanceEventType::IntentReceived,
            ProvenanceEventType::PrePlanBrief,
            ProvenanceEventType::PlanProposed,
            ProvenanceEventType::PlanReviewed,
            ProvenanceEventType::AuthorizationIssued,
            ProvenanceEventType::ScopeChangeDetected,
            ProvenanceEventType::ScopeReviewed,
            ProvenanceEventType::ReworkRequested,
            ProvenanceEventType::DecompositionRequested,
            ProvenanceEventType::FinalAssurance,
            ProvenanceEventType::ConformanceDetermined,
        ];
        for t in types {
            let json = serde_json::to_string(&t).unwrap();
            let back: ProvenanceEventType = serde_json::from_str(&json).unwrap();
            assert_eq!(t, back);
        }
    }

    // 31. ProvenanceEvent round-trip
    #[test]
    fn provenance_event_round_trip() {
        let event = ProvenanceEvent {
            event_type: ProvenanceEventType::PlanReviewed,
            actor: "governance-evaluator".into(),
            timestamp: "2026-08-11T12:00:00Z".into(),
            repo_state: Some("abc123".into()),
            decision: Some("APPROVE".into()),
            evidence_refs: vec!["proposal-1".into()],
            policy_refs: vec!["p1".into(), "p2".into()],
            context: serde_json::json!({"findings_count": 0}),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: ProvenanceEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.event_type, ProvenanceEventType::PlanReviewed);
        assert_eq!(back.decision, Some("APPROVE".into()));
        assert_eq!(back.policy_refs.len(), 2);
    }

    // 32. ProvenanceChainResponse round-trip
    #[test]
    fn provenance_chain_response_round_trip() {
        let resp = ProvenanceChainResponse {
            session_id: "sess-1".into(),
            events: vec![
                ProvenanceEvent {
                    event_type: ProvenanceEventType::IntentReceived,
                    actor: "agent".into(),
                    timestamp: "2026-08-11T12:00:00Z".into(),
                    repo_state: None,
                    decision: None,
                    evidence_refs: vec![],
                    policy_refs: vec![],
                    context: serde_json::json!({}),
                },
                ProvenanceEvent {
                    event_type: ProvenanceEventType::PlanReviewed,
                    actor: "evaluator".into(),
                    timestamp: "2026-08-11T12:00:01Z".into(),
                    repo_state: Some("abc123".into()),
                    decision: Some("APPROVE".into()),
                    evidence_refs: vec!["proposal-1".into()],
                    policy_refs: vec![],
                    context: serde_json::json!({}),
                },
            ],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: ProvenanceChainResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session_id, "sess-1");
        assert_eq!(back.events.len(), 2);
    }

    // 33. format_governance_history shows events
    #[test]
    fn format_governance_history_shows_events() {
        let resp = ProvenanceChainResponse {
            session_id: "sess-1".into(),
            events: vec![
                ProvenanceEvent {
                    event_type: ProvenanceEventType::PlanProposed,
                    actor: "agent".into(),
                    timestamp: "2026-08-11T12:00:00Z".into(),
                    repo_state: Some("abc123".into()),
                    decision: None,
                    evidence_refs: vec!["proposal-1".into()],
                    policy_refs: vec![],
                    context: serde_json::json!({}),
                },
                ProvenanceEvent {
                    event_type: ProvenanceEventType::PlanReviewed,
                    actor: "evaluator".into(),
                    timestamp: "2026-08-11T12:00:01Z".into(),
                    repo_state: None,
                    decision: Some("APPROVE".into()),
                    evidence_refs: vec![],
                    policy_refs: vec!["policy-1".into()],
                    context: serde_json::json!({}),
                },
            ],
        };
        let output = format_governance_history(&resp);
        assert!(output.contains("GOVERNANCE PROVENANCE"));
        assert!(output.contains("sess-1"));
        assert!(output.contains("2 events"));
        assert!(output.contains("PlanProposed"));
        assert!(output.contains("APPROVE"));
        assert!(output.contains("abc123"));
        assert!(output.contains("policy-1"));
    }

    // 34. format_governance_history empty chain
    #[test]
    fn format_governance_history_empty_chain() {
        let resp = ProvenanceChainResponse {
            session_id: "sess-empty".into(),
            events: vec![],
        };
        let output = format_governance_history(&resp);
        assert!(output.contains("0 events"));
    }
}
