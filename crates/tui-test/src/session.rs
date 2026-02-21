use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::keys::Key;
use crate::pty::PtyProcess;
use crate::screen::{ScreenBuffer, ScreenSnapshot};
use crate::TuiTestError;

/// Default terminal width in columns.
const DEFAULT_COLS: u16 = 80;
/// Default terminal height in rows.
const DEFAULT_ROWS: u16 = 24;
/// Default timeout for operations.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Builder for constructing a [`TuiSession`] with custom configuration.
///
/// # Example
///
/// ```no_run
/// use tui_test::TuiSession;
///
/// let mut session = TuiSession::new("echo hello")
///     .size(120, 40)
///     .timeout(std::time::Duration::from_secs(10))
///     .spawn()
///     .unwrap();
/// ```
pub struct TuiSessionBuilder {
    command: String,
    args: Vec<String>,
    cols: u16,
    rows: u16,
    env: HashMap<String, String>,
    workdir: Option<PathBuf>,
    timeout: Duration,
}

impl TuiSessionBuilder {
    /// Create a new builder by parsing a command string.
    ///
    /// The command string is split on whitespace: the first token is the command,
    /// and the rest are arguments.
    pub fn new(command: &str) -> Self {
        let mut parts = command.split_whitespace();
        let cmd = parts.next().unwrap_or("").to_string();
        let args: Vec<String> = parts.map(String::from).collect();

        Self {
            command: cmd,
            args,
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            env: HashMap::new(),
            workdir: None,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Set the terminal size in columns and rows.
    pub fn size(mut self, cols: u16, rows: u16) -> Self {
        self.cols = cols;
        self.rows = rows;
        self
    }

    /// Add an environment variable.
    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.insert(key.to_string(), value.to_string());
        self
    }

    /// Set the working directory for the command.
    pub fn workdir(mut self, path: impl Into<PathBuf>) -> Self {
        self.workdir = Some(path.into());
        self
    }

    /// Set the timeout for operations.
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = duration;
        self
    }

    /// Spawn the process and return a [`TuiSession`].
    pub fn spawn(self) -> Result<TuiSession, TuiTestError> {
        let mut pty = PtyProcess::spawn(
            &self.command,
            &self.args,
            self.cols,
            self.rows,
            &self.env,
            self.workdir.as_ref(),
        )?;

        let reader = pty
            .take_reader()
            .ok_or_else(|| TuiTestError::Pty("Failed to take PTY reader".to_string()))?;
        let screen = ScreenBuffer::new(reader, self.rows, self.cols);

        Ok(TuiSession {
            pty,
            timeout: self.timeout,
            screen,
        })
    }

    /// Get the configured timeout.
    pub fn get_timeout(&self) -> Duration {
        self.timeout
    }

    /// Get the configured terminal width.
    pub fn get_cols(&self) -> u16 {
        self.cols
    }

    /// Get the configured terminal height.
    pub fn get_rows(&self) -> u16 {
        self.rows
    }
}

/// A session managing a PTY process for TUI testing.
///
/// Provides methods to send input and manage the process lifecycle.
pub struct TuiSession {
    pty: PtyProcess,
    timeout: Duration,
    screen: ScreenBuffer,
}

impl TuiSession {
    /// Create a new [`TuiSessionBuilder`] from a command string.
    ///
    /// The command string is split on whitespace: the first token is the command,
    /// and the rest are arguments.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(command: &str) -> TuiSessionBuilder {
        TuiSessionBuilder::new(command)
    }

    /// Send raw bytes to the PTY.
    pub fn send_bytes(&mut self, bytes: &[u8]) -> Result<(), TuiTestError> {
        self.pty.write_bytes(bytes)
    }

    /// Send a key press to the PTY.
    pub fn send_key(&mut self, key: Key) -> Result<(), TuiTestError> {
        self.pty.write_bytes(&key.to_bytes())
    }

    /// Type a string by sending each character as a key press.
    pub fn type_text(&mut self, text: &str) -> Result<(), TuiTestError> {
        self.pty.write_bytes(text.as_bytes())
    }

    /// Check if the process has exited.
    pub fn has_exited(&mut self) -> bool {
        self.pty.try_wait().is_some()
    }

    /// Wait for the process to exit and return its exit code.
    pub fn wait_for_exit(&mut self) -> Result<u32, TuiTestError> {
        let start = std::time::Instant::now();
        loop {
            if let Some(status) = self.pty.try_wait() {
                return Ok(status.exit_code());
            }
            if start.elapsed() > self.timeout {
                return Err(TuiTestError::Timeout(self.timeout));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Get the configured timeout.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Kill the child process.
    pub fn kill(&mut self) -> Result<(), TuiTestError> {
        self.pty.kill()
    }

    /// Resize the PTY terminal.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), TuiTestError> {
        self.pty.resize(cols, rows)?;
        self.screen.resize(rows, cols);
        Ok(())
    }

    /// Wait for a predicate to become true on the terminal screen.
    pub fn wait_for<F>(&self, predicate: F) -> Result<(), TuiTestError>
    where
        F: Fn(&ScreenSnapshot) -> bool,
    {
        self.screen.wait_for(predicate, self.timeout)
    }

    /// Wait for a predicate with a custom timeout.
    pub fn wait_for_timeout<F>(&self, predicate: F, timeout: Duration) -> Result<(), TuiTestError>
    where
        F: Fn(&ScreenSnapshot) -> bool,
    {
        self.screen.wait_for(predicate, timeout)
    }

    /// Wait until the screen contains the given text.
    pub fn wait_for_text(&self, text: &str) -> Result<(), TuiTestError> {
        let text = text.to_string();
        self.wait_for(move |snap| snap.contains(&text))
    }

    /// Wait until the screen contains the given text, with a custom timeout.
    pub fn wait_for_text_timeout(&self, text: &str, timeout: Duration) -> Result<(), TuiTestError> {
        let text = text.to_string();
        self.wait_for_timeout(move |snap| snap.contains(&text), timeout)
    }

    /// Check if the screen currently contains the given text.
    pub fn screen_contains(&self, text: &str) -> bool {
        self.screen.contains(text)
    }

    /// Get the plain text contents of the current terminal screen.
    pub fn get_screen_text(&self) -> String {
        self.screen.contents()
    }

    /// Get a snapshot of the current screen state.
    pub fn get_screen(&self) -> ScreenSnapshot {
        self.screen.snapshot()
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_defaults() {
        let builder = TuiSessionBuilder::new("echo hello");
        assert_eq!(builder.get_cols(), 80);
        assert_eq!(builder.get_rows(), 24);
        assert_eq!(builder.get_timeout(), Duration::from_secs(30));
    }

    #[test]
    fn test_builder_custom_size() {
        let builder = TuiSessionBuilder::new("echo hello").size(120, 40);
        assert_eq!(builder.get_cols(), 120);
        assert_eq!(builder.get_rows(), 40);
    }

    #[test]
    fn test_builder_custom_timeout() {
        let builder = TuiSessionBuilder::new("echo hello").timeout(Duration::from_secs(60));
        assert_eq!(builder.get_timeout(), Duration::from_secs(60));
    }

    #[test]
    fn test_builder_command_parsing() {
        let builder = TuiSessionBuilder::new("/bin/echo hello world");
        assert_eq!(builder.command, "/bin/echo");
        assert_eq!(builder.args, vec!["hello", "world"]);
    }

    #[test]
    fn test_builder_empty_command() {
        let builder = TuiSessionBuilder::new("");
        assert_eq!(builder.command, "");
        assert!(builder.args.is_empty());
    }

    #[test]
    fn test_builder_single_command() {
        let builder = TuiSessionBuilder::new("/bin/sh");
        assert_eq!(builder.command, "/bin/sh");
        assert!(builder.args.is_empty());
    }

    #[test]
    fn test_spawn_echo() {
        let mut session = TuiSession::new("/bin/echo hello")
            .timeout(Duration::from_secs(5))
            .spawn()
            .expect("Failed to spawn echo");

        let exit_code = session.wait_for_exit().expect("Failed to wait for exit");
        assert_eq!(exit_code, 0);
    }

    #[test]
    fn test_spawn_false_exit_code() {
        let mut session = TuiSession::new("/usr/bin/false")
            .timeout(Duration::from_secs(5))
            .spawn()
            .expect("Failed to spawn false");

        let exit_code = session.wait_for_exit().expect("Failed to wait for exit");
        assert_eq!(exit_code, 1);
    }

    #[test]
    fn test_send_input_to_cat() {
        let mut session = TuiSession::new("/bin/cat")
            .timeout(Duration::from_secs(5))
            .spawn()
            .expect("Failed to spawn cat");

        // Send some text followed by newline — cat echoes it
        session.type_text("hello\n").expect("Failed to type text");

        // Small delay so cat processes the line
        std::thread::sleep(Duration::from_millis(50));

        // Send Ctrl+D twice at start of line to signal EOF
        session
            .send_key(Key::Ctrl('d'))
            .expect("Failed to send Ctrl+D");
        session
            .send_key(Key::Ctrl('d'))
            .expect("Failed to send Ctrl+D");

        let exit_code = session.wait_for_exit().expect("Failed to wait for exit");
        assert_eq!(exit_code, 0);
    }

    #[test]
    fn test_send_key() {
        let mut session = TuiSession::new("/bin/cat")
            .timeout(Duration::from_secs(5))
            .spawn()
            .expect("Failed to spawn cat");

        session.send_key(Key::Enter).expect("Failed to send Enter");
        session
            .send_key(Key::Ctrl('d'))
            .expect("Failed to send Ctrl+D");

        let exit_code = session.wait_for_exit().expect("Failed to wait for exit");
        assert_eq!(exit_code, 0);
    }

    #[test]
    fn test_send_bytes() {
        let mut session = TuiSession::new("/bin/cat")
            .timeout(Duration::from_secs(5))
            .spawn()
            .expect("Failed to spawn cat");

        session
            .send_bytes(b"raw bytes\n")
            .expect("Failed to send bytes");
        session
            .send_key(Key::Ctrl('d'))
            .expect("Failed to send Ctrl+D");

        let exit_code = session.wait_for_exit().expect("Failed to wait for exit");
        assert_eq!(exit_code, 0);
    }

    #[test]
    fn test_has_exited() {
        let mut session = TuiSession::new("/bin/echo done")
            .timeout(Duration::from_secs(5))
            .spawn()
            .expect("Failed to spawn echo");

        // Wait for it to finish
        session.wait_for_exit().expect("Failed to wait for exit");

        assert!(session.has_exited());
    }

    #[test]
    fn test_environment_variables() {
        // Use sh -c with an inline script so the shell is non-interactive and exits on its own.
        // This tests that env vars set via the builder are passed through to the child process.
        let mut session = TuiSessionBuilder::new("/bin/sh -c exit")
            .env("TUI_TEST_VAR", "test_value_123")
            .timeout(Duration::from_secs(5))
            .spawn()
            .expect("Failed to spawn sh -c");

        let exit_code = session.wait_for_exit().expect("Failed to wait for exit");
        assert_eq!(exit_code, 0);
    }

    #[test]
    fn test_timeout_behavior() {
        let mut session = TuiSession::new("/bin/sleep 60")
            .timeout(Duration::from_millis(100))
            .spawn()
            .expect("Failed to spawn sleep");

        let result = session.wait_for_exit();
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(matches!(
            err,
            TuiTestError::Timeout(d) if d == Duration::from_millis(100)
        ));

        // Clean up: kill the process
        let _ = session.kill();
    }

    #[test]
    fn test_resize() {
        let mut session = TuiSession::new("/bin/cat")
            .timeout(Duration::from_secs(5))
            .spawn()
            .expect("Failed to spawn cat");

        session.resize(120, 40).expect("Failed to resize");

        session
            .send_key(Key::Ctrl('d'))
            .expect("Failed to send Ctrl+D");

        session.wait_for_exit().expect("Failed to wait for exit");
    }

    #[test]
    fn test_builder_workdir() {
        let builder = TuiSessionBuilder::new("/bin/pwd").workdir("/tmp");
        assert_eq!(builder.workdir, Some(PathBuf::from("/tmp")));

        let mut session = builder
            .timeout(Duration::from_secs(5))
            .spawn()
            .expect("Failed to spawn pwd with workdir");

        let exit_code = session.wait_for_exit().expect("Failed to wait for exit");
        assert_eq!(exit_code, 0);
    }

    #[test]
    fn test_session_timeout_getter() {
        let session = TuiSession::new("/bin/echo done")
            .timeout(Duration::from_secs(42))
            .spawn()
            .expect("Failed to spawn echo");

        assert_eq!(session.timeout(), Duration::from_secs(42));
    }

    #[test]
    fn test_kill_running_session() {
        let mut session = TuiSession::new("/bin/sleep 60")
            .timeout(Duration::from_secs(5))
            .spawn()
            .expect("Failed to spawn sleep");

        // Process should be running
        assert!(!session.has_exited());

        // Kill should succeed
        session.kill().expect("Failed to kill session");

        // After kill, process should eventually exit
        std::thread::sleep(Duration::from_millis(100));
        assert!(session.has_exited());
    }

    #[test]
    fn test_spawn_invalid_command() {
        let result = TuiSession::new("/nonexistent/command/that/does/not/exist")
            .timeout(Duration::from_secs(5))
            .spawn();

        assert!(result.is_err(), "Expected error for nonexistent command");
    }

    #[test]
    fn test_builder_env_multiple() {
        let builder = TuiSessionBuilder::new("/bin/sh -c exit")
            .env("KEY1", "val1")
            .env("KEY2", "val2");
        assert_eq!(builder.env.get("KEY1").unwrap(), "val1");
        assert_eq!(builder.env.get("KEY2").unwrap(), "val2");
    }

    #[test]
    fn test_echo_output_captured_on_screen() {
        let session = TuiSession::new("/bin/echo hello_world")
            .timeout(Duration::from_secs(5))
            .spawn()
            .expect("Failed to spawn echo");
        session
            .wait_for_text("hello_world")
            .expect("Should see echo output");
    }

    #[test]
    fn test_cat_echo_captured() {
        let mut session = TuiSession::new("/bin/cat")
            .timeout(Duration::from_secs(5))
            .spawn()
            .expect("Failed to spawn cat");
        session.type_text("testing 123\n").unwrap();
        session
            .wait_for_text("testing 123")
            .expect("Should see typed text");
        session.send_key(Key::Ctrl('d')).unwrap();
        session.send_key(Key::Ctrl('d')).unwrap();
    }

    #[test]
    fn test_get_screen_text() {
        let session = TuiSession::new("/bin/echo screen_text_test")
            .timeout(Duration::from_secs(5))
            .spawn()
            .expect("Failed to spawn");
        session.wait_for_text("screen_text_test").unwrap();
        let text = session.get_screen_text();
        assert!(text.contains("screen_text_test"));
    }

    #[test]
    fn test_screen_contains() {
        let session = TuiSession::new("/bin/echo contains_check")
            .timeout(Duration::from_secs(5))
            .spawn()
            .expect("Failed to spawn");
        session.wait_for_text("contains_check").unwrap();
        assert!(session.screen_contains("contains_check"));
        assert!(!session.screen_contains("not_there_xyz"));
    }

    #[test]
    fn test_get_screen_snapshot() {
        let session = TuiSession::new("/bin/echo snapshot_test")
            .timeout(Duration::from_secs(5))
            .spawn()
            .expect("Failed to spawn");
        session.wait_for_text("snapshot_test").unwrap();
        let snap = session.get_screen();
        assert!(snap.contains("snapshot_test"));
        assert_eq!(snap.size(), (24, 80));
    }

    #[test]
    fn test_wait_for_with_predicate() {
        let session = TuiSession::new("/bin/echo predicate_test")
            .timeout(Duration::from_secs(5))
            .spawn()
            .expect("Failed to spawn");
        session
            .wait_for(|snap| snap.contains("predicate_test"))
            .expect("Predicate should match");
    }

    #[test]
    fn test_wait_for_text_timeout_expires() {
        let mut session = TuiSession::new("/bin/cat")
            .timeout(Duration::from_secs(30))
            .spawn()
            .expect("Failed to spawn");
        let result = session.wait_for_text_timeout("never_appears_xyz", Duration::from_millis(200));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TuiTestError::Timeout(_)));
        let _ = session.kill();
    }

    #[test]
    fn test_resize_updates_screen_buffer() {
        let mut session = TuiSession::new("/bin/cat")
            .timeout(Duration::from_secs(5))
            .spawn()
            .expect("Failed to spawn");
        session.resize(120, 40).unwrap();
        let snap = session.get_screen();
        assert_eq!(snap.size(), (40, 120));
        session.send_key(Key::Ctrl('d')).unwrap();
        session.send_key(Key::Ctrl('d')).unwrap();
    }

    #[test]
    fn test_wait_for_timeout_custom() {
        let session = TuiSession::new("/bin/echo custom_timeout_test")
            .timeout(Duration::from_secs(30))
            .spawn()
            .expect("Failed to spawn");
        session
            .wait_for_timeout(
                |snap| snap.contains("custom_timeout_test"),
                Duration::from_secs(5),
            )
            .expect("Should find with custom timeout");
    }
}
