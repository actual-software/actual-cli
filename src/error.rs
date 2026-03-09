use std::borrow::Cow;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ActualError {
    #[error("Claude Code is not installed")]
    ClaudeNotFound,

    /// The Codex CLI binary was not found.
    #[error("Codex CLI (codex) not found. Install with: npm install -g @openai/codex")]
    CodexNotFound,

    /// The Cursor agent CLI binary was not found.
    #[error("Cursor agent CLI (cursor-agent) not found. Install from: https://cursor.com/install")]
    CursorNotFound,

    #[error("Claude Code is not authenticated")]
    ClaudeNotAuthenticated,

    #[error("Codex CLI is not authenticated. Set OPENAI_API_KEY or run: codex login")]
    CodexNotAuthenticated,

    #[error("Cursor CLI is not authenticated. Set CURSOR_API_KEY or run: cursor-agent login")]
    CursorNotAuthenticated,

    /// No runner was available for the given model after probing all candidates.
    #[error("No runner available for model '{model}'.\nTried:\n{tried}")]
    NoRunnerAvailable { model: String, tried: String },

    #[error("API key not set. Set {env_var} or configure the api key in your config")]
    ApiKeyMissing { env_var: String },

    /// Model '{model}' was explicitly requested with codex-cli but no API key is available.
    /// ChatGPT OAuth (codex login) only supports the Codex CLI default model.
    #[error(
        "Model '{model}' requires an OpenAI API key when used with codex-cli.\n\
         ChatGPT authentication only supports the default model.\n\
         Set OPENAI_API_KEY or use --runner openai-api."
    )]
    CodexCliModelRequiresApiKey { model: String },

    #[error("Runner failed: {message}\nstderr: {stderr}")]
    RunnerFailed { message: String, stderr: String },

    #[error("Insufficient credits: {message}")]
    CreditBalanceTooLow { message: String },

    #[error("Failed to parse runner output: {0}")]
    RunnerOutputParse(#[from] serde_json::Error),

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

    #[error("Runner timed out after {seconds}s")]
    RunnerTimeout { seconds: u64 },

    #[error("User cancelled")]
    UserCancelled,

    #[error("Tailoring output validation failed: {0}")]
    TailoringValidationError(String),

    #[error("Internal error: {0}")]
    InternalError(String),

    #[error("Terminal I/O error: {0}")]
    TerminalIOError(String),

    #[error("Actual AI API is being updated and will be available shortly")]
    ServiceUnavailable,

    #[error("Code review failed: {0}")]
    CodeReviewError(String),
}

impl ActualError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::UserCancelled => 4,
            Self::ClaudeNotFound
            | Self::ClaudeNotAuthenticated
            | Self::CodexNotFound
            | Self::CodexNotAuthenticated
            | Self::CursorNotFound
            | Self::CursorNotAuthenticated
            | Self::ApiKeyMissing { .. }
            | Self::CodexCliModelRequiresApiKey { .. }
            | Self::NoRunnerAvailable { .. } => 2,
            Self::CreditBalanceTooLow { .. } => 3,
            Self::ApiError(_) | Self::ApiResponseError { .. } | Self::ServiceUnavailable => 3,
            Self::IoError(_) => 5,
            _ => 1,
        }
    }

    /// Returns a human-friendly fix suggestion for this error, if available.
    pub fn hint(&self) -> Option<Cow<'static, str>> {
        match self {
            Self::ClaudeNotFound => {
                Some(Cow::Borrowed("npm install -g @anthropic-ai/claude-code"))
            }
            Self::CodexNotFound => Some(Cow::Borrowed("npm install -g @openai/codex")),
            Self::CursorNotFound => {
                Some(Cow::Borrowed("curl https://cursor.com/install -fsS | bash"))
            }
            Self::ClaudeNotAuthenticated => Some(Cow::Borrowed("claude auth login")),
            Self::CodexNotAuthenticated => {
                Some(Cow::Borrowed("Set OPENAI_API_KEY or run: codex login"))
            }
            Self::CursorNotAuthenticated => {
                Some(Cow::Borrowed("Set CURSOR_API_KEY or run: cursor-agent login"))
            }
            Self::ApiKeyMissing { env_var } => Some(Cow::Owned(format!(
                "Set {env_var} environment variable or add it to your config file"
            ))),
            Self::CodexCliModelRequiresApiKey { .. } => {
                Some(Cow::Borrowed("Set OPENAI_API_KEY or switch to --runner openai-api"))
            }
            Self::NoRunnerAvailable { .. } => Some(Cow::Borrowed(
                "Install a runner (e.g. `npm install -g @anthropic-ai/claude-code`) or set an API key",
            )),
            Self::CreditBalanceTooLow { .. } => Some(Cow::Borrowed(
                "Add credits at your provider's billing page or check your account quota",
            )),
            Self::ConfigError(_) => {
                Some(Cow::Borrowed("Check ~/.actualai/actual/config.yaml"))
            }
            Self::RunnerTimeout { .. } => Some(Cow::Borrowed(
                "Set `invocation_timeout_secs` in ~/.actualai/actual/config.yaml to increase the limit",
            )),
            Self::RunnerFailed { .. } => Some(Cow::Borrowed(
                "Check the error details above. For subprocess runners, re-run with --verbose for more output.",
            )),
            _ => None,
        }
    }

    /// Returns `true` if this error indicates a model-not-supported or
    /// model-not-found condition.  Used by runner fallback logic to decide
    /// whether to retry with a different backend.
    pub fn is_model_error(&self) -> bool {
        match self {
            Self::RunnerFailed { message, .. } => {
                let lower = message.to_lowercase();
                lower.contains("model is not supported")
                    || lower.contains("model is not available")
                    || lower.contains("model not found")
                    || lower.contains("does not exist")
                    || lower.contains("invalid model")
                    || lower.contains("model_not_found")
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;

    #[test]
    fn test_exit_codes() {
        assert_eq!(ActualError::ClaudeNotFound.exit_code(), 2);
        assert_eq!(ActualError::ClaudeNotAuthenticated.exit_code(), 2);
        assert_eq!(ActualError::CodexNotAuthenticated.exit_code(), 2);
        assert_eq!(ActualError::CodexNotFound.exit_code(), 2);
        assert_eq!(ActualError::CursorNotFound.exit_code(), 2);
        assert_eq!(ActualError::CursorNotAuthenticated.exit_code(), 2);
        assert_eq!(
            ActualError::RunnerFailed {
                message: "fail".to_string(),
                stderr: "err".to_string(),
            }
            .exit_code(),
            1
        );
        assert_eq!(
            ActualError::RunnerOutputParse(serde_json::from_str::<()>("invalid").unwrap_err())
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
        assert_eq!(ActualError::RunnerTimeout { seconds: 30 }.exit_code(), 1);
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
        assert_eq!(
            ActualError::ApiKeyMissing {
                env_var: "ANTHROPIC_API_KEY".to_string()
            }
            .exit_code(),
            2
        );
        assert_eq!(
            ActualError::CreditBalanceTooLow {
                message: "Credit balance is too low".to_string()
            }
            .exit_code(),
            3
        );
        assert_eq!(
            ActualError::CodexCliModelRequiresApiKey {
                model: "gpt-5.2-codex".to_string()
            }
            .exit_code(),
            2
        );
        assert_eq!(
            ActualError::NoRunnerAvailable {
                model: "sonnet".to_string(),
                tried: "  - claude-cli: binary not found".to_string(),
            }
            .exit_code(),
            2
        );
        assert_eq!(ActualError::ServiceUnavailable.exit_code(), 3);
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

        let msg = ActualError::CursorNotFound.to_string();
        assert!(msg.contains("not found"), "expected 'not found' in: {msg}");
        assert!(
            msg.contains("cursor.com"),
            "expected 'cursor.com' in: {msg}"
        );

        let msg = ActualError::ClaudeNotAuthenticated.to_string();
        assert!(
            msg.contains("not authenticated"),
            "expected 'not authenticated' in: {msg}"
        );

        let msg = ActualError::CodexNotAuthenticated.to_string();
        assert!(
            msg.contains("not authenticated"),
            "expected 'not authenticated' in: {msg}"
        );
        assert!(
            msg.contains("OPENAI_API_KEY"),
            "expected 'OPENAI_API_KEY' in: {msg}"
        );

        let msg = ActualError::CursorNotAuthenticated.to_string();
        assert!(
            msg.contains("not authenticated"),
            "expected 'not authenticated' in: {msg}"
        );
        assert!(
            msg.contains("CURSOR_API_KEY"),
            "expected 'CURSOR_API_KEY' in: {msg}"
        );

        let msg = ActualError::RunnerFailed {
            message: "oops".to_string(),
            stderr: "some error output".to_string(),
        }
        .to_string();
        assert!(msg.contains("oops"), "expected 'oops' in: {msg}");
        assert!(
            msg.contains("some error output"),
            "expected stderr content in: {msg}"
        );

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

        let msg = ActualError::RunnerTimeout { seconds: 30 }.to_string();
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

        let msg = ActualError::ApiKeyMissing {
            env_var: "OPENAI_API_KEY".to_string(),
        }
        .to_string();
        assert!(
            msg.contains("API key not set"),
            "expected 'API key not set' in: {msg}"
        );
        assert!(
            msg.contains("OPENAI_API_KEY"),
            "expected env var name in: {msg}"
        );

        let msg = ActualError::CreditBalanceTooLow {
            message: "Credit balance is too low".to_string(),
        }
        .to_string();
        assert!(
            msg.contains("Insufficient credits"),
            "expected 'Insufficient credits' in: {msg}"
        );
        assert!(
            msg.contains("Credit balance is too low"),
            "expected detail message in: {msg}"
        );

        let msg = ActualError::CodexCliModelRequiresApiKey {
            model: "gpt-5.2-codex".to_string(),
        }
        .to_string();
        assert!(
            msg.contains("gpt-5.2-codex"),
            "expected model name in: {msg}"
        );
        assert!(
            msg.contains("OPENAI_API_KEY"),
            "expected 'OPENAI_API_KEY' in: {msg}"
        );
        assert!(
            msg.contains("openai-api"),
            "expected '--runner openai-api' suggestion in: {msg}"
        );
        assert!(
            msg.contains("ChatGPT"),
            "expected 'ChatGPT' explanation in: {msg}"
        );

        let msg = ActualError::NoRunnerAvailable {
            model: "sonnet".to_string(),
            tried: "  - claude-cli: binary not found\n  - anthropic-api: ANTHROPIC_API_KEY not set"
                .to_string(),
        }
        .to_string();
        assert!(
            msg.contains("No runner available"),
            "expected 'No runner available' in: {msg}"
        );
        assert!(msg.contains("sonnet"), "expected model name in: {msg}");
        assert!(msg.contains("claude-cli"), "expected tried list in: {msg}");
    }

    #[test]
    fn test_hint_claude_not_found() {
        assert_eq!(
            ActualError::ClaudeNotFound.hint(),
            Some(Cow::Borrowed("npm install -g @anthropic-ai/claude-code"))
        );
    }

    #[test]
    fn test_hint_codex_not_found() {
        assert_eq!(
            ActualError::CodexNotFound.hint(),
            Some(Cow::Borrowed("npm install -g @openai/codex"))
        );
    }

    #[test]
    fn test_hint_cursor_not_found() {
        assert_eq!(
            ActualError::CursorNotFound.hint(),
            Some(Cow::Borrowed("curl https://cursor.com/install -fsS | bash"))
        );
    }

    #[test]
    fn test_hint_claude_not_authenticated() {
        assert_eq!(
            ActualError::ClaudeNotAuthenticated.hint(),
            Some(Cow::Borrowed("claude auth login"))
        );
    }

    #[test]
    fn test_hint_codex_not_authenticated() {
        assert_eq!(
            ActualError::CodexNotAuthenticated.hint(),
            Some(Cow::Borrowed("Set OPENAI_API_KEY or run: codex login"))
        );
    }

    #[test]
    fn test_hint_cursor_not_authenticated() {
        assert_eq!(
            ActualError::CursorNotAuthenticated.hint(),
            Some(Cow::Borrowed(
                "Set CURSOR_API_KEY or run: cursor-agent login"
            ))
        );
    }

    #[test]
    fn test_hint_config_error() {
        assert_eq!(
            ActualError::ConfigError("test".to_string()).hint(),
            Some(Cow::Borrowed("Check ~/.actualai/actual/config.yaml"))
        );
    }

    #[test]
    fn test_hint_claude_timeout() {
        assert_eq!(
            ActualError::RunnerTimeout { seconds: 30 }.hint(),
            Some(Cow::Borrowed(
                "Set `invocation_timeout_secs` in ~/.actualai/actual/config.yaml to increase the limit",
            ))
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
    fn test_hint_api_key_missing() {
        let err = ActualError::ApiKeyMissing {
            env_var: "ANTHROPIC_API_KEY".to_string(),
        };
        let hint = err.hint();
        assert!(hint.is_some(), "expected Some hint for ApiKeyMissing");
        let hint_str = hint.unwrap();
        assert!(
            hint_str.contains("ANTHROPIC_API_KEY"),
            "expected env var name in hint: {hint_str:?}"
        );
        assert!(
            hint_str.contains("environment variable"),
            "expected 'environment variable' in hint: {hint_str:?}"
        );
    }

    #[test]
    fn test_hint_api_key_missing_includes_env_var_name() {
        let err = ActualError::ApiKeyMissing {
            env_var: "OPENAI_API_KEY".to_string(),
        };
        let hint = err.hint().expect("expected Some hint for ApiKeyMissing");
        assert!(
            hint.contains("OPENAI_API_KEY"),
            "expected 'OPENAI_API_KEY' in hint: {hint:?}"
        );
    }

    #[test]
    fn test_hint_codex_cli_model_requires_api_key() {
        let err = ActualError::CodexCliModelRequiresApiKey {
            model: "gpt-5.2-codex".to_string(),
        };
        let hint = err
            .hint()
            .expect("expected Some hint for CodexCliModelRequiresApiKey");
        assert!(
            hint.contains("OPENAI_API_KEY"),
            "expected 'OPENAI_API_KEY' in hint: {hint:?}"
        );
    }

    #[test]
    fn test_hint_no_runner_available() {
        let err = ActualError::NoRunnerAvailable {
            model: "sonnet".to_string(),
            tried: "  - claude-cli: binary not found".to_string(),
        };
        let hint = err
            .hint()
            .expect("expected Some hint for NoRunnerAvailable");
        assert!(
            hint.contains("Install a runner"),
            "expected install hint in: {hint:?}"
        );
    }

    #[test]
    fn test_hint_credit_balance_too_low() {
        let err = ActualError::CreditBalanceTooLow {
            message: "Credit balance is too low".to_string(),
        };
        let hint = err
            .hint()
            .expect("expected Some hint for CreditBalanceTooLow");
        assert!(
            hint.contains("billing"),
            "expected 'billing' in hint: {hint:?}"
        );
    }

    #[test]
    fn test_hint_runner_failed() {
        let err = ActualError::RunnerFailed {
            message: "exit code 1".to_string(),
            stderr: String::new(),
        };
        let hint = err.hint().expect("expected Some hint for RunnerFailed");
        assert!(
            hint.contains("error details"),
            "expected 'error details' in hint: {hint:?}"
        );
        assert!(
            hint.contains("--verbose"),
            "expected '--verbose' in hint: {hint:?}"
        );
    }

    #[test]
    fn test_is_model_error_supported_pattern() {
        let err = ActualError::RunnerFailed {
            message: "Codex CLI failed: The 'gpt-5.2' model is not supported".to_string(),
            stderr: String::new(),
        };
        assert!(err.is_model_error());
    }

    #[test]
    fn test_is_model_error_not_found_pattern() {
        let err = ActualError::RunnerFailed {
            message: "model not found: gpt-5-mini".to_string(),
            stderr: String::new(),
        };
        assert!(err.is_model_error());
    }

    #[test]
    fn test_is_model_error_does_not_exist_pattern() {
        let err = ActualError::RunnerFailed {
            message: "The model 'gpt-99' does not exist".to_string(),
            stderr: String::new(),
        };
        assert!(err.is_model_error());
    }

    #[test]
    fn test_is_model_error_case_insensitive() {
        let err = ActualError::RunnerFailed {
            message: "MODEL IS NOT SUPPORTED".to_string(),
            stderr: String::new(),
        };
        assert!(err.is_model_error());
    }

    #[test]
    fn test_is_model_error_false_for_other_runner_failed() {
        let err = ActualError::RunnerFailed {
            message: "Codex CLI exited with code 1".to_string(),
            stderr: "connection refused".to_string(),
        };
        assert!(!err.is_model_error());
    }

    #[test]
    fn test_is_model_error_false_for_non_runner_errors() {
        assert!(!ActualError::UserCancelled.is_model_error());
        assert!(!ActualError::RunnerTimeout { seconds: 30 }.is_model_error());
        assert!(!ActualError::ConfigError("bad".to_string()).is_model_error());
    }

    #[test]
    fn test_runner_failed_stderr_displayed() {
        let err = ActualError::RunnerFailed {
            message: "exit code 1".to_string(),
            stderr: "error: could not compile `foo`".to_string(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Runner failed: exit code 1"),
            "expected message in display: {msg}"
        );
        assert!(
            msg.contains("stderr:"),
            "expected 'stderr:' label in display: {msg}"
        );
        assert!(
            msg.contains("could not compile `foo`"),
            "expected stderr content in display: {msg}"
        );
    }

    #[test]
    fn test_runner_failed_empty_stderr_displayed() {
        let err = ActualError::RunnerFailed {
            message: "exit code 1".to_string(),
            stderr: String::new(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Runner failed: exit code 1"),
            "expected message in display: {msg}"
        );
        assert!(
            msg.contains("stderr:"),
            "expected 'stderr:' label even when empty: {msg}"
        );
    }

    #[test]
    fn test_service_unavailable_display() {
        let msg = ActualError::ServiceUnavailable.to_string();
        assert!(
            msg.contains("being updated"),
            "expected 'being updated' in: {msg}"
        );
        assert!(
            msg.contains("available shortly"),
            "expected 'available shortly' in: {msg}"
        );
    }

    #[test]
    fn test_service_unavailable_hint_is_none() {
        assert_eq!(ActualError::ServiceUnavailable.hint(), None);
    }

    #[test]
    fn test_service_unavailable_is_not_model_error() {
        assert!(!ActualError::ServiceUnavailable.is_model_error());
    }
}
