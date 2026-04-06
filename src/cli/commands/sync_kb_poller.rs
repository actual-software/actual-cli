/// Keyboard-cancel poller for TUI mode.
///
/// During `Tailoring ADRs`, `rt.block_on()` holds the main thread while
/// crossterm raw mode is active.  OS-level SIGINT from Ctrl+C may not fire
/// because the terminal intercepts keystrokes before the signal is delivered.
/// This module spawns a background thread that polls crossterm every 100 ms
/// for `q`, `Q`, `Esc`, or `Ctrl+C` and signals cancellation via a oneshot
/// channel.
///
/// Excluded from coverage measurement because the thread body only runs on a
/// real TTY — unit tests cannot drive crossterm event I/O.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use tokio::sync::oneshot;

use crate::cli::ui::tui::renderer::NavCmd;

/// Guard returned by [`setup`].  Signals the poller thread to stop when
/// dropped.  When no thread was started (non-TUI mode) this is a no-op.
pub struct KbPoller {
    inner: Option<KbPollerInner>,
}

struct KbPollerInner {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<()>,
}

impl Drop for KbPoller {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.stop.store(true, Ordering::Relaxed);
            let _ = inner.handle.join();
        }
    }
}

/// Set up a navigation-only keyboard poller (no quit/cancel handling).
///
/// When `is_tui` is `true`, spawns a background crossterm-poller thread that
/// forwards navigation commands (arrow keys, scroll, copy, fullscreen) via
/// the returned `Receiver<NavCmd>`.  Quit keys are ignored.
///
/// When `is_tui` is `false`, returns a no-op guard and `None`.
///
/// Drop the returned [`KbPoller`] to stop the thread.
pub fn setup_nav_only(is_tui: bool) -> (KbPoller, Option<std::sync::mpsc::Receiver<NavCmd>>) {
    if is_tui {
        let (inner, nav_rx) = spawn_nav_thread();
        (KbPoller { inner: Some(inner) }, Some(nav_rx))
    } else {
        (KbPoller { inner: None }, None)
    }
}

/// Set up the keyboard-cancel poller.
///
/// When `is_tui` is `true`, spawns a background crossterm-poller thread and
/// returns a cancel future that fires on `q`/`Esc`/`Ctrl+C` **or** OS SIGINT.
/// Also returns a `Receiver<NavCmd>` for navigation commands.
///
/// When `is_tui` is `false`, returns a no-op guard and a cancel future that
/// only fires on OS SIGINT.
///
/// Drop the returned [`KbPoller`] once tailoring completes; it will signal
/// the poller thread (if any) to stop.
pub fn setup(
    is_tui: bool,
) -> (
    KbPoller,
    Option<std::sync::mpsc::Receiver<NavCmd>>,
    impl std::future::Future<Output = std::io::Result<()>>,
) {
    let (kb_poller, kb_cancel_rx, nav_rx) = if is_tui {
        let (inner, rx, nav_rx) = spawn_thread();
        (KbPoller { inner: Some(inner) }, Some(rx), Some(nav_rx))
    } else {
        (KbPoller { inner: None }, None, None)
    };

    let cancel_fut = async move {
        tokio::select! {
            r = tokio::signal::ctrl_c() => r,
            r = async {
                match kb_cancel_rx {
                    Some(rx) => rx.await.map_err(|_| std::io::Error::other("keyboard cancel channel closed")),
                    None => std::future::pending().await,
                }
            } => r,
        }
    };

    (kb_poller, nav_rx, cancel_fut)
}

/// Spawn a nav-only crossterm-poller thread (ignores quit keys).
fn spawn_nav_thread() -> (KbPollerInner, std::sync::mpsc::Receiver<NavCmd>) {
    let (nav_tx, nav_rx) = std::sync::mpsc::channel::<NavCmd>();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = stop.clone();

    let handle = std::thread::spawn(move || {
        use crossterm::event::{poll, read, Event, KeyCode};

        loop {
            if stop_flag.load(Ordering::Relaxed) {
                break;
            }
            if let Ok(true) = poll(std::time::Duration::from_millis(100)) {
                if let Ok(Event::Key(key)) = read() {
                    let nav_cmd = match key.code {
                        KeyCode::Up => Some(NavCmd::StepUp),
                        KeyCode::Down => Some(NavCmd::StepDown),
                        KeyCode::PageUp | KeyCode::Char('u') => Some(NavCmd::ScrollUp),
                        KeyCode::PageDown | KeyCode::Char('d') => Some(NavCmd::ScrollDown),
                        KeyCode::Home | KeyCode::Char('g') => Some(NavCmd::ScrollTop),
                        KeyCode::End | KeyCode::Char('G') => Some(NavCmd::ScrollBottom),
                        KeyCode::Char('y') => Some(NavCmd::CopyOutput),
                        KeyCode::Char('f') => Some(NavCmd::ToggleFullscreen),
                        _ => None,
                    };
                    if let Some(cmd) = nav_cmd {
                        let _ = nav_tx.send(cmd);
                    }
                }
            }
        }
    });

    (KbPollerInner { stop, handle }, nav_rx)
}

/// Spawn the background crossterm-poller thread (with quit/cancel support).
fn spawn_thread() -> (
    KbPollerInner,
    oneshot::Receiver<()>,
    std::sync::mpsc::Receiver<NavCmd>,
) {
    let (tx, rx) = oneshot::channel::<()>();
    let (nav_tx, nav_rx) = std::sync::mpsc::channel::<NavCmd>();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = stop.clone();

    let handle = std::thread::spawn(move || {
        use crossterm::event::{poll, read, Event, KeyCode, KeyModifiers};

        loop {
            if stop_flag.load(Ordering::Relaxed) {
                break;
            }
            // Short timeout so the thread wakes frequently to check `stop`.
            if let Ok(true) = poll(std::time::Duration::from_millis(100)) {
                if let Ok(Event::Key(key)) = read() {
                    let quit = matches!(
                        key.code,
                        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc
                    ) || matches!(
                        (key.code, key.modifiers),
                        (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL)
                    );
                    if quit {
                        let _ = tx.send(());
                        break;
                    }
                    // Navigation keys: send NavCmd events (non-blocking, ignore if receiver dropped)
                    let nav_cmd = match key.code {
                        KeyCode::Up => Some(NavCmd::StepUp),
                        KeyCode::Down => Some(NavCmd::StepDown),
                        KeyCode::PageUp | KeyCode::Char('u') => Some(NavCmd::ScrollUp),
                        KeyCode::PageDown | KeyCode::Char('d') => Some(NavCmd::ScrollDown),
                        KeyCode::Home | KeyCode::Char('g') => Some(NavCmd::ScrollTop),
                        KeyCode::End | KeyCode::Char('G') => Some(NavCmd::ScrollBottom),
                        KeyCode::Char('y') => Some(NavCmd::CopyOutput),
                        KeyCode::Char('f') => Some(NavCmd::ToggleFullscreen),
                        _ => None,
                    };
                    if let Some(cmd) = nav_cmd {
                        let _ = nav_tx.send(cmd);
                    }
                }
            }
        }
    });

    (KbPollerInner { stop, handle }, rx, nav_rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn drop_sets_flag_and_joins_thread() {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let handle = std::thread::spawn(move || {
            while !stop_clone.load(Ordering::Relaxed) {
                std::thread::yield_now();
            }
        });

        let poller = KbPoller {
            inner: Some(KbPollerInner { stop, handle }),
        };

        // Drop must set the flag and join the thread; reaching this line confirms it.
        drop(poller);
    }

    #[test]
    fn drop_is_noop_when_inner_is_none() {
        let poller = KbPoller { inner: None };
        drop(poller);
    }
}
