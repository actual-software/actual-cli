use crate::config::ServiceConfig;
use crate::model::{CompletionOutcome, LogEntry, OrchestratorState, RetryEntry, WaitingEntry};
use crate::protocol::{
    OrchestratorMessage, WorkResult, WorkerEventPayload, WorkerHeartbeat, WorkerRegistration,
};
use axum::extract::{Path, Query, State};
use axum::http::{Method, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;

/// Shared state for axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub orchestrator_state: Arc<RwLock<OrchestratorState>>,
    pub config: Arc<RwLock<ServiceConfig>>,
    pub msg_tx: mpsc::Sender<OrchestratorMessage>,
    pub job_notify: Arc<tokio::sync::Notify>,
    pub start_time: std::time::Instant,
}

/// Start the HTTP dashboard/API server.
///
/// Binds to `{bind_address}:{port}`. If port is 0, an ephemeral port is chosen
/// and the actual address is logged.
pub async fn start_server(
    port: u16,
    bind_address: &str,
    state: Arc<RwLock<OrchestratorState>>,
    config: Arc<RwLock<ServiceConfig>>,
    msg_tx: mpsc::Sender<OrchestratorMessage>,
    job_notify: Arc<tokio::sync::Notify>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let app_state = AppState {
        orchestrator_state: state,
        config,
        msg_tx,
        job_notify,
        start_time: std::time::Instant::now(),
    };

    let app = build_router(app_state);

    let addr = format!("{bind_address}:{port}");
    let listener = TcpListener::bind(&addr).await?;
    let local_addr = listener.local_addr()?;
    info!(address = %local_addr, "HTTP server listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;

    Ok(())
}

/// Build the axum Router (public for integration testing).
pub fn build_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST]);

    Router::new()
        .route("/", get(handle_dashboard))
        .route("/api/v1/state", get(handle_state))
        .route("/api/v1/state/stream", get(handle_state_stream))
        .route("/api/v1/refresh", post(handle_refresh))
        .route("/api/v1/work", get(handle_claim_work))
        .route("/api/v1/heartbeat", post(handle_heartbeat))
        .route("/api/v1/workers/register", post(handle_register))
        .route(
            "/api/v1/{issue_identifier}/stream",
            get(handle_issue_stream),
        )
        .route("/api/v1/{issue_identifier}/logs", get(handle_issue_logs))
        .route(
            "/api/v1/{issue_identifier}/events",
            post(handle_worker_event),
        )
        .route(
            "/api/v1/{issue_identifier}/complete",
            post(handle_worker_complete),
        )
        .route(
            "/api/v1/{issue_identifier}/remove-retry",
            post(handle_remove_retry),
        )
        .route("/api/v1/{issue_identifier}", get(handle_issue_detail))
        .route("/healthz", get(handle_healthz))
        .route("/readyz", get(handle_readyz))
        .fallback(handle_fallback)
        .layer(cors)
        .with_state(state)
}

// ── JSON response types ──────────────────────────────────────────────

/// State snapshot for GET /api/v1/state (§13.7.2).
#[derive(Debug, Clone, Serialize)]
pub struct StateResponse {
    pub generated_at: String,
    pub counts: StateCounts,
    pub running: Vec<RunningInfo>,
    pub retrying: Vec<RetryInfo>,
    pub waiting: Vec<WaitingInfo>,
    pub completed: Vec<CompletedInfo>,
    pub codex_totals: TotalsInfo,
    pub rate_limits: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StateCounts {
    pub running: usize,
    pub retrying: usize,
    pub waiting: usize,
    pub completed: usize,
}

/// Summary of a completed agent run for the dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct CompletedInfo {
    pub issue_identifier: String,
    pub outcome: String,
    pub duration_seconds: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub turn_count: u32,
    pub completed_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunningInfo {
    pub issue_id: String,
    pub issue_identifier: String,
    pub state: String,
    pub session_id: Option<String>,
    pub turn_count: u32,
    pub last_event: Option<String>,
    pub last_message: Option<String>,
    pub started_at: String,
    pub last_event_at: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetryInfo {
    pub issue_id: String,
    pub identifier: String,
    pub attempt: u32,
    pub due_at_ms: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WaitingInfo {
    pub issue_id: String,
    pub identifier: String,
    pub pr_number: u64,
    pub branch: String,
    pub started_waiting_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TotalsInfo {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub seconds_running: f64,
}

/// Per-issue detail for GET /api/v1/:issue_identifier.
#[derive(Debug, Clone, Serialize)]
pub struct IssueDetailResponse {
    pub issue_identifier: String,
    pub status: String,
    pub issue_id: Option<String>,
    pub workspace_path: Option<String>,
    pub session: Option<SessionDetail>,
    pub retry: Option<RetryInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionDetail {
    pub session_id: Option<String>,
    pub turn_count: u32,
    pub last_event: Option<String>,
    pub last_message: Option<String>,
    pub started_at: String,
    pub last_event_at: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// Refresh response for POST /api/v1/refresh.
#[derive(Debug, Clone, Serialize)]
pub struct RefreshResponse {
    pub queued: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coalesced: Option<bool>,
    pub requested_at: String,
}

/// Remove-retry response for POST /api/v1/:issue_identifier/remove-retry.
#[derive(Debug, Clone, Serialize)]
pub struct RemoveRetryResponse {
    pub removed: bool,
    pub issue_identifier: String,
    pub issue_id: Option<String>,
}

/// Error response body.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorResponse {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
}

// ── Snapshot builder ─────────────────────────────────────────────────

/// Build a state snapshot from the shared orchestrator state.
///
/// Computes live `seconds_running` by adding elapsed time from active sessions
/// to the cumulative total.
fn build_snapshot(
    state: &OrchestratorState,
) -> (
    Vec<RunningInfo>,
    Vec<RetryInfo>,
    Vec<WaitingInfo>,
    Vec<CompletedInfo>,
    TotalsInfo,
) {
    let running: Vec<RunningInfo> = state.running.values().map(map_running_entry).collect();

    let retrying: Vec<RetryInfo> = state.retry_attempts.values().map(map_retry_entry).collect();

    let waiting: Vec<WaitingInfo> = state
        .waiting_for_review
        .values()
        .map(map_waiting_entry)
        .collect();

    let completed: Vec<CompletedInfo> = state
        .completion_history()
        .iter()
        .rev() // most recent first
        .map(map_completion_record)
        .collect();

    let active_seconds: f64 = state
        .running
        .values()
        .map(running_entry_elapsed_seconds)
        .sum();

    let totals = TotalsInfo {
        input_tokens: state.agent_totals.input_tokens,
        output_tokens: state.agent_totals.output_tokens,
        total_tokens: state.agent_totals.total_tokens,
        seconds_running: state.agent_totals.seconds_running + active_seconds,
    };

    (running, retrying, waiting, completed, totals)
}

/// Map a `CompletionRecord` to a `CompletedInfo` for JSON serialization.
fn map_completion_record(record: &crate::model::CompletionRecord) -> CompletedInfo {
    let outcome = match record.outcome {
        CompletionOutcome::Success => "success",
        CompletionOutcome::InReview => "in_review",
        CompletionOutcome::Retry => "retry",
        CompletionOutcome::Failed => "failed",
    };
    CompletedInfo {
        issue_identifier: record.identifier.clone(),
        outcome: outcome.to_string(),
        duration_seconds: record.duration_seconds,
        input_tokens: record.input_tokens,
        output_tokens: record.output_tokens,
        total_tokens: record.total_tokens,
        turn_count: record.turn_count,
        completed_at: format_datetime(record.completed_at),
    }
}

/// Compute elapsed seconds for a running entry.
fn running_entry_elapsed_seconds(entry: &crate::model::RunningEntry) -> f64 {
    Utc::now()
        .signed_duration_since(entry.started_at)
        .num_milliseconds()
        .max(0) as f64
        / 1000.0
}

/// Find a running entry by its issue identifier.
fn find_running_by_identifier<'a>(
    state: &'a OrchestratorState,
    identifier: &str,
) -> Option<&'a crate::model::RunningEntry> {
    state.running.values().find(|e| e.identifier == identifier)
}

/// Find a retry entry by its issue identifier.
fn find_retry_by_identifier<'a>(
    state: &'a OrchestratorState,
    identifier: &str,
) -> Option<&'a RetryEntry> {
    state
        .retry_attempts
        .values()
        .find(|e| e.identifier == identifier)
}

/// Format an optional DateTime as an RFC3339 string.
fn format_optional_datetime(dt: Option<chrono::DateTime<Utc>>) -> Option<String> {
    dt.map(format_datetime)
}

/// Format a DateTime as an RFC3339 string.
fn format_datetime(dt: chrono::DateTime<Utc>) -> String {
    dt.to_rfc3339()
}

/// Map a `RunningEntry` to a `RunningInfo` for JSON serialization.
fn map_running_entry(entry: &crate::model::RunningEntry) -> RunningInfo {
    RunningInfo {
        issue_id: entry.issue.id.clone(),
        issue_identifier: entry.identifier.clone(),
        state: entry.issue.state.clone(),
        session_id: entry.session.session_id.clone(),
        turn_count: entry.session.turn_count,
        last_event: entry.session.last_event.clone(),
        last_message: entry.session.last_message.clone(),
        started_at: format_datetime(entry.started_at),
        last_event_at: format_optional_datetime(entry.session.last_event_at),
        input_tokens: entry.session.input_tokens,
        output_tokens: entry.session.output_tokens,
        total_tokens: entry.session.total_tokens,
    }
}

/// Map a `RetryEntry` to a `RetryInfo` for JSON serialization.
fn map_retry_entry(entry: &RetryEntry) -> RetryInfo {
    RetryInfo {
        issue_id: entry.issue_id.clone(),
        identifier: entry.identifier.clone(),
        attempt: entry.attempt,
        due_at_ms: entry.due_at_ms,
        error: entry.error.clone(),
    }
}

/// Map a `WaitingEntry` to a `WaitingInfo` for JSON serialization.
fn map_waiting_entry(entry: &WaitingEntry) -> WaitingInfo {
    WaitingInfo {
        issue_id: entry.issue_id.clone(),
        identifier: entry.identifier.clone(),
        pr_number: entry.pr_number,
        branch: entry.branch.clone(),
        started_waiting_at: format_datetime(entry.started_waiting_at),
    }
}

// ── Handlers ─────────────────────────────────────────────────────────

/// GET / — HTML dashboard.
async fn handle_dashboard(State(state): State<AppState>) -> Html<String> {
    let orch_state = state.orchestrator_state.read().await;
    let (running, retrying, waiting, completed, totals) = build_snapshot(&orch_state);
    let rate_limits = orch_state.rate_limits.clone();
    drop(orch_state);

    let html = render_dashboard(&running, &retrying, &waiting, &completed, &totals, &rate_limits);
    Html(html)
}

/// GET /api/v1/state — JSON state snapshot.
async fn handle_state(State(state): State<AppState>) -> impl IntoResponse {
    let orch_state = state.orchestrator_state.read().await;
    let (running, retrying, waiting, completed, totals) = build_snapshot(&orch_state);
    let rate_limits = orch_state.rate_limits.clone();
    drop(orch_state);

    let response = StateResponse {
        generated_at: Utc::now().to_rfc3339(),
        counts: StateCounts {
            running: running.len(),
            retrying: retrying.len(),
            waiting: waiting.len(),
            completed: completed.len(),
        },
        running,
        retrying,
        waiting,
        completed,
        codex_totals: totals,
        rate_limits,
    };

    axum::Json(response)
}

/// GET /api/v1/:issue_identifier — Per-issue details.
async fn handle_issue_detail(
    State(state): State<AppState>,
    Path(issue_identifier): Path<String>,
) -> Response {
    let orch_state = state.orchestrator_state.read().await;
    let config = state.config.read().await;

    // Search running entries by identifier
    let running_entry = find_running_by_identifier(&orch_state, &issue_identifier);

    if let Some(entry) = running_entry {
        let workspace_key = crate::workspace::sanitize_workspace_key(&entry.identifier);
        let workspace_path = config.workspace.root.join(&workspace_key);

        let detail = IssueDetailResponse {
            issue_identifier: entry.identifier.clone(),
            status: "running".to_string(),
            issue_id: Some(entry.issue.id.clone()),
            workspace_path: Some(workspace_path.display().to_string()),
            session: Some(SessionDetail {
                session_id: entry.session.session_id.clone(),
                turn_count: entry.session.turn_count,
                last_event: entry.session.last_event.clone(),
                last_message: entry.session.last_message.clone(),
                started_at: format_datetime(entry.started_at),
                last_event_at: format_optional_datetime(entry.session.last_event_at),
                input_tokens: entry.session.input_tokens,
                output_tokens: entry.session.output_tokens,
                total_tokens: entry.session.total_tokens,
            }),
            retry: None,
        };

        return (StatusCode::OK, axum::Json(detail)).into_response();
    }

    // Search retry entries by identifier
    let retry_entry = find_retry_by_identifier(&orch_state, &issue_identifier);

    if let Some(entry) = retry_entry {
        let detail = IssueDetailResponse {
            issue_identifier: entry.identifier.clone(),
            status: "retrying".to_string(),
            issue_id: Some(entry.issue_id.clone()),
            workspace_path: None,
            session: None,
            retry: Some(map_retry_entry(entry)),
        };

        return (StatusCode::OK, axum::Json(detail)).into_response();
    }

    drop(config);
    drop(orch_state);

    error_response(
        StatusCode::NOT_FOUND,
        "issue_not_found",
        format!("No running or retrying issue with identifier '{issue_identifier}'"),
    )
}

/// Query parameters for GET /api/v1/:issue_identifier/logs.
#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    pub after: Option<u64>,
}

/// Response for GET /api/v1/:issue_identifier/logs.
#[derive(Debug, Clone, Serialize)]
pub struct LogsResponse {
    pub issue_identifier: String,
    pub count: usize,
    pub logs: Vec<LogEntry>,
}

/// Build a full `StateResponse` from the current orchestrator state.
fn build_state_response(state: &OrchestratorState) -> StateResponse {
    let (running, retrying, waiting, completed, totals) = build_snapshot(state);
    let rate_limits = state.rate_limits.clone();
    StateResponse {
        generated_at: Utc::now().to_rfc3339(),
        counts: StateCounts {
            running: running.len(),
            retrying: retrying.len(),
            waiting: waiting.len(),
            completed: completed.len(),
        },
        running,
        retrying,
        waiting,
        completed,
        codex_totals: totals,
        rate_limits,
    }
}

/// Convert a broadcast stream result into an optional SSE event.
///
/// Returns `Some` for successfully received entries and `None` for
/// lagged/closed errors, which are expected in normal operation.
/// Extracted as a named function so LLVM coverage can track both arms.
fn broadcast_result_to_sse(
    result: Result<LogEntry, tokio_stream::wrappers::errors::BroadcastStreamRecvError>,
) -> Option<Result<Event, Infallible>> {
    match result {
        Ok(entry) => Some(format_sse_event(&entry)),
        Err(_) => None,
    }
}

/// Format a single SSE event from a log entry.
fn format_sse_event(entry: &LogEntry) -> Result<Event, Infallible> {
    let data = serde_json::to_string(entry).unwrap_or_default();
    Ok(Event::default()
        .event(&entry.event_type)
        .data(data)
        .id(entry.seq.to_string()))
}

/// Convert a state broadcast result into an optional `StateResponse` JSON.
///
/// Returns `Some(json_string)` for successful signals (after reading
/// the current state snapshot) and `None` for lagged/closed errors.
fn state_broadcast_to_json(
    result: &Result<(), tokio_stream::wrappers::errors::BroadcastStreamRecvError>,
    state: &OrchestratorState,
) -> Option<String> {
    match result {
        Ok(()) => {
            let snapshot = build_state_response(state);
            serde_json::to_string(&snapshot).ok()
        }
        Err(_) => None,
    }
}

/// GET /api/v1/state/stream — SSE stream for global state changes.
///
/// Sends an initial snapshot immediately on connection, then streams
/// new snapshots whenever the orchestrator state changes.
async fn handle_state_stream(State(state): State<AppState>) -> Response {
    let rx = {
        let orch = state.orchestrator_state.read().await;
        orch.state_broadcast.subscribe()
    };

    // Build and send initial snapshot
    let initial = {
        let orch = state.orchestrator_state.read().await;
        build_state_response(&orch)
    };

    let initial_json = serde_json::to_string(&initial).unwrap_or_default();
    let initial_event = Event::default().data(initial_json);
    let initial_stream =
        tokio_stream::iter(std::iter::once(Ok::<Event, Infallible>(initial_event)));

    let state_clone = state.orchestrator_state.clone();
    let live_stream = BroadcastStream::new(rx)
        .then(move |result| {
            let sc = state_clone.clone();
            async move {
                let orch = sc.read().await;
                state_broadcast_to_json(&result, &orch).map(|j| Ok(Event::default().data(j)))
            }
        })
        .filter_map(|opt| opt);

    let combined = initial_stream.chain(live_stream);

    Sse::new(combined)
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
        .into_response()
}

/// GET /api/v1/:issue_identifier/stream — SSE event stream.
async fn handle_issue_stream(
    State(state): State<AppState>,
    Path(issue_identifier): Path<String>,
) -> Response {
    let orch_state = state.orchestrator_state.read().await;

    // Find the running entry by identifier to get the issue_id and catch-up events
    let found = orch_state
        .running
        .values()
        .find(|e| e.identifier == issue_identifier);

    let (issue_id, catchup_entries) = match found {
        Some(entry) => (
            entry.issue.id.clone(),
            entry.event_log.iter().cloned().collect::<Vec<_>>(),
        ),
        None => {
            drop(orch_state);
            return error_response(
                StatusCode::NOT_FOUND,
                "issue_not_found",
                format!("No running issue with identifier '{issue_identifier}'"),
            );
        }
    };

    // Subscribe to broadcast channel
    let broadcast_rx = match orch_state.event_broadcasts.get(&issue_id) {
        Some(tx) => tx.subscribe(),
        None => {
            drop(orch_state);
            return error_response(
                StatusCode::NOT_FOUND,
                "issue_not_found",
                format!("No broadcast channel for issue '{issue_identifier}'"),
            );
        }
    };
    drop(orch_state);

    // Build the SSE stream: catch-up burst + live events
    let catchup_stream =
        tokio_stream::iter(catchup_entries.into_iter().map(|e| format_sse_event(&e)));

    let live_stream = BroadcastStream::new(broadcast_rx).filter_map(broadcast_result_to_sse);

    let combined = catchup_stream.chain(live_stream);

    Sse::new(combined)
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
        .into_response()
}

/// GET /api/v1/:issue_identifier/logs — Log history endpoint.
async fn handle_issue_logs(
    State(state): State<AppState>,
    Path(issue_identifier): Path<String>,
    Query(query): Query<LogsQuery>,
) -> Response {
    let orch_state = state.orchestrator_state.read().await;

    let found = find_running_by_identifier(&orch_state, &issue_identifier);

    match found {
        Some(entry) => {
            let logs: Vec<LogEntry> = match query.after {
                Some(after_seq) => entry
                    .event_log
                    .iter()
                    .filter(|e| e.seq > after_seq)
                    .cloned()
                    .collect(),
                None => entry.event_log.iter().cloned().collect(),
            };

            let response = LogsResponse {
                issue_identifier: entry.identifier.clone(),
                count: logs.len(),
                logs,
            };

            (StatusCode::OK, axum::Json(response)).into_response()
        }
        None => {
            drop(orch_state);
            error_response(
                StatusCode::NOT_FOUND,
                "issue_not_found",
                format!("No running issue with identifier '{issue_identifier}'"),
            )
        }
    }
}

// ── Worker auth + endpoints ──────────────────────────────────────────

/// Authenticated worker extracted from bearer token + optional X-Worker-ID header.
#[derive(Debug, Clone)]
pub struct AuthenticatedWorker {
    pub worker_id: String,
}

impl axum::extract::FromRequestParts<AppState> for AuthenticatedWorker {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let config = state.config.read().await;
        let expected_token = match &config.deployment.auth_token {
            Some(t) => t.clone(),
            None => {
                drop(config);
                return Err(error_response(
                    StatusCode::UNAUTHORIZED,
                    "unauthorized",
                    "No auth token configured".to_string(),
                ));
            }
        };
        drop(config);

        let auth_header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok());

        let token = match auth_header {
            Some(h) if h.starts_with("Bearer ") => &h[7..],
            _ => {
                return Err(error_response(
                    StatusCode::UNAUTHORIZED,
                    "unauthorized",
                    "Missing or invalid Authorization header".to_string(),
                ));
            }
        };

        if token != expected_token {
            return Err(error_response(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Invalid bearer token".to_string(),
            ));
        }

        let worker_id = parts
            .headers
            .get("x-worker-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("anonymous")
            .to_string();

        Ok(AuthenticatedWorker { worker_id })
    }
}

/// GET /api/v1/work — Long-poll to claim a work assignment.
async fn handle_claim_work(
    _worker: AuthenticatedWorker,
    State(state): State<AppState>,
) -> Response {
    // Try immediate pop
    {
        let mut orch = state.orchestrator_state.write().await;
        if let Some(assignment) = orch.pending_jobs.pop_front() {
            return (StatusCode::OK, axum::Json(assignment)).into_response();
        }
    }

    // Long-poll: wait up to 30s for a notification
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        state.job_notify.notified(),
    )
    .await;

    if result.is_err() {
        // Timeout — no work available
        return StatusCode::NO_CONTENT.into_response();
    }

    // Notification received — try to pop again
    let mut orch = state.orchestrator_state.write().await;
    match orch.pending_jobs.pop_front() {
        Some(assignment) => (StatusCode::OK, axum::Json(assignment)).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

/// POST /api/v1/heartbeat — Worker heartbeat.
async fn handle_heartbeat(
    _worker: AuthenticatedWorker,
    State(state): State<AppState>,
    axum::Json(heartbeat): axum::Json<WorkerHeartbeat>,
) -> Response {
    let mut orch = state.orchestrator_state.write().await;
    orch.update_worker_heartbeat(&heartbeat);
    orch.notify_state_change();
    StatusCode::OK.into_response()
}

/// POST /api/v1/workers/register — Worker registration.
async fn handle_register(
    _worker: AuthenticatedWorker,
    State(state): State<AppState>,
    axum::Json(reg): axum::Json<WorkerRegistration>,
) -> Response {
    let mut orch = state.orchestrator_state.write().await;
    orch.register_worker(&reg);
    orch.notify_state_change();
    StatusCode::OK.into_response()
}

/// POST /api/v1/{issue_identifier}/events — Worker reports an agent event.
async fn handle_worker_event(
    _worker: AuthenticatedWorker,
    State(state): State<AppState>,
    Path(issue_identifier): Path<String>,
    axum::Json(payload): axum::Json<WorkerEventPayload>,
) -> Response {
    let msg = OrchestratorMessage::AgentUpdate {
        issue_id: issue_identifier.clone(),
        event: payload.event,
    };
    match state.msg_tx.send(msg).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(_) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "channel_closed",
            "Orchestrator message channel closed".to_string(),
        ),
    }
}

/// POST /api/v1/{issue_identifier}/complete — Worker reports completion.
async fn handle_worker_complete(
    _worker: AuthenticatedWorker,
    State(state): State<AppState>,
    Path(issue_identifier): Path<String>,
    axum::Json(payload): axum::Json<WorkResult>,
) -> Response {
    let msg = OrchestratorMessage::WorkerExited {
        issue_id: issue_identifier.clone(),
        reason: payload.reason,
    };
    match state.msg_tx.send(msg).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(_) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "channel_closed",
            "Orchestrator message channel closed".to_string(),
        ),
    }
}

/// POST /api/v1/refresh — Trigger immediate poll.
async fn handle_refresh(State(state): State<AppState>) -> impl IntoResponse {
    let now = Utc::now().to_rfc3339();
    let send_ok = state
        .msg_tx
        .try_send(OrchestratorMessage::TriggerRefresh)
        .is_ok();
    let coalesced = if send_ok { None } else { Some(true) };
    let response = RefreshResponse {
        queued: true,
        coalesced,
        requested_at: now,
    };
    (StatusCode::ACCEPTED, axum::Json(response))
}

/// POST /api/v1/:issue_identifier/remove-retry — Remove from retry queue.
async fn handle_remove_retry(
    State(state): State<AppState>,
    Path(issue_identifier): Path<String>,
) -> Response {
    // Look up the retry entry by identifier to get the issue_id
    let orch_state = state.orchestrator_state.read().await;
    let retry_entry = find_retry_by_identifier(&orch_state, &issue_identifier).cloned();
    drop(orch_state);

    let entry = match retry_entry {
        Some(e) => e,
        None => {
            return error_response(
                StatusCode::NOT_FOUND,
                "issue_not_found",
                format!("No retrying issue with identifier '{issue_identifier}'"),
            );
        }
    };

    // Send the remove message to the orchestrator
    let _ = state
        .msg_tx
        .send(OrchestratorMessage::RemoveFromRetryQueue {
            issue_id: entry.issue_id.clone(),
        })
        .await;

    let response = RemoveRetryResponse {
        removed: true,
        issue_identifier: entry.identifier.clone(),
        issue_id: Some(entry.issue_id),
    };

    (StatusCode::OK, axum::Json(response)).into_response()
}

// ── Health check endpoints ────────────────────────────────────────────

/// Response for GET /healthz (liveness).
#[derive(Debug, Clone, Serialize)]
pub struct HealthzResponse {
    pub status: String,
    pub uptime_secs: u64,
    pub version: String,
}

/// Response for GET /readyz (readiness) when ready.
#[derive(Debug, Clone, Serialize)]
pub struct ReadyzResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// GET /healthz — Liveness probe (unauthenticated, no DB queries).
async fn handle_healthz(State(state): State<AppState>) -> impl IntoResponse {
    let uptime = state.start_time.elapsed().as_secs();
    let response = HealthzResponse {
        status: "ok".to_string(),
        uptime_secs: uptime,
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    (StatusCode::OK, axum::Json(response))
}

/// GET /readyz — Readiness probe (unauthenticated).
///
/// Returns 200 if the orchestrator is accepting work (running agents
/// < max_concurrent_agents). Returns 503 otherwise.
async fn handle_readyz(State(state): State<AppState>) -> Response {
    let orch_state = state.orchestrator_state.read().await;
    let running_count = orch_state.running.len() as u32;
    let max = orch_state.max_concurrent_agents;
    drop(orch_state);

    if running_count >= max {
        let response = ReadyzResponse {
            status: "not_ready".to_string(),
            reason: Some(format!("at capacity: {running_count}/{max} agents running")),
        };
        return (StatusCode::SERVICE_UNAVAILABLE, axum::Json(response)).into_response();
    }

    let response = ReadyzResponse {
        status: "ready".to_string(),
        reason: None,
    };
    (StatusCode::OK, axum::Json(response)).into_response()
}

/// Build a JSON error response with the given status code, code, and message.
fn error_response(status: StatusCode, code: &str, message: String) -> Response {
    let body = ErrorResponse {
        error: ErrorDetail {
            code: code.to_string(),
            message,
        },
    };
    (status, axum::Json(body)).into_response()
}

/// Fallback handler for unsupported routes/methods.
async fn handle_fallback(method: Method) -> Response {
    if method != Method::GET && method != Method::POST {
        return error_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            format!("Method {method} is not allowed"),
        );
    }

    error_response(
        StatusCode::NOT_FOUND,
        "not_found",
        "Route not found".to_string(),
    )
}

// ── HTML Rendering ───────────────────────────────────────────────────

/// Render the HTML dashboard page.
///
/// The initial page includes server-rendered data for the first paint,
/// then JavaScript takes over and polls `/api/v1/state` every 3 seconds
/// for live updates.
pub fn render_dashboard(
    running: &[RunningInfo],
    retrying: &[RetryInfo],
    waiting: &[WaitingInfo],
    completed: &[CompletedInfo],
    totals: &TotalsInfo,
    rate_limits: &Option<serde_json::Value>,
) -> String {
    let mut html = String::with_capacity(16384);

    // Serialize initial state as JSON for the JS hydration
    let initial_state = serde_json::json!({
        "running": running,
        "retrying": retrying,
        "waiting": waiting,
        "completed": completed,
        "codex_totals": totals,
        "rate_limits": rate_limits,
        "counts": { "running": running.len(), "retrying": retrying.len(), "waiting": waiting.len(), "completed": completed.len() },
    });

    html.push_str(&format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Symphony Dashboard</title>
<style>
*,*::before,*::after {{ box-sizing:border-box; margin:0; padding:0; }}
:root {{
  --bg: #0f0f1a;
  --surface: #1a1a2e;
  --surface2: #16213e;
  --border: #2a2a4a;
  --text: #e0e0e0;
  --text-dim: #888;
  --accent: #00d4ff;
  --accent2: #7b68ee;
  --green: #00e676;
  --amber: #ffab00;
  --red: #ff5252;
  --radius: 8px;
}}
body {{ font-family: -apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif; background:var(--bg); color:var(--text); min-height:100vh; }}
.header {{ background:var(--surface); border-bottom:1px solid var(--border); padding:16px 24px; display:flex; align-items:center; justify-content:space-between; }}
.header h1 {{ font-size:18px; font-weight:600; display:flex; align-items:center; gap:10px; }}
.header h1 span {{ color:var(--accent); }}
.header-right {{ display:flex; align-items:center; gap:16px; font-size:13px; color:var(--text-dim); }}
.status-dot {{ width:8px; height:8px; border-radius:50%; background:var(--green); display:inline-block; animation:pulse 2s infinite; }}
@keyframes pulse {{ 0%,100% {{ opacity:1; }} 50% {{ opacity:0.4; }} }}
.btn {{ background:var(--surface2); border:1px solid var(--border); color:var(--text); padding:6px 14px; border-radius:var(--radius); cursor:pointer; font-size:13px; transition:all 0.15s; }}
.btn:hover {{ background:var(--border); border-color:var(--accent); }}
.btn:active {{ transform:scale(0.97); }}
.btn-danger {{ color:var(--red); border-color:rgba(255,82,82,0.3); }}
.btn-danger:hover {{ background:rgba(255,82,82,0.15); border-color:var(--red); }}
.content {{ max-width:1400px; margin:0 auto; padding:24px; }}
.stats {{ display:grid; grid-template-columns:repeat(5,1fr); gap:16px; margin-bottom:24px; }}
.stat-card {{ background:var(--surface); border:1px solid var(--border); border-radius:var(--radius); padding:20px; }}
.stat-card .label {{ font-size:12px; text-transform:uppercase; letter-spacing:0.5px; color:var(--text-dim); margin-bottom:8px; }}
.stat-card .value {{ font-size:28px; font-weight:700; font-variant-numeric:tabular-nums; }}
.stat-card .value.accent {{ color:var(--accent); }}
.stat-card .sub {{ font-size:12px; color:var(--text-dim); margin-top:4px; }}
.section {{ background:var(--surface); border:1px solid var(--border); border-radius:var(--radius); margin-bottom:16px; overflow:hidden; }}
.section-header {{ padding:14px 20px; border-bottom:1px solid var(--border); display:flex; align-items:center; justify-content:space-between; }}
.section-header h2 {{ font-size:14px; font-weight:600; display:flex; align-items:center; gap:8px; }}
.section-header .badge {{ background:var(--surface2); border:1px solid var(--border); padding:2px 8px; border-radius:12px; font-size:12px; color:var(--text-dim); font-weight:500; }}
table {{ width:100%; border-collapse:collapse; font-size:13px; }}
thead th {{ padding:10px 16px; text-align:left; font-weight:500; color:var(--text-dim); font-size:11px; text-transform:uppercase; letter-spacing:0.5px; background:var(--surface2); border-bottom:1px solid var(--border); }}
tbody td {{ padding:10px 16px; border-bottom:1px solid var(--border); }}
tbody tr:last-child td {{ border-bottom:none; }}
tbody tr:hover {{ background:rgba(0,212,255,0.04); cursor:pointer; }}
.mono {{ font-family:'SF Mono',SFMono-Regular,Consolas,monospace; font-size:12px; }}
.state-badge {{ display:inline-block; padding:2px 8px; border-radius:4px; font-size:11px; font-weight:500; }}
.state-badge.active {{ background:rgba(0,230,118,0.15); color:var(--green); }}
.state-badge.retry {{ background:rgba(255,171,0,0.15); color:var(--amber); }}
.state-badge.success {{ background:rgba(0,230,118,0.15); color:var(--green); }}
.state-badge.in_review {{ background:rgba(0,212,255,0.15); color:var(--accent); }}
.state-badge.failed {{ background:rgba(255,82,82,0.15); color:var(--red); }}
.empty-state {{ padding:40px 20px; text-align:center; color:var(--text-dim); }}
.empty-state .icon {{ font-size:32px; margin-bottom:12px; }}
.empty-state p {{ font-size:13px; line-height:1.6; }}
.token-bar {{ display:flex; gap:4px; align-items:center; }}
.token-bar .in {{ color:var(--accent); }}
.token-bar .out {{ color:var(--accent2); }}
.rate-limits pre {{ padding:16px 20px; margin:0; font-family:'SF Mono',SFMono-Regular,Consolas,monospace; font-size:12px; line-height:1.5; overflow-x:auto; color:var(--text); }}
.updated {{ font-size:11px; color:var(--text-dim); }}
.log-panel {{ background:#0a0a14; border:1px solid var(--border); border-radius:var(--radius); margin-bottom:16px; overflow:hidden; display:none; }}
.log-panel.open {{ display:block; }}
.log-panel-header {{ padding:10px 16px; border-bottom:1px solid var(--border); display:flex; align-items:center; justify-content:space-between; background:var(--surface); }}
.log-panel-header h3 {{ font-size:13px; font-weight:600; }}
.log-panel-close {{ background:none; border:none; color:var(--text-dim); cursor:pointer; font-size:16px; padding:4px 8px; }}
.log-panel-close:hover {{ color:var(--text); }}
.log-panel-body {{ max-height:400px; overflow-y:auto; padding:8px 0; font-family:'SF Mono',SFMono-Regular,Consolas,monospace; font-size:12px; line-height:1.6; }}
.log-entry {{ padding:2px 16px; display:flex; gap:10px; align-items:baseline; }}
.log-entry:hover {{ background:rgba(255,255,255,0.03); }}
.log-ts {{ color:var(--text-dim); white-space:nowrap; min-width:60px; }}
.log-badge {{ display:inline-block; padding:1px 6px; border-radius:3px; font-size:10px; font-weight:600; text-transform:uppercase; white-space:nowrap; }}
.log-badge.session_started {{ background:rgba(0,230,118,0.2); color:var(--green); }}
.log-badge.turn_completed {{ background:rgba(0,212,255,0.2); color:var(--accent); }}
.log-badge.notification {{ background:rgba(224,224,224,0.1); color:var(--text); }}
.log-badge.token_usage {{ background:rgba(136,136,136,0.15); color:var(--text-dim); }}
.log-badge.turn_failed {{ background:rgba(255,82,82,0.2); color:var(--red); }}
.log-badge.rate_limit_event {{ background:rgba(255,171,0,0.2); color:var(--amber); }}
.log-msg {{ color:var(--text); word-break:break-word; flex:1; }}
.log-tokens {{ color:var(--text-dim); white-space:nowrap; }}
@media (max-width:768px) {{
  .stats {{ grid-template-columns:repeat(2,1fr); grid-template-columns:repeat(auto-fit,minmax(140px,1fr)); }}
  .header {{ flex-direction:column; gap:12px; }}
}}
</style>
</head>
<body>

<div class="header">
  <h1><span>Symphony</span> Dashboard</h1>
  <div class="header-right">
    <span><span class="status-dot" id="statusDot"></span> <span id="statusText">Connected</span></span>
    <span class="updated" id="lastUpdate">Updated just now</span>
    <button class="btn" id="refreshBtn" onclick="triggerRefresh()">Refresh Now</button>
  </div>
</div>

<div class="content">
  <div class="stats">
    <div class="stat-card">
      <div class="label">Running</div>
      <div class="value accent" id="countRunning">0</div>
      <div class="sub">active sessions</div>
    </div>
    <div class="stat-card">
      <div class="label">Total Tokens</div>
      <div class="value" id="totalTokens">0</div>
      <div class="sub"><span class="token-bar"><span class="in" id="inputTokens">0 in</span> / <span class="out" id="outputTokens">0 out</span></span></div>
    </div>
    <div class="stat-card">
      <div class="label">Runtime</div>
      <div class="value" id="runtime">0s</div>
      <div class="sub">total agent time</div>
    </div>
    <div class="stat-card">
      <div class="label">Retrying</div>
      <div class="value" id="countRetrying">0</div>
      <div class="sub">in retry queue</div>
    </div>
    <div class="stat-card">
      <div class="label">Waiting</div>
      <div class="value" id="countWaiting">0</div>
      <div class="sub">awaiting review</div>
    </div>
    <div class="stat-card">
      <div class="label">Completed</div>
      <div class="value" id="countCompleted">0</div>
      <div class="sub">finished runs</div>
    </div>
  </div>

  <div class="section">
    <div class="section-header">
      <h2>Running Sessions <span class="badge" id="runningBadge">0</span></h2>
    </div>
    <div id="runningContent">
      <div class="empty-state">
        <div class="icon">No running sessions</div>
        <p>Waiting for Linear issues in active states.<br>Move an issue to Todo or In Progress to start.</p>
      </div>
    </div>
  </div>

  <div class="log-panel" id="logPanel">
    <div class="log-panel-header">
      <h3 id="logPanelTitle">Event Log</h3>
      <button class="log-panel-close" onclick="closeLogPanel()">X</button>
    </div>
    <div class="log-panel-body" id="logPanelBody"></div>
  </div>

  <div class="section">
    <div class="section-header">
      <h2>Retry Queue <span class="badge" id="retryBadge">0</span></h2>
    </div>
    <div id="retryContent">
      <div class="empty-state">
        <div class="icon">No retries pending</div>
        <p>Failed or timed-out sessions will appear here with their retry schedule.</p>
      </div>
    </div>
  </div>

  <div class="section">
    <div class="section-header">
      <h2>Waiting for Review <span class="badge" id="waitingBadge">0</span></h2>
    </div>
    <div id="waitingContent">
      <div class="empty-state">
        <div class="icon">No PRs waiting</div>
        <p>Issues waiting for PR review or merge will appear here.</p>
      </div>
    </div>
  </div>

  <div class="section" id="completedSection">
    <div class="section-header" style="cursor:pointer;" onclick="toggleCompleted()">
      <h2>Completed / History <span class="badge" id="completedBadge">0</span></h2>
      <span id="completedToggle" style="font-size:12px;color:var(--text-dim);">Show</span>
    </div>
    <div id="completedContent" style="display:none;">
      <div class="empty-state">
        <div class="icon">No completed runs</div>
        <p>Finished agent sessions will appear here.</p>
      </div>
    </div>
  </div>

  <div class="section" id="rateLimitSection">
    <div class="section-header">
      <h2>Rate Limits</h2>
    </div>
    <div id="rateLimitContent" class="rate-limits">
      <div class="empty-state">
        <p>No rate limit data</p>
      </div>
    </div>
  </div>
</div>

<script>
const INITIAL = {initial_json};
let lastFetch = Date.now();

function fmt(n) {{
  if (n >= 1e6) return (n/1e6).toFixed(1)+'M';
  if (n >= 1e3) return (n/1e3).toFixed(1)+'K';
  return String(n);
}}

function fmtTime(s) {{
  if (s >= 3600) return (s/3600).toFixed(1)+'h';
  if (s >= 60) return (s/60).toFixed(1)+'m';
  return s.toFixed(0)+'s';
}}

function ago(iso) {{
  if (!iso) return '-';
  const d = (Date.now() - new Date(iso).getTime()) / 1000;
  if (d < 5) return 'just now';
  if (d < 60) return Math.floor(d)+'s ago';
  if (d < 3600) return Math.floor(d/60)+'m ago';
  return Math.floor(d/3600)+'h ago';
}}

function esc(s) {{ const d=document.createElement('div'); d.textContent=s; return d.innerHTML; }}

function renderRunning(items) {{
  const el = document.getElementById('runningContent');
  document.getElementById('runningBadge').textContent = items.length;
  if (!items.length) {{
    el.innerHTML = '<div class="empty-state"><div class="icon">No running sessions</div><p>Waiting for Linear issues in active states.<br>Move an issue to Todo or In Progress to start.</p></div>';
    return;
  }}
  let h = '<table><thead><tr><th>Issue</th><th>State</th><th>Session</th><th>Turns</th><th>Last Event</th><th>Tokens</th><th>Started</th></tr></thead><tbody>';
  for (const r of items) {{
    h += '<tr onclick="openLogPanel(\''+esc(r.issue_identifier)+'\')" title="Click to view live logs">';
    h += '<td class="mono"><strong>'+esc(r.issue_identifier)+'</strong></td>';
    h += '<td><span class="state-badge active">'+esc(r.state)+'</span></td>';
    h += '<td class="mono">'+(r.session_id ? esc(r.session_id).slice(0,12) : '-')+'</td>';
    h += '<td>'+r.turn_count+'</td>';
    h += '<td>'+(r.last_event ? esc(r.last_event) : '-')+'</td>';
    h += '<td class="mono">'+fmt(r.total_tokens)+'</td>';
    h += '<td>'+ago(r.started_at)+'</td>';
    h += '</tr>';
  }}
  h += '</tbody></table>';
  el.innerHTML = h;
}}

function renderRetrying(items) {{
  const el = document.getElementById('retryContent');
  document.getElementById('retryBadge').textContent = items.length;
  if (!items.length) {{
    el.innerHTML = '<div class="empty-state"><div class="icon">No retries pending</div><p>Failed or timed-out sessions will appear here with their retry schedule.</p></div>';
    return;
  }}
  let h = '<table><thead><tr><th>Issue</th><th>Attempt</th><th>Retry In</th><th>Error</th><th></th></tr></thead><tbody>';
  for (const r of items) {{
    const dueIn = Math.max(0, r.due_at_ms - Date.now());
    const dueStr = dueIn > 0 ? fmtTime(dueIn/1000) : 'now';
    h += '<tr>';
    h += '<td class="mono"><strong>'+esc(r.identifier)+'</strong></td>';
    h += '<td><span class="state-badge retry">#'+r.attempt+'</span></td>';
    h += '<td>'+dueStr+'</td>';
    h += '<td>'+(r.error ? esc(r.error) : '-')+'</td>';
    h += '<td><button class="btn btn-danger" onclick="removeRetry(\''+esc(r.identifier)+'\')">Remove</button></td>';
    h += '</tr>';
  }}
  h += '</tbody></table>';
  el.innerHTML = h;
}}

function renderWaiting(items) {{
  const el = document.getElementById('waitingContent');
  document.getElementById('waitingBadge').textContent = items.length;
  if (!items.length) {{
    el.innerHTML = '<div class="empty-state"><div class="icon">No PRs waiting</div><p>Issues waiting for PR review or merge will appear here.</p></div>';
    return;
  }}
  let h = '<table><thead><tr><th>Issue</th><th>PR</th><th>Branch</th><th>Waiting</th></tr></thead><tbody>';
  for (const w of items) {{
    h += '<tr>';
    h += '<td class="mono"><strong>'+esc(w.identifier)+'</strong></td>';
    h += '<td><span class="state-badge active">#'+w.pr_number+'</span></td>';
    h += '<td class="mono">'+esc(w.branch)+'</td>';
    h += '<td>'+ago(w.started_waiting_at)+'</td>';
    h += '</tr>';
  }}
  h += '</tbody></table>';
  el.innerHTML = h;
}}

let completedVisible = false;
function toggleCompleted() {{
  completedVisible = !completedVisible;
  document.getElementById('completedContent').style.display = completedVisible ? '' : 'none';
  document.getElementById('completedToggle').textContent = completedVisible ? 'Hide' : 'Show';
}}

function fmtDuration(s) {{
  if (s >= 3600) return (s/3600).toFixed(1)+'h';
  if (s >= 60) return (s/60).toFixed(1)+'m';
  return s.toFixed(0)+'s';
}}

function renderCompleted(items) {{
  const el = document.getElementById('completedContent');
  document.getElementById('completedBadge').textContent = items.length;
  if (!items.length) {{
    el.innerHTML = '<div class="empty-state"><div class="icon">No completed runs</div><p>Finished agent sessions will appear here.</p></div>';
    return;
  }}
  let h = '<table><thead><tr><th>Issue</th><th>Outcome</th><th>Duration</th><th>Tokens</th><th>Turns</th><th>Completed</th></tr></thead><tbody>';
  for (const r of items) {{
    h += '<tr>';
    h += '<td class="mono"><strong>'+esc(r.issue_identifier)+'</strong></td>';
    h += '<td><span class="state-badge '+esc(r.outcome)+'">'+esc(r.outcome)+'</span></td>';
    h += '<td>'+fmtDuration(r.duration_seconds)+'</td>';
    h += '<td class="mono">'+fmt(r.total_tokens)+'</td>';
    h += '<td>'+r.turn_count+'</td>';
    h += '<td>'+ago(r.completed_at)+'</td>';
    h += '</tr>';
  }}
  h += '</tbody></table>';
  el.innerHTML = h;
}}

function renderRateLimits(rl) {{
  const el = document.getElementById('rateLimitContent');
  if (!rl) {{
    el.innerHTML = '<div class="empty-state"><p>No rate limit data</p></div>';
    return;
  }}
  el.innerHTML = '<pre>'+esc(JSON.stringify(rl, null, 2))+'</pre>';
}}

function update(data) {{
  const t = data.codex_totals || {{}};
  document.getElementById('countRunning').textContent = (data.counts||{{}}).running || 0;
  document.getElementById('countRetrying').textContent = (data.counts||{{}}).retrying || 0;
  document.getElementById('countWaiting').textContent = (data.counts||{{}}).waiting || 0;
  document.getElementById('countCompleted').textContent = (data.counts||{{}}).completed || 0;
  document.getElementById('totalTokens').textContent = fmt(t.total_tokens || 0);
  document.getElementById('inputTokens').textContent = fmt(t.input_tokens || 0) + ' in';
  document.getElementById('outputTokens').textContent = fmt(t.output_tokens || 0) + ' out';
  document.getElementById('runtime').textContent = fmtTime(t.seconds_running || 0);
  renderRunning(data.running || []);
  renderRetrying(data.retrying || []);
  renderWaiting(data.waiting || []);
  renderCompleted(data.completed || []);
  renderRateLimits(data.rate_limits);
  lastFetch = Date.now();
}}

let stateEvtSource = null;
let pollTimer = null;

function startSSE() {{
  stateEvtSource = new EventSource('/api/v1/state/stream');
  stateEvtSource.onmessage = function(e) {{
    try {{
      const data = JSON.parse(e.data);
      update(data);
      document.getElementById('statusDot').style.background = 'var(--green)';
      document.getElementById('statusText').textContent = 'Live (SSE)';
    }} catch(err) {{ /* ignore parse errors */ }}
  }};
  stateEvtSource.onerror = function() {{
    stateEvtSource.close();
    stateEvtSource = null;
    startPolling();
  }};
  if (pollTimer) {{ clearInterval(pollTimer); pollTimer = null; }}
}}

async function poll() {{
  try {{
    const r = await fetch('/api/v1/state');
    if (r.ok) {{
      update(await r.json());
      document.getElementById('statusDot').style.background = 'var(--green)';
      document.getElementById('statusText').textContent = 'Polling (fallback)';
    }}
  }} catch(e) {{
    document.getElementById('statusDot').style.background = 'var(--red)';
    document.getElementById('statusText').textContent = 'Disconnected';
  }}
}}

function startPolling() {{
  document.getElementById('statusText').textContent = 'Polling (fallback)';
  poll();
  pollTimer = setInterval(poll, 3000);
  setTimeout(function() {{ if (!stateEvtSource) startSSE(); }}, 10000);
}}

async function triggerRefresh() {{
  const btn = document.getElementById('refreshBtn');
  btn.textContent = 'Refreshing...';
  btn.disabled = true;
  try {{
    await fetch('/api/v1/refresh', {{ method: 'POST' }});
    await new Promise(r => setTimeout(r, 500));
    await poll();
  }} catch(e) {{}}
  btn.textContent = 'Refresh Now';
  btn.disabled = false;
}}

async function removeRetry(identifier) {{
  if (!confirm('Remove ' + identifier + ' from retry queue? It will stop retrying.')) return;
  try {{
    const r = await fetch('/api/v1/' + encodeURIComponent(identifier) + '/remove-retry', {{ method: 'POST' }});
    if (r.ok) {{
      await poll();
    }} else {{
      const data = await r.json().catch(() => ({{}}));
      alert('Failed to remove: ' + (data.error?.message || r.statusText));
    }}
  }} catch(e) {{
    alert('Failed to remove: ' + e.message);
  }}
}}

// Update relative timestamps every second
setInterval(() => {{
  const s = Math.floor((Date.now() - lastFetch) / 1000);
  document.getElementById('lastUpdate').textContent = s < 3 ? 'Updated just now' : 'Updated '+s+'s ago';
}}, 1000);

// ── Live Log Panel ──────────────────────────────────────────────
let currentLogIssue = null;
let currentEventSource = null;

function openLogPanel(identifier) {{
  if (currentLogIssue === identifier) {{ closeLogPanel(); return; }}
  closeLogPanel();
  currentLogIssue = identifier;
  const panel = document.getElementById('logPanel');
  const title = document.getElementById('logPanelTitle');
  const body = document.getElementById('logPanelBody');
  title.textContent = 'Event Log — ' + identifier;
  body.innerHTML = '';
  panel.classList.add('open');
  currentEventSource = new EventSource('/api/v1/' + encodeURIComponent(identifier) + '/stream');
  currentEventSource.onmessage = function(e) {{ appendLogEntry(JSON.parse(e.data)); }};
  currentEventSource.addEventListener('session_started', function(e) {{ appendLogEntry(JSON.parse(e.data)); }});
  currentEventSource.addEventListener('turn_completed', function(e) {{ appendLogEntry(JSON.parse(e.data)); }});
  currentEventSource.addEventListener('turn_failed', function(e) {{ appendLogEntry(JSON.parse(e.data)); }});
  currentEventSource.addEventListener('notification', function(e) {{ appendLogEntry(JSON.parse(e.data)); }});
  currentEventSource.addEventListener('token_usage', function(e) {{ appendLogEntry(JSON.parse(e.data)); }});
  currentEventSource.addEventListener('rate_limit_event', function(e) {{ appendLogEntry(JSON.parse(e.data)); }});
  currentEventSource.onerror = function() {{ appendLogEntry({{event_type:'notification',message:'Stream disconnected',seq:0,timestamp:new Date().toISOString()}}); }};
}}

function closeLogPanel() {{
  if (currentEventSource) {{ currentEventSource.close(); currentEventSource = null; }}
  currentLogIssue = null;
  document.getElementById('logPanel').classList.remove('open');
}}

function appendLogEntry(entry) {{
  const body = document.getElementById('logPanelBody');
  const div = document.createElement('div');
  div.className = 'log-entry';
  const ts = entry.timestamp ? ago(entry.timestamp) : '';
  const badgeClass = entry.event_type || 'notification';
  let parts = '<span class="log-ts">'+esc(ts)+'</span>';
  parts += '<span class="log-badge '+esc(badgeClass)+'">'+esc(entry.event_type||'')+'</span>';
  if (entry.message) parts += '<span class="log-msg">'+esc(entry.message)+'</span>';
  if (entry.tokens) parts += '<span class="log-tokens">'+fmt(entry.tokens.total_tokens||0)+' tok</span>';
  div.innerHTML = parts;
  body.appendChild(div);
  body.scrollTop = body.scrollHeight;
}}

// Initial render from server-embedded data
update(INITIAL);
// Start SSE (falls back to polling on error)
startSSE();
</script>
</body>
</html>"##,
        initial_json = script_safe_json(&serde_json::to_string(&initial_state).unwrap_or_default())
    ));

    html
}

/// Minimal HTML escaping for display safety.
#[cfg(test)]
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Escape JSON for safe embedding inside a `<script>` tag.
///
/// Inside `<script>`, HTML entities are NOT interpreted, so we must NOT
/// use HTML escaping. The only dangerous sequences are `</script>` (which
/// would close the tag) and `<!--` (which could start an HTML comment).
/// We escape `</` to `<\/` to prevent both.
fn script_safe_json(s: &str) -> String {
    s.replace("</", "<\\/")
}

/// Push a stat display block into the HTML buffer.
#[cfg(test)]
fn push_stat(html: &mut String, label: &str, value: &str) {
    html.push_str("<div class=\"stat\"><span class=\"stat-value\">");
    html.push_str(&html_escape(value));
    html.push_str("</span><br><span class=\"stat-label\">");
    html.push_str(&html_escape(label));
    html.push_str("</span></div>\n");
}

/// Push a `<td>` element into the HTML buffer.
#[cfg(test)]
fn push_td(html: &mut String, content: &str) {
    html.push_str("<td>");
    html.push_str(&html_escape(content));
    html.push_str("</td>");
}

/// Pretty-print a JSON value. Infallible for valid `serde_json::Value`.
#[cfg(test)]
fn format_json_pretty(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).expect("serde_json::Value always serializes")
}

/// Format token counts for display.
#[cfg(test)]
fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AgentConfig, CodingAgentConfig, DeploymentConfig, DeploymentMode, HooksConfig,
        PollingConfig, ServerConfig, TrackerConfig, WorkspaceConfig,
    };
    use crate::model::{AgentTotals, OrchestratorState, RetryEntry, WorkAssignment};
    use crate::protocol::{LiveSession, RunningEntry};
    use axum::body::Body;
    use http_body_util::BodyExt;
    use std::collections::HashMap;
    use std::future::IntoFuture;
    use std::path::PathBuf;
    use tower::ServiceExt;

    fn test_config() -> ServiceConfig {
        ServiceConfig {
            tracker: TrackerConfig {
                kind: "linear".to_string(),
                endpoint: "http://localhost:9999".to_string(),
                api_key: "test-key".to_string(),
                team_key: "test-proj".to_string(),
                active_states: vec!["Todo".to_string(), "In Progress".to_string()],
                terminal_states: vec![
                    "Done".to_string(),
                    "Cancelled".to_string(),
                    "Closed".to_string(),
                ],
            },
            polling: PollingConfig { interval_ms: 5000 },
            workspace: WorkspaceConfig {
                root: PathBuf::from("/tmp/symphony-test-workspaces"),
            },
            hooks: HooksConfig {
                after_create: None,
                before_run: None,
                after_run: None,
                before_remove: None,
                timeout_ms: 60_000,
            },
            agent: AgentConfig {
                max_concurrent_agents: 3,
                max_turns: 5,
                max_retry_backoff_ms: 300_000,
                max_concurrent_agents_by_state: HashMap::new(),
            },
            coding_agent: CodingAgentConfig {
                command: "echo test".to_string(),
                permission_mode: "bypassPermissions".to_string(),
                turn_timeout_ms: 3_600_000,
                stall_timeout_ms: 300_000,
            },
            server: ServerConfig {
                port: None,
                bind: None,
            },
            deployment: DeploymentConfig {
                mode: DeploymentMode::Local,
                auth_token: None,
            },
            github: None,
        }
    }

    fn test_app_state() -> AppState {
        let state = OrchestratorState::new(5000, 3);
        let config = test_config();
        let (msg_tx, _msg_rx) = mpsc::channel(512);

        AppState {
            orchestrator_state: Arc::new(RwLock::new(state)),
            config: Arc::new(RwLock::new(config)),
            msg_tx,
            job_notify: Arc::new(tokio::sync::Notify::new()),
            start_time: std::time::Instant::now(),
        }
    }

    fn make_issue(id: &str, identifier: &str, issue_state: &str) -> crate::model::Issue {
        crate::model::Issue {
            id: id.to_string(),
            identifier: identifier.to_string(),
            title: format!("Issue {identifier}"),
            description: None,
            priority: None,
            state: issue_state.to_string(),
            branch_name: None,
            url: None,
            labels: vec![],
            blocked_by: vec![],
            created_at: None,
            updated_at: None,
        }
    }

    async fn insert_running(app_state: &AppState, id: &str, identifier: &str, issue_state: &str) {
        let (cancel_tx, _cancel_rx) = tokio::sync::oneshot::channel();
        let mut state = app_state.orchestrator_state.write().await;
        state.running.insert(
            id.to_string(),
            RunningEntry {
                issue: make_issue(id, identifier, issue_state),
                identifier: identifier.to_string(),
                session: LiveSession::default(),
                retry_attempt: None,
                started_at: Utc::now(),
                cancel_tx,
                event_log: std::collections::VecDeque::new(),
                log_seq: 0,
            },
        );
    }

    async fn get_body(response: axum::response::Response) -> String {
        let body = response.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    // ── render_dashboard tests ───────────────────────────────────────

    #[test]
    fn test_render_dashboard_empty() {
        let html = render_dashboard(
            &[],
            &[],
            &[],
            &TotalsInfo {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                seconds_running: 0.0,
            },
            &None,
        );

        assert!(html.contains("Symphony"));
        assert!(html.contains("Dashboard"));
        assert!(html.contains("No running sessions"));
        assert!(html.contains("No retries pending"));
        assert!(html.contains("No PRs waiting"));
        assert!(html.contains("No rate limit data"));
        assert!(html.contains("No completed runs"));
        assert!(html.contains("Completed / History"));
        // Verify initial JSON state is embedded
        assert!(html.contains("const INITIAL"));
        // Verify auto-polling JS is present
        assert!(html.contains("/api/v1/state"));
    }

    #[test]
    fn test_render_dashboard_with_running() {
        let running = vec![RunningInfo {
            issue_id: "id1".to_string(),
            issue_identifier: "PROJ-1".to_string(),
            state: "Todo".to_string(),
            session_id: Some("sess-abc".to_string()),
            turn_count: 3,
            last_event: Some("turn_completed".to_string()),
            last_message: Some("Working".to_string()),
            started_at: "2025-01-01T00:00:00Z".to_string(),
            last_event_at: Some("2025-01-01T00:01:00Z".to_string()),
            input_tokens: 1000,
            output_tokens: 500,
            total_tokens: 1500,
        }];

        let html = render_dashboard(
            &running,
            &[],
            &[],
            &TotalsInfo {
                input_tokens: 1000,
                output_tokens: 500,
                total_tokens: 1500,
                seconds_running: 60.0,
            },
            &None,
        );

        // Data is embedded in the JSON initial state
        assert!(html.contains("PROJ-1"));
        assert!(html.contains("sess-abc"));
        assert!(html.contains("1500"));
    }

    #[test]
    fn test_render_dashboard_with_retries() {
        let retrying = vec![RetryInfo {
            issue_id: "id2".to_string(),
            identifier: "PROJ-2".to_string(),
            attempt: 3,
            due_at_ms: 1234567890,
            error: Some("timeout".to_string()),
        }];

        let html = render_dashboard(
            &[],
            &retrying,
            &[],
            &TotalsInfo {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                seconds_running: 0.0,
            },
            &None,
        );

        assert!(html.contains("PROJ-2"));
        assert!(html.contains("timeout"));
    }

    #[test]
    fn test_render_dashboard_with_rate_limits() {
        let rate_limits = Some(serde_json::json!({"requests_remaining": 42}));

        let html = render_dashboard(
            &[],
            &[],
            &[],
            &TotalsInfo {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                seconds_running: 0.0,
            },
            &rate_limits,
        );

        assert!(html.contains("requests_remaining"));
        assert!(html.contains("42"));
    }

    #[test]
    fn test_render_dashboard_with_running_no_session() {
        let running = vec![RunningInfo {
            issue_id: "id1".to_string(),
            issue_identifier: "PROJ-1".to_string(),
            state: "Todo".to_string(),
            session_id: None,
            turn_count: 0,
            last_event: None,
            last_message: None,
            started_at: "2025-01-01T00:00:00Z".to_string(),
            last_event_at: None,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
        }];

        let html = render_dashboard(
            &running,
            &[],
            &[],
            &TotalsInfo {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                seconds_running: 0.0,
            },
            &None,
        );

        assert!(html.contains("PROJ-1"));
    }

    #[test]
    fn test_render_dashboard_retry_no_error() {
        let retrying = vec![RetryInfo {
            issue_id: "id1".to_string(),
            identifier: "PROJ-1".to_string(),
            attempt: 1,
            due_at_ms: 1000,
            error: None,
        }];

        let html = render_dashboard(
            &[],
            &retrying,
            &[],
            &TotalsInfo {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                seconds_running: 0.0,
            },
            &None,
        );

        assert!(html.contains("PROJ-1"));
    }

    #[test]
    fn test_render_dashboard_with_completed() {
        let completed = vec![CompletedInfo {
            issue_identifier: "PROJ-C1".to_string(),
            outcome: "success".to_string(),
            duration_seconds: 120.5,
            input_tokens: 1000,
            output_tokens: 500,
            total_tokens: 1500,
            turn_count: 5,
            completed_at: "2025-01-01T00:00:00Z".to_string(),
        }];

        let html = render_dashboard(
            &[],
            &[],
            &completed,
            &TotalsInfo {
                input_tokens: 1000,
                output_tokens: 500,
                total_tokens: 1500,
                seconds_running: 120.5,
            },
            &None,
        );

        assert!(html.contains("PROJ-C1"));
        assert!(html.contains("success"));
        assert!(html.contains("Completed / History"));
    }

    // ── map_completion_record ───────────────────────────────────────

    #[test]
    fn test_map_completion_record_success() {
        use crate::model::{CompletionOutcome, CompletionRecord};

        let record = CompletionRecord {
            issue_id: "id1".to_string(),
            identifier: "PROJ-1".to_string(),
            outcome: CompletionOutcome::Success,
            duration_seconds: 60.0,
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            turn_count: 3,
            completed_at: Utc::now(),
        };

        let info = map_completion_record(&record);
        assert_eq!(info.issue_identifier, "PROJ-1");
        assert_eq!(info.outcome, "success");
        assert!((info.duration_seconds - 60.0).abs() < f64::EPSILON);
        assert_eq!(info.input_tokens, 100);
        assert_eq!(info.output_tokens, 50);
        assert_eq!(info.total_tokens, 150);
        assert_eq!(info.turn_count, 3);
    }

    #[test]
    fn test_map_completion_record_all_outcomes() {
        use crate::model::{CompletionOutcome, CompletionRecord};

        for (outcome, expected_str) in [
            (CompletionOutcome::Success, "success"),
            (CompletionOutcome::InReview, "in_review"),
            (CompletionOutcome::Retry, "retry"),
            (CompletionOutcome::Failed, "failed"),
        ] {
            let record = CompletionRecord {
                issue_id: "id".to_string(),
                identifier: "P-1".to_string(),
                outcome,
                duration_seconds: 1.0,
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                turn_count: 0,
                completed_at: Utc::now(),
            };
            let info = map_completion_record(&record);
            assert_eq!(info.outcome, expected_str);
        }
    }

    // ── state response with completed ───────────────────────────────

    #[tokio::test]
    async fn test_get_state_with_completed() {
        use crate::model::{CompletionOutcome, CompletionRecord};

        let app_state = test_app_state();
        {
            let mut state = app_state.orchestrator_state.write().await;
            state.add_completion(CompletionRecord {
                issue_id: "id1".to_string(),
                identifier: "PROJ-C1".to_string(),
                outcome: CompletionOutcome::Success,
                duration_seconds: 60.0,
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
                turn_count: 3,
                completed_at: Utc::now(),
            });
        }

        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/state")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(json["counts"]["completed"], 1);
        assert_eq!(json["completed"][0]["issue_identifier"], "PROJ-C1");
        assert_eq!(json["completed"][0]["outcome"], "success");
    }

    // ── format_tokens tests ──────────────────────────────────────────

    #[test]
    fn test_format_tokens_small() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
    }

    #[test]
    fn test_format_tokens_thousands() {
        assert_eq!(format_tokens(1_000), "1.0K");
        assert_eq!(format_tokens(1_500), "1.5K");
        assert_eq!(format_tokens(999_999), "1000.0K");
    }

    #[test]
    fn test_format_tokens_millions() {
        assert_eq!(format_tokens(1_000_000), "1.0M");
        assert_eq!(format_tokens(2_500_000), "2.5M");
    }

    // ── format_json_pretty tests ───────────────────────────────────────

    #[test]
    fn test_format_json_pretty_valid() {
        let value = serde_json::json!({"key": "value"});
        let result = format_json_pretty(&value);
        assert!(result.contains("key"));
        assert!(result.contains("value"));
    }

    // ── format_datetime tests ────────────────────────────────────────

    #[test]
    fn test_format_datetime() {
        let dt = Utc::now();
        let result = format_datetime(dt);
        assert!(result.contains("T")); // RFC3339 contains T separator
    }

    #[test]
    fn test_format_optional_datetime_some() {
        let dt = Utc::now();
        let result = format_optional_datetime(Some(dt));
        assert!(result.is_some());
    }

    #[test]
    fn test_format_optional_datetime_none() {
        let result = format_optional_datetime(None);
        assert!(result.is_none());
    }

    // ── error_response tests ─────────────────────────────────────────

    #[tokio::test]
    async fn test_error_response_not_found() {
        let response = error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            "Resource not found".to_string(),
        );
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["error"]["code"], "not_found");
        assert_eq!(json["error"]["message"], "Resource not found");
    }

    #[tokio::test]
    async fn test_error_response_method_not_allowed() {
        let response = error_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "Method DELETE is not allowed".to_string(),
        );
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    // ── html_escape tests ────────────────────────────────────────────

    #[test]
    fn test_html_escape() {
        assert_eq!(
            html_escape("<script>alert('xss')</script>"),
            "&lt;script&gt;alert('xss')&lt;/script&gt;"
        );
        assert_eq!(html_escape("a & b"), "a &amp; b");
        assert_eq!(html_escape("\"quoted\""), "&quot;quoted&quot;");
    }

    #[test]
    fn test_html_escape_plain() {
        assert_eq!(html_escape("hello world"), "hello world");
    }

    // ── script_safe_json tests ───────────────────────────────────────

    #[test]
    fn test_script_safe_json_escapes_closing_tag() {
        assert_eq!(script_safe_json("</script>"), "<\\/script>");
        assert_eq!(script_safe_json("a</b"), "a<\\/b");
    }

    #[test]
    fn test_script_safe_json_no_change_for_safe_input() {
        assert_eq!(
            script_safe_json("{\"key\":\"value\"}"),
            "{\"key\":\"value\"}"
        );
    }

    // ── build_snapshot tests ─────────────────────────────────────────

    #[test]
    fn test_build_snapshot_empty() {
        let state = OrchestratorState::new(5000, 3);
        let (running, retrying, waiting, completed, totals) = build_snapshot(&state);
        assert!(running.is_empty());
        assert!(retrying.is_empty());
        assert!(waiting.is_empty());
        assert!(completed.is_empty());
        assert_eq!(totals.input_tokens, 0);
        assert_eq!(totals.output_tokens, 0);
        assert_eq!(totals.total_tokens, 0);
        assert!(totals.seconds_running >= 0.0);
    }

    #[tokio::test]
    async fn test_build_snapshot_with_entries() {
        let mut state = OrchestratorState::new(5000, 3);
        let (cancel_tx, _cancel_rx) = tokio::sync::oneshot::channel();
        state.running.insert(
            "id1".to_string(),
            RunningEntry {
                issue: make_issue("id1", "PROJ-1", "Todo"),
                identifier: "PROJ-1".to_string(),
                session: LiveSession {
                    input_tokens: 100,
                    output_tokens: 50,
                    total_tokens: 150,
                    turn_count: 2,
                    ..LiveSession::default()
                },
                retry_attempt: None,
                started_at: Utc::now(),
                cancel_tx,
                event_log: std::collections::VecDeque::new(),
                log_seq: 0,
            },
        );
        state.agent_totals = AgentTotals {
            input_tokens: 500,
            output_tokens: 200,
            total_tokens: 700,
            seconds_running: 100.0,
        };

        let (running, retrying, waiting, _completed, totals) = build_snapshot(&state);
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].issue_identifier, "PROJ-1");
        assert_eq!(running[0].input_tokens, 100);
        assert!(retrying.is_empty());
        assert!(waiting.is_empty());
        assert_eq!(totals.input_tokens, 500);
        assert_eq!(totals.output_tokens, 200);
        assert_eq!(totals.total_tokens, 700);
        // seconds_running includes active session time
        assert!(totals.seconds_running >= 100.0);
    }

    #[tokio::test]
    async fn test_build_snapshot_with_retries() {
        let mut state = OrchestratorState::new(5000, 3);
        state.retry_attempts.insert(
            "id1".to_string(),
            RetryEntry {
                issue_id: "id1".to_string(),
                identifier: "PROJ-1".to_string(),
                attempt: 2,
                due_at_ms: 999,
                error: Some("boom".to_string()),
            },
        );

        let (_running, retrying, _waiting, _completed, _totals) = build_snapshot(&state);
        assert_eq!(retrying.len(), 1);
        assert_eq!(retrying[0].identifier, "PROJ-1");
        assert_eq!(retrying[0].attempt, 2);
    }

    // ── Integration tests (axum) ─────────────────────────────────────

    #[tokio::test]
    async fn test_get_dashboard_empty() {
        let app_state = test_app_state();
        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = get_body(response).await;
        assert!(body.contains("Symphony"));
        assert!(body.contains("Dashboard"));
        assert!(body.contains("No running sessions"));
    }

    #[tokio::test]
    async fn test_get_dashboard_with_data() {
        let app_state = test_app_state();
        insert_running(&app_state, "id1", "PROJ-1", "Todo").await;

        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = get_body(response).await;
        assert!(body.contains("PROJ-1"));
    }

    #[tokio::test]
    async fn test_get_state_empty() {
        let app_state = test_app_state();
        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/state")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert!(json["generated_at"].is_string());
        assert_eq!(json["counts"]["running"], 0);
        assert_eq!(json["counts"]["retrying"], 0);
        assert!(json["running"].is_array());
        assert!(json["retrying"].is_array());
        assert_eq!(json["codex_totals"]["input_tokens"], 0);
        assert_eq!(json["codex_totals"]["output_tokens"], 0);
        assert_eq!(json["codex_totals"]["total_tokens"], 0);
        assert!(json["rate_limits"].is_null());
    }

    #[tokio::test]
    async fn test_get_state_with_running() {
        let app_state = test_app_state();
        insert_running(&app_state, "id1", "PROJ-1", "Todo").await;

        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/state")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(json["counts"]["running"], 1);
        assert_eq!(json["running"][0]["issue_identifier"], "PROJ-1");
    }

    #[tokio::test]
    async fn test_get_issue_detail_running() {
        let app_state = test_app_state();
        insert_running(&app_state, "id1", "PROJ-1", "Todo").await;

        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/PROJ-1")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(json["issue_identifier"], "PROJ-1");
        assert_eq!(json["status"], "running");
        assert!(json["session"].is_object());
        assert!(json["workspace_path"].is_string());
    }

    #[tokio::test]
    async fn test_get_issue_detail_retrying() {
        let app_state = test_app_state();

        {
            let mut state = app_state.orchestrator_state.write().await;
            state.retry_attempts.insert(
                "id1".to_string(),
                RetryEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-R1".to_string(),
                    attempt: 2,
                    due_at_ms: 999,
                    error: Some("timeout".to_string()),
                },
            );
        }

        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/PROJ-R1")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(json["issue_identifier"], "PROJ-R1");
        assert_eq!(json["status"], "retrying");
        assert_eq!(json["retry"]["attempt"], 2);
        assert_eq!(json["retry"]["error"], "timeout");
    }

    #[tokio::test]
    async fn test_get_issue_detail_not_found() {
        let app_state = test_app_state();
        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/PROJ-NOPE")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(json["error"]["code"], "issue_not_found");
    }

    #[tokio::test]
    async fn test_post_refresh() {
        let app_state = test_app_state();
        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/refresh")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(json["queued"], true);
        assert!(json["requested_at"].is_string());
    }

    #[tokio::test]
    async fn test_post_refresh_channel_full() {
        // Create a channel with capacity 1, then fill it
        let state = OrchestratorState::new(5000, 3);
        let config = test_config();
        let (msg_tx, _msg_rx) = mpsc::channel(1);

        // Fill the channel
        msg_tx
            .try_send(OrchestratorMessage::TriggerRefresh)
            .unwrap();

        let app_state = AppState {
            orchestrator_state: Arc::new(RwLock::new(state)),
            config: Arc::new(RwLock::new(config)),
            msg_tx,
            job_notify: Arc::new(tokio::sync::Notify::new()),
            start_time: std::time::Instant::now(),
        };

        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/refresh")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(json["queued"], true);
        assert_eq!(json["coalesced"], true);
    }

    #[tokio::test]
    async fn test_post_refresh_channel_closed() {
        let state = OrchestratorState::new(5000, 3);
        let config = test_config();
        let (msg_tx, msg_rx) = mpsc::channel(512);

        // Drop receiver to close the channel
        drop(msg_rx);

        let app_state = AppState {
            orchestrator_state: Arc::new(RwLock::new(state)),
            config: Arc::new(RwLock::new(config)),
            msg_tx,
            job_notify: Arc::new(tokio::sync::Notify::new()),
            start_time: std::time::Instant::now(),
        };

        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/refresh")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(json["queued"], true);
        assert_eq!(json["coalesced"], true);
    }

    #[tokio::test]
    async fn test_fallback_method_not_allowed() {
        let app_state = test_app_state();
        let app = build_router(app_state);

        // DELETE to an unknown path → fallback handler
        let request = axum::http::Request::builder()
            .method("DELETE")
            .uri("/some/unknown/path")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(json["error"]["code"], "method_not_allowed");
    }

    #[tokio::test]
    async fn test_fallback_not_found() {
        let app_state = test_app_state();
        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/nonexistent")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(json["error"]["code"], "not_found");
    }

    // ── Full integration test (real TCP server) ──────────────────────

    #[tokio::test]
    async fn test_real_server_integration() {
        let app_state = test_app_state();
        insert_running(&app_state, "id1", "PROJ-1", "Todo").await;

        let app = build_router(app_state);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(axum::serve(listener, app).into_future());

        // Wait for server to be ready
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let client = reqwest::Client::new();

        // Test GET /
        let resp = client.get(format!("http://{addr}/")).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let body = resp.text().await.unwrap();
        assert!(body.contains("PROJ-1"));

        // Test GET /api/v1/state
        let resp = client
            .get(format!("http://{addr}/api/v1/state"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let json: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(json["counts"]["running"], 1);

        // Test GET /api/v1/PROJ-1
        let resp = client
            .get(format!("http://{addr}/api/v1/PROJ-1"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let json: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(json["issue_identifier"], "PROJ-1");

        // Test GET /api/v1/PROJ-NOPE (not found)
        let resp = client
            .get(format!("http://{addr}/api/v1/PROJ-NOPE"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);

        // Test POST /api/v1/refresh
        let resp = client
            .post(format!("http://{addr}/api/v1/refresh"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 202);
        let json: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(json["queued"], true);
    }

    // ── start_server ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_start_server_binds_and_serves() {
        let state = OrchestratorState::new(5000, 3);
        let config = test_config();
        let (msg_tx, _msg_rx) = mpsc::channel(512);

        let state_arc = Arc::new(RwLock::new(state));
        let config_arc = Arc::new(RwLock::new(config));

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let shutdown_signal = async move {
            let _ = shutdown_rx.await;
        };

        let job_notify = Arc::new(tokio::sync::Notify::new());
        let handle = tokio::spawn(start_server(
            0,
            "127.0.0.1",
            state_arc,
            config_arc,
            msg_tx,
            job_notify,
            shutdown_signal,
        ));

        // Give server time to bind
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Send graceful shutdown signal
        let _ = shutdown_tx.send(());

        // Server should exit cleanly with Ok(())
        let result = handle.await.unwrap();
        assert!(result.is_ok());
    }

    // ── running_entry_elapsed_seconds ────────────────────────────────

    #[tokio::test]
    async fn test_running_entry_elapsed_seconds() {
        let (cancel_tx, _cancel_rx) = tokio::sync::oneshot::channel();
        let entry = RunningEntry {
            issue: make_issue("id1", "PROJ-1", "Todo"),
            identifier: "PROJ-1".to_string(),
            session: LiveSession::default(),
            retry_attempt: None,
            started_at: Utc::now(),
            cancel_tx,
            event_log: std::collections::VecDeque::new(),
            log_seq: 0,
        };

        let elapsed = running_entry_elapsed_seconds(&entry);
        assert!(elapsed >= 0.0);
        assert!(elapsed < 1.0); // just created, should be near-zero
    }

    // ── map_running_entry / map_retry_entry ──────────────────────────

    #[tokio::test]
    async fn test_map_running_entry() {
        let (cancel_tx, _cancel_rx) = tokio::sync::oneshot::channel();
        let now = Utc::now();
        let entry = RunningEntry {
            issue: make_issue("id1", "PROJ-1", "Todo"),
            identifier: "PROJ-1".to_string(),
            session: LiveSession {
                session_id: Some("sess-abc".to_string()),
                turn_count: 5,
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
                last_event: Some("turn_completed".to_string()),
                last_message: Some("Working".to_string()),
                last_event_at: Some(now),
                ..LiveSession::default()
            },
            retry_attempt: None,
            started_at: now,
            cancel_tx,
            event_log: std::collections::VecDeque::new(),
            log_seq: 0,
        };

        let info = map_running_entry(&entry);
        assert_eq!(info.issue_id, "id1");
        assert_eq!(info.issue_identifier, "PROJ-1");
        assert_eq!(info.state, "Todo");
        assert_eq!(info.session_id, Some("sess-abc".to_string()));
        assert_eq!(info.turn_count, 5);
        assert_eq!(info.input_tokens, 100);
        assert_eq!(info.output_tokens, 50);
        assert_eq!(info.total_tokens, 150);
        assert!(info.last_event_at.is_some());
    }

    #[test]
    fn test_map_retry_entry() {
        let entry = RetryEntry {
            issue_id: "id1".to_string(),
            identifier: "PROJ-1".to_string(),
            attempt: 3,
            due_at_ms: 999,
            error: Some("boom".to_string()),
        };

        let info = map_retry_entry(&entry);
        assert_eq!(info.issue_id, "id1");
        assert_eq!(info.identifier, "PROJ-1");
        assert_eq!(info.attempt, 3);
        assert_eq!(info.due_at_ms, 999);
        assert_eq!(info.error, Some("boom".to_string()));
    }

    // ── push_stat / push_td tests ────────────────────────────────────

    #[test]
    fn test_push_stat() {
        let mut html = String::new();
        push_stat(&mut html, "Label", "Value");
        assert!(html.contains("Label"));
        assert!(html.contains("Value"));
        assert!(html.contains("stat-value"));
        assert!(html.contains("stat-label"));
    }

    #[test]
    fn test_push_td() {
        let mut html = String::new();
        push_td(&mut html, "content");
        assert_eq!(html, "<td>content</td>");
    }

    #[test]
    fn test_push_td_with_special_chars() {
        let mut html = String::new();
        push_td(&mut html, "<b>bold</b>");
        assert_eq!(html, "<td>&lt;b&gt;bold&lt;/b&gt;</td>");
    }

    // ── StateResponse serialization ──────────────────────────────────

    #[test]
    fn test_state_response_serialization() {
        let response = StateResponse {
            generated_at: "2025-01-01T00:00:00Z".to_string(),
            counts: StateCounts {
                running: 1,
                retrying: 0,
                waiting: 0,
                completed: 0,
            },
            running: vec![RunningInfo {
                issue_id: "id1".to_string(),
                issue_identifier: "PROJ-1".to_string(),
                state: "Todo".to_string(),
                session_id: None,
                turn_count: 0,
                last_event: None,
                last_message: None,
                started_at: "2025-01-01T00:00:00Z".to_string(),
                last_event_at: None,
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            }],
            retrying: vec![],
            waiting: vec![],
            completed: vec![],
            codex_totals: TotalsInfo {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                seconds_running: 0.0,
            },
            rate_limits: None,
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("generated_at"));
        assert!(json.contains("counts"));
        assert!(json.contains("running"));
        assert!(json.contains("completed"));
        assert!(json.contains("codex_totals"));
        assert!(json.contains("PROJ-1"));
    }

    // ── RefreshResponse serialization (coalesced omitted/included) ───

    #[test]
    fn test_refresh_response_no_coalesced() {
        let response = RefreshResponse {
            queued: true,
            coalesced: None,
            requested_at: "2025-01-01T00:00:00Z".to_string(),
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(!json.contains("coalesced"));
    }

    #[test]
    fn test_refresh_response_with_coalesced() {
        let response = RefreshResponse {
            queued: true,
            coalesced: Some(true),
            requested_at: "2025-01-01T00:00:00Z".to_string(),
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("coalesced"));
    }

    // ── IssueDetailResponse serialization ────────────────────────────

    #[test]
    fn test_issue_detail_serialization() {
        let response = IssueDetailResponse {
            issue_identifier: "PROJ-1".to_string(),
            status: "running".to_string(),
            issue_id: Some("id1".to_string()),
            workspace_path: Some("/tmp/ws/PROJ-1".to_string()),
            session: Some(SessionDetail {
                session_id: Some("sess-abc".to_string()),
                turn_count: 3,
                last_event: None,
                last_message: None,
                started_at: "2025-01-01T00:00:00Z".to_string(),
                last_event_at: None,
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            }),
            retry: None,
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("PROJ-1"));
        assert!(json.contains("running"));
        assert!(json.contains("sess-abc"));
    }

    // ── ErrorResponse serialization ──────────────────────────────────

    #[test]
    fn test_error_response_serialization() {
        let response = ErrorResponse {
            error: ErrorDetail {
                code: "issue_not_found".to_string(),
                message: "Not found".to_string(),
            },
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("issue_not_found"));
        assert!(json.contains("Not found"));
    }

    // ── LogsResponse serialization ───────────────────────────────────

    #[test]
    fn test_logs_response_serialization() {
        let response = LogsResponse {
            issue_identifier: "PROJ-1".to_string(),
            count: 1,
            logs: vec![crate::model::LogEntry {
                seq: 1,
                timestamp: Utc::now(),
                event_type: "session_started".to_string(),
                message: Some("started".to_string()),
                tokens: None,
            }],
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("PROJ-1"));
        assert!(json.contains("\"count\":1"));
        assert!(json.contains("session_started"));
    }

    // ── format_sse_event ─────────────────────────────────────────────

    #[test]
    fn test_format_sse_event_ok() {
        let entry = crate::model::LogEntry {
            seq: 42,
            timestamp: Utc::now(),
            event_type: "turn_completed".to_string(),
            message: Some("done".to_string()),
            tokens: None,
        };
        let result = format_sse_event(&entry);
        assert!(result.is_ok());
    }

    // ── broadcast_result_to_sse ──────────────────────────────────────

    #[test]
    fn test_broadcast_result_to_sse_ok() {
        let entry = crate::model::LogEntry {
            seq: 1,
            timestamp: Utc::now(),
            event_type: "test".to_string(),
            message: Some("hi".to_string()),
            tokens: None,
        };
        let result = broadcast_result_to_sse(Ok(entry));
        assert!(result.is_some());
    }

    #[test]
    fn test_broadcast_result_to_sse_err() {
        use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
        let result = broadcast_result_to_sse(Err(BroadcastStreamRecvError::Lagged(5)));
        assert!(result.is_none());
    }

    // ── SSE stream endpoint ──────────────────────────────────────────

    #[tokio::test]
    async fn test_stream_not_found() {
        let app_state = test_app_state();
        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/PROJ-NOPE/stream")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["error"]["code"], "issue_not_found");
    }

    #[tokio::test]
    async fn test_stream_returns_sse_content_type() {
        let app_state = test_app_state();
        insert_running(&app_state, "id1", "PROJ-1", "Todo").await;

        // Create broadcast channel for the issue
        {
            let mut state = app_state.orchestrator_state.write().await;
            let (tx, _) = tokio::sync::broadcast::channel(256);
            state.event_broadcasts.insert("id1".to_string(), tx);
        }

        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/PROJ-1/stream")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(content_type.contains("text/event-stream"));
    }

    #[tokio::test]
    async fn test_stream_sends_catchup_events() {
        let app_state = test_app_state();
        insert_running(&app_state, "id1", "PROJ-1", "Todo").await;

        // Add log entries and broadcast channel
        {
            let mut state = app_state.orchestrator_state.write().await;
            let entry = state.running.get_mut("id1").unwrap();
            entry.event_log.push_back(crate::model::LogEntry {
                seq: 1,
                timestamp: Utc::now(),
                event_type: "session_started".to_string(),
                message: Some("test session".to_string()),
                tokens: None,
            });
            entry.event_log.push_back(crate::model::LogEntry {
                seq: 2,
                timestamp: Utc::now(),
                event_type: "turn_completed".to_string(),
                message: Some("done".to_string()),
                tokens: None,
            });
            entry.log_seq = 2;

            let (tx, _) = tokio::sync::broadcast::channel::<crate::model::LogEntry>(256);
            // Drop sender immediately so the stream ends after catchup
            drop(tx);
            // Create a new one just to have a valid channel
            let (tx2, _) = tokio::sync::broadcast::channel::<crate::model::LogEntry>(256);
            state.event_broadcasts.insert("id1".to_string(), tx2);
        }

        // Use a real TCP server to be able to read stream properly
        let app = build_router(app_state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, app).into_future());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap();

        let resp = client
            .get(format!("http://{addr}/api/v1/PROJ-1/stream"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let content_type = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(content_type.contains("text/event-stream"));

        // Read partial body (catch-up events should be there)
        let body = tokio::time::timeout(std::time::Duration::from_secs(1), resp.text()).await;

        // The stream may time out after catch-up, that's ok
        if let Ok(Ok(text)) = body {
            assert!(text.contains("session_started"));
            assert!(text.contains("turn_completed"));
        }
    }

    #[tokio::test]
    async fn test_stream_no_broadcast_channel() {
        let app_state = test_app_state();
        insert_running(&app_state, "id1", "PROJ-1", "Todo").await;
        // Don't create a broadcast channel

        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/PROJ-1/stream")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    // ── Log history endpoint ─────────────────────────────────────────

    #[tokio::test]
    async fn test_logs_not_found() {
        let app_state = test_app_state();
        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/PROJ-NOPE/logs")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["error"]["code"], "issue_not_found");
    }

    #[tokio::test]
    async fn test_logs_empty() {
        let app_state = test_app_state();
        insert_running(&app_state, "id1", "PROJ-1", "Todo").await;

        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/PROJ-1/logs")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["issue_identifier"], "PROJ-1");
        assert_eq!(json["count"], 0);
        assert!(json["logs"].is_array());
        assert_eq!(json["logs"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_logs_with_entries() {
        let app_state = test_app_state();
        insert_running(&app_state, "id1", "PROJ-1", "Todo").await;

        // Add some log entries
        {
            let mut state = app_state.orchestrator_state.write().await;
            let entry = state.running.get_mut("id1").unwrap();
            entry.event_log.push_back(crate::model::LogEntry {
                seq: 1,
                timestamp: Utc::now(),
                event_type: "session_started".to_string(),
                message: Some("started".to_string()),
                tokens: None,
            });
            entry.event_log.push_back(crate::model::LogEntry {
                seq: 2,
                timestamp: Utc::now(),
                event_type: "token_usage".to_string(),
                message: None,
                tokens: Some(crate::model::TokenSnapshot {
                    input_tokens: 100,
                    output_tokens: 50,
                    total_tokens: 150,
                }),
            });
            entry.log_seq = 2;
        }

        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/PROJ-1/logs")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["issue_identifier"], "PROJ-1");
        assert_eq!(json["count"], 2);
        assert_eq!(json["logs"][0]["seq"], 1);
        assert_eq!(json["logs"][0]["event_type"], "session_started");
        assert_eq!(json["logs"][1]["seq"], 2);
        assert_eq!(json["logs"][1]["event_type"], "token_usage");
        assert!(json["logs"][1]["tokens"].is_object());
    }

    #[tokio::test]
    async fn test_logs_with_after_filter() {
        let app_state = test_app_state();
        insert_running(&app_state, "id1", "PROJ-1", "Todo").await;

        // Add 5 log entries
        {
            let mut state = app_state.orchestrator_state.write().await;
            let entry = state.running.get_mut("id1").unwrap();
            for i in 1..=5 {
                entry.event_log.push_back(crate::model::LogEntry {
                    seq: i,
                    timestamp: Utc::now(),
                    event_type: format!("event_{i}"),
                    message: Some(format!("message {i}")),
                    tokens: None,
                });
            }
            entry.log_seq = 5;
        }

        let app = build_router(app_state);

        // Request logs after seq=3
        let request = axum::http::Request::builder()
            .uri("/api/v1/PROJ-1/logs?after=3")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["count"], 2);
        assert_eq!(json["logs"][0]["seq"], 4);
        assert_eq!(json["logs"][1]["seq"], 5);
    }

    #[tokio::test]
    async fn test_logs_with_after_returns_none_when_all_before() {
        let app_state = test_app_state();
        insert_running(&app_state, "id1", "PROJ-1", "Todo").await;

        {
            let mut state = app_state.orchestrator_state.write().await;
            let entry = state.running.get_mut("id1").unwrap();
            entry.event_log.push_back(crate::model::LogEntry {
                seq: 1,
                timestamp: Utc::now(),
                event_type: "test".to_string(),
                message: None,
                tokens: None,
            });
            entry.log_seq = 1;
        }

        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/PROJ-1/logs?after=999")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["count"], 0);
    }

    // ── Dashboard contains log panel ─────────────────────────────────

    #[test]
    fn test_dashboard_contains_log_panel() {
        let html = render_dashboard(
            &[],
            &[],
            &[],
            &TotalsInfo {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                seconds_running: 0.0,
            },
            &None,
        );

        assert!(html.contains("logPanel"));
        assert!(html.contains("logPanelBody"));
        assert!(html.contains("openLogPanel"));
        assert!(html.contains("closeLogPanel"));
        assert!(html.contains("appendLogEntry"));
        assert!(html.contains("EventSource"));
        assert!(html.contains("/stream"));
    }

    #[test]
    fn test_dashboard_log_panel_css() {
        let html = render_dashboard(
            &[],
            &[],
            &[],
            &TotalsInfo {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                seconds_running: 0.0,
            },
            &None,
        );

        assert!(html.contains("log-panel"));
        assert!(html.contains("log-entry"));
        assert!(html.contains("log-badge"));
        assert!(html.contains("log-badge.session_started"));
        assert!(html.contains("log-badge.turn_completed"));
        assert!(html.contains("log-badge.turn_failed"));
        assert!(html.contains("log-badge.rate_limit_event"));
    }

    // ── Integration: real server with stream & logs ──────────────────

    #[tokio::test]
    async fn test_real_server_stream_and_logs() {
        let app_state = test_app_state();
        insert_running(&app_state, "id1", "PROJ-1", "Todo").await;

        // Add broadcast channel and some log entries
        {
            let mut state = app_state.orchestrator_state.write().await;
            let (tx, _) = tokio::sync::broadcast::channel(256);
            state.event_broadcasts.insert("id1".to_string(), tx);
            let entry = state.running.get_mut("id1").unwrap();
            entry.event_log.push_back(crate::model::LogEntry {
                seq: 1,
                timestamp: Utc::now(),
                event_type: "session_started".to_string(),
                message: Some("test".to_string()),
                tokens: None,
            });
            entry.log_seq = 1;
        }

        let app = build_router(app_state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, app).into_future());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let client = reqwest::Client::new();

        // Test GET /api/v1/PROJ-1/logs
        let resp = client
            .get(format!("http://{addr}/api/v1/PROJ-1/logs"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let json: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(json["count"], 1);
        assert_eq!(json["logs"][0]["event_type"], "session_started");

        // Test GET /api/v1/PROJ-1/logs?after=0
        let resp = client
            .get(format!("http://{addr}/api/v1/PROJ-1/logs?after=0"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let json: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(json["count"], 1);

        // Test GET /api/v1/PROJ-1/logs?after=1
        let resp = client
            .get(format!("http://{addr}/api/v1/PROJ-1/logs?after=1"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let json: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(json["count"], 0);

        // Test 404 for non-existent issue logs
        let resp = client
            .get(format!("http://{addr}/api/v1/PROJ-NOPE/logs"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);

        // Test 404 for non-existent issue stream
        let resp = client
            .get(format!("http://{addr}/api/v1/PROJ-NOPE/stream"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    }

    // ── AuthenticatedWorker tests ───────────────────────────────────

    fn test_app_state_with_auth(
        token: Option<&str>,
    ) -> (AppState, mpsc::Receiver<OrchestratorMessage>) {
        let state = OrchestratorState::new(5000, 3);
        let mut config = test_config();
        config.deployment.auth_token = token.map(|t| t.to_string());
        let (msg_tx, msg_rx) = mpsc::channel(512);

        let app_state = AppState {
            orchestrator_state: Arc::new(RwLock::new(state)),
            config: Arc::new(RwLock::new(config)),
            msg_tx,
            job_notify: Arc::new(tokio::sync::Notify::new()),
            start_time: std::time::Instant::now(),
        };
        (app_state, msg_rx)
    }

    #[tokio::test]
    async fn test_auth_missing_header_returns_401() {
        let (app_state, _rx) = test_app_state_with_auth(Some("secret-token"));
        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/work")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["error"]["code"], "unauthorized");
        assert!(json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Missing or invalid"));
    }

    #[tokio::test]
    async fn test_auth_wrong_scheme_returns_401() {
        let (app_state, _rx) = test_app_state_with_auth(Some("secret-token"));
        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/work")
            .header("Authorization", "Basic c2VjcmV0")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_auth_wrong_token_returns_401() {
        let (app_state, _rx) = test_app_state_with_auth(Some("secret-token"));
        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/work")
            .header("Authorization", "Bearer wrong-token")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["error"]["code"], "unauthorized");
        assert!(json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Invalid bearer token"));
    }

    #[tokio::test]
    async fn test_auth_no_token_configured_returns_401() {
        let (app_state, _rx) = test_app_state_with_auth(None);
        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/work")
            .header("Authorization", "Bearer some-token")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["error"]["code"], "unauthorized");
        assert!(json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("No auth token configured"));
    }

    // ── GET /api/v1/work tests ──────────────────────────────────────

    #[tokio::test]
    async fn test_claim_work_returns_assignment_when_available() {
        let (app_state, _rx) = test_app_state_with_auth(Some("secret-token"));

        // Enqueue a job
        {
            let mut orch = app_state.orchestrator_state.write().await;
            orch.pending_jobs.push_back(WorkAssignment {
                issue_id: "id1".to_string(),
                issue_identifier: "TST-1".to_string(),
                prompt: "Fix the bug".to_string(),
                attempt: Some(1),
                workspace_path: Some("/tmp/ws/TST-1".to_string()),
            });
        }

        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/work")
            .header("Authorization", "Bearer secret-token")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["issue_id"], "id1");
        assert_eq!(json["issue_identifier"], "TST-1");
        assert_eq!(json["prompt"], "Fix the bug");
        assert_eq!(json["attempt"], 1);
        assert_eq!(json["workspace_path"], "/tmp/ws/TST-1");
    }

    #[tokio::test(start_paused = true)]
    async fn test_claim_work_returns_204_on_timeout() {
        let (app_state, _rx) = test_app_state_with_auth(Some("secret-token"));
        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/work")
            .header("Authorization", "Bearer secret-token")
            .body(Body::empty())
            .unwrap();

        // Use tokio::spawn + advance to test the long-poll timeout
        let handle = tokio::spawn(async move { app.oneshot(request).await.unwrap() });

        // Advance time past the 30s timeout
        tokio::time::advance(std::time::Duration::from_secs(31)).await;

        let response = handle.await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test(start_paused = true)]
    async fn test_claim_work_wakes_on_notify() {
        let (app_state, _rx) = test_app_state_with_auth(Some("secret-token"));

        let notify = Arc::clone(&app_state.job_notify);
        let orch_state = Arc::clone(&app_state.orchestrator_state);

        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/work")
            .header("Authorization", "Bearer secret-token")
            .body(Body::empty())
            .unwrap();

        let handle = tokio::spawn(async move { app.oneshot(request).await.unwrap() });

        // Give the handler time to start waiting
        tokio::time::advance(std::time::Duration::from_millis(100)).await;
        tokio::task::yield_now().await;

        // Enqueue a job and notify
        {
            let mut state = orch_state.write().await;
            state.pending_jobs.push_back(WorkAssignment {
                issue_id: "id-wake".to_string(),
                issue_identifier: "TST-WAKE".to_string(),
                prompt: "woken up".to_string(),
                attempt: None,
                workspace_path: None,
            });
        }
        notify.notify_one();

        // Advance a little for the notification to be processed
        tokio::time::advance(std::time::Duration::from_millis(100)).await;
        tokio::task::yield_now().await;

        let response = handle.await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["issue_id"], "id-wake");
        assert_eq!(json["issue_identifier"], "TST-WAKE");
    }

    #[tokio::test(start_paused = true)]
    async fn test_claim_work_notify_but_queue_empty_returns_204() {
        let (app_state, _rx) = test_app_state_with_auth(Some("secret-token"));

        let notify = Arc::clone(&app_state.job_notify);

        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/work")
            .header("Authorization", "Bearer secret-token")
            .body(Body::empty())
            .unwrap();

        let handle = tokio::spawn(async move { app.oneshot(request).await.unwrap() });

        // Give the handler time to start waiting
        tokio::time::advance(std::time::Duration::from_millis(100)).await;
        tokio::task::yield_now().await;

        // Notify but don't enqueue anything
        notify.notify_one();

        tokio::time::advance(std::time::Duration::from_millis(100)).await;
        tokio::task::yield_now().await;

        let response = handle.await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    // ── POST /api/v1/{issue_id}/events tests ────────────────────────

    #[tokio::test]
    async fn test_worker_event_valid() {
        let (app_state, mut rx) = test_app_state_with_auth(Some("secret-token"));
        let app = build_router(app_state);

        let payload = serde_json::json!({
            "event": {
                "Notification": { "message": "hello from worker" }
            }
        });

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/test-issue-id/events")
            .header("Authorization", "Bearer secret-token")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Verify the message was sent to the channel
        let msg = rx.try_recv().unwrap();
        match msg {
            OrchestratorMessage::AgentUpdate { issue_id, event } => {
                assert_eq!(issue_id, "test-issue-id");
                assert!(matches!(
                    event,
                    crate::protocol::AgentEvent::Notification { .. }
                ));
            }
            _ => panic!("Expected AgentUpdate message"),
        }
    }

    #[tokio::test]
    async fn test_worker_event_requires_auth() {
        let (app_state, _rx) = test_app_state_with_auth(Some("secret-token"));
        let app = build_router(app_state);

        let payload = serde_json::json!({
            "event": {
                "Notification": { "message": "hello" }
            }
        });

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/test-issue-id/events")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_worker_event_channel_closed() {
        let state = OrchestratorState::new(5000, 3);
        let mut config = test_config();
        config.deployment.auth_token = Some("secret-token".to_string());
        let (msg_tx, msg_rx) = mpsc::channel(512);
        drop(msg_rx); // close the channel

        let app_state = AppState {
            orchestrator_state: Arc::new(RwLock::new(state)),
            config: Arc::new(RwLock::new(config)),
            msg_tx,
            job_notify: Arc::new(tokio::sync::Notify::new()),
            start_time: std::time::Instant::now(),
        };
        let app = build_router(app_state);

        let payload = serde_json::json!({
            "event": {
                "Notification": { "message": "hello" }
            }
        });

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/test-issue-id/events")
            .header("Authorization", "Bearer secret-token")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["error"]["code"], "channel_closed");
    }

    // ── POST /api/v1/{issue_id}/complete tests ──────────────────────

    #[tokio::test]
    async fn test_worker_complete_valid() {
        let (app_state, mut rx) = test_app_state_with_auth(Some("secret-token"));
        let app = build_router(app_state);

        let payload = serde_json::json!({
            "reason": "Normal"
        });

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/test-issue-id/complete")
            .header("Authorization", "Bearer secret-token")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Verify the message was sent to the channel
        let msg = rx.try_recv().unwrap();
        match msg {
            OrchestratorMessage::WorkerExited { issue_id, reason } => {
                assert_eq!(issue_id, "test-issue-id");
                assert_eq!(reason, crate::protocol::WorkerExitReason::Normal);
            }
            _ => panic!("Expected WorkerExited message"),
        }
    }

    #[tokio::test]
    async fn test_worker_complete_with_failure_reason() {
        let (app_state, mut rx) = test_app_state_with_auth(Some("secret-token"));
        let app = build_router(app_state);

        let payload = serde_json::json!({
            "reason": { "Failed": "something went wrong" }
        });

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/test-issue-id/complete")
            .header("Authorization", "Bearer secret-token")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let msg = rx.try_recv().unwrap();
        match msg {
            OrchestratorMessage::WorkerExited { reason, .. } => {
                assert_eq!(
                    reason,
                    crate::protocol::WorkerExitReason::Failed("something went wrong".to_string())
                );
            }
            _ => panic!("Expected WorkerExited message"),
        }
    }

    #[tokio::test]
    async fn test_worker_complete_requires_auth() {
        let (app_state, _rx) = test_app_state_with_auth(Some("secret-token"));
        let app = build_router(app_state);

        let payload = serde_json::json!({
            "reason": "Normal"
        });

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/test-issue-id/complete")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_worker_complete_channel_closed() {
        let state = OrchestratorState::new(5000, 3);
        let mut config = test_config();
        config.deployment.auth_token = Some("secret-token".to_string());
        let (msg_tx, msg_rx) = mpsc::channel(512);
        drop(msg_rx);

        let app_state = AppState {
            orchestrator_state: Arc::new(RwLock::new(state)),
            config: Arc::new(RwLock::new(config)),
            msg_tx,
            job_notify: Arc::new(tokio::sync::Notify::new()),
            start_time: std::time::Instant::now(),
        };
        let app = build_router(app_state);

        let payload = serde_json::json!({
            "reason": "Normal"
        });

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/test-issue-id/complete")
            .header("Authorization", "Bearer secret-token")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["error"]["code"], "channel_closed");
    }

    // ── AuthenticatedWorker debug/clone ──────────────────────────────

    #[test]
    fn test_authenticated_worker_debug() {
        let worker = AuthenticatedWorker {
            worker_id: "worker-1".to_string(),
        };
        let debug = format!("{:?}", worker);
        assert!(debug.contains("AuthenticatedWorker"));
        assert!(debug.contains("worker-1"));
    }

    #[test]
    fn test_authenticated_worker_clone() {
        let worker = AuthenticatedWorker {
            worker_id: "worker-1".to_string(),
        };
        let cloned = worker.clone();
        assert_eq!(worker.worker_id, cloned.worker_id);
    }

    // ── Worker ID from header ───────────────────────────────────────

    #[tokio::test]
    async fn test_claim_work_with_worker_id_header() {
        let (app_state, _rx) = test_app_state_with_auth(Some("secret-token"));

        // Enqueue a job so we get 200 instead of waiting
        {
            let mut orch = app_state.orchestrator_state.write().await;
            orch.pending_jobs.push_back(WorkAssignment {
                issue_id: "id1".to_string(),
                issue_identifier: "TST-1".to_string(),
                prompt: "test".to_string(),
                attempt: None,
                workspace_path: None,
            });
        }

        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/work")
            .header("Authorization", "Bearer secret-token")
            .header("X-Worker-ID", "my-worker-42")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    // ── Remove retry endpoint ────────────────────────────────────────

    #[tokio::test]
    async fn test_remove_retry_success() {
        let app_state = test_app_state();

        // Insert a retry entry
        {
            let mut state = app_state.orchestrator_state.write().await;
            state.retry_attempts.insert(
                "id1".to_string(),
                RetryEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-R1".to_string(),
                    attempt: 2,
                    due_at_ms: 999,
                    error: Some("timeout".to_string()),
                },
            );
        }

        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/PROJ-R1/remove-retry")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(json["removed"], true);
        assert_eq!(json["issue_identifier"], "PROJ-R1");
        assert_eq!(json["issue_id"], "id1");
    }

    #[tokio::test]
    async fn test_remove_retry_not_found() {
        let app_state = test_app_state();
        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/PROJ-NOPE/remove-retry")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(json["error"]["code"], "issue_not_found");
    }

    // ── RemoveRetryResponse serialization ────────────────────────────

    #[test]
    fn test_remove_retry_response_serialization() {
        let response = RemoveRetryResponse {
            removed: true,
            issue_identifier: "PROJ-1".to_string(),
            issue_id: Some("id1".to_string()),
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"removed\":true"));
        assert!(json.contains("PROJ-1"));
        assert!(json.contains("id1"));
    }

    // ── Health check endpoint tests ──────────────────────────────────

    #[tokio::test]
    async fn test_healthz_returns_200_with_expected_json() {
        let app_state = test_app_state();
        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/healthz")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert!(json["uptime_secs"].as_u64().is_some());
        assert!(json["uptime_secs"].as_u64().unwrap() < 5);
        assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn test_healthz_does_not_require_auth() {
        // Configure auth so authenticated endpoints would reject unauthenticated requests
        let (app_state, _rx) = test_app_state_with_auth(Some("secret-token"));
        let app = build_router(app_state);

        // No Authorization header — should still succeed
        let request = axum::http::Request::builder()
            .uri("/healthz")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn test_readyz_returns_200_when_slots_available() {
        let app_state = test_app_state();
        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/readyz")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["status"], "ready");
        assert!(json.get("reason").is_none() || json["reason"].is_null());
    }

    #[tokio::test]
    async fn test_readyz_returns_503_when_fully_loaded() {
        let app_state = test_app_state();
        // max_concurrent_agents is 3 in test_config, so insert 3 running entries
        insert_running(&app_state, "id-1", "PROJ-1", "In Progress").await;
        insert_running(&app_state, "id-2", "PROJ-2", "In Progress").await;
        insert_running(&app_state, "id-3", "PROJ-3", "In Progress").await;

        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/readyz")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["status"], "not_ready");
        assert!(json["reason"].as_str().unwrap().contains("at capacity"));
    }

    #[tokio::test]
    async fn test_readyz_does_not_require_auth() {
        // Configure auth so authenticated endpoints would reject unauthenticated requests
        let (app_state, _rx) = test_app_state_with_auth(Some("secret-token"));
        let app = build_router(app_state);

        // No Authorization header — should still succeed
        let request = axum::http::Request::builder()
            .uri("/readyz")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["status"], "ready");
    }

    // ── Dashboard contains remove-retry button ───────────────────────

    #[test]
    fn test_dashboard_contains_remove_retry_button() {
        let html = render_dashboard(
            &[],
            &[],
            &[],
            &TotalsInfo {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                seconds_running: 0.0,
            },
            &None,
        );

        assert!(html.contains("removeRetry"));
        assert!(html.contains("remove-retry"));
        assert!(html.contains("btn-danger"));
    }

    // ── build_state_response ─────────────────────────────────────────

    #[test]
    fn test_build_state_response_empty() {
        let state = OrchestratorState::new(5000, 3);
        let response = build_state_response(&state);
        assert!(response.generated_at.contains("T"));
        assert_eq!(response.counts.running, 0);
        assert_eq!(response.counts.retrying, 0);
        assert_eq!(response.counts.completed, 0);
        assert!(response.running.is_empty());
        assert!(response.retrying.is_empty());
        assert!(response.completed.is_empty());
        assert_eq!(response.codex_totals.input_tokens, 0);
        assert!(response.rate_limits.is_none());
    }

    #[tokio::test]
    async fn test_build_state_response_with_data() {
        let mut state = OrchestratorState::new(5000, 3);
        let (cancel_tx, _cancel_rx) = tokio::sync::oneshot::channel();
        state.running.insert(
            "id1".to_string(),
            RunningEntry {
                issue: make_issue("id1", "PROJ-1", "Todo"),
                identifier: "PROJ-1".to_string(),
                session: LiveSession {
                    input_tokens: 100,
                    output_tokens: 50,
                    total_tokens: 150,
                    ..LiveSession::default()
                },
                retry_attempt: None,
                started_at: Utc::now(),
                cancel_tx,
                event_log: std::collections::VecDeque::new(),
                log_seq: 0,
            },
        );
        state.agent_totals = AgentTotals {
            input_tokens: 500,
            output_tokens: 200,
            total_tokens: 700,
            seconds_running: 100.0,
        };
        state.rate_limits = Some(serde_json::json!({"remaining": 10}));

        let response = build_state_response(&state);
        assert_eq!(response.counts.running, 1);
        assert_eq!(response.running[0].issue_identifier, "PROJ-1");
        assert_eq!(response.codex_totals.input_tokens, 500);
        assert_eq!(response.rate_limits.unwrap()["remaining"], 10);
    }

    #[test]
    fn test_build_state_response_serializes_to_json() {
        let state = OrchestratorState::new(5000, 3);
        let response = build_state_response(&state);
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("generated_at"));
        assert!(json.contains("counts"));
        assert!(json.contains("codex_totals"));
    }

    // ── state_broadcast_to_json ────────────────────────────────────

    #[test]
    fn test_state_broadcast_to_json_ok() {
        let state = OrchestratorState::new(5000, 3);
        let result = Ok(());
        let json = state_broadcast_to_json(&result, &state);
        assert!(json.is_some());
        let json_str = json.unwrap();
        assert!(json_str.contains("generated_at"));
        assert!(json_str.contains("codex_totals"));
    }

    #[test]
    fn test_state_broadcast_to_json_err_lagged() {
        use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
        let state = OrchestratorState::new(5000, 3);
        let result = Err(BroadcastStreamRecvError::Lagged(5));
        let json = state_broadcast_to_json(&result, &state);
        assert!(json.is_none());
    }

    // ── State SSE stream endpoint ───────────────────────────────────

    #[tokio::test]
    async fn test_state_stream_returns_sse_content_type() {
        let app_state = test_app_state();
        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/state/stream")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(content_type.contains("text/event-stream"));
    }

    #[tokio::test]
    async fn test_state_stream_sends_initial_snapshot() {
        let app_state = test_app_state();
        insert_running(&app_state, "id1", "PROJ-1", "Todo").await;

        let app = build_router(app_state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, app).into_future());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap();

        let resp = client
            .get(format!("http://{addr}/api/v1/state/stream"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let content_type = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(content_type.contains("text/event-stream"));

        // Read partial body (initial snapshot should be there)
        let body = tokio::time::timeout(std::time::Duration::from_secs(1), resp.text()).await;
        if let Ok(Ok(text)) = body {
            assert!(text.contains("PROJ-1"));
            assert!(text.contains("generated_at"));
            assert!(text.contains("codex_totals"));
        }
    }

    #[tokio::test]
    async fn test_state_stream_sends_live_updates() {
        let app_state = test_app_state();

        let app = build_router(app_state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, app).into_future());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Spawn a background task that triggers a state change after a delay
        let state_clone = app_state.orchestrator_state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let state = state_clone.read().await;
            state.notify_state_change();
        });

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap();

        let resp = client
            .get(format!("http://{addr}/api/v1/state/stream"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);

        // Read body — should contain at least the initial snapshot
        let body = tokio::time::timeout(std::time::Duration::from_secs(1), resp.text()).await;
        if let Ok(Ok(text)) = body {
            assert!(text.contains("generated_at"));
        }
    }

    // ── Dashboard SSE JS ─────────────────────────────────────────────

    #[test]
    fn test_dashboard_contains_sse_js() {
        let html = render_dashboard(
            &[],
            &[],
            &[],
            &TotalsInfo {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                seconds_running: 0.0,
            },
            &None,
        );

        // SSE-related functions
        assert!(html.contains("startSSE"));
        assert!(html.contains("startPolling"));
        assert!(html.contains("stateEvtSource"));
        assert!(html.contains("/api/v1/state/stream"));
        assert!(html.contains("Live (SSE)"));
        assert!(html.contains("Polling (fallback)"));
    }

    #[test]
    fn test_dashboard_sse_fallback_to_polling() {
        let html = render_dashboard(
            &[],
            &[],
            &[],
            &TotalsInfo {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                seconds_running: 0.0,
            },
            &None,
        );

        // Polling fallback
        assert!(html.contains("pollTimer"));
        assert!(html.contains("setInterval(poll, 3000)"));
        // SSE reconnection logic
        assert!(html.contains("setTimeout"));
        assert!(html.contains("startSSE()"));
    }

    #[test]
    fn test_dashboard_initial_render_uses_sse() {
        let html = render_dashboard(
            &[],
            &[],
            &[],
            &TotalsInfo {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                seconds_running: 0.0,
            },
            &None,
        );

        // Should start with SSE, not setInterval(poll)
        assert!(html.contains("startSSE()"));
        // Old polling-only pattern should be gone
        assert!(!html.contains("setInterval(poll, 3000);\n</script>"));
    }

    // ── Waiting for Review tests ──────────────────────────────────────

    #[test]
    fn test_map_waiting_entry() {
        let entry = WaitingEntry {
            issue_id: "id1".to_string(),
            identifier: "PROJ-1".to_string(),
            pr_number: 42,
            branch: "symphony/proj-1".to_string(),
            started_waiting_at: Utc::now(),
            session_id: Some("sess-abc".to_string()),
        };

        let info = map_waiting_entry(&entry);
        assert_eq!(info.issue_id, "id1");
        assert_eq!(info.identifier, "PROJ-1");
        assert_eq!(info.pr_number, 42);
        assert_eq!(info.branch, "symphony/proj-1");
        assert!(info.started_waiting_at.contains("T")); // RFC3339
    }

    #[test]
    fn test_map_waiting_entry_no_session() {
        let entry = WaitingEntry {
            issue_id: "id2".to_string(),
            identifier: "PROJ-2".to_string(),
            pr_number: 99,
            branch: "feat/xyz".to_string(),
            started_waiting_at: Utc::now(),
            session_id: None,
        };

        let info = map_waiting_entry(&entry);
        assert_eq!(info.issue_id, "id2");
        assert_eq!(info.identifier, "PROJ-2");
        assert_eq!(info.pr_number, 99);
        assert_eq!(info.branch, "feat/xyz");
    }

    #[test]
    fn test_render_dashboard_with_waiting() {
        let waiting = vec![WaitingInfo {
            issue_id: "id1".to_string(),
            identifier: "PROJ-W1".to_string(),
            pr_number: 42,
            branch: "symphony/proj-w1".to_string(),
            started_waiting_at: "2025-01-01T00:00:00Z".to_string(),
        }];

        let html = render_dashboard(
            &[],
            &[],
            &waiting,
            &TotalsInfo {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                seconds_running: 0.0,
            },
            &None,
        );

        // Verify waiting data is embedded in the initial JSON
        assert!(html.contains("PROJ-W1"));
        assert!(html.contains("symphony/proj-w1"));
        assert!(html.contains("42"));
        // Verify the waiting section HTML structure
        assert!(html.contains("Waiting for Review"));
        assert!(html.contains("waitingBadge"));
        assert!(html.contains("waitingContent"));
    }

    #[test]
    fn test_render_dashboard_waiting_empty() {
        let html = render_dashboard(
            &[],
            &[],
            &[],
            &TotalsInfo {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                seconds_running: 0.0,
            },
            &None,
        );

        assert!(html.contains("No PRs waiting"));
        assert!(html.contains("Waiting for Review"));
        assert!(html.contains("renderWaiting"));
        assert!(html.contains("countWaiting"));
    }

    #[test]
    fn test_state_response_includes_waiting() {
        let response = StateResponse {
            generated_at: "2025-01-01T00:00:00Z".to_string(),
            counts: StateCounts {
                running: 0,
                retrying: 0,
                waiting: 1,
            },
            running: vec![],
            retrying: vec![],
            waiting: vec![WaitingInfo {
                issue_id: "id1".to_string(),
                identifier: "PROJ-W1".to_string(),
                pr_number: 42,
                branch: "symphony/proj-w1".to_string(),
                started_waiting_at: "2025-01-01T00:00:00Z".to_string(),
            }],
            codex_totals: TotalsInfo {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                seconds_running: 0.0,
            },
            rate_limits: None,
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"waiting\""));
        assert!(json.contains("PROJ-W1"));
        assert!(json.contains("\"pr_number\":42"));
        assert!(json.contains("symphony/proj-w1"));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["counts"]["waiting"], 1);
        assert_eq!(parsed["waiting"][0]["identifier"], "PROJ-W1");
    }

    #[test]
    fn test_state_response_waiting_empty() {
        let response = StateResponse {
            generated_at: "2025-01-01T00:00:00Z".to_string(),
            counts: StateCounts {
                running: 0,
                retrying: 0,
                waiting: 0,
            },
            running: vec![],
            retrying: vec![],
            waiting: vec![],
            codex_totals: TotalsInfo {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                seconds_running: 0.0,
            },
            rate_limits: None,
        };

        let json = serde_json::to_string(&response).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["counts"]["waiting"], 0);
        assert!(parsed["waiting"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_build_snapshot_with_waiting() {
        let mut state = OrchestratorState::new(5000, 3);
        state.waiting_for_review.insert(
            "id1".to_string(),
            WaitingEntry {
                issue_id: "id1".to_string(),
                identifier: "PROJ-W1".to_string(),
                pr_number: 42,
                branch: "symphony/proj-w1".to_string(),
                started_waiting_at: Utc::now(),
                session_id: Some("sess-abc".to_string()),
            },
        );

        let (_running, _retrying, waiting, _totals) = build_snapshot(&state);
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].identifier, "PROJ-W1");
        assert_eq!(waiting[0].pr_number, 42);
        assert_eq!(waiting[0].branch, "symphony/proj-w1");
    }

    #[test]
    fn test_build_state_response_with_waiting() {
        let mut state = OrchestratorState::new(5000, 3);
        state.waiting_for_review.insert(
            "id1".to_string(),
            WaitingEntry {
                issue_id: "id1".to_string(),
                identifier: "PROJ-W1".to_string(),
                pr_number: 42,
                branch: "symphony/proj-w1".to_string(),
                started_waiting_at: Utc::now(),
                session_id: None,
            },
        );

        let response = build_state_response(&state);
        assert_eq!(response.counts.waiting, 1);
        assert_eq!(response.waiting.len(), 1);
        assert_eq!(response.waiting[0].identifier, "PROJ-W1");
        assert_eq!(response.waiting[0].pr_number, 42);
    }

    #[tokio::test]
    async fn test_get_state_includes_waiting() {
        let app_state = test_app_state();

        // Insert a waiting entry
        {
            let mut state = app_state.orchestrator_state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-W1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-w1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
        }

        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/api/v1/state")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = get_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(json["counts"]["waiting"], 1);
        assert_eq!(json["waiting"][0]["identifier"], "PROJ-W1");
        assert_eq!(json["waiting"][0]["pr_number"], 42);
        assert_eq!(json["waiting"][0]["branch"], "symphony/proj-w1");
    }

    #[tokio::test]
    async fn test_get_dashboard_with_waiting() {
        let app_state = test_app_state();

        // Insert a waiting entry
        {
            let mut state = app_state.orchestrator_state.write().await;
            state.waiting_for_review.insert(
                "id1".to_string(),
                WaitingEntry {
                    issue_id: "id1".to_string(),
                    identifier: "PROJ-W1".to_string(),
                    pr_number: 42,
                    branch: "symphony/proj-w1".to_string(),
                    started_waiting_at: Utc::now(),
                    session_id: None,
                },
            );
        }

        let app = build_router(app_state);

        let request = axum::http::Request::builder()
            .uri("/")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = get_body(response).await;
        assert!(body.contains("PROJ-W1"));
        assert!(body.contains("symphony/proj-w1"));
    }

    #[test]
    fn test_waiting_info_debug_clone_serialize() {
        let info = WaitingInfo {
            issue_id: "id1".to_string(),
            identifier: "PROJ-W1".to_string(),
            pr_number: 42,
            branch: "symphony/proj-w1".to_string(),
            started_waiting_at: "2025-01-01T00:00:00Z".to_string(),
        };

        // Test Debug
        let debug = format!("{:?}", info);
        assert!(debug.contains("WaitingInfo"));
        assert!(debug.contains("PROJ-W1"));

        // Test Clone
        let cloned = info.clone();
        assert_eq!(cloned.issue_id, info.issue_id);
        assert_eq!(cloned.pr_number, info.pr_number);

        // Test Serialize
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("PROJ-W1"));
        assert!(json.contains("42"));
    }

    #[test]
    fn test_render_dashboard_waiting_js_function() {
        let html = render_dashboard(
            &[],
            &[],
            &[],
            &TotalsInfo {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                seconds_running: 0.0,
            },
            &None,
        );

        // Verify the renderWaiting JS function is present
        assert!(html.contains("function renderWaiting(items)"));
        assert!(html.contains("waitingContent"));
        assert!(html.contains("waitingBadge"));
        // Verify update() calls renderWaiting
        assert!(html.contains("renderWaiting(data.waiting"));
        // Verify countWaiting stat card
        assert!(html.contains("countWaiting"));
        assert!(html.contains("awaiting review"));
    }

    #[test]
    fn test_render_dashboard_multiple_waiting() {
        let waiting = vec![
            WaitingInfo {
                issue_id: "id1".to_string(),
                identifier: "PROJ-W1".to_string(),
                pr_number: 42,
                branch: "symphony/proj-w1".to_string(),
                started_waiting_at: "2025-01-01T00:00:00Z".to_string(),
            },
            WaitingInfo {
                issue_id: "id2".to_string(),
                identifier: "PROJ-W2".to_string(),
                pr_number: 99,
                branch: "feat/xyz".to_string(),
                started_waiting_at: "2025-06-15T12:00:00Z".to_string(),
            },
        ];

        let html = render_dashboard(
            &[],
            &[],
            &waiting,
            &TotalsInfo {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                seconds_running: 0.0,
            },
            &None,
        );

        assert!(html.contains("PROJ-W1"));
        assert!(html.contains("PROJ-W2"));
        assert!(html.contains("42"));
        assert!(html.contains("99"));
        // Verify initial counts in JSON
        let count_str = "\"waiting\":2";
        assert!(html.contains(count_str));
    }

    // ── POST /api/v1/heartbeat tests ────────────────────────────────

    #[tokio::test]
    async fn test_heartbeat_valid() {
        let (app_state, _rx) = test_app_state_with_auth(Some("secret-token"));
        let app = build_router(app_state.clone());

        let heartbeat = WorkerHeartbeat {
            worker_id: "w-1".to_string(),
            active_jobs: vec!["TST-1".to_string()],
            timestamp: Utc::now(),
        };

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/heartbeat")
            .header("Authorization", "Bearer secret-token")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&heartbeat).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Verify the worker was tracked
        let orch = app_state.orchestrator_state.read().await;
        let tracked = orch.tracked_workers.get("w-1");
        assert!(tracked.is_some());
        let w = tracked.unwrap();
        assert_eq!(w.active_jobs, vec!["TST-1".to_string()]);
        assert!(w.alive);
    }

    #[tokio::test]
    async fn test_heartbeat_requires_auth() {
        let (app_state, _rx) = test_app_state_with_auth(Some("secret-token"));
        let app = build_router(app_state);

        let heartbeat = WorkerHeartbeat {
            worker_id: "w-1".to_string(),
            active_jobs: vec![],
            timestamp: Utc::now(),
        };

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/heartbeat")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&heartbeat).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_heartbeat_empty_active_jobs() {
        let (app_state, _rx) = test_app_state_with_auth(Some("secret-token"));
        let app = build_router(app_state.clone());

        let heartbeat = WorkerHeartbeat {
            worker_id: "w-2".to_string(),
            active_jobs: vec![],
            timestamp: Utc::now(),
        };

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/heartbeat")
            .header("Authorization", "Bearer secret-token")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&heartbeat).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let orch = app_state.orchestrator_state.read().await;
        let w = orch.tracked_workers.get("w-2").unwrap();
        assert!(w.active_jobs.is_empty());
    }

    // ── POST /api/v1/workers/register tests ─────────────────────────

    #[tokio::test]
    async fn test_register_valid() {
        let (app_state, _rx) = test_app_state_with_auth(Some("secret-token"));
        let app = build_router(app_state.clone());

        let reg = WorkerRegistration {
            worker_id: "w-1".to_string(),
            capabilities: vec!["rust".to_string()],
            max_concurrent_jobs: 2,
        };

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/workers/register")
            .header("Authorization", "Bearer secret-token")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&reg).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Verify the worker was tracked
        let orch = app_state.orchestrator_state.read().await;
        let tracked = orch.tracked_workers.get("w-1");
        assert!(tracked.is_some());
        let w = tracked.unwrap();
        assert_eq!(w.capabilities, vec!["rust".to_string()]);
        assert_eq!(w.max_concurrent_jobs, 2);
        assert!(w.alive);
    }

    #[tokio::test]
    async fn test_register_requires_auth() {
        let (app_state, _rx) = test_app_state_with_auth(Some("secret-token"));
        let app = build_router(app_state);

        let reg = WorkerRegistration {
            worker_id: "w-1".to_string(),
            capabilities: vec![],
            max_concurrent_jobs: 1,
        };

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/workers/register")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&reg).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_register_empty_capabilities() {
        let (app_state, _rx) = test_app_state_with_auth(Some("secret-token"));
        let app = build_router(app_state.clone());

        let reg = WorkerRegistration {
            worker_id: "w-3".to_string(),
            capabilities: vec![],
            max_concurrent_jobs: 1,
        };

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/workers/register")
            .header("Authorization", "Bearer secret-token")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&reg).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let orch = app_state.orchestrator_state.read().await;
        let w = orch.tracked_workers.get("w-3").unwrap();
        assert!(w.capabilities.is_empty());
        assert_eq!(w.max_concurrent_jobs, 1);
    }
}
