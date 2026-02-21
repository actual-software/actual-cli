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
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph},
    Terminal,
};

use super::gradient::{color_for_y, paint_gradient_border};
use super::log::LogPane;
use super::steps::{StepStatus, StepsPane};
use crate::analysis::confirm::ConfirmAction;
use crate::analysis::types::RepoAnalysis;
use crate::branding::banner::BANNER;
use crate::cli::ui::confirm::{format_project_summary_plain, prompt_project_confirmation};
use crate::cli::ui::progress::SyncPhase;
use crate::cli::ui::terminal::TerminalIO;

/// Navigation command produced by the background keyboard poller during execution.
///
/// Commands are sent via an `mpsc` channel and drained in each `draw()` call
/// so navigation is applied without blocking the main render loop.
#[derive(Debug, Clone, Copy)]
pub enum NavCmd {
    /// Move the viewed step one up.
    StepUp,
    /// Move the viewed step one down (clamped to `active_step`).
    StepDown,
    /// Scroll the log pane up (toward older content).
    ScrollUp,
    /// Scroll the log pane down (toward newer content).
    ScrollDown,
    /// Jump to the top of the log pane (oldest content).
    ScrollTop,
    /// Jump to the bottom of the log pane (newest content, follow mode).
    ScrollBottom,
}

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

/// State for the inline file multi-select rendered at the bottom of the log pane.
pub struct FileSelectState {
    /// Display label for each file (e.g. "CLAUDE.md  (3 ADRs)").
    pub items: Vec<String>,
    /// Toggle state for each item (true = selected).
    pub checked: Vec<bool>,
    /// Index of the currently highlighted row.
    pub cursor: usize,
}

/// Rendering context passed to `render_to` and `draw_frame`.
pub struct RenderContext<'a> {
    pub steps: &'a StepsPane,
    pub log: &'a LogPane,
    pub confirm: Option<&'a ConfirmState>,
    pub file_select: Option<&'a FileSelectState>,
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

fn append_file_select_lines(lines: &mut Vec<String>, fs: &FileSelectState) {
    lines.push("  Select files to write:".to_string());
    lines.push("  \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}".to_string());
    for (i, item) in fs.items.iter().enumerate() {
        let checkbox = if fs.checked[i] { "[x]" } else { "[ ]" };
        let cursor = if i == fs.cursor { ">" } else { " " };
        lines.push(format!("  {cursor} {checkbox} {item}"));
    }
    lines.push(String::new());
    lines.push("  Space toggle  Enter confirm  Esc cancel".to_string());
}

/// Build the keybinding hint text as a ratatui `Line` for embedding in the bottom border.
fn build_hint_line(
    ctx: &RenderContext<'_>,
    log_height: usize,
    log_width: usize,
) -> Option<Line<'static>> {
    let has_hint = ctx.hint || ctx.confirm.is_some() || ctx.file_select.is_some();
    if !has_hint {
        return None;
    }

    let hint_text = if ctx.confirm.is_some() {
        " ← → select  Enter confirm ".to_string()
    } else if ctx.file_select.is_some() {
        " ↑/↓ move  Space toggle  Enter confirm  Esc cancel ".to_string()
    } else {
        let max = ctx.log.max_scroll_wrapped(log_height, log_width);
        if max > 0 {
            format!(
                " ↑/↓ steps  u/d scroll  g/G top/bottom  q quit  [{}/{}] ",
                ctx.scroll_offset.min(max),
                max
            )
        } else {
            " ↑/↓ steps  q quit ".to_string()
        }
    };

    Some(Line::from(hint_text).right_aligned())
}

/// Width of the left panel (banner box + steps box).
///
/// The banner art is 37 chars wide; +2 for borders = 39. We add a few chars
/// of breathing room and round up to 44 so step messages are readable too.
const LEFT_COL_WIDTH: u16 = 44;

/// Number of banner lines + border rows for the banner box height.
/// BANNER has 11 lines + 1 blank above + 1 blank below + 2 border rows = 15.
const BANNER_BOX_HEIGHT: u16 = 15;

/// Horizontal padding (left + right) applied inside the Output panel border.
const HORIZ_PAD: usize = 2;

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
        let frame_area = area;

        // ── Outer vertical split: content area on top, footer hint row on bottom ──
        let has_hint = ctx.hint || ctx.confirm.is_some() || ctx.file_select.is_some();
        // In wide mode, hints go into the Output pane's bottom border — no separate footer row.
        // In narrow mode, keep the footer row since there's no bordered panel.
        let needs_footer_row = has_hint && cols < 80;
        let outer_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(if needs_footer_row {
                vec![Constraint::Fill(1), Constraint::Length(1)]
            } else {
                vec![Constraint::Fill(1), Constraint::Length(0)]
            })
            .split(area);
        let content_area = outer_chunks[0];
        let footer_area = outer_chunks[1];

        // ── Footer hint row (narrow mode only) ──
        if needs_footer_row {
            let approx_log_height = content_area.height.saturating_sub(2) as usize;
            let hint_text = if ctx.confirm.is_some() {
                "  ← → select  Enter confirm".to_string()
            } else if ctx.file_select.is_some() {
                "  ↑/↓ move  Space toggle  Enter confirm  Esc cancel".to_string()
            } else {
                let approx_log_width = content_area.width.saturating_sub(2) as usize;
                let max = ctx
                    .log
                    .max_scroll_wrapped(approx_log_height, approx_log_width);
                if max > 0 {
                    format!(
                        "  ↑/↓ steps  u/d scroll  g/G top/bottom  q quit  [{}/{}]",
                        ctx.scroll_offset.min(max),
                        max
                    )
                } else {
                    "  ↑/↓ steps  q quit".to_string()
                }
            };
            frame.render_widget(Clear, footer_area);
            frame.render_widget(Paragraph::new(hint_text), footer_area);
        }

        if cols >= 80 {
            // ── Horizontal split: left (banner + steps) | right (output) ──
            let h_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(LEFT_COL_WIDTH), Constraint::Fill(1)])
                .split(content_area);

            // ── Left column: vertical split — banner box on top, steps below ──
            let left_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(BANNER_BOX_HEIGHT), Constraint::Fill(1)])
                .split(h_chunks[0]);

            // Banner box (top-left)
            let inner_width = (LEFT_COL_WIDTH as usize).saturating_sub(2); // 42
            let banner_lines: Vec<&str> = BANNER.lines().collect();
            // Uniform pad: center the widest line; apply the same pad to every line.
            // This preserves the relative indentation baked into the art.
            let max_art_width = banner_lines
                .iter()
                .map(|l| l.chars().count())
                .max()
                .unwrap_or(0);
            let uniform_pad = inner_width.saturating_sub(max_art_width) / 2;
            let pad_str = " ".repeat(uniform_pad);

            // Build padded strings (blank + art lines + blank)
            let mut padded_banner: Vec<String> = Vec::with_capacity(banner_lines.len() + 2);
            padded_banner.push(String::new()); // blank line above art
            for l in &banner_lines {
                padded_banner.push(format!("{pad_str}{l}"));
            }
            padded_banner.push(String::new()); // blank line below art

            let version_title = format!(" actual v{} ", ctx.version);

            // Right-aligned URL title for the logo box top border.
            // Only add it when the box is wide enough to fit both titles
            // without overlapping (version ~16 chars + URL 19 chars + 2 borders = ~37).
            let url = "https://app.actual.ai";
            let url_display = format!(" {url} ");
            let url_display_len = url_display.len() as u16;
            let version_title_len = version_title.len() as u16;
            let min_width_for_url = version_title_len + url_display_len + 2 + 2; // borders + gap
            let box_width = left_chunks[0].width;
            let url_title: Option<Line<'_>> = if box_width >= min_width_for_url {
                Some(Line::from(url_display.clone()).right_aligned())
            } else {
                None
            };

            let banner_widget = if use_color {
                // Build colored Lines — each line's color is determined by its absolute y in the frame.
                // Content starts at left_chunks[0].y + 1 (inside the top border).
                let content_y_start = left_chunks[0].y + 1;
                let colored_lines: Vec<Line<'_>> = padded_banner
                    .iter()
                    .enumerate()
                    .map(|(i, text)| {
                        let abs_y = content_y_start + i as u16;
                        let color = color_for_y(abs_y, frame_area);
                        Line::from(Span::styled(text.clone(), Style::default().fg(color)))
                    })
                    .collect();
                let mut block = Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .title(version_title.as_str());
                if let Some(ref title) = url_title {
                    block = block.title_top(title.clone());
                }
                Paragraph::new(colored_lines).block(block)
            } else {
                let banner_text = padded_banner.join("\n");
                let mut block = Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .title(version_title.as_str());
                if let Some(ref title) = url_title {
                    block = block.title_top(title.clone());
                }
                Paragraph::new(banner_text).block(block)
            };
            frame.render_widget(Clear, left_chunks[0]);
            frame.render_widget(banner_widget, left_chunks[0]);
            if use_color {
                paint_gradient_border(frame.buffer_mut(), left_chunks[0], frame_area);
            }

            // Steps box (bottom-left)
            let step_inner_width = (LEFT_COL_WIDTH as usize).saturating_sub(2);
            let mut step_lines = ctx.steps.render_lines(step_inner_width, ctx.selected);
            // Add one line of padding at the top and bottom of the Steps section.
            step_lines.insert(0, String::new());
            step_lines.push(String::new());
            let step_text = step_lines.join("\n");
            let step_widget = Paragraph::new(step_text).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .title(" Steps "),
            );
            frame.render_widget(Clear, left_chunks[1]);
            frame.render_widget(step_widget, left_chunks[1]);
            if use_color {
                paint_gradient_border(frame.buffer_mut(), left_chunks[1], frame_area);
            }

            // ── Right column: output pane ──
            // Reserve 2 lines for padding (top + bottom) inside the Output box.
            const OUTPUT_PADDING: usize = 2;
            let log_inner_height = h_chunks[1].height.saturating_sub(2) as usize;
            let log_height = log_inner_height.saturating_sub(OUTPUT_PADDING);
            let log_width = h_chunks[1].width.saturating_sub(2 + 2 * HORIZ_PAD as u16) as usize;
            let mut log_lines = ctx
                .log
                .render_to_string(log_height, log_width, ctx.scroll_offset);
            // Insert one blank line at the top for visual breathing room.
            // Pad to `log_width` so Paragraph writes every cell in the row,
            // preventing stale buffer content from bleeding through.
            let pad = " ".repeat(log_width);
            log_lines.insert(0, pad.clone());
            if let Some(cs) = ctx.confirm {
                log_lines.push(String::new());
                append_confirm_lines(&mut log_lines, cs);
            } else if let Some(fs) = ctx.file_select {
                log_lines.push(String::new());
                append_file_select_lines(&mut log_lines, fs);
            } else {
                log_lines.push(pad);
            }
            let log_text = log_lines.join("\n");
            let hint_line = build_hint_line(&ctx, log_height, log_width);
            let mut output_block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(" Output ")
                .padding(Padding::horizontal(HORIZ_PAD as u16));
            if let Some(hint) = hint_line {
                output_block = output_block.title_bottom(hint);
            }
            let log_widget = Paragraph::new(log_text).block(output_block);
            frame.render_widget(Clear, h_chunks[1]);
            frame.render_widget(log_widget, h_chunks[1]);
            if use_color {
                paint_gradient_border(frame.buffer_mut(), h_chunks[1], frame_area);
            }
        } else {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Fill(1)])
                .split(content_area);

            let condensed = ctx.steps.render_condensed(area.width as usize);
            frame.render_widget(Clear, chunks[0]);
            frame.render_widget(Paragraph::new(condensed), chunks[0]);

            let log_inner_height = chunks[1].height as usize;
            let log_width = area.width as usize;
            let mut log_lines =
                ctx.log
                    .render_to_string(log_inner_height, log_width, ctx.scroll_offset);
            if let Some(cs) = ctx.confirm {
                append_confirm_lines(&mut log_lines, cs);
            } else if let Some(fs) = ctx.file_select {
                append_file_select_lines(&mut log_lines, fs);
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
    /// State for the inline file multi-select (None when not active).
    file_select_state: Option<FileSelectState>,
    /// Which step is highlighted in review mode (None = show active_step's log).
    selected_step: Option<usize>,
    /// Which step the user is viewing during execution (None = follow active_step).
    viewing_step: Option<usize>,
    /// Lines scrolled up from the bottom of the log pane (0 = pinned to bottom).
    scroll_offset: usize,
    /// Version string shown in the banner box title.
    version: String,
    /// Receives navigation commands from the background keyboard poller during execution.
    nav_rx: Option<std::sync::mpsc::Receiver<NavCmd>>,
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
            file_select_state: None,
            selected_step: None,
            viewing_step: None,
            scroll_offset: 0,
            version: version.to_string(),
            nav_rx: None,
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
            file_select_state: None,
            selected_step: None,
            viewing_step: None,
            scroll_offset: 0,
            version: "0.0.0-test".to_string(),
            nav_rx: None,
        }
    }

    /// Set the navigation command receiver.  Called from `sync.rs` after the
    /// background keyboard poller is started.  If `rx` is `None` (non-TUI mode),
    /// the existing `nav_rx` field is cleared so no stale receiver remains.
    pub fn set_nav_rx_opt(&mut self, rx: Option<std::sync::mpsc::Receiver<NavCmd>>) {
        self.nav_rx = rx;
    }

    /// Drain all pending [`NavCmd`]s from the channel and apply each one.
    fn apply_nav_cmds(&mut self) {
        // We can't hold a borrow on self.nav_rx while also calling handle_nav_cmd(self, …),
        // so we collect first, then process.
        let cmds: Vec<NavCmd> = match &self.nav_rx {
            Some(rx) => {
                let mut buf = Vec::new();
                while let Ok(cmd) = rx.try_recv() {
                    buf.push(cmd);
                }
                buf
            }
            None => return,
        };
        for cmd in cmds {
            self.handle_nav_cmd(cmd);
        }
    }

    /// Apply a single [`NavCmd`] to the renderer state.
    fn handle_nav_cmd(&mut self, cmd: NavCmd) {
        let n_steps = self.steps.steps.len();
        if n_steps == 0 {
            return;
        }
        let cur = self.viewing_step.unwrap_or(self.active_step);
        match cmd {
            NavCmd::StepUp => {
                let next = cur.saturating_sub(1);
                if next != cur {
                    self.viewing_step = Some(next);
                    self.scroll_offset = 0;
                }
            }
            NavCmd::StepDown => {
                // Cannot navigate past the active step during execution.
                let max = self.active_step;
                let next = (cur + 1).min(max);
                if next != cur {
                    self.viewing_step = Some(next);
                    self.scroll_offset = 0;
                }
                // If we've reached the active step, snap back to follow mode.
                if self.viewing_step == Some(self.active_step) {
                    self.viewing_step = None;
                }
            }
            NavCmd::ScrollUp => {
                // Scroll up by half a page (fixed heuristic).
                self.scroll_offset = self.scroll_offset.saturating_add(5);
            }
            NavCmd::ScrollDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(5);
            }
            NavCmd::ScrollTop => {
                // usize::MAX is clamped by render_to_string's own clamp logic.
                self.scroll_offset = usize::MAX;
            }
            NavCmd::ScrollBottom => {
                self.scroll_offset = 0;
            }
        }
    }

    /// Return all log lines across all per-step buffers, concatenated.
    /// Useful in tests to assert content without knowing which step it went to.
    #[cfg(test)]
    pub fn all_log_text(&self) -> String {
        self.logs
            .iter()
            .flat_map(|l| l.raw_lines())
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
        self.steps.steps[idx].message = String::new();
        self.push_log(idx, message);
        // Once there is navigable history (at least one completed step before this one),
        // enable the footer hint so the user knows they can use arrow keys.
        if idx > 0 {
            self.hint = true;
        }
        self.draw();
        if let Mode::Plain = &self.mode {
            eprintln!("  ○ {message}");
        }
    }

    /// Stop a phase and show a success message.
    pub fn success(&mut self, phase: SyncPhase, message: &str) {
        let idx = Self::phase_idx(phase);
        self.steps.steps[idx].status = StepStatus::Success;
        self.steps.steps[idx].message = String::new();
        self.steps.steps[idx].elapsed = self.starts[idx].map(|s| s.elapsed());
        self.push_log(idx, message);
        self.draw();
        if let Mode::Plain = &self.mode {
            eprintln!("  ✔ {message}");
        }
    }

    /// Mark a phase as skipped.
    pub fn skip(&mut self, phase: SyncPhase, message: &str) {
        let idx = Self::phase_idx(phase);
        self.steps.steps[idx].status = StepStatus::Skipped;
        self.steps.steps[idx].message = String::new();
        self.push_log(idx, message);
        self.draw();
        if let Mode::Plain = &self.mode {
            eprintln!("  ─ {message}");
        }
    }

    /// Stop a phase and show an error message.
    pub fn error(&mut self, phase: SyncPhase, message: &str) {
        let idx = Self::phase_idx(phase);
        self.steps.steps[idx].status = StepStatus::Error;
        self.steps.steps[idx].message = String::new();
        self.steps.steps[idx].elapsed = self.starts[idx].map(|s| s.elapsed());
        self.push_log(idx, message);
        self.draw();
        if let Mode::Plain = &self.mode {
            eprintln!("  ✖ {message}");
        }
    }

    /// Stop a phase and show a warning message.
    pub fn warn(&mut self, phase: SyncPhase, message: &str) {
        let idx = Self::phase_idx(phase);
        self.steps.steps[idx].status = StepStatus::Warn;
        self.steps.steps[idx].message = String::new();
        self.steps.steps[idx].elapsed = self.starts[idx].map(|s| s.elapsed());
        self.push_log(idx, message);
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
    ///
    /// The `message` string is intentionally not displayed in the step row or the
    /// output log pane.  Setting `step.message` would pollute the Steps pane with
    /// transient progress text; pushing to the log pane would flood it with one
    /// entry every 100 ms during tailoring.  Instead we update `step.elapsed` so
    /// the step row shows a live `[Xs]` counter while the phase is running.
    pub fn update_message(&mut self, phase: SyncPhase, message: &str) {
        let idx = Self::phase_idx(phase);
        let _ = message; // progress text is intentionally discarded (see doc comment)
                         // Show live elapsed in the step row while the phase is running.
        self.steps.steps[idx].elapsed = self.starts[idx].map(|s| s.elapsed());
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
        self.push_log(self.active_step, line);
        self.draw();
        if let Mode::Plain = &self.mode {
            eprintln!("{line}");
        }
    }

    /// Push a line to a specific step's log pane, stripping ANSI escape codes.
    ///
    /// All internal log pushes should go through this method (or through
    /// [`println`] which delegates here) so that ratatui never receives raw
    /// escape sequences.
    fn push_log(&mut self, step: usize, line: &str) {
        let plain = console::strip_ansi_codes(line);
        self.logs[step].push(plain.into_owned());
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
            // Entering post-sync review mode: clear viewing_step so selected_step
            // takes over cleanly (viewing_step is only for mid-execution navigation).
            self.viewing_step = None;
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
                // render_to uses:
                //   Wide (>=80): no footer row, box borders = -2, OUTPUT_PADDING = -2 → log = rows-4
                //   Narrow (<80): footer row = -1, condensed row = -1 → log = rows-2
                let (log_height, log_width): (usize, usize) = crossterm::terminal::size()
                    .map(|(cols, rows)| {
                        let rows = rows as usize;
                        let height = if cols >= 80 {
                            rows.saturating_sub(4)
                        } else {
                            rows.saturating_sub(2)
                        };
                        let width = if cols >= 80 {
                            (cols as usize)
                                .saturating_sub(LEFT_COL_WIDTH as usize + 2 + 2 * HORIZ_PAD)
                        } else {
                            (cols as usize).saturating_sub(2)
                        };
                        (height, width)
                    })
                    .unwrap_or((20, 80));
                let n_steps = self.steps.steps.len();
                let cur = self.selected_step.unwrap_or(self.active_step);
                let log_idx = self.selected_step.unwrap_or(self.active_step);
                let max = self.logs[log_idx].max_scroll_wrapped(log_height, log_width);
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
                let summary = format_project_summary_plain(analysis);
                for line in summary.lines() {
                    self.push_log(self.active_step, line);
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

    /// Show the file multi-select prompt.
    ///
    /// In TUI mode: renders an inline checklist at the bottom of the log pane.
    /// In Plain/Quiet mode: delegates to `confirm_files` (the existing dialoguer-based flow).
    pub fn select_files_in_tui(
        &mut self,
        output: &crate::tailoring::types::TailoringOutput,
        force: bool,
        term: &dyn TerminalIO,
    ) -> Result<Vec<crate::tailoring::types::FileOutput>, crate::error::ActualError> {
        self.select_files_in_tui_impl(output, force, term, &mut CrosstermEventSource)
    }

    fn select_files_in_tui_impl(
        &mut self,
        output: &crate::tailoring::types::TailoringOutput,
        force: bool,
        term: &dyn TerminalIO,
        source: &mut dyn EventSource,
    ) -> Result<Vec<crate::tailoring::types::FileOutput>, crate::error::ActualError> {
        use crate::cli::ui::file_confirm::confirm_files;

        match &self.mode {
            Mode::Tui(_) => {
                if force {
                    return Ok(output.files.clone());
                }

                // Show skipped ADRs in the log pane before the file list.
                if !output.skipped_adrs.is_empty() {
                    self.push_log(
                        self.active_step,
                        &format!(
                            "{} ADR(s) skipped (not applicable)",
                            output.skipped_adrs.len()
                        ),
                    );
                }

                if output.files.is_empty() {
                    return Ok(vec![]);
                }

                // Build display items.
                let items: Vec<String> = output
                    .files
                    .iter()
                    .map(|file| {
                        let count = file.adr_ids.len();
                        let plural = if count == 1 { "" } else { "s" };
                        let safe_path = console::strip_ansi_codes(&file.path);
                        format!("{safe_path}  ({count} ADR{plural})")
                    })
                    .collect();

                let n = items.len();
                self.file_select_state = Some(FileSelectState {
                    items,
                    checked: vec![true; n],
                    cursor: 0,
                });
                self.draw();

                let result = self.run_file_select_loop(source);

                self.file_select_state = None;
                self.draw();

                match result? {
                    Some(indices) => {
                        let selected: Result<Vec<_>, _> = indices
                            .into_iter()
                            .map(|i| {
                                output.files.get(i).cloned().ok_or_else(|| {
                                    crate::error::ActualError::InternalError(format!(
                                        "file selection index {i} out of bounds (max {})",
                                        output.files.len()
                                    ))
                                })
                            })
                            .collect();
                        Ok(selected?)
                    }
                    None => Err(crate::error::ActualError::UserCancelled),
                }
            }
            Mode::Plain | Mode::Quiet => confirm_files(output, force, term),
        }
    }

    /// Inner event loop for the file multi-select.
    ///
    /// Returns `Ok(Some(indices))` for a confirmed selection, `Ok(None)` for cancel,
    /// or `Err` on I/O failure.
    fn run_file_select_loop(
        &mut self,
        source: &mut dyn EventSource,
    ) -> Result<Option<Vec<usize>>, crate::error::ActualError> {
        use crossterm::event::{KeyCode, KeyModifiers};
        loop {
            let key = source.next_key().map_err(|e| {
                crate::error::ActualError::InternalError(format!("event read failed: {e}"))
            })?;
            let n = self
                .file_select_state
                .as_ref()
                .map(|fs| fs.items.len())
                .unwrap_or(0);
            match (key.code, key.modifiers) {
                // Confirm selection
                (KeyCode::Enter, _) => {
                    if let Some(ref fs) = self.file_select_state {
                        let indices: Vec<usize> = fs
                            .checked
                            .iter()
                            .enumerate()
                            .filter_map(|(i, &checked)| if checked { Some(i) } else { None })
                            .collect();
                        return Ok(Some(indices));
                    }
                    return Ok(Some(vec![]));
                }
                // Cancel
                (KeyCode::Esc, _) | (KeyCode::Char('q'), _) | (KeyCode::Char('Q'), _) => {
                    return Ok(None);
                }
                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                    return Err(crate::error::ActualError::UserCancelled);
                }
                // Move cursor up
                (KeyCode::Up, _) => {
                    if let Some(ref mut fs) = self.file_select_state {
                        if fs.cursor > 0 {
                            fs.cursor -= 1;
                        }
                    }
                    self.draw();
                }
                // Move cursor down
                (KeyCode::Down, _) => {
                    if let Some(ref mut fs) = self.file_select_state {
                        if n > 0 && fs.cursor < n - 1 {
                            fs.cursor += 1;
                        }
                    }
                    self.draw();
                }
                // Toggle current item
                (KeyCode::Char(' '), _) => {
                    if let Some(ref mut fs) = self.file_select_state {
                        let cur = fs.cursor;
                        if cur < fs.checked.len() {
                            fs.checked[cur] = !fs.checked[cur];
                        }
                    }
                    self.draw();
                }
                _ => {}
            }
        }
    }

    /// Adjusts the start time for the given phase forward by `paused` duration,
    /// effectively excluding that duration from the elapsed time calculation.
    ///
    /// All elapsed calculations use `start.elapsed()` (i.e. `Instant::now() - start`).
    /// By shifting `start` forward, the computed elapsed shrinks by `paused`.
    pub fn adjust_start_for_pause(&mut self, phase: SyncPhase, paused: Duration) {
        let idx = Self::phase_idx(phase);
        if let Some(ref mut start) = self.starts[idx] {
            *start += paused;
        }
    }

    /// Returns `true` when running in full TUI mode (crossterm raw mode).
    pub fn is_tui(&self) -> bool {
        matches!(self.mode, Mode::Tui(_))
    }

    /// Return the wall-clock time elapsed since this renderer was created.
    pub fn total_elapsed(&self) -> Duration {
        self.created_at.elapsed()
    }

    /// Return the instant this renderer was created.
    pub fn start_time(&self) -> Instant {
        self.created_at
    }

    /// Returns the sum of elapsed times across all completed steps,
    /// excluding any time spent waiting for user input.
    pub fn steps_elapsed(&self) -> Duration {
        self.steps.steps.iter().filter_map(|s| s.elapsed).sum()
    }

    /// Redraw the TUI. No-op in Plain and Quiet modes.
    ///
    /// Gap 4: draw errors are logged via `tracing::error!` and the renderer
    /// falls back to [`Mode::Plain`] so subsequent output is still visible.
    /// The terminal cleanup (`disable_raw_mode` + `LeaveAlternateScreen`) is
    /// performed here before switching modes so the Drop impl does not try to
    /// clean up a terminal that is no longer in Tui mode.
    fn draw(&mut self) {
        // Drain any pending navigation commands before rendering.
        self.apply_nav_cmds();

        // Build the context and attempt the draw while the Tui borrow is active,
        // then handle any error after the borrow is released.
        let draw_error = if let Mode::Tui(ref mut terminal) = self.mode {
            // During execution, `viewing_step` overrides `selected_step` for log display.
            // In post-sync review mode only `selected_step` is set.
            let log_idx = self
                .viewing_step
                .or(self.selected_step)
                .unwrap_or(self.active_step);
            // The selection indicator follows `viewing_step` during execution,
            // falling back to `selected_step` in post-sync review mode.
            let selected = self.viewing_step.or(self.selected_step);
            let ctx = RenderContext {
                steps: &self.steps,
                log: &self.logs[log_idx],
                confirm: self.confirm_state.as_ref(),
                file_select: self.file_select_state.as_ref(),
                hint: self.hint,
                scroll_offset: self.scroll_offset,
                selected,
                version: &self.version,
            };
            terminal.draw_frame(ctx).err().map(|e| e.to_string())
        } else {
            None
        };

        if let Some(err_msg) = draw_error {
            tracing::error!("TUI draw failed: {err_msg}; falling back to plain output");
            // Clean up terminal state before switching to plain mode so the
            // Drop impl does not attempt a double-cleanup.
            let _ = disable_raw_mode();
            let _ = execute!(io::stderr(), LeaveAlternateScreen, Show);
            self.mode = Mode::Plain;
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
                file_select: None,
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
                file_select: None,
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
                file_select: None,
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
                file_select: None,
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
        // message is discarded — log pane unchanged (start pushed "initial", len stays 1).
        assert_eq!(r.logs[3].len(), 1);
        // step.message must remain empty (progress text is not shown in the step row).
        assert_eq!(r.steps.steps[3].message, "");
        // elapsed must be updated for the live [Xs] counter in the step row.
        assert!(r.steps.steps[3].elapsed.is_some());
        // tick should have advanced
        assert!(matches!(
            r.steps.steps[3].status,
            StepStatus::Running { tick } if tick == 1
        ));
    }

    #[test]
    fn test_update_message_non_running_step() {
        // update_message on a non-Running step should update elapsed without
        // panicking (the tick increment branch is skipped for non-Running steps).
        let mut r = TuiRenderer::new(false, true);
        // Step is Waiting — not Running
        r.update_message(SyncPhase::Environment, "some message");
        // message is discarded — log pane must remain empty.
        assert_eq!(r.logs[0].len(), 0);
        // step.message must remain empty.
        assert_eq!(r.steps.steps[0].message, "");
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
        assert!(
            stored[0].starts_with("hello"),
            "ANSI codes should be stripped from log storage, got {:?}",
            stored[0]
        );
        assert_eq!(
            stored[0].trim_end(),
            "hello",
            "stored content should be 'hello' (possibly right-padded)"
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

    // ── adjust_start_for_pause tests ──

    #[test]
    fn test_adjust_start_for_pause_reduces_elapsed() {
        let mut r = TuiRenderer::new(false, true);
        r.start(SyncPhase::Write, "writing...");
        // Sleep briefly so there is measurable elapsed time.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let before = r.starts[4].unwrap().elapsed();
        // Shift start forward by 20ms — elapsed should decrease by ~20ms.
        r.adjust_start_for_pause(SyncPhase::Write, std::time::Duration::from_millis(20));
        let after = r.starts[4].unwrap().elapsed();
        assert!(
            after < before,
            "elapsed after adjustment ({after:?}) should be less than before ({before:?})"
        );
    }

    #[test]
    fn test_adjust_start_for_pause_no_start_is_noop() {
        let mut r = TuiRenderer::new(false, true);
        // No start() called — adjust should be a no-op without panic.
        r.adjust_start_for_pause(SyncPhase::Write, std::time::Duration::from_millis(100));
        assert!(r.starts[4].is_none());
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
                file_select: None,
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
                file_select: None,
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
                file_select: None,
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
                file_select: None,
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

    #[test]
    fn test_confirm_project_strips_ansi_from_logs() {
        use crate::analysis::types::{Framework, FrameworkCategory, Language, Project};
        use crate::cli::ui::test_utils::MockTerminal;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let term = MockTerminal::new(vec![]);

        // Build a non-empty analysis so format_project_summary produces
        // ANSI-styled output (via theme::heading, theme::hint, etc.).
        let analysis = RepoAnalysis {
            is_monorepo: false,
            projects: vec![Project {
                path: ".".to_string(),
                name: "TestProject".to_string(),
                languages: vec![Language::Rust],
                frameworks: vec![Framework {
                    name: "actix".to_string(),
                    category: FrameworkCategory::WebBackend,
                }],
                package_manager: Some("cargo".to_string()),
                description: None,
            }],
        };

        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![enter]);
        let result = r.confirm_project_impl(&analysis, &term, &mut source);
        assert!(matches!(result, Ok(ConfirmAction::Accept)));

        // Every line pushed to the log pane must be free of ANSI escape codes.
        let raw = r.logs[r.active_step].raw_lines();
        assert!(
            !raw.is_empty(),
            "confirm_project_impl should push at least one log line"
        );
        for line in &raw {
            assert!(
                !line.contains('\x1b'),
                "log line should not contain ANSI escape codes, got: {line:?}"
            );
        }
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
                file_select: None,
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
                file_select: None,
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
                file_select: None,
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
                file_select: None,
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
                file_select: None,
                hint: false,
                scroll_offset: 0,
                selected: None,
                version: "0.0.0",
            },
        )
        .unwrap();
    }

    // ── FileSelectState rendering tests ──

    #[test]
    fn test_render_to_with_file_select() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let steps = StepsPane::new(&["Environment", "Analysis", "Fetch ADRs", "Tailoring"]);
        let log = LogPane::new();
        let fs = FileSelectState {
            items: vec![
                "file1.md  (1 ADR)".to_string(),
                "file2.md  (2 ADRs)".to_string(),
            ],
            checked: vec![true, false],
            cursor: 0,
        };
        render_to(
            &mut terminal,
            RenderContext {
                steps: &steps,
                log: &log,
                confirm: None,
                file_select: Some(&fs),
                hint: false,
                scroll_offset: 0,
                selected: None,
                version: "0.0.0",
            },
        )
        .unwrap();
    }

    #[test]
    fn test_render_to_narrow_with_file_select() {
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let steps = StepsPane::new(&["Environment", "Analysis", "Fetch ADRs", "Tailoring"]);
        let log = LogPane::new();
        let fs = FileSelectState {
            items: vec!["file1.md  (1 ADR)".to_string()],
            checked: vec![true],
            cursor: 0,
        };
        render_to(
            &mut terminal,
            RenderContext {
                steps: &steps,
                log: &log,
                confirm: None,
                file_select: Some(&fs),
                hint: false,
                scroll_offset: 0,
                selected: None,
                version: "0.0.0",
            },
        )
        .unwrap();
    }

    // ── select_files_in_tui tests ──

    fn make_tailoring_output(file_count: usize) -> crate::tailoring::types::TailoringOutput {
        use crate::tailoring::types::{FileOutput, TailoringSummary};
        let files: Vec<FileOutput> = (1..=file_count)
            .map(|i| FileOutput {
                path: format!("file{i}.md"),
                content: format!("content {i}"),
                reasoning: format!("reason {i}"),
                adr_ids: vec![format!("adr-{i:03}")],
            })
            .collect();
        crate::tailoring::types::TailoringOutput {
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

    #[test]
    fn test_select_files_in_tui_plain_mode_delegates() {
        use crate::cli::ui::test_utils::MockTerminal;
        let mut r = TuiRenderer::new(false, true); // Plain mode
        let output = make_tailoring_output(2);
        let term = MockTerminal::with_selection_only(Some(vec![0, 1]));
        let result = r.select_files_in_tui(&output, false, &term).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_select_files_in_tui_quiet_mode_delegates() {
        use crate::cli::ui::test_utils::MockTerminal;
        let mut r = TuiRenderer::new(true, false); // Quiet mode
        let output = make_tailoring_output(2);
        let term = MockTerminal::with_selection_only(Some(vec![0, 1]));
        let result = r.select_files_in_tui(&output, false, &term).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_select_files_in_tui_force_returns_all() {
        use crate::cli::ui::test_utils::MockTerminal;
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let output = make_tailoring_output(3);
        let term = MockTerminal::with_selection_only(None);
        let result = r.select_files_in_tui(&output, true, &term).unwrap();
        assert_eq!(result.len(), 3);
        // force=true must not activate file_select_state
        assert!(r.file_select_state.is_none());
    }

    #[test]
    fn test_select_files_in_tui_empty_files() {
        use crate::cli::ui::test_utils::MockTerminal;
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let output = make_tailoring_output(0);
        let term = MockTerminal::with_selection_only(None);
        let result = r.select_files_in_tui(&output, false, &term).unwrap();
        assert!(result.is_empty());
        assert!(r.file_select_state.is_none());
    }

    #[test]
    fn test_select_files_in_tui_enter_selects_all_checked() {
        use crate::cli::ui::test_utils::MockTerminal;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let output = make_tailoring_output(2);
        let term = MockTerminal::with_selection_only(None);
        // Press Enter immediately — both files are checked by default.
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![enter]);
        let result = r
            .select_files_in_tui_impl(&output, false, &term, &mut source)
            .unwrap();
        assert_eq!(result.len(), 2);
        assert!(r.file_select_state.is_none(), "state must be cleared");
    }

    #[test]
    fn test_select_files_in_tui_space_toggles_and_enter_confirms() {
        use crate::cli::ui::test_utils::MockTerminal;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let output = make_tailoring_output(2);
        let term = MockTerminal::with_selection_only(None);
        // Space on item 0 unchecks it, then Enter confirms item 1 only.
        let space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![space, enter]);
        let result = r
            .select_files_in_tui_impl(&output, false, &term, &mut source)
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, "file2.md");
    }

    #[test]
    fn test_select_files_in_tui_down_up_navigation() {
        use crate::cli::ui::test_utils::MockTerminal;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let output = make_tailoring_output(3);
        let term = MockTerminal::with_selection_only(None);
        // Down → cursor=1; Down → cursor=2; Space (uncheck item 2); Up → cursor=1;
        // Space (uncheck item 1); Enter → only item 0 selected.
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        let space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![down, down, space, up, space, enter]);
        let result = r
            .select_files_in_tui_impl(&output, false, &term, &mut source)
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, "file1.md");
    }

    #[test]
    fn test_select_files_in_tui_esc_cancels() {
        use crate::cli::ui::test_utils::MockTerminal;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let output = make_tailoring_output(2);
        let term = MockTerminal::with_selection_only(None);
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![esc]);
        let result = r.select_files_in_tui_impl(&output, false, &term, &mut source);
        assert!(
            matches!(result, Err(crate::error::ActualError::UserCancelled)),
            "Esc should return UserCancelled"
        );
        assert!(r.file_select_state.is_none());
    }

    #[test]
    fn test_select_files_in_tui_q_cancels() {
        use crate::cli::ui::test_utils::MockTerminal;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let output = make_tailoring_output(2);
        let term = MockTerminal::with_selection_only(None);
        let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![q]);
        let result = r.select_files_in_tui_impl(&output, false, &term, &mut source);
        assert!(
            matches!(result, Err(crate::error::ActualError::UserCancelled)),
            "q should return UserCancelled"
        );
    }

    #[test]
    fn test_select_files_in_tui_ctrl_c_cancels() {
        use crate::cli::ui::test_utils::MockTerminal;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let output = make_tailoring_output(2);
        let term = MockTerminal::with_selection_only(None);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let mut source = MockEventSource::new(vec![ctrl_c]);
        let result = r.select_files_in_tui_impl(&output, false, &term, &mut source);
        assert!(
            matches!(result, Err(crate::error::ActualError::UserCancelled)),
            "Ctrl-C should return UserCancelled"
        );
    }

    #[test]
    fn test_select_files_in_tui_skipped_adrs_appear_in_log() {
        use crate::cli::ui::test_utils::MockTerminal;
        use crate::tailoring::types::SkippedAdr;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let mut output = make_tailoring_output(1);
        output.skipped_adrs = vec![SkippedAdr {
            id: "adr-skip-1".to_string(),
            reason: "not applicable".to_string(),
        }];
        let term = MockTerminal::with_selection_only(None);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![enter]);
        let _ = r.select_files_in_tui_impl(&output, false, &term, &mut source);
        let log_text = r.all_log_text();
        assert!(
            log_text.contains("1 ADR(s) skipped (not applicable)"),
            "skipped ADRs must appear in the TUI log; got: {log_text}"
        );
    }

    #[test]
    fn test_select_files_in_tui_cursor_clamps_at_bottom() {
        use crate::cli::ui::test_utils::MockTerminal;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let output = make_tailoring_output(2);
        let term = MockTerminal::with_selection_only(None);
        // Press Down 5 times on 2 items — cursor must not go beyond 1.
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![down, down, down, down, down, enter]);
        let result = r
            .select_files_in_tui_impl(&output, false, &term, &mut source)
            .unwrap();
        // Both files should still be selected (cursor clamped, no toggle).
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_select_files_in_tui_cursor_clamps_at_top() {
        use crate::cli::ui::test_utils::MockTerminal;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let output = make_tailoring_output(2);
        let term = MockTerminal::with_selection_only(None);
        // Press Up 5 times from cursor=0 — cursor must stay at 0.
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![up, up, up, up, up, enter]);
        let result = r
            .select_files_in_tui_impl(&output, false, &term, &mut source)
            .unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_select_files_in_tui_source_exhausted_returns_error() {
        use crate::cli::ui::test_utils::MockTerminal;
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        let output = make_tailoring_output(2);
        let term = MockTerminal::with_selection_only(None);
        let mut source = MockEventSource::new(vec![]); // EOF immediately
        let result = r.select_files_in_tui_impl(&output, false, &term, &mut source);
        assert!(result.is_err());
        assert!(
            r.file_select_state.is_none(),
            "state must be cleared on error"
        );
    }

    // ── NavCmd / handle_nav_cmd tests ──

    #[test]
    fn test_handle_nav_cmd_step_up_sets_viewing_step() {
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        // Advance to step 2 so there is history to navigate.
        r.start(SyncPhase::Environment, "env");
        r.success(SyncPhase::Environment, "ok");
        r.start(SyncPhase::Analysis, "analysis");
        // active_step is now 1; navigate up.
        r.handle_nav_cmd(NavCmd::StepUp);
        assert_eq!(
            r.viewing_step,
            Some(0),
            "StepUp from 1 should set viewing_step=0"
        );
    }

    #[test]
    fn test_handle_nav_cmd_step_up_at_zero_does_nothing() {
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        r.start(SyncPhase::Environment, "env");
        // active_step=0, viewing_step=None → cur=0; StepUp should be a no-op.
        r.handle_nav_cmd(NavCmd::StepUp);
        assert_eq!(
            r.viewing_step, None,
            "StepUp at step 0 should leave viewing_step as None"
        );
    }

    #[test]
    fn test_handle_nav_cmd_step_down_snaps_to_follow_mode_at_active() {
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        r.start(SyncPhase::Environment, "env");
        r.success(SyncPhase::Environment, "ok");
        r.start(SyncPhase::Analysis, "analysis");
        // Navigate up, then back down to active_step — should snap to follow mode.
        r.handle_nav_cmd(NavCmd::StepUp);
        assert_eq!(r.viewing_step, Some(0));
        r.handle_nav_cmd(NavCmd::StepDown);
        assert_eq!(
            r.viewing_step, None,
            "StepDown back to active_step should restore follow mode"
        );
    }

    #[test]
    fn test_handle_nav_cmd_step_down_cannot_exceed_active_step() {
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        r.start(SyncPhase::Environment, "env");
        // active_step=0; pressing StepDown should be a no-op (cannot go past active).
        r.handle_nav_cmd(NavCmd::StepDown);
        assert_eq!(
            r.viewing_step, None,
            "StepDown past active_step should not move"
        );
    }

    #[test]
    fn test_handle_nav_cmd_scroll_up_increases_offset() {
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        r.scroll_offset = 0;
        r.handle_nav_cmd(NavCmd::ScrollUp);
        assert!(
            r.scroll_offset > 0,
            "ScrollUp should increase scroll_offset"
        );
    }

    #[test]
    fn test_handle_nav_cmd_scroll_down_decreases_offset() {
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        r.scroll_offset = 10;
        r.handle_nav_cmd(NavCmd::ScrollDown);
        assert!(
            r.scroll_offset < 10,
            "ScrollDown should decrease scroll_offset"
        );
    }

    #[test]
    fn test_handle_nav_cmd_scroll_down_at_zero_stays_at_zero() {
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        r.scroll_offset = 0;
        r.handle_nav_cmd(NavCmd::ScrollDown);
        assert_eq!(r.scroll_offset, 0, "ScrollDown at 0 should not underflow");
    }

    #[test]
    fn test_handle_nav_cmd_scroll_top_sets_max_offset() {
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        r.scroll_offset = 3;
        r.handle_nav_cmd(NavCmd::ScrollTop);
        assert_eq!(
            r.scroll_offset,
            usize::MAX,
            "ScrollTop should set scroll_offset to usize::MAX"
        );
    }

    #[test]
    fn test_handle_nav_cmd_scroll_bottom_clears_offset() {
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        r.scroll_offset = 20;
        r.handle_nav_cmd(NavCmd::ScrollBottom);
        assert_eq!(
            r.scroll_offset, 0,
            "ScrollBottom should reset scroll_offset to 0"
        );
    }

    #[test]
    fn test_apply_nav_cmds_drains_channel() {
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        // Start two phases so navigation is meaningful.
        r.start(SyncPhase::Environment, "env");
        r.success(SyncPhase::Environment, "ok");
        r.start(SyncPhase::Analysis, "analysis");

        let (tx, rx) = std::sync::mpsc::channel::<NavCmd>();
        r.set_nav_rx_opt(Some(rx));

        tx.send(NavCmd::StepUp).unwrap();
        tx.send(NavCmd::StepUp).unwrap(); // already at 0 after first — second is no-op

        r.apply_nav_cmds();
        assert_eq!(
            r.viewing_step,
            Some(0),
            "apply_nav_cmds should drain all commands"
        );
    }

    #[test]
    fn test_set_nav_rx_none_clears_channel() {
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        // Set a channel, then clear it with None — apply_nav_cmds must be a no-op.
        let (_tx, rx) = std::sync::mpsc::channel::<NavCmd>();
        r.set_nav_rx_opt(Some(rx));
        r.set_nav_rx_opt(None);
        r.apply_nav_cmds(); // must not panic
        assert_eq!(r.viewing_step, None);
    }

    #[test]
    fn test_set_nav_rx_no_channel_does_not_panic() {
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        // No nav_rx set — apply_nav_cmds must not panic.
        r.apply_nav_cmds();
    }

    #[test]
    fn test_hint_enabled_when_second_phase_starts() {
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        r.start(SyncPhase::Environment, "env");
        assert!(!r.hint, "hint should be false before first phase advances");
        r.success(SyncPhase::Environment, "ok");
        r.start(SyncPhase::Analysis, "analysis");
        assert!(r.hint, "hint should be true once active_step > 0");
    }

    #[test]
    fn test_viewing_step_cleared_on_wait_for_keypress() {
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        r.start(SyncPhase::Environment, "env");
        r.success(SyncPhase::Environment, "ok");
        r.start(SyncPhase::Analysis, "analysis");
        r.viewing_step = Some(0);

        // Feed 'q' to immediately exit wait_for_keypress_impl.
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![q]);
        r.wait_for_keypress_impl(&mut source);

        assert_eq!(
            r.viewing_step, None,
            "viewing_step must be cleared when entering post-sync review mode"
        );
    }

    #[test]
    fn test_review_mode_u_scrolls_up() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        // Push enough content to make scrolling possible.
        for i in 0..50 {
            r.logs[0].push(format!("line {i}"));
        }
        r.start(SyncPhase::Environment, "env");

        // Press 'u' to scroll up, then 'q' to exit.
        let u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE);
        let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![u, q]);
        r.wait_for_keypress_impl(&mut source);

        // scroll_offset is reset to 0 on exit, but we verify no panic occurred
        // and events were consumed.
        assert!(source.events.is_empty(), "all events should be consumed");
    }

    #[test]
    fn test_review_mode_d_scrolls_down() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        for i in 0..50 {
            r.logs[0].push(format!("line {i}"));
        }
        r.start(SyncPhase::Environment, "env");

        // Press 'u' to scroll up, then 'd' to scroll back down, then 'q'.
        let u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE);
        let d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
        let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![u, d, q]);
        r.wait_for_keypress_impl(&mut source);

        assert!(source.events.is_empty(), "all events should be consumed");
    }

    #[test]
    fn test_review_mode_g_jumps_to_top() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        for i in 0..50 {
            r.logs[0].push(format!("line {i}"));
        }
        r.start(SyncPhase::Environment, "env");

        // Press 'g' (Home/top), then 'q'.
        let g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);
        let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![g, q]);
        r.wait_for_keypress_impl(&mut source);

        assert!(source.events.is_empty(), "all events should be consumed");
    }

    #[test]
    fn test_review_mode_shift_g_jumps_to_bottom() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        for i in 0..50 {
            r.logs[0].push(format!("line {i}"));
        }
        r.start(SyncPhase::Environment, "env");

        // Press 'g' to jump to top, then 'G' to bottom, then 'q'.
        let g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);
        let big_g = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
        let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![g, big_g, q]);
        r.wait_for_keypress_impl(&mut source);

        assert!(source.events.is_empty(), "all events should be consumed");
    }

    // ── steps_elapsed tests ──

    #[test]
    fn test_steps_elapsed_sums_completed_steps() {
        let mut r = TuiRenderer::new(false, true); // Plain mode
                                                   // Manually set elapsed on steps to get deterministic values.
        r.steps.steps[0].elapsed = Some(Duration::from_secs_f64(1.5));
        r.steps.steps[1].elapsed = Some(Duration::from_secs_f64(2.0));
        r.steps.steps[2].elapsed = Some(Duration::from_secs_f64(0.5));
        // Steps 3 and 4 have no elapsed (not yet completed).
        let total = r.steps_elapsed();
        let expected = Duration::from_secs_f64(1.5)
            + Duration::from_secs_f64(2.0)
            + Duration::from_secs_f64(0.5);
        assert_eq!(
            total, expected,
            "steps_elapsed should sum all completed step durations"
        );
    }

    #[test]
    fn test_steps_elapsed_returns_zero_when_no_steps_completed() {
        let r = TuiRenderer::new(false, true);
        assert_eq!(
            r.steps_elapsed(),
            Duration::ZERO,
            "steps_elapsed should be zero when no steps have elapsed"
        );
    }

    #[test]
    fn test_steps_elapsed_excludes_none_steps() {
        let mut r = TuiRenderer::new(false, true);
        r.steps.steps[0].elapsed = Some(Duration::from_millis(100));
        // steps[1] has no elapsed
        r.steps.steps[2].elapsed = Some(Duration::from_millis(200));
        let total = r.steps_elapsed();
        assert_eq!(total, Duration::from_millis(300));
    }

    #[test]
    fn test_review_mode_up_down_navigates_steps() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let backend = TestBackend::new(100, 30);
        let terminal = Terminal::new(backend).unwrap();
        let mut r = TuiRenderer::new_with_tui(terminal);
        r.start(SyncPhase::Environment, "env");
        r.success(SyncPhase::Environment, "ok");
        r.start(SyncPhase::Analysis, "analysis");
        r.success(SyncPhase::Analysis, "ok");

        // Press Up to go to step 0, Down to go back, then 'q'.
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        let mut source = MockEventSource::new(vec![up, down, q]);
        r.wait_for_keypress_impl(&mut source);

        assert!(source.events.is_empty(), "all events should be consumed");
    }
}
