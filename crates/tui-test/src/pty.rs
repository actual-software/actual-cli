use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;

use crate::TuiTestError;

/// A wrapper around a PTY process that provides a simplified interface
/// for spawning commands and interacting with them.
pub(crate) struct PtyProcess {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    /// Reader for the PTY output — can be taken by ScreenBuffer for VT100 parsing.
    reader: Option<Box<dyn Read + Send>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
}

impl PtyProcess {
    /// Spawn a new process in a PTY with the given parameters.
    pub fn spawn(
        command: &str,
        args: &[String],
        cols: u16,
        rows: u16,
        env: &HashMap<String, String>,
        workdir: Option<&PathBuf>,
    ) -> Result<Self, TuiTestError> {
        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| TuiTestError::Pty(e.to_string()))?;

        let mut cmd = CommandBuilder::new(command);
        cmd.args(args);

        for (key, value) in env {
            cmd.env(key, value);
        }

        if let Some(dir) = workdir {
            cmd.cwd(dir);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| TuiTestError::Pty(e.to_string()))?;

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| TuiTestError::Pty(e.to_string()))?;

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| TuiTestError::Pty(e.to_string()))?;

        Ok(Self {
            child,
            reader: Some(reader),
            writer,
            master: pair.master,
        })
    }

    /// Write bytes to the PTY's stdin.
    pub fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), TuiTestError> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Try to read from the PTY's stdout without blocking indefinitely.
    /// Returns the number of bytes read.
    ///
    /// Returns an error if the reader has been taken by `take_reader()`.
    #[cfg(test)]
    pub fn try_read(&mut self, buf: &mut [u8]) -> Result<usize, TuiTestError> {
        match &mut self.reader {
            Some(reader) => reader.read(buf).map_err(TuiTestError::Io),
            None => Err(TuiTestError::ScreenBufferUnavailable(
                "reader has been taken".to_string(),
            )),
        }
    }

    /// Take the reader from this PTY process, transferring ownership to the caller.
    ///
    /// Returns `Some(reader)` on the first call, `None` on subsequent calls.
    /// After taking the reader, `try_read()` will return an error.
    pub fn take_reader(&mut self) -> Option<Box<dyn Read + Send>> {
        self.reader.take()
    }

    /// Check if the process has exited without blocking.
    /// Returns `Some(ExitStatus)` if the process has exited.
    pub fn try_wait(&mut self) -> Option<portable_pty::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Wait for the process to exit and return its exit status.
    #[cfg(test)]
    pub fn wait(&mut self) -> Result<portable_pty::ExitStatus, TuiTestError> {
        self.child
            .wait()
            .map_err(|e| TuiTestError::Pty(e.to_string()))
    }

    /// Resize the PTY.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), TuiTestError> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| TuiTestError::Pty(e.to_string()))
    }

    /// Kill the child process.
    pub fn kill(&mut self) -> Result<(), TuiTestError> {
        self.child
            .kill()
            .map_err(|e| TuiTestError::Pty(e.to_string()))
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    fn default_env() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn test_try_read_returns_output() {
        let mut pty = PtyProcess::spawn("/bin/cat", &[], 80, 24, &default_env(), None)
            .expect("Failed to spawn cat");

        // Write to cat's stdin — the PTY echoes input back through the master reader
        pty.write_bytes(b"hello from try_read\n")
            .expect("Failed to write");

        // PTY reader.read() blocks until data is available, so this is reliable
        let mut buf = [0u8; 1024];
        let n = pty.try_read(&mut buf).expect("Failed to try_read");
        assert!(n > 0, "Expected to read some bytes from PTY output");

        let output = String::from_utf8_lossy(&buf[..n]);
        assert!(
            output.contains("hello"),
            "Expected output to contain 'hello', got: {output:?}"
        );

        pty.kill().expect("Failed to kill cat");
    }

    #[test]
    fn test_wait_returns_exit_status() {
        let mut pty =
            PtyProcess::spawn("/bin/echo", &["done".into()], 80, 24, &default_env(), None)
                .expect("Failed to spawn echo");

        let status = pty.wait().expect("Failed to wait");
        assert_eq!(status.exit_code(), 0);
    }

    #[test]
    fn test_wait_nonzero_exit_code() {
        let mut pty = PtyProcess::spawn("/usr/bin/false", &[], 80, 24, &default_env(), None)
            .expect("Failed to spawn false");

        let status = pty.wait().expect("Failed to wait");
        assert_eq!(status.exit_code(), 1);
    }

    #[test]
    fn test_spawn_with_workdir() {
        let dir = PathBuf::from("/tmp");
        let mut pty = PtyProcess::spawn(
            "/bin/sh",
            &["-c".into(), "exit 0".into()],
            80,
            24,
            &default_env(),
            Some(&dir),
        )
        .expect("Failed to spawn with workdir");

        // Verify the process spawned and exits cleanly with a workdir set
        let status = pty.wait().expect("Failed to wait");
        assert_eq!(status.exit_code(), 0);
    }

    #[test]
    fn test_spawn_with_env() {
        let mut env = HashMap::new();
        env.insert("MY_TEST_VAR".to_string(), "test_value_42".to_string());

        // Use sh -c to check if the env var is set; exits 0 if set, 1 if not
        let mut pty = PtyProcess::spawn(
            "/bin/sh",
            &["-c".into(), "test \"$MY_TEST_VAR\" = test_value_42".into()],
            80,
            24,
            &env,
            None,
        )
        .expect("Failed to spawn sh");

        let status = pty.wait().expect("Failed to wait");
        assert_eq!(
            status.exit_code(),
            0,
            "Expected env var MY_TEST_VAR to be set to test_value_42"
        );
    }

    #[test]
    fn test_spawn_invalid_command() {
        let result = PtyProcess::spawn("/nonexistent/command", &[], 80, 24, &default_env(), None);
        assert!(result.is_err(), "Expected error for nonexistent command");
    }

    #[test]
    fn test_kill_running_process() {
        let mut pty = PtyProcess::spawn("/bin/sleep", &["60".into()], 80, 24, &default_env(), None)
            .expect("Failed to spawn sleep");

        pty.kill().expect("Failed to kill process");

        // After killing, wait should complete
        let status = pty.wait().expect("Failed to wait after kill");
        // Killed processes have non-zero exit code
        assert_ne!(status.exit_code(), 0);
    }

    #[test]
    fn test_write_bytes_and_read() {
        let mut pty = PtyProcess::spawn("/bin/cat", &[], 80, 24, &default_env(), None)
            .expect("Failed to spawn cat");

        pty.write_bytes(b"test input\n")
            .expect("Failed to write bytes");

        // reader.read() blocks until data is available
        let mut buf = [0u8; 1024];
        let n = pty.try_read(&mut buf).expect("Failed to read");
        let output = String::from_utf8_lossy(&buf[..n]);
        assert!(
            output.contains("test input"),
            "Expected 'test input' in output, got: {output:?}"
        );

        // Kill cat
        pty.kill().expect("Failed to kill cat");
    }

    #[test]
    fn test_resize() {
        let pty = PtyProcess::spawn("/bin/cat", &[], 80, 24, &default_env(), None)
            .expect("Failed to spawn cat");

        pty.resize(120, 40).expect("Failed to resize");
        pty.resize(40, 10).expect("Failed to resize again");

        // Clean up
        drop(pty);
    }

    #[test]
    fn test_try_wait_before_exit() {
        let mut pty = PtyProcess::spawn("/bin/sleep", &["60".into()], 80, 24, &default_env(), None)
            .expect("Failed to spawn sleep");

        // Process should not have exited yet
        assert!(pty.try_wait().is_none());

        // Clean up
        pty.kill().expect("Failed to kill");
    }

    #[test]
    fn test_try_wait_after_exit() {
        let mut pty =
            PtyProcess::spawn("/bin/echo", &["done".into()], 80, 24, &default_env(), None)
                .expect("Failed to spawn echo");

        // Block until the process exits
        let wait_status = pty.wait().expect("Failed to wait");
        assert_eq!(wait_status.exit_code(), 0);

        // After wait() returns, try_wait should also see the exit
        let status = pty.try_wait();
        assert!(status.is_some(), "Expected process to have exited");
        assert_eq!(status.unwrap().exit_code(), 0);
    }

    #[test]
    fn test_take_reader_returns_some_then_none() {
        let mut pty = PtyProcess::spawn("/bin/cat", &[], 80, 24, &default_env(), None)
            .expect("Failed to spawn cat");

        // First call returns Some
        let reader = pty.take_reader();
        assert!(reader.is_some(), "First take_reader should return Some");

        // Second call returns None
        let reader2 = pty.take_reader();
        assert!(reader2.is_none(), "Second take_reader should return None");

        // Clean up
        pty.kill().expect("Failed to kill");
    }

    #[test]
    fn test_try_read_after_take_reader_returns_error() {
        let mut pty = PtyProcess::spawn("/bin/cat", &[], 80, 24, &default_env(), None)
            .expect("Failed to spawn cat");

        // Take the reader
        let _reader = pty.take_reader();

        // try_read should now return an error
        let mut buf = [0u8; 1024];
        let result = pty.try_read(&mut buf);
        assert!(result.is_err(), "try_read after take_reader should error");

        let err = result.unwrap_err();
        assert!(
            matches!(err, TuiTestError::ScreenBufferUnavailable(_)),
            "Expected ScreenBufferUnavailable error, got: {err:?}"
        );

        // Clean up
        pty.kill().expect("Failed to kill");
    }
}
