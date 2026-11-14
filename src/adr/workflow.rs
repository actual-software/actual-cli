//! The decision workflow the web app runs in its own code around the decision
//! routes, run here for `actual adr`: which of the web app's lists a decision
//! is in, which change its state allows, which route a change takes for a
//! person's decision or an AI-grouped one, and where Restore sends it. The
//! routes check little of this themselves, so a change is refused here before
//! anything is sent.
//!
//! A person's decision takes the single-row routes, which answer a wrong state
//! with a 409 of their own. An AI-grouped decision, whose id names no row,
//! takes the group routes, where the group's accept also publishes a draft.

use std::collections::HashSet;

use serde_json::json;

use crate::adr::context_file::js_trim;
use crate::api::adr_types::{
    Decision, DecisionFields, DecisionList, DecisionPolicy, DecisionScope, DecisionState,
    DecisionWrite, DecisionsResponse, GroupContent, GroupPoliciesInput, GroupPoliciesSaveRequest,
    GroupPolicyAdd, GroupPolicyEdit, GroupPolicyRef, GroupUpdateRequest, LifecycleAction,
    PolicyRuleEdit, WebDecision, WebPolicyRule, WebScope,
};
use crate::rules::types::RuleLevel;

/// A decision from the list read, with the web app's list it is in.
#[derive(Debug, Clone, PartialEq)]
pub struct Listed {
    pub decision: WebDecision,
    pub state: DecisionState,
}

/// Every decision of `response` that one of the web app's lists shows, those
/// read in `adrs` first. A decision no list shows, such as a deferred
/// suggestion, is left out, as the web app leaves it out.
pub fn listed(response: DecisionsResponse) -> Vec<Listed> {
    let from = |decisions: Vec<WebDecision>, suggestions: bool| {
        decisions.into_iter().filter_map(move |decision| {
            let state = state_of(decision.status.as_deref()?, suggestions)?;
            Some(Listed { decision, state })
        })
    };
    from(response.adrs, false)
        .chain(from(response.suggestions, true))
        .collect()
}

/// The list a decision read with `status` is in, from the web app's own
/// derivation; `in_suggestions` says it was read in `suggestions`.
fn state_of(status: &str, in_suggestions: bool) -> Option<DecisionState> {
    match (status, in_suggestions) {
        ("suggested", _) => Some(DecisionState::Suggested),
        ("updated", _) => Some(DecisionState::Updated),
        ("rejected", true) => Some(DecisionState::Archived),
        (_, true) => None,
        ("created-from-user" | "created-from-suggestion" | "accepted", _) => {
            Some(DecisionState::Active)
        }
        ("draft", _) => Some(DecisionState::Draft),
        ("deleted" | "archived-draft" | "rejected", _) => Some(DecisionState::Archived),
        _ => None,
    }
}

/// A policy's id as `show` prints it: its row and its rule id, so an edit
/// can name it whatever its position.
fn policy_id(rule: &WebPolicyRule) -> String {
    format!("{}:{}", rule.adr_id, rule.id)
}

/// The decision as `list` and `show` print it.
pub fn view(listed: &Listed) -> Decision {
    let decision = &listed.decision;
    let policies = if decision.policy_rules.is_empty() {
        let unleveled = |(index, statement): (usize, &String)| DecisionPolicy {
            id: format!("{}:unleveled-{index}", decision.id),
            level: RuleLevel::Must,
            statement: statement.clone(),
        };
        decision
            .policies
            .iter()
            .enumerate()
            .map(unleveled)
            .collect()
    } else {
        let leveled = |rule: &WebPolicyRule| DecisionPolicy {
            id: policy_id(rule),
            level: rule.level,
            statement: rule.statement.clone(),
        };
        decision.policy_rules.iter().map(leveled).collect()
    };
    Decision {
        id: decision.id.clone(),
        kind: decision
            .kind
            .clone()
            .unwrap_or_else(|| "single".to_string()),
        repo_unique_id: decision.source_repo_unique_id.clone(),
        title: decision
            .title
            .clone()
            .unwrap_or_else(|| "Untitled".to_string()),
        state: listed.state.as_str().to_string(),
        status: decision.status.clone().unwrap_or_default(),
        date: decision.date.clone(),
        context: decision.context.clone(),
        policies,
        scope: decision.scope.as_ref().map(|scope| DecisionScope {
            root: scope.root.clone(),
            paths: scope.paths.clone(),
        }),
        category: decision.category.clone(),
        base_adr_id: decision.base_adr_id.clone(),
    }
}

/// `limit` decisions from `offset`, with the total and the next page's
/// offset.
pub fn page(decisions: Vec<Decision>, limit: u32, offset: u64) -> DecisionList {
    let total = decisions.len() as u64;
    let shown: Vec<Decision> = decisions
        .into_iter()
        .skip(usize::try_from(offset).unwrap_or(usize::MAX))
        .take(limit as usize)
        .collect();
    let end = offset.saturating_add(shown.len() as u64);
    DecisionList {
        decisions: shown,
        total,
        next_offset: (end < total).then_some(end),
    }
}

/// The decision whose id is `id`, else the AI-grouped one holding a row with
/// that id.
pub fn find<'a>(rows: &'a [Listed], id: &str) -> Option<&'a Listed> {
    rows.iter().find(|row| row.decision.id == id).or_else(|| {
        rows.iter().find(|row| {
            row.decision
                .member_adr_ids
                .iter()
                .any(|member| member == id)
        })
    })
}

/// Whether the decision is AI-grouped: its id is a candidate id that names
/// none of its rows.
pub fn is_group(decision: &WebDecision) -> bool {
    decision.candidate_group_id.is_some() && !decision.member_adr_ids.contains(&decision.id)
}

/// The row a single-row route takes: the decision's first row, which is its
/// own id for a person's decision.
fn row_id(decision: &WebDecision) -> &str {
    decision.primary_adr_id.as_deref().unwrap_or(&decision.id)
}

/// Where Restore sends an archived decision: back to the list it was
/// archived from.
pub fn restore_target(decision: &WebDecision) -> &'static str {
    match decision.status.as_deref() {
        Some("archived-draft") => "draft",
        _ if decision.base_adr_id.is_some() => "updated",
        Some("rejected") => "suggested",
        _ => "accepted",
    }
}

/// The write `action` sends for the decision, or why its state doesn't allow
/// the change.
pub fn lifecycle_write(
    listed: &Listed,
    action: LifecycleAction,
    org_id: &str,
) -> Result<DecisionWrite, String> {
    use DecisionState::{Active, Archived, Draft, Suggested, Updated};
    let decision = &listed.decision;
    let state = listed.state;
    let group = is_group(decision);
    let is = state.as_str();
    let row = |route| DecisionWrite {
        route,
        body: json!({ "organization_id": org_id, "adr_id": row_id(decision) }),
    };
    let suggestion = |route| DecisionWrite {
        route,
        body: json!({ "organization_id": org_id, "suggestion_id": row_id(decision) }),
    };
    let group_action = |action: &str| DecisionWrite {
        route: "adr-group-action",
        body: json!({
            "organization_id": org_id,
            "source_candidate_id": decision.id,
            "action": action,
        }),
    };
    match action {
        LifecycleAction::Publish if state != Draft => Err(format!(
            "Only a draft can be published; this decision is {is}."
        )),
        LifecycleAction::Publish if group => Ok(group_action("accept")),
        LifecycleAction::Publish => Ok(publish_write(row_id(decision), org_id)),
        LifecycleAction::Unpublish if state != Active => Err(format!(
            "Only an active decision can move back to draft; this decision is {is}."
        )),
        LifecycleAction::Unpublish if group => Ok(group_action("move_to_draft")),
        LifecycleAction::Unpublish => Ok(row("adr-move-to-draft")),
        LifecycleAction::Archive if state == Archived => {
            Err("This decision is already archived.".to_string())
        }
        LifecycleAction::Archive if state != Active && state != Draft => Err(format!(
            "Only an active decision or a draft can be deleted; this decision is {is}. \
             `actual adr reject` turns down a suggestion."
        )),
        LifecycleAction::Archive if group => Ok(group_action("archive")),
        LifecycleAction::Archive => Ok(row("adr-archive")),
        LifecycleAction::Restore if state != Archived => Err(format!(
            "Only an archived decision can be restored; this decision is {is}."
        )),
        LifecycleAction::Restore if group => {
            Err("A grouped decision can't be restored yet.".to_string())
        }
        LifecycleAction::Restore => Ok(DecisionWrite {
            route: "adr-restore",
            body: json!({
                "organization_id": org_id,
                "adr_id": row_id(decision),
                "target_status": restore_target(decision),
            }),
        }),
        LifecycleAction::Accept | LifecycleAction::Reject
            if state != Suggested && state != Updated =>
        {
            let done = if action == LifecycleAction::Accept {
                "accepted"
            } else {
                "rejected"
            };
            Err(format!(
                "Only a suggestion can be {done}; this decision is {is}."
            ))
        }
        LifecycleAction::Accept if group => Ok(group_action("accept")),
        LifecycleAction::Accept => Ok(suggestion("adr-accept")),
        LifecycleAction::Reject if group => Ok(group_action("reject")),
        LifecycleAction::Reject => Ok(suggestion("adr-reject")),
    }
}

/// The write that publishes the draft whose row is `adr_id`.
pub fn publish_write(adr_id: &str, org_id: &str) -> DecisionWrite {
    DecisionWrite {
        route: "adr-publish",
        body: json!({ "organization_id": org_id, "adr_id": adr_id }),
    }
}

/// The write that saves why a decision was rejected. The route takes a row,
/// so an AI-grouped decision's reason goes on its first row.
pub fn rejection_feedback(decision: &WebDecision, org_id: &str, reason: &str) -> DecisionWrite {
    DecisionWrite {
        route: "adr-rejection-feedback",
        body: json!({
            "organization_id": org_id,
            "adr_id": row_id(decision),
            "reason": reason,
        }),
    }
}

/// One policy in `actual adr update --policy`'s whole list: an `id` from
/// `show` keeps that policy, and none adds one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyEdit {
    pub id: Option<String>,
    pub level: RuleLevel,
    pub statement: String,
}

/// A level change for one policy, by its id from `show`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelEdit {
    pub id: String,
    pub level: RuleLevel,
}

/// What `actual adr update` changes. `None` and an empty `levels` leave
/// that part as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecisionEdit {
    pub title: Option<String>,
    /// One point per line.
    pub context: Option<String>,
    /// The whole policy list after the edit.
    pub policies: Option<Vec<PolicyEdit>>,
    pub levels: Vec<LevelEdit>,
    /// The paths under the decision's root; empty for the whole root.
    pub scope: Option<Vec<String>>,
}

/// A policy after the edit, with the rule it keeps, if any.
struct NextPolicy<'a> {
    rule: Option<&'a WebPolicyRule>,
    statement: String,
    level: RuleLevel,
}

fn not_a_policy(id: &str) -> String {
    format!("Policy {id} is not a policy of this decision.")
}

/// The policy list `edit` leaves, from the decision's `rules`.
fn next_policies<'a>(
    rules: &'a [WebPolicyRule],
    edit: &DecisionEdit,
) -> Result<Vec<NextPolicy<'a>>, String> {
    let position = |id: &str| {
        rules
            .iter()
            .position(|rule| policy_id(rule) == id)
            .ok_or_else(|| not_a_policy(id))
    };
    let mut next: Vec<NextPolicy> = rules
        .iter()
        .map(|rule| NextPolicy {
            rule: Some(rule),
            statement: rule.statement.clone(),
            level: rule.level,
        })
        .collect();
    for change in &edit.levels {
        next[position(&change.id)?].level = change.level;
    }
    if let Some(policies) = &edit.policies {
        let mut seen = HashSet::new();
        next = Vec::new();
        for policy in policies {
            let rule = match &policy.id {
                None => None,
                Some(id) => {
                    let rule = &rules[position(id)?];
                    if !seen.insert(id.as_str()) {
                        return Err(format!("Policy {id} is listed twice."));
                    }
                    Some(rule)
                }
            };
            next.push(NextPolicy {
                rule,
                statement: js_trim(&policy.statement).to_string(),
                level: policy.level,
            });
        }
    }
    Ok(next)
}

/// The decision's fields after `edit`: the current title, context, scope,
/// category and schema version, with what the edit changes. The routes keep
/// the stored value of a field left out, so an edited text goes out even when
/// it trims to empty, which is how an empty `--context` clears the context; a
/// blank stored one is left out.
fn fields_after(decision: &WebDecision, edit: &DecisionEdit) -> DecisionFields {
    let text = |edited: &Option<String>, current: &Option<String>| match edited {
        Some(text) => Some(js_trim(text).to_string()),
        None => Some(js_trim(current.as_deref()?).to_string()).filter(|text| !text.is_empty()),
    };
    let scope = match &edit.scope {
        Some(paths) => Some(WebScope {
            root: decision
                .scope
                .as_ref()
                .map(|scope| scope.root.clone())
                .or_else(|| decision.root.clone())
                .unwrap_or_else(|| ".".to_string()),
            paths: paths.clone(),
            languages: decision.scope.as_ref().and_then(|s| s.languages.clone()),
        }),
        None => decision.scope.clone(),
    };
    DecisionFields {
        title: text(&edit.title, &decision.title),
        context: text(&edit.context, &decision.context),
        scope,
        category: decision.category.clone(),
        schema_version: decision.schema_version,
    }
}

/// The write `actual adr update` sends for `edit`, or why the decision can't
/// take it. A person's decision is saved whole through `adr-group-update`, as
/// the web app saves one; an AI-grouped decision keeps, removes and adds
/// policies through `adr-group-policies-save`.
pub fn edit_write(
    listed: &Listed,
    edit: &DecisionEdit,
    org_id: &str,
) -> Result<DecisionWrite, String> {
    let decision = &listed.decision;
    if listed.state == DecisionState::Archived {
        return Err(format!(
            "An archived decision can't be changed; `actual adr restore {}` brings it back first.",
            decision.id
        ));
    }
    let rules = &decision.policy_rules;
    if rules.is_empty() {
        return Err(
            "This decision has no per-policy levels; change it in the web app.".to_string(),
        );
    }
    let next = next_policies(rules, edit)?;
    let fields = fields_after(decision, edit);
    let (route, body) = if is_group(decision) {
        let kept: HashSet<String> = next
            .iter()
            .filter_map(|entry| entry.rule.map(policy_id))
            .collect();
        let edits = next
            .iter()
            .filter_map(|entry| {
                let rule = entry.rule?;
                Some(GroupPolicyEdit {
                    adr_id: rule.adr_id.clone(),
                    policy_rule_id: rule.id.clone(),
                    statement: entry.statement.clone(),
                    level: entry.level,
                })
            })
            .collect();
        let removes = rules
            .iter()
            .filter(|rule| !kept.contains(&policy_id(rule)))
            .map(|rule| GroupPolicyRef {
                adr_id: rule.adr_id.clone(),
                policy_rule_id: rule.id.clone(),
            })
            .collect();
        let adds = next
            .iter()
            .filter(|entry| entry.rule.is_none())
            .map(|entry| GroupPolicyAdd {
                statement: entry.statement.clone(),
                level: entry.level,
            })
            .collect();
        let request = GroupPoliciesSaveRequest {
            organization_id: org_id.to_string(),
            group_id: decision.id.clone(),
            input: GroupPoliciesInput {
                fields,
                edits,
                removes,
                adds,
            },
        };
        ("adr-group-policies-save", serde_json::to_value(request))
    } else {
        let member = row_id(decision);
        let policy_rules = next
            .iter()
            .map(|entry| PolicyRuleEdit {
                adr_id: entry.rule.map_or(member, |rule| &rule.adr_id).to_string(),
                statement: entry.statement.clone(),
                level: entry.level,
            })
            .collect();
        let request = GroupUpdateRequest {
            organization_id: org_id.to_string(),
            group_id: decision
                .candidate_group_id
                .clone()
                .unwrap_or_else(|| decision.id.clone()),
            content: GroupContent {
                fields,
                policy_rules,
            },
        };
        ("adr-group-update", serde_json::to_value(request))
    };
    Ok(DecisionWrite {
        route,
        body: body.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::adr_types::DecisionCategory;
    use serde_json::Value;

    const ORG: &str = "11111111-1111-4111-8111-111111111111";
    const ROW: &str = "33333333-3333-4333-8333-333333333333";
    const GROUP: &str = "99999999-9999-4999-8999-999999999999";
    const MEMBER_A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const MEMBER_B: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

    fn rule(adr_id: &str, id: &str, level: RuleLevel, statement: &str) -> WebPolicyRule {
        WebPolicyRule {
            id: id.to_string(),
            adr_id: adr_id.to_string(),
            level,
            statement: statement.to_string(),
        }
    }

    /// A person's decision: a group of its own row.
    fn single(status: &str) -> WebDecision {
        WebDecision {
            id: ROW.to_string(),
            date: "2026-11-02".to_string(),
            title: Some("Use Postgres".to_string()),
            context: Some("Why.".to_string()),
            status: Some(status.to_string()),
            category: Some(DecisionCategory {
                id: "c".to_string(),
                name: "Storage".to_string(),
                path: "storage".to_string(),
            }),
            scope: Some(WebScope {
                root: "backend".to_string(),
                paths: vec!["src/**".to_string()],
                languages: Some(vec!["rust".to_string()]),
            }),
            root: Some("backend".to_string()),
            source_repo_unique_id: Some("repo".to_string()),
            schema_version: Some(2),
            policy_rules: vec![
                rule(ROW, "r1", RuleLevel::Must, "Store data in Postgres."),
                rule(ROW, "r2", RuleLevel::Should, "Use one schema per service."),
            ],
            candidate_group_id: Some(ROW.to_string()),
            member_adr_ids: vec![ROW.to_string()],
            kind: Some("single".to_string()),
            primary_adr_id: Some(ROW.to_string()),
            ..WebDecision::default()
        }
    }

    /// An AI suggestion: one row per policy, under a candidate id.
    fn group(status: &str) -> WebDecision {
        WebDecision {
            id: GROUP.to_string(),
            title: Some("Page reads".to_string()),
            status: Some(status.to_string()),
            policy_rules: vec![
                rule(MEMBER_A, "R-1", RuleLevel::Must, "Page every read."),
                rule(MEMBER_B, "R-2", RuleLevel::May, "Cache a page."),
            ],
            candidate_group_id: Some(GROUP.to_string()),
            member_adr_ids: vec![MEMBER_A.to_string(), MEMBER_B.to_string()],
            kind: Some("group".to_string()),
            primary_adr_id: Some(MEMBER_A.to_string()),
            ..WebDecision::default()
        }
    }

    fn listed_as(decision: WebDecision, state: DecisionState) -> Listed {
        Listed { decision, state }
    }

    #[test]
    fn lists_match_the_web_app_and_drop_the_rest() {
        let adrs = [
            "created-from-user",
            "created-from-suggestion",
            "accepted",
            "draft",
            "suggested",
            "updated",
            "deleted",
            "archived-draft",
            "rejected",
            "deferred",
        ];
        let suggestions = ["suggested", "updated", "rejected", "draft", "accepted"];
        let item = |status: &str| WebDecision {
            id: status.to_string(),
            status: Some(status.to_string()),
            ..WebDecision::default()
        };
        let mut response = DecisionsResponse {
            adrs: adrs.iter().map(|s| item(s)).collect(),
            suggestions: suggestions.iter().map(|s| item(s)).collect(),
        };
        response.adrs.push(WebDecision::default());
        let states: Vec<(String, &str)> = listed(response)
            .into_iter()
            .map(|row| (row.decision.id, row.state.as_str()))
            .collect();
        let expected = [
            ("created-from-user", "active"),
            ("created-from-suggestion", "active"),
            ("accepted", "active"),
            ("draft", "draft"),
            ("suggested", "suggested"),
            ("updated", "updated"),
            ("deleted", "archived"),
            ("archived-draft", "archived"),
            ("rejected", "archived"),
            ("suggested", "suggested"),
            ("updated", "updated"),
            ("rejected", "archived"),
        ];
        let expected: Vec<(String, &str)> = expected
            .iter()
            .map(|(id, state)| (id.to_string(), *state))
            .collect();
        assert_eq!(states, expected);
    }

    #[test]
    fn view_prints_policy_ids_by_row_and_rule() {
        let decision = view(&listed_as(single("accepted"), DecisionState::Active));
        assert_eq!(decision.kind, "single");
        assert_eq!(decision.state, "active");
        assert_eq!(decision.status, "accepted");
        assert_eq!(decision.repo_unique_id.as_deref(), Some("repo"));
        assert_eq!(decision.policies[1].id, format!("{ROW}:r2"));
        assert_eq!(decision.policies[1].level, RuleLevel::Should);
        assert_eq!(decision.scope.unwrap().paths, ["src/**"]);

        let grouped = view(&listed_as(group("suggested"), DecisionState::Suggested));
        assert_eq!(grouped.kind, "group");
        assert_eq!(grouped.policies[1].id, format!("{MEMBER_B}:R-2"));

        let bare = view(&listed_as(
            WebDecision {
                id: "d".to_string(),
                status: Some("draft".to_string()),
                policies: vec!["Plain.".to_string()],
                ..WebDecision::default()
            },
            DecisionState::Draft,
        ));
        assert_eq!(
            (bare.kind.as_str(), bare.title.as_str()),
            ("single", "Untitled")
        );
        assert_eq!(bare.policies[0].id, "d:unleveled-0");
        assert_eq!(bare.policies[0].level, RuleLevel::Must);
        assert_eq!(bare.scope, None);
    }

    #[test]
    fn page_slices_and_points_at_the_next_page() {
        let decisions: Vec<Decision> = (0..5)
            .map(|n| {
                let mut decision = single("accepted");
                decision.id = n.to_string();
                view(&listed_as(decision, DecisionState::Active))
            })
            .collect();
        let first = page(decisions.clone(), 2, 0);
        let ids: Vec<&str> = first.decisions.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(
            (ids, first.total, first.next_offset),
            (vec!["0", "1"], 5, Some(2))
        );
        let last = page(decisions.clone(), 2, 4);
        assert_eq!((last.decisions.len(), last.next_offset), (1, None));
        let past = page(decisions, 2, u64::MAX);
        assert_eq!(
            (past.decisions.len(), past.total, past.next_offset),
            (0, 5, None)
        );
    }

    #[test]
    fn find_takes_the_decision_id_then_a_row_id() {
        let rows = [
            listed_as(single("accepted"), DecisionState::Active),
            listed_as(group("suggested"), DecisionState::Suggested),
        ];
        assert_eq!(find(&rows, ROW).unwrap().decision.id, ROW);
        assert_eq!(find(&rows, GROUP).unwrap().decision.id, GROUP);
        assert_eq!(find(&rows, MEMBER_B).unwrap().decision.id, GROUP);
        assert!(find(&rows, "nothing").is_none());
        assert!(!is_group(&single("draft")) && is_group(&group("draft")));
        let mut bare = single("draft");
        bare.candidate_group_id = None;
        bare.member_adr_ids.clear();
        assert!(!is_group(&bare));
    }

    #[test]
    fn restore_goes_back_to_the_list_it_left() {
        let target = |status: &str, base: Option<&str>| {
            let mut decision = single(status);
            decision.base_adr_id = base.map(str::to_string);
            restore_target(&decision)
        };
        assert_eq!(target("archived-draft", Some("b")), "draft");
        assert_eq!(target("rejected", Some("b")), "updated");
        assert_eq!(target("rejected", None), "suggested");
        assert_eq!(target("deleted", None), "accepted");
    }

    fn write(
        decision: WebDecision,
        state: DecisionState,
        action: LifecycleAction,
    ) -> DecisionWrite {
        lifecycle_write(&listed_as(decision, state), action, ORG).unwrap()
    }

    fn refusal(decision: WebDecision, state: DecisionState, action: LifecycleAction) -> String {
        lifecycle_write(&listed_as(decision, state), action, ORG).unwrap_err()
    }

    #[test]
    fn a_persons_decision_takes_the_row_routes() {
        use DecisionState::*;
        use LifecycleAction::*;
        let row_body = json!({"organization_id": ORG, "adr_id": ROW});
        let suggestion_body = json!({"organization_id": ORG, "suggestion_id": ROW});
        let cases = [
            (Draft, Publish, "adr-publish", row_body.clone()),
            (Active, Unpublish, "adr-move-to-draft", row_body.clone()),
            (Active, Archive, "adr-archive", row_body.clone()),
            (Draft, Archive, "adr-archive", row_body),
            (Suggested, Accept, "adr-accept", suggestion_body.clone()),
            (Updated, Reject, "adr-reject", suggestion_body),
        ];
        for (state, action, route, body) in cases {
            let sent = write(single("x"), state, action);
            assert_eq!(
                (sent.route, sent.body),
                (route, body),
                "{action:?} from {state:?}"
            );
        }
        let restore = write(single("archived-draft"), Archived, Restore);
        assert_eq!(restore.route, "adr-restore");
        assert_eq!(
            restore.body,
            json!({"organization_id": ORG, "adr_id": ROW, "target_status": "draft"})
        );
    }

    #[test]
    fn an_ai_group_takes_the_group_action() {
        use DecisionState::*;
        use LifecycleAction::*;
        for (state, action, name) in [
            (Draft, Publish, "accept"),
            (Active, Unpublish, "move_to_draft"),
            (Draft, Archive, "archive"),
            (Updated, Accept, "accept"),
            (Suggested, Reject, "reject"),
        ] {
            let sent = write(group("x"), state, action);
            assert_eq!(sent.route, "adr-group-action");
            assert_eq!(
                sent.body,
                json!({"organization_id": ORG, "source_candidate_id": GROUP, "action": name})
            );
        }
        assert_eq!(
            refusal(group("deleted"), Archived, Restore),
            "A grouped decision can't be restored yet."
        );
    }

    #[test]
    fn a_change_the_state_does_not_allow_is_refused() {
        use DecisionState::*;
        use LifecycleAction::*;
        let cases = [
            (
                Active,
                Publish,
                "Only a draft can be published; this decision is active.",
            ),
            (
                Draft,
                Unpublish,
                "Only an active decision can move back to draft; this decision is draft.",
            ),
            (Archived, Archive, "This decision is already archived."),
            (
                Suggested,
                Archive,
                "Only an active decision or a draft can be deleted; this decision is suggested. \
                 `actual adr reject` turns down a suggestion.",
            ),
            (
                Active,
                Restore,
                "Only an archived decision can be restored; this decision is active.",
            ),
            (
                Draft,
                Accept,
                "Only a suggestion can be accepted; this decision is draft.",
            ),
            (
                Archived,
                Reject,
                "Only a suggestion can be rejected; this decision is archived.",
            ),
        ];
        for (state, action, message) in cases {
            assert_eq!(refusal(single("x"), state, action), message);
        }
    }

    #[test]
    fn a_reason_goes_on_the_first_row() {
        let feedback = rejection_feedback(&group("rejected"), ORG, "Not ours.");
        assert_eq!(feedback.route, "adr-rejection-feedback");
        assert_eq!(
            feedback.body,
            json!({"organization_id": ORG, "adr_id": MEMBER_A, "reason": "Not ours."})
        );
        let mut no_rows = single("rejected");
        no_rows.primary_adr_id = None;
        assert_eq!(rejection_feedback(&no_rows, ORG, "r").body["adr_id"], ROW);
    }

    fn edit_of(decision: WebDecision, edit: &DecisionEdit) -> DecisionWrite {
        edit_write(&listed_as(decision, DecisionState::Active), edit, ORG).unwrap()
    }

    #[test]
    fn a_level_edit_sends_the_whole_decision() {
        let edit = DecisionEdit {
            levels: vec![LevelEdit {
                id: format!("{ROW}:r2"),
                level: RuleLevel::MustNot,
            }],
            ..DecisionEdit::default()
        };
        let sent = edit_of(single("accepted"), &edit);
        assert_eq!(sent.route, "adr-group-update");
        assert_eq!(
            sent.body,
            json!({
                "organization_id": ORG,
                "group_id": ROW,
                "content": {
                    "title": "Use Postgres",
                    "context": "Why.",
                    "scope": {"root": "backend", "paths": ["src/**"], "languages": ["rust"]},
                    "category": {"id": "c", "name": "Storage", "path": "storage"},
                    "schemaVersion": 2,
                    "policyRules": [
                        {"adrId": ROW, "statement": "Store data in Postgres.", "level": "MUST"},
                        {"adrId": ROW, "statement": "Use one schema per service.", "level": "MUST_NOT"}
                    ]
                }
            })
        );
    }

    #[test]
    fn a_policy_list_edit_keeps_adds_and_drops_by_id() {
        let edit = DecisionEdit {
            title: Some("  Use Postgres everywhere ".to_string()),
            context: Some("One.\nTwo.".to_string()),
            policies: Some(vec![
                PolicyEdit {
                    id: Some(format!("{ROW}:r2")),
                    level: RuleLevel::May,
                    statement: " Use schemas. ".to_string(),
                },
                PolicyEdit {
                    id: None,
                    level: RuleLevel::Should,
                    statement: "Back up nightly.".to_string(),
                },
            ]),
            scope: Some(Vec::new()),
            ..DecisionEdit::default()
        };
        let sent = edit_of(single("accepted"), &edit);
        let content = &sent.body["content"];
        assert_eq!(content["title"], "Use Postgres everywhere");
        assert_eq!(content["context"], "One.\nTwo.");
        assert_eq!(
            content["scope"],
            json!({"root": "backend", "paths": [], "languages": ["rust"]})
        );
        assert_eq!(
            content["policyRules"],
            json!([
                {"adrId": ROW, "statement": "Use schemas.", "level": "MAY"},
                {"adrId": ROW, "statement": "Back up nightly.", "level": "SHOULD"}
            ])
        );
    }

    #[test]
    fn an_edit_without_stored_fields_leaves_them_out() {
        let mut bare = single("draft");
        bare.title = Some("   ".to_string());
        bare.context = None;
        bare.scope = None;
        bare.category = None;
        bare.schema_version = None;
        bare.candidate_group_id = None;
        let sent = edit_of(
            bare.clone(),
            &DecisionEdit {
                scope: Some(vec!["src/**".to_string()]),
                ..DecisionEdit::default()
            },
        );
        let content = sent.body["content"].as_object().unwrap();
        let keys: Vec<&String> = content.keys().collect();
        assert_eq!(keys, ["policyRules", "scope"]);
        assert_eq!(
            content["scope"],
            json!({"root": "backend", "paths": ["src/**"]})
        );
        assert_eq!(sent.body["group_id"], ROW);

        bare.root = None;
        let rootless = edit_of(
            bare,
            &DecisionEdit {
                scope: Some(Vec::new()),
                ..DecisionEdit::default()
            },
        );
        assert_eq!(
            rootless.body["content"]["scope"],
            json!({"root": ".", "paths": []})
        );
    }

    #[test]
    fn an_edited_context_goes_out_even_when_empty() {
        let sent = edit_of(
            single("accepted"),
            &DecisionEdit {
                context: Some("  ".to_string()),
                ..DecisionEdit::default()
            },
        );
        assert_eq!(sent.body["content"]["context"], "");
        assert_eq!(
            sent.body["content"]["title"],
            single("accepted").title.unwrap()
        );
    }

    #[test]
    fn an_ai_group_edit_keeps_removes_and_adds() {
        let edit = DecisionEdit {
            policies: Some(vec![
                PolicyEdit {
                    id: Some(format!("{MEMBER_B}:R-2")),
                    level: RuleLevel::Should,
                    statement: "Cache a page for a minute.".to_string(),
                },
                PolicyEdit {
                    id: None,
                    level: RuleLevel::MustNot,
                    statement: "Read every row.".to_string(),
                },
            ]),
            ..DecisionEdit::default()
        };
        let sent = edit_of(group("suggested"), &edit);
        assert_eq!(sent.route, "adr-group-policies-save");
        assert_eq!(sent.body["group_id"], GROUP);
        assert_eq!(
            sent.body["input"],
            json!({
                "title": "Page reads",
                "edits": [{"adrId": MEMBER_B, "policyRuleId": "R-2", "statement": "Cache a page for a minute.", "level": "SHOULD"}],
                "removes": [{"adrId": MEMBER_A, "policyRuleId": "R-1"}],
                "adds": [{"statement": "Read every row.", "level": "MUST_NOT"}]
            })
        );
    }

    #[test]
    fn an_edit_the_decision_cannot_take_is_refused() {
        let refused = |decision: WebDecision, state: DecisionState, edit: DecisionEdit| {
            edit_write(&listed_as(decision, state), &edit, ORG).unwrap_err()
        };
        let title = DecisionEdit {
            title: Some("T".to_string()),
            ..DecisionEdit::default()
        };
        assert_eq!(
            refused(single("deleted"), DecisionState::Archived, title.clone()),
            format!(
                "An archived decision can't be changed; `actual adr restore {ROW}` brings it back first."
            )
        );
        let mut unleveled = single("draft");
        unleveled.policy_rules.clear();
        assert_eq!(
            refused(unleveled, DecisionState::Draft, title),
            "This decision has no per-policy levels; change it in the web app."
        );
        let level = |id: &str| DecisionEdit {
            levels: vec![LevelEdit {
                id: id.to_string(),
                level: RuleLevel::May,
            }],
            ..DecisionEdit::default()
        };
        assert_eq!(
            refused(single("draft"), DecisionState::Draft, level("x:r1")),
            "Policy x:r1 is not a policy of this decision."
        );
        let keep = |id: &str| PolicyEdit {
            id: Some(id.to_string()),
            level: RuleLevel::Must,
            statement: "s".to_string(),
        };
        let twice = DecisionEdit {
            policies: Some(vec![keep(&format!("{ROW}:r1")), keep(&format!("{ROW}:r1"))]),
            ..DecisionEdit::default()
        };
        assert_eq!(
            refused(single("draft"), DecisionState::Draft, twice),
            format!("Policy {ROW}:r1 is listed twice.")
        );
        let unknown = DecisionEdit {
            policies: Some(vec![keep("y:r9")]),
            ..DecisionEdit::default()
        };
        assert_eq!(
            refused(single("draft"), DecisionState::Draft, unknown),
            "Policy y:r9 is not a policy of this decision."
        );
        let edited: Value = edit_of(group("suggested"), &level(&format!("{MEMBER_A}:R-1"))).body;
        assert_eq!(edited["input"]["edits"][0]["level"], "MAY");
        assert_eq!(edited["input"]["removes"], json!([]));
    }
}
