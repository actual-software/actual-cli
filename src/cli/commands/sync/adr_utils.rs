use std::collections::HashMap;
use std::path::Path;

use crate::error::ActualError;
use crate::generation::markers;
use crate::generation::OutputFormat;
use crate::tailoring::types::{FileOutput, TailoringOutput, TailoringSummary};

pub(crate) fn zero_adr_context_lines(request: &crate::api::types::MatchRequest) -> Vec<String> {
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
pub(crate) fn fetch_error_message(e: &ActualError, api_url: &str) -> String {
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

/// Strip non-printable ASCII control characters from server-controlled ADR content.
///
/// Keeps newlines (`\n`), carriage returns (`\r`), and tabs (`\t`) as they are
/// legitimate in markdown, but removes other control characters (bytes < 0x20
/// and DEL 0x7F) that could be used for terminal manipulation.
pub(crate) fn strip_control_chars(s: &str) -> String {
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
pub(crate) fn validate_project_path(p: &str) -> Result<(), ActualError> {
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
pub(crate) fn raw_adrs_to_output(
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
pub(crate) fn find_existing_output_files(root_dir: &Path, format: &OutputFormat) -> String {
    let files = super::super::find_output_files(root_dir, format);
    // `find_output_files` returns paths in sorted order, and `filter_map`
    // preserves that order, so no additional sort is needed here.
    let results: Vec<String> = files
        .iter()
        .filter_map(|path| {
            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("failed to read output file {}: {e}", path.display());
                    return None;
                }
            };
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
