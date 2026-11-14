//! The decision routes `actual adr` calls, the ones the web app calls under
//! `/v1/gcp/product/`: their request and response bodies, and the CLI's own
//! view of a decision that `list` and `show` print.
//!
//! Responses deserialize leniently: a field this build doesn't know is
//! ignored, and `status` and `kind` stay strings, so a value the server adds
//! later doesn't break a read. Requests serialize only the fields the routes
//! accept, because every route refuses unknown keys, and leave out a field
//! that has no value, because the routes refuse `null` too.

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
    /// The spelling `list` prints and `--state` takes.
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
    /// The CLI's spelling, also the status a local file's header carries.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Draft => "draft",
        }
    }
}

/// A change `actual adr` makes to a decision's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleAction {
    Publish,
    Unpublish,
    Archive,
    Restore,
    Accept,
    Reject,
}

// ── The CLI's view of a decision ───────────────────────────────────────────

/// One policy of a decision. `id` is `<decision row id>:<rule id>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionPolicy {
    pub id: String,
    pub level: RuleLevel,
    pub statement: String,
}

/// Where a decision applies: `root` `.` is the whole repository, and empty
/// `paths` the whole root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionScope {
    pub root: String,
    pub paths: Vec<String>,
}

/// A decision's category, as the routes read and write it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionCategory {
    pub id: String,
    pub name: String,
    pub path: String,
}

/// A decision as `list` and `show` print it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Decision {
    pub id: String,
    /// `single`, or `group` for an AI-grouped decision.
    pub kind: String,
    pub repo_unique_id: Option<String>,
    pub title: String,
    /// `active`, `draft`, `suggested`, `updated` or `archived`.
    pub state: String,
    /// The stored status, such as `accepted`, `draft` or `created-from-user`.
    pub status: String,
    pub date: String,
    pub context: Option<String>,
    pub policies: Vec<DecisionPolicy>,
    pub scope: Option<DecisionScope>,
    pub category: Option<DecisionCategory>,
    /// For an `updated` decision, the active decision it would change.
    pub base_adr_id: Option<String>,
}

/// One page of `actual adr list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionList {
    pub decisions: Vec<Decision>,
    /// Matching decisions across every page.
    pub total: u64,
    /// The offset of the next page, or `None` on the last one.
    pub next_offset: Option<u64>,
}

/// What a lifecycle command did, for `--json`. `reason_saved` comes only
/// from `reject`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionAction {
    pub success: bool,
    pub adr_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_saved: Option<bool>,
}

// ── The decision list read ─────────────────────────────────────────────────

/// `GET /v1/gcp/product/decisions`. Every decision but a pending AI
/// suggestion is in `adrs`; `status` says which list of the web app's it is
/// in.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct DecisionsResponse {
    #[serde(default)]
    pub adrs: Vec<WebDecision>,
    #[serde(default)]
    pub suggestions: Vec<WebDecision>,
}

/// A decision as the list read returns it. Every decision is a group: a
/// person's decision is a group of its own row, with `id` that row's id, and
/// an AI suggestion is a group of one row per policy, with `id` a candidate
/// id that names no row.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDecision {
    pub id: String,
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub title: Option<String>,
    /// One point per line.
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub category: Option<DecisionCategory>,
    #[serde(default)]
    pub base_adr_id: Option<String>,
    #[serde(default)]
    pub scope: Option<WebScope>,
    /// The repository directory the decision governs.
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub source_repo_unique_id: Option<String>,
    #[serde(default)]
    pub schema_version: Option<u8>,
    #[serde(default)]
    pub policy_rules: Vec<WebPolicyRule>,
    /// The statements alone, for a decision read without `policyRules`.
    #[serde(default)]
    pub policies: Vec<String>,
    #[serde(default)]
    pub candidate_group_id: Option<String>,
    /// The rows the decision holds.
    #[serde(default)]
    pub member_adr_ids: Vec<String>,
    #[serde(default)]
    pub kind: Option<String>,
    /// The first of `member_adr_ids`.
    #[serde(default)]
    pub primary_adr_id: Option<String>,
}

/// One policy of a [`WebDecision`]: `adr_id` is the row that holds it and
/// `id` its rule id in that row.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebPolicyRule {
    pub id: String,
    pub adr_id: String,
    pub level: RuleLevel,
    pub statement: String,
}

/// A decision's scope as the routes read and write it: the repository
/// directory it governs (`.` for all of it), and globs under that root, where
/// `!` excludes and none means the whole root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebScope {
    pub root: String,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub languages: Option<Vec<String>>,
}

// ── Writes ─────────────────────────────────────────────────────────────────

/// One write to a decision route: its name under `/v1/gcp/product/` and its
/// JSON body.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionWrite {
    pub route: &'static str,
    pub body: serde_json::Value,
}

/// `POST adr-create`, the web app's New Decision: the decision as fields, saved
/// as a draft when `status` is `draft` and as an active decision without it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CreateRequest {
    pub organization_id: String,
    pub repo_unique_id: String,
    pub adr: NewDecision,
}

/// The decision a create saves. `policy_levels` gives one level per policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewDecision {
    pub title: String,
    /// One point per line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    pub policies: Vec<String>,
    pub policy_levels: Vec<RuleLevel>,
    /// `draft`, or left out for an active decision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<&'static str>,
}

/// What a create saved: the new decision's row.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CreateResponse {
    pub data: CreatedRow,
}

/// The saved decision in a create's answer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CreatedRow {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
}

/// `POST adr-import`, the web app's Import: a decision's Context File
/// markdown, always saved as a draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportRequest {
    pub organization_id: String,
    pub repo_unique_id: String,
    pub markdown: String,
}

/// What an import saved.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ImportResponse {
    pub adr_id: String,
}

/// The decision fields an edit sends. The edit routes rebuild a decision's
/// stored markdown from the title and context they are sent, so an edit sends
/// the current ones with whatever it changes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<WebScope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<DecisionCategory>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u8>,
}

/// `POST adr-group-update`, the web app's edit of a person's decision: its
/// fields and its whole policy list, each policy tagged with the row it goes
/// in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroupUpdateRequest {
    pub organization_id: String,
    pub group_id: String,
    pub content: GroupContent,
}

/// The content of a [`GroupUpdateRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupContent {
    #[serde(flatten)]
    pub fields: DecisionFields,
    pub policy_rules: Vec<PolicyRuleEdit>,
}

/// One policy of a [`GroupContent`]. The route matches policies to the row's
/// old ones by position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRuleEdit {
    pub adr_id: String,
    pub statement: String,
    pub level: RuleLevel,
}

/// `POST adr-group-policies-save`, the web app's edit of an AI-grouped
/// decision: every policy it keeps, the ones it removes and the ones it adds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroupPoliciesSaveRequest {
    pub organization_id: String,
    pub group_id: String,
    pub input: GroupPoliciesInput,
}

/// The input of a [`GroupPoliciesSaveRequest`]. `edits` must name every policy
/// the decision keeps, exactly once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroupPoliciesInput {
    #[serde(flatten)]
    pub fields: DecisionFields,
    pub edits: Vec<GroupPolicyEdit>,
    pub removes: Vec<GroupPolicyRef>,
    pub adds: Vec<GroupPolicyAdd>,
}

/// A kept policy of an AI-grouped decision, by its row and rule id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupPolicyEdit {
    pub adr_id: String,
    pub policy_rule_id: String,
    pub statement: String,
    pub level: RuleLevel,
}

/// A removed policy of an AI-grouped decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupPolicyRef {
    pub adr_id: String,
    pub policy_rule_id: String,
}

/// A policy added to an AI-grouped decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroupPolicyAdd {
    pub statement: String,
    pub level: RuleLevel,
}

// ── Errors ─────────────────────────────────────────────────────────────────

/// An error body from the decision routes. The routes answer
/// `{error, message}`, and a failed save check puts every reason in
/// `message`. A body or query the route's schema refuses gets the router's
/// own shape instead, with the issues under `bodyErrors`,
/// `queryParameterErrors` or `pathParameterErrors`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct AdrErrorBody {
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
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

    /// A person's decision as the list read returns it.
    const SINGLE: &str = r#"{
        "id": "adadadad-0002-4000-8000-000000000002",
        "date": "2026-11-09T14:30:00.000Z",
        "title": "Page every list read in the decisions API",
        "context": "Large organizations hold thousands of decisions.",
        "policies": ["Bound every list read with a limit."],
        "status": "draft",
        "category": {"id": "c1", "name": "API", "path": "api"},
        "scope": {"root": ".", "paths": ["src/**"], "languages": ["rust"]},
        "root": ".",
        "rootStatus": "active",
        "sourceRepoUniqueId": "cccccccc-0000-4000-8000-00000000000a",
        "schemaVersion": 2,
        "policyRules": [
            {"id": "rule-1", "adrId": "adadadad-0002-4000-8000-000000000002", "level": "SHOULD_NOT", "statement": "Return more than 200 rows from one request."}
        ],
        "candidateGroupId": "adadadad-0002-4000-8000-000000000002",
        "memberAdrIds": ["adadadad-0002-4000-8000-000000000002"],
        "kind": "single",
        "primaryAdrId": "adadadad-0002-4000-8000-000000000002",
        "confidence": 0.5
    }"#;

    #[test]
    fn web_decision_reads_the_list_item() {
        let decision: WebDecision = serde_json::from_str(SINGLE).unwrap();
        assert_eq!(decision.status.as_deref(), Some("draft"));
        assert_eq!(decision.kind.as_deref(), Some("single"));
        assert_eq!(decision.schema_version, Some(2));
        assert_eq!(decision.policy_rules[0].adr_id, decision.id);
        assert_eq!(decision.policy_rules[0].level, RuleLevel::ShouldNot);
        let scope = decision.scope.as_ref().unwrap();
        assert_eq!(scope.languages.as_deref(), Some(&["rust".to_string()][..]));
        assert_eq!(
            decision.source_repo_unique_id.as_deref(),
            Some("cccccccc-0000-4000-8000-00000000000a")
        );
        assert_eq!(decision.member_adr_ids, std::slice::from_ref(&decision.id));
        assert_eq!(decision.category.unwrap().name, "API");
    }

    #[test]
    fn web_decision_tolerates_missing_and_unknown_fields() {
        let response: DecisionsResponse = serde_json::from_str(
            r#"{"adrs": [{"id": "d1", "status": "someday", "extra": 1}], "version": 3}"#,
        )
        .unwrap();
        let decision = &response.adrs[0];
        assert_eq!(decision.status.as_deref(), Some("someday"));
        assert!(decision.policy_rules.is_empty() && decision.member_adr_ids.is_empty());
        assert_eq!(
            (decision.title.as_ref(), decision.scope.as_ref()),
            (None, None)
        );
        assert!(response.suggestions.is_empty());
    }

    #[test]
    fn scope_writes_no_languages_it_has_not_got() {
        let scope = WebScope {
            root: ".".into(),
            paths: vec!["src/**".into()],
            languages: None,
        };
        assert_eq!(
            serde_json::to_value(&scope).unwrap(),
            serde_json::json!({"root": ".", "paths": ["src/**"]})
        );
    }

    #[test]
    fn create_request_spells_the_route_fields() {
        let mut request = CreateRequest {
            organization_id: "o".into(),
            repo_unique_id: "r".into(),
            adr: NewDecision {
                title: "T".into(),
                context: Some("a\nb".into()),
                policies: vec!["s".into(), "t".into()],
                policy_levels: vec![RuleLevel::MustNot, RuleLevel::May],
                status: Some("draft"),
            },
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({
                "organization_id": "o",
                "repo_unique_id": "r",
                "adr": {
                    "title": "T",
                    "context": "a\nb",
                    "policies": ["s", "t"],
                    "policyLevels": ["MUST_NOT", "MAY"],
                    "status": "draft"
                }
            })
        );
        request.adr.context = None;
        request.adr.status = None;
        let active = serde_json::to_value(&request).unwrap();
        assert!(active["adr"].get("context").is_none() && active["adr"].get("status").is_none());
        let created: CreateResponse =
            serde_json::from_str(r#"{"success": true, "data": {"id": "a", "date": "d"}}"#).unwrap();
        assert_eq!((created.data.id.as_str(), created.data.title), ("a", None));
    }

    #[test]
    fn edit_requests_flatten_the_decision_fields() {
        let fields = DecisionFields {
            title: Some("T".into()),
            context: None,
            scope: None,
            category: Some(DecisionCategory {
                id: "c".into(),
                name: "C".into(),
                path: "c".into(),
            }),
            schema_version: Some(2),
        };
        let update = GroupUpdateRequest {
            organization_id: "o".into(),
            group_id: "g".into(),
            content: GroupContent {
                fields: fields.clone(),
                policy_rules: vec![PolicyRuleEdit {
                    adr_id: "a".into(),
                    statement: "s".into(),
                    level: RuleLevel::Should,
                }],
            },
        };
        assert_eq!(
            serde_json::to_value(&update).unwrap(),
            serde_json::json!({
                "organization_id": "o",
                "group_id": "g",
                "content": {
                    "title": "T",
                    "category": {"id": "c", "name": "C", "path": "c"},
                    "schemaVersion": 2,
                    "policyRules": [{"adrId": "a", "statement": "s", "level": "SHOULD"}]
                }
            })
        );
        let save = GroupPoliciesSaveRequest {
            organization_id: "o".into(),
            group_id: "g".into(),
            input: GroupPoliciesInput {
                fields: DecisionFields::default(),
                edits: vec![GroupPolicyEdit {
                    adr_id: "a".into(),
                    policy_rule_id: "R-1".into(),
                    statement: "s".into(),
                    level: RuleLevel::Must,
                }],
                removes: vec![GroupPolicyRef {
                    adr_id: "b".into(),
                    policy_rule_id: "R-2".into(),
                }],
                adds: vec![GroupPolicyAdd {
                    statement: "t".into(),
                    level: RuleLevel::May,
                }],
            },
        };
        assert_eq!(
            serde_json::to_value(&save).unwrap()["input"],
            serde_json::json!({
                "edits": [{"adrId": "a", "policyRuleId": "R-1", "statement": "s", "level": "MUST"}],
                "removes": [{"adrId": "b", "policyRuleId": "R-2"}],
                "adds": [{"statement": "t", "level": "MAY"}]
            })
        );
    }

    #[test]
    fn import_bodies_and_actions_serialize() {
        let request = ImportRequest {
            organization_id: "o".into(),
            repo_unique_id: "r".into(),
            markdown: "# T".into(),
        };
        assert_eq!(
            serde_json::to_value(&request)
                .unwrap()
                .as_object()
                .unwrap()
                .len(),
            3
        );
        let imported: ImportResponse =
            serde_json::from_str(r#"{"success": true, "adr_id": "a"}"#).unwrap();
        assert_eq!(imported.adr_id, "a");
        let action = DecisionAction {
            success: true,
            adr_id: "a".into(),
            reason_saved: None,
        };
        assert!(serde_json::to_value(&action)
            .unwrap()
            .get("reason_saved")
            .is_none());
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
    }

    #[test]
    fn error_body_reads_both_shapes() {
        let check: AdrErrorBody = serde_json::from_str(
            r#"{"error":"bad_request","message":"Add a title. Add at least one policy."}"#,
        )
        .unwrap();
        assert_eq!(check.error.as_deref(), Some("bad_request"));
        assert!(check.schema_issues().is_empty());

        let schema: AdrErrorBody = serde_json::from_str(
            r#"{"pathParameterErrors":{"issues":[{"path":["adr_id"],"message":"Too long"}],"name":"ZodError"},
                "headerErrors":null,
                "queryParameterErrors":{"issues":[{"path":["include_summaries"],"message":"Invalid enum value"}],"name":"ZodError"},
                "bodyErrors":{"issues":[
                    {"path":["adr","policies",0],"message":"Required"},
                    {"path":[],"message":"Unrecognized key(s) in object: 'extra'"}
                ],"name":"ZodError"}}"#,
        )
        .unwrap();
        assert_eq!(schema.error, None);
        assert_eq!(
            schema.schema_issues(),
            vec![
                "adr.policies.0: Required".to_string(),
                "Unrecognized key(s) in object: 'extra'".to_string(),
                "include_summaries: Invalid enum value".to_string(),
                "adr_id: Too long".to_string(),
            ]
        );
    }
}
