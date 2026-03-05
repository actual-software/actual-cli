use crate::error::{Result, SymphonyError};
use crate::model::Issue;

/// Render a prompt template with issue context.
///
/// Uses Liquid-compatible template rendering with strict mode:
/// - Unknown variables fail rendering
/// - Unknown filters fail rendering
pub fn render_prompt(template_str: &str, issue: &Issue, attempt: Option<u32>) -> Result<String> {
    if template_str.is_empty() {
        return Ok("You are working on an issue from Linear.".to_string());
    }

    let parser = liquid::ParserBuilder::with_stdlib().build().map_err(|e| {
        SymphonyError::TemplateParseError {
            reason: e.to_string(),
        }
    })?;

    let template = parser
        .parse(template_str)
        .map_err(|e| SymphonyError::TemplateParseError {
            reason: e.to_string(),
        })?;

    let issue_obj = build_issue_object(issue);

    let mut globals = liquid::object!({
        "issue": issue_obj,
    });

    if let Some(att) = attempt {
        globals.insert("attempt".into(), liquid::model::Value::scalar(att as i64));
    }

    template
        .render(&globals)
        .map_err(|e| SymphonyError::TemplateRenderError {
            reason: e.to_string(),
        })
}

fn build_issue_object(issue: &Issue) -> liquid::Object {
    let mut obj = liquid::Object::new();

    obj.insert("id".into(), liquid::model::Value::scalar(issue.id.clone()));
    obj.insert(
        "identifier".into(),
        liquid::model::Value::scalar(issue.identifier.clone()),
    );
    obj.insert(
        "title".into(),
        liquid::model::Value::scalar(issue.title.clone()),
    );

    if let Some(ref desc) = issue.description {
        obj.insert(
            "description".into(),
            liquid::model::Value::scalar(desc.clone()),
        );
    } else {
        obj.insert("description".into(), liquid::model::Value::Nil);
    }

    if let Some(pri) = issue.priority {
        obj.insert("priority".into(), liquid::model::Value::scalar(pri as i64));
    } else {
        obj.insert("priority".into(), liquid::model::Value::Nil);
    }

    obj.insert(
        "state".into(),
        liquid::model::Value::scalar(issue.state.clone()),
    );

    if let Some(ref branch) = issue.branch_name {
        obj.insert(
            "branch_name".into(),
            liquid::model::Value::scalar(branch.clone()),
        );
    } else {
        obj.insert("branch_name".into(), liquid::model::Value::Nil);
    }

    if let Some(ref url) = issue.url {
        obj.insert("url".into(), liquid::model::Value::scalar(url.clone()));
    } else {
        obj.insert("url".into(), liquid::model::Value::Nil);
    }

    // Labels as array
    let labels: Vec<liquid::model::Value> = issue
        .labels
        .iter()
        .map(|l| liquid::model::Value::scalar(l.clone()))
        .collect();
    obj.insert("labels".into(), liquid::model::Value::Array(labels));

    // Blocked_by as array of objects
    let blockers: Vec<liquid::model::Value> = issue
        .blocked_by
        .iter()
        .map(|b| {
            let mut blocker_obj = liquid::Object::new();
            if let Some(ref id) = b.id {
                blocker_obj.insert("id".into(), liquid::model::Value::scalar(id.clone()));
            } else {
                blocker_obj.insert("id".into(), liquid::model::Value::Nil);
            }
            if let Some(ref ident) = b.identifier {
                blocker_obj.insert(
                    "identifier".into(),
                    liquid::model::Value::scalar(ident.clone()),
                );
            } else {
                blocker_obj.insert("identifier".into(), liquid::model::Value::Nil);
            }
            if let Some(ref state) = b.state {
                blocker_obj.insert("state".into(), liquid::model::Value::scalar(state.clone()));
            } else {
                blocker_obj.insert("state".into(), liquid::model::Value::Nil);
            }
            liquid::model::Value::Object(blocker_obj)
        })
        .collect();
    obj.insert("blocked_by".into(), liquid::model::Value::Array(blockers));

    if let Some(ref created_at) = issue.created_at {
        obj.insert(
            "created_at".into(),
            liquid::model::Value::scalar(created_at.to_rfc3339()),
        );
    } else {
        obj.insert("created_at".into(), liquid::model::Value::Nil);
    }

    if let Some(ref updated_at) = issue.updated_at {
        obj.insert(
            "updated_at".into(),
            liquid::model::Value::scalar(updated_at.to_rfc3339()),
        );
    } else {
        obj.insert("updated_at".into(), liquid::model::Value::Nil);
    }

    obj
}

/// Build a continuation prompt for subsequent turns on the same thread.
pub fn build_continuation_prompt(issue: &Issue, turn_number: u32, max_turns: u32) -> String {
    format!(
        "Continue working on {}: {}. This is turn {}/{} of your session. \
         Check the current state of your work and continue making progress. \
         If the task is complete, summarize what was done.",
        issue.identifier, issue.title, turn_number, max_turns
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BlockerRef;

    fn test_issue() -> Issue {
        Issue {
            id: "abc123".to_string(),
            identifier: "PROJ-42".to_string(),
            title: "Fix the login bug".to_string(),
            description: Some("Users can't log in".to_string()),
            priority: Some(1),
            state: "Todo".to_string(),
            branch_name: None,
            url: Some("https://linear.app/test/PROJ-42".to_string()),
            labels: vec!["bug".to_string(), "p1".to_string()],
            blocked_by: vec![],
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn test_render_empty_template() {
        let issue = test_issue();
        let result = render_prompt("", &issue, None).unwrap();
        assert_eq!(result, "You are working on an issue from Linear.");
    }

    #[test]
    fn test_render_simple_template() {
        let issue = test_issue();
        let template = "Work on {{ issue.identifier }}: {{ issue.title }}";
        let result = render_prompt(template, &issue, None).unwrap();
        assert_eq!(result, "Work on PROJ-42: Fix the login bug");
    }

    #[test]
    fn test_render_with_attempt() {
        let issue = test_issue();
        let template = "{% if attempt %}Retry #{{ attempt }}{% else %}First run{% endif %} for {{ issue.identifier }}";
        let result_first = render_prompt(template, &issue, None).unwrap();
        assert!(result_first.contains("First run"));

        let result_retry = render_prompt(template, &issue, Some(2)).unwrap();
        assert!(result_retry.contains("Retry #2"));
    }

    #[test]
    fn test_render_with_labels() {
        let issue = test_issue();
        let template = "Labels: {% for label in issue.labels %}{{ label }}{% unless forloop.last %}, {% endunless %}{% endfor %}";
        let result = render_prompt(template, &issue, None).unwrap();
        assert_eq!(result, "Labels: bug, p1");
    }

    #[test]
    fn test_render_with_description() {
        let issue = test_issue();
        let template = "Description: {{ issue.description }}";
        let result = render_prompt(template, &issue, None).unwrap();
        assert_eq!(result, "Description: Users can't log in");
    }

    #[test]
    fn test_render_with_nil_fields() {
        let mut issue = test_issue();
        issue.description = None;
        issue.branch_name = None;
        let template =
            "{{ issue.identifier }}: desc={{ issue.description }}, branch={{ issue.branch_name }}";
        let result = render_prompt(template, &issue, None).unwrap();
        assert_eq!(result, "PROJ-42: desc=, branch=");
    }

    #[test]
    fn test_render_with_blockers() {
        let mut issue = test_issue();
        issue.blocked_by = vec![BlockerRef {
            id: Some("def456".to_string()),
            identifier: Some("PROJ-10".to_string()),
            state: Some("In Progress".to_string()),
        }];
        let template = "Blockers: {% for b in issue.blocked_by %}{{ b.identifier }} ({{ b.state }}){% endfor %}";
        let result = render_prompt(template, &issue, None).unwrap();
        assert_eq!(result, "Blockers: PROJ-10 (In Progress)");
    }

    #[test]
    fn test_continuation_prompt() {
        let issue = test_issue();
        let prompt = build_continuation_prompt(&issue, 3, 10);
        assert!(prompt.contains("PROJ-42"));
        assert!(prompt.contains("turn 3/10"));
    }
}
