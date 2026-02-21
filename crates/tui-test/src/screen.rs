use std::io::Read;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::TuiTestError;

/// Internal state shared between the reader thread and the `ScreenBuffer`.
struct ScreenState {
    parser: vt100::Parser,
    generation: u64,
    prev_screen: vt100::Screen,
    reader_done: bool,
}

/// An immutable snapshot of the terminal screen at a point in time.
///
/// Created by `ScreenBuffer::snapshot()`. Contains a copy of the screen
/// state and the generation counter at the time the snapshot was taken.
pub struct ScreenSnapshot {
    screen: vt100::Screen,
    generation: u64,
}

impl ScreenSnapshot {
    /// Return the full text contents of the screen.
    pub fn contents(&self) -> String {
        self.screen.contents()
    }

    /// Check whether the screen text contains the given substring.
    pub fn contains(&self, text: &str) -> bool {
        self.screen.contents().contains(text)
    }

    /// Return the text content of a single row (0-indexed).
    ///
    /// Returns an empty string if the row is out of bounds.
    pub fn row_text(&self, row: u16) -> String {
        let (rows, cols) = self.screen.size();
        if row >= rows {
            return String::new();
        }
        self.screen.contents_between(row, 0, row, cols)
    }

    /// Return the terminal size as `(rows, cols)`.
    pub fn size(&self) -> (u16, u16) {
        self.screen.size()
    }

    /// Return the current cursor position as `(row, col)`.
    pub fn cursor_position(&self) -> (u16, u16) {
        self.screen.cursor_position()
    }

    /// Return a reference to the cell at `(row, col)`.
    pub fn cell(&self, row: u16, col: u16) -> Option<&vt100::Cell> {
        self.screen.cell(row, col)
    }

    /// Return the generation counter at the time this snapshot was taken.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Return a reference to the underlying `vt100::Screen`.
    pub fn screen(&self) -> &vt100::Screen {
        &self.screen
    }

    /// Return the text contents between two positions on the screen.
    pub fn contents_between(
        &self,
        start_row: u16,
        start_col: u16,
        end_row: u16,
        end_col: u16,
    ) -> String {
        self.screen
            .contents_between(start_row, start_col, end_row, end_col)
    }
}

/// A VT100 screen buffer that continuously parses PTY output in a background thread.
///
/// Detects screen changes using `contents_diff()` and notifies waiting threads
/// via a `Condvar` when the screen content changes.
pub(crate) struct ScreenBuffer {
    state: Arc<Mutex<ScreenState>>,
    changed: Arc<Condvar>,
    reader_thread: Option<JoinHandle<()>>,
}

impl ScreenBuffer {
    /// Create a new `ScreenBuffer` that reads from the given reader and parses
    /// VT100 output into a virtual terminal of the specified size.
    ///
    /// Spawns a background thread that continuously reads from the reader.
    pub fn new(reader: Box<dyn Read + Send>, rows: u16, cols: u16) -> Self {
        let parser = vt100::Parser::new(rows, cols, 0);
        let prev_screen = parser.screen().clone();
        let state = Arc::new(Mutex::new(ScreenState {
            parser,
            generation: 0,
            prev_screen,
            reader_done: false,
        }));
        let changed = Arc::new(Condvar::new());

        let thread_state = Arc::clone(&state);
        let thread_changed = Arc::clone(&changed);
        let reader_thread = std::thread::spawn(move || {
            reader_loop(reader, thread_state, thread_changed);
        });

        Self {
            state,
            changed,
            reader_thread: Some(reader_thread),
        }
    }

    /// Take an immutable snapshot of the current screen state.
    pub fn snapshot(&self) -> ScreenSnapshot {
        let state = self.state.lock().unwrap();
        ScreenSnapshot {
            screen: state.parser.screen().clone(),
            generation: state.generation,
        }
    }

    /// Return the full text contents of the current screen.
    pub fn contents(&self) -> String {
        let state = self.state.lock().unwrap();
        state.parser.screen().contents()
    }

    /// Check whether the current screen text contains the given substring.
    pub fn contains(&self, text: &str) -> bool {
        let state = self.state.lock().unwrap();
        state.parser.screen().contents().contains(text)
    }

    /// Wait until the predicate returns `true` for a screen snapshot,
    /// or until the timeout expires.
    ///
    /// The predicate is checked immediately, then re-checked each time
    /// the screen changes or the reader thread finishes.
    pub fn wait_for<F>(&self, predicate: F, timeout: Duration) -> Result<(), TuiTestError>
    where
        F: Fn(&ScreenSnapshot) -> bool,
    {
        let deadline = std::time::Instant::now() + timeout;
        let mut state = self.state.lock().unwrap();

        loop {
            let snap = ScreenSnapshot {
                screen: state.parser.screen().clone(),
                generation: state.generation,
            };
            if predicate(&snap) {
                return Ok(());
            }

            if state.reader_done {
                // Reader is done and predicate wasn't satisfied — give one last check
                // (already checked above), so return timeout
                return Err(TuiTestError::Timeout(timeout));
            }

            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(TuiTestError::Timeout(timeout));
            }

            let (new_state, wait_result) = self.changed.wait_timeout(state, remaining).unwrap();
            state = new_state;

            if wait_result.timed_out() {
                // One final check after timeout
                let snap = ScreenSnapshot {
                    screen: state.parser.screen().clone(),
                    generation: state.generation,
                };
                if predicate(&snap) {
                    return Ok(());
                }
                return Err(TuiTestError::Timeout(timeout));
            }
        }
    }

    /// Check if the reader thread has finished (EOF or error).
    #[cfg(test)]
    pub fn is_reader_done(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.reader_done
    }

    /// Resize the virtual terminal.
    pub fn resize(&self, rows: u16, cols: u16) {
        let mut state = self.state.lock().unwrap();
        state.parser.set_size(rows, cols);
    }
}

impl Drop for ScreenBuffer {
    fn drop(&mut self) {
        if let Some(handle) = self.reader_thread.take() {
            // Best-effort join; if the thread panicked we don't propagate it.
            let _ = handle.join();
        }
    }
}

/// Background reader loop that continuously reads from the PTY reader
/// and feeds bytes to the VT100 parser.
fn reader_loop(
    mut reader: Box<dyn Read + Send>,
    state: Arc<Mutex<ScreenState>>,
    changed: Arc<Condvar>,
) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                // EOF
                let mut s = state.lock().unwrap();
                s.reader_done = true;
                changed.notify_all();
                break;
            }
            Ok(n) => {
                let mut s = state.lock().unwrap();
                s.parser.process(&buf[..n]);

                let diff = s.parser.screen().contents_diff(&s.prev_screen);
                if !diff.is_empty() {
                    s.generation += 1;
                    s.prev_screen = s.parser.screen().clone();
                    // Drop lock before notifying to avoid waking threads that
                    // immediately block on the mutex.
                    drop(s);
                    changed.notify_all();
                }
            }
            Err(_) => {
                // I/O error — treat as done
                let mut s = state.lock().unwrap();
                s.reader_done = true;
                changed.notify_all();
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Duration;

    /// Helper: create a pipe pair and return (writer, reader).
    fn pipe_pair() -> (os_pipe::PipeWriter, os_pipe::PipeReader) {
        let (reader, writer) = os_pipe::pipe().expect("Failed to create pipe");
        (writer, reader)
    }

    // ---------------------------------------------------------------
    // ScreenSnapshot tests
    // ---------------------------------------------------------------

    #[test]
    fn test_snapshot_contents() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"Hello, world!");
        let snap = ScreenSnapshot {
            screen: parser.screen().clone(),
            generation: 1,
        };
        assert!(snap.contents().contains("Hello, world!"));
    }

    #[test]
    fn test_snapshot_contains() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"foobar");
        let snap = ScreenSnapshot {
            screen: parser.screen().clone(),
            generation: 0,
        };
        assert!(snap.contains("foobar"));
        assert!(!snap.contains("bazqux"));
    }

    #[test]
    fn test_snapshot_row_text_valid() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"first line\r\nsecond line");
        let snap = ScreenSnapshot {
            screen: parser.screen().clone(),
            generation: 0,
        };
        let row0 = snap.row_text(0);
        assert!(
            row0.contains("first line"),
            "row 0 should contain 'first line', got: {row0:?}"
        );
        let row1 = snap.row_text(1);
        assert!(
            row1.contains("second line"),
            "row 1 should contain 'second line', got: {row1:?}"
        );
    }

    #[test]
    fn test_snapshot_row_text_out_of_bounds() {
        let parser = vt100::Parser::new(24, 80, 0);
        let snap = ScreenSnapshot {
            screen: parser.screen().clone(),
            generation: 0,
        };
        let row = snap.row_text(100);
        assert!(row.is_empty(), "Out of bounds row should be empty");
    }

    #[test]
    fn test_snapshot_size() {
        let parser = vt100::Parser::new(24, 80, 0);
        let snap = ScreenSnapshot {
            screen: parser.screen().clone(),
            generation: 0,
        };
        assert_eq!(snap.size(), (24, 80));
    }

    #[test]
    fn test_snapshot_cursor_position() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"ABC");
        let snap = ScreenSnapshot {
            screen: parser.screen().clone(),
            generation: 0,
        };
        // Cursor should be at (0, 3) after writing 3 chars on first row
        assert_eq!(snap.cursor_position(), (0, 3));
    }

    #[test]
    fn test_snapshot_cell() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        // Write 'A' with bold SGR
        parser.process(b"\x1b[1mA\x1b[0m");
        let snap = ScreenSnapshot {
            screen: parser.screen().clone(),
            generation: 0,
        };

        let cell = snap.cell(0, 0).expect("Cell (0,0) should exist");
        assert_eq!(cell.contents(), "A");
        assert!(cell.bold(), "Cell should be bold");

        // Check default colors
        assert_eq!(cell.fgcolor(), vt100::Color::Default);
        assert_eq!(cell.bgcolor(), vt100::Color::Default);
    }

    #[test]
    fn test_snapshot_cell_with_color() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        // Red foreground (color index 31 → idx 1), green background (color index 42 → idx 2)
        parser.process(b"\x1b[31;42mX\x1b[0m");
        let snap = ScreenSnapshot {
            screen: parser.screen().clone(),
            generation: 0,
        };

        let cell = snap.cell(0, 0).expect("Cell (0,0) should exist");
        assert_eq!(cell.contents(), "X");
        assert_eq!(cell.fgcolor(), vt100::Color::Idx(1));
        assert_eq!(cell.bgcolor(), vt100::Color::Idx(2));
    }

    #[test]
    fn test_snapshot_generation() {
        let parser = vt100::Parser::new(24, 80, 0);
        let snap = ScreenSnapshot {
            screen: parser.screen().clone(),
            generation: 42,
        };
        assert_eq!(snap.generation(), 42);
    }

    #[test]
    fn test_snapshot_screen_ref() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"test");
        let snap = ScreenSnapshot {
            screen: parser.screen().clone(),
            generation: 0,
        };
        // screen() should return a reference to the same screen
        assert_eq!(snap.screen().contents(), snap.contents());
    }

    #[test]
    fn test_snapshot_contents_between() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"ABCDEF");
        let snap = ScreenSnapshot {
            screen: parser.screen().clone(),
            generation: 0,
        };
        let between = snap.contents_between(0, 1, 0, 4);
        assert_eq!(between, "BCD");
    }

    // ---------------------------------------------------------------
    // ScreenBuffer tests
    // ---------------------------------------------------------------

    #[test]
    fn test_screen_buffer_reads_and_parses() {
        let (mut writer, reader) = pipe_pair();
        let buffer = ScreenBuffer::new(Box::new(reader), 24, 80);

        writer.write_all(b"Hello from pipe").unwrap();
        writer.flush().unwrap();
        // Drop writer to trigger EOF so the reader thread finishes
        drop(writer);

        // Wait for the reader thread to process
        buffer
            .wait_for(
                |snap| snap.contains("Hello from pipe"),
                Duration::from_secs(5),
            )
            .expect("Should find text in screen");

        assert!(buffer.contains("Hello from pipe"));
        assert!(buffer.contents().contains("Hello from pipe"));
    }

    #[test]
    fn test_screen_buffer_wait_for_success_delayed_write() {
        let (mut writer, reader) = pipe_pair();
        let buffer = ScreenBuffer::new(Box::new(reader), 24, 80);

        // Spawn a thread that writes after a short delay
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            writer.write_all(b"delayed text").unwrap();
            writer.flush().unwrap();
            drop(writer);
        });

        buffer
            .wait_for(|snap| snap.contains("delayed text"), Duration::from_secs(5))
            .expect("Should find delayed text");
    }

    #[test]
    fn test_screen_buffer_wait_for_timeout() {
        let (_writer, reader) = pipe_pair();
        let buffer = ScreenBuffer::new(Box::new(reader), 24, 80);

        let result = buffer.wait_for(
            |snap| snap.contains("nonexistent text"),
            Duration::from_millis(100),
        );
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TuiTestError::Timeout(_)));

        // Clean up: drop the writer so the reader thread can exit
        drop(_writer);
    }

    #[test]
    fn test_screen_buffer_generation_increments() {
        let (mut writer, reader) = pipe_pair();
        let buffer = ScreenBuffer::new(Box::new(reader), 24, 80);

        // Initial generation should be 0
        assert_eq!(buffer.snapshot().generation(), 0);

        // Write something to cause a screen change
        writer.write_all(b"change1").unwrap();
        writer.flush().unwrap();

        buffer
            .wait_for(|snap| snap.generation() >= 1, Duration::from_secs(5))
            .expect("Generation should increment");

        let gen1 = buffer.snapshot().generation();
        assert!(gen1 >= 1, "Generation should be >= 1, got: {gen1}");

        // Write more to cause another change — move to new line to ensure visual diff
        writer.write_all(b"\r\nchange2").unwrap();
        writer.flush().unwrap();

        buffer
            .wait_for(|snap| snap.generation() > gen1, Duration::from_secs(5))
            .expect("Generation should increment again");

        let gen2 = buffer.snapshot().generation();
        assert!(
            gen2 > gen1,
            "Generation should have increased: {gen1} -> {gen2}"
        );

        drop(writer);
    }

    #[test]
    fn test_screen_buffer_resize() {
        let (mut writer, reader) = pipe_pair();
        let buffer = ScreenBuffer::new(Box::new(reader), 24, 80);

        // Initial size should be 24x80
        assert_eq!(buffer.snapshot().size(), (24, 80));

        // Resize
        buffer.resize(40, 120);

        // Write something to ensure we see the new size in a snapshot
        writer.write_all(b"after resize").unwrap();
        writer.flush().unwrap();

        buffer
            .wait_for(|snap| snap.contains("after resize"), Duration::from_secs(5))
            .expect("Should find text after resize");

        assert_eq!(buffer.snapshot().size(), (40, 120));

        drop(writer);
    }

    #[test]
    fn test_screen_buffer_is_reader_done_after_eof() {
        let (writer, reader) = pipe_pair();
        let buffer = ScreenBuffer::new(Box::new(reader), 24, 80);

        assert!(!buffer.is_reader_done());

        // Drop writer to trigger EOF
        drop(writer);

        // Wait a bit for the reader thread to notice EOF
        std::thread::sleep(Duration::from_millis(100));
        assert!(buffer.is_reader_done());
    }

    #[test]
    fn test_screen_buffer_reader_done_on_error() {
        // Create a reader that returns an error immediately
        struct ErrorReader;
        impl Read for ErrorReader {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "simulated error",
                ))
            }
        }

        let buffer = ScreenBuffer::new(Box::new(ErrorReader), 24, 80);

        // Wait a bit for the reader thread to encounter the error
        std::thread::sleep(Duration::from_millis(100));
        assert!(buffer.is_reader_done());
    }

    #[test]
    fn test_screen_buffer_wait_for_reader_done_returns_timeout() {
        // When the reader is done and predicate is never satisfied,
        // wait_for should return Timeout
        let (writer, reader) = pipe_pair();
        let buffer = ScreenBuffer::new(Box::new(reader), 24, 80);

        // Drop writer immediately to trigger EOF
        drop(writer);

        // Wait for reader to finish
        std::thread::sleep(Duration::from_millis(100));

        let result = buffer.wait_for(
            |snap| snap.contains("will never appear"),
            Duration::from_secs(1),
        );
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TuiTestError::Timeout(_)));
    }

    #[test]
    fn test_screen_buffer_snapshot() {
        let (mut writer, reader) = pipe_pair();
        let buffer = ScreenBuffer::new(Box::new(reader), 24, 80);

        writer.write_all(b"snapshot test").unwrap();
        writer.flush().unwrap();

        buffer
            .wait_for(
                |snap| snap.contains("snapshot test"),
                Duration::from_secs(5),
            )
            .expect("Should find text");

        let snap = buffer.snapshot();
        assert!(snap.contains("snapshot test"));
        assert!(snap.generation() >= 1);

        drop(writer);
    }

    #[test]
    fn test_screen_buffer_wait_for_predicate_true_immediately() {
        let (mut writer, reader) = pipe_pair();
        let buffer = ScreenBuffer::new(Box::new(reader), 24, 80);

        writer.write_all(b"already here").unwrap();
        writer.flush().unwrap();

        // Wait for the text to appear
        buffer
            .wait_for(|snap| snap.contains("already here"), Duration::from_secs(5))
            .expect("Text should appear");

        // Now call wait_for again — predicate should be true immediately
        buffer
            .wait_for(
                |snap| snap.contains("already here"),
                Duration::from_millis(10),
            )
            .expect("Should succeed immediately since text is already present");

        drop(writer);
    }

    #[test]
    fn test_screen_buffer_cell_out_of_bounds() {
        let (writer, reader) = pipe_pair();
        let buffer = ScreenBuffer::new(Box::new(reader), 24, 80);
        drop(writer);

        let snap = buffer.snapshot();
        // Out of bounds cell should return None
        assert!(snap.cell(100, 100).is_none());
    }

    #[test]
    fn test_screen_buffer_wait_for_zero_remaining_timeout() {
        // Covers line 174: the `remaining.is_zero()` early-exit path.
        // Using a zero-duration timeout with the writer still open (reader NOT done)
        // forces the loop to hit the `remaining.is_zero()` check after the
        // first predicate failure.
        let (_writer, reader) = pipe_pair();
        let buffer = ScreenBuffer::new(Box::new(reader), 24, 80);

        let result = buffer.wait_for(
            |snap| snap.contains("will never appear"),
            Duration::from_nanos(0),
        );
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TuiTestError::Timeout(_)));

        drop(_writer);
    }

    #[test]
    fn test_screen_buffer_wait_for_predicate_true_after_condvar_timeout() {
        // Covers line 187: the post-condvar-timeout predicate success path.
        //
        // To reach line 187 we need:
        //   1. Predicate fails on initial check (line 162)
        //   2. reader_done is false (line 166)
        //   3. remaining > 0 (line 173)
        //   4. condvar wait_timeout returns with timed_out() == true (line 180)
        //   5. Predicate succeeds on the post-timeout check (line 186)
        //
        // The condvar is only notified on screen changes or reader_done. If
        // we use an external AtomicBool in the predicate (not screen content)
        // and flip it after a short delay, the condvar will time out (no
        // screen changes happen) but the predicate will succeed because the
        // external state changed.
        use std::sync::atomic::{AtomicBool, Ordering};

        let (_writer, reader) = pipe_pair();
        let buffer = ScreenBuffer::new(Box::new(reader), 24, 80);

        let flag = Arc::new(AtomicBool::new(false));
        let flag_clone = Arc::clone(&flag);

        // Flip the flag after a short delay. No screen changes occur, so
        // the condvar will time out.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            flag_clone.store(true, Ordering::SeqCst);
        });

        // The overall timeout is long enough for the flag to be flipped.
        // The condvar wait uses `remaining` which will be ~200ms on the
        // first iteration. It will time out (no notifications), then the
        // post-timeout predicate check will see the flag is true.
        let result = buffer.wait_for(
            |_snap| flag.load(Ordering::SeqCst),
            Duration::from_millis(200),
        );
        assert!(
            result.is_ok(),
            "Predicate should succeed in post-condvar-timeout check"
        );

        drop(_writer);
    }
}
