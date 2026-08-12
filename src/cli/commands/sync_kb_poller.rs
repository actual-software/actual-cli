/// Unified keyboard/resize input hub for TUI mode.
///
/// Exactly one background thread owns `crossterm::event::read()` for the
/// whole sync run. Events are routed according to a shared route flag:
///
/// - **Execution** ([`renderer::ROUTE_EXEC`]): navigation keys become
///   [`NavCmd`]s, quit keys (`q`/`Q`/`Esc`/`Ctrl+C`) set the cancel signal,
///   and resizes become [`NavCmd::Redraw`].
/// - **Prompt** ([`renderer::ROUTE_PROMPT`]): every key and resize is
///   forwarded verbatim to the active prompt over the prompt channel.
///
/// This replaces the previous design where a nav-only poller thread and the
/// prompts' own blocking readers competed for the same event queue, so a key
/// pressed at a prompt could be consumed (and dropped or misrouted as a
/// scroll/copy command) by the poller thread instead.
///
/// Cancellation is a `tokio::sync::watch` channel: quit keys flip it to
/// `true`, [`wait_cancelled`] awaits it (combined with OS SIGINT), and
/// [`InputHub::is_cancelled`] allows checks between synchronous pipeline
/// stages. Raw mode prevents Ctrl+C from reliably delivering SIGINT while
/// the TUI is active, which is why the quit keys are handled here.
///
/// Excluded from coverage measurement because the thread body only runs on a
/// real TTY — unit tests cannot drive crossterm event I/O. The routing logic
/// is a pure function ([`route_event`]) and is unit-tested below.
use std::sync::{atomic::Ordering, Arc};

use crate::cli::ui::tui::renderer::{self, HubConnection, HubShared, InputEvent, NavCmd};

/// Owner of the background input thread and the cancel signal.
///
/// Dropping the hub signals the thread to stop and joins it. In non-TUI mode
/// no thread is spawned and every accessor returns an inert value.
pub struct InputHub {
    inner: Option<HubInner>,
    cancel_rx: tokio::sync::watch::Receiver<bool>,
    /// Keeps the cancel channel's sender alive in non-TUI mode so
    /// `wait_cancelled` treats the channel as "never fires" rather than
    /// "sender dropped".
    _cancel_tx: Option<tokio::sync::watch::Sender<bool>>,
    /// The renderer's half of the hub, handed over once via [`Self::connect`].
    conn: Option<HubConnection>,
}

struct HubInner {
    shared: Arc<HubShared>,
    handle: std::thread::JoinHandle<()>,
}

impl Drop for InputHub {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.shared.stop.store(true, Ordering::Relaxed);
            let _ = inner.handle.join();
        }
    }
}

impl InputHub {
    /// Hand the renderer its half of the hub (channels + shared state) in a
    /// single step. Returns `None` in non-TUI mode or if already connected.
    pub(crate) fn connect(&mut self) -> Option<HubConnection> {
        self.conn.take()
    }

    /// A clonable token that [`wait_cancelled`] can await. Each call returns
    /// an independent receiver, so multiple pipeline stages can each build
    /// their own cancel future.
    pub fn cancel_token(&self) -> tokio::sync::watch::Receiver<bool> {
        self.cancel_rx.clone()
    }

    /// Whether a quit key has been pressed. Checked between synchronous
    /// pipeline stages, where no future can be awaited.
    pub fn is_cancelled(&self) -> bool {
        *self.cancel_rx.borrow()
    }
}

/// Spawn the input hub. In non-TUI mode (`is_tui == false`) no thread is
/// started: prompts read the terminal directly (dialoguer) and cancellation
/// falls back to OS SIGINT only.
pub fn setup_input_hub(is_tui: bool) -> InputHub {
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    if !is_tui {
        return InputHub {
            inner: None,
            cancel_rx,
            _cancel_tx: Some(cancel_tx),
            conn: None,
        };
    }

    let shared = Arc::new(HubShared::new());
    let (nav_tx, nav_rx) = std::sync::mpsc::channel::<NavCmd>();
    let (key_tx, key_rx) = std::sync::mpsc::channel::<InputEvent>();

    let handle = spawn_input_thread(shared.clone(), nav_tx, key_tx, cancel_tx);

    InputHub {
        inner: Some(HubInner {
            shared: shared.clone(),
            handle,
        }),
        cancel_rx,
        _cancel_tx: None,
        conn: Some(HubConnection {
            nav_rx,
            prompt_rx: key_rx,
            shared,
        }),
    }
}

/// Resolve when the user cancels: a quit key flips the watch channel, or the
/// OS delivers SIGINT (Ctrl+C outside raw mode, `kill -INT`, ...).
///
/// If the watch sender is gone (hub dropped), the keyboard branch waits
/// forever rather than resolving spuriously.
pub async fn wait_cancelled(mut rx: tokio::sync::watch::Receiver<bool>) -> std::io::Result<()> {
    let keyboard = async move {
        loop {
            if *rx.borrow() {
                return;
            }
            if rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    };
    tokio::select! {
        r = tokio::signal::ctrl_c() => r,
        _ = keyboard => Ok(()),
    }
}

/// Where the input thread should deliver one routed event.
#[derive(Debug)]
pub(crate) enum RoutedAction {
    /// Deliver as a navigation command (execution route).
    Nav(NavCmd),
    /// Forward verbatim to the active prompt (prompt route).
    Prompt(InputEvent),
    /// Set the cancel signal (quit key on the execution route).
    Cancel,
    /// Discard (irrelevant event, e.g. focus/paste, or unbound key).
    Ignore,
}

/// Pure routing decision for one crossterm event given the current route
/// flag and fullscreen state. Extracted from the thread body so it can be
/// unit-tested.
///
/// `fullscreen` matters only for Esc on the execution route: the hint line
/// advertises "Esc exit fullscreen", so while fullscreen is active Esc must
/// leave fullscreen, not cancel the whole run.
pub(crate) fn route_event(
    route: u8,
    fullscreen: bool,
    event: &crossterm::event::Event,
) -> RoutedAction {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
    match event {
        Event::Key(key) => {
            // Terminals using the kitty keyboard protocol (and Windows)
            // report Release events too; acting on them double-fires every
            // key. Repeat is kept so held keys keep scrolling.
            if key.kind == KeyEventKind::Release {
                return RoutedAction::Ignore;
            }
            if route == renderer::ROUTE_PROMPT {
                return RoutedAction::Prompt(InputEvent::Key(*key));
            }
            if key.code == KeyCode::Esc && fullscreen {
                return RoutedAction::Nav(NavCmd::ExitFullscreen);
            }
            let quit = matches!(
                key.code,
                KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc
            ) || matches!(
                (key.code, key.modifiers),
                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL)
            );
            if quit {
                RoutedAction::Cancel
            } else if let Some(cmd) = match_nav_key(key.code) {
                RoutedAction::Nav(cmd)
            } else {
                RoutedAction::Ignore
            }
        }
        Event::Resize(_, _) => {
            if route == renderer::ROUTE_PROMPT {
                RoutedAction::Prompt(InputEvent::Resize)
            } else {
                RoutedAction::Nav(NavCmd::Redraw)
            }
        }
        _ => RoutedAction::Ignore,
    }
}

/// Map a key code to a navigation command, if applicable.
fn match_nav_key(code: crossterm::event::KeyCode) -> Option<NavCmd> {
    use crossterm::event::KeyCode;
    match code {
        KeyCode::Up => Some(NavCmd::StepUp),
        KeyCode::Down => Some(NavCmd::StepDown),
        KeyCode::PageUp | KeyCode::Char('u') => Some(NavCmd::ScrollUp),
        KeyCode::PageDown | KeyCode::Char('d') => Some(NavCmd::ScrollDown),
        KeyCode::Home | KeyCode::Char('g') => Some(NavCmd::ScrollTop),
        KeyCode::End | KeyCode::Char('G') => Some(NavCmd::ScrollBottom),
        KeyCode::Char('y') => Some(NavCmd::CopyOutput),
        KeyCode::Char('f') => Some(NavCmd::ToggleFullscreen),
        _ => None,
    }
}

/// Spawn the single crossterm-reader thread.
///
/// The thread keeps running after a cancel (repeated quit presses are
/// harmless) and stops when the hub is dropped or the renderer sets the
/// stop flag on a plain-mode fallback.
///
/// Fail-safe: if the thread exits for any reason other than a requested
/// stop (a panic, or a future bug that breaks the loop), it sets the cancel
/// signal before dying. Raw mode eats Ctrl+C's SIGINT while the TUI is
/// live, so a silently dead input thread would otherwise leave the user
/// with no cancel path at all — cancelling winds the run down instead.
fn spawn_input_thread(
    shared: Arc<HubShared>,
    nav_tx: std::sync::mpsc::Sender<NavCmd>,
    key_tx: std::sync::mpsc::Sender<InputEvent>,
    cancel_tx: tokio::sync::watch::Sender<bool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            input_loop(&shared, &nav_tx, &key_tx, &cancel_tx);
        }));
        if outcome.is_err() || !shared.stop.load(Ordering::Relaxed) {
            tracing::error!("input hub thread exited unexpectedly; requesting cancellation");
            let _ = cancel_tx.send(true);
        }
    })
}

/// The hub thread's event loop. Extracted so [`spawn_input_thread`] can wrap
/// it in the fail-safe above.
fn input_loop(
    shared: &HubShared,
    nav_tx: &std::sync::mpsc::Sender<NavCmd>,
    key_tx: &std::sync::mpsc::Sender<InputEvent>,
    cancel_tx: &tokio::sync::watch::Sender<bool>,
) {
    use crossterm::event::{poll, read};

    let mut prev_route = renderer::ROUTE_EXEC;
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            break;
        }
        // A prompt opening supersedes any quit key pressed while it was
        // still on its way to the screen: clear the cancel latch on the
        // EXEC→PROMPT transition so a keystroke racing the prompt cannot
        // poison the run after the user explicitly answers the prompt.
        // (Quit keys pressed AT the prompt are routed to the prompt and
        // handled there — they never touch this latch.)
        let cur_route = shared.route.load(Ordering::Relaxed);
        if cur_route == renderer::ROUTE_PROMPT
            && prev_route == renderer::ROUTE_EXEC
            && *cancel_tx.borrow()
        {
            tracing::debug!("prompt opened; clearing pending cancel latch");
            let _ = cancel_tx.send(false);
        }
        prev_route = cur_route;

        // Short timeout so the thread wakes frequently to check `stop`
        // and the route transition above.
        if let Ok(true) = poll(std::time::Duration::from_millis(100)) {
            if let Ok(event) = read() {
                match route_event(
                    shared.route.load(Ordering::Relaxed),
                    shared.fullscreen.load(Ordering::Relaxed),
                    &event,
                ) {
                    // Send errors mean the receiver is gone; ignore.
                    RoutedAction::Nav(cmd) => {
                        let _ = nav_tx.send(cmd);
                    }
                    RoutedAction::Prompt(ev) => {
                        let _ = key_tx.send(ev);
                    }
                    RoutedAction::Cancel => {
                        tracing::debug!("quit key latched cancel (exec route)");
                        let _ = cancel_tx.send(true);
                    }
                    RoutedAction::Ignore => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    // ── route_event: execution route ──

    #[test]
    fn exec_route_quit_keys_cancel() {
        for ev in [
            key(KeyCode::Char('q')),
            key(KeyCode::Char('Q')),
            key(KeyCode::Esc),
            ctrl('c'),
        ] {
            assert!(
                matches!(
                    route_event(renderer::ROUTE_EXEC, false, &ev),
                    RoutedAction::Cancel
                ),
                "expected Cancel for {ev:?}"
            );
        }
    }

    #[test]
    fn exec_route_nav_keys_become_nav_cmds() {
        assert!(matches!(
            route_event(renderer::ROUTE_EXEC, false, &key(KeyCode::Up)),
            RoutedAction::Nav(NavCmd::StepUp)
        ));
        assert!(matches!(
            route_event(renderer::ROUTE_EXEC, false, &key(KeyCode::Char('u'))),
            RoutedAction::Nav(NavCmd::ScrollUp)
        ));
        assert!(matches!(
            route_event(renderer::ROUTE_EXEC, false, &key(KeyCode::Char('y'))),
            RoutedAction::Nav(NavCmd::CopyOutput)
        ));
        assert!(matches!(
            route_event(renderer::ROUTE_EXEC, false, &key(KeyCode::Char('f'))),
            RoutedAction::Nav(NavCmd::ToggleFullscreen)
        ));
    }

    #[test]
    fn exec_route_unbound_key_ignored() {
        assert!(matches!(
            route_event(renderer::ROUTE_EXEC, false, &key(KeyCode::Char('x'))),
            RoutedAction::Ignore
        ));
        assert!(matches!(
            route_event(renderer::ROUTE_EXEC, false, &key(KeyCode::Enter)),
            RoutedAction::Ignore
        ));
    }

    #[test]
    fn exec_route_resize_requests_redraw() {
        assert!(matches!(
            route_event(renderer::ROUTE_EXEC, false, &Event::Resize(80, 24)),
            RoutedAction::Nav(NavCmd::Redraw)
        ));
    }

    #[test]
    fn exec_route_esc_exits_fullscreen_instead_of_cancelling() {
        // The hint line advertises "Esc exit fullscreen" — while fullscreen
        // is active Esc must NOT abort the run.
        assert!(matches!(
            route_event(renderer::ROUTE_EXEC, true, &key(KeyCode::Esc)),
            RoutedAction::Nav(NavCmd::ExitFullscreen)
        ));
        // Without fullscreen, Esc is a quit key.
        assert!(matches!(
            route_event(renderer::ROUTE_EXEC, false, &key(KeyCode::Esc)),
            RoutedAction::Cancel
        ));
        // Fullscreen does not shield the other quit keys.
        assert!(matches!(
            route_event(renderer::ROUTE_EXEC, true, &key(KeyCode::Char('q'))),
            RoutedAction::Cancel
        ));
    }

    #[test]
    fn release_events_are_ignored_on_both_routes() {
        use crossterm::event::KeyEventKind;
        let mut release = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        let ev = Event::Key(release);
        assert!(matches!(
            route_event(renderer::ROUTE_EXEC, false, &ev),
            RoutedAction::Ignore
        ));
        assert!(matches!(
            route_event(renderer::ROUTE_PROMPT, false, &ev),
            RoutedAction::Ignore
        ));
    }

    #[test]
    fn repeat_events_still_act() {
        use crossterm::event::KeyEventKind;
        let mut repeat = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE);
        repeat.kind = KeyEventKind::Repeat;
        assert!(
            matches!(
                route_event(renderer::ROUTE_EXEC, false, &Event::Key(repeat)),
                RoutedAction::Nav(NavCmd::ScrollUp)
            ),
            "held keys must keep scrolling"
        );
    }

    // ── route_event: prompt route ──

    #[test]
    fn prompt_route_forwards_all_keys_verbatim() {
        // Keys that would be quit/nav/copy on the exec route must reach the
        // prompt untouched — this is the race the hub exists to fix.
        for code in [
            KeyCode::Enter,
            KeyCode::Char('y'),
            KeyCode::Char('q'),
            KeyCode::Esc,
            KeyCode::Char('d'),
            KeyCode::Up,
        ] {
            match route_event(renderer::ROUTE_PROMPT, false, &key(code)) {
                RoutedAction::Prompt(InputEvent::Key(k)) => assert_eq!(k.code, code),
                other => panic!("expected Prompt(Key) for {code:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn prompt_route_ctrl_c_forwarded_to_prompt() {
        // Prompts implement their own Ctrl+C handling (UserCancelled).
        assert!(matches!(
            route_event(renderer::ROUTE_PROMPT, false, &ctrl('c')),
            RoutedAction::Prompt(InputEvent::Key(_))
        ));
    }

    #[test]
    fn prompt_route_resize_forwarded_as_resize() {
        assert!(matches!(
            route_event(renderer::ROUTE_PROMPT, false, &Event::Resize(80, 24)),
            RoutedAction::Prompt(InputEvent::Resize)
        ));
    }

    #[test]
    fn non_key_events_ignored_on_exec_route() {
        assert!(matches!(
            route_event(renderer::ROUTE_EXEC, false, &Event::FocusGained),
            RoutedAction::Ignore
        ));
    }

    // ── hub in non-TUI mode ──

    #[test]
    fn non_tui_hub_is_inert() {
        let mut hub = setup_input_hub(false);
        assert!(hub.connect().is_none());
        assert!(!hub.is_cancelled());
        drop(hub);
    }

    // ── wait_cancelled ──

    #[tokio::test]
    async fn wait_cancelled_resolves_when_watch_fires() {
        let (tx, rx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn(wait_cancelled(rx));
        tx.send(true).expect("receiver alive");
        let result = handle.await.expect("task completes");
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn wait_cancelled_resolves_immediately_when_already_set() {
        let (tx, rx) = tokio::sync::watch::channel(true);
        let result = wait_cancelled(rx).await;
        assert!(result.is_ok());
        drop(tx);
    }

    #[tokio::test]
    async fn wait_cancelled_pends_when_sender_dropped() {
        // A dropped sender must NOT resolve the future (that would cancel a
        // run spuriously); it should stay pending until the timeout.
        let (tx, rx) = tokio::sync::watch::channel(false);
        drop(tx);
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(50), wait_cancelled(rx)).await;
        assert!(result.is_err(), "expected timeout, got {result:?}");
    }

    #[tokio::test]
    async fn cancel_token_clones_are_independent() {
        let (tx, rx) = tokio::sync::watch::channel(false);
        let hub = InputHub {
            inner: None,
            cancel_rx: rx,
            _cancel_tx: None,
            conn: None,
        };
        let token_a = hub.cancel_token();
        let token_b = hub.cancel_token();
        tx.send(true).expect("receivers alive");
        assert!(wait_cancelled(token_a).await.is_ok());
        assert!(wait_cancelled(token_b).await.is_ok());
        assert!(hub.is_cancelled());
    }

    // ── drop semantics ──

    #[test]
    fn drop_sets_flag_and_joins_thread() {
        let shared = Arc::new(HubShared::new());
        let shared_clone = shared.clone();
        let handle = std::thread::spawn(move || {
            while !shared_clone.stop.load(Ordering::Relaxed) {
                std::thread::yield_now();
            }
        });

        let (_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let hub = InputHub {
            inner: Some(HubInner { shared, handle }),
            cancel_rx,
            _cancel_tx: None,
            conn: None,
        };

        // Drop must set the flag and join the thread; reaching the next line
        // confirms it.
        drop(hub);
    }
}
