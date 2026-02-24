use serde::{Deserialize, Serialize};

/// Progress event emitted during concurrent tailoring.
///
/// Sent through a channel to allow callers to display real-time progress
/// as individual projects and batches complete.
#[derive(Debug, Clone, PartialEq)]
pub enum TailoringEvent {
    /// A project started processing.
    ProjectStarted {
        /// Human-readable project name.
        project_name: String,
        /// Total number of batches that will be processed for this project.
        batch_count: usize,
    },
    /// A single batch within a project started (emitted after semaphore acquired,
    /// before invocation begins).
    BatchStarted {
        /// Human-readable project name.
        project_name: String,
        /// 1-based index of this batch within the project (1..=batch_count).
        batch_index: usize,
        /// Total number of batches for this project.
        batch_count: usize,
        /// Number of ADRs in this batch.
        adr_count: usize,
        /// Category name shared by all ADRs in this batch.
        category_name: String,
        /// Titles of ADRs in this batch.
        adr_titles: Vec<String>,
    },
    /// A single batch within a project completed successfully.
    BatchCompleted {
        /// Human-readable project name.
        project_name: String,
        /// 1-based index of this batch within the project (1..=batch_count).
        batch_index: usize,
        /// Total number of batches for this project.
        batch_count: usize,
        /// Number of ADRs in this batch.
        adr_count: usize,
        /// Number of ADRs that were applicable (applied) in this batch.
        applied_count: usize,
        /// Number of ADRs that were not applicable (skipped) in this batch.
        skipped_count: usize,
        /// Skipped ADRs: (adr_id, reason), truncated to first 3 if many.
        skipped_adrs: Vec<(String, String)>,
        /// Output file paths produced by this batch.
        file_paths: Vec<String>,
        /// Wall-clock duration of the invocation in seconds.
        elapsed_secs: u64,
    },
    /// A project finished successfully.
    ProjectCompleted {
        /// Human-readable project name.
        project_name: String,
        /// Number of CLAUDE.md files generated for this project.
        files_generated: usize,
        /// Number of ADRs that were applicable (not skipped) for this project.
        adrs_applied: usize,
        /// Merged output file paths for this project.
        file_paths: Vec<String>,
    },
    /// A project failed.
    ProjectFailed {
        /// Human-readable project name.
        project_name: String,
        /// Error message.
        error: String,
    },
}

/// Output from the combined tailoring + formatting Claude Code invocation.
///
/// Represents the complete result of processing ADRs through the tailoring pipeline,
/// including generated CLAUDE.md files, skipped ADRs, and a summary of the operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TailoringOutput {
    /// Generated CLAUDE.md files, one per target location
    pub files: Vec<FileOutput>,
    /// ADRs that were not applicable to this repository
    pub skipped_adrs: Vec<SkippedAdr>,
    /// Summary statistics of the tailoring operation
    #[serde(default)]
    pub summary: TailoringSummary,
}

/// A single ADR's contribution to a file's managed section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdrSection {
    /// The ADR UUID
    pub adr_id: String,
    /// AI-generated markdown content for this ADR
    pub content: String,
}

/// A single CLAUDE.md file to write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileOutput {
    /// File path relative to repo root (e.g., "CLAUDE.md" or "apps/web/CLAUDE.md")
    pub path: String,
    /// Per-ADR sections: one entry per ADR included in this file
    pub sections: Vec<AdrSection>,
    /// Brief explanation of what this file contains and why.
    /// Populated from LLM output and emitted via `tracing::debug!` in
    /// `invoke_tailoring` (`invoke.rs`) after the full tailoring result is
    /// assembled. Not surfaced in user-visible UI (not in --verbose, not in diff panels).
    pub reasoning: String,
}

impl FileOutput {
    /// Combined content from all sections, joined with double newline.
    pub fn content(&self) -> String {
        self.sections
            .iter()
            .map(|s| s.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// All ADR IDs from sections, in order.
    pub fn adr_ids(&self) -> Vec<String> {
        self.sections.iter().map(|s| s.adr_id.clone()).collect()
    }
}

/// An ADR that was not applicable to the repository.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkippedAdr {
    /// The ADR identifier
    pub id: String,
    /// Explanation of why this ADR was not applicable
    pub reason: String,
}

/// Summary statistics for the tailoring operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TailoringSummary {
    /// Total number of ADRs provided as input
    pub total_input: usize,
    /// Number of ADRs that were applicable to the repo
    pub applicable: usize,
    /// Number of ADRs that were not applicable
    pub not_applicable: usize,
    /// Number of CLAUDE.md files generated
    pub files_generated: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── TailoringEvent tests ──

    #[test]
    fn test_tailoring_event_project_started_debug_clone_eq() {
        let event = TailoringEvent::ProjectStarted {
            project_name: "my-app".to_string(),
            batch_count: 3,
        };
        let cloned = event.clone();
        assert_eq!(event, cloned);
        let debug = format!("{:?}", event);
        assert!(
            debug.contains("ProjectStarted"),
            "expected variant in: {debug}"
        );
        assert!(debug.contains("my-app"), "expected name in: {debug}");
    }

    #[test]
    fn test_tailoring_event_project_completed_debug_clone_eq() {
        let event = TailoringEvent::ProjectCompleted {
            project_name: "web-app".to_string(),
            files_generated: 3,
            adrs_applied: 5,
            file_paths: vec!["CLAUDE.md".to_string()],
        };
        let cloned = event.clone();
        assert_eq!(event, cloned);
        let debug = format!("{:?}", event);
        assert!(
            debug.contains("ProjectCompleted"),
            "expected variant in: {debug}"
        );
        assert!(debug.contains("web-app"), "expected name in: {debug}");
        assert!(debug.contains('3'), "expected files_generated in: {debug}");
        assert!(debug.contains('5'), "expected adrs_applied in: {debug}");
    }

    #[test]
    fn test_tailoring_event_project_failed_debug_clone_eq() {
        let event = TailoringEvent::ProjectFailed {
            project_name: "api-service".to_string(),
            error: "timeout".to_string(),
        };
        let cloned = event.clone();
        assert_eq!(event, cloned);
        let debug = format!("{:?}", event);
        assert!(
            debug.contains("ProjectFailed"),
            "expected variant in: {debug}"
        );
        assert!(debug.contains("api-service"), "expected name in: {debug}");
        assert!(debug.contains("timeout"), "expected error in: {debug}");
    }

    #[test]
    fn test_tailoring_event_batch_completed_debug_clone_eq() {
        let event = TailoringEvent::BatchCompleted {
            project_name: "my-app".to_string(),
            batch_index: 2,
            batch_count: 4,
            adr_count: 10,
            applied_count: 8,
            skipped_count: 2,
            skipped_adrs: vec![("adr-1".to_string(), "not applicable".to_string())],
            file_paths: vec!["CLAUDE.md".to_string()],
            elapsed_secs: 42,
        };
        let cloned = event.clone();
        assert_eq!(event, cloned);
        let debug = format!("{:?}", event);
        assert!(
            debug.contains("BatchCompleted"),
            "expected variant in: {debug}"
        );
        assert!(debug.contains("my-app"), "expected name in: {debug}");
        assert!(debug.contains('2'), "expected batch_index in: {debug}");
        assert!(debug.contains('4'), "expected batch_count in: {debug}");
    }

    #[test]
    fn test_tailoring_event_variants_not_equal() {
        let started = TailoringEvent::ProjectStarted {
            project_name: "app".to_string(),
            batch_count: 1,
        };
        let batch_started = TailoringEvent::BatchStarted {
            project_name: "app".to_string(),
            batch_index: 1,
            batch_count: 1,
            adr_count: 3,
            category_name: "Security".to_string(),
            adr_titles: vec![],
        };
        let batch_completed = TailoringEvent::BatchCompleted {
            project_name: "app".to_string(),
            batch_index: 1,
            batch_count: 1,
            adr_count: 5,
            applied_count: 5,
            skipped_count: 0,
            skipped_adrs: vec![],
            file_paths: vec![],
            elapsed_secs: 10,
        };
        let completed = TailoringEvent::ProjectCompleted {
            project_name: "app".to_string(),
            files_generated: 1,
            adrs_applied: 1,
            file_paths: vec![],
        };
        let failed = TailoringEvent::ProjectFailed {
            project_name: "app".to_string(),
            error: "err".to_string(),
        };
        assert_ne!(started, batch_started);
        assert_ne!(started, batch_completed);
        assert_ne!(started, completed);
        assert_ne!(started, failed);
        assert_ne!(batch_started, batch_completed);
        assert_ne!(batch_started, completed);
        assert_ne!(batch_started, failed);
        assert_ne!(batch_completed, completed);
        assert_ne!(batch_completed, failed);
        assert_ne!(completed, failed);
    }

    #[test]
    fn test_tailoring_event_batch_started_debug_clone_eq() {
        let event = TailoringEvent::BatchStarted {
            project_name: "my-app".to_string(),
            batch_index: 1,
            batch_count: 2,
            adr_count: 3,
            category_name: "Error Handling".to_string(),
            adr_titles: vec!["ADR 001".to_string(), "ADR 002".to_string()],
        };
        let cloned = event.clone();
        assert_eq!(event, cloned);
        let debug = format!("{:?}", event);
        assert!(
            debug.contains("BatchStarted"),
            "expected variant in: {debug}"
        );
        assert!(debug.contains("my-app"), "expected name in: {debug}");
        assert!(
            debug.contains("Error Handling"),
            "expected category_name in: {debug}"
        );
    }

    #[test]
    fn test_tailoring_event_batch_started_fields() {
        let category_name = "Security".to_string();
        let adr_titles = vec!["Use HTTPS".to_string(), "Validate inputs".to_string()];
        let event = TailoringEvent::BatchStarted {
            project_name: "repo".to_string(),
            batch_index: 2,
            batch_count: 3,
            adr_count: 2,
            category_name: category_name.clone(),
            adr_titles: adr_titles.clone(),
        };
        // Verify fields via equality with expected value
        let expected = TailoringEvent::BatchStarted {
            project_name: "repo".to_string(),
            batch_index: 2,
            batch_count: 3,
            adr_count: 2,
            category_name,
            adr_titles,
        };
        assert_eq!(event, expected);
    }

    #[test]
    fn test_deserialize_sample() {
        let json = r##"{
  "files": [
    {
      "path": "CLAUDE.md",
      "sections": [
        {"adr_id": "adr-001", "content": "# Project Rules\n\nUse consistent error handling patterns."},
        {"adr_id": "adr-002", "content": "Follow logging standards."}
      ],
      "reasoning": "Root-level rules applicable to the entire project"
    },
    {
      "path": "apps/web/CLAUDE.md",
      "sections": [
        {"adr_id": "adr-003", "content": "# Web App Rules\n\nUse React Server Components for data fetching."}
      ],
      "reasoning": "Web-specific frontend framework rules"
    }
  ],
  "skipped_adrs": [
    {
      "id": "adr-004",
      "reason": "This ADR applies to mobile apps, but no mobile project was detected"
    }
  ],
  "summary": {
    "total_input": 4,
    "applicable": 3,
    "not_applicable": 1,
    "files_generated": 2
  }
}"##;

        let output: TailoringOutput = serde_json::from_str(json).unwrap();

        assert_eq!(output.files.len(), 2);
        assert_eq!(output.files[0].path, "CLAUDE.md");
        assert_eq!(
            output.files[0].content(),
            "# Project Rules\n\nUse consistent error handling patterns.\n\nFollow logging standards."
        );
        assert_eq!(
            output.files[0].reasoning,
            "Root-level rules applicable to the entire project"
        );
        assert_eq!(output.files[0].adr_ids(), vec!["adr-001", "adr-002"]);

        assert_eq!(output.files[1].path, "apps/web/CLAUDE.md");
        assert_eq!(
            output.files[1].content(),
            "# Web App Rules\n\nUse React Server Components for data fetching."
        );
        assert_eq!(
            output.files[1].reasoning,
            "Web-specific frontend framework rules"
        );
        assert_eq!(output.files[1].adr_ids(), vec!["adr-003"]);

        assert_eq!(output.skipped_adrs.len(), 1);
        assert_eq!(output.skipped_adrs[0].id, "adr-004");
        assert_eq!(
            output.skipped_adrs[0].reason,
            "This ADR applies to mobile apps, but no mobile project was detected"
        );

        assert_eq!(output.summary.total_input, 4);
        assert_eq!(output.summary.applicable, 3);
        assert_eq!(output.summary.not_applicable, 1);
        assert_eq!(output.summary.files_generated, 2);
    }

    #[test]
    fn test_roundtrip() {
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                sections: vec![
                    AdrSection {
                        adr_id: "adr-001".to_string(),
                        content: "# Rules\n\nFollow coding standards.".to_string(),
                    },
                    AdrSection {
                        adr_id: "adr-002".to_string(),
                        content: "Use consistent naming.".to_string(),
                    },
                ],
                reasoning: "General project rules".to_string(),
            }],
            skipped_adrs: vec![SkippedAdr {
                id: "adr-005".to_string(),
                reason: "Not applicable to this tech stack".to_string(),
            }],
            summary: TailoringSummary {
                total_input: 3,
                applicable: 2,
                not_applicable: 1,
                files_generated: 1,
            },
        };

        let json = serde_json::to_string(&output).unwrap();
        let deserialized: TailoringOutput = serde_json::from_str(&json).unwrap();

        assert_eq!(output, deserialized);
    }

    #[test]
    fn test_subdirectory_paths() {
        let file_output = FileOutput {
            path: "apps/web/CLAUDE.md".to_string(),
            sections: vec![AdrSection {
                adr_id: "adr-010".to_string(),
                content: "# Web Rules".to_string(),
            }],
            reasoning: "Web-specific rules".to_string(),
        };

        let json = serde_json::to_string(&file_output).unwrap();
        let deserialized: FileOutput = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.path, "apps/web/CLAUDE.md");
        assert_eq!(file_output, deserialized);
    }
}
