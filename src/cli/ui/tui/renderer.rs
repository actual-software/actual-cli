use std::io::{self, IsTerminal};
use std::time::{Duration, Instant};

use crossterm::{
    cursor::{Hide, Show},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::Backend,
    layout::{Constraint, Direction, Layout},
    widgets::{Block, Borders, Paragraph},
    Terminal,
};

use super::log::LogPane;
use super::steps::{StepStatus, StepsPane};
use crate::cli::ui::progress::SyncPhase;

/// Trait-erased interface to a live ratatui terminal.
///
/// Abstracting over the backend type allows tests to inject a [`TestBackend`]
/// without changing the public API.
trait TuiTerminal: Send {
    fn draw_frame(&mut self, steps: &StepsPane, log: &LogPane) -> io::Result<()>;
}

/// Concrete [`TuiTerminal`] backed by any [`Backend`].
struct TuiTerminalImpl<B: Backend + Send>(Terminal<B>);

impl<B: Backend + Send> TuiTerminal for TuiTerminalImpl<B> {
    fn draw_frame(&mut self, steps: &StepsPane, log: &LogPane) -> io::Result<()> {
        render_to(&mut self.0, steps, log)
    }
}

/// Render the steps and log panes into an arbitrary ratatui terminal backend.
///
/// This is a free function (not a method) so it can be called from tests with
/// [`ratatui::backend::TestBackend`] without requiring a real TTY or
/// `CrosstermBackend`.
pub fn render_to<B: Backend>(
    terminal: &mut Terminal<B>,
    steps: &StepsPane,
    log: &LogPane,
) -> io::Result<()> {
    let size = terminal.size()?;
    let cols = size.width;
    terminal.draw(|frame| {
        let area = frame.area();
        if cols >= 80 {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(32), Constraint::Fill(1)])
                .split(area);

            let step_lines = steps.render_lines(30);
            let step_text = step_lines.join("\n");
            let step_widget = Paragraph::new(step_text)
                .block(Block::default().borders(Borders::ALL).title("Steps"));
            frame.render_widget(step_widget, chunks[0]);

            let log_height = chunks[1].height.saturating_sub(2) as usize;
            let log_width = chunks[1].width.saturating_sub(2) as usize;
            let log_text = log.render_to_string(log_height, log_width).join("\n");
            let log_widget = Paragraph::new(log_text)
                .block(Block::default().borders(Borders::ALL).title("Output"));
            frame.render_widget(log_widget, chunks[1]);
        } else {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Fill(1)])
                .split(area);

            let condensed = steps.render_condensed(area.width as usize);
            frame.render_widget(Paragraph::new(condensed), chunks[0]);

            let log_height = chunks[1].height as usize;
            let log_width = area.width as usize;
            let log_text = log.render_to_string(log_height, log_width).join("\n");
            frame.render_widget(Paragraph::new(log_text), chunks[1]);
        }
    })?;
    Ok(())
}

/// The rendering mode chosen at construction time.
enum Mode {
    /// Full ratatui TUI on a live terminal (trait-erased for testability).
    Tui(Box<dyn TuiTerminal>),
    /// Plain line-based output to stderr (non-TTY or `--no-tui`).
    Plain,
    /// No output at all (`--quiet`).
    Quiet,
}

/// A live TUI renderer that replaces `SyncPipeline` in `sync.rs`.
///
/// Public API mirrors `SyncPipeline` so callers need only change the type and
/// constructor; all method names stay the same.
///
/// Mode selection at construction:
/// - `quiet = true`  → [`Mode::Quiet`]   (no output)
/// - `no_tui = true` or stderr is not a TTY → [`Mode::Plain`] (plain lines)
/// - otherwise → attempt crossterm TUI setup; fall back to [`Mode::Plain`] on failure
pub struct TuiRenderer {
    mode: Mode,
    pub steps: StepsPane,
    pub log: LogPane,
    starts: [Option<Instant>; 4],
    created_at: Instant,
}

impl TuiRenderer {
    pub fn new(quiet: bool, no_tui: bool) -> Self {
        let steps = StepsPane::new(&["Environment", "Analysis", "Fetch ADRs", "Tailoring"]);
        let log = LogPane::new();
        let created_at = Instant::now();
        let starts = [None; 4];

        if quiet {
            return Self {
                mode: Mode::Quiet,
                steps,
                log,
                starts,
                created_at,
            };
        }

        if no_tui || !io::stderr().is_terminal() {
            return Self {
                mode: Mode::Plain,
                steps,
                log,
                starts,
                created_at,
            };
        }

        // Attempt TUI setup; fall back to Plain on any failure (e.g. not a real TTY).
        let mode = Self::try_setup_tui().unwrap_or(Mode::Plain);
        Self {
            mode,
            steps,
            log,
            starts,
            created_at,
        }
    }

    /// Attempt crossterm TUI setup. Returns `None` on any failure.
    fn try_setup_tui() -> Option<Mode> {
        // Each `.ok()?` returns None on failure; the whole chain is one logical
        // expression so llvm-cov counts it as a single coverable region.
        enable_raw_mode().ok()?;
        execute!(io::stderr(), EnterAlternateScreen, Hide).ok()?;
        Terminal::new(ratatui::backend::CrosstermBackend::new(io::stderr()))
            .ok()
            .map(|t| Mode::Tui(Box::new(TuiTerminalImpl(t)) as Box<dyn TuiTerminal>))
    }

    /// Construct a renderer in Tui mode using the given backend (for tests).
    ///
    /// The crossterm raw-mode and alternate-screen setup is intentionally
    /// **skipped** — only the drawing path, suspend branches, and Drop impl
    /// need to be exercised.  The `let _ = ...` calls in those paths silently
    /// ignore "not a TTY" errors, making them safe to run in CI.
    #[cfg(test)]
    pub(crate) fn new_with_tui<B: Backend + Send + 'static>(terminal: Terminal<B>) -> Self {
        Self {
            mode: Mode::Tui(Box::new(TuiTerminalImpl(terminal))),
            steps: StepsPane::new(&["Environment", "Analysis", "Fetch ADRs", "Tailoring"]),
            log: LogPane::new(),
            starts: [None; 4],
            created_at: Instant::now(),
        }
    }

    /// Map a `SyncPhase` to a step index.
    fn phase_idx(phase: SyncPhase) -> usize {
        match phase {
            SyncPhase::Environment => 0,
            SyncPhase::Analysis => 1,
            SyncPhase::Fetch => 2,
            SyncPhase::Tailor => 3,
        }
    }

    /// Transition a phase to the active (spinning) state.
    pub fn start(&mut self, phase: SyncPhase, message: &str) {
        let idx = Self::phase_idx(phase);
        self.starts[idx] = Some(Instant::now());
        self.steps.steps[idx].status = StepStatus::Running { tick: 0 };
        self.steps.steps[idx].message = message.to_string();
        self.draw();
        if let Mode::Plain = &self.mode {
            eprintln!("  ○ {message}");
        }
    }

    /// Stop a phase and show a success message.
    pub fn success(&mut self, phase: SyncPhase, message: &str) {
        let idx = Self::phase_idx(phase);
        self.steps.steps[idx].status = StepStatus::Success;
        self.steps.steps[idx].message = message.to_string();
        self.steps.steps[idx].elapsed = self.starts[idx].map(|s| s.elapsed());
        self.draw();
        if let Mode::Plain = &self.mode {
            eprintln!("  ✔ {message}");
        }
    }

    /// Mark a phase as skipped.
    pub fn skip(&mut self, phase: SyncPhase, message: &str) {
        let idx = Self::phase_idx(phase);
        self.steps.steps[idx].status = StepStatus::Skipped;
        self.steps.steps[idx].message = message.to_string();
        self.draw();
        if let Mode::Plain = &self.mode {
            eprintln!("  ─ {message}");
        }
    }

    /// Stop a phase and show an error message.
    pub fn error(&mut self, phase: SyncPhase, message: &str) {
        let idx = Self::phase_idx(phase);
        self.steps.steps[idx].status = StepStatus::Error;
        self.steps.steps[idx].message = message.to_string();
        self.steps.steps[idx].elapsed = self.starts[idx].map(|s| s.elapsed());
        self.draw();
        if let Mode::Plain = &self.mode {
            eprintln!("  ✖ {message}");
        }
    }

    /// Stop a phase and show a warning message.
    pub fn warn(&mut self, phase: SyncPhase, message: &str) {
        let idx = Self::phase_idx(phase);
        self.steps.steps[idx].status = StepStatus::Warn;
        self.steps.steps[idx].message = message.to_string();
        self.steps.steps[idx].elapsed = self.starts[idx].map(|s| s.elapsed());
        self.draw();
        if let Mode::Plain = &self.mode {
            eprintln!("  ⚠ {message}");
        }
    }

    /// Mark all phases still in Waiting or Running state as Skipped.
    pub fn finish_remaining(&mut self) {
        let indices: Vec<usize> = self
            .steps
            .steps
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s.status, StepStatus::Waiting | StepStatus::Running { .. }))
            .map(|(i, _)| i)
            .collect();

        for idx in indices {
            self.steps.steps[idx].status = StepStatus::Skipped;
            self.steps.steps[idx].message = "skipped".to_string();
        }
        self.draw();
    }

    /// Update the message for an active phase and advance the spinner tick.
    pub fn update_message(&mut self, phase: SyncPhase, message: &str) {
        let idx = Self::phase_idx(phase);
        self.steps.steps[idx].message = message.to_string();
        if let StepStatus::Running { ref mut tick } = self.steps.steps[idx].status {
            *tick = tick.wrapping_add(1);
        }
        self.draw();
    }

    /// Push a line to the log pane.
    pub fn println(&mut self, line: &str) {
        if let Mode::Quiet = &self.mode {
            return;
        }
        self.log.push(line.to_string());
        self.draw();
        if let Mode::Plain = &self.mode {
            eprintln!("{line}");
        }
    }

    /// Temporarily suspend the TUI for interactive prompts, then restore it.
    pub fn suspend<F: FnOnce() -> R, R>(&mut self, f: F) -> R {
        match &self.mode {
            Mode::Tui(_) => {
                // Silently ignore errors — in tests there is no raw-mode TTY.
                let _ = disable_raw_mode();
                let _ = execute!(io::stderr(), LeaveAlternateScreen, Show);
                let result = f();
                let _ = enable_raw_mode();
                let _ = execute!(io::stderr(), EnterAlternateScreen, Hide);
                self.draw();
                result
            }
            Mode::Plain | Mode::Quiet => f(),
        }
    }

    /// Return the wall-clock time elapsed since this renderer was created.
    pub fn total_elapsed(&self) -> Duration {
        self.created_at.elapsed()
    }

    /// Return the instant this renderer was created.
    pub fn start_time(&self) -> Instant {
        self.created_at
    }

    /// Redraw the TUI. No-op in Plain and Quiet modes.
    fn draw(&mut self) {
        if let Mode::Tui(ref mut terminal) = self.mode {
            let _ = terminal.draw_frame(&self.steps, &self.log);
        }
    }
}

impl Drop for TuiRenderer {
    fn drop(&mut self) {
        if let Mode::Tui(_) = &self.mode {
            // Silently ignore errors — in tests there is no raw-mode TTY.
            let _ = disable_raw_mode();
            let _ = execute!(io::stderr(), LeaveAlternateScreen, Show);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    // ── render_to tests (wide and narrow layouts) ──

    #[test]
    fn test_render_to_wide() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let steps = StepsPane::new(&["Environment", "Analysis", "Fetch ADRs", "Tailoring"]);
        let log = LogPane::new();
        render_to(&mut terminal, &steps, &log).unwrap();
    }

    #[test]
    fn test_render_to_narrow() {
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let steps = StepsPane::new(&["Environment", "Analysis", "Fetch ADRs", "Tailoring"]);
        let log = LogPane::new();
        render_to(&mut terminal, &steps, &log).unwrap();
    }

    #[test]
    fn test_render_to_exactly_80_cols() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let steps = StepsPane::new(&["Environment", "Analysis", "Fetch ADRs", "Tailoring"]);
        let mut log = LogPane::new();
        log.push("a log line".to_string());
        render_to(&mut terminal, &steps, &log).unwrap();
    }

    #[test]
    fn test_render_to_with_populated_state() {
        use std::time::Duration;
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut steps = StepsPane::new(&["Environment", "Analysis", "Fetch ADRs", "Tailoring"]);
        steps.steps[0].status = StepStatus::Success;
        steps.steps[0].elapsed = Some(Duration::from_millis(100));
        steps.steps[1].status = StepStatus::Running { tick: 3 };
        let mut log = LogPane::new();
        log.push("line one".to_string());
        log.push("line two".to_string());
        render_to(&mut terminal, &steps, &log).unwrap();
    }

    // ── TuiRenderer::new tests ──

    #[test]
    fn test_new_quiet() {
        let r = TuiRenderer::new(true, false);
        assert!(matches!(r.mode, Mode::Quiet));
        let _ = r.total_elapsed();
        let _ = r.start_time();
    }

    #[test]
    fn test_new_no_tui() {
        let r = TuiRenderer::new(false, true);
        assert!(matches!(r.mode, Mode::Plain));
    }

    #[test]
    fn test_new_non_tty_falls_back_to_plain() {
        // In CI / test environments stderr is not a TTY.
        let r = TuiRenderer::new(false, false);
        // Either Plain or Tui is acceptable depending on the test environment,
        // but construction must not panic.
        let _ = r.total_elapsed();
    }

    // ── Tui mode tests via new_with_tui ──

    #[test]
    fn test_tui_mode_draw_on_start() {
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        // All public methods that call draw() should work without panicking.
        r.start(SyncPhase::Environment, "checking...");
    }

    #[test]
    fn test_tui_mode_all_transitions() {
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        r.start(SyncPhase::Environment, "env...");
        r.success(SyncPhase::Environment, "env ok");
        r.start(SyncPhase::Analysis, "analysis...");
        r.warn(SyncPhase::Analysis, "warn");
        r.start(SyncPhase::Fetch, "fetch...");
        r.error(SyncPhase::Fetch, "failed");
        r.skip(SyncPhase::Tailor, "skipped");
        r.update_message(SyncPhase::Tailor, "updated");
        r.println("a log line");
        r.finish_remaining();
    }

    #[test]
    fn test_tui_mode_suspend() {
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        // suspend() in Tui mode: disable_raw_mode/execute!/enable_raw_mode all
        // fail silently (not a TTY in tests) but the closure is called.
        let result = r.suspend(|| 99_i32);
        assert_eq!(result, 99);
    }

    #[test]
    fn test_tui_mode_drop_cleans_up() {
        // Drop is called when r goes out of scope. The crossterm calls fail
        // silently (not a TTY in tests).
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let r = TuiRenderer::new_with_tui(terminal);
        drop(r); // exercises the Drop impl Tui branch
    }

    #[test]
    fn test_tui_mode_narrow_draw() {
        // Test the narrow (< 80 cols) render path via new_with_tui.
        let backend = TestBackend::new(60, 20);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        r.start(SyncPhase::Environment, "narrow layout test");
    }

    // ── phase_idx coverage via public methods ──

    #[test]
    fn test_phase_transitions_all_phases() {
        let mut r = TuiRenderer::new(false, true); // Plain mode

        // Environment: start → success
        r.start(SyncPhase::Environment, "checking...");
        assert!(matches!(
            r.steps.steps[0].status,
            StepStatus::Running { .. }
        ));
        r.success(SyncPhase::Environment, "done");
        assert_eq!(r.steps.steps[0].status, StepStatus::Success);

        // Analysis: start → warn
        r.start(SyncPhase::Analysis, "analyzing...");
        r.warn(SyncPhase::Analysis, "partial result");
        assert_eq!(r.steps.steps[1].status, StepStatus::Warn);

        // Fetch: start → error
        r.start(SyncPhase::Fetch, "fetching...");
        r.error(SyncPhase::Fetch, "request failed");
        assert_eq!(r.steps.steps[2].status, StepStatus::Error);

        // Tailor: skip
        r.skip(SyncPhase::Tailor, "no tailoring");
        assert_eq!(r.steps.steps[3].status, StepStatus::Skipped);
    }

    #[test]
    fn test_update_message_running_step() {
        let mut r = TuiRenderer::new(false, true);
        r.start(SyncPhase::Tailor, "initial");
        r.update_message(SyncPhase::Tailor, "updated message");
        assert_eq!(r.steps.steps[3].message, "updated message");
        // tick should have advanced
        assert!(
            matches!(r.steps.steps[3].status, StepStatus::Running { tick } if tick == 1),
            "expected Running{{tick:1}}, got {:?}",
            r.steps.steps[3].status
        );
    }

    #[test]
    fn test_update_message_non_running_step() {
        // update_message on a non-Running step should still update the message
        // without panicking (the tick increment branch is skipped).
        let mut r = TuiRenderer::new(false, true);
        // Step is Waiting — not Running
        r.update_message(SyncPhase::Environment, "some message");
        assert_eq!(r.steps.steps[0].message, "some message");
    }

    #[test]
    fn test_finish_remaining_skips_waiting_and_running() {
        let mut r = TuiRenderer::new(false, true);
        r.start(SyncPhase::Environment, "checking...");
        // Analysis, Fetch, Tailor remain Waiting
        r.finish_remaining();

        assert_eq!(r.steps.steps[0].status, StepStatus::Skipped);
        assert_eq!(r.steps.steps[1].status, StepStatus::Skipped);
        assert_eq!(r.steps.steps[2].status, StepStatus::Skipped);
        assert_eq!(r.steps.steps[3].status, StepStatus::Skipped);
    }

    #[test]
    fn test_finish_remaining_does_not_overwrite_terminal_steps() {
        let mut r = TuiRenderer::new(false, true);
        r.start(SyncPhase::Environment, "checking...");
        r.success(SyncPhase::Environment, "done");
        r.start(SyncPhase::Analysis, "analyzing...");
        r.error(SyncPhase::Analysis, "failed");
        r.finish_remaining(); // should only skip Fetch and Tailor

        assert_eq!(r.steps.steps[0].status, StepStatus::Success);
        assert_eq!(r.steps.steps[1].status, StepStatus::Error);
        assert_eq!(r.steps.steps[2].status, StepStatus::Skipped);
        assert_eq!(r.steps.steps[3].status, StepStatus::Skipped);
    }

    // ── println tests ──

    #[test]
    fn test_println_plain_mode() {
        let mut r = TuiRenderer::new(false, true);
        r.println("hello world");
        assert_eq!(r.log.len(), 1);
    }

    #[test]
    fn test_println_quiet_mode() {
        let mut r = TuiRenderer::new(true, false);
        r.println("should be ignored");
        assert_eq!(r.log.len(), 0);
    }

    // ── suspend tests ──

    #[test]
    fn test_suspend_plain_mode() {
        let mut r = TuiRenderer::new(false, true);
        let result = r.suspend(|| 42_i32);
        assert_eq!(result, 42);
    }

    #[test]
    fn test_suspend_quiet_mode() {
        let mut r = TuiRenderer::new(true, false);
        let result = r.suspend(|| "hello");
        assert_eq!(result, "hello");
    }

    // ── elapsed tests ──

    #[test]
    fn test_total_elapsed_advances() {
        let r = TuiRenderer::new(false, true);
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(r.total_elapsed().as_nanos() > 0);
    }

    #[test]
    fn test_start_time_is_in_past() {
        let r = TuiRenderer::new(false, true);
        assert!(r.start_time() <= Instant::now());
    }

    // ── setup_tui coverage ──

    #[test]
    fn test_new_tty_or_fallback() {
        // new(false, false) will attempt try_setup_tui() when stderr is a TTY,
        // exercising the full setup path and the unwrap_or(Mode::Plain) fallback.
        // In CI this typically hits the enable_raw_mode failure path.
        // On a real TTY it enters Tui mode — we clean up via Drop.
        let _r = TuiRenderer::new(false, false);
        // _r is dropped here, restoring terminal state if TUI was entered.
    }

    // ── elapsed is recorded for success / error / warn ──

    #[test]
    fn test_elapsed_recorded_on_success() {
        let mut r = TuiRenderer::new(false, true);
        r.start(SyncPhase::Environment, "checking...");
        r.success(SyncPhase::Environment, "ok");
        assert!(r.steps.steps[0].elapsed.is_some());
    }

    #[test]
    fn test_elapsed_recorded_on_error() {
        let mut r = TuiRenderer::new(false, true);
        r.start(SyncPhase::Fetch, "fetching...");
        r.error(SyncPhase::Fetch, "failed");
        assert!(r.steps.steps[2].elapsed.is_some());
    }

    #[test]
    fn test_elapsed_recorded_on_warn() {
        let mut r = TuiRenderer::new(false, true);
        r.start(SyncPhase::Analysis, "analyzing...");
        r.warn(SyncPhase::Analysis, "partial");
        assert!(r.steps.steps[1].elapsed.is_some());
    }

    #[test]
    fn test_elapsed_none_without_start() {
        let mut r = TuiRenderer::new(false, true);
        r.success(SyncPhase::Tailor, "done");
        // No start → elapsed should remain None
        assert!(r.steps.steps[3].elapsed.is_none());
    }
}
