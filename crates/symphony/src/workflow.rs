use crate::error::{Result, SymphonyError};
use crate::model::WorkflowDefinition;
use std::path::Path;

/// Load and parse a WORKFLOW.md file.
///
/// Format:
/// - If file starts with `---`, parse lines until next `---` as YAML front matter
/// - Remaining lines become the prompt body
/// - If front matter absent, treat entire file as prompt body with empty config
/// - YAML front matter must decode to a map; non-map YAML is an error
pub fn load_workflow(path: &Path) -> Result<WorkflowDefinition> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            SymphonyError::MissingWorkflowFile {
                path: path.display().to_string(),
            }
        } else {
            SymphonyError::WorkflowParseError {
                reason: format!("failed to read {}: {}", path.display(), e),
            }
        }
    })?;

    parse_workflow(&content)
}

/// Parse workflow content (YAML front matter + markdown body).
pub fn parse_workflow(content: &str) -> Result<WorkflowDefinition> {
    let (front_matter_str, prompt_body) = split_front_matter(content);

    let config = if let Some(yaml_str) = front_matter_str {
        let value: serde_yaml::Value =
            serde_yaml::from_str(&yaml_str).map_err(|e| SymphonyError::WorkflowParseError {
                reason: format!("invalid YAML front matter: {e}"),
            })?;

        // Must be a mapping
        if !value.is_mapping() && !value.is_null() {
            return Err(SymphonyError::WorkflowFrontMatterNotAMap);
        }

        if value.is_null() {
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
        } else {
            value
        }
    } else {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    };

    let prompt_template = prompt_body.trim().to_string();

    Ok(WorkflowDefinition {
        config,
        prompt_template,
    })
}

/// Split front matter from body.
/// Returns (Option<yaml_string>, body_string).
fn split_front_matter(content: &str) -> (Option<String>, String) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (None, content.to_string());
    }

    // Find the opening `---` line
    let lines: Vec<&str> = content.lines().collect();

    // Find the first line that is `---`
    let mut start_idx = None;
    for (i, line) in lines.iter().enumerate() {
        if line.trim() == "---" {
            start_idx = Some(i);
            break;
        }
    }

    let start_idx = match start_idx {
        Some(i) => i,
        None => return (None, content.to_string()),
    };

    // Find the closing `---`
    let mut end_idx = None;
    for (i, line) in lines.iter().enumerate().skip(start_idx + 1) {
        if line.trim() == "---" {
            end_idx = Some(i);
            break;
        }
    }

    let end_idx = match end_idx {
        Some(i) => i,
        None => {
            // No closing `---`, treat everything as prompt
            return (None, content.to_string());
        }
    };

    let yaml_lines = &lines[start_idx + 1..end_idx];
    let yaml_str = yaml_lines.join("\n");

    let body_lines = &lines[end_idx + 1..];
    let body_str = body_lines.join("\n");

    (Some(yaml_str), body_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty_workflow() {
        let wf = parse_workflow("").unwrap();
        assert!(wf.prompt_template.is_empty());
        assert!(wf.config.is_mapping());
    }

    #[test]
    fn test_parse_prompt_only() {
        let content = "You are a helpful assistant.\nDo the work.";
        let wf = parse_workflow(content).unwrap();
        assert_eq!(wf.prompt_template, content.trim());
        assert!(wf.config.is_mapping());
    }

    #[test]
    fn test_parse_front_matter_and_prompt() {
        let content = r#"---
tracker:
  kind: linear
  project_slug: my-project
polling:
  interval_ms: 5000
---

You are working on issue {{ issue.identifier }}: {{ issue.title }}.
"#;
        let wf = parse_workflow(content).unwrap();
        assert!(wf.config.is_mapping());
        let mapping = wf.config.as_mapping().unwrap();
        let tracker = mapping
            .get(serde_yaml::Value::String("tracker".to_string()))
            .unwrap();
        assert!(tracker.is_mapping());
        assert!(wf.prompt_template.contains("{{ issue.identifier }}"));
    }

    #[test]
    fn test_front_matter_non_map_error() {
        let content = "---\n- item1\n- item2\n---\nSome prompt";
        let result = parse_workflow(content);
        assert!(matches!(
            result,
            Err(SymphonyError::WorkflowFrontMatterNotAMap)
        ));
    }

    #[test]
    fn test_front_matter_empty_yaml() {
        let content = "---\n---\nSome prompt body";
        let wf = parse_workflow(content).unwrap();
        assert!(wf.config.is_mapping());
        assert_eq!(wf.prompt_template, "Some prompt body");
    }

    #[test]
    fn test_no_closing_front_matter() {
        let content = "---\ntracker:\n  kind: linear\nSome prompt body";
        let wf = parse_workflow(content).unwrap();
        // No closing ---, entire content is prompt
        assert!(wf.config.is_mapping());
        assert!(wf.prompt_template.contains("tracker:"));
    }
}
