use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::cell::Cell;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use super::theme;

/// Style for phases in "waiting" state (not yet started).
static WAITING_STYLE: LazyLock<ProgressStyle> =
    LazyLock::new(|| ProgressStyle::with_template("  ○ {msg}").expect("invalid waiting template"));

/// Style for the active (spinning) state.
static SPINNING_STYLE: LazyLock<ProgressStyle> = LazyLock::new(|| {
    ProgressStyle::with_template("{spinner} {msg}").expect("invalid spinner template")
});

/// Style for completed phases (success, error, warn, skip).
static FINISHED_STYLE: LazyLock<ProgressStyle> =
    LazyLock::new(|| ProgressStyle::with_template("  {msg}").expect("invalid finished template"));

/// Identifies a phase in the sync pipeline.
#[derive(Debug, Clone, Copy)]
pub enum SyncPhase {
    Environment = 0,
    Analysis = 1,
    Fetch = 2,
    Tailor = 3,
}

/// Number of phases in the sync pipeline.
///
/// Keep in sync with the `SyncPhase` enum variants. Update this whenever
/// a variant is added or removed. The compile-time assert below catches
/// mismatches between this constant and the `PHASE_LABELS` array.
const PHASE_COUNT: usize = 4;

/// Display labels for each phase, indexed by `SyncPhase` discriminant.
/// The typed array `[&str; PHASE_COUNT]` ensures the label count matches
/// `PHASE_COUNT` at compile time — adding a variant without a label is an error.
const PHASE_LABELS: [&str; PHASE_COUNT] = ["Environment", "Analysis", "Fetch ADRs", "Tailoring"];

// Verify that the last variant's discriminant matches the expected count.
// Catches mismatches if a variant is added/removed without updating PHASE_COUNT.
const _: () = assert!(SyncPhase::Tailor as usize == PHASE_COUNT - 1);

/// A multi-phase progress display for the sync command.
///
/// All 4 phases are shown simultaneously: waiting → active → completed.
/// When `quiet` is true, all operations are no-ops (no output produced).
pub struct SyncPipeline {
    mp: Option<MultiProgress>,
    bars: [Option<ProgressBar>; PHASE_COUNT],
    starts: [Cell<Option<Instant>>; PHASE_COUNT],
    created_at: Instant,
}

impl SyncPipeline {
    /// Create a new pipeline with all 4 phases in "waiting" state.
    /// If `quiet` is true, no output is produced.
    pub fn new(quiet: bool) -> Self {
        if quiet {
            return Self {
                mp: None,
                bars: [const { None }; PHASE_COUNT],
                starts: [const { Cell::new(None) }; PHASE_COUNT],
                created_at: Instant::now(),
            };
        }

        let mp = MultiProgress::new();

        let bars = std::array::from_fn(|i| {
            let bar = mp.add(ProgressBar::new_spinner());
            bar.set_style(WAITING_STYLE.clone());
            bar.set_message(format!("{:<18}...", PHASE_LABELS[i]));
            Some(bar)
        });

        Self {
            mp: Some(mp),
            bars,
            starts: [const { Cell::new(None) }; PHASE_COUNT],
            created_at: Instant::now(),
        }
    }

    /// Get the bar for a phase, if present.
    fn bar(&self, phase: SyncPhase) -> Option<&ProgressBar> {
        self.bars[phase as usize].as_ref()
    }

    /// Compute the elapsed-time suffix for a phase (e.g. ` [0.3s]`).
    ///
    /// Returns an empty string if the phase was never started.
    fn elapsed_suffix(&self, phase: SyncPhase) -> String {
        match self.starts[phase as usize].get() {
            Some(start) => {
                let secs = start.elapsed().as_secs_f64();
                format!(" {}", theme::muted(format!("[{secs:.1}s]")))
            }
            None => String::new(),
        }
    }

    /// Return the wall-clock time since this pipeline was created.
    pub fn total_elapsed(&self) -> Duration {
        self.created_at.elapsed()
    }

    /// Return the instant this pipeline was created.
    ///
    /// Useful for deferring elapsed-time computation to the point where
    /// the result is actually rendered, so that confirmation prompts and
    /// disk writes are included in the total.
    pub fn start_time(&self) -> Instant {
        self.created_at
    }

    /// Transition a phase to the active (spinning) state.
    pub fn start(&self, phase: SyncPhase, message: &str) {
        self.starts[phase as usize].set(Some(Instant::now()));
        if let Some(bar) = self.bar(phase) {
            bar.set_style(SPINNING_STYLE.clone());
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
            let suffix = self.elapsed_suffix(phase);
            bar.set_style(FINISHED_STYLE.clone());
            bar.finish_with_message(format!(
                "{} {message}{suffix}",
                theme::success(&theme::SUCCESS)
            ));
        }
    }

    /// Mark a phase as skipped without starting it first.
    ///
    /// Use this when a phase is intentionally not executed (e.g. `--no-tailor`),
    /// as opposed to `success()` which implies the phase ran and succeeded.
    pub fn skip(&self, phase: SyncPhase, message: &str) {
        if let Some(bar) = self.bar(phase) {
            bar.set_style(FINISHED_STYLE.clone());
            bar.finish_with_message(format!("{} {message}", style("─").dim()));
        }
    }

    /// Stop a phase and show an error (red X) message.
    ///
    /// Switches the style to a plain `"  {msg}"` template so the rendered
    /// output is consistent regardless of whether `start()` was called first.
    pub fn error(&self, phase: SyncPhase, message: &str) {
        if let Some(bar) = self.bar(phase) {
            let suffix = self.elapsed_suffix(phase);
            bar.set_style(FINISHED_STYLE.clone());
            bar.finish_with_message(format!("{} {message}{suffix}", theme::error(&theme::ERROR)));
        }
    }

    /// Stop a phase and show a warning (yellow triangle) message.
    ///
    /// Switches the style to a plain `"  {msg}"` template so the rendered
    /// output is consistent regardless of whether `start()` was called first.
    pub fn warn(&self, phase: SyncPhase, message: &str) {
        if let Some(bar) = self.bar(phase) {
            let suffix = self.elapsed_suffix(phase);
            bar.set_style(FINISHED_STYLE.clone());
            bar.finish_with_message(format!(
                "{} {message}{suffix}",
                theme::warning(&theme::WARN)
            ));
        }
    }

    /// Mark all unfinished phases as skipped.
    ///
    /// Uses the same visual treatment as [`skip()`]: a dim dash prefix
    /// rendered via `FINISHED_STYLE` with the symbol embedded in the message.
    pub fn finish_remaining(&self) {
        for bar in self.bars.iter().flatten() {
            if !bar.is_finished() {
                bar.set_style(FINISHED_STYLE.clone());
                bar.finish_with_message(format!("{} skipped", style("─").dim()));
            }
        }
    }

    /// Update the message displayed on an active (spinning) phase without
    /// changing its state.
    ///
    /// This is a no-op if the phase bar is not present (quiet mode). On a
    /// finished bar, `indicatif` silently ignores the `set_message` call.
    pub fn update_message(&self, phase: SyncPhase, message: &str) {
        if let Some(bar) = self.bar(phase) {
            bar.set_message(message.to_string());
        }
    }

    /// Print a line above the progress bars without disrupting the spinner.
    ///
    /// Uses [`indicatif::MultiProgress::println`] when in non-quiet mode,
    /// which flushes the line cleanly above all active bars.
    /// In quiet mode this is a no-op.
    pub fn println(&self, line: &str) {
        if let Some(mp) = &self.mp {
            let _ = mp.println(line);
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
        assert!(msg.contains('['), "expected timing bracket in: {msg}");
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
        assert!(msg.contains('['), "expected timing bracket in: {msg}");
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
        assert!(msg.contains('['), "expected timing bracket in: {msg}");
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
        assert!(!msg.contains('['), "skip should have no timing in: {msg}");
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
    fn test_sync_pipeline_update_message() {
        let pipeline = SyncPipeline::new(false);
        pipeline.start(SyncPhase::Tailor, "Tailoring ADRs (0/3)...");
        let msg_before = bar(&pipeline, SyncPhase::Tailor).message().to_string();
        assert!(
            msg_before.contains("0/3"),
            "expected initial count in: {msg_before}"
        );

        pipeline.update_message(SyncPhase::Tailor, "Tailoring ADRs (1/3)...");
        let msg_after = bar(&pipeline, SyncPhase::Tailor).message().to_string();
        assert!(
            msg_after.contains("1/3"),
            "expected updated count in: {msg_after}"
        );
    }

    #[test]
    fn test_sync_pipeline_update_message_quiet() {
        // In quiet mode update_message should be a no-op without panic
        let pipeline = SyncPipeline::new(true);
        pipeline.update_message(SyncPhase::Tailor, "should not panic");
    }

    #[test]
    fn test_sync_pipeline_update_message_finished_bar() {
        // update_message on a finished bar should be a no-op without panic
        let pipeline = SyncPipeline::new(false);
        pipeline.start(SyncPhase::Tailor, "Running...");
        pipeline.success(SyncPhase::Tailor, "Done");
        // After finish, set_message is a no-op in indicatif — should not panic
        pipeline.update_message(SyncPhase::Tailor, "this is ignored");
    }

    #[test]
    fn test_sync_pipeline_println() {
        // println should not panic and returns unit
        let pipeline = SyncPipeline::new(false);
        pipeline.println("project alpha done");
    }

    #[test]
    fn test_sync_pipeline_println_quiet() {
        // In quiet mode println is a no-op without panic
        let pipeline = SyncPipeline::new(true);
        pipeline.println("should not panic");
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

    #[test]
    fn test_phase_label_alignment() {
        let pipeline = SyncPipeline::new(false);
        // Start all 4 phases — initial messages should have consistent width
        for (i, phase) in [
            SyncPhase::Environment,
            SyncPhase::Analysis,
            SyncPhase::Fetch,
            SyncPhase::Tailor,
        ]
        .iter()
        .enumerate()
        {
            let msg = bar(&pipeline, *phase).message().to_string();
            // Each initial message is "{:<18}...", so the label is padded
            assert!(
                msg.contains("..."),
                "expected '...' in initial message {i}: {msg}"
            );
            // The label part (before "...") should be 18 chars (padded)
            let label_end = msg.find("...").unwrap();
            assert_eq!(
                label_end, 18,
                "expected 18-char padded label in phase {i}, got {label_end}: {msg}"
            );
        }
    }

    #[test]
    fn test_total_elapsed() {
        let pipeline = SyncPipeline::new(false);
        std::thread::sleep(Duration::from_millis(10));
        assert!(
            pipeline.total_elapsed() > Duration::ZERO,
            "expected total_elapsed > 0"
        );
    }

    #[test]
    fn test_total_elapsed_quiet() {
        let pipeline = SyncPipeline::new(true);
        std::thread::sleep(Duration::from_millis(10));
        assert!(
            pipeline.total_elapsed() > Duration::ZERO,
            "expected total_elapsed > 0 in quiet mode"
        );
    }

    #[test]
    fn test_timing_format() {
        let pipeline = SyncPipeline::new(false);
        pipeline.start(SyncPhase::Environment, "Checking...");
        pipeline.success(SyncPhase::Environment, "OK");
        let msg = bar(&pipeline, SyncPhase::Environment).message().to_string();
        // Should contain "[X.Xs]" pattern
        assert!(msg.contains('['), "expected '[' in: {msg}");
        assert!(msg.contains("s]"), "expected 's]' in: {msg}");
    }

    #[test]
    fn test_skip_no_timing() {
        let pipeline = SyncPipeline::new(false);
        // Skip without start — should have no timing bracket
        pipeline.skip(SyncPhase::Analysis, "Skipped");
        let msg = bar(&pipeline, SyncPhase::Analysis).message().to_string();
        assert!(msg.contains("Skipped"), "expected skip msg in: {msg}");
        assert!(!msg.contains('['), "skip should have no timing in: {msg}");
    }

    #[test]
    fn test_success_without_start_no_timing() {
        let pipeline = SyncPipeline::new(false);
        // success() without start() — elapsed_suffix should return empty
        pipeline.success(SyncPhase::Fetch, "Done");
        let msg = bar(&pipeline, SyncPhase::Fetch).message().to_string();
        assert!(msg.contains("Done"), "expected msg in: {msg}");
        assert!(!msg.contains('['), "no timing without start in: {msg}");
    }
}
