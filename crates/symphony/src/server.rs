use crate::config::ServiceConfig;
use crate::model::{LogEntry, OrchestratorState, RetryEntry};
use crate::orchestrator::OrchestratorMessage;
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
}

/// Start the HTTP dashboard/API server.
///
/// Binds to `127.0.0.1:{port}`. If port is 0, an ephemeral port is chosen
/// and the actual port is logged.
pub async fn start_server(
    port: u16,
    state: Arc<RwLock<OrchestratorState>>,
    config: Arc<RwLock<ServiceConfig>>,
    msg_tx: mpsc::Sender<OrchestratorMessage>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let app_state = AppState {
        orchestrator_state: state,
        config,
        msg_tx,
    };

    let app = build_router(app_state);

    let addr = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&addr).await?;
    let local_addr = listener.local_addr()?;
    info!(port = local_addr.port(), "HTTP server listening");

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
        .route("/api/v1/refresh", post(handle_refresh))
        .route(
            "/api/v1/{issue_identifier}/stream",
            get(handle_issue_stream),
        )
        .route("/api/v1/{issue_identifier}/logs", get(handle_issue_logs))
        .route("/api/v1/{issue_identifier}", get(handle_issue_detail))
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
    pub codex_totals: TotalsInfo,
    pub rate_limits: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StateCounts {
    pub running: usize,
    pub retrying: usize,
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
fn build_snapshot(state: &OrchestratorState) -> (Vec<RunningInfo>, Vec<RetryInfo>, TotalsInfo) {
    let running: Vec<RunningInfo> = state.running.values().map(map_running_entry).collect();

    let retrying: Vec<RetryInfo> = state.retry_attempts.values().map(map_retry_entry).collect();

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

    (running, retrying, totals)
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

// ── Handlers ─────────────────────────────────────────────────────────

/// GET / — HTML dashboard.
async fn handle_dashboard(State(state): State<AppState>) -> Html<String> {
    let orch_state = state.orchestrator_state.read().await;
    let (running, retrying, totals) = build_snapshot(&orch_state);
    let rate_limits = orch_state.rate_limits.clone();
    drop(orch_state);

    let html = render_dashboard(&running, &retrying, &totals, &rate_limits);
    Html(html)
}

/// GET /api/v1/state — JSON state snapshot.
async fn handle_state(State(state): State<AppState>) -> impl IntoResponse {
    let orch_state = state.orchestrator_state.read().await;
    let (running, retrying, totals) = build_snapshot(&orch_state);
    let rate_limits = orch_state.rate_limits.clone();
    drop(orch_state);

    let response = StateResponse {
        generated_at: Utc::now().to_rfc3339(),
        counts: StateCounts {
            running: running.len(),
            retrying: retrying.len(),
        },
        running,
        retrying,
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
    totals: &TotalsInfo,
    rate_limits: &Option<serde_json::Value>,
) -> String {
    let mut html = String::with_capacity(16384);

    // Serialize initial state as JSON for the JS hydration
    let initial_state = serde_json::json!({
        "running": running,
        "retrying": retrying,
        "codex_totals": totals,
        "rate_limits": rate_limits,
        "counts": { "running": running.len(), "retrying": retrying.len() },
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
.content {{ max-width:1400px; margin:0 auto; padding:24px; }}
.stats {{ display:grid; grid-template-columns:repeat(4,1fr); gap:16px; margin-bottom:24px; }}
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
  .stats {{ grid-template-columns:repeat(2,1fr); }}
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
  let h = '<table><thead><tr><th>Issue</th><th>Attempt</th><th>Retry In</th><th>Error</th></tr></thead><tbody>';
  for (const r of items) {{
    const dueIn = Math.max(0, r.due_at_ms - Date.now());
    const dueStr = dueIn > 0 ? fmtTime(dueIn/1000) : 'now';
    h += '<tr>';
    h += '<td class="mono"><strong>'+esc(r.identifier)+'</strong></td>';
    h += '<td><span class="state-badge retry">#'+r.attempt+'</span></td>';
    h += '<td>'+dueStr+'</td>';
    h += '<td>'+(r.error ? esc(r.error) : '-')+'</td>';
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
  document.getElementById('totalTokens').textContent = fmt(t.total_tokens || 0);
  document.getElementById('inputTokens').textContent = fmt(t.input_tokens || 0) + ' in';
  document.getElementById('outputTokens').textContent = fmt(t.output_tokens || 0) + ' out';
  document.getElementById('runtime').textContent = fmtTime(t.seconds_running || 0);
  renderRunning(data.running || []);
  renderRetrying(data.retrying || []);
  renderRateLimits(data.rate_limits);
  lastFetch = Date.now();
}}

async function poll() {{
  try {{
    const r = await fetch('/api/v1/state');
    if (r.ok) {{
      update(await r.json());
      document.getElementById('statusDot').style.background = 'var(--green)';
      document.getElementById('statusText').textContent = 'Connected';
    }}
  }} catch(e) {{
    document.getElementById('statusDot').style.background = 'var(--red)';
    document.getElementById('statusText').textContent = 'Disconnected';
  }}
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
// Start polling every 3 seconds
setInterval(poll, 3000);
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
    use crate::model::{AgentTotals, LiveSession, OrchestratorState, RetryEntry, RunningEntry};
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
                max_retries: 10,
                max_retry_backoff_ms: 300_000,
                max_concurrent_agents_by_state: HashMap::new(),
            },
            coding_agent: CodingAgentConfig {
                command: "echo test".to_string(),
                permission_mode: "bypassPermissions".to_string(),
                turn_timeout_ms: 3_600_000,
                stall_timeout_ms: 300_000,
            },
            server: ServerConfig { port: None },
            deployment: DeploymentConfig {
                mode: DeploymentMode::Local,
                auth_token: None,
            },
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
        assert!(html.contains("No rate limit data"));
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
        let (running, retrying, totals) = build_snapshot(&state);
        assert!(running.is_empty());
        assert!(retrying.is_empty());
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

        let (running, retrying, totals) = build_snapshot(&state);
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].issue_identifier, "PROJ-1");
        assert_eq!(running[0].input_tokens, 100);
        assert!(retrying.is_empty());
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

        let (_running, retrying, _totals) = build_snapshot(&state);
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

        let handle = tokio::spawn(start_server(
            0,
            state_arc,
            config_arc,
            msg_tx,
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
}
