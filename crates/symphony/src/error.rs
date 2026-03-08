use thiserror::Error;

/// Top-level error type for Symphony.
#[derive(Debug, Error)]
pub enum SymphonyError {
    // Workflow / Config errors
    #[error("missing workflow file: {path}")]
    MissingWorkflowFile { path: String },

    #[error("workflow parse error: {reason}")]
    WorkflowParseError { reason: String },

    #[error("workflow front matter is not a map")]
    WorkflowFrontMatterNotAMap,

    #[error("template parse error: {reason}")]
    TemplateParseError { reason: String },

    #[error("template render error: {reason}")]
    TemplateRenderError { reason: String },

    // Config validation errors
    #[error("missing required config: {field}")]
    MissingRequiredConfig { field: String },

    #[error("unsupported tracker kind: {kind}")]
    UnsupportedTrackerKind { kind: String },

    #[error("invalid config value: {field}: {reason}")]
    InvalidConfigValue { field: String, reason: String },

    // Workspace errors
    #[error("workspace creation failed: {reason}")]
    WorkspaceCreationFailed { reason: String },

    #[error("workspace path outside root: workspace={workspace}, root={root}")]
    WorkspacePathOutsideRoot { workspace: String, root: String },

    #[error("workspace hook failed: hook={hook}, reason={reason}")]
    WorkspaceHookFailed { hook: String, reason: String },

    #[error("workspace hook timed out: hook={hook}")]
    WorkspaceHookTimedOut { hook: String },

    // Agent session errors
    #[error("agent not found: {command}")]
    AgentNotFound { command: String },

    #[error("invalid workspace cwd: {path}")]
    InvalidWorkspaceCwd { path: String },

    #[error("agent startup failed: {reason}")]
    AgentStartupFailed { reason: String },

    #[error("agent response timeout")]
    AgentResponseTimeout,

    #[error("agent turn timeout")]
    AgentTurnTimeout,

    #[error("agent process exited unexpectedly: {reason}")]
    AgentProcessExited { reason: String },

    #[error("agent turn failed: {reason}")]
    AgentTurnFailed { reason: String },

    #[error("agent turn cancelled")]
    AgentTurnCancelled,

    #[error("agent turn requires user input")]
    AgentTurnInputRequired,

    #[error("agent stalled: no activity for {elapsed_ms}ms")]
    AgentStalled { elapsed_ms: u64 },

    // Tracker errors
    #[error("missing tracker API key")]
    MissingTrackerApiKey,

    #[error("missing tracker team key")]
    MissingTrackerTeamKey,

    #[error("linear API request error: {reason}")]
    LinearApiRequest { reason: String },

    #[error("linear API status error: status={status}, body={body}")]
    LinearApiStatus { status: u16, body: String },

    #[error("linear GraphQL errors: {errors}")]
    LinearGraphqlErrors { errors: String },

    #[error("linear unknown payload: {reason}")]
    LinearUnknownPayload { reason: String },

    #[error("linear missing end cursor")]
    LinearMissingEndCursor,

    // GitHub errors
    #[error("github API request error: {reason}")]
    GitHubApiRequest { reason: String },

    #[error("github API status error: status={status}, body={body}")]
    GitHubApiStatus { status: u16, body: String },

    #[error("github invalid repo format: {repo}")]
    GitHubInvalidRepo { repo: String },

    // MCP errors
    #[error("MCP server error: {reason}")]
    McpServerError { reason: String },

    #[error("MCP config write failed: {reason}")]
    McpConfigWriteFailed { reason: String },

    // Dispatch errors
    #[error("dispatch validation failed: {reason}")]
    DispatchValidationFailed { reason: String },

    #[error("no available orchestrator slots")]
    NoAvailableSlots,

    // Generic
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, SymphonyError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_github_api_request_display() {
        let err = SymphonyError::GitHubApiRequest {
            reason: "connection refused".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("github API request error"));
        assert!(msg.contains("connection refused"));
    }

    #[test]
    fn test_github_api_status_display() {
        let err = SymphonyError::GitHubApiStatus {
            status: 403,
            body: "forbidden".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("github API status error"));
        assert!(msg.contains("403"));
        assert!(msg.contains("forbidden"));
    }

    #[test]
    fn test_github_invalid_repo_display() {
        let err = SymphonyError::GitHubInvalidRepo {
            repo: "bad-format".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("github invalid repo format"));
        assert!(msg.contains("bad-format"));
    }

    #[test]
    fn test_github_errors_are_debug() {
        let err = SymphonyError::GitHubApiRequest {
            reason: "test".to_string(),
        };
        let debug = format!("{:?}", err);
        assert!(debug.contains("GitHubApiRequest"));
    }

    #[test]
    fn test_mcp_server_error_display() {
        let err = SymphonyError::McpServerError {
            reason: "stdin closed".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("MCP server error"));
        assert!(msg.contains("stdin closed"));
    }

    #[test]
    fn test_mcp_config_write_failed_display() {
        let err = SymphonyError::McpConfigWriteFailed {
            reason: "permission denied".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("MCP config write failed"));
        assert!(msg.contains("permission denied"));
    }

    #[test]
    fn test_mcp_errors_are_debug() {
        let err = SymphonyError::McpServerError {
            reason: "test".to_string(),
        };
        let debug = format!("{:?}", err);
        assert!(debug.contains("McpServerError"));

        let err2 = SymphonyError::McpConfigWriteFailed {
            reason: "test".to_string(),
        };
        let debug2 = format!("{:?}", err2);
        assert!(debug2.contains("McpConfigWriteFailed"));
    }
}
