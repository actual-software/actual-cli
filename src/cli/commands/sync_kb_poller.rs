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

/// Guard returned by [`setup`].  Signals the poller thread to stop when
/// dropped.  When no thread was started (non-TUI mode) this is a no-op.
pub struct KbPoller {
    inner: Option<KbPollerInner>,
}

struct KbPollerInner {
    stop: Arc<AtomicBool>,
    _handle: std::thread::JoinHandle<()>,
}

impl Drop for KbPoller {
    fn drop(&mut self) {
        if let Some(inner) = &self.inner {
            inner.stop.store(true, Ordering::Relaxed);
            // The thread wakes every 100 ms and will exit after seeing the flag.
            // We don't join here because `JoinHandle::join` requires ownership.
        }
    }
}

/// Set up the keyboard-cancel poller.
///
/// When `is_tui` is `true`, spawns a background crossterm-poller thread and
/// returns a cancel future that fires on `q`/`Esc`/`Ctrl+C` **or** OS SIGINT.
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
    impl std::future::Future<Output = std::io::Result<()>>,
) {
    let (kb_poller, kb_cancel_rx) = if is_tui {
        let (inner, rx) = spawn_thread();
        (KbPoller { inner: Some(inner) }, Some(rx))
    } else {
        (KbPoller { inner: None }, None)
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

    (kb_poller, cancel_fut)
}

/// Spawn the background crossterm-poller thread.
fn spawn_thread() -> (KbPollerInner, oneshot::Receiver<()>) {
    let (tx, rx) = oneshot::channel::<()>();
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
                }
            }
        }
    });

    (
        KbPollerInner {
            stop,
            _handle: handle,
        },
        rx,
    )
}
