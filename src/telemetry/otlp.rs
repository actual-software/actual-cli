//! ADR telemetry as OpenTelemetry log events over OTLP/HTTP with JSON
//! encoding, starting with an ADR entering an agent's context.
//!
//! # Design
//!
//! **Events, not spans.** Each event is one `LogRecord` with `eventName` set.
//! An ADR entering context happens at a point in time and has no duration,
//! which is what the semantic conventions reserve events for, and an event
//! needs no enclosing span.
//!
//! **Hand-built OTLP/JSON.** OTLP is a documented HTTP and JSON protocol, so
//! the request is built with serde and sent on the existing HTTP client. The
//! OpenTelemetry SDK would add a background exporter to a process that lives
//! for milliseconds. A test decodes the request into the official generated
//! types to hold the encoding to the protocol.
//!
//! **Off until configured.** Nothing is built unless an endpoint or a local
//! file is set, and only this CLI's own settings are read, never
//! `OTEL_EXPORTER_OTLP_*`, which a shell or CI job may set for other tools.
//!
//! **The hook spools, a detached uploader sends.** The hook appends records to
//! [`OTLP_LOGS`] and spawns `__telemetry-flush`, whose [`flush`] takes them
//! out a request at a time, writes each request to the file sink, and POSTs
//! it. Nothing here prints: every outcome is logged at debug and dropped.

use std::future::Future;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::config::types::{validate_otlp_file, validate_otlp_logs_endpoint, Config};
use crate::telemetry::identity::IdentityEnvelope;
use crate::telemetry::opt_out;
use crate::telemetry::spool::OTLP_LOGS;

/// The event sent when an ADR first enters a session's context.
pub(crate) const CONTEXT_ADDED: &str = "ai.actual.adr.context_added";

/// Overrides `telemetry.otlp_logs_endpoint`.
const ENDPOINT_ENV: &str = "ACTUAL_OTLP_LOGS_ENDPOINT";

/// Overrides `telemetry.otlp_file`.
const FILE_ENV: &str = "ACTUAL_OTLP_FILE";

/// Records per request.
pub(crate) const MAX_RECORDS_PER_REQUEST: usize = 100;

/// How long one request may take, retries included: the OTLP exporter's
/// default export timeout.
pub(crate) const SEND_BUDGET: Duration = Duration::from_secs(10);

/// The first retry's nominal wait, doubled for each retry after it.
const BACKOFF_BASE: Duration = Duration::from_millis(500);

/// `SEVERITY_NUMBER_INFO`.
const SEVERITY_INFO: i32 = 9;

/// The service, SDK and instrumentation scope name. Never `opentelemetry`,
/// which is reserved for OpenTelemetry's own SDKs.
const NAME: &str = env!("CARGO_PKG_NAME");

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where this run's records go. At least one is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outputs {
    /// The OTLP/HTTP logs URL, used as-is.
    pub(crate) endpoint: Option<String>,
    /// The file each request body is appended to, one per line.
    pub(crate) file: Option<PathBuf>,
}

impl Outputs {
    /// This run's outputs, or `None` when telemetry is opted out or nothing
    /// usable is set. The opt-out is checked first. Each setting is its env
    /// override when that is non-empty, else the config value, and a value
    /// that fails validation counts as unset.
    pub(crate) fn resolve(config: &Config) -> Option<Self> {
        if opt_out::is_disabled(config) {
            return None;
        }
        let telemetry = config.telemetry.as_ref();
        let endpoint = setting(
            ENDPOINT_ENV,
            telemetry.and_then(|t| t.otlp_logs_endpoint.as_deref()),
        )
        .filter(|url| validate_otlp_logs_endpoint(url).is_ok());
        let file = setting(FILE_ENV, telemetry.and_then(|t| t.otlp_file.as_deref()))
            .filter(|path| validate_otlp_file(path).is_ok())
            .map(PathBuf::from);
        (endpoint.is_some() || file.is_some()).then_some(Self { endpoint, file })
    }
}

fn setting(env: &str, configured: Option<&str>) -> Option<String> {
    std::env::var(env)
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            configured
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
}

/// One `LogRecord` in OTLP/JSON: lowerCamelCase keys, 64-bit integers as
/// decimal strings, ids as hex. An absent field reads as its default.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LogRecord {
    time_unix_nano: String,
    observed_time_unix_nano: String,
    severity_number: i32,
    event_name: &'static str,
    attributes: Vec<KeyValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    span_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flags: Option<u32>,
}

#[derive(Debug, Serialize)]
struct KeyValue {
    key: &'static str,
    value: AnyValue,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
enum AnyValue {
    StringValue(String),
    ArrayValue { values: Vec<AnyValue> },
}

fn text(key: &'static str, value: Option<&str>) -> KeyValue {
    KeyValue {
        key,
        value: AnyValue::StringValue(value.unwrap_or_default().to_string()),
    }
}

/// What the records from one run share: who sends them, when, and the trace
/// they belong to.
pub(crate) struct Origin {
    install_id: String,
    identity: IdentityEnvelope,
    time_unix_nano: String,
    trace: Option<TraceContext>,
}

impl Origin {
    /// The origin of a run about to emit: the install id, the hashed identity
    /// envelope (from local state only), the current time, and the trace in
    /// `TRACEPARENT`, if any.
    pub(crate) fn resolve(config: &Config, repo_url: &str) -> Self {
        Self {
            install_id: crate::telemetry::plan_governance::distinct_id(),
            identity: IdentityEnvelope::resolve(config, repo_url),
            time_unix_nano: unix_nanos(SystemTime::now()),
            trace: std::env::var("TRACEPARENT")
                .ok()
                .and_then(|value| TraceContext::parse(&value)),
        }
    }

    /// The [`CONTEXT_ADDED`] record for one logical ADR. Every attribute is
    /// present, an empty string when unknown, so a logged-out record carries
    /// the same keys as a logged-in one.
    pub(crate) fn context_added(
        &self,
        session_id: &str,
        agent_id: Option<&str>,
        candidate_group_id: Option<&str>,
        adr_ids: &[String],
    ) -> LogRecord {
        let uid = crate::telemetry::plan_governance::new_insert_id();
        let ids = adr_ids
            .iter()
            .map(|id| AnyValue::StringValue(id.clone()))
            .collect();
        let identity = &self.identity;
        LogRecord {
            time_unix_nano: self.time_unix_nano.clone(),
            observed_time_unix_nano: self.time_unix_nano.clone(),
            severity_number: SEVERITY_INFO,
            event_name: CONTEXT_ADDED,
            attributes: vec![
                text("log.record.uid", Some(uid.as_str())),
                text("session.id", Some(session_id)),
                text("ai.actual.agent.id", agent_id),
                text("ai.actual.adr.candidate_group_id", candidate_group_id),
                KeyValue {
                    key: "ai.actual.adr.ids",
                    value: AnyValue::ArrayValue { values: ids },
                },
                text("ai.actual.install.id", Some(self.install_id.as_str())),
                text("ai.actual.user.id_hash", identity.user_id_hash.as_deref()),
                text("ai.actual.org.id_hash", identity.org_id_hash.as_deref()),
                text("ai.actual.repo.id_hash", identity.repo_id_hash.as_deref()),
                text("ai.actual.org.fallback_id", identity.org_id.as_deref()),
            ],
            trace_id: self.trace.as_ref().map(|t| t.trace_id.clone()),
            span_id: self.trace.as_ref().map(|t| t.span_id.clone()),
            flags: self.trace.as_ref().map(|t| t.flags),
        }
    }
}

fn unix_nanos(time: SystemTime) -> String {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .to_string()
}

/// The W3C trace context a record links to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TraceContext {
    trace_id: String,
    span_id: String,
    flags: u32,
}

impl TraceContext {
    /// Parse a W3C `traceparent` value. A malformed one is `None`, ignored
    /// whole rather than half-used. Fields past the flags are allowed only
    /// for a version after `00`, as the recommendation asks.
    fn parse(value: &str) -> Option<Self> {
        let fields: Vec<&str> = value.trim().split('-').collect();
        let [version, trace_id, span_id, flags, rest @ ..] = fields.as_slice() else {
            return None;
        };
        let valid = is_lower_hex(version, 2)
            && *version != "ff"
            && (rest.is_empty() || *version != "00")
            && is_lower_hex(trace_id, 32)
            && !is_all_zero(trace_id)
            && is_lower_hex(span_id, 16)
            && !is_all_zero(span_id)
            && is_lower_hex(flags, 2);
        valid.then(|| Self {
            trace_id: trace_id.to_string(),
            span_id: span_id.to_string(),
            flags: u32::from_str_radix(flags, 16).unwrap_or_default(),
        })
    }
}

fn is_lower_hex(field: &str, len: usize) -> bool {
    field.len() == len
        && field
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_all_zero(field: &str) -> bool {
    field.bytes().all(|b| b == b'0')
}

/// Append `records` to the OTLP spool for the uploader, best-effort: records
/// the spool turns away are logged at debug and dropped.
pub(crate) fn spool(records: &[LogRecord]) {
    let lines: Vec<String> = records
        .iter()
        .filter_map(|record| serde_json::to_string(record).ok())
        .collect();
    if let Err(e) = OTLP_LOGS.append_lines(&lines) {
        tracing::debug!("otlp logs: {} records not spooled: {e}", lines.len());
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportLogsServiceRequest {
    resource_logs: [ResourceLogs; 1],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResourceLogs {
    resource: Resource,
    scope_logs: [ScopeLogs; 1],
}

#[derive(Serialize)]
struct Resource {
    attributes: Vec<KeyValue>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScopeLogs {
    scope: InstrumentationScope,
    log_records: Vec<serde_json::Value>,
}

#[derive(Serialize)]
struct InstrumentationScope {
    name: &'static str,
    version: &'static str,
}

/// The `ExportLogsServiceRequest` body for spooled record `lines`. A line that
/// is not a JSON object is dropped, and with no record left there is no body,
/// since a sender should not send an empty envelope.
pub(crate) fn request_body(lines: &[String]) -> Option<String> {
    let records: Vec<serde_json::Value> = lines
        .iter()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(serde_json::Value::is_object)
        .collect();
    if records.is_empty() {
        return None;
    }
    let request = ExportLogsServiceRequest {
        resource_logs: [ResourceLogs {
            resource: Resource {
                attributes: vec![
                    text("service.name", Some(NAME)),
                    text("service.version", Some(VERSION)),
                    text("telemetry.sdk.name", Some(NAME)),
                    text("telemetry.sdk.language", Some("rust")),
                    text("telemetry.sdk.version", Some(VERSION)),
                ],
            },
            scope_logs: [ScopeLogs {
                scope: InstrumentationScope {
                    name: NAME,
                    version: VERSION,
                },
                log_records: records,
            }],
        }],
    };
    serde_json::to_string(&request).ok()
}

/// Send what the OTLP spool holds, a request at a time, then return.
///
/// Each request's records are taken out of the spool before it is sent, so
/// they are gone whatever the outcome. The body is appended to the file sink
/// when one is set and POSTed when an endpoint is. An opted-out run, or one
/// with no usable output, leaves the spool alone.
pub(crate) async fn flush(config: &Config) {
    let Some(outputs) = Outputs::resolve(config) else {
        return;
    };
    let client = outputs.endpoint.as_deref().and_then(client_for);
    loop {
        let lines = OTLP_LOGS.take(MAX_RECORDS_PER_REQUEST);
        if lines.is_empty() {
            return;
        }
        let Some(body) = request_body(&lines) else {
            continue;
        };
        if let Some(file) = &outputs.file {
            let _ = crate::config::paths::append_secure(file, format!("{body}\n").as_bytes());
        }
        if let (Some(client), Some(endpoint)) = (&client, &outputs.endpoint) {
            let sent = post(client, endpoint, body, SEND_BUDGET, tokio::time::sleep).await;
            tracing::debug!("otlp logs: {} records: {sent:?}", lines.len());
        }
    }
}

/// A client with the API client's User-Agent that refuses plain HTTP unless
/// `endpoint` is loopback.
fn client_for(endpoint: &str) -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(crate::api::client::USER_AGENT_VALUE)
        .https_only(!crate::net::is_loopback_http_url(endpoint))
        .build()
        .ok()
}

/// How one request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sent {
    /// Any 2xx, partial success included.
    Accepted,
    /// A status OTLP says not to retry.
    Rejected(u16),
    /// The budget ran out first.
    Dropped,
}

/// POST `body` to `endpoint`, retrying what OTLP/HTTP says to retry for no
/// longer than `budget`.
///
/// 429, 502, 503 and 504, and a connection refused or dropped before an
/// answer, are retried after [`retry_delay`]. Every other status is final, as
/// is a 2xx carrying a partial success and a request that cannot be built. `sleep` does the waiting, so a test can
/// record the waits instead of sitting through them; the budget counts each
/// wait whether or not it was slept.
pub(crate) async fn post<S, F>(
    client: &reqwest::Client,
    endpoint: &str,
    body: String,
    budget: Duration,
    mut sleep: S,
) -> Sent
where
    S: FnMut(Duration) -> F,
    F: Future<Output = ()>,
{
    let mut spent = Duration::ZERO;
    let mut retries = 0;
    loop {
        let started = Instant::now();
        let response = client
            .post(endpoint)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .timeout(budget.saturating_sub(spent))
            .body(body.clone())
            .send()
            .await;
        spent += started.elapsed();
        let retry_after = match response {
            Ok(response) if response.status().is_success() => return Sent::Accepted,
            Ok(response) if is_retryable(response.status()) => {
                crate::runner::http_retry::parse_retry_after(response.headers())
            }
            Ok(response) => return Sent::Rejected(response.status().as_u16()),
            Err(error) if error.is_timeout() || error.is_builder() => return Sent::Dropped,
            Err(_) => None,
        };
        retries += 1;
        let Some(wait) = retry_delay(retries, retry_after, budget.saturating_sub(spent), jitter())
        else {
            return Sent::Dropped;
        };
        spent += wait;
        sleep(wait).await;
    }
}

fn is_retryable(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 502 | 503 | 504)
}

/// The wait before retry number `retry`, counting from 1: the server's
/// `Retry-After` when it sent one, else exponential backoff from
/// [`BACKOFF_BASE`], jittered to a point between half and all of the nominal
/// wait. `None` when the wait would leave no time to try again.
fn retry_delay(
    retry: u32,
    retry_after: Option<u64>,
    remaining: Duration,
    jitter: f64,
) -> Option<Duration> {
    let wait = match retry_after {
        Some(seconds) => Duration::from_secs(seconds),
        None => BACKOFF_BASE
            .saturating_mul(1 << retry.saturating_sub(1).min(16))
            .mul_f64(0.5 + jitter / 2.0),
    };
    (wait < remaining).then_some(wait)
}

/// A uniform draw from [0, 1), from the low 53 bits of a v4 UUID: random bits
/// from the OS RNG `uuid` already uses, clear of the version and variant bits.
fn jitter() -> f64 {
    let bits = uuid::Uuid::new_v4().as_u128() & ((1 << 53) - 1);
    bits as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    // These async tests hold `ENV_MUTEX` (a std Mutex) across `.await` points on
    // purpose: it is an env-serialization latch held across await points by design,
    // serializing tests that mutate process-global env vars so they never race.
    #![allow(clippy::await_holding_lock)]
    use super::*;
    use crate::config::types::TelemetryConfig;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use serde_json::Value;
    use tempfile::tempdir;

    /// Every env var these tests read, cleared so the machine's own shell
    /// cannot leak into a test.
    fn clear_env() -> Vec<EnvGuard> {
        [
            ENDPOINT_ENV,
            FILE_ENV,
            "ACTUAL_NO_TELEMETRY",
            "TRACEPARENT",
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
        ]
        .into_iter()
        .map(EnvGuard::remove)
        .collect()
    }

    fn config(endpoint: Option<&str>, file: Option<&str>) -> Config {
        Config {
            telemetry: Some(TelemetryConfig {
                otlp_logs_endpoint: endpoint.map(str::to_string),
                otlp_file: file.map(str::to_string),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn origin(identity: IdentityEnvelope, trace: Option<TraceContext>) -> Origin {
        Origin {
            install_id: "install-1".to_string(),
            identity,
            time_unix_nano: "1792446915838119000".to_string(),
            trace,
        }
    }

    fn logged_in() -> IdentityEnvelope {
        IdentityEnvelope {
            user_id_hash: Some("a".repeat(64)),
            org_id_hash: Some("b".repeat(64)),
            repo_id_hash: Some("c".repeat(64)),
            org_id: None,
            datetime_utc: String::new(),
        }
    }

    fn ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn record_json(record: &LogRecord) -> Value {
        serde_json::to_value(record).unwrap()
    }

    /// The record's attributes as `key -> value`, in order.
    fn attributes(record: &Value) -> Vec<(String, Value)> {
        record["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|kv| (kv["key"].as_str().unwrap().to_string(), kv["value"].clone()))
            .collect()
    }

    fn lines(records: &[LogRecord]) -> Vec<String> {
        records
            .iter()
            .map(|record| serde_json::to_string(record).unwrap())
            .collect()
    }

    // ── settings ────────────────────────────────────────────────────────

    #[test]
    fn test_nothing_set_resolves_to_no_outputs() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        assert_eq!(Outputs::resolve(&Config::default()), None);
        assert_eq!(Outputs::resolve(&config(None, None)), None);
        assert_eq!(Outputs::resolve(&config(Some(""), Some(""))), None);
    }

    #[test]
    fn test_configured_outputs_resolve() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let outputs = Outputs::resolve(&config(
            Some("https://collector.example.com/v1/logs"),
            Some("/tmp/otlp.jsonl"),
        ))
        .unwrap();
        assert_eq!(
            outputs.endpoint.as_deref(),
            Some("https://collector.example.com/v1/logs")
        );
        assert_eq!(outputs.file, Some(PathBuf::from("/tmp/otlp.jsonl")));
    }

    /// The env overrides win over config, and an empty one counts as unset.
    #[test]
    fn test_env_overrides_win_and_empty_ones_fall_back() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let configured = config(Some("https://configured.example.com/v1/logs"), None);

        let _endpoint = EnvGuard::set(ENDPOINT_ENV, "http://127.0.0.1:4318/v1/logs");
        let _file = EnvGuard::set(FILE_ENV, "/tmp/from-env.jsonl");
        let outputs = Outputs::resolve(&configured).unwrap();
        assert_eq!(
            outputs.endpoint.as_deref(),
            Some("http://127.0.0.1:4318/v1/logs")
        );
        assert_eq!(outputs.file, Some(PathBuf::from("/tmp/from-env.jsonl")));

        let _endpoint = EnvGuard::set(ENDPOINT_ENV, "");
        let _file = EnvGuard::set(FILE_ENV, "");
        let outputs = Outputs::resolve(&configured).unwrap();
        assert_eq!(
            outputs.endpoint.as_deref(),
            Some("https://configured.example.com/v1/logs")
        );
        assert_eq!(outputs.file, None);
    }

    /// A value that `config set` would refuse, reaching the run through a
    /// hand-edited config or an env var, sends nothing to that output.
    #[test]
    fn test_invalid_values_count_as_unset() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        assert_eq!(
            Outputs::resolve(&config(
                Some("http://collector.example.com/v1/logs"),
                Some("relative.jsonl")
            )),
            None
        );
        let outputs = Outputs::resolve(&config(
            Some("http://collector.example.com/v1/logs"),
            Some("/tmp/otlp.jsonl"),
        ))
        .unwrap();
        assert_eq!(outputs.endpoint, None);
        assert_eq!(outputs.file, Some(PathBuf::from("/tmp/otlp.jsonl")));
    }

    #[test]
    fn test_every_opt_out_resolves_to_no_outputs() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let mut opted_out = config(Some("https://c.example.com/v1/logs"), None);
        opted_out.telemetry.as_mut().unwrap().enabled = Some(false);
        assert_eq!(Outputs::resolve(&opted_out), None);

        let _no = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
        assert_eq!(
            Outputs::resolve(&config(Some("https://c.example.com/v1/logs"), None)),
            None
        );
    }

    /// The standard exporter variables configure other tools, never this one.
    #[test]
    fn test_otel_exporter_variables_are_ignored() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let _a = EnvGuard::set("OTEL_EXPORTER_OTLP_ENDPOINT", "https://other.example.com");
        let _b = EnvGuard::set(
            "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
            "https://other.example.com/v1/logs",
        );
        assert_eq!(Outputs::resolve(&Config::default()), None);
    }

    // ── the record ──────────────────────────────────────────────────────

    #[test]
    fn test_context_added_is_an_info_event_with_no_body() {
        let record = record_json(&origin(logged_in(), None).context_added(
            "session-1",
            None,
            Some("grp-1"),
            &ids(&["adr-1", "adr-2"]),
        ));

        assert_eq!(record["eventName"], CONTEXT_ADDED);
        assert_eq!(record["severityNumber"], 9);
        assert_eq!(record["timeUnixNano"], "1792446915838119000");
        assert_eq!(record["observedTimeUnixNano"], record["timeUnixNano"]);
        for absent in ["body", "severityText", "traceId", "spanId", "flags"] {
            assert!(record.get(absent).is_none(), "{absent} must be absent");
        }
    }

    #[test]
    fn test_context_added_carries_every_attribute_in_order() {
        let record = record_json(&origin(logged_in(), None).context_added(
            "session-1",
            Some("agent-7"),
            Some("grp-1"),
            &ids(&["adr-1", "adr-2"]),
        ));
        let attributes = attributes(&record);

        let keys: Vec<&str> = attributes.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "log.record.uid",
                "session.id",
                "ai.actual.agent.id",
                "ai.actual.adr.candidate_group_id",
                "ai.actual.adr.ids",
                "ai.actual.install.id",
                "ai.actual.user.id_hash",
                "ai.actual.org.id_hash",
                "ai.actual.repo.id_hash",
                "ai.actual.org.fallback_id",
            ]
        );
        let value = |key: &str| {
            attributes
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert!(
            uuid::Uuid::parse_str(value("log.record.uid")["stringValue"].as_str().unwrap()).is_ok()
        );
        assert_eq!(value("session.id")["stringValue"], "session-1");
        assert_eq!(value("ai.actual.agent.id")["stringValue"], "agent-7");
        assert_eq!(
            value("ai.actual.adr.candidate_group_id")["stringValue"],
            "grp-1"
        );
        assert_eq!(
            value("ai.actual.adr.ids"),
            serde_json::json!({"arrayValue": {"values": [{"stringValue": "adr-1"}, {"stringValue": "adr-2"}]}})
        );
        assert_eq!(value("ai.actual.install.id")["stringValue"], "install-1");
        assert_eq!(
            value("ai.actual.user.id_hash")["stringValue"],
            "a".repeat(64)
        );
        assert_eq!(
            value("ai.actual.org.id_hash")["stringValue"],
            "b".repeat(64)
        );
        assert_eq!(
            value("ai.actual.repo.id_hash")["stringValue"],
            "c".repeat(64)
        );
        assert_eq!(value("ai.actual.org.fallback_id")["stringValue"], "");
    }

    /// Logged out, the record has the same keys, with empty strings where a
    /// logged-in one has hashes.
    #[test]
    fn test_a_logged_out_record_has_the_same_keys_with_empty_values() {
        let logged_out = IdentityEnvelope {
            org_id: Some("fallback-org".to_string()),
            ..Default::default()
        };
        let a =
            record_json(&origin(logged_out, None).context_added("s", None, None, &ids(&["adr-9"])));
        let b = record_json(&origin(logged_in(), None).context_added(
            "s",
            None,
            None,
            &ids(&["adr-9"]),
        ));

        let keys = |record: &Value| -> Vec<String> {
            attributes(record).into_iter().map(|(key, _)| key).collect()
        };
        assert_eq!(keys(&a), keys(&b));
        let out = attributes(&a);
        for (key, expected) in [
            ("ai.actual.agent.id", ""),
            ("ai.actual.adr.candidate_group_id", ""),
            ("ai.actual.user.id_hash", ""),
            ("ai.actual.org.id_hash", ""),
            ("ai.actual.repo.id_hash", ""),
            ("ai.actual.org.fallback_id", "fallback-org"),
        ] {
            let value = &out.iter().find(|(k, _)| k == key).unwrap().1;
            assert_eq!(value["stringValue"], expected, "{key}");
        }
    }

    #[test]
    fn test_each_record_gets_its_own_uid() {
        let origin = origin(logged_in(), None);
        let uid = |record: &LogRecord| attributes(&record_json(record))[0].1.clone();
        let a = origin.context_added("s", None, Some("g1"), &ids(&["x"]));
        let b = origin.context_added("s", None, Some("g1"), &ids(&["x"]));
        assert_ne!(uid(&a), uid(&b));
    }

    #[test]
    fn test_a_trace_context_links_the_record() {
        let trace = TraceContext::parse("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01");
        let record =
            record_json(&origin(logged_in(), trace).context_added("s", None, None, &ids(&["x"])));
        assert_eq!(record["traceId"], "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(record["spanId"], "00f067aa0ba902b7");
        assert_eq!(record["flags"], 1);
    }

    #[test]
    fn test_origin_resolve_reads_traceparent_and_the_install_id() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _trace = EnvGuard::set(
            "TRACEPARENT",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00",
        );

        let origin = Origin::resolve(&Config::default(), "");

        assert_eq!(
            origin.install_id,
            crate::telemetry::plan_governance::distinct_id()
        );
        assert_eq!(origin.trace.unwrap().flags, 0);
        assert!(origin.time_unix_nano.parse::<u128>().unwrap() > 0);
    }

    #[test]
    fn test_traceparent_parsing() {
        let valid = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        assert_eq!(
            TraceContext::parse(valid),
            Some(TraceContext {
                trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".to_string(),
                span_id: "00f067aa0ba902b7".to_string(),
                flags: 1,
            })
        );
        assert!(TraceContext::parse(&format!("  {valid}\n")).is_some());
        // A later version may append fields; version 00 may not.
        assert!(TraceContext::parse(&format!("{}-extra", valid.replacen("00", "01", 1))).is_some());
        for malformed in [
            "",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7",
            &format!("{valid}-extra"),
            "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "0-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
            "00-4bf92f3577b34da6a3ce929d0e0e473-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-1",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-0g",
        ] {
            assert_eq!(TraceContext::parse(malformed), None, "{malformed:?}");
        }
    }

    #[test]
    fn test_unix_nanos_counts_from_the_epoch() {
        assert_eq!(unix_nanos(UNIX_EPOCH + Duration::from_nanos(42)), "42");
        assert_eq!(unix_nanos(UNIX_EPOCH - Duration::from_secs(1)), "0");
    }

    // ── the request ─────────────────────────────────────────────────────

    fn sample_records() -> Vec<LogRecord> {
        let trace = TraceContext::parse("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01");
        let origin = origin(logged_in(), trace);
        vec![
            origin.context_added("session-1", None, Some("grp-1"), &ids(&["adr-1", "adr-2"])),
            origin.context_added("session-1", Some("agent-2"), None, &ids(&["adr-3"])),
        ]
    }

    #[test]
    fn test_request_body_wraps_records_in_the_resource_and_scope() {
        let body: Value =
            serde_json::from_str(&request_body(&lines(&sample_records())).unwrap()).unwrap();

        let resource_logs = &body["resourceLogs"][0];
        let resource: Vec<(String, Value)> = resource_logs["resource"]["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|kv| {
                (
                    kv["key"].as_str().unwrap().to_string(),
                    kv["value"]["stringValue"].clone(),
                )
            })
            .collect();
        assert_eq!(
            resource,
            [
                ("service.name".to_string(), Value::from("actual-cli")),
                ("service.version".to_string(), Value::from(VERSION)),
                ("telemetry.sdk.name".to_string(), Value::from("actual-cli")),
                ("telemetry.sdk.language".to_string(), Value::from("rust")),
                ("telemetry.sdk.version".to_string(), Value::from(VERSION)),
            ]
        );
        let scope_logs = &resource_logs["scopeLogs"][0];
        assert_eq!(
            scope_logs["scope"],
            serde_json::json!({"name": "actual-cli", "version": VERSION})
        );
        assert_eq!(scope_logs["logRecords"].as_array().unwrap().len(), 2);
    }

    /// A damaged spool line is dropped, and a request with nothing left in it
    /// is never built.
    #[test]
    fn test_request_body_drops_bad_lines_and_never_builds_an_empty_one() {
        let mut spooled = lines(&sample_records());
        spooled.push("not json".to_string());
        spooled.push("[1, 2]".to_string());
        let body: Value = serde_json::from_str(&request_body(&spooled).unwrap()).unwrap();
        assert_eq!(
            body["resourceLogs"][0]["scopeLogs"][0]["logRecords"]
                .as_array()
                .unwrap()
                .len(),
            2
        );

        assert_eq!(request_body(&[]), None);
        assert_eq!(request_body(&["not json".to_string()]), None);
    }

    /// `official` with each field the CLI omitted removed, when it holds its
    /// proto3 default, so what is left can be compared with what was sent.
    fn without_omitted_defaults(official: &Value, sent: &Value) -> Value {
        match (official, sent) {
            (Value::Object(official), Value::Object(sent)) => Value::Object(
                official
                    .iter()
                    .filter(|(key, value)| sent.contains_key(*key) || !is_proto_default(value))
                    .map(|(key, value)| {
                        let pruned = sent.get(key).map_or_else(
                            || value.clone(),
                            |sent| without_omitted_defaults(value, sent),
                        );
                        (key.clone(), pruned)
                    })
                    .collect(),
            ),
            (Value::Array(official), Value::Array(sent)) if official.len() == sent.len() => {
                Value::Array(
                    official
                        .iter()
                        .zip(sent)
                        .map(|(official, sent)| without_omitted_defaults(official, sent))
                        .collect(),
                )
            }
            _ => official.clone(),
        }
    }

    /// The proto3 defaults the official encoding writes for the fields the CLI
    /// leaves out: null messages, zero numbers, empty strings and lists.
    fn is_proto_default(value: &Value) -> bool {
        [
            Value::Null,
            Value::from(0),
            Value::from(""),
            Value::Array(Vec::new()),
        ]
        .contains(value)
    }

    /// Decode `body` with the official generated types and re-encode it, with
    /// the defaults the CLI leaves out removed again.
    fn official_round_trip(body: &str) -> Value {
        let decoded: opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest =
            serde_json::from_str(body).expect("the official types decode the request");
        let official = serde_json::to_value(&decoded).unwrap();
        without_omitted_defaults(&official, &serde_json::from_str(body).unwrap())
    }

    /// The request decodes into opentelemetry-proto's `ExportLogsServiceRequest`
    /// and re-encodes to the same JSON. A receiver ignores a key it does not
    /// know, so decoding alone could not catch a misspelled one; the
    /// comparison can.
    #[test]
    fn test_the_request_round_trips_through_the_official_types() {
        let body = request_body(&lines(&sample_records())).unwrap();
        let sent: Value = serde_json::from_str(&body).unwrap();

        assert_eq!(official_round_trip(&body), sent);
        let decoded: opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest =
            serde_json::from_str(&body).unwrap();
        let record = &decoded.resource_logs[0].scope_logs[0].log_records[0];
        assert_eq!(record.event_name, CONTEXT_ADDED);
        assert_eq!(record.time_unix_nano, 1_792_446_915_838_119_000);
        assert_eq!(record.trace_id.len(), 16);
    }

    /// The negative control: one renamed key, and the comparison fails.
    #[test]
    fn test_the_round_trip_catches_a_renamed_key() {
        let body = request_body(&lines(&sample_records())).unwrap();
        for (key, renamed) in [
            ("\"eventName\"", "\"event_name\""),
            ("\"timeUnixNano\"", "\"timeUnixNanos\""),
            ("\"logRecords\"", "\"logRecord\""),
        ] {
            let mangled = body.replacen(key, renamed, 1);
            let sent: Value = serde_json::from_str(&mangled).unwrap();
            assert_ne!(official_round_trip(&mangled), sent, "{key} -> {renamed}");
        }
    }

    // ── spooling ────────────────────────────────────────────────────────

    #[test]
    fn test_spool_appends_records_for_the_uploader() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());

        spool(&sample_records());

        let taken = OTLP_LOGS.take(MAX_RECORDS_PER_REQUEST);
        assert_eq!(taken.len(), 2);
        assert!(taken[0].contains(CONTEXT_ADDED));
        assert!(OTLP_LOGS.take(MAX_RECORDS_PER_REQUEST).is_empty());
    }

    /// A spool that refuses the records drops them without failing anything.
    #[test]
    fn test_spool_drops_records_the_spool_turns_away() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        std::fs::create_dir_all(home.path().join("otlp-logs-spool.lock")).unwrap();

        spool(&sample_records());

        assert!(!home.path().join("otlp-logs-spool.jsonl").exists());
    }

    // ── sending ─────────────────────────────────────────────────────────

    fn loopback_client() -> reqwest::Client {
        client_for("http://127.0.0.1:1").unwrap()
    }

    /// Records the waits `post` asks for instead of sleeping through them.
    async fn post_recording(endpoint: &str, budget: Duration) -> (Sent, Vec<Duration>) {
        post_recording_with(&loopback_client(), endpoint, budget).await
    }

    async fn post_recording_with(
        client: &reqwest::Client,
        endpoint: &str,
        budget: Duration,
    ) -> (Sent, Vec<Duration>) {
        let mut waits = Vec::new();
        let sent = post(client, endpoint, "{}".to_string(), budget, |wait| {
            waits.push(wait);
            std::future::ready(())
        })
        .await;
        (sent, waits)
    }

    #[tokio::test]
    async fn test_post_sends_json_with_the_cli_user_agent() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/logs")
            .match_header("content-type", "application/json")
            .match_header("user-agent", crate::api::client::USER_AGENT_VALUE)
            .match_body("{}")
            .with_status(200)
            .expect(1)
            .create_async()
            .await;

        let (sent, waits) = post_recording(&format!("{}/v1/logs", server.url()), SEND_BUDGET).await;

        assert_eq!(sent, Sent::Accepted);
        assert!(waits.is_empty());
        mock.assert_async().await;
    }

    /// A partial success is a 2xx and final: the client must not retry it.
    #[tokio::test]
    async fn test_a_partial_success_is_accepted_and_not_retried() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/logs")
            .with_status(200)
            .with_body(
                r#"{"partialSuccess":{"rejectedLogRecords":"1","errorMessage":"dropped one"}}"#,
            )
            .expect(1)
            .create_async()
            .await;

        let (sent, _) = post_recording(&format!("{}/v1/logs", server.url()), SEND_BUDGET).await;

        assert_eq!(sent, Sent::Accepted);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_non_retryable_statuses_are_final() {
        for status in [400, 401, 404, 413, 500] {
            let mut server = mockito::Server::new_async().await;
            let mock = server
                .mock("POST", "/v1/logs")
                .with_status(status)
                .expect(1)
                .create_async()
                .await;

            let (sent, waits) =
                post_recording(&format!("{}/v1/logs", server.url()), SEND_BUDGET).await;

            assert_eq!(sent, Sent::Rejected(status as u16));
            assert!(waits.is_empty(), "{status} must not be retried");
            mock.assert_async().await;
        }
    }

    /// A 503 with `Retry-After: 2` waits exactly two seconds, then retries.
    #[tokio::test]
    async fn test_retry_after_sets_the_wait() {
        let mut server = mockito::Server::new_async().await;
        let throttled = server
            .mock("POST", "/v1/logs")
            .with_status(503)
            .with_header("retry-after", "2")
            .expect(1)
            .create_async()
            .await;
        let accepted = server
            .mock("POST", "/v1/logs")
            .with_status(200)
            .expect(1)
            .create_async()
            .await;

        let (sent, waits) = post_recording(&format!("{}/v1/logs", server.url()), SEND_BUDGET).await;

        assert_eq!(sent, Sent::Accepted);
        assert_eq!(waits, [Duration::from_secs(2)]);
        throttled.assert_async().await;
        accepted.assert_async().await;
    }

    /// Each retryable status backs off and retries until the budget runs out,
    /// and no wait is planned past it.
    #[tokio::test]
    async fn test_retryable_statuses_back_off_until_the_budget_runs_out() {
        for status in [429, 502, 503, 504] {
            let mut server = mockito::Server::new_async().await;
            let mock = server
                .mock("POST", "/v1/logs")
                .with_status(status)
                .expect_at_least(3)
                .create_async()
                .await;

            let (sent, waits) =
                post_recording(&format!("{}/v1/logs", server.url()), SEND_BUDGET).await;

            assert_eq!(sent, Sent::Dropped, "{status}");
            assert!(waits.len() >= 2, "{status}: {waits:?}");
            assert!(waits.iter().sum::<Duration>() < SEND_BUDGET, "{waits:?}");
            mock.assert_async().await;
        }
    }

    /// A `Retry-After` longer than the budget has left drops the batch now
    /// rather than waiting for nothing.
    #[tokio::test]
    async fn test_a_retry_after_past_the_budget_drops_the_batch() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/logs")
            .with_status(429)
            .with_header("retry-after", "60")
            .expect(1)
            .create_async()
            .await;

        let (sent, waits) = post_recording(&format!("{}/v1/logs", server.url()), SEND_BUDGET).await;

        assert_eq!(sent, Sent::Dropped);
        assert!(waits.is_empty());
        mock.assert_async().await;
    }

    /// A refused connection is retried like a 503.
    #[tokio::test]
    async fn test_a_refused_connection_is_retried() {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };

        let (sent, waits) =
            post_recording(&format!("http://127.0.0.1:{port}/v1/logs"), SEND_BUDGET).await;

        assert_eq!(sent, Sent::Dropped);
        assert!(waits.len() >= 2, "{waits:?}");
    }

    /// An endpoint that accepts the connection and never answers is given up
    /// on when the budget ends. Scaled down from [`SEND_BUDGET`] so the test
    /// does not sit through ten seconds.
    #[tokio::test]
    async fn test_an_endpoint_that_never_answers_is_dropped_at_the_budget() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1/logs", listener.local_addr().unwrap());
        let budget = Duration::from_millis(300);

        let started = Instant::now();
        let (sent, waits) = post_recording(&endpoint, budget).await;

        assert_eq!(sent, Sent::Dropped);
        assert!(waits.is_empty());
        let elapsed = started.elapsed();
        assert!(elapsed >= budget, "{elapsed:?}");
        assert!(elapsed < budget + Duration::from_secs(5), "{elapsed:?}");
        drop(listener);
    }

    /// Ten seconds: the OTLP exporter's default export timeout.
    #[test]
    fn test_the_budget_is_the_exporter_default() {
        assert_eq!(SEND_BUDGET, Duration::from_secs(10));
        assert_eq!(MAX_RECORDS_PER_REQUEST, 100);
    }

    #[test]
    fn test_retry_delay() {
        let ten = Duration::from_secs(10);
        assert_eq!(
            retry_delay(1, Some(2), ten, 0.7),
            Some(Duration::from_secs(2))
        );
        assert_eq!(retry_delay(1, Some(0), ten, 0.7), Some(Duration::ZERO));
        assert_eq!(retry_delay(1, Some(10), ten, 0.7), None);
        // Half to all of 500 ms, doubling per retry.
        assert_eq!(
            retry_delay(1, None, ten, 0.0),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            retry_delay(2, None, ten, 0.0),
            Some(Duration::from_millis(500))
        );
        assert_eq!(retry_delay(3, None, ten, 0.0), Some(Duration::from_secs(1)));
        let near_all = retry_delay(1, None, ten, 0.999_999).unwrap();
        assert!(near_all < Duration::from_millis(500) && near_all > Duration::from_millis(499));
        assert_eq!(retry_delay(4, None, Duration::from_secs(1), 0.0), None);
        // A huge retry count saturates instead of overflowing.
        assert_eq!(retry_delay(u32::MAX, None, ten, 0.0), None);
    }

    #[test]
    fn test_jitter_is_a_unit_fraction() {
        for _ in 0..1000 {
            let j = jitter();
            assert!((0.0..1.0).contains(&j), "{j}");
        }
    }

    #[test]
    fn test_only_the_four_otlp_codes_are_retryable() {
        let retryable: Vec<u16> = (100..600)
            .filter(|&code| is_retryable(reqwest::StatusCode::from_u16(code).unwrap()))
            .collect();
        assert_eq!(retryable, [429, 502, 503, 504]);
    }

    /// A non-loopback endpoint gets a client that refuses plain HTTP, even to
    /// loopback, and a request the client refuses to build is final, not
    /// retried.
    #[tokio::test]
    async fn test_the_client_refuses_plain_http_off_loopback() {
        let client = client_for("https://collector.example.com/v1/logs").unwrap();

        let (sent, waits) =
            post_recording_with(&client, "http://127.0.0.1:1/v1/logs", SEND_BUDGET).await;

        assert_eq!(sent, Sent::Dropped);
        assert!(waits.is_empty());
    }

    // ── the uploader ────────────────────────────────────────────────────

    /// 150 spooled records go out as two requests, 100 and 50, each also
    /// appended to the file sink as one line, and the spool ends empty.
    #[tokio::test]
    async fn test_flush_sends_in_requests_of_100_and_writes_the_file() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/logs")
            .with_status(200)
            .expect(2)
            .create_async()
            .await;
        let origin = origin(logged_in(), None);
        let records: Vec<LogRecord> = (0..150)
            .map(|i| origin.context_added("s", None, None, &ids(&[&format!("adr-{i}")])))
            .collect();
        spool(&records);
        let file = home.path().join("otlp.jsonl");

        flush(&config(
            Some(&format!("{}/v1/logs", server.url())),
            Some(file.to_str().unwrap()),
        ))
        .await;

        mock.assert_async().await;
        let written = std::fs::read_to_string(&file).unwrap();
        let counts: Vec<usize> = written
            .lines()
            .map(|line| {
                let body: Value = serde_json::from_str(line).unwrap();
                body["resourceLogs"][0]["scopeLogs"][0]["logRecords"]
                    .as_array()
                    .unwrap()
                    .len()
            })
            .collect();
        assert_eq!(counts, [100, 50]);
        assert!(OTLP_LOGS.take(1).is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    /// With a file and no endpoint, the file is written and nothing is sent.
    #[tokio::test]
    async fn test_flush_with_only_a_file_writes_it() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        spool(&sample_records());
        let file = home.path().join("otlp.jsonl");

        flush(&config(None, Some(file.to_str().unwrap()))).await;

        assert_eq!(std::fs::read_to_string(&file).unwrap().lines().count(), 1);
        assert!(OTLP_LOGS.take(1).is_empty());
    }

    /// Opted out, or with no output left, the uploader touches nothing.
    #[tokio::test]
    async fn test_flush_without_outputs_leaves_the_spool_alone() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        spool(&sample_records());

        flush(&Config::default()).await;
        let _no = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
        flush(&config(None, Some("/tmp/never-written.jsonl"))).await;

        assert_eq!(OTLP_LOGS.take(MAX_RECORDS_PER_REQUEST).len(), 2);
    }

    /// A batch of damaged lines is dropped without a request, and the
    /// uploader carries on to the next one.
    #[tokio::test]
    async fn test_flush_skips_a_batch_with_no_valid_record() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let damaged: Vec<String> = (0..MAX_RECORDS_PER_REQUEST)
            .map(|i| format!("damaged {i}"))
            .collect();
        OTLP_LOGS.append_lines(&damaged).unwrap();
        spool(&sample_records());
        let file = home.path().join("otlp.jsonl");

        flush(&config(None, Some(file.to_str().unwrap()))).await;

        let written = std::fs::read_to_string(&file).unwrap();
        assert_eq!(written.lines().count(), 1, "{written}");
        assert!(OTLP_LOGS.take(1).is_empty());
    }
}
