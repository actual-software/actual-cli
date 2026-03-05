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

    #[error("missing tracker project slug")]
    MissingTrackerProjectSlug,

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
