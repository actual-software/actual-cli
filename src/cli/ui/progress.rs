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

/// Display labels for each phase, indexed by `SyncPhase` discriminant.
const PHASE_LABELS: [&str; PHASE_COUNT] = ["Environment", "Analysis", "Fetch ADRs", "Tailoring"];

/// A multi-phase progress display for the sync command.
///
/// All 4 phases are shown simultaneously: waiting → active → completed.
/// When `quiet` is true, all operations are no-ops (no output produced).
pub struct SyncPipeline {
    mp: Option<MultiProgress>,
    bars: [Option<ProgressBar>; PHASE_COUNT],
}

impl SyncPipeline {
    /// Create a new pipeline with all 4 phases in "waiting" state.
    /// If `quiet` is true, no output is produced.
    pub fn new(quiet: bool) -> Self {
        if quiet {
            return Self {
                mp: None,
                bars: [const { None }; PHASE_COUNT],
            };
        }

        let mp = MultiProgress::new();
        let waiting_style =
            ProgressStyle::with_template("  ○ {msg}").expect("invalid waiting template");

        let bars = std::array::from_fn(|i| {
            let bar = mp.add(ProgressBar::new_spinner());
            bar.set_style(waiting_style.clone());
            bar.set_message(format!("{}...", PHASE_LABELS[i]));
            Some(bar)
        });

        Self { mp: Some(mp), bars }
    }

    /// Get the bar for a phase, if present.
    fn bar(&self, phase: SyncPhase) -> Option<&ProgressBar> {
        self.bars[phase as usize].as_ref()
    }

    /// Transition a phase to the active (spinning) state.
    pub fn start(&self, phase: SyncPhase, message: &str) {
        if let Some(bar) = self.bar(phase) {
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
        if let Some(bar) = self.bar(phase) {
            bar.set_style(Self::finished_style());
            bar.finish_with_message(format!("{} {message}", style(SUCCESS_SYMBOL).green()));
        }
    }

    /// Mark a phase as skipped without starting it first.
    ///
    /// Use this when a phase is intentionally not executed (e.g. `--no-tailor`),
    /// as opposed to `success()` which implies the phase ran and succeeded.
    pub fn skip(&self, phase: SyncPhase, message: &str) {
        if let Some(bar) = self.bar(phase) {
            bar.set_style(Self::finished_style());
            bar.finish_with_message(format!("{} {message}", style("─").dim()));
        }
    }

    /// Stop a phase and show an error (red X) message.
    ///
    /// Switches the style to a plain `"  {msg}"` template so the rendered
    /// output is consistent regardless of whether `start()` was called first.
    pub fn error(&self, phase: SyncPhase, message: &str) {
        if let Some(bar) = self.bar(phase) {
            bar.set_style(Self::finished_style());
            bar.finish_with_message(format!("{} {message}", style(ERROR_SYMBOL).red()));
        }
    }

    /// Stop a phase and show a warning (yellow triangle) message.
    ///
    /// Switches the style to a plain `"  {msg}"` template so the rendered
    /// output is consistent regardless of whether `start()` was called first.
    pub fn warn(&self, phase: SyncPhase, message: &str) {
        if let Some(bar) = self.bar(phase) {
            bar.set_style(Self::finished_style());
            bar.finish_with_message(format!("{} {message}", style(WARN_SYMBOL).yellow()));
        }
    }

    /// Style used when a phase has completed (success, error, or warn).
    fn finished_style() -> ProgressStyle {
        ProgressStyle::with_template("  {msg}").expect("invalid finished template")
    }

    /// Style used when a phase was skipped.
    fn skipped_style() -> ProgressStyle {
        ProgressStyle::with_template("  ─ {msg}").expect("invalid skipped template")
    }

    /// Mark all unfinished phases as skipped.
    pub fn finish_remaining(&self) {
        for bar in self.bars.iter().flatten() {
            if !bar.is_finished() {
                bar.set_style(Self::skipped_style());
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

    /// Helper to unwrap a bar from the pipeline for test assertions.
    fn bar(pipeline: &SyncPipeline, phase: SyncPhase) -> &ProgressBar {
        pipeline.bars[phase as usize]
            .as_ref()
            .expect("expected bar to be Some")
    }

    #[test]
    fn test_sync_pipeline_quiet_mode() {
        let pipeline = SyncPipeline::new(true);
        assert!(
            pipeline.mp.is_none(),
            "expected mp to be None in quiet mode"
        );
        assert!(
            pipeline.bars.iter().all(|b| b.is_none()),
            "expected all bars to be None in quiet mode"
        );
        // All ops should be no-ops without panic
        pipeline.start(SyncPhase::Environment, "checking");
        pipeline.success(SyncPhase::Analysis, "done");
        pipeline.skip(SyncPhase::Fetch, "skipped");
        pipeline.error(SyncPhase::Fetch, "failed");
        pipeline.warn(SyncPhase::Tailor, "warning");
        pipeline.finish_remaining();
    }

    #[test]
    fn test_sync_pipeline_phases() {
        let pipeline = SyncPipeline::new(false);
        assert!(pipeline.mp.is_some(), "expected mp to be Some");
        assert_eq!(
            pipeline.bars.iter().filter(|b| b.is_some()).count(),
            4,
            "expected 4 phase bars"
        );

        // Start and succeed each phase
        pipeline.start(SyncPhase::Environment, "Checking environment...");
        pipeline.success(SyncPhase::Environment, "Environment OK");
        let msg = bar(&pipeline, SyncPhase::Environment).message().to_string();
        assert!(
            msg.contains("Environment OK"),
            "expected success msg in: {msg}"
        );
        assert!(bar(&pipeline, SyncPhase::Environment).is_finished());

        pipeline.start(SyncPhase::Analysis, "Analyzing...");
        pipeline.success(SyncPhase::Analysis, "Analysis complete");
        assert!(bar(&pipeline, SyncPhase::Analysis).is_finished());

        pipeline.start(SyncPhase::Fetch, "Fetching...");
        pipeline.success(SyncPhase::Fetch, "Fetched 5 ADRs");
        assert!(bar(&pipeline, SyncPhase::Fetch).is_finished());

        pipeline.start(SyncPhase::Tailor, "Tailoring...");
        pipeline.success(SyncPhase::Tailor, "Tailored 5 into 3");
        assert!(bar(&pipeline, SyncPhase::Tailor).is_finished());
    }

    #[test]
    fn test_sync_pipeline_error() {
        let pipeline = SyncPipeline::new(false);
        pipeline.start(SyncPhase::Fetch, "Fetching...");
        pipeline.error(SyncPhase::Fetch, "API request failed");
        let msg = bar(&pipeline, SyncPhase::Fetch).message().to_string();
        assert!(
            msg.contains("API request failed"),
            "expected error msg in: {msg}"
        );
        assert!(bar(&pipeline, SyncPhase::Fetch).is_finished());
    }

    #[test]
    fn test_sync_pipeline_warn() {
        let pipeline = SyncPipeline::new(false);
        pipeline.start(SyncPhase::Environment, "Checking...");
        pipeline.warn(SyncPhase::Environment, "Not a git repo");
        let msg = bar(&pipeline, SyncPhase::Environment).message().to_string();
        assert!(
            msg.contains("Not a git repo"),
            "expected warn msg in: {msg}"
        );
        assert!(bar(&pipeline, SyncPhase::Environment).is_finished());
    }

    #[test]
    fn test_sync_pipeline_skip() {
        let pipeline = SyncPipeline::new(false);
        // skip() without start() — the intended semantic
        pipeline.skip(SyncPhase::Tailor, "Tailoring skipped (--no-tailor)");
        let msg = bar(&pipeline, SyncPhase::Tailor).message().to_string();
        assert!(
            msg.contains("Tailoring skipped"),
            "expected skip msg in: {msg}"
        );
        assert!(bar(&pipeline, SyncPhase::Tailor).is_finished());
    }

    #[test]
    fn test_sync_pipeline_finish_remaining() {
        let pipeline = SyncPipeline::new(false);
        // Finish only the first phase
        pipeline.start(SyncPhase::Environment, "Checking...");
        pipeline.success(SyncPhase::Environment, "OK");
        // Remaining 3 should be unfinished
        assert!(!bar(&pipeline, SyncPhase::Analysis).is_finished());
        assert!(!bar(&pipeline, SyncPhase::Fetch).is_finished());
        assert!(!bar(&pipeline, SyncPhase::Tailor).is_finished());

        pipeline.finish_remaining();

        // All should now be finished
        for (i, slot) in pipeline.bars.iter().enumerate() {
            let b = slot.as_ref().expect("expected bar to be Some");
            assert!(b.is_finished(), "expected bar {i} to be finished");
        }
        // Skipped bars should show "skipped" in message
        let msg = bar(&pipeline, SyncPhase::Analysis).message().to_string();
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
