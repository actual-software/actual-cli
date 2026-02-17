use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::time::Duration;

/// Symbols for terminal status output
pub const SUCCESS_SYMBOL: &str = "✔";
pub const ERROR_SYMBOL: &str = "✖";
pub const WARN_SYMBOL: &str = "⚠";

/// Identifies a phase in the sync pipeline.
#[derive(Debug, Clone, Copy)]
pub enum SyncPhase {
    Environment = 0,
    Analysis = 1,
    Fetch = 2,
    Tailor = 3,
}

/// Number of phases in the sync pipeline.
const PHASE_COUNT: usize = SyncPhase::Tailor as usize + 1;

/// A multi-phase progress display for the sync command.
///
/// All 4 phases are shown simultaneously: waiting → active → completed.
/// When `quiet` is true, all operations are no-ops (no output produced).
pub struct SyncPipeline {
    mp: Option<MultiProgress>,
    bars: Vec<ProgressBar>,
}

impl SyncPipeline {
    /// Create a new pipeline with all 4 phases in "waiting" state.
    /// If `quiet` is true, no output is produced.
    pub fn new(quiet: bool) -> Self {
        if quiet {
            return Self {
                mp: None,
                bars: Vec::new(),
            };
        }

        let mp = MultiProgress::new();
        let labels: [&str; PHASE_COUNT] = ["Environment", "Analysis", "Fetch ADRs", "Tailoring"];
        let waiting_style =
            ProgressStyle::with_template("  ○ {msg}").expect("invalid waiting template");

        let bars: Vec<ProgressBar> = labels
            .iter()
            .map(|label| {
                let bar = mp.add(ProgressBar::new_spinner());
                bar.set_style(waiting_style.clone());
                bar.set_message(format!("{label}..."));
                bar
            })
            .collect();

        Self { mp: Some(mp), bars }
    }

    /// Transition a phase to the active (spinning) state.
    pub fn start(&self, phase: SyncPhase, message: &str) {
        if let Some(bar) = self.bars.get(phase as usize) {
            let spinning_style =
                ProgressStyle::with_template("{spinner} {msg}").expect("invalid spinner template");
            bar.set_style(spinning_style);
            bar.set_message(message.to_string());
            bar.enable_steady_tick(Duration::from_millis(80));
        }
    }

    /// Stop a phase and show a success (green checkmark) message.
    ///
    /// Switches the style to a plain `"  {msg}"` template so the rendered
    /// output is consistent regardless of whether `start()` was called first.
    pub fn success(&self, phase: SyncPhase, message: &str) {
        if let Some(bar) = self.bars.get(phase as usize) {
            bar.set_style(Self::finished_style());
            bar.finish_with_message(format!("{} {message}", style(SUCCESS_SYMBOL).green()));
        }
    }

    /// Stop a phase and show an error (red X) message.
    ///
    /// Switches the style to a plain `"  {msg}"` template so the rendered
    /// output is consistent regardless of whether `start()` was called first.
    pub fn error(&self, phase: SyncPhase, message: &str) {
        if let Some(bar) = self.bars.get(phase as usize) {
            bar.set_style(Self::finished_style());
            bar.finish_with_message(format!("{} {message}", style(ERROR_SYMBOL).red()));
        }
    }

    /// Stop a phase and show a warning (yellow triangle) message.
    ///
    /// Switches the style to a plain `"  {msg}"` template so the rendered
    /// output is consistent regardless of whether `start()` was called first.
    pub fn warn(&self, phase: SyncPhase, message: &str) {
        if let Some(bar) = self.bars.get(phase as usize) {
            bar.set_style(Self::finished_style());
            bar.finish_with_message(format!("{} {message}", style(WARN_SYMBOL).yellow()));
        }
    }

    /// Style used when a phase has completed (success, error, or warn).
    fn finished_style() -> ProgressStyle {
        ProgressStyle::with_template("  {msg}").expect("invalid finished template")
    }

    /// Mark all unfinished phases as skipped.
    pub fn finish_remaining(&self) {
        for bar in &self.bars {
            if !bar.is_finished() {
                let skipped_style =
                    ProgressStyle::with_template("  ─ {msg}").expect("invalid skipped template");
                bar.set_style(skipped_style);
                bar.finish_with_message("skipped".to_string());
            }
        }
    }

    /// Temporarily hide progress for interactive prompts.
    pub fn suspend<F: FnOnce() -> R, R>(&self, f: F) -> R {
        match &self.mp {
            Some(mp) => mp.suspend(f),
            None => f(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbol_constants() {
        assert_eq!(SUCCESS_SYMBOL, "✔", "expected green checkmark symbol");
        assert_eq!(ERROR_SYMBOL, "✖", "expected red X symbol");
        assert_eq!(WARN_SYMBOL, "⚠", "expected yellow triangle symbol");
    }

    // ── SyncPipeline tests ──

    #[test]
    fn test_sync_pipeline_quiet_mode() {
        let pipeline = SyncPipeline::new(true);
        assert!(
            pipeline.mp.is_none(),
            "expected mp to be None in quiet mode"
        );
        assert!(pipeline.bars.is_empty(), "expected no bars in quiet mode");
        // All ops should be no-ops without panic
        pipeline.start(SyncPhase::Environment, "checking");
        pipeline.success(SyncPhase::Analysis, "done");
        pipeline.error(SyncPhase::Fetch, "failed");
        pipeline.warn(SyncPhase::Tailor, "warning");
        pipeline.finish_remaining();
    }

    #[test]
    fn test_sync_pipeline_phases() {
        let pipeline = SyncPipeline::new(false);
        assert!(pipeline.mp.is_some(), "expected mp to be Some");
        assert_eq!(pipeline.bars.len(), 4, "expected 4 phase bars");

        // Start and succeed each phase
        pipeline.start(SyncPhase::Environment, "Checking environment...");
        pipeline.success(SyncPhase::Environment, "Environment OK");
        let msg = pipeline.bars[0].message().to_string();
        assert!(
            msg.contains("Environment OK"),
            "expected success msg in: {msg}"
        );
        assert!(pipeline.bars[0].is_finished());

        pipeline.start(SyncPhase::Analysis, "Analyzing...");
        pipeline.success(SyncPhase::Analysis, "Analysis complete");
        assert!(pipeline.bars[1].is_finished());

        pipeline.start(SyncPhase::Fetch, "Fetching...");
        pipeline.success(SyncPhase::Fetch, "Fetched 5 ADRs");
        assert!(pipeline.bars[2].is_finished());

        pipeline.start(SyncPhase::Tailor, "Tailoring...");
        pipeline.success(SyncPhase::Tailor, "Tailored 5 into 3");
        assert!(pipeline.bars[3].is_finished());
    }

    #[test]
    fn test_sync_pipeline_error() {
        let pipeline = SyncPipeline::new(false);
        pipeline.start(SyncPhase::Fetch, "Fetching...");
        pipeline.error(SyncPhase::Fetch, "API request failed");
        let msg = pipeline.bars[2].message().to_string();
        assert!(
            msg.contains("API request failed"),
            "expected error msg in: {msg}"
        );
        assert!(pipeline.bars[2].is_finished());
    }

    #[test]
    fn test_sync_pipeline_warn() {
        let pipeline = SyncPipeline::new(false);
        pipeline.start(SyncPhase::Environment, "Checking...");
        pipeline.warn(SyncPhase::Environment, "Not a git repo");
        let msg = pipeline.bars[0].message().to_string();
        assert!(
            msg.contains("Not a git repo"),
            "expected warn msg in: {msg}"
        );
        assert!(pipeline.bars[0].is_finished());
    }

    #[test]
    fn test_sync_pipeline_finish_remaining() {
        let pipeline = SyncPipeline::new(false);
        // Finish only the first phase
        pipeline.start(SyncPhase::Environment, "Checking...");
        pipeline.success(SyncPhase::Environment, "OK");
        // Remaining 3 should be unfinished
        assert!(!pipeline.bars[1].is_finished());
        assert!(!pipeline.bars[2].is_finished());
        assert!(!pipeline.bars[3].is_finished());

        pipeline.finish_remaining();

        // All should now be finished
        for (i, bar) in pipeline.bars.iter().enumerate() {
            assert!(bar.is_finished(), "expected bar {i} to be finished");
        }
        // Skipped bars should show "skipped" in message
        let msg = pipeline.bars[1].message().to_string();
        assert!(msg.contains("skipped"), "expected 'skipped' in: {msg}");
    }

    #[test]
    fn test_sync_pipeline_suspend() {
        let pipeline = SyncPipeline::new(false);
        let result = pipeline.suspend(|| 42);
        assert_eq!(result, 42, "expected suspend to return closure value");
    }

    #[test]
    fn test_sync_pipeline_suspend_quiet() {
        let pipeline = SyncPipeline::new(true);
        let result = pipeline.suspend(|| "hello");
        assert_eq!(
            result, "hello",
            "expected suspend to return closure value in quiet mode"
        );
    }
}
