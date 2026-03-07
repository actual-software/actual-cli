use crate::config::ServiceConfig;
use crate::model::{OrchestratorState, RetryEntry};
use crate::orchestrator::OrchestratorMessage;
use axum::extract::{Path, State};
use axum::http::{Method, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use chrono::Utc;
use serde::Serialize;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, RwLock};
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

    axum::serve(listener, app).await?;

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
        .map(|entry| {
            Utc::now()
                .signed_duration_since(entry.started_at)
                .num_milliseconds()
                .max(0) as f64
                / 1000.0
        })
        .sum();

    let totals = TotalsInfo {
        input_tokens: state.agent_totals.input_tokens,
        output_tokens: state.agent_totals.output_tokens,
        total_tokens: state.agent_totals.total_tokens,
        seconds_running: state.agent_totals.seconds_running + active_seconds,
    };

    (running, retrying, totals)
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
        started_at: entry.started_at.to_rfc3339(),
        last_event_at: entry.session.last_event_at.map(|t| t.to_rfc3339()),
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
    let running_entry = orch_state
        .running
        .values()
        .find(|e| e.identifier == issue_identifier);

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
                started_at: entry.started_at.to_rfc3339(),
                last_event_at: entry.session.last_event_at.map(|t| t.to_rfc3339()),
                input_tokens: entry.session.input_tokens,
                output_tokens: entry.session.output_tokens,
                total_tokens: entry.session.total_tokens,
            }),
            retry: None,
        };

        return (StatusCode::OK, axum::Json(detail)).into_response();
    }

    // Search retry entries by identifier
    let retry_entry = orch_state
        .retry_attempts
        .values()
        .find(|e| e.identifier == issue_identifier);

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

    let error = ErrorResponse {
        error: ErrorDetail {
            code: "issue_not_found".to_string(),
            message: format!("No running or retrying issue with identifier '{issue_identifier}'"),
        },
    };

    (StatusCode::NOT_FOUND, axum::Json(error)).into_response()
}

/// POST /api/v1/refresh — Trigger immediate poll.
async fn handle_refresh(State(state): State<AppState>) -> impl IntoResponse {
    let now = Utc::now().to_rfc3339();

    match state.msg_tx.try_send(OrchestratorMessage::TriggerRefresh) {
        Ok(()) => {
            let response = RefreshResponse {
                queued: true,
                coalesced: None,
                requested_at: now,
            };
            (StatusCode::ACCEPTED, axum::Json(response))
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            let response = RefreshResponse {
                queued: true,
                coalesced: Some(true),
                requested_at: now,
            };
            (StatusCode::ACCEPTED, axum::Json(response))
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            let response = RefreshResponse {
                queued: true,
                coalesced: Some(true),
                requested_at: now,
            };
            (StatusCode::ACCEPTED, axum::Json(response))
        }
    }
}

/// Fallback handler for unsupported routes/methods.
async fn handle_fallback(method: Method) -> Response {
    if method != Method::GET && method != Method::POST {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            axum::Json(ErrorResponse {
                error: ErrorDetail {
                    code: "method_not_allowed".to_string(),
                    message: format!("Method {method} is not allowed"),
                },
            }),
        )
            .into_response();
    }

    (
        StatusCode::NOT_FOUND,
        axum::Json(ErrorResponse {
            error: ErrorDetail {
                code: "not_found".to_string(),
                message: "Route not found".to_string(),
            },
        }),
    )
        .into_response()
}

// ── HTML Rendering ───────────────────────────────────────────────────

/// Render the HTML dashboard page.
pub fn render_dashboard(
    running: &[RunningInfo],
    retrying: &[RetryInfo],
    totals: &TotalsInfo,
    rate_limits: &Option<serde_json::Value>,
) -> String {
    let mut html = String::with_capacity(4096);

    html.push_str(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Symphony Dashboard</title>
<style>
body { font-family: monospace; margin: 20px; background: #1a1a2e; color: #e0e0e0; }
h1 { color: #00d4ff; }
h2 { color: #7b68ee; margin-top: 30px; }
table { border-collapse: collapse; width: 100%; margin-top: 10px; }
th, td { border: 1px solid #333; padding: 6px 10px; text-align: left; }
th { background: #16213e; color: #00d4ff; }
tr:nth-child(even) { background: #0f3460; }
tr:nth-child(odd) { background: #1a1a2e; }
.stat { display: inline-block; margin-right: 30px; }
.stat-value { font-size: 1.4em; color: #00d4ff; }
.stat-label { color: #888; }
.empty { color: #666; font-style: italic; }
pre { background: #16213e; padding: 10px; border-radius: 4px; overflow-x: auto; }
</style>
</head>
<body>
<h1>Symphony Dashboard</h1>
"#,
    );

    // Aggregate totals
    html.push_str("<div>\n");
    push_stat(
        &mut html,
        "Total Tokens",
        &format_tokens(totals.total_tokens),
    );
    push_stat(
        &mut html,
        "Input Tokens",
        &format_tokens(totals.input_tokens),
    );
    push_stat(
        &mut html,
        "Output Tokens",
        &format_tokens(totals.output_tokens),
    );
    push_stat(
        &mut html,
        "Seconds Running",
        &format!("{:.1}", totals.seconds_running),
    );
    html.push_str("</div>\n");

    // Running sessions table
    html.push_str("<h2>Running Sessions</h2>\n");
    if running.is_empty() {
        html.push_str("<p class=\"empty\">No running sessions</p>\n");
    } else {
        html.push_str(
            "<table>\n<tr><th>Identifier</th><th>State</th><th>Session</th>\
             <th>Turns</th><th>Last Event</th><th>Tokens</th><th>Started</th></tr>\n",
        );
        for entry in running {
            html.push_str("<tr>");
            push_td(&mut html, &entry.issue_identifier);
            push_td(&mut html, &entry.state);
            push_td(&mut html, entry.session_id.as_deref().unwrap_or("-"));
            push_td(&mut html, &entry.turn_count.to_string());
            push_td(&mut html, entry.last_event.as_deref().unwrap_or("-"));
            push_td(&mut html, &format_tokens(entry.total_tokens));
            push_td(&mut html, &entry.started_at);
            html.push_str("</tr>\n");
        }
        html.push_str("</table>\n");
    }

    // Retry queue table
    html.push_str("<h2>Retry Queue</h2>\n");
    if retrying.is_empty() {
        html.push_str("<p class=\"empty\">No retries pending</p>\n");
    } else {
        html.push_str(
            "<table>\n<tr><th>Identifier</th><th>Attempt</th>\
             <th>Due At (ms)</th><th>Error</th></tr>\n",
        );
        for entry in retrying {
            html.push_str("<tr>");
            push_td(&mut html, &entry.identifier);
            push_td(&mut html, &entry.attempt.to_string());
            push_td(&mut html, &entry.due_at_ms.to_string());
            push_td(&mut html, entry.error.as_deref().unwrap_or("-"));
            html.push_str("</tr>\n");
        }
        html.push_str("</table>\n");
    }

    // Rate limits
    html.push_str("<h2>Rate Limits</h2>\n");
    match rate_limits {
        Some(rl) => {
            html.push_str("<pre>");
            let formatted = serde_json::to_string_pretty(rl).unwrap_or_else(|_| "{}".to_string());
            html.push_str(&html_escape(&formatted));
            html.push_str("</pre>\n");
        }
        None => {
            html.push_str("<p class=\"empty\">No rate limit data</p>\n");
        }
    }

    html.push_str("</body>\n</html>");
    html
}

/// Push a stat display block into the HTML buffer.
fn push_stat(html: &mut String, label: &str, value: &str) {
    html.push_str("<div class=\"stat\"><span class=\"stat-value\">");
    html.push_str(&html_escape(value));
    html.push_str("</span><br><span class=\"stat-label\">");
    html.push_str(&html_escape(label));
    html.push_str("</span></div>\n");
}

/// Push a `<td>` element into the HTML buffer.
fn push_td(html: &mut String, content: &str) {
    html.push_str("<td>");
    html.push_str(&html_escape(content));
    html.push_str("</td>");
}

/// Minimal HTML escaping for display safety.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Format token counts for display.
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
        AgentConfig, CodingAgentConfig, HooksConfig, PollingConfig, ServerConfig, TrackerConfig,
        WorkspaceConfig,
    };
    use crate::model::{AgentTotals, LiveSession, OrchestratorState, RetryEntry, RunningEntry};
    use axum::body::Body;
    use http_body_util::BodyExt;
    use std::collections::HashMap;
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

        assert!(html.contains("Symphony Dashboard"));
        assert!(html.contains("No running sessions"));
        assert!(html.contains("No retries pending"));
        assert!(html.contains("No rate limit data"));
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

        assert!(html.contains("PROJ-1"));
        assert!(html.contains("sess-abc"));
        assert!(html.contains("1.5K"));
        assert!(!html.contains("No running sessions"));
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
        assert!(!html.contains("No retries pending"));
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
        assert!(!html.contains("No rate limit data"));
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

        // Should show "-" for missing session/event
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
        assert!(body.contains("Symphony Dashboard"));
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

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

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
}
