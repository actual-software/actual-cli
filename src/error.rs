use thiserror::Error;

#[derive(Error, Debug)]
pub enum ActualError {
    #[error("Claude Code not found. Install with: npm install -g @anthropic-ai/claude-code")]
    ClaudeNotFound,

    #[error("Claude Code not authenticated. Run: claude auth login")]
    ClaudeNotAuthenticated,

    #[error("Claude Code subprocess failed: {message}")]
    ClaudeSubprocessFailed { message: String, stderr: String },

    #[error("Failed to parse Claude Code output: {0}")]
    ClaudeOutputParse(#[from] serde_json::Error),

    #[error("API request failed: {0}")]
    ApiError(String),

    #[error("API returned error: {code}: {message}")]
    ApiResponseError { code: String, message: String },

    #[error("Failed to write CLAUDE.md: {0}")]
    FileWriteError(#[from] std::io::Error),

    #[error("Config error: {0}")]
    ConfigError(String),

    #[error("User cancelled")]
    UserCancelled,
}

impl ActualError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::UserCancelled => 4,
            Self::ClaudeNotFound | Self::ClaudeNotAuthenticated => 2,
            Self::ApiError(_) | Self::ApiResponseError { .. } => 3,
            Self::FileWriteError(_) => 5,
            _ => 1,
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
            ActualError::FileWriteError(std::io::Error::new(std::io::ErrorKind::Other, "test"))
                .exit_code(),
            5
        );
        assert_eq!(
            ActualError::ConfigError("bad key".to_string()).exit_code(),
            1
        );
        assert_eq!(ActualError::UserCancelled.exit_code(), 4);
    }

    #[test]
    fn test_display_messages() {
        let msg = ActualError::ClaudeNotFound.to_string();
        assert!(
            msg.contains("npm install"),
            "expected 'npm install' in: {msg}"
        );
        assert!(
            msg.contains("@anthropic-ai/claude-code"),
            "expected '@anthropic-ai/claude-code' in: {msg}"
        );

        let msg = ActualError::ClaudeNotAuthenticated.to_string();
        assert!(
            msg.contains("claude auth login"),
            "expected 'claude auth login' in: {msg}"
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

        let msg = ActualError::UserCancelled.to_string();
        assert!(msg.contains("cancelled"), "expected 'cancelled' in: {msg}");
    }
}
