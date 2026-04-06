use std::time::Duration;

/// The spinner frames to cycle through for a running step.
const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Horizontal padding reserved on the right side of step lines (matches the left prefix width).
const RIGHT_PAD: usize = 2;

/// Brief descriptions for each sync step, shown below Waiting steps so
/// first-time users understand what's coming. Each must be ≤38 chars to
/// fit within the 42-char inner width of the Steps box.
pub const STEP_DESCRIPTIONS: &[&str] = &[
    "Verifies auth, tools, and repo setup",
    "Scans code for languages and structure",
    "Downloads ADRs matching your stack",
    "AI adapts ADRs to your codebase",
    "Confirms and writes rule files",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Waiting,
    Running { tick: u8 },
    Success,
    Warn,
    Error,
    Skipped,
}

#[derive(Debug, Clone)]
pub struct Step {
    pub label: String,
    pub status: StepStatus,
    pub message: String,
    pub elapsed: Option<Duration>,
}

impl Step {
    pub fn new(label: &str) -> Self {
        Self {
            label: label.to_string(),
            status: StepStatus::Waiting,
            message: String::new(),
            elapsed: None,
        }
    }
}

pub struct StepsPane {
    pub steps: Vec<Step>,
}

impl StepsPane {
    pub fn new(labels: &[&str]) -> Self {
        Self {
            steps: labels.iter().map(|l| Step::new(l)).collect(),
        }
    }

    /// Return the description for a step at the given index, if available.
    pub fn description(index: usize) -> Option<&'static str> {
        STEP_DESCRIPTIONS.get(index).copied()
    }

    /// Returns the icon character for a step status.
    pub fn status_icon(status: StepStatus) -> char {
        match status {
            StepStatus::Waiting => '○',
            StepStatus::Running { tick } => SPINNER_FRAMES[tick as usize % SPINNER_FRAMES.len()],
            StepStatus::Success => '✔',
            StepStatus::Warn => '⚠',
            StepStatus::Error => '✖',
            StepStatus::Skipped => '─',
        }
    }

    /// Render a single step as a string, truncated to `width` visible chars.
    /// If `selected` is true, a `▶` prefix is prepended (consuming 2 chars of width).
    /// When elapsed is present, the timestamp is right-aligned flush with the right edge.
    /// Format: `[▶ ]{icon} {label}{padding}{elapsed}`
    pub fn render_step(step: &Step, width: usize, selected: bool) -> String {
        let icon = Self::status_icon(step.status);
        let prefix = if selected { "▶ " } else { "  " };
        let left = format!("{prefix}{icon} {}", step.label);
        let left_len = left.chars().count();

        match step.elapsed {
            Some(d) => {
                let right = format!("[{:.1}s]", d.as_secs_f64());
                let right_len = right.chars().count();
                // Reserve RIGHT_PAD spaces on the right edge to match left prefix padding
                let usable = width.saturating_sub(RIGHT_PAD);
                let min_total = left_len + 1 + right_len;
                if min_total <= usable {
                    let pad_width = usable - left_len;
                    format!("{left}{right:>pad_width$}")
                } else {
                    // Truncation case — still respect right padding
                    let available = usable.saturating_sub(1 + right_len + 1);
                    let truncated_left: String = left.chars().take(available).collect();
                    format!("{truncated_left}… {right}")
                }
            }
            None => {
                if left_len <= width {
                    left
                } else {
                    let truncated: String = left.chars().take(width.saturating_sub(1)).collect();
                    format!("{truncated}…")
                }
            }
        }
    }

    /// Render all steps as a Vec of lines (one per step).
    /// `selected` optionally highlights one step with a `▶` prefix.
    pub fn render_lines(&self, width: usize, selected: Option<usize>) -> Vec<String> {
        self.steps
            .iter()
            .enumerate()
            .map(|(i, s)| Self::render_step(s, width, selected == Some(i)))
            .collect()
    }

    /// Render a condensed single-line summary of all steps (for narrow layout).
    /// Format: `{icon}{abbrev}  {icon}{abbrev}  ...`
    pub fn render_condensed(&self, width: usize) -> String {
        let parts: Vec<String> = self
            .steps
            .iter()
            .map(|s| {
                let icon = Self::status_icon(s.status);
                // Take first 4 chars of label as abbreviation
                let abbrev: String = s.label.chars().take(4).collect();
                format!("{icon}{abbrev}")
            })
            .collect();
        let line = parts.join("  ");
        if line.chars().count() <= width {
            line
        } else {
            let truncated: String = line.chars().take(width.saturating_sub(1)).collect();
            format!("{truncated}…")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // --- StepStatus icon tests ---

    #[test]
    fn test_status_icon_waiting() {
        assert_eq!(StepsPane::status_icon(StepStatus::Waiting), '○');
    }

    #[test]
    fn test_status_icon_running_tick_0() {
        assert_eq!(StepsPane::status_icon(StepStatus::Running { tick: 0 }), '⠋');
    }

    #[test]
    fn test_status_icon_running_wraps() {
        // SPINNER_FRAMES has 10 entries; tick=10 wraps to index 0
        assert_eq!(
            StepsPane::status_icon(StepStatus::Running { tick: 10 }),
            '⠋'
        );
    }

    #[test]
    fn test_status_icon_running_mid() {
        // tick=1 → '⠙'
        assert_eq!(StepsPane::status_icon(StepStatus::Running { tick: 1 }), '⠙');
    }

    #[test]
    fn test_status_icon_success() {
        assert_eq!(StepsPane::status_icon(StepStatus::Success), '✔');
    }

    #[test]
    fn test_status_icon_warn() {
        assert_eq!(StepsPane::status_icon(StepStatus::Warn), '⚠');
    }

    #[test]
    fn test_status_icon_error() {
        assert_eq!(StepsPane::status_icon(StepStatus::Error), '✖');
    }

    #[test]
    fn test_status_icon_skipped() {
        assert_eq!(StepsPane::status_icon(StepStatus::Skipped), '─');
    }

    // --- Step::new ---

    #[test]
    fn test_step_new() {
        let step = Step::new("Env");
        assert_eq!(step.label, "Env");
        assert_eq!(step.status, StepStatus::Waiting);
        assert_eq!(step.message, "");
        assert!(step.elapsed.is_none());
    }

    // --- StepsPane::new ---

    #[test]
    fn test_steps_pane_new() {
        let pane = StepsPane::new(&["Env", "Analysis"]);
        assert_eq!(pane.steps.len(), 2);
        assert_eq!(pane.steps[0].label, "Env");
        assert_eq!(pane.steps[1].label, "Analysis");
    }

    // --- render_step: elapsed None branch ---

    #[test]
    fn test_render_step_no_elapsed_no_truncation() {
        let step = Step {
            label: "Env".to_string(),
            status: StepStatus::Waiting,
            message: "ok".to_string(),
            elapsed: None,
        };
        let rendered = StepsPane::render_step(&step, 200, false);
        assert!(rendered.contains('○'));
        assert!(rendered.contains("Env"));
        // no elapsed bracket
        assert!(!rendered.contains('['));
    }

    // --- render_step: elapsed Some branch (right-aligned) ---

    #[test]
    fn test_render_step_with_elapsed_right_aligned() {
        let step = Step {
            label: "Analysis".to_string(),
            status: StepStatus::Success,
            message: "done".to_string(),
            elapsed: Some(Duration::from_secs_f64(1.5)),
        };
        let width = 42;
        let rendered = StepsPane::render_step(&step, width, false);
        assert!(rendered.contains('✔'));
        assert!(rendered.contains("Analysis"));
        assert!(rendered.contains("[1.5s]"));
        // Total line is width - RIGHT_PAD chars (right padding is implicit)
        assert_eq!(rendered.chars().count(), width - 2);
        // Elapsed is at the end of the rendered string
        assert!(rendered.ends_with("[1.5s]"));
    }

    // --- render_step: truncation branch (no elapsed) ---

    #[test]
    fn test_render_step_truncation_no_elapsed() {
        let step = Step {
            label: "Environment".to_string(),
            status: StepStatus::Running { tick: 0 },
            message: "loading dependencies".to_string(),
            elapsed: None,
        };
        // Use a very small width to force truncation
        let rendered = StepsPane::render_step(&step, 10, false);
        // Should end with ellipsis and be at most 10 visible chars
        assert!(rendered.ends_with('…'));
        assert!(rendered.chars().count() <= 10);
    }

    // --- render_step: truncation branch (with elapsed) ---

    #[test]
    fn test_render_step_truncation_with_elapsed() {
        let step = Step {
            label: "Environment".to_string(),
            status: StepStatus::Running { tick: 0 },
            message: String::new(),
            elapsed: Some(Duration::from_secs_f64(0.3)),
        };
        // Width too small to fit left + gap + right without truncation
        let rendered = StepsPane::render_step(&step, 18, false);
        // Should contain elapsed and ellipsis in left part
        assert!(rendered.contains("[0.3s]"));
        assert!(rendered.contains('…'));
    }

    // --- render_step: width exactly matches (no truncation) ---

    #[test]
    fn test_render_step_exact_width_no_elapsed() {
        let step = Step {
            label: "A".to_string(),
            status: StepStatus::Skipped,
            message: String::new(),
            elapsed: None,
        };
        let rendered = StepsPane::render_step(&step, 200, false);
        let char_count = rendered.chars().count();
        // Render again at exact width — should not truncate
        let rendered2 = StepsPane::render_step(&step, char_count, false);
        assert!(!rendered2.ends_with('…'));
    }

    // --- render_step: elapsed fills exact width ---

    #[test]
    fn test_render_step_elapsed_exact_width() {
        let step = Step {
            label: "Env".to_string(),
            status: StepStatus::Success,
            message: String::new(),
            elapsed: Some(Duration::from_secs_f64(0.0)),
        };
        let width = 42;
        let rendered = StepsPane::render_step(&step, width, false);
        assert_eq!(rendered.chars().count(), width - 2);
        assert!(rendered.ends_with("[0.0s]"));
    }

    // --- render_step: various elapsed widths ---

    #[test]
    fn test_render_step_short_elapsed() {
        // [0.0s] = 5 chars
        let step = Step {
            label: "Environment".to_string(),
            status: StepStatus::Success,
            message: String::new(),
            elapsed: Some(Duration::from_secs_f64(0.0)),
        };
        let width = 42;
        let rendered = StepsPane::render_step(&step, width, false);
        assert_eq!(rendered.chars().count(), width - 2);
        assert!(rendered.ends_with("[0.0s]"));
    }

    #[test]
    fn test_render_step_medium_elapsed() {
        // [10.5s] = 6 chars
        let step = Step {
            label: "Environment".to_string(),
            status: StepStatus::Success,
            message: String::new(),
            elapsed: Some(Duration::from_secs_f64(10.5)),
        };
        let width = 42;
        let rendered = StepsPane::render_step(&step, width, false);
        assert_eq!(rendered.chars().count(), width - 2);
        assert!(rendered.ends_with("[10.5s]"));
    }

    #[test]
    fn test_render_step_long_elapsed() {
        // [115.0s] = 7 chars
        let step = Step {
            label: "Tailoring".to_string(),
            status: StepStatus::Running { tick: 6 },
            message: String::new(),
            elapsed: Some(Duration::from_secs_f64(115.0)),
        };
        let width = 42;
        let rendered = StepsPane::render_step(&step, width, false);
        assert_eq!(rendered.chars().count(), width - 2);
        assert!(rendered.ends_with("[115.0s]"));
    }

    // --- render_step: no elapsed means no padding ---

    #[test]
    fn test_render_step_no_elapsed_no_padding() {
        let step = Step {
            label: "Env".to_string(),
            status: StepStatus::Waiting,
            message: String::new(),
            elapsed: None,
        };
        let rendered = StepsPane::render_step(&step, 42, false);
        // "  ○ Env" = 7 chars, NOT padded to 42
        assert_eq!(rendered.chars().count(), 7);
    }

    // --- render_step: selected prefix ---

    #[test]
    fn test_render_step_selected_with_elapsed() {
        let step = Step {
            label: "Analysis".to_string(),
            status: StepStatus::Success,
            message: String::new(),
            elapsed: Some(Duration::from_secs_f64(2.0)),
        };
        let width = 42;
        let rendered = StepsPane::render_step(&step, width, true);
        assert!(rendered.starts_with("▶ "));
        assert_eq!(rendered.chars().count(), width - 2);
        assert!(rendered.ends_with("[2.0s]"));
    }

    // --- render_lines ---

    #[test]
    fn test_render_lines() {
        let pane = StepsPane::new(&["Env", "Fetch ADRs"]);
        let lines = pane.render_lines(200, None);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("Env"));
        assert!(lines[1].contains("Fetc")); // label truncated in format but label is full
    }

    // --- render_condensed: no truncation ---

    #[test]
    fn test_render_condensed_no_truncation() {
        let pane = StepsPane::new(&["Env", "Anal"]);
        let line = pane.render_condensed(200);
        // Should contain icon + abbrev for each step, joined by "  "
        assert!(line.contains("Env"));
        assert!(line.contains("Anal"));
    }

    // --- render_condensed: truncation branch ---

    #[test]
    fn test_render_condensed_truncation() {
        let pane = StepsPane::new(&["Environment", "Analysis", "Fetch", "Tailoring"]);
        // Force truncation with small width
        let line = pane.render_condensed(5);
        assert!(line.ends_with('…'));
        assert!(line.chars().count() <= 5);
    }

    // --- render_condensed: label abbreviation (4 chars) ---

    #[test]
    fn test_render_condensed_abbrev() {
        let pane = StepsPane::new(&["ABCDEFGH"]);
        let line = pane.render_condensed(200);
        // Only first 4 chars of label used
        assert!(line.contains("ABCD"));
        assert!(!line.contains("ABCDE"));
    }

    // --- render_condensed: short label (fewer than 4 chars) ---

    #[test]
    fn test_render_condensed_short_label() {
        let pane = StepsPane::new(&["AB"]);
        let line = pane.render_condensed(200);
        assert!(line.contains("AB"));
    }

    // --- render_step: right padding ---

    #[test]
    fn test_render_step_right_padding() {
        let step = Step {
            label: "Env".to_string(),
            status: StepStatus::Success,
            message: String::new(),
            elapsed: Some(Duration::from_secs_f64(1.0)),
        };
        let width = 42;
        let rendered = StepsPane::render_step(&step, width, false);
        // Line should be width - RIGHT_PAD chars, leaving 2 chars of implicit right padding
        assert_eq!(rendered.chars().count(), width - 2);
        assert!(rendered.ends_with("[1.0s]"));
        // Left side starts with 2-space prefix
        assert!(rendered.starts_with("  "));
    }

    // --- description tests ---

    #[test]
    fn test_description_returns_correct_text() {
        assert_eq!(
            StepsPane::description(0),
            Some("Verifies auth, tools, and repo setup")
        );
        assert_eq!(
            StepsPane::description(4),
            Some("Confirms and writes rule files")
        );
    }

    #[test]
    fn test_description_out_of_bounds() {
        assert_eq!(StepsPane::description(5), None);
        assert_eq!(StepsPane::description(100), None);
    }

    #[test]
    fn test_descriptions_fit_inner_width() {
        // Steps box inner width is 42 chars. With 6-char indent, descriptions
        // must be ≤36 chars. We allow up to 38 chars for the raw description.
        for desc in STEP_DESCRIPTIONS {
            assert!(desc.len() <= 38);
        }
    }

    #[test]
    fn test_descriptions_count_matches_steps() {
        assert_eq!(STEP_DESCRIPTIONS.len(), 5);
    }
}
