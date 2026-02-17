use crate::error::ActualError;
use crate::tailoring::types::{FileOutput, TailoringOutput};

/// Trait abstracting terminal I/O for testability.
pub trait TerminalIO: Send + Sync {
    /// Read a line of input from the user, displaying the given prompt.
    fn read_line(&self, prompt: &str) -> Result<String, ActualError>;
    /// Write a line of text to the terminal.
    fn write_line(&self, text: &str);

    /// Prompt the user for a yes/no confirmation.
    ///
    /// Default implementation uses `read_line` as fallback.
    /// `RealTerminal` overrides this with `dialoguer::Confirm`.
    fn confirm(&self, prompt: &str) -> Result<bool, ActualError> {
        let input = self.read_line(prompt)?;
        match input.trim().to_lowercase().as_str() {
            "y" | "yes" => Ok(true),
            "n" | "no" => Ok(false),
            _ => Ok(false), // Default to rejection for safety
        }
    }

    /// Present a multi-select file picker.
    /// Returns `Ok(Some(indices))` for confirmed selection, `Ok(None)` for cancel.
    fn select_files(
        &self,
        prompt: &str,
        items: &[String],
        defaults: &[bool],
    ) -> Result<Option<Vec<usize>>, ActualError>;
}

/// Main entry point for the per-file confirmation flow.
///
/// If `force` is true, returns all files immediately without prompting.
/// Otherwise, presents a multi-select dialog for the user to choose files.
pub fn confirm_files(
    output: &TailoringOutput,
    force: bool,
    term: &dyn TerminalIO,
) -> Result<Vec<FileOutput>, ActualError> {
    if force {
        return Ok(output.files.clone());
    }

    let files = &output.files;

    // Show skipped ADRs count before the empty check so users always see it
    if !output.skipped_adrs.is_empty() {
        term.write_line(&format!(
            "{} ADR(s) skipped (not applicable)",
            output.skipped_adrs.len()
        ));
    }

    if files.is_empty() {
        return Ok(vec![]);
    }

    // Build display items
    let items: Vec<String> = files
        .iter()
        .map(|file| {
            let count = file.adr_ids.len();
            let plural = if count == 1 { "" } else { "s" };
            format!("{}  ({count} ADR{plural})", file.path)
        })
        .collect();

    let defaults = vec![true; files.len()];

    match term.select_files("Select files to write", &items, &defaults)? {
        Some(indices) => {
            let result: Vec<FileOutput> = indices.into_iter().map(|i| files[i].clone()).collect();
            Ok(result)
        }
        None => Err(ActualError::UserCancelled),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tailoring::types::{SkippedAdr, TailoringSummary};
    use std::sync::Mutex;

    struct MockTerminal {
        select_result: Mutex<Option<Option<Vec<usize>>>>,
        output: Mutex<Vec<String>>,
    }

    impl MockTerminal {
        fn with_selection(result: Option<Vec<usize>>) -> Self {
            Self {
                select_result: Mutex::new(Some(result)),
                output: Mutex::new(Vec::new()),
            }
        }

        fn output_text(&self) -> String {
            self.output.lock().unwrap().join("\n")
        }
    }

    impl TerminalIO for MockTerminal {
        fn read_line(&self, _prompt: &str) -> Result<String, ActualError> {
            Err(ActualError::UserCancelled)
        }

        fn write_line(&self, text: &str) {
            self.output.lock().unwrap().push(text.to_string());
        }

        fn select_files(
            &self,
            _prompt: &str,
            _items: &[String],
            _defaults: &[bool],
        ) -> Result<Option<Vec<usize>>, ActualError> {
            self.select_result
                .lock()
                .unwrap()
                .take()
                .ok_or(ActualError::UserCancelled)
        }
    }

    fn make_test_output(file_count: usize) -> TailoringOutput {
        let files: Vec<FileOutput> = (1..=file_count)
            .map(|i| {
                // First file gets 1 ADR (singular), rest get 2 (plural)
                let adr_ids = if i == 1 {
                    vec![format!("adr-{i:03}")]
                } else {
                    vec![format!("adr-{i:03}"), format!("adr-{i:03}-extra")]
                };
                FileOutput {
                    path: format!("path/file{i}.md"),
                    content: format!("content of file {i}"),
                    reasoning: format!("reason for file {i}"),
                    adr_ids,
                }
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

    // ── confirm_files with force=true ──

    #[test]
    fn test_confirm_files_force_returns_all() {
        let output = make_test_output(3);
        let term = MockTerminal::with_selection(Some(vec![]));
        let result = confirm_files(&output, true, &term).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].path, "path/file1.md");
        assert_eq!(result[1].path, "path/file2.md");
        assert_eq!(result[2].path, "path/file3.md");
    }

    #[test]
    fn test_confirm_files_force_empty_list() {
        let output = make_test_output(0);
        let term = MockTerminal::with_selection(Some(vec![]));
        let result = confirm_files(&output, true, &term).unwrap();
        assert!(result.is_empty());
    }

    // ── confirm_files interactive tests ──

    #[test]
    fn test_confirm_files_accept_all() {
        let output = make_test_output(2);
        let term = MockTerminal::with_selection(Some(vec![0, 1]));
        let result = confirm_files(&output, false, &term).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].path, "path/file1.md");
        assert_eq!(result[1].path, "path/file2.md");
    }

    #[test]
    fn test_confirm_files_cancelled() {
        let output = make_test_output(2);
        let term = MockTerminal::with_selection(None);
        let result = confirm_files(&output, false, &term);
        assert!(
            matches!(result, Err(ActualError::UserCancelled)),
            "expected UserCancelled"
        );
    }

    #[test]
    fn test_confirm_files_reject_file() {
        let output = make_test_output(3);
        // Select files 0 and 2, skip file 1
        let term = MockTerminal::with_selection(Some(vec![0, 2]));
        let result = confirm_files(&output, false, &term).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].path, "path/file1.md");
        assert_eq!(result[1].path, "path/file3.md");
    }

    #[test]
    fn test_confirm_files_reject_all() {
        let output = make_test_output(2);
        let term = MockTerminal::with_selection(Some(vec![]));
        let result = confirm_files(&output, false, &term).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_confirm_files_skipped_adrs_message() {
        let output = make_test_output_with_skipped();
        let term = MockTerminal::with_selection(Some(vec![0, 1]));
        let _ = confirm_files(&output, false, &term).unwrap();
        let text = term.output_text();
        assert!(
            text.contains("2 ADR(s) skipped (not applicable)"),
            "expected skipped ADRs message in output, got: {text}"
        );
    }

    #[test]
    fn test_confirm_files_partial_selection() {
        let output = make_test_output(4);
        // Select only files at index 1 and 3
        let term = MockTerminal::with_selection(Some(vec![1, 3]));
        let result = confirm_files(&output, false, &term).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].path, "path/file2.md");
        assert_eq!(result[1].path, "path/file4.md");
    }

    #[test]
    fn test_confirm_files_empty_file_list_no_force() {
        let output = make_test_output(0);
        let term = MockTerminal::with_selection(Some(vec![]));
        let result = confirm_files(&output, false, &term).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_confirm_files_skipped_adrs_shown_with_empty_files() {
        let mut output = make_test_output(0);
        output.skipped_adrs = vec![SkippedAdr {
            id: "adr-skip-1".to_string(),
            reason: "not applicable".to_string(),
        }];
        let term = MockTerminal::with_selection(Some(vec![]));
        let result = confirm_files(&output, false, &term).unwrap();
        assert!(result.is_empty());
        let text = term.output_text();
        assert!(
            text.contains("1 ADR(s) skipped (not applicable)"),
            "expected skipped ADRs message even with empty files, got: {text}"
        );
    }

    #[test]
    fn test_confirm_files_select_error() {
        let output = make_test_output(2);
        // No result set — select_files returns an error
        let term = MockTerminal {
            select_result: Mutex::new(None),
            output: Mutex::new(Vec::new()),
        };
        let result = confirm_files(&output, false, &term);
        assert!(
            matches!(result, Err(ActualError::UserCancelled)),
            "expected UserCancelled on I/O error"
        );
    }

    // ── MockTerminal trait coverage ──

    #[test]
    fn mock_terminal_read_line_returns_error() {
        let term = MockTerminal::with_selection(Some(vec![]));
        let result = term.read_line("prompt");
        assert!(matches!(result, Err(ActualError::UserCancelled)));
    }
}
