use thiserror::Error;

#[derive(Error, Debug)]
pub enum ActualError {
    #[error("Claude Code is not installed")]
    ClaudeNotFound,

    /// The Codex CLI binary was not found.
    #[error("Codex CLI (codex) not found. Install with: npm install -g @openai/codex")]
    CodexNotFound,

    #[error("Claude Code is not authenticated")]
    ClaudeNotAuthenticated,

    #[error("Claude Code subprocess failed: {message}")]
    ClaudeSubprocessFailed { message: String, stderr: String },

    #[error("Failed to parse Claude Code output: {0}")]
    ClaudeOutputParse(#[from] serde_json::Error),

    #[error("Analysis returned no projects")]
    AnalysisEmpty,

    #[error("API request failed: {0}")]
    ApiError(String),

    #[error("API returned error: {code}: {message}")]
    ApiResponseError { code: String, message: String },

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Config error: {0}")]
    ConfigError(String),

    #[error("Claude Code subprocess timed out after {seconds}s")]
    ClaudeTimeout { seconds: u64 },

    #[error("User cancelled")]
    UserCancelled,

    #[error("Tailoring output validation failed: {0}")]
    TailoringValidationError(String),

    #[error("Internal error: {0}")]
    InternalError(String),

    #[error("Terminal I/O error: {0}")]
    TerminalIOError(String),
}

impl ActualError {
    /// Convenience constructor for `ClaudeSubprocessFailed` with an empty stderr.
    pub(crate) fn subprocess_error(message: String) -> Self {
        Self::ClaudeSubprocessFailed {
            message,
            stderr: String::new(),
        }
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            Self::UserCancelled => 4,
            Self::ClaudeNotFound | Self::ClaudeNotAuthenticated | Self::CodexNotFound => 2,
            Self::ApiError(_) | Self::ApiResponseError { .. } => 3,
            Self::IoError(_) => 5,
            _ => 1,
        }
    }

    /// Returns a human-friendly fix suggestion for this error, if available.
    pub fn hint(&self) -> Option<&str> {
        match self {
            Self::ClaudeNotFound => Some("npm install -g @anthropic-ai/claude-code"),
            Self::CodexNotFound => Some("npm install -g @openai/codex"),
            Self::ClaudeNotAuthenticated => Some("claude auth login"),
            Self::ConfigError(_) => Some("Check ~/.config/actual/config.yaml"),
            Self::ClaudeTimeout { .. } => {
                Some("Try increasing the timeout or check Claude Code status")
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exit_codes() {
        assert_eq!(ActualError::ClaudeNotFound.exit_code(), 2);
        assert_eq!(ActualError::ClaudeNotAuthenticated.exit_code(), 2);
        assert_eq!(ActualError::CodexNotFound.exit_code(), 2);
        assert_eq!(
            ActualError::ClaudeSubprocessFailed {
                message: "fail".to_string(),
                stderr: "err".to_string(),
            }
            .exit_code(),
            1
        );
        assert_eq!(
            ActualError::ClaudeOutputParse(serde_json::from_str::<()>("invalid").unwrap_err())
                .exit_code(),
            1
        );
        assert_eq!(ActualError::ApiError("timeout".to_string()).exit_code(), 3);
        assert_eq!(
            ActualError::ApiResponseError {
                code: "401".to_string(),
                message: "unauthorized".to_string(),
            }
            .exit_code(),
            3
        );
        assert_eq!(
            ActualError::IoError(std::io::Error::new(std::io::ErrorKind::Other, "test"))
                .exit_code(),
            5
        );
        assert_eq!(
            ActualError::ConfigError("bad key".to_string()).exit_code(),
            1
        );
        assert_eq!(ActualError::ClaudeTimeout { seconds: 30 }.exit_code(), 1);
        assert_eq!(ActualError::AnalysisEmpty.exit_code(), 1);
        assert_eq!(ActualError::UserCancelled.exit_code(), 4);
        assert_eq!(
            ActualError::TailoringValidationError("test".to_string()).exit_code(),
            1
        );
        assert_eq!(
            ActualError::InternalError("test".to_string()).exit_code(),
            1
        );
        assert_eq!(
            ActualError::TerminalIOError("test".to_string()).exit_code(),
            1
        );
    }

    #[test]
    fn test_display_messages() {
        let msg = ActualError::ClaudeNotFound.to_string();
        assert!(
            msg.contains("not installed"),
            "expected 'not installed' in: {msg}"
        );

        let msg = ActualError::CodexNotFound.to_string();
        assert!(msg.contains("not found"), "expected 'not found' in: {msg}");
        assert!(
            msg.contains("@openai/codex"),
            "expected '@openai/codex' in: {msg}"
        );

        let msg = ActualError::ClaudeNotAuthenticated.to_string();
        assert!(
            msg.contains("not authenticated"),
            "expected 'not authenticated' in: {msg}"
        );

        let msg = ActualError::ClaudeSubprocessFailed {
            message: "oops".to_string(),
            stderr: "...".to_string(),
        }
        .to_string();
        assert!(msg.contains("oops"), "expected 'oops' in: {msg}");

        let msg = ActualError::ApiError("timeout".to_string()).to_string();
        assert!(msg.contains("timeout"), "expected 'timeout' in: {msg}");

        let msg = ActualError::ApiResponseError {
            code: "401".to_string(),
            message: "unauthorized".to_string(),
        }
        .to_string();
        assert!(msg.contains("401"), "expected '401' in: {msg}");
        assert!(
            msg.contains("unauthorized"),
            "expected 'unauthorized' in: {msg}"
        );

        let msg = ActualError::ConfigError("bad key".to_string()).to_string();
        assert!(msg.contains("bad key"), "expected 'bad key' in: {msg}");

        let msg = ActualError::ClaudeTimeout { seconds: 30 }.to_string();
        assert!(msg.contains("30"), "expected '30' in: {msg}");
        assert!(msg.contains("timed out"), "expected 'timed out' in: {msg}");

        let msg = ActualError::AnalysisEmpty.to_string();
        assert!(
            msg.contains("no projects"),
            "expected 'no projects' in: {msg}"
        );

        let msg = ActualError::UserCancelled.to_string();
        assert!(msg.contains("cancelled"), "expected 'cancelled' in: {msg}");

        let msg = ActualError::TailoringValidationError("empty content".to_string()).to_string();
        assert!(
            msg.contains("Tailoring output validation failed"),
            "expected 'Tailoring output validation failed' in: {msg}"
        );
        assert!(
            msg.contains("empty content"),
            "expected 'empty content' in: {msg}"
        );

        let msg = ActualError::IoError(std::io::Error::new(std::io::ErrorKind::Other, "disk full"))
            .to_string();
        assert!(msg.contains("I/O error"), "expected 'I/O error' in: {msg}");
        assert!(msg.contains("disk full"), "expected 'disk full' in: {msg}");

        let msg = ActualError::InternalError("runtime failed".to_string()).to_string();
        assert!(
            msg.contains("Internal error"),
            "expected 'Internal error' in: {msg}"
        );
        assert!(
            msg.contains("runtime failed"),
            "expected 'runtime failed' in: {msg}"
        );

        let msg = ActualError::TerminalIOError("broken pipe".to_string()).to_string();
        assert!(
            msg.contains("Terminal I/O error"),
            "expected 'Terminal I/O error' in: {msg}"
        );
        assert!(
            msg.contains("broken pipe"),
            "expected 'broken pipe' in: {msg}"
        );
    }

    #[test]
    fn test_hint_claude_not_found() {
        assert_eq!(
            ActualError::ClaudeNotFound.hint(),
            Some("npm install -g @anthropic-ai/claude-code")
        );
    }

    #[test]
    fn test_hint_codex_not_found() {
        assert_eq!(
            ActualError::CodexNotFound.hint(),
            Some("npm install -g @openai/codex")
        );
    }

    #[test]
    fn test_hint_claude_not_authenticated() {
        assert_eq!(
            ActualError::ClaudeNotAuthenticated.hint(),
            Some("claude auth login")
        );
    }

    #[test]
    fn test_hint_config_error() {
        assert_eq!(
            ActualError::ConfigError("test".to_string()).hint(),
            Some("Check ~/.config/actual/config.yaml")
        );
    }

    #[test]
    fn test_hint_claude_timeout() {
        assert_eq!(
            ActualError::ClaudeTimeout { seconds: 30 }.hint(),
            Some("Try increasing the timeout or check Claude Code status")
        );
    }

    #[test]
    fn test_hint_none_for_user_cancelled() {
        assert_eq!(ActualError::UserCancelled.hint(), None);
    }

    #[test]
    fn test_hint_none_for_api_error() {
        assert_eq!(ActualError::ApiError("test".to_string()).hint(), None);
    }

    #[test]
    fn test_hint_none_for_internal_error() {
        assert_eq!(ActualError::InternalError("test".to_string()).hint(), None);
    }

    #[test]
    fn test_hint_none_for_terminal_io_error() {
        assert_eq!(
            ActualError::TerminalIOError("test".to_string()).hint(),
            None
        );
    }

    #[test]
    fn test_subprocess_error_helper() {
        let err = ActualError::subprocess_error("spawn failed".to_string());
        assert!(matches!(
            &err,
            ActualError::ClaudeSubprocessFailed { message, stderr }
                if message == "spawn failed" && stderr.is_empty()
        ));
    }
}
