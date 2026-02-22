use std::collections::HashMap;
use std::path::Path;

use crate::api::types::MatchRequest;
use crate::error::ActualError;
use crate::generation::markers;
use crate::generation::OutputFormat;
use crate::tailoring::types::{FileOutput, TailoringOutput, TailoringSummary};

/// Strip non-printable ASCII control characters from server-controlled ADR content.
///
/// Keeps newlines (`\n`), carriage returns (`\r`), and tabs (`\t`) as they are
/// legitimate in markdown, but removes other control characters (bytes < 0x20
/// and DEL 0x7F) that could be used for terminal manipulation.
pub(super) fn strip_control_chars(s: &str) -> String {
    s.chars()
        .filter(|&c| c >= ' ' || c == '\n' || c == '\r' || c == '\t')
        .filter(|&c| c != '\x7f')
        .collect()
}

/// Validate a server-controlled project path before using it to construct output file paths.
///
/// Rejects paths that:
/// - Contain `..` components (path traversal)
/// - Start with `/` or `\` (absolute paths)
/// - Contain null bytes
pub(super) fn validate_project_path(p: &str) -> Result<(), ActualError> {
    if p.contains('\0') {
        return Err(ActualError::InternalError(format!(
            "project path '{p}' contains null bytes"
        )));
    }
    if p.starts_with('/') || p.starts_with('\\') {
        return Err(ActualError::InternalError(format!(
            "project path '{p}' must be a relative path"
        )));
    }
    if std::path::Path::new(p)
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        return Err(ActualError::InternalError(format!(
            "project path '{p}' contains path traversal components"
        )));
    }
    Ok(())
}

/// Convert raw ADRs into a [`TailoringOutput`] without Claude tailoring.
///
/// Groups ADRs by their `matched_projects` field. Each group produces a
/// `FileOutput` whose content is the ADR policies/instructions formatted as
/// markdown. ADRs with no `matched_projects` are assigned to the root output
/// file (e.g. `CLAUDE.md` or `AGENTS.md` depending on `format`).
pub(super) fn raw_adrs_to_output(
    adrs: &[crate::api::types::Adr],
    format: &OutputFormat,
) -> TailoringOutput {
    let filename = format.filename();
    let mut project_adrs: HashMap<String, Vec<&crate::api::types::Adr>> = HashMap::new();

    for adr in adrs {
        if adr.matched_projects.is_empty() {
            project_adrs
                .entry(filename.to_string())
                .or_default()
                .push(adr);
        } else {
            for project_path in &adr.matched_projects {
                if validate_project_path(project_path).is_err() {
                    tracing::warn!(
                        "skipping invalid project path '{}' for ADR '{}'",
                        console::strip_ansi_codes(project_path),
                        adr.id
                    );
                    continue;
                }
                let file_path = if project_path == "." {
                    filename.to_string()
                } else {
                    format!("{project_path}/{filename}")
                };
                project_adrs.entry(file_path).or_default().push(adr);
            }
        }
    }

    let mut files: Vec<FileOutput> = project_adrs
        .into_iter()
        .map(|(path, adrs_for_file)| {
            use crate::tailoring::types::AdrSection;

            let sections: Vec<AdrSection> = adrs_for_file
                .iter()
                .map(|adr| {
                    let safe_title = strip_control_chars(&adr.title);
                    let mut content = format!("## {safe_title}\n");
                    for policy in &adr.policies {
                        let safe_policy = strip_control_chars(policy);
                        content.push_str(&format!("- {safe_policy}\n"));
                    }
                    if let Some(instructions) = &adr.instructions {
                        if !instructions.is_empty() {
                            content.push('\n');
                            for instruction in instructions {
                                let safe_instruction = strip_control_chars(instruction);
                                content.push_str(&format!("- {safe_instruction}\n"));
                            }
                        }
                    }
                    AdrSection {
                        adr_id: adr.id.clone(),
                        content,
                    }
                })
                .collect();

            FileOutput {
                path,
                sections,
                reasoning: "Raw ADR output (--no-tailor mode)".to_string(),
            }
        })
        .collect();

    // Sort for deterministic output
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let applicable = adrs.len();
    let files_generated = files.len();

    TailoringOutput {
        files,
        skipped_adrs: vec![],
        summary: TailoringSummary {
            total_input: applicable,
            applicable,
            not_applicable: 0,
            files_generated,
        },
    }
}

/// Scan the repository for existing output files (CLAUDE.md or AGENTS.md depending on format)
/// and return their contents concatenated as a string, for use as context during tailoring.
pub(super) fn find_existing_output_files(root_dir: &Path, format: &OutputFormat) -> String {
    let files = super::super::find_output_files(root_dir, format);
    // `find_output_files` returns paths in sorted order, and `filter_map`
    // preserves that order, so no additional sort is needed here.
    let results: Vec<String> = files
        .iter()
        .filter_map(|path| {
            let content = std::fs::read_to_string(path).ok()?;
            let rel = path
                .strip_prefix(root_dir)
                .unwrap_or(path)
                .to_string_lossy();
            let cleaned = markers::strip_managed_metadata(&content);
            Some(format!("=== {rel} ===\n{cleaned}"))
        })
        .collect();
    results.join("\n\n")
}

/// Produce contextual follow-up lines for a zero-ADR fetch result.
///
/// Examines the languages and frameworks in the match request and returns one
/// or more indented lines explaining *why* no ADRs were matched:
///
/// - No languages detected → indicates the repo language is not recognized.
/// - Languages but no frameworks → only general ADRs were eligible.
/// - Languages + frameworks → the combination has no ADR coverage yet.
pub(super) fn zero_adr_context_lines(request: &MatchRequest) -> Vec<String> {
    let mut langs: Vec<&str> = request
        .projects
        .iter()
        .flat_map(|p| p.languages.iter().map(|l| l.as_str()))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    langs.sort_unstable();

    let mut fws: Vec<&str> = request
        .projects
        .iter()
        .flat_map(|p| p.frameworks.iter().map(|f| f.name.as_str()))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    fws.sort_unstable();

    if langs.is_empty() {
        vec!["  No supported languages detected \u{2014} unable to match ADRs".to_string()]
    } else if fws.is_empty() {
        vec![
            format!("  Languages: {}", langs.join(", ")),
            "  No frameworks detected; only general ADRs were eligible".to_string(),
        ]
    } else {
        vec![
            format!("  Languages: {}", langs.join(", ")),
            format!("  Frameworks: {}", fws.join(", ")),
            "  No ADRs available for this stack yet".to_string(),
        ]
    }
}

/// Produce a user-friendly error message for an ADR fetch failure.
///
/// Distinguishes connection failures, timeouts, structured API errors (4xx/5xx),
/// and falls back to a raw message for unrecognized errors.
pub(super) fn fetch_error_message(e: &ActualError, api_url: &str) -> String {
    match e {
        ActualError::ApiError(s)
            if s.contains("error trying to connect")
                || s.contains("Connection refused")
                || s.contains("connection refused")
                || s.contains("dns error") =>
        {
            format!(
                "Unable to connect to Actual AI ({api_url}) \u{2014} check your network connection"
            )
        }
        ActualError::ApiError(s)
            if s.contains("timed out") || s.contains("operation timed out") =>
        {
            "Connection to Actual AI timed out \u{2014} try again later".to_string()
        }
        ActualError::ApiResponseError { code, message } => {
            format!("Actual AI returned an error ({code}): {message}")
        }
        _ => format!("API request failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::OutputFormat;

    // ── validate_project_path tests ──

    #[test]
    fn test_validate_project_path_valid() {
        assert!(validate_project_path("apps/web").is_ok());
        assert!(validate_project_path(".").is_ok());
        assert!(validate_project_path("services/api/v2").is_ok());
    }

    #[test]
    fn test_validate_project_path_rejects_path_traversal() {
        let result = validate_project_path("../secret");
        assert!(result.is_err(), "expected error for path traversal");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("path traversal"),
            "expected 'path traversal' in: {msg}"
        );
    }

    #[test]
    fn test_validate_project_path_rejects_absolute_slash() {
        let result = validate_project_path("/etc/passwd");
        assert!(result.is_err(), "expected error for absolute path");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("relative path"),
            "expected 'relative path' in: {msg}"
        );
    }

    #[test]
    fn test_validate_project_path_rejects_absolute_backslash() {
        let result = validate_project_path("\\windows\\system32");
        assert!(
            result.is_err(),
            "expected error for backslash absolute path"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("relative path"),
            "expected 'relative path' in: {msg}"
        );
    }

    #[test]
    fn test_validate_project_path_rejects_null_bytes() {
        let result = validate_project_path("apps\0evil");
        assert!(result.is_err(), "expected error for null bytes");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("null bytes"),
            "expected 'null bytes' in: {msg}"
        );
    }

    // ── strip_control_chars tests ──

    #[test]
    fn test_strip_control_chars_keeps_printable() {
        let input = "Hello, World! 123 abc ABC";
        assert_eq!(strip_control_chars(input), input);
    }

    #[test]
    fn test_strip_control_chars_keeps_newline_tab_cr() {
        let input = "line1\nline2\r\n\ttabbed";
        assert_eq!(strip_control_chars(input), input);
    }

    #[test]
    fn test_strip_control_chars_removes_control_chars() {
        // ESC (0x1B), BEL (0x07), SOH (0x01)
        let input = "hello\x1b[31mworld\x07\x01end";
        let result = strip_control_chars(input);
        assert!(!result.contains('\x1b'), "should strip ESC");
        assert!(!result.contains('\x07'), "should strip BEL");
        assert!(!result.contains('\x01'), "should strip SOH");
        assert!(result.contains("hello"), "should keep 'hello'");
        assert!(result.contains("world"), "should keep 'world'");
        assert!(result.contains("end"), "should keep 'end'");
    }

    #[test]
    fn test_strip_control_chars_removes_del() {
        let input = "abc\x7fdef";
        let result = strip_control_chars(input);
        assert!(!result.contains('\x7f'), "should strip DEL");
        assert_eq!(result, "abcdef");
    }

    #[test]
    fn test_strip_control_chars_empty_string() {
        assert_eq!(strip_control_chars(""), "");
    }

    // ── raw_adrs_to_output tests ──

    fn make_test_adr(id: &str, title: &str, projects: Vec<&str>) -> crate::api::types::Adr {
        crate::api::types::Adr {
            id: id.to_string(),
            title: title.to_string(),
            context: None,
            policies: vec![format!("Policy for {title}")],
            instructions: Some(vec![format!("Instruction for {title}")]),
            category: crate::api::types::AdrCategory {
                id: "cat-1".to_string(),
                name: "General".to_string(),
                path: "General".to_string(),
            },
            applies_to: crate::api::types::AppliesTo {
                languages: vec!["rust".to_string()],
                frameworks: vec![],
            },
            matched_projects: projects.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn test_raw_adrs_to_output_empty() {
        let output = raw_adrs_to_output(&[], &OutputFormat::ClaudeMd);
        assert!(output.files.is_empty());
        assert_eq!(output.summary.total_input, 0);
        assert_eq!(output.summary.applicable, 0);
        assert_eq!(output.summary.files_generated, 0);
    }

    #[test]
    fn test_raw_adrs_to_output_single_adr_no_projects() {
        let adrs = vec![make_test_adr("adr-001", "Test ADR", vec![])];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::ClaudeMd);
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "CLAUDE.md");
        assert!(output.files[0].content().contains("Test ADR"));
        assert!(output.files[0].content().contains("Policy for Test ADR"));
        assert!(output.files[0]
            .content()
            .contains("Instruction for Test ADR"));
        assert_eq!(output.files[0].adr_ids(), vec!["adr-001"]);
        assert_eq!(output.summary.total_input, 1);
        assert_eq!(output.summary.files_generated, 1);
    }

    #[test]
    fn test_raw_adrs_to_output_adr_with_project() {
        let adrs = vec![make_test_adr("adr-001", "Web Rules", vec!["apps/web"])];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::ClaudeMd);
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "apps/web/CLAUDE.md");
    }

    #[test]
    fn test_raw_adrs_to_output_adr_with_dot_project() {
        let adrs = vec![make_test_adr("adr-001", "Root Rules", vec!["."])];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::ClaudeMd);
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "CLAUDE.md");
    }

    #[test]
    fn test_raw_adrs_to_output_multiple_projects() {
        let adrs = vec![
            make_test_adr("adr-001", "Web ADR", vec!["apps/web"]),
            make_test_adr("adr-002", "API ADR", vec!["services/api"]),
            make_test_adr("adr-003", "Shared ADR", vec![]),
        ];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::ClaudeMd);
        assert_eq!(output.files.len(), 3);
        let paths: Vec<&str> = output.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"CLAUDE.md"));
        assert!(paths.contains(&"apps/web/CLAUDE.md"));
        assert!(paths.contains(&"services/api/CLAUDE.md"));
        assert_eq!(output.summary.total_input, 3);
        assert_eq!(output.summary.files_generated, 3);
    }

    #[test]
    fn test_raw_adrs_to_output_adr_without_instructions() {
        let mut adr = make_test_adr("adr-001", "Test ADR", vec![]);
        adr.instructions = None;
        let output = raw_adrs_to_output(&[adr], &OutputFormat::ClaudeMd);
        assert_eq!(output.files.len(), 1);
        assert!(output.files[0].content().contains("Policy for Test ADR"));
        // With instructions=None, the instruction line should not appear
        let content = output.files[0].content();
        assert!(
            !content.contains("Instruction for"),
            "expected no instruction lines"
        );
    }

    #[test]
    fn test_raw_adrs_to_output_adr_with_empty_instructions() {
        let mut adr = make_test_adr("adr-001", "Test ADR", vec![]);
        adr.instructions = Some(vec![]);
        let output = raw_adrs_to_output(&[adr], &OutputFormat::ClaudeMd);
        assert_eq!(output.files.len(), 1);
        assert!(output.files[0].content().contains("Policy for Test ADR"));
        // With empty instructions vec, no instruction lines should appear
        let content = output.files[0].content();
        assert!(
            !content.contains("Instruction for"),
            "expected no instruction lines"
        );
    }

    // ── raw_adrs_to_output format tests ──

    #[test]
    fn test_raw_adrs_to_output_agents_md_format_root() {
        let adrs = vec![make_test_adr("adr-001", "Test ADR", vec![])];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::AgentsMd);
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "AGENTS.md");
    }

    #[test]
    fn test_raw_adrs_to_output_agents_md_format_with_project() {
        let adrs = vec![make_test_adr("adr-001", "Web Rules", vec!["apps/web"])];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::AgentsMd);
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "apps/web/AGENTS.md");
    }

    #[test]
    fn test_raw_adrs_to_output_agents_md_format_dot_project() {
        let adrs = vec![make_test_adr("adr-001", "Root Rules", vec!["."])];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::AgentsMd);
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "AGENTS.md");
    }

    // ── raw_adrs_to_output with invalid project paths tests ──

    #[test]
    fn test_raw_adrs_to_output_skips_traversal_project_path() {
        // An ADR with a path-traversal project path should be skipped (no file for that project)
        let adrs = vec![make_test_adr("adr-001", "Test ADR", vec!["../evil"])];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::ClaudeMd);
        // The invalid project path should be skipped; no file generated
        assert!(
            output.files.is_empty(),
            "expected no files for traversal path"
        );
    }

    #[test]
    fn test_raw_adrs_to_output_skips_absolute_project_path() {
        let adrs = vec![make_test_adr("adr-001", "Test ADR", vec!["/etc/passwd"])];
        let output = raw_adrs_to_output(&adrs, &OutputFormat::ClaudeMd);
        assert!(
            output.files.is_empty(),
            "expected no files for absolute path"
        );
    }

    #[test]
    fn test_raw_adrs_to_output_strips_control_chars_from_content() {
        // Use actual control chars (not full ANSI sequences) to verify stripping
        let mut adr = make_test_adr("adr-001", "Test\x07ADR", vec![]);
        adr.policies = vec!["Policy\x01with\x02bells".to_string()];
        adr.instructions = Some(vec!["Instruction\x03normal".to_string()]);
        let output = raw_adrs_to_output(&[adr], &OutputFormat::ClaudeMd);
        assert_eq!(output.files.len(), 1);
        let content = output.files[0].content();
        assert!(
            !content.contains('\x07'),
            "BEL should be stripped from title"
        );
        assert!(
            !content.contains('\x01'),
            "SOH should be stripped from policy"
        );
        assert!(
            !content.contains('\x02'),
            "STX should be stripped from policy"
        );
        assert!(
            !content.contains('\x03'),
            "ETX should be stripped from instruction"
        );
        assert!(
            content.contains("TestADR"),
            "title text should be preserved (without control char)"
        );
        assert!(
            content.contains("Policy"),
            "policy text should be preserved"
        );
        assert!(
            content.contains("Instruction"),
            "instruction text should be preserved"
        );
    }

    // ── find_existing_output_files tests ──

    #[test]
    fn test_find_existing_claude_md_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(result.is_empty());
    }

    #[test]
    fn test_find_existing_claude_md_finds_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "Root content").unwrap();
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(result.contains("Root content"));
        assert!(result.contains("CLAUDE.md"));
    }

    #[test]
    fn test_find_existing_claude_md_finds_nested() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("apps").join("web")).unwrap();
        std::fs::write(
            dir.path().join("apps").join("web").join("CLAUDE.md"),
            "Web content",
        )
        .unwrap();
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(result.contains("Web content"));
    }

    #[test]
    fn test_find_existing_claude_md_skips_hidden_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git").join("CLAUDE.md"), "Git content").unwrap();
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(result.is_empty());
    }

    #[test]
    fn test_find_existing_claude_md_skips_node_modules() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules")).unwrap();
        std::fs::write(
            dir.path().join("node_modules").join("CLAUDE.md"),
            "Module content",
        )
        .unwrap();
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(result.is_empty());
    }

    #[test]
    fn test_find_existing_claude_md_strips_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let content = "# My Rules\n\n<!-- managed:actual-start -->\n<!-- last-synced: 2024-01-01T00:00:00Z -->\n<!-- version: 1 -->\n<!-- adr-ids: abc-123,def-456 -->\n\n## Some Rules\n\nDo this.\n\n<!-- managed:actual-end -->";
        std::fs::write(dir.path().join("CLAUDE.md"), content).unwrap();
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(
            !result.contains("adr-ids"),
            "should strip adr-ids metadata: {result}"
        );
        assert!(
            !result.contains("last-synced"),
            "should strip last-synced metadata: {result}"
        );
        assert!(
            !result.contains("version:"),
            "should strip version metadata: {result}"
        );
        assert!(
            !result.contains("managed:actual"),
            "should strip managed markers: {result}"
        );
        assert!(
            result.contains("Some Rules"),
            "should preserve content: {result}"
        );
        assert!(
            result.contains("Do this"),
            "should preserve content: {result}"
        );
    }

    #[test]
    fn test_find_existing_claude_md_unreadable_dir() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let unreadable = dir.path().join("noperm");
        std::fs::create_dir(&unreadable).unwrap();
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Should not panic, just skip the unreadable dir
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(result.is_empty());
        // Restore permissions for cleanup
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn test_find_existing_claude_md_skips_vendor() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("vendor")).unwrap();
        std::fs::write(
            dir.path().join("vendor").join("CLAUDE.md"),
            "Vendor content",
        )
        .unwrap();
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(result.is_empty(), "should skip vendor directory");
    }

    #[test]
    fn test_find_existing_claude_md_skips_pycache() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("__pycache__")).unwrap();
        std::fs::write(
            dir.path().join("__pycache__").join("CLAUDE.md"),
            "Pycache content",
        )
        .unwrap();
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(result.is_empty(), "should skip __pycache__ directory");
    }

    #[test]
    fn test_find_existing_claude_md_skips_all_ignored_dirs() {
        let dir = tempfile::tempdir().unwrap();
        for skip in super::super::super::SKIP_DIRS {
            let d = dir.path().join(skip);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("CLAUDE.md"), "# Skip").unwrap();
        }
        let result = find_existing_output_files(dir.path(), &OutputFormat::ClaudeMd);
        assert!(
            result.is_empty(),
            "should skip all SKIP_DIRS directories, got: {result}"
        );
    }

    // ── zero_adr_context_message tests ──

    use crate::api::types::{MatchFramework, MatchProject, MatchRequest};

    fn make_match_request(projects: Vec<MatchProject>) -> MatchRequest {
        MatchRequest {
            projects,
            options: None,
        }
    }

    fn make_match_project(languages: Vec<&str>, frameworks: Vec<(&str, &str)>) -> MatchProject {
        MatchProject {
            path: ".".to_string(),
            name: "test-project".to_string(),
            languages: languages.into_iter().map(str::to_string).collect(),
            frameworks: frameworks
                .into_iter()
                .map(|(name, category)| MatchFramework {
                    name: name.to_string(),
                    category: category.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn test_zero_adr_context_message_no_languages() {
        let req = make_match_request(vec![make_match_project(vec![], vec![])]);
        let lines = zero_adr_context_lines(&req);
        let msg = lines.join("\n");
        assert!(
            msg.contains("No supported languages detected"),
            "unexpected message: {msg}"
        );
        assert!(
            msg.contains("unable to match ADRs"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn test_zero_adr_context_message_languages_no_frameworks() {
        let req = make_match_request(vec![make_match_project(vec!["typescript"], vec![])]);
        let lines = zero_adr_context_lines(&req);
        let msg = lines.join("\n");
        assert!(
            msg.contains("Languages: typescript"),
            "unexpected message: {msg}"
        );
        assert!(
            msg.contains("No frameworks detected"),
            "unexpected message: {msg}"
        );
        assert!(
            msg.contains("only general ADRs were eligible"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn test_zero_adr_context_message_languages_and_frameworks() {
        let req = make_match_request(vec![make_match_project(
            vec!["typescript"],
            vec![("nextjs", "web-frontend")],
        )]);
        let lines = zero_adr_context_lines(&req);
        let msg = lines.join("\n");
        assert!(
            msg.contains("Languages: typescript"),
            "unexpected message: {msg}"
        );
        assert!(
            msg.contains("Frameworks: nextjs"),
            "unexpected message: {msg}"
        );
        assert!(
            msg.contains("No ADRs available for this stack yet"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn test_zero_adr_context_message_deduplicates_across_projects() {
        // Two projects with the same language → appears only once in message
        let req = make_match_request(vec![
            make_match_project(vec!["typescript"], vec![]),
            make_match_project(vec!["typescript"], vec![]),
        ]);
        let lines = zero_adr_context_lines(&req);
        let msg = lines.join("\n");
        // "typescript" should appear exactly once
        assert_eq!(
            msg.matches("typescript").count(),
            1,
            "language should be deduplicated: {msg}"
        );
    }

    #[test]
    fn test_zero_adr_context_message_multiple_languages_sorted() {
        let req = make_match_request(vec![make_match_project(vec!["rust", "typescript"], vec![])]);
        let lines = zero_adr_context_lines(&req);
        let msg = lines.join("\n");
        // Languages should be sorted alphabetically
        let rust_pos = msg.find("rust").unwrap();
        let ts_pos = msg.find("typescript").unwrap();
        assert!(
            rust_pos < ts_pos,
            "languages should be sorted: rust before typescript in '{msg}'"
        );
    }

    #[test]
    fn test_zero_adr_context_message_empty_request() {
        // No projects at all
        let req = make_match_request(vec![]);
        let lines = zero_adr_context_lines(&req);
        let msg = lines.join("\n");
        assert!(
            msg.contains("No supported languages detected"),
            "empty request should produce no-languages message: {msg}"
        );
    }

    // ── fetch_error_message tests ──

    #[test]
    fn test_fetch_error_message_connection_refused() {
        let e = ActualError::ApiError("error sending request: Connection refused".to_string());
        let msg = fetch_error_message(&e, "https://api.example.com");
        assert!(
            msg.contains("Unable to connect to Actual AI"),
            "unexpected msg: {msg}"
        );
        assert!(
            msg.contains("https://api.example.com"),
            "should include api_url: {msg}"
        );
        assert!(
            msg.contains("check your network connection"),
            "unexpected msg: {msg}"
        );
    }

    #[test]
    fn test_fetch_error_message_error_trying_to_connect() {
        let e = ActualError::ApiError(
            "error trying to connect: tcp connect error: Connection refused".to_string(),
        );
        let msg = fetch_error_message(&e, "https://api.example.com");
        assert!(
            msg.contains("Unable to connect to Actual AI"),
            "unexpected msg: {msg}"
        );
    }

    #[test]
    fn test_fetch_error_message_dns_error() {
        let e = ActualError::ApiError("dns error: failed to lookup".to_string());
        let msg = fetch_error_message(&e, "https://api.example.com");
        assert!(
            msg.contains("Unable to connect to Actual AI"),
            "unexpected msg: {msg}"
        );
    }

    #[test]
    fn test_fetch_error_message_timed_out() {
        let e = ActualError::ApiError("request timed out after 30s".to_string());
        let msg = fetch_error_message(&e, "https://api.example.com");
        assert!(
            msg.contains("Connection to Actual AI timed out"),
            "unexpected msg: {msg}"
        );
        assert!(msg.contains("try again later"), "unexpected msg: {msg}");
    }

    #[test]
    fn test_fetch_error_message_operation_timed_out() {
        let e = ActualError::ApiError("operation timed out".to_string());
        let msg = fetch_error_message(&e, "https://api.example.com");
        assert!(
            msg.contains("Connection to Actual AI timed out"),
            "unexpected msg: {msg}"
        );
    }

    #[test]
    fn test_fetch_error_message_api_response_error() {
        let e = ActualError::ApiResponseError {
            code: "401".to_string(),
            message: "unauthorized".to_string(),
        };
        let msg = fetch_error_message(&e, "https://api.example.com");
        assert!(
            msg.contains("Actual AI returned an error (401): unauthorized"),
            "unexpected msg: {msg}"
        );
    }

    #[test]
    fn test_fetch_error_message_api_response_error_500() {
        let e = ActualError::ApiResponseError {
            code: "500".to_string(),
            message: "internal server error".to_string(),
        };
        let msg = fetch_error_message(&e, "https://api.example.com");
        assert!(
            msg.contains("Actual AI returned an error (500): internal server error"),
            "unexpected msg: {msg}"
        );
    }

    #[test]
    fn test_fetch_error_message_generic_api_error_catch_all() {
        let e = ActualError::ApiError("some unexpected error".to_string());
        let msg = fetch_error_message(&e, "https://api.example.com");
        assert!(msg.contains("API request failed:"), "unexpected msg: {msg}");
        assert!(
            msg.contains("some unexpected error"),
            "unexpected msg: {msg}"
        );
    }

    #[test]
    fn test_fetch_error_message_user_cancelled_catch_all() {
        let e = ActualError::UserCancelled;
        let msg = fetch_error_message(&e, "https://api.example.com");
        assert!(msg.contains("API request failed:"), "unexpected msg: {msg}");
    }
}
