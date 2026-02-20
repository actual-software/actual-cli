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

/// A guard that keeps the keyboard poller thread alive.
/// Drop it (or call `stop_and_join`) to cleanly shut the thread down.
pub struct KbPoller {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<()>,
}

impl KbPoller {
    /// Signal the poller thread to stop and wait for it to exit.
    pub fn stop_and_join(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.join();
    }
}

/// Spawn a background keyboard-poller thread.
///
/// Returns a `(KbPoller, oneshot::Receiver<()>)` pair.  The receiver fires
/// when the user presses a quit key (`q`, `Q`, `Esc`, `Ctrl+C`).
///
/// Call [`KbPoller::stop_and_join`] once tailoring completes (regardless of
/// whether the user cancelled) to cleanly shut the thread down.
pub fn spawn() -> (KbPoller, oneshot::Receiver<()>) {
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

    (KbPoller { stop, handle }, rx)
}
