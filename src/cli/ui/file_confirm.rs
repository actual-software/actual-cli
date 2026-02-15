use crate::error::ActualError;
use crate::tailoring::types::{FileOutput, TailoringOutput};

/// Trait abstracting terminal I/O for testability.
pub trait TerminalIO: Send + Sync {
    /// Read a line of input from the user, displaying the given prompt.
    fn read_line(&self, prompt: &str) -> Result<String, ActualError>;
    /// Write a line of text to the terminal.
    fn write_line(&self, text: &str);
}

/// Parsed user action from the per-file confirmation prompt.
#[derive(Debug, Clone, PartialEq)]
pub enum FileConfirmAction {
    /// Accept all currently-accepted files and proceed.
    AcceptAll,
    /// Toggle rejection of a file (1-indexed).
    Reject(usize),
    /// Preview a file's content (1-indexed).
    Preview(usize),
    /// Quit the confirmation flow.
    Quit,
}

/// Main entry point for the per-file confirmation flow.
///
/// If `force` is true, returns all files immediately without prompting.
/// Otherwise, runs an interactive loop allowing the user to reject, preview,
/// accept, or quit.
pub fn confirm_files(
    output: &TailoringOutput,
    force: bool,
    term: &dyn TerminalIO,
) -> Result<Vec<FileOutput>, ActualError> {
    if force {
        return Ok(output.files.clone());
    }

    let files = &output.files;
    let mut accepted: Vec<bool> = vec![true; files.len()];

    display_file_summary(files, &accepted, term);

    if !output.skipped_adrs.is_empty() {
        term.write_line(&format!(
            "{} ADR(s) skipped (not applicable)",
            output.skipped_adrs.len()
        ));
    }

    loop {
        let action = prompt_action(files.len(), term)?;
        match action {
            FileConfirmAction::AcceptAll => {
                let result = files
                    .iter()
                    .zip(accepted.iter())
                    .filter(|(_, &acc)| acc)
                    .map(|(f, _)| f.clone())
                    .collect();
                return Ok(result);
            }
            FileConfirmAction::Reject(n) => {
                accepted[n - 1] = !accepted[n - 1];
                display_file_summary(files, &accepted, term);
            }
            FileConfirmAction::Preview(n) => {
                preview_file(&files[n - 1], term);
                display_file_summary(files, &accepted, term);
            }
            FileConfirmAction::Quit => {
                return Err(ActualError::UserCancelled);
            }
        }
    }
}

/// Display a numbered summary of files with their status.
pub fn display_file_summary(files: &[FileOutput], accepted: &[bool], term: &dyn TerminalIO) {
    term.write_line("");
    term.write_line("Files to write:");

    for (i, file) in files.iter().enumerate() {
        let num = i + 1;
        let adr_count = file.adr_ids.len();
        let status = if accepted[i] {
            "✔ accepted"
        } else {
            "✖ rejected"
        };

        let plural = if adr_count == 1 { "" } else { "s" };
        term.write_line(&format!(
            "  {num}. {}  ({adr_count} ADR{plural})  [{status}]",
            file.path
        ));
        term.write_line(&format!("     {}", file.reasoning));
    }
    term.write_line("");
}

/// Prompt the user for an action and parse it, re-prompting on invalid input.
pub fn prompt_action(
    file_count: usize,
    term: &dyn TerminalIO,
) -> Result<FileConfirmAction, ActualError> {
    loop {
        let input = term.read_line("[a]ccept all  [r N]eject  [p N]review  [q]uit: ")?;
        match parse_action(input.trim(), file_count) {
            Ok(action) => return Ok(action),
            Err(msg) => {
                term.write_line(&format!("Error: {msg}"));
            }
        }
    }
}

/// Parse user input into a `FileConfirmAction`.
///
/// Handles:
/// - `"a"` -> `AcceptAll`
/// - `"r N"` or `"rN"` -> `Reject(N)` (1-indexed, validated against `file_count`)
/// - `"p N"` or `"pN"` -> `Preview(N)` (1-indexed, validated against `file_count`)
/// - `"q"` -> `Quit`
///
/// Case-insensitive. Returns `Err(String)` with an error message on invalid input.
pub fn parse_action(input: &str, file_count: usize) -> Result<FileConfirmAction, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("empty input".to_string());
    }

    let lower = trimmed.to_lowercase();

    if lower == "a" {
        return Ok(FileConfirmAction::AcceptAll);
    }

    if lower == "q" {
        return Ok(FileConfirmAction::Quit);
    }

    // Try to parse "r N", "rN", "p N", "pN"
    let (prefix, rest) = if let Some(rest) = lower.strip_prefix('r') {
        ('r', rest)
    } else if let Some(rest) = lower.strip_prefix('p') {
        ('p', rest)
    } else {
        return Err(format!("unknown command: {trimmed}"));
    };

    let rest = rest.trim();
    if rest.is_empty() {
        return Err(format!("'{prefix}' requires a file number"));
    }

    let n: usize = rest
        .parse()
        .map_err(|_| format!("invalid file number: {rest}"))?;

    if n == 0 || n > file_count {
        return Err(format!("file number {n} out of range (1..={file_count})"));
    }

    if prefix == 'r' {
        Ok(FileConfirmAction::Reject(n))
    } else {
        Ok(FileConfirmAction::Preview(n))
    }
}

/// Display the full content of a file for preview.
pub fn preview_file(file: &FileOutput, term: &dyn TerminalIO) {
    term.write_line("");
    term.write_line(&format!("── {} ──", file.path));
    term.write_line(&file.content);
    term.write_line("── end ──");
    term.write_line("");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tailoring::types::{SkippedAdr, TailoringSummary};
    use std::sync::Mutex;

    struct MockTerminal {
        inputs: Mutex<Vec<String>>,
        output: Mutex<Vec<String>>,
    }

    impl MockTerminal {
        fn new(inputs: Vec<&str>) -> Self {
            Self {
                inputs: Mutex::new(inputs.into_iter().map(|s| s.to_string()).collect()),
                output: Mutex::new(Vec::new()),
            }
        }

        fn output_text(&self) -> String {
            self.output.lock().unwrap().join("\n")
        }
    }

    impl TerminalIO for MockTerminal {
        fn read_line(&self, _prompt: &str) -> Result<String, ActualError> {
            let mut inputs = self.inputs.lock().unwrap();
            if inputs.is_empty() {
                return Err(ActualError::UserCancelled);
            }
            Ok(inputs.remove(0))
        }

        fn write_line(&self, text: &str) {
            self.output.lock().unwrap().push(text.to_string());
        }
    }

    fn make_test_output(file_count: usize) -> TailoringOutput {
        let files: Vec<FileOutput> = (1..=file_count)
            .map(|i| FileOutput {
                path: format!("path/file{i}.md"),
                content: format!("content of file {i}"),
                reasoning: format!("reason for file {i}"),
                adr_ids: vec![format!("adr-{i:03}")],
            })
            .collect();

        TailoringOutput {
            summary: TailoringSummary {
                total_input: file_count,
                applicable: file_count,
                not_applicable: 0,
                files_generated: file_count,
            },
            skipped_adrs: vec![],
            files,
        }
    }

    fn make_test_output_with_skipped() -> TailoringOutput {
        let mut output = make_test_output(2);
        output.skipped_adrs = vec![
            SkippedAdr {
                id: "adr-skip-1".to_string(),
                reason: "not applicable".to_string(),
            },
            SkippedAdr {
                id: "adr-skip-2".to_string(),
                reason: "wrong tech stack".to_string(),
            },
        ];
        output.summary.not_applicable = 2;
        output
    }

    // ── parse_action tests ──

    #[test]
    fn test_parse_action_accept_all() {
        assert_eq!(parse_action("a", 3), Ok(FileConfirmAction::AcceptAll));
    }

    #[test]
    fn test_parse_action_quit() {
        assert_eq!(parse_action("q", 3), Ok(FileConfirmAction::Quit));
    }

    #[test]
    fn test_parse_action_reject_with_space() {
        assert_eq!(parse_action("r 1", 3), Ok(FileConfirmAction::Reject(1)));
    }

    #[test]
    fn test_parse_action_reject_no_space() {
        assert_eq!(parse_action("r1", 3), Ok(FileConfirmAction::Reject(1)));
    }

    #[test]
    fn test_parse_action_preview_with_space() {
        assert_eq!(parse_action("p 2", 3), Ok(FileConfirmAction::Preview(2)));
    }

    #[test]
    fn test_parse_action_preview_no_space() {
        assert_eq!(parse_action("p2", 3), Ok(FileConfirmAction::Preview(2)));
    }

    #[test]
    fn test_parse_action_reject_zero_out_of_range() {
        assert!(parse_action("r 0", 3).is_err());
    }

    #[test]
    fn test_parse_action_reject_exceeds_count() {
        let result = parse_action("r 99", 3);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("out of range"));
    }

    #[test]
    fn test_parse_action_reject_no_number() {
        let result = parse_action("r", 3);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("requires a file number"));
    }

    #[test]
    fn test_parse_action_preview_no_number() {
        let result = parse_action("p", 3);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("requires a file number"));
    }

    #[test]
    fn test_parse_action_reject_non_numeric() {
        let result = parse_action("r abc", 3);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid file number"));
    }

    #[test]
    fn test_parse_action_empty() {
        let result = parse_action("", 3);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty input"));
    }

    #[test]
    fn test_parse_action_unknown_command() {
        let result = parse_action("x", 3);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown command"));
    }

    #[test]
    fn test_parse_action_case_insensitive_accept() {
        assert_eq!(parse_action("A", 3), Ok(FileConfirmAction::AcceptAll));
    }

    #[test]
    fn test_parse_action_case_insensitive_reject() {
        assert_eq!(parse_action("R 1", 3), Ok(FileConfirmAction::Reject(1)));
    }

    // ── confirm_files with force=true ──

    #[test]
    fn test_confirm_files_force_returns_all() {
        let output = make_test_output(3);
        let term = MockTerminal::new(vec![]);
        let result = confirm_files(&output, true, &term).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].path, "path/file1.md");
        assert_eq!(result[1].path, "path/file2.md");
        assert_eq!(result[2].path, "path/file3.md");
    }

    #[test]
    fn test_confirm_files_force_empty_list() {
        let output = make_test_output(0);
        let term = MockTerminal::new(vec![]);
        let result = confirm_files(&output, true, &term).unwrap();
        assert!(result.is_empty());
    }

    // ── confirm_files interactive tests ──

    #[test]
    fn test_confirm_files_accept_immediately() {
        let output = make_test_output(2);
        let term = MockTerminal::new(vec!["a"]);
        let result = confirm_files(&output, false, &term).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_confirm_files_quit() {
        let output = make_test_output(2);
        let term = MockTerminal::new(vec!["q"]);
        let result = confirm_files(&output, false, &term);
        assert!(
            matches!(result, Err(ActualError::UserCancelled)),
            "expected UserCancelled"
        );
    }

    #[test]
    fn test_confirm_files_reject_then_accept() {
        let output = make_test_output(3);
        let term = MockTerminal::new(vec!["r 2", "a"]);
        let result = confirm_files(&output, false, &term).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].path, "path/file1.md");
        assert_eq!(result[1].path, "path/file3.md");
    }

    #[test]
    fn test_confirm_files_preview_then_accept() {
        let output = make_test_output(2);
        let term = MockTerminal::new(vec!["p 1", "a"]);
        let result = confirm_files(&output, false, &term).unwrap();
        assert_eq!(result.len(), 2);
        // Verify preview output contains file content
        let text = term.output_text();
        assert!(
            text.contains("content of file 1"),
            "expected file content in preview output"
        );
    }

    #[test]
    fn test_confirm_files_reject_all_then_accept() {
        let output = make_test_output(2);
        let term = MockTerminal::new(vec!["r 1", "r 2", "a"]);
        let result = confirm_files(&output, false, &term).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_confirm_files_toggle_reject() {
        let output = make_test_output(2);
        // Reject file 1, then un-reject it, then accept
        let term = MockTerminal::new(vec!["r 1", "r 1", "a"]);
        let result = confirm_files(&output, false, &term).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_confirm_files_skipped_adrs_message() {
        let output = make_test_output_with_skipped();
        let term = MockTerminal::new(vec!["a"]);
        let _ = confirm_files(&output, false, &term).unwrap();
        let text = term.output_text();
        assert!(
            text.contains("2 ADR(s) skipped (not applicable)"),
            "expected skipped ADRs message in output, got: {text}"
        );
    }

    // ── display_file_summary tests ──

    #[test]
    fn test_display_file_summary_contains_paths() {
        let output = make_test_output(2);
        let term = MockTerminal::new(vec![]);
        display_file_summary(&output.files, &[true, true], &term);
        let text = term.output_text();
        assert!(
            text.contains("path/file1.md"),
            "expected file1 path in output"
        );
        assert!(
            text.contains("path/file2.md"),
            "expected file2 path in output"
        );
    }

    #[test]
    fn test_display_file_summary_shows_adr_counts() {
        let output = make_test_output(1);
        let term = MockTerminal::new(vec![]);
        display_file_summary(&output.files, &[true], &term);
        let text = term.output_text();
        assert!(
            text.contains("1 ADR"),
            "expected ADR count in output, got: {text}"
        );
    }

    #[test]
    fn test_display_file_summary_rejected_files_marked() {
        let output = make_test_output(2);
        let term = MockTerminal::new(vec![]);
        display_file_summary(&output.files, &[true, false], &term);
        let text = term.output_text();
        assert!(
            text.contains("rejected"),
            "expected 'rejected' marker in output, got: {text}"
        );
        assert!(
            text.contains("accepted"),
            "expected 'accepted' marker in output, got: {text}"
        );
    }

    // ── preview_file tests ──

    #[test]
    fn test_preview_file_contains_path_and_content() {
        let file = FileOutput {
            path: "CLAUDE.md".to_string(),
            content: "# Rules\nBe nice.".to_string(),
            reasoning: "root rules".to_string(),
            adr_ids: vec!["adr-001".to_string()],
        };
        let term = MockTerminal::new(vec![]);
        preview_file(&file, &term);
        let text = term.output_text();
        assert!(text.contains("CLAUDE.md"), "expected file path in preview");
        assert!(
            text.contains("# Rules\nBe nice."),
            "expected file content in preview"
        );
    }

    // ── prompt_action error recovery tests ──

    #[test]
    fn test_prompt_action_invalid_then_valid() {
        // First input is invalid ("xyz"), second is valid ("a")
        let term = MockTerminal::new(vec!["xyz", "a"]);
        let action = prompt_action(3, &term).unwrap();
        assert_eq!(action, FileConfirmAction::AcceptAll);
        let text = term.output_text();
        assert!(
            text.contains("Error:"),
            "expected error message for invalid input, got: {text}"
        );
    }

    #[test]
    fn test_prompt_action_multiple_invalid_then_valid() {
        let term = MockTerminal::new(vec!["", "bad", "r", "q"]);
        let action = prompt_action(3, &term).unwrap();
        assert_eq!(action, FileConfirmAction::Quit);
    }

    #[test]
    fn test_prompt_action_read_error_propagates() {
        // Empty inputs -> MockTerminal returns UserCancelled
        let term = MockTerminal::new(vec![]);
        let result = prompt_action(3, &term);
        assert!(
            matches!(result, Err(ActualError::UserCancelled)),
            "expected UserCancelled"
        );
    }

    // ── display_file_summary pluralization tests ──

    #[test]
    fn test_display_file_summary_multiple_adrs_plural() {
        let files = vec![FileOutput {
            path: "CLAUDE.md".to_string(),
            content: "content".to_string(),
            reasoning: "reason".to_string(),
            adr_ids: vec![
                "adr-001".to_string(),
                "adr-002".to_string(),
                "adr-003".to_string(),
            ],
        }];
        let term = MockTerminal::new(vec![]);
        display_file_summary(&files, &[true], &term);
        let text = term.output_text();
        assert!(
            text.contains("3 ADRs"),
            "expected plural 'ADRs' in output, got: {text}"
        );
    }

    #[test]
    fn test_display_file_summary_single_adr_no_plural() {
        let files = vec![FileOutput {
            path: "CLAUDE.md".to_string(),
            content: "content".to_string(),
            reasoning: "reason".to_string(),
            adr_ids: vec!["adr-001".to_string()],
        }];
        let term = MockTerminal::new(vec![]);
        display_file_summary(&files, &[true], &term);
        let text = term.output_text();
        assert!(
            text.contains("1 ADR)"),
            "expected singular 'ADR)' without 's' in output, got: {text}"
        );
    }

    // ── confirm_files with invalid input recovery ──

    #[test]
    fn test_confirm_files_invalid_input_then_accept() {
        let output = make_test_output(2);
        let term = MockTerminal::new(vec!["invalid", "a"]);
        let result = confirm_files(&output, false, &term).unwrap();
        assert_eq!(result.len(), 2);
        let text = term.output_text();
        assert!(
            text.contains("Error:"),
            "expected error message for invalid input"
        );
    }
}
