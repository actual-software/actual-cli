use thiserror::Error;

#[derive(Error, Debug)]
pub enum TuiTestError {
    #[error("PTY error: {0}")]
    Pty(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Timeout after {0:?}")]
    Timeout(std::time::Duration),
    #[error("Process exited with code {0:?}")]
    ProcessExited(Option<u32>),
    #[error("Session not started")]
    NotStarted,
    #[error("Screen buffer unavailable: {0}")]
    ScreenBufferUnavailable(String),
    #[error("Render error: {0}")]
    Render(String),
}
