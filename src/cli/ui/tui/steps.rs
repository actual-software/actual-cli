use std::time::Duration;

/// The spinner frames to cycle through for a running step.
const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

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
    /// Format: `{icon} {label:<12} {message}  {elapsed}`
    pub fn render_step(step: &Step, width: usize) -> String {
        let icon = Self::status_icon(step.status);
        let elapsed_str = match step.elapsed {
            Some(d) => format!(" [{:.1}s]", d.as_secs_f64()),
            None => String::new(),
        };
        let base = format!(
            "{} {:<12} {}{}",
            icon, step.label, step.message, elapsed_str
        );
        // Truncate to width (simple char-level truncation; no ANSI codes here)
        if base.chars().count() <= width {
            base
        } else {
            let truncated: String = base.chars().take(width.saturating_sub(1)).collect();
            format!("{truncated}…")
        }
    }

    /// Render all steps as a Vec of lines (one per step).
    pub fn render_lines(&self, width: usize) -> Vec<String> {
        self.steps
            .iter()
            .map(|s| Self::render_step(s, width))
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
        let rendered = StepsPane::render_step(&step, 200);
        assert!(rendered.contains('○'));
        assert!(rendered.contains("Env"));
        assert!(rendered.contains("ok"));
        // no elapsed bracket
        assert!(!rendered.contains('['));
    }

    // --- render_step: elapsed Some branch ---

    #[test]
    fn test_render_step_with_elapsed_no_truncation() {
        let step = Step {
            label: "Analysis".to_string(),
            status: StepStatus::Success,
            message: "done".to_string(),
            elapsed: Some(Duration::from_secs_f64(1.5)),
        };
        let rendered = StepsPane::render_step(&step, 200);
        assert!(rendered.contains('✔'));
        assert!(rendered.contains("Analysis"));
        assert!(rendered.contains("done"));
        assert!(rendered.contains("[1.5s]"));
    }

    // --- render_step: truncation branch ---

    #[test]
    fn test_render_step_truncation() {
        let step = Step {
            label: "Environment".to_string(),
            status: StepStatus::Running { tick: 0 },
            message: "loading dependencies".to_string(),
            elapsed: None,
        };
        // Use a very small width to force truncation
        let rendered = StepsPane::render_step(&step, 10);
        // Should end with ellipsis and be at most 10 visible chars
        assert!(rendered.ends_with('…'));
        assert!(rendered.chars().count() <= 10);
    }

    // --- render_step: width exactly matches (no truncation) ---

    #[test]
    fn test_render_step_exact_width() {
        let step = Step {
            label: "A".to_string(),
            status: StepStatus::Skipped,
            message: String::new(),
            elapsed: None,
        };
        let rendered = StepsPane::render_step(&step, 200);
        let char_count = rendered.chars().count();
        // Render again at exact width — should not truncate
        let rendered2 = StepsPane::render_step(&step, char_count);
        assert!(!rendered2.ends_with('…'));
    }

    // --- render_lines ---

    #[test]
    fn test_render_lines() {
        let pane = StepsPane::new(&["Env", "Fetch ADRs"]);
        let lines = pane.render_lines(200);
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
}
