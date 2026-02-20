use std::io::{self, BufWriter, IsTerminal};
use std::time::{Duration, Instant};

use crossterm::{
    cursor::{Hide, Show},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::Backend,
    layout::{Constraint, Direction, Layout},
    widgets::{Block, Borders, Clear, Paragraph},
    Terminal,
};

use super::gradient::paint_gradient_border;
use super::log::LogPane;
use super::steps::{StepStatus, StepsPane};
use crate::analysis::confirm::ConfirmAction;
use crate::analysis::types::RepoAnalysis;
use crate::branding::banner::BANNER;
use crate::cli::ui::confirm::{format_project_summary, prompt_project_confirmation};
use crate::cli::ui::progress::SyncPhase;
use crate::cli::ui::term_size;
use crate::cli::ui::terminal::TerminalIO;

/// Abstraction over crossterm event reading so the event loop can be tested
/// without a real terminal.
pub(crate) trait EventSource {
    /// Block until the next key event and return it.
    fn next_key(&mut self) -> io::Result<crossterm::event::KeyEvent>;
}

/// Production event source that reads from crossterm.
pub(crate) struct CrosstermEventSource;

impl EventSource for CrosstermEventSource {
    fn next_key(&mut self) -> io::Result<crossterm::event::KeyEvent> {
        use crossterm::event::{read, Event};
        loop {
            match read()? {
                Event::Key(key) => return Ok(key),
                _ => continue,
            }
        }
    }
}

/// Which button has focus in the inline confirmation UI.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConfirmFocus {
    Accept,
    Reject,
}

/// State for the inline confirmation prompt rendered at the bottom of the log pane.
pub struct ConfirmState {
    pub question: String,
    pub focus: ConfirmFocus,
}

/// Rendering context passed to `render_to` and `draw_frame`.
pub struct RenderContext<'a> {
    pub steps: &'a StepsPane,
    pub log: &'a LogPane,
    pub confirm: Option<&'a ConfirmState>,
    pub hint: bool,
    pub scroll_offset: usize,
    pub selected: Option<usize>,
    pub version: &'a str,
}

/// Trait-erased interface to a live ratatui terminal.
///
/// Abstracting over the backend type allows tests to inject a [`TestBackend`]
/// without changing the public API.
trait TuiTerminal: Send {
    fn draw_frame(&mut self, ctx: RenderContext<'_>) -> io::Result<()>;
}

/// Concrete [`TuiTerminal`] backed by any [`Backend`].
struct TuiTerminalImpl<B: Backend + Send>(Terminal<B>);

impl<B: Backend + Send> TuiTerminal for TuiTerminalImpl<B> {
    fn draw_frame(&mut self, ctx: RenderContext<'_>) -> io::Result<()> {
        render_to(&mut self.0, ctx)
    }
}

fn append_confirm_lines(lines: &mut Vec<String>, cs: &ConfirmState) {
    lines.push(format!("  {}", cs.question));
    lines.push("  \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}".to_string());
    let (accept_label, reject_label) = match cs.focus {
        ConfirmFocus::Accept => ("[>Accept<]", "[ Reject ]"),
        ConfirmFocus::Reject => ("[ Accept ]", "[>Reject<]"),
    };
    lines.push(format!("  {accept_label}   {reject_label}"));
}

/// Width of the left panel (banner box + steps box).
///
/// The banner art is 37 chars wide; +2 for borders = 39. We add a few chars
/// of breathing room and round up to 44 so step messages are readable too.
const LEFT_COL_WIDTH: u16 = 44;

/// Number of banner lines + border rows for the banner box height.
/// BANNER has 11 lines + 1 blank above + 1 blank below + 2 border rows = 15.
const BANNER_BOX_HEIGHT: u16 = 15;

/// Render the steps and log panes into an arbitrary ratatui terminal backend.
///
/// This is a free function (not a method) so it can be called from tests with
/// [`ratatui::backend::TestBackend`] without requiring a real TTY or
/// `CrosstermBackend`.
pub fn render_to<B: Backend>(terminal: &mut Terminal<B>, ctx: RenderContext<'_>) -> io::Result<()> {
    let size = terminal.size()?;
    let cols = size.width;
    let use_color = console::colors_enabled_stderr();
    terminal.draw(|frame| {
        let area = frame.area();
        if cols >= 80 {
            // ── Horizontal split: left (banner + steps) | right (output) ──
            let h_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(LEFT_COL_WIDTH), Constraint::Fill(1)])
                .split(area);

            // ── Left column: vertical split — banner box on top, steps below ──
            let left_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(BANNER_BOX_HEIGHT), Constraint::Fill(1)])
                .split(h_chunks[0]);

            // Banner box (top-left)
            let inner_width = (LEFT_COL_WIDTH as usize).saturating_sub(2);
            let banner_lines: Vec<&str> = BANNER.lines().collect();
            let mut padded_banner: Vec<String> = Vec::with_capacity(banner_lines.len() + 2);
            padded_banner.push(String::new()); // blank line above art
            for l in &banner_lines {
                let chars: Vec<char> = l.chars().collect();
                let line = if chars.len() > inner_width {
                    chars[..inner_width].iter().collect::<String>()
                } else {
                    l.to_string()
                };
                // Center the line within inner_width
                let pad = inner_width.saturating_sub(line.chars().count()) / 2;
                padded_banner.push(format!("{}{}", " ".repeat(pad), line));
            }
            padded_banner.push(String::new()); // blank line below art
            let banner_text = padded_banner.join("\n");
            let version_title = format!(" actual v{} ", ctx.version);
            let banner_widget = Paragraph::new(banner_text).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(version_title.as_str()),
            );
            frame.render_widget(Clear, left_chunks[0]);
            frame.render_widget(banner_widget, left_chunks[0]);
            if use_color {
                paint_gradient_border(frame.buffer_mut(), left_chunks[0]);
            }

            // Steps box (bottom-left)
            let step_inner_width = (LEFT_COL_WIDTH as usize).saturating_sub(2);
            let step_lines = ctx.steps.render_lines(step_inner_width, ctx.selected);
            let step_text = step_lines.join("\n");
            let step_widget = Paragraph::new(step_text)
                .block(Block::default().borders(Borders::ALL).title("Steps"));
            frame.render_widget(Clear, left_chunks[1]);
            frame.render_widget(step_widget, left_chunks[1]);
            if use_color {
                paint_gradient_border(frame.buffer_mut(), left_chunks[1]);
            }

            // ── Right column: output pane ──
            // Reserve one line for the footer hint so it is always visible.
            let log_inner_height = h_chunks[1].height.saturating_sub(2) as usize;
            let footer_height: usize = if ctx.confirm.is_some() || ctx.hint {
                1
            } else {
                0
            };
            let log_height = log_inner_height.saturating_sub(footer_height);
            let log_width = h_chunks[1].width.saturating_sub(2) as usize;
            let mut log_lines = ctx
                .log
                .render_to_string(log_height, log_width, ctx.scroll_offset);
            if let Some(cs) = ctx.confirm {
                append_confirm_lines(&mut log_lines, cs);
            } else if ctx.hint {
                let max = ctx.log.max_scroll(log_height);
                let scroll_hint = if max > 0 {
                    format!(
                        "  ↑/↓ scroll  Home/End top/bottom  q quit  [{}/{}]",
                        ctx.scroll_offset.min(max),
                        max
                    )
                } else {
                    "  q to quit".to_string()
                };
                log_lines.push(scroll_hint);
            }
            let log_text = log_lines.join("\n");
            let log_widget = Paragraph::new(log_text)
                .block(Block::default().borders(Borders::ALL).title("Output"));
            frame.render_widget(Clear, h_chunks[1]);
            frame.render_widget(log_widget, h_chunks[1]);
            if use_color {
                paint_gradient_border(frame.buffer_mut(), h_chunks[1]);
            }
        } else {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Fill(1)])
                .split(area);

            let condensed = ctx.steps.render_condensed(area.width as usize);
            frame.render_widget(Clear, chunks[0]);
            frame.render_widget(Paragraph::new(condensed), chunks[0]);

            let log_inner_height = chunks[1].height as usize;
            let footer_height: usize = if ctx.confirm.is_some() || ctx.hint {
                1
            } else {
                0
            };
            let log_height = log_inner_height.saturating_sub(footer_height);
            let log_width = area.width as usize;
            let mut log_lines = ctx
                .log
                .render_to_string(log_height, log_width, ctx.scroll_offset);
            if let Some(cs) = ctx.confirm {
                append_confirm_lines(&mut log_lines, cs);
            } else if ctx.hint {
                let max = ctx.log.max_scroll(log_height);
                let scroll_hint = if max > 0 {
                    format!(
                        "  ↑/↓ scroll  Home/End top/bottom  q quit  [{}/{}]",
                        ctx.scroll_offset.min(max),
                        max
                    )
                } else {
                    "  q to quit".to_string()
                };
                log_lines.push(scroll_hint);
            }
            let log_text = log_lines.join("\n");
            frame.render_widget(Clear, chunks[1]);
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
    /// Per-step log buffers (index matches phase_idx).
    pub logs: [LogPane; 5],
    /// Which step's log currently receives `println` output.
    active_step: usize,
    starts: [Option<Instant>; 5],
    created_at: Instant,
    hint: bool,
    confirm_state: Option<ConfirmState>,
    /// Which step is highlighted in review mode (None = show active_step's log).
    selected_step: Option<usize>,
    /// Lines scrolled up from the bottom of the log pane (0 = pinned to bottom).
    scroll_offset: usize,
    /// Version string shown in the banner box title.
    version: String,
}

impl TuiRenderer {
    pub fn new(quiet: bool, no_tui: bool) -> Self {
        Self::new_with_version(quiet, no_tui, env!("CARGO_PKG_VERSION"))
    }

    pub fn new_with_version(quiet: bool, no_tui: bool, version: &str) -> Self {
        let steps = StepsPane::new(&[
            "Environment",
            "Analysis",
            "Fetch ADRs",
            "Tailoring",
            "Write Files",
        ]);
        let logs = [
            LogPane::new(),
            LogPane::new(),
            LogPane::new(),
            LogPane::new(),
            LogPane::new(),
        ];
        let created_at = Instant::now();
        let starts = [None; 5];

        let mode = if quiet {
            Mode::Quiet
        } else if no_tui || !io::stderr().is_terminal() {
            Mode::Plain
        } else {
            // Attempt TUI setup; fall back to Plain on any failure (e.g. not a real TTY).
            Self::try_setup_tui().unwrap_or(Mode::Plain)
        };
        Self {
            mode,
            steps,
            logs,
            active_step: 0,
            starts,
            created_at,
            hint: false,
            confirm_state: None,
            selected_step: None,
            scroll_offset: 0,
            version: version.to_string(),
        }
    }

    /// Attempt crossterm TUI setup. Returns `None` on any failure.
    ///
    /// The backend is wrapped in [`BufWriter`] so that ratatui's per-cell writes
    /// are buffered and flushed as a single atomic write per frame, eliminating
    /// visible tearing on slow terminals.
    fn try_setup_tui() -> Option<Mode> {
        enable_raw_mode().ok()?;
        execute!(io::stderr(), EnterAlternateScreen, Hide).ok()?;
        Terminal::new(ratatui::backend::CrosstermBackend::new(BufWriter::new(
            io::stderr(),
        )))
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
            steps: StepsPane::new(&[
                "Environment",
                "Analysis",
                "Fetch ADRs",
                "Tailoring",
                "Write Files",
            ]),
            logs: [
                LogPane::new(),
                LogPane::new(),
                LogPane::new(),
                LogPane::new(),
                LogPane::new(),
            ],
            active_step: 0,
            starts: [None; 5],
            created_at: Instant::now(),
            hint: false,
            confirm_state: None,
            selected_step: None,
            scroll_offset: 0,
            version: "0.0.0-test".to_string(),
        }
    }

    /// Return all log lines across all per-step buffers, concatenated.
    /// Useful in tests to assert content without knowing which step it went to.
    #[cfg(test)]
    pub fn all_log_text(&self) -> String {
        self.logs
            .iter()
            .flat_map(|l| l.render_to_string(10000, 1000, 0))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Map a `SyncPhase` to a step index.
    fn phase_idx(phase: SyncPhase) -> usize {
        match phase {
            SyncPhase::Environment => 0,
            SyncPhase::Analysis => 1,
            SyncPhase::Fetch => 2,
            SyncPhase::Tailor => 3,
            SyncPhase::Write => 4,
        }
    }

    /// Transition a phase to the active (spinning) state.
    pub fn start(&mut self, phase: SyncPhase, message: &str) {
        let idx = Self::phase_idx(phase);
        self.active_step = idx;
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

    /// Push a line to the active step's log pane.
    pub fn println(&mut self, line: &str) {
        if let Mode::Quiet = &self.mode {
            return;
        }
        // LogPane is rendered by ratatui which does not interpret raw ANSI escape
        // sequences — strip them before storing so they don't appear as literal text.
        let plain = console::strip_ansi_codes(line);
        self.logs[self.active_step].push(plain.into_owned());
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

    /// Show "Press any key to close" hint and block until a keypress in TUI mode.
    /// In Plain/Quiet mode: no-op (non-interactive sessions should not block).
    pub fn wait_for_keypress(&mut self) {
        self.wait_for_keypress_impl(&mut CrosstermEventSource);
    }

    fn wait_for_keypress_impl(&mut self, source: &mut dyn EventSource) {
        use crossterm::event::{KeyCode, KeyModifiers};
        if let Mode::Tui(_) = &self.mode {
            // Default to the most recently active step.
            self.selected_step = Some(self.active_step);
            self.hint = true;
            self.scroll_offset = 0;
            self.draw();
            loop {
                let Ok(key) = source.next_key() else {
                    break;
                };
                // Compute log_height matching render_to's formula exactly.
                let log_height: usize = crossterm::terminal::size()
                    .map(|(cols, rows)| {
                        let rows = rows as usize;
                        if cols >= 80 {
                            rows.saturating_sub(3)
                        } else {
                            rows.saturating_sub(1)
                        }
                    })
                    .unwrap_or(20);
                let n_steps = self.steps.steps.len();
                let cur = self.selected_step.unwrap_or(self.active_step);
                let log_idx = self.selected_step.unwrap_or(self.active_step);
                let max = self.logs[log_idx].max_scroll(log_height);
                match (key.code, key.modifiers) {
                    // Quit
                    (KeyCode::Char('q'), _)
                    | (KeyCode::Char('Q'), _)
                    | (KeyCode::Esc, _)
                    | (KeyCode::Enter, _) => break,
                    (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => break,
                    // Navigate to previous step (↑)
                    (KeyCode::Up, _) => {
                        self.selected_step = Some(cur.saturating_sub(1));
                        self.scroll_offset = 0;
                        self.draw();
                    }
                    // Navigate to next step (↓)
                    (KeyCode::Down, _) => {
                        self.selected_step = Some((cur + 1).min(n_steps - 1));
                        self.scroll_offset = 0;
                        self.draw();
                    }
                    // Scroll output up within selected step (PgUp)
                    (KeyCode::PageUp, _) | (KeyCode::Char('u'), _) => {
                        self.scroll_offset = (self.scroll_offset + log_height / 2).min(max);
                        self.draw();
                    }
                    // Scroll output down within selected step (PgDn)
                    (KeyCode::PageDown, _) | (KeyCode::Char('d'), _) => {
                        self.scroll_offset = self.scroll_offset.saturating_sub(log_height / 2);
                        self.draw();
                    }
                    // Jump to top of output
                    (KeyCode::Home, _) | (KeyCode::Char('g'), _) => {
                        self.scroll_offset = max;
                        self.draw();
                    }
                    // Jump to bottom of output
                    (KeyCode::End, _) | (KeyCode::Char('G'), _) => {
                        self.scroll_offset = 0;
                        self.draw();
                    }
                    _ => {}
                }
            }
            self.hint = false;
            self.scroll_offset = 0;
            self.selected_step = None;
        }
    }

    /// Show the project confirmation prompt.
    /// In TUI mode: renders inline buttons in the log pane.
    /// In Plain/Quiet mode: delegates to the existing terminal prompt.
    pub fn confirm_project(
        &mut self,
        analysis: &RepoAnalysis,
        term: &dyn TerminalIO,
    ) -> Result<ConfirmAction, crate::error::ActualError> {
        self.confirm_project_impl(analysis, term, &mut CrosstermEventSource)
    }

    fn confirm_project_impl(
        &mut self,
        analysis: &RepoAnalysis,
        term: &dyn TerminalIO,
        source: &mut dyn EventSource,
    ) -> Result<ConfirmAction, crate::error::ActualError> {
        match &self.mode {
            Mode::Tui(_) => {
                // Push the project summary to the log pane
                let width = term_size::terminal_width();
                let summary = format_project_summary(analysis, width);
                for line in summary.lines() {
                    self.logs[self.active_step].push(line.to_string());
                }
                // Set up confirm state
                self.confirm_state = Some(ConfirmState {
                    question: "Proceed with sync?".to_string(),
                    focus: ConfirmFocus::Accept,
                });
                self.draw();
                // Event loop
                let result = self.run_confirm_loop(source);
                self.confirm_state = None;
                self.draw();
                result
            }
            Mode::Plain | Mode::Quiet => prompt_project_confirmation(analysis, term),
        }
    }

    fn run_confirm_loop(
        &mut self,
        source: &mut dyn EventSource,
    ) -> Result<ConfirmAction, crate::error::ActualError> {
        use crossterm::event::{KeyCode, KeyModifiers};
        loop {
            let key = source.next_key().map_err(|e| {
                crate::error::ActualError::InternalError(format!("event read failed: {e}"))
            })?;
            match (key.code, key.modifiers) {
                (KeyCode::Enter, _) => {
                    let focus = self
                        .confirm_state
                        .as_ref()
                        .map(|cs| cs.focus)
                        .unwrap_or(ConfirmFocus::Accept);
                    return if focus == ConfirmFocus::Accept {
                        Ok(ConfirmAction::Accept)
                    } else {
                        Ok(ConfirmAction::Reject)
                    };
                }
                (KeyCode::Left, _) | (KeyCode::BackTab, _) => {
                    if let Some(ref mut cs) = self.confirm_state {
                        cs.focus = ConfirmFocus::Accept;
                    }
                    self.draw();
                }
                (KeyCode::Right, _) | (KeyCode::Tab, _) => {
                    if let Some(ref mut cs) = self.confirm_state {
                        cs.focus = ConfirmFocus::Reject;
                    }
                    self.draw();
                }
                (KeyCode::Char('y'), _) | (KeyCode::Char('Y'), _) => {
                    return Ok(ConfirmAction::Accept);
                }
                (KeyCode::Char('n'), _) | (KeyCode::Char('N'), _) => {
                    return Ok(ConfirmAction::Reject);
                }
                (KeyCode::Char('q'), _) | (KeyCode::Char('Q'), _) | (KeyCode::Esc, _) => {
                    return Ok(ConfirmAction::Reject);
                }
                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                    return Err(crate::error::ActualError::UserCancelled);
                }
                _ => {
                    // resize or unknown — ignore and continue
                }
            }
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
            // In review mode, show the selected step's log; otherwise show active step's log.
            let log_idx = self.selected_step.unwrap_or(self.active_step);
            let ctx = RenderContext {
                steps: &self.steps,
                log: &self.logs[log_idx],
                confirm: self.confirm_state.as_ref(),
                hint: self.hint,
                scroll_offset: self.scroll_offset,
                selected: self.selected_step,
                version: &self.version,
            };
            let _ = terminal.draw_frame(ctx);
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
pub(crate) struct MockEventSource {
    pub(crate) events: std::collections::VecDeque<crossterm::event::KeyEvent>,
}

#[cfg(test)]
impl MockEventSource {
    pub(crate) fn new(events: Vec<crossterm::event::KeyEvent>) -> Self {
        Self {
            events: events.into(),
        }
    }
}

#[cfg(test)]
impl EventSource for MockEventSource {
    fn next_key(&mut self) -> io::Result<crossterm::event::KeyEvent> {
        self.events
            .pop_front()
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "no more events"))
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
        let steps = StepsPane::new(&[
            "Environment",
            "Analysis",
            "Fetch ADRs",
            "Tailoring",
            "Write Files",
        ]);
        let log = LogPane::new();
        render_to(
            &mut terminal,
            RenderContext {
                steps: &steps,
                log: &log,
                confirm: None,
                hint: false,
                scroll_offset: 0,
                selected: None,
                version: "0.0.0",
            },
        )
        .unwrap();
    }

    #[test]
    fn test_render_to_narrow() {
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let steps = StepsPane::new(&[
            "Environment",
            "Analysis",
            "Fetch ADRs",
            "Tailoring",
            "Write Files",
        ]);
        let log = LogPane::new();
        render_to(
            &mut terminal,
            RenderContext {
                steps: &steps,
                log: &log,
                confirm: None,
                hint: false,
                scroll_offset: 0,
                selected: None,
                version: "0.0.0",
            },
        )
        .unwrap();
    }

    #[test]
    fn test_render_to_exactly_80_cols() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let steps = StepsPane::new(&[
            "Environment",
            "Analysis",
            "Fetch ADRs",
            "Tailoring",
            "Write Files",
        ]);
        let mut log = LogPane::new();
        log.push("a log line".to_string());
        render_to(
            &mut terminal,
            RenderContext {
                steps: &steps,
                log: &log,
                confirm: None,
                hint: false,
                scroll_offset: 0,
                selected: None,
                version: "0.0.0",
            },
        )
        .unwrap();
    }

    #[test]
    fn test_render_to_with_populated_state() {
        use std::time::Duration;
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut steps = StepsPane::new(&[
            "Environment",
            "Analysis",
            "Fetch ADRs",
            "Tailoring",
            "Write Files",
        ]);
        steps.steps[0].status = StepStatus::Success;
        steps.steps[0].elapsed = Some(Duration::from_millis(100));
        steps.steps[1].status = StepStatus::Running { tick: 3 };
        let mut log = LogPane::new();
        log.push("line one".to_string());
        log.push("line two".to_string());
        render_to(
            &mut terminal,
            RenderContext {
                steps: &steps,
                log: &log,
                confirm: None,
                hint: false,
                scroll_offset: 0,
                selected: None,
                version: "0.0.0",
            },
        )
        .unwrap();
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

        // Write: start → success
        r.start(SyncPhase::Write, "writing...");
        assert!(matches!(
            r.steps.steps[4].status,
            StepStatus::Running { .. }
        ));
        r.success(SyncPhase::Write, "2 created · 0 updated · 0 failed");
        assert_eq!(r.steps.steps[4].status, StepStatus::Success);
    }

    #[test]
    fn test_update_message_running_step() {
        let mut r = TuiRenderer::new(false, true);
        r.start(SyncPhase::Tailor, "initial");
        r.update_message(SyncPhase::Tailor, "updated message");
        assert_eq!(r.steps.steps[3].message, "updated message");
        // tick should have advanced
        assert!(matches!(
            r.steps.steps[3].status,
            StepStatus::Running { tick } if tick == 1
        ));
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
        // Analysis, Fetch, Tailor, Write remain Waiting
        r.finish_remaining();

        assert_eq!(r.steps.steps[0].status, StepStatus::Skipped);
        assert_eq!(r.steps.steps[1].status, StepStatus::Skipped);
        assert_eq!(r.steps.steps[2].status, StepStatus::Skipped);
        assert_eq!(r.steps.steps[3].status, StepStatus::Skipped);
        assert_eq!(r.steps.steps[4].status, StepStatus::Skipped);
    }

    #[test]
    fn test_finish_remaining_does_not_overwrite_terminal_steps() {
        let mut r = TuiRenderer::new(false, true);
        r.start(SyncPhase::Environment, "checking...");
        r.success(SyncPhase::Environment, "done");
        r.start(SyncPhase::Analysis, "analyzing...");
        r.error(SyncPhase::Analysis, "failed");
        r.finish_remaining(); // should only skip Fetch, Tailor, and Write

        assert_eq!(r.steps.steps[0].status, StepStatus::Success);
        assert_eq!(r.steps.steps[1].status, StepStatus::Error);
        assert_eq!(r.steps.steps[2].status, StepStatus::Skipped);
        assert_eq!(r.steps.steps[3].status, StepStatus::Skipped);
        assert_eq!(r.steps.steps[4].status, StepStatus::Skipped);
    }

    // ── println tests ──

    #[test]
    fn test_println_plain_mode() {
        let mut r = TuiRenderer::new(false, true);
        r.println("hello world");
        assert_eq!(r.logs[0].len(), 1);
    }

    #[test]
    fn test_println_quiet_mode() {
        let mut r = TuiRenderer::new(true, false);
        r.println("should be ignored");
        assert_eq!(r.logs[0].len(), 0);
    }

    #[test]
    fn test_println_strips_ansi_codes() {
        let mut r = TuiRenderer::new(false, true); // Plain mode
        let ansi_line = "\x1b[38;2;0;251;126mhello\x1b[0m";
        r.println(ansi_line);
        let stored = r.logs[0].render_to_string(10, 100, 0);
        assert_eq!(
            stored[0], "hello",
            "ANSI codes should be stripped from log storage"
        );
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
    fn test_try_setup_tui_direct() {
        // Call try_setup_tui() directly to exercise lines 153-161 in CI.
        // In CI, enable_raw_mode() fails and returns None (line 156 ok()? path).
        // On a real TTY it returns Some(Mode::Tui(...)) — we clean up via drop.
        let result = TuiRenderer::try_setup_tui();
        drop(result); // Mode::Tui Drop impl restores terminal state if entered
    }

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

    // ── SIGWINCH/resize test ──

    #[test]
    fn test_render_to_after_resize() {
        // Start at wide layout (100 cols × 30 rows).
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let steps = StepsPane::new(&[
            "Environment",
            "Analysis",
            "Fetch ADRs",
            "Tailoring",
            "Write Files",
        ]);
        let log = LogPane::new();

        // First draw at wide size — must not panic.
        render_to(
            &mut terminal,
            RenderContext {
                steps: &steps,
                log: &log,
                confirm: None,
                hint: false,
                scroll_offset: 0,
                selected: None,
                version: "0.0.0",
            },
        )
        .unwrap();
        assert_eq!(terminal.size().unwrap().width, 100);

        // Simulate a SIGWINCH / terminal resize: shrink to narrow layout (60 cols).
        terminal.backend_mut().resize(60, 20);

        // Second draw at narrow size — must not panic and must use new dimensions.
        render_to(
            &mut terminal,
            RenderContext {
                steps: &steps,
                log: &log,
                confirm: None,
                hint: false,
                scroll_offset: 0,
                selected: None,
                version: "0.0.0",
            },
        )
        .unwrap();
        assert_eq!(terminal.size().unwrap().width, 60);
        assert_eq!(terminal.size().unwrap().height, 20);
    }

    #[test]
    fn test_render_to_resize_wide_to_narrow_boundary() {
        // Start just above the 80-col breakpoint.
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let steps = StepsPane::new(&[
            "Environment",
            "Analysis",
            "Fetch ADRs",
            "Tailoring",
            "Write Files",
        ]);
        let log = LogPane::new();

        render_to(
            &mut terminal,
            RenderContext {
                steps: &steps,
                log: &log,
                confirm: None,
                hint: false,
                scroll_offset: 0,
                selected: None,
                version: "0.0.0",
            },
        )
        .unwrap();

        // Resize to just below the breakpoint — switches from wide to narrow layout.
        terminal.backend_mut().resize(79, 24);
        render_to(
            &mut terminal,
            RenderContext {
                steps: &steps,
                log: &log,
                confirm: None,
                hint: false,
                scroll_offset: 0,
                selected: None,
                version: "0.0.0",
            },
        )
        .unwrap();
        assert_eq!(terminal.size().unwrap().width, 79);
    }

    // ── wait_for_keypress tests ──

    #[test]
    fn test_wait_for_keypress_plain_mode_is_noop() {
        let mut r = TuiRenderer::new(false, true); // Plain mode
                                                   // Should return without blocking
        r.wait_for_keypress_impl(&mut MockEventSource::new(vec![]));
        // no panic, no event consumed
    }

    #[test]
    fn test_wait_for_keypress_quiet_mode_is_noop() {
        let mut r = TuiRenderer::new(true, false); // Quiet mode
        r.wait_for_keypress_impl(&mut MockEventSource::new(vec![]));
    }

    #[test]
    fn test_wait_for_keypress_tui_mode_consumes_key() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![key]);
        r.wait_for_keypress_impl(&mut source);
        assert!(!r.hint, "hint should be reset after keypress");
        assert!(source.events.is_empty(), "event should be consumed");
    }

    // ── confirm_project tests ──

    #[test]
    fn test_confirm_project_plain_mode_delegates() {
        use crate::cli::ui::test_utils::MockTerminal;
        // MockTerminal with default (accept) response
        let term = MockTerminal::new(vec![]);
        let mut r = TuiRenderer::new(false, true); // Plain mode
                                                   // Just test it doesn't panic; result depends on MockTerminal default
        let analysis = RepoAnalysis {
            is_monorepo: false,
            projects: vec![],
        };
        // This calls prompt_project_confirmation which uses MockTerminal
        let _ = r.confirm_project(&analysis, &term);
    }

    #[test]
    fn test_confirm_project_quiet_mode_delegates() {
        use crate::cli::ui::test_utils::MockTerminal;
        let term = MockTerminal::new(vec![]);
        let mut r = TuiRenderer::new(true, false); // Quiet mode
        let analysis = RepoAnalysis {
            is_monorepo: false,
            projects: vec![],
        };
        let _ = r.confirm_project(&analysis, &term);
    }

    #[test]
    fn test_confirm_project_tui_enter_accept() {
        use crate::cli::ui::test_utils::MockTerminal;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let term = MockTerminal::new(vec![]);
        let analysis = RepoAnalysis {
            is_monorepo: false,
            projects: vec![],
        };
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![enter]);
        let result = r.confirm_project_impl(&analysis, &term, &mut source);
        assert!(matches!(result, Ok(ConfirmAction::Accept)));
        assert!(r.confirm_state.is_none(), "confirm state should be cleared");
    }

    #[test]
    fn test_confirm_project_tui_y_accept() {
        use crate::cli::ui::test_utils::MockTerminal;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let term = MockTerminal::new(vec![]);
        let analysis = RepoAnalysis {
            is_monorepo: false,
            projects: vec![],
        };
        let y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![y]);
        let result = r.confirm_project_impl(&analysis, &term, &mut source);
        assert!(matches!(result, Ok(ConfirmAction::Accept)));
    }

    #[test]
    fn test_confirm_project_tui_n_reject() {
        use crate::cli::ui::test_utils::MockTerminal;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let term = MockTerminal::new(vec![]);
        let analysis = RepoAnalysis {
            is_monorepo: false,
            projects: vec![],
        };
        let n = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![n]);
        let result = r.confirm_project_impl(&analysis, &term, &mut source);
        assert!(matches!(result, Ok(ConfirmAction::Reject)));
    }

    #[test]
    fn test_confirm_project_tui_esc_reject() {
        use crate::cli::ui::test_utils::MockTerminal;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let term = MockTerminal::new(vec![]);
        let analysis = RepoAnalysis {
            is_monorepo: false,
            projects: vec![],
        };
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![esc]);
        let result = r.confirm_project_impl(&analysis, &term, &mut source);
        assert!(matches!(result, Ok(ConfirmAction::Reject)));
    }

    #[test]
    fn test_confirm_project_tui_tab_switches_focus_then_enter() {
        use crate::cli::ui::test_utils::MockTerminal;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let term = MockTerminal::new(vec![]);
        let analysis = RepoAnalysis {
            is_monorepo: false,
            projects: vec![],
        };
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![tab, enter]);
        let result = r.confirm_project_impl(&analysis, &term, &mut source);
        // After Tab, focus moves to Reject, then Enter confirms Reject
        assert!(matches!(result, Ok(ConfirmAction::Reject)));
    }

    #[test]
    fn test_confirm_project_tui_right_then_left_then_enter() {
        use crate::cli::ui::test_utils::MockTerminal;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let term = MockTerminal::new(vec![]);
        let analysis = RepoAnalysis {
            is_monorepo: false,
            projects: vec![],
        };
        let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![right, left, enter]);
        let result = r.confirm_project_impl(&analysis, &term, &mut source);
        // Right → Reject, Left → Accept, Enter confirms Accept
        assert!(matches!(result, Ok(ConfirmAction::Accept)));
    }

    #[test]
    fn test_confirm_project_tui_ctrl_c_cancels() {
        use crate::cli::ui::test_utils::MockTerminal;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let term = MockTerminal::new(vec![]);
        let analysis = RepoAnalysis {
            is_monorepo: false,
            projects: vec![],
        };
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let mut source = MockEventSource::new(vec![ctrl_c]);
        let result = r.confirm_project_impl(&analysis, &term, &mut source);
        assert!(matches!(
            result,
            Err(crate::error::ActualError::UserCancelled)
        ));
    }

    #[test]
    fn test_confirm_project_tui_source_exhausted() {
        use crate::cli::ui::test_utils::MockTerminal;
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let term = MockTerminal::new(vec![]);
        let analysis = RepoAnalysis {
            is_monorepo: false,
            projects: vec![],
        };
        let mut source = MockEventSource::new(vec![]); // no events → EOF error
        let result = r.confirm_project_impl(&analysis, &term, &mut source);
        assert!(result.is_err()); // IO error propagated
    }

    // ── render_to with hint/confirm tests ──

    #[test]
    fn test_render_to_with_hint() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let steps = StepsPane::new(&["Environment", "Analysis", "Fetch ADRs", "Tailoring"]);
        let log = LogPane::new();
        render_to(
            &mut terminal,
            RenderContext {
                steps: &steps,
                log: &log,
                confirm: None,
                hint: true,
                scroll_offset: 0,
                selected: None,
                version: "0.0.0",
            },
        )
        .unwrap();
    }

    #[test]
    fn test_render_to_with_confirm() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let steps = StepsPane::new(&["Environment", "Analysis", "Fetch ADRs", "Tailoring"]);
        let log = LogPane::new();
        let cs = ConfirmState {
            question: "Proceed?".to_string(),
            focus: ConfirmFocus::Accept,
        };
        render_to(
            &mut terminal,
            RenderContext {
                steps: &steps,
                log: &log,
                confirm: Some(&cs),
                hint: false,
                scroll_offset: 0,
                selected: None,
                version: "0.0.0",
            },
        )
        .unwrap();
    }

    #[test]
    fn test_render_to_with_confirm_reject_focus() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let steps = StepsPane::new(&["Environment", "Analysis", "Fetch ADRs", "Tailoring"]);
        let log = LogPane::new();
        let cs = ConfirmState {
            question: "Proceed?".to_string(),
            focus: ConfirmFocus::Reject,
        };
        render_to(
            &mut terminal,
            RenderContext {
                steps: &steps,
                log: &log,
                confirm: Some(&cs),
                hint: false,
                scroll_offset: 0,
                selected: None,
                version: "0.0.0",
            },
        )
        .unwrap();
    }

    #[test]
    fn test_render_to_narrow_with_hint() {
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let steps = StepsPane::new(&["Environment", "Analysis", "Fetch ADRs", "Tailoring"]);
        let log = LogPane::new();
        render_to(
            &mut terminal,
            RenderContext {
                steps: &steps,
                log: &log,
                confirm: None,
                hint: true,
                scroll_offset: 0,
                selected: None,
                version: "0.0.0",
            },
        )
        .unwrap();
    }

    #[test]
    fn test_render_to_narrow_with_confirm() {
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let steps = StepsPane::new(&["Environment", "Analysis", "Fetch ADRs", "Tailoring"]);
        let log = LogPane::new();
        let cs = ConfirmState {
            question: "Proceed?".to_string(),
            focus: ConfirmFocus::Accept,
        };
        render_to(
            &mut terminal,
            RenderContext {
                steps: &steps,
                log: &log,
                confirm: Some(&cs),
                hint: false,
                scroll_offset: 0,
                selected: None,
                version: "0.0.0",
            },
        )
        .unwrap();
    }
}
