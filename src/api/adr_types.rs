//! Request and response bodies for the decision routes under `/v1/cli/adrs`,
//! which `actual adr` calls with the signed-in session's token.
//!
//! Responses deserialize leniently: a field this build doesn't know is
//! ignored, and `kind`, `state` and `status` stay strings, so a value the
//! server adds later doesn't break `list` or `show`. Requests serialize only
//! the fields the routes accept, because every route refuses unknown keys.

use serde::{Deserialize, Serialize};

use crate::rules::types::RuleLevel;

/// A decision's list on the web app's decisions page, the `state` filter of
/// `actual adr list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum DecisionState {
    Active,
    Draft,
    Suggested,
    Updated,
    Archived,
}

impl DecisionState {
    /// The query-string spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Draft => "draft",
            Self::Suggested => "suggested",
            Self::Updated => "updated",
            Self::Archived => "archived",
        }
    }
}

/// How a create saves a decision: `accepted` governs at once, `draft` waits
/// for a publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum SaveStatus {
    Accepted,
    Draft,
}

impl SaveStatus {
    /// The wire spelling, also the status a local file's header carries.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Draft => "draft",
        }
    }
}

/// One policy of a stored decision. `id` is `<decision row id>:<rule id>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionPolicy {
    pub id: String,
    pub level: RuleLevel,
    pub statement: String,
}

/// Where a decision applies: `root` `.` is the whole repository, and empty
/// `paths` the whole root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionScope {
    pub root: String,
    #[serde(default)]
    pub paths: Vec<String>,
}

/// A decision's category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionCategory {
    pub id: String,
    pub name: String,
    pub path: String,
}

/// A decision as `list` and `show` return it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub id: String,
    /// `single`, or `group` for an AI-grouped decision.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub repo_unique_id: Option<String>,
    pub title: String,
    /// `active`, `draft`, `suggested`, `updated` or `archived`.
    pub state: String,
    /// The stored status, such as `accepted`, `draft` or `created-from-user`.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub date: String,
    /// One point per line.
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub policies: Vec<DecisionPolicy>,
    #[serde(default)]
    pub scope: Option<DecisionScope>,
    #[serde(default)]
    pub category: Option<DecisionCategory>,
    /// For an `updated` decision, the active decision it would change.
    #[serde(default)]
    pub base_adr_id: Option<String>,
}

/// One page of `GET /v1/cli/adrs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionList {
    pub decisions: Vec<Decision>,
    /// Matching decisions across every page.
    pub total: u64,
    /// The offset of the next page, or `None` on the last one.
    #[serde(default)]
    pub next_offset: Option<u64>,
}

/// `GET /v1/cli/adrs/:adr_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionShow {
    pub decision: Decision,
}

/// `POST /v1/cli/adrs`: a decision sent as Context File markdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CreateDecisionRequest {
    pub org_id: String,
    pub repo_unique_id: String,
    pub status: SaveStatus,
    pub markdown: String,
}

/// What a create saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedDecision {
    pub adr_id: String,
    pub status: String,
    /// The title as stored, trimmed.
    pub title: String,
    pub repo_unique_id: String,
}

/// One policy in an `update`'s full policy list: no `id` adds it, and a
/// policy left out of the list is removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PolicyEdit {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub level: RuleLevel,
    pub statement: String,
}

/// A level change for one existing policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LevelEdit {
    pub id: String,
    pub level: RuleLevel,
}

/// The paths a decision applies to, under its existing root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScopeEdit {
    pub paths: Vec<String>,
}

/// `POST /v1/cli/adrs/:adr_id/update`. The route needs at least one field
/// besides `org_id`, and never `policies` and `levels` together.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct UpdateDecisionRequest {
    pub org_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// One point per line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policies: Option<Vec<PolicyEdit>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub levels: Option<Vec<LevelEdit>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeEdit>,
}

/// The answer to an update or a lifecycle action. `sync` is `requested` when
/// the change started a sync of the repository's rule files, else
/// `not_needed`. `reason_saved` comes only from `reject`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionAction {
    #[serde(default)]
    pub success: bool,
    pub adr_id: String,
    #[serde(default)]
    pub sync: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_saved: Option<bool>,
}

impl DecisionAction {
    /// Whether the change started a sync of the repository's rule files.
    pub fn sync_requested(&self) -> bool {
        self.sync == "requested"
    }
}

/// A lifecycle route under `/v1/cli/adrs/:adr_id/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleAction {
    Publish,
    Unpublish,
    Archive,
    Restore,
    Accept,
}

impl LifecycleAction {
    /// The route's last path segment.
    pub fn path_segment(self) -> &'static str {
        match self {
            Self::Publish => "publish",
            Self::Unpublish => "unpublish",
            Self::Archive => "archive",
            Self::Restore => "restore",
            Self::Accept => "accept",
        }
    }
}

/// An error body from the decision routes. The routes answer
/// `{error, message}`, a failed save check adds `errors`, and a body or query
/// the route's schema refuses gets the router's own shape instead, with the
/// issues under `bodyErrors`, `queryParameterErrors` or
/// `pathParameterErrors`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct AdrErrorBody {
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default, rename = "bodyErrors")]
    pub body_errors: Option<SchemaErrors>,
    #[serde(default, rename = "queryParameterErrors")]
    pub query_errors: Option<SchemaErrors>,
    #[serde(default, rename = "pathParameterErrors")]
    pub path_errors: Option<SchemaErrors>,
}

/// The issues one part of a refused request had.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct SchemaErrors {
    #[serde(default)]
    pub issues: Vec<SchemaIssue>,
}

/// One schema issue: the field's path and what was wrong with it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct SchemaIssue {
    #[serde(default)]
    pub path: Vec<serde_json::Value>,
    #[serde(default)]
    pub message: String,
}

impl SchemaIssue {
    /// `field.path: message`, or the message alone for an issue on the
    /// whole body.
    pub fn describe(&self) -> String {
        let path = self
            .path
            .iter()
            .map(|part| match part {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(".");
        if path.is_empty() {
            self.message.clone()
        } else {
            format!("{path}: {}", self.message)
        }
    }
}

impl AdrErrorBody {
    /// Every schema issue in the body, described one per entry, body issues
    /// first.
    pub fn schema_issues(&self) -> Vec<String> {
        [&self.body_errors, &self.query_errors, &self.path_errors]
            .into_iter()
            .flatten()
            .flat_map(|errors| errors.issues.iter().map(SchemaIssue::describe))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DECISION: &str = r#"{
        "id": "adadadad-0002-4000-8000-000000000002",
        "kind": "single",
        "repo_unique_id": "cccccccc-0000-4000-8000-00000000000a",
        "title": "Page every list read in the decisions API",
        "state": "draft",
        "status": "draft",
        "date": "2026-11-09T14:30:00.000Z",
        "context": "Large organizations hold thousands of decisions.",
        "policies": [
            {"id": "adadadad-0002-4000-8000-000000000002:rule-1", "level": "MUST", "statement": "Bound every list read with a limit."},
            {"id": "adadadad-0002-4000-8000-000000000002:rule-2", "level": "SHOULD_NOT", "statement": "Return more than 200 rows from one request."}
        ],
        "scope": {"root": ".", "paths": []},
        "category": {"id": "c1", "name": "API", "path": "api"},
        "base_adr_id": null
    }"#;

    #[test]
    fn decision_reads_the_route_shape() {
        let decision: Decision = serde_json::from_str(DECISION).unwrap();
        assert_eq!(decision.kind, "single");
        assert_eq!(decision.state, "draft");
        assert_eq!(decision.policies[1].level, RuleLevel::ShouldNot);
        assert_eq!(decision.scope.as_ref().unwrap().root, ".");
        assert_eq!(decision.category.as_ref().unwrap().name, "API");
        assert_eq!(decision.base_adr_id, None);
        let back = serde_json::to_value(&decision).unwrap();
        assert_eq!(back["policies"][1]["level"], "SHOULD_NOT");
    }

    #[test]
    fn decision_tolerates_missing_and_unknown_fields() {
        let decision: Decision =
            serde_json::from_str(r#"{"id": "d1", "title": "T", "state": "someday", "extra": 1}"#)
                .unwrap();
        assert_eq!(decision.state, "someday");
        assert!(decision.policies.is_empty());
        assert_eq!(decision.context, None);
        assert_eq!(decision.scope, None);
    }

    #[test]
    fn list_and_show_bodies_parse() {
        let list: DecisionList = serde_json::from_str(&format!(
            r#"{{"decisions": [{DECISION}], "total": 3, "next_offset": 1}}"#
        ))
        .unwrap();
        assert_eq!(list.total, 3);
        assert_eq!(list.next_offset, Some(1));
        let last: DecisionList =
            serde_json::from_str(r#"{"decisions": [], "total": 0, "next_offset": null}"#).unwrap();
        assert_eq!(last.next_offset, None);
        let show: DecisionShow =
            serde_json::from_str(&format!(r#"{{"decision": {DECISION}}}"#)).unwrap();
        assert_eq!(show.decision.policies.len(), 2);
    }

    #[test]
    fn create_request_serializes_status_lowercase() {
        let request = CreateDecisionRequest {
            org_id: "o".into(),
            repo_unique_id: "r".into(),
            status: SaveStatus::Draft,
            markdown: "# T".into(),
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["status"], "draft");
        assert_eq!(value.as_object().unwrap().len(), 4);
        let created: CreatedDecision = serde_json::from_str(
            r#"{"adr_id": "a", "status": "accepted", "title": "T", "repo_unique_id": "r"}"#,
        )
        .unwrap();
        assert_eq!(created.adr_id, "a");
    }

    #[test]
    fn update_request_sends_only_the_fields_set() {
        let empty = serde_json::to_value(UpdateDecisionRequest {
            org_id: "o".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(empty, serde_json::json!({"org_id": "o"}));

        let full = serde_json::to_value(UpdateDecisionRequest {
            org_id: "o".into(),
            title: Some("T".into()),
            context: Some("a\nb".into()),
            policies: Some(vec![
                PolicyEdit {
                    id: Some("d:rule-1".into()),
                    level: RuleLevel::MustNot,
                    statement: "s".into(),
                },
                PolicyEdit {
                    id: None,
                    level: RuleLevel::May,
                    statement: "t".into(),
                },
            ]),
            levels: Some(vec![LevelEdit {
                id: "d:rule-2".into(),
                level: RuleLevel::Should,
            }]),
            scope: Some(ScopeEdit {
                paths: vec!["src/**".into()],
            }),
        })
        .unwrap();
        assert_eq!(full["policies"][0]["level"], "MUST_NOT");
        assert!(full["policies"][1].get("id").is_none());
        assert_eq!(full["levels"][0]["level"], "SHOULD");
        assert_eq!(full["scope"]["paths"][0], "src/**");
    }

    #[test]
    fn action_body_reads_sync_and_reason() {
        let action: DecisionAction =
            serde_json::from_str(r#"{"success": true, "adr_id": "a", "sync": "requested"}"#)
                .unwrap();
        assert!(action.sync_requested());
        assert_eq!(action.reason_saved, None);
        assert!(serde_json::to_value(&action)
            .unwrap()
            .get("reason_saved")
            .is_none());
        let reject: DecisionAction = serde_json::from_str(
            r#"{"success": true, "adr_id": "a", "sync": "not_needed", "reason_saved": false}"#,
        )
        .unwrap();
        assert!(!reject.sync_requested());
        assert_eq!(reject.reason_saved, Some(false));
    }

    #[test]
    fn enum_spellings() {
        let states = [
            (DecisionState::Active, "active"),
            (DecisionState::Draft, "draft"),
            (DecisionState::Suggested, "suggested"),
            (DecisionState::Updated, "updated"),
            (DecisionState::Archived, "archived"),
        ];
        for (state, text) in states {
            assert_eq!(state.as_str(), text);
            assert_eq!(serde_json::to_value(state).unwrap(), text);
        }
        assert_eq!(SaveStatus::Accepted.as_str(), "accepted");
        assert_eq!(SaveStatus::Draft.as_str(), "draft");
        let segments = [
            (LifecycleAction::Publish, "publish"),
            (LifecycleAction::Unpublish, "unpublish"),
            (LifecycleAction::Archive, "archive"),
            (LifecycleAction::Restore, "restore"),
            (LifecycleAction::Accept, "accept"),
        ];
        for (action, segment) in segments {
            assert_eq!(action.path_segment(), segment);
        }
    }

    #[test]
    fn error_body_reads_both_shapes() {
        let check: AdrErrorBody = serde_json::from_str(
            r#"{"error":"bad_request","message":"Add a title. Add context.","errors":["Add a title.","Add context."]}"#,
        )
        .unwrap();
        assert_eq!(check.error.as_deref(), Some("bad_request"));
        assert_eq!(check.errors.len(), 2);
        assert!(check.schema_issues().is_empty());

        let schema: AdrErrorBody = serde_json::from_str(
            r#"{"pathParameterErrors":{"issues":[{"path":["adr_id"],"message":"Too long"}],"name":"ZodError"},
                "headerErrors":null,
                "queryParameterErrors":{"issues":[{"path":["limit"],"message":"Too big"}],"name":"ZodError"},
                "bodyErrors":{"issues":[
                    {"path":["policies",0,"statement"],"message":"Required"},
                    {"path":[],"message":"Name at least one field"}
                ],"name":"ZodError"}}"#,
        )
        .unwrap();
        assert_eq!(schema.error, None);
        assert_eq!(
            schema.schema_issues(),
            vec![
                "policies.0.statement: Required".to_string(),
                "Name at least one field".to_string(),
                "limit: Too big".to_string(),
                "adr_id: Too long".to_string(),
            ]
        );
    }
}
