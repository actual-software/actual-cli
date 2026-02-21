use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;

use crate::TuiTestError;

/// A wrapper around a PTY process that provides a simplified interface
/// for spawning commands and interacting with them.
pub(crate) struct PtyProcess {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    /// Reader for the PTY output — used by the VT100 parser in later tasks.
    #[allow(dead_code)]
    pub(crate) reader: Box<dyn Read + Send>,
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
            reader,
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
    /// Used by the VT100 parser in later tasks.
    #[allow(dead_code)]
    pub fn try_read(&mut self, buf: &mut [u8]) -> Result<usize, TuiTestError> {
        self.reader.read(buf).map_err(TuiTestError::Io)
    }

    /// Check if the process has exited without blocking.
    /// Returns `Some(ExitStatus)` if the process has exited.
    pub fn try_wait(&mut self) -> Option<portable_pty::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Wait for the process to exit and return its exit status.
    ///
    /// Used by the VT100 parser in later tasks.
    #[allow(dead_code)]
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
