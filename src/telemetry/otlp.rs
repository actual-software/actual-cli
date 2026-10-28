//! ADR telemetry as OpenTelemetry log events in Actual's `actual.*` event
//! catalog, over OTLP/HTTP with protobuf encoding: ADRs entering an agent's
//! context and meeting the gates, and the gate checks themselves.
//!
//! # Design
//!
//! **Events, not spans.** Each event is one `LogRecord` with `eventName` set.
//! An ADR entering context happens at a point in time and has no duration,
//! which is what the semantic conventions reserve events for, and an event
//! needs no enclosing span. A check, which does take time, is two events, one
//! when it starts and one when it completes, and the second carries the
//! duration.
//!
//! **No identity on the record.** A record names no user, organization,
//! repository or installation, hashed or otherwise. It carries ids that
//! correlate events instead: one per event, one per CLI execution, and the
//! agent session's.
//!
//! **Protobuf on the wire, OTLP/JSON on disk.** Records are built and spooled
//! as OTLP/JSON. The uploader builds each request from the official
//! `opentelemetry-proto` messages and POSTs it as binary protobuf on the
//! existing HTTP client, while the file sink keeps the same request as a line
//! of OTLP/JSON a person can read. The OpenTelemetry SDK would add a
//! background exporter to a process that lives for milliseconds. A test
//! decodes what the endpoint receives with the official types and checks it
//! against the file's line.
//!
//! **Off until configured.** Nothing is built unless an endpoint or a local
//! file is set, and only this CLI's own settings are read, never
//! `OTEL_EXPORTER_OTLP_*`, which a shell or CI job may set for other tools.
//!
//! **The hook spools, a detached uploader sends.** The hook appends records to
//! [`OTLP_LOGS`] and spawns `__telemetry-flush`, whose [`flush`] takes them
//! out a request at a time for up to [`SEND_BUDGET`], writes each request to
//! the file sink, and POSTs it. Nothing here prints: every outcome is logged
//! at debug and dropped.
//!
//! **Signed in, a request to Actual's API says whose events it holds.** When
//! the endpoint is at the address the advisor uses, the records recorded under
//! the current sign-in go in a request carrying its access token as the
//! bearer, and Actual attributes them from the token. The records themselves
//! stay anonymous. [`crate::telemetry::sign_in`] decides which records the
//! token covers, and a 401 sends the batch again once, signed out.

use std::future::Future;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use opentelemetry_proto::tonic::collector::logs::v1 as collector;
use opentelemetry_proto::tonic::common::v1 as common;
use opentelemetry_proto::tonic::logs::v1 as logs;
use opentelemetry_proto::tonic::resource::v1 as resource;
use prost::Message;
use serde::{Deserialize, Serialize};

use crate::config::types::{validate_otlp_file, validate_otlp_logs_endpoint, Config};
use crate::telemetry::opt_out;
use crate::telemetry::sign_in::{self, SignIn};
use crate::telemetry::spool::OTLP_LOGS;

/// The event sent when an ADR is detected: by entering a session's context
/// for the first time, or by a gate judging any of its rules.
pub(crate) const ADR_DETECTED: &str = "actual.adr.detected";

/// The event sent when a gate is evaluated: a gate blocking on an ADR's rules,
/// or a human overriding them with `check-override`.
pub(crate) const GATE_EVALUATED: &str = "actual.gate.evaluated";

/// The event a `plan-check` or `impl-check` run sends for its start.
pub(crate) const ACTION_STARTED: &str = "actual.action.started";

/// The event the same run sends once it has a verdict.
pub(crate) const ACTION_COMPLETED: &str = "actual.action.completed";

/// The version of the attributes below, sent as `actual.event.schema_version`.
const SCHEMA_VERSION: i64 = 1;

/// `deployment.environment.name`: release builds are the ones users run.
const ENVIRONMENT: &str = if cfg!(debug_assertions) {
    "development"
} else {
    "production"
};

/// `actual.surface`: the kind of producer this CLI is.
const SURFACE: &str = "coding-agent";

/// Overrides `telemetry.otlp_logs_endpoint`.
const ENDPOINT_ENV: &str = "ACTUAL_OTLP_LOGS_ENDPOINT";

/// Overrides `telemetry.otlp_file`.
const FILE_ENV: &str = "ACTUAL_OTLP_FILE";

/// Records per request.
pub(crate) const MAX_RECORDS_PER_REQUEST: usize = 100;

/// How long one upload may take in all, every request and retry included.
/// The batches it hasn't reached by then stay in the spool for the next one.
pub(crate) const SEND_BUDGET: Duration = Duration::from_secs(5);

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
/// decimal strings, ids as hex. An absent field reads as its default. This is
/// the form the file sink holds, and the spool with a [`Spooled`] stamp beside
/// it; the endpoint gets [`LogRecord::to_proto`].
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LogRecord {
    time_unix_nano: String,
    observed_time_unix_nano: String,
    severity_number: i32,
    event_name: String,
    attributes: Vec<KeyValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    span_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flags: Option<u32>,
}

impl LogRecord {
    /// The official message for this record, or `None` when a spooled copy
    /// was damaged: a time that isn't a count of nanoseconds, or an id that
    /// isn't hex.
    fn to_proto(&self) -> Option<logs::LogRecord> {
        Some(logs::LogRecord {
            time_unix_nano: self.time_unix_nano.parse().ok()?,
            observed_time_unix_nano: self.observed_time_unix_nano.parse().ok()?,
            severity_number: self.severity_number,
            event_name: self.event_name.clone(),
            attributes: self.attributes.iter().map(KeyValue::to_proto).collect(),
            trace_id: hex_bytes(self.trace_id.as_deref().unwrap_or_default())?,
            span_id: hex_bytes(self.span_id.as_deref().unwrap_or_default())?,
            flags: self.flags.unwrap_or_default(),
            ..Default::default()
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct KeyValue {
    key: String,
    value: AnyValue,
}

impl KeyValue {
    fn to_proto(&self) -> common::KeyValue {
        common::KeyValue {
            key: self.key.clone(),
            value: Some(self.value.to_proto()),
            ..Default::default()
        }
    }
}

// The variants spell OTLP/JSON's keys: stringValue, intValue, arrayValue.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum AnyValue {
    StringValue(String),
    IntValue(#[serde(with = "decimal")] i64),
    ArrayValue { values: Vec<AnyValue> },
}

impl AnyValue {
    fn to_proto(&self) -> common::AnyValue {
        let value = match self {
            Self::StringValue(value) => common::any_value::Value::StringValue(value.clone()),
            Self::IntValue(value) => common::any_value::Value::IntValue(*value),
            Self::ArrayValue { values } => {
                common::any_value::Value::ArrayValue(common::ArrayValue {
                    values: values.iter().map(Self::to_proto).collect(),
                })
            }
        };
        common::AnyValue { value: Some(value) }
    }
}

/// OTLP/JSON's 64-bit integers, which it writes as decimal strings.
mod decimal {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(value: &i64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(value)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

fn text(key: &'static str, value: Option<&str>) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: AnyValue::StringValue(value.unwrap_or_default().to_string()),
    }
}

fn strings(key: &'static str, values: &[impl AsRef<str>]) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: AnyValue::ArrayValue {
            values: values
                .iter()
                .map(|value| AnyValue::StringValue(value.as_ref().to_string()))
                .collect(),
        },
    }
}

fn int(key: &'static str, value: i64) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: AnyValue::IntValue(value),
    }
}

/// The bytes `hex` spells, or `None` when it isn't whole hex digit pairs.
fn hex_bytes(hex: &str) -> Option<Vec<u8>> {
    let digit = |byte: u8| char::from(byte).to_digit(16);
    hex.as_bytes()
        .chunks(2)
        .map(|pair| match *pair {
            [high, low] => Some((digit(high)? * 16 + digit(low)?) as u8),
            _ => None,
        })
        .collect()
}

/// What the records from one run share: the CLI execution that sends them,
/// when, and the trace they belong to.
pub(crate) struct Origin {
    action_id: String,
    time_unix_nano: String,
    trace: Option<TraceContext>,
}

impl Origin {
    /// The origin of a run about to emit: this execution's action id, the
    /// current time, and the trace in `TRACEPARENT`, if any.
    pub(crate) fn resolve() -> Self {
        Self {
            action_id: action_id().to_string(),
            time_unix_nano: unix_nanos(SystemTime::now()),
            trace: std::env::var("TRACEPARENT")
                .ok()
                .and_then(|value| TraceContext::parse(&value)),
        }
    }

    /// The [`ADR_DETECTED`] record for one logical ADR entering a session's
    /// context.
    pub(crate) fn context_added(
        &self,
        session_id: &str,
        agent_id: Option<&str>,
        adr_ref: &str,
        adr_ids: &[String],
    ) -> LogRecord {
        let mut record = self.adr_record(ADR_DETECTED, session_id, agent_id, adr_ref, adr_ids);
        record
            .attributes
            .push(text("actual.detection.method", Some("context")));
        record
    }

    /// The [`GATE_EVALUATED`] record for one logical ADR a human overrode:
    /// no agent, since a human ran the command, then the outcome, then the
    /// gates whose loop in this session had denied the overridden rules.
    pub(crate) fn overridden(
        &self,
        session_id: &str,
        adr_ref: &str,
        adr_ids: &[String],
        denied_gates: &[&str],
    ) -> LogRecord {
        let mut record = self.adr_record(GATE_EVALUATED, session_id, None, adr_ref, adr_ids);
        record.attributes.extend([
            text("actual.gate.outcome", Some("overridden")),
            strings("actual.gate.denied_types", denied_gates),
        ]);
        record
    }

    /// The [`ADR_DETECTED`] record for one logical ADR a gate judged rules of,
    /// with that gate, `planning` or `implementation`, as how it was detected.
    pub(crate) fn used(
        &self,
        session_id: &str,
        agent_id: Option<&str>,
        adr_ref: &str,
        adr_ids: &[String],
        gate: &str,
    ) -> LogRecord {
        let mut record = self.adr_record(ADR_DETECTED, session_id, agent_id, adr_ref, adr_ids);
        record
            .attributes
            .push(text("actual.detection.method", Some(gate)));
        record
    }

    /// The [`GATE_EVALUATED`] record for one logical ADR a gate blocked on:
    /// the outcome, then the gate, `planning` or `implementation`.
    pub(crate) fn breached(
        &self,
        session_id: &str,
        agent_id: Option<&str>,
        adr_ref: &str,
        adr_ids: &[String],
        gate: &str,
    ) -> LogRecord {
        let mut record = self.adr_record(GATE_EVALUATED, session_id, agent_id, adr_ref, adr_ids);
        record.attributes.extend([
            text("actual.gate.outcome", Some("breached")),
            text("actual.gate.type", Some(gate)),
        ]);
        record
    }

    /// The [`ACTION_STARTED`] record for a check that began at `started`:
    /// `action_type` names the check, `plan-check` or `impl-check`, and `mode`
    /// says how it ran, `hook` or `direct`.
    pub(crate) fn action_started(
        &self,
        started: SystemTime,
        session_id: &str,
        agent_id: Option<&str>,
        action_type: &str,
        mode: &str,
    ) -> LogRecord {
        let mut record = self.record(ACTION_STARTED, session_id, agent_id);
        record.time_unix_nano = unix_nanos(started);
        record.observed_time_unix_nano = record.time_unix_nano.clone();
        record.attributes.extend([
            text("actual.action.type", Some(action_type)),
            text("actual.action.mode", Some(mode)),
        ]);
        record
    }

    /// The [`ACTION_COMPLETED`] record for the same check: its type and mode,
    /// then its outcome, `allow`, `warn` or `block`, what made a `warn` one,
    /// and how long it took.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn action_completed(
        &self,
        session_id: &str,
        agent_id: Option<&str>,
        action_type: &str,
        mode: &str,
        outcome: &str,
        warn_reasons: &[&str],
        duration: Duration,
    ) -> LogRecord {
        let mut record = self.record(ACTION_COMPLETED, session_id, agent_id);
        record.attributes.extend([
            text("actual.action.type", Some(action_type)),
            text("actual.action.mode", Some(mode)),
            text("actual.action.outcome", Some(outcome)),
            strings("actual.action.warn_reasons", warn_reasons),
            int(
                "actual.action.duration_ms",
                i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
            ),
        ]);
        record
    }

    /// A record carrying the attributes every event shares, each present on
    /// every record and an empty string when unknown: its own event id, which
    /// `log.record.uid` repeats, the schema version, and the action, session
    /// and subagent it belongs to.
    fn record(&self, event_name: &str, session_id: &str, agent_id: Option<&str>) -> LogRecord {
        let event_id = uuid::Uuid::now_v7().to_string();
        LogRecord {
            time_unix_nano: self.time_unix_nano.clone(),
            observed_time_unix_nano: self.time_unix_nano.clone(),
            severity_number: SEVERITY_INFO,
            event_name: event_name.to_string(),
            attributes: vec![
                text("log.record.uid", Some(&event_id)),
                text("actual.event.id", Some(&event_id)),
                int("actual.event.schema_version", SCHEMA_VERSION),
                text("actual.action.id", Some(&self.action_id)),
                text("actual.session.id", Some(session_id)),
                text("actual.agent.id", agent_id),
            ],
            trace_id: self.trace.as_ref().map(|t| t.trace_id.clone()),
            span_id: self.trace.as_ref().map(|t| t.span_id.clone()),
            flags: self.trace.as_ref().map(|t| t.flags),
        }
    }

    /// [`Self::record`] for an ADR event, followed by `adr_ref` naming the
    /// logical ADR and the `adr_id` of each of its files.
    fn adr_record(
        &self,
        event_name: &str,
        session_id: &str,
        agent_id: Option<&str>,
        adr_ref: &str,
        adr_ids: &[String],
    ) -> LogRecord {
        let mut record = self.record(event_name, session_id, agent_id);
        record.attributes.extend([
            text("actual.adr.ref", Some(adr_ref)),
            strings("actual.adr.ids", adr_ids),
        ]);
        record
    }
}

/// This execution's `actual.action.id`: a UUIDv7 made the first time a record
/// asks, and shared by every record the process builds after that.
fn action_id() -> &'static str {
    static ACTION_ID: OnceLock<String> = OnceLock::new();
    ACTION_ID.get_or_init(|| uuid::Uuid::now_v7().to_string())
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

/// One line of the spool: a record, and the [`sign_in::stamp`] of the sign-in
/// it was recorded under when it was bound for Actual's API. The stamp is the
/// uploader's own bookkeeping, so neither the file sink nor the endpoint gets
/// it, and a line without one reads as recorded signed out.
#[derive(Serialize, Deserialize)]
struct Spooled<R> {
    #[serde(flatten)]
    record: R,
    #[serde(
        rename = "actualSignIn",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    sign_in: Option<String>,
}

/// Append `records` to the OTLP spool for the uploader, best-effort: records
/// the spool turns away are logged at debug and dropped. Records bound for
/// Actual's API in `outputs` are stamped with the current sign-in, if any.
pub(crate) fn spool(outputs: &Outputs, records: &[LogRecord]) {
    let sign_in = sign_in::recording_stamp(outputs.endpoint.as_deref());
    let lines: Vec<String> = records
        .iter()
        .filter_map(|record| {
            serde_json::to_string(&Spooled {
                record,
                sign_in: sign_in.clone(),
            })
            .ok()
        })
        .collect();
    if let Err(e) = OTLP_LOGS.append_lines(&lines) {
        tracing::debug!("otlp logs: {} records not spooled: {e}", lines.len());
    }
}

/// The records spooled `lines` hold, with their stamps. A line is dropped when
/// it isn't a record, or when its time or ids won't encode.
fn records(lines: &[String]) -> Vec<Spooled<LogRecord>> {
    lines
        .iter()
        .filter_map(|line| serde_json::from_str::<Spooled<LogRecord>>(line).ok())
        .filter(|spooled| spooled.record.to_proto().is_some())
        .collect()
}

/// The requests `spooled` goes out in: the records `sign_in` covers, flagged
/// to carry its token, then the rest. An empty one is left out, so a batch
/// sent signed out is one request, as it always was.
fn requests(
    spooled: Vec<Spooled<LogRecord>>,
    sign_in: Option<&SignIn>,
) -> Vec<(Vec<LogRecord>, bool)> {
    let (covered, rest): (Vec<_>, Vec<_>) = spooled.into_iter().partition(|spooled| {
        sign_in.is_some_and(|sign_in| sign_in.covers(spooled.sign_in.as_deref()))
    });
    [(covered, true), (rest, false)]
        .into_iter()
        .filter(|(group, _)| !group.is_empty())
        .map(|(group, signed)| (group.into_iter().map(|s| s.record).collect(), signed))
        .collect()
}

/// What every request says about its sender: this CLI, as the service and as
/// the SDK.
fn resource_attributes() -> Vec<KeyValue> {
    vec![
        text("service.name", Some(NAME)),
        text("service.version", Some(VERSION)),
        text("deployment.environment.name", Some(ENVIRONMENT)),
        text("actual.surface", Some(SURFACE)),
        text("telemetry.sdk.name", Some(NAME)),
        text("telemetry.sdk.language", Some("rust")),
        text("telemetry.sdk.version", Some(VERSION)),
    ]
}

/// `records` as one `ExportLogsServiceRequest` in OTLP/JSON, the line the
/// file sink appends.
fn json_request(records: &[LogRecord]) -> String {
    serde_json::json!({
        "resourceLogs": [{
            "resource": {"attributes": resource_attributes()},
            "scopeLogs": [{
                "scope": {"name": NAME, "version": VERSION},
                "logRecords": records,
            }],
        }],
    })
    .to_string()
}

/// `records` as one `ExportLogsServiceRequest` built from the official
/// messages, in the binary protobuf encoding the endpoint receives.
fn protobuf_request(records: &[LogRecord]) -> Vec<u8> {
    collector::ExportLogsServiceRequest {
        resource_logs: vec![logs::ResourceLogs {
            resource: Some(resource::Resource {
                attributes: resource_attributes()
                    .iter()
                    .map(KeyValue::to_proto)
                    .collect(),
                ..Default::default()
            }),
            scope_logs: vec![logs::ScopeLogs {
                scope: Some(common::InstrumentationScope {
                    name: NAME.to_string(),
                    version: VERSION.to_string(),
                    ..Default::default()
                }),
                log_records: records.iter().filter_map(LogRecord::to_proto).collect(),
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

/// Send what the OTLP spool holds, a request at a time, for up to
/// [`SEND_BUDGET`] in all, then return.
///
/// Each request's records are taken out of the spool before it is sent, so
/// they are gone whatever the outcome. A request gets what is left of the
/// budget. The upload ends when the budget runs out or a request is dropped,
/// since a dropped request means the time left can't hold another try, and
/// the records it didn't reach stay in the spool for the next one. The
/// request is appended to the file sink as OTLP/JSON when one is set and
/// POSTed as protobuf when an endpoint is. A batch with no record left in it
/// sends nothing, since a sender should not send an empty envelope. An
/// opted-out run, or one with no usable output, leaves the spool alone.
///
/// Bound for Actual's API, a batch is split by sign-in: the records recorded
/// under the current one go with its token, the rest go signed out in a
/// request of their own. Actual answering 401 to a signed request sends it
/// once more without the token, and the rest of the upload goes signed out.
pub(crate) async fn flush(config: &Config) {
    flush_within(config, SEND_BUDGET).await;
}

/// [`flush`] with `budget` in place of [`SEND_BUDGET`], so a test can scale
/// it down.
async fn flush_within(config: &Config, budget: Duration) {
    let deadline = Instant::now() + budget;
    let Some(outputs) = Outputs::resolve(config) else {
        return;
    };
    if OTLP_LOGS.is_empty() {
        return;
    }
    let client = outputs.endpoint.as_deref().and_then(client_for);
    let mut sign_in = match (&client, outputs.endpoint.as_deref()) {
        (Some(_), Some(endpoint)) => sign_in::for_upload(endpoint, deadline).await,
        _ => None,
    };
    while Instant::now() < deadline {
        let lines = OTLP_LOGS.take(MAX_RECORDS_PER_REQUEST);
        if lines.is_empty() {
            return;
        }
        for (records, signed) in requests(records(&lines), sign_in.as_ref()) {
            if let Some(file) = &outputs.file {
                let line = format!("{}\n", json_request(&records));
                let _ = crate::config::paths::append_secure(file, line.as_bytes());
            }
            let (Some(client), Some(endpoint)) = (&client, &outputs.endpoint) else {
                continue;
            };
            let body = protobuf_request(&records);
            let token = sign_in.as_ref().filter(|_| signed).map(SignIn::token);
            let left = deadline.saturating_duration_since(Instant::now());
            let mut sent = post(
                client,
                endpoint,
                body.clone(),
                token,
                left,
                tokio::time::sleep,
            )
            .await;
            if token.is_some() && sent == Sent::Rejected(401) {
                sign_in = None;
                let left = deadline.saturating_duration_since(Instant::now());
                sent = post(client, endpoint, body, None, left, tokio::time::sleep).await;
            }
            tracing::debug!("otlp logs: {} records: {sent:?}", records.len());
            if sent == Sent::Dropped {
                return;
            }
        }
    }
}

/// A client with the API client's User-Agent that refuses plain HTTP unless
/// `endpoint` is loopback, and follows no redirect: the relaxed check covers
/// `endpoint` alone, so a 3xx is the final answer and the body goes nowhere
/// else.
fn client_for(endpoint: &str) -> Option<reqwest::Client> {
    client_builder(endpoint).build().ok()
}

fn client_builder(endpoint: &str) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(crate::api::client::USER_AGENT_VALUE)
        .https_only(!crate::net::is_loopback_http_url(endpoint))
        .redirect(reqwest::redirect::Policy::none())
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

/// POST `body` to `endpoint`, with `token` as its bearer when one is given,
/// retrying what OTLP/HTTP says to retry for no longer than `budget`.
///
/// 429, 502, 503 and 504, and a connection refused or dropped before an
/// answer, are retried after [`retry_delay`]. Every other status is final, as
/// is a 2xx carrying a partial success and a request that cannot be built. `sleep` does the waiting, so a test can
/// record the waits instead of sitting through them; the budget counts each
/// wait whether or not it was slept.
pub(crate) async fn post<S, F>(
    client: &reqwest::Client,
    endpoint: &str,
    body: Vec<u8>,
    token: Option<&str>,
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
        let request = client
            .post(endpoint)
            .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
            .timeout(budget.saturating_sub(spent))
            .body(body.clone());
        let request = match token {
            Some(token) => request.bearer_auth(token),
            None => request,
        };
        let response = request.send().await;
        spent += started.elapsed();
        let retry_after = match response {
            Ok(response) if response.status().is_success() => return Sent::Accepted,
            Ok(response) if is_retryable(response.status()) => {
                crate::runner::http_retry::parse_retry_after_delay(
                    response.headers(),
                    SystemTime::now(),
                )
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

/// The wait before retry number `retry`, counting from 1: exponential backoff
/// from [`BACKOFF_BASE`], jittered to a point between half and all of the
/// nominal wait, or the server's `Retry-After` when that is longer. `None`
/// when the wait would leave no time to try again.
fn retry_delay(
    retry: u32,
    retry_after: Option<Duration>,
    remaining: Duration,
    jitter: f64,
) -> Option<Duration> {
    let backoff = BACKOFF_BASE
        .saturating_mul(1 << retry.saturating_sub(1).min(16))
        .mul_f64(0.5 + jitter / 2.0);
    let wait = retry_after.unwrap_or_default().max(backoff);
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
            "ACTUAL_API_URL",
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

    fn origin(trace: Option<TraceContext>) -> Origin {
        Origin {
            action_id: "action-1".to_string(),
            time_unix_nano: "1792446915838119000".to_string(),
            trace,
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

    /// Outputs with no endpoint, which spool records with no sign-in stamp.
    fn no_outputs() -> Outputs {
        Outputs {
            endpoint: None,
            file: None,
        }
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
        let record = record_json(&origin(None).context_added(
            "session-1",
            None,
            "grp-1",
            &ids(&["adr-1", "adr-2"]),
        ));

        assert_eq!(record["eventName"], ADR_DETECTED);
        assert_eq!(record["severityNumber"], 9);
        assert_eq!(record["timeUnixNano"], "1792446915838119000");
        assert_eq!(record["observedTimeUnixNano"], record["timeUnixNano"]);
        for absent in ["body", "severityText", "traceId", "spanId", "flags"] {
            assert!(record.get(absent).is_none(), "{absent} must be absent");
        }
    }

    #[test]
    fn test_context_added_carries_every_attribute_in_order() {
        let record = record_json(&origin(None).context_added(
            "session-1",
            Some("agent-7"),
            "grp-1",
            &ids(&["adr-1", "adr-2"]),
        ));
        let attributes = attributes(&record);

        let keys: Vec<&str> = attributes.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "log.record.uid",
                "actual.event.id",
                "actual.event.schema_version",
                "actual.action.id",
                "actual.session.id",
                "actual.agent.id",
                "actual.adr.ref",
                "actual.adr.ids",
                "actual.detection.method",
            ]
        );
        let value = |key: &str| {
            attributes
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        let event_id = value("actual.event.id")["stringValue"].clone();
        let event_id = uuid::Uuid::parse_str(event_id.as_str().unwrap()).unwrap();
        assert_eq!(event_id.get_version_num(), 7);
        assert_eq!(value("log.record.uid"), value("actual.event.id"));
        assert_eq!(
            value("actual.event.schema_version"),
            serde_json::json!({"intValue": "1"})
        );
        assert_eq!(value("actual.action.id")["stringValue"], "action-1");
        assert_eq!(value("actual.session.id")["stringValue"], "session-1");
        assert_eq!(value("actual.agent.id")["stringValue"], "agent-7");
        assert_eq!(value("actual.adr.ref")["stringValue"], "grp-1");
        assert_eq!(
            value("actual.adr.ids"),
            serde_json::json!({"arrayValue": {"values": [{"stringValue": "adr-1"}, {"stringValue": "adr-2"}]}})
        );
        assert_eq!(value("actual.detection.method")["stringValue"], "context");
    }

    /// A record names no user, organization, repository or installation, and
    /// the main agent's record carries the agent id empty.
    #[test]
    fn test_a_record_carries_no_identity() {
        let record = record_json(&origin(None).context_added("s", None, "adr-9", &ids(&["adr-9"])));
        let attributes = attributes(&record);

        for (key, _) in &attributes {
            for identity in ["user", "org", "repo", "install", "hash", "fallback"] {
                assert!(!key.contains(identity), "{key} names an identity");
            }
        }
        let agent = &attributes
            .iter()
            .find(|(key, _)| key == "actual.agent.id")
            .unwrap()
            .1;
        assert_eq!(agent["stringValue"], "");
    }

    /// An override record carries the same attributes as a context record, in
    /// the same order and with no agent, then its outcome and the gates that
    /// had denied the overridden rules.
    #[test]
    fn test_overridden_carries_the_common_attributes_then_the_outcome_and_gates() {
        let origin = origin(None);
        let record = record_json(&origin.overridden(
            "session-1",
            "grp-1",
            &ids(&["adr-1", "adr-2"]),
            &["planning", "implementation"],
        ));
        let context = record_json(&origin.context_added(
            "session-1",
            None,
            "grp-1",
            &ids(&["adr-1", "adr-2"]),
        ));
        // Everything but the per-record ids.
        let shared = |record: &Value| -> Vec<(String, Value)> {
            attributes(record)
                .into_iter()
                .filter(|(key, _)| key != "log.record.uid" && key != "actual.event.id")
                .collect()
        };

        assert_eq!(record["eventName"], GATE_EVALUATED);
        assert_eq!(record["severityNumber"], 9);
        let mut overridden = shared(&record);
        assert_eq!(
            overridden.pop().unwrap(),
            (
                "actual.gate.denied_types".to_string(),
                serde_json::json!({"arrayValue": {"values": [{"stringValue": "planning"}, {"stringValue": "implementation"}]}})
            )
        );
        assert_eq!(
            overridden.pop().unwrap(),
            (
                "actual.gate.outcome".to_string(),
                serde_json::json!({"stringValue": "overridden"})
            )
        );
        let mut context = shared(&context);
        assert_eq!(context.pop().unwrap().0, "actual.detection.method");
        assert_eq!(overridden, context);
    }

    /// An override before any denial sends the gate list empty, not absent.
    #[test]
    fn test_overridden_with_no_denial_sends_an_empty_list() {
        let record = record_json(&origin(None).overridden("s", "adr-9", &ids(&["adr-9"]), &[]));
        let attributes = attributes(&record);
        assert_eq!(
            attributes.last().unwrap(),
            &(
                "actual.gate.denied_types".to_string(),
                serde_json::json!({"arrayValue": {"values": []}})
            )
        );
    }

    /// Used and breached records carry the same attributes as a context
    /// record, in the same order and with the gate's agent, then what each
    /// says about the gate: how a used ADR was detected, and a breach's
    /// outcome and gate.
    #[test]
    fn test_used_and_breached_carry_the_common_attributes_then_the_gate() {
        let origin = origin(None);
        let context = record_json(&origin.context_added(
            "session-1",
            Some("agent-1"),
            "grp-1",
            &ids(&["adr-1", "adr-2"]),
        ));
        // Everything but the per-record ids.
        let shared = |record: &Value| -> Vec<(String, Value)> {
            attributes(record)
                .into_iter()
                .filter(|(key, _)| key != "log.record.uid" && key != "actual.event.id")
                .collect()
        };
        let mut context = shared(&context);
        assert_eq!(context.pop().unwrap().0, "actual.detection.method");

        let used = record_json(&origin.used(
            "session-1",
            Some("agent-1"),
            "grp-1",
            &ids(&["adr-1", "adr-2"]),
            "planning",
        ));
        assert_eq!(used["eventName"], ADR_DETECTED);
        assert_eq!(used["severityNumber"], 9);
        let mut used = shared(&used);
        assert_eq!(
            used.pop().unwrap(),
            (
                "actual.detection.method".to_string(),
                serde_json::json!({"stringValue": "planning"})
            )
        );
        assert_eq!(used, context);

        let breached = record_json(&origin.breached(
            "session-1",
            Some("agent-1"),
            "grp-1",
            &ids(&["adr-1", "adr-2"]),
            "implementation",
        ));
        assert_eq!(breached["eventName"], GATE_EVALUATED);
        assert_eq!(breached["severityNumber"], 9);
        let mut breached = shared(&breached);
        assert_eq!(
            breached.pop().unwrap(),
            (
                "actual.gate.type".to_string(),
                serde_json::json!({"stringValue": "implementation"})
            )
        );
        assert_eq!(
            breached.pop().unwrap(),
            (
                "actual.gate.outcome".to_string(),
                serde_json::json!({"stringValue": "breached"})
            )
        );
        assert_eq!(breached, context);
    }

    /// A check's two records carry the attributes every record shares, in a
    /// context record's order without its ADR attributes, then the check's
    /// type and mode, and the completed record its outcome, warn reasons and
    /// duration. The started record is stamped when the check started, the
    /// completed one when the record is made.
    #[test]
    fn test_the_action_records_carry_the_common_attributes_then_the_check() {
        let origin = origin(None);
        let started = UNIX_EPOCH + Duration::from_secs(1_792_446_915);
        // Everything but the per-record ids.
        let shared = |record: &Value| -> Vec<(String, Value)> {
            attributes(record)
                .into_iter()
                .filter(|(key, _)| key != "log.record.uid" && key != "actual.event.id")
                .collect()
        };
        let mut common = shared(&record_json(&origin.context_added(
            "session-1",
            Some("agent-1"),
            "grp-1",
            &ids(&["adr-1"]),
        )));
        let adr: Vec<String> = common
            .split_off(4)
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(
            adr,
            [
                "actual.adr.ref",
                "actual.adr.ids",
                "actual.detection.method"
            ]
        );
        let check = |action_type: &str, mode: &str| {
            [
                (
                    "actual.action.type".to_string(),
                    serde_json::json!({"stringValue": action_type}),
                ),
                (
                    "actual.action.mode".to_string(),
                    serde_json::json!({"stringValue": mode}),
                ),
            ]
        };

        let start = record_json(&origin.action_started(
            started,
            "session-1",
            Some("agent-1"),
            "plan-check",
            "hook",
        ));
        assert_eq!(start["eventName"], ACTION_STARTED);
        assert_eq!(start["severityNumber"], 9);
        assert_eq!(start["timeUnixNano"], "1792446915000000000");
        assert_eq!(start["observedTimeUnixNano"], start["timeUnixNano"]);
        let mut start = shared(&start);
        assert_eq!(start.split_off(4), check("plan-check", "hook"));
        assert_eq!(start, common);

        let done = record_json(&origin.action_completed(
            "session-1",
            Some("agent-1"),
            "plan-check",
            "hook",
            "warn",
            &["partial_coverage", "active_override"],
            Duration::from_millis(1234),
        ));
        assert_eq!(done["eventName"], ACTION_COMPLETED);
        assert_eq!(done["severityNumber"], 9);
        assert_eq!(done["timeUnixNano"], "1792446915838119000");
        assert_eq!(done["observedTimeUnixNano"], done["timeUnixNano"]);
        let mut done = shared(&done);
        let mut tail = done.split_off(4);
        assert_eq!(
            tail.split_off(2),
            [
                (
                    "actual.action.outcome".to_string(),
                    serde_json::json!({"stringValue": "warn"})
                ),
                (
                    "actual.action.warn_reasons".to_string(),
                    serde_json::json!({"arrayValue": {"values": [{"stringValue": "partial_coverage"}, {"stringValue": "active_override"}]}})
                ),
                (
                    "actual.action.duration_ms".to_string(),
                    serde_json::json!({"intValue": "1234"})
                ),
            ]
        );
        assert_eq!(tail, check("plan-check", "hook"));
        assert_eq!(done, common);

        let forever = record_json(&origin.action_completed(
            "s",
            None,
            "impl-check",
            "direct",
            "allow",
            &[],
            Duration::MAX,
        ));
        assert_eq!(
            attributes(&forever).last().unwrap().1,
            serde_json::json!({"intValue": i64::MAX.to_string()})
        );
    }

    /// The action records survive the official types' round trip as the ADR
    /// records do, the warn reasons as a list and the duration as a 64-bit
    /// integer.
    #[test]
    fn test_the_action_records_round_trip_through_the_official_types() {
        let origin = origin(None);
        let records = [
            origin.action_started(
                UNIX_EPOCH + Duration::from_secs(1),
                "s",
                None,
                "impl-check",
                "direct",
            ),
            origin.action_completed(
                "s",
                None,
                "impl-check",
                "direct",
                "warn",
                &["requires_decision"],
                Duration::from_millis(7),
            ),
        ];
        let request = decoded(&records);
        let line: Value = serde_json::from_str(&json_request(&records)).unwrap();

        assert_eq!(official_json(&request, &line), line);
        let log_records = &request.resource_logs[0].scope_logs[0].log_records;
        assert_eq!(log_records[0].event_name, ACTION_STARTED);
        assert_eq!(log_records[0].time_unix_nano, 1_000_000_000);
        let duration = log_records[1].attributes.last().unwrap();
        assert_eq!(duration.key, "actual.action.duration_ms");
        assert_eq!(
            duration
                .value
                .as_ref()
                .and_then(|value| value.value.as_ref()),
            Some(&common::any_value::Value::IntValue(7))
        );
    }

    /// Each record gets its own event id, which `log.record.uid` repeats, and
    /// every record from one run shares the run's action id.
    #[test]
    fn test_each_record_gets_its_own_event_id_and_shares_the_action_id() {
        let origin = origin(None);
        let ids_of = |record: &LogRecord| {
            let attributes = attributes(&record_json(record));
            let text = |key: &str| {
                let value = &attributes.iter().find(|(k, _)| k == key).unwrap().1;
                value["stringValue"].as_str().unwrap().to_string()
            };
            (
                text("log.record.uid"),
                text("actual.event.id"),
                text("actual.action.id"),
            )
        };
        let a = ids_of(&origin.context_added("s", None, "g1", &ids(&["x"])));
        let b = ids_of(&origin.context_added("s", None, "g1", &ids(&["x"])));

        assert_eq!(a.0, a.1);
        assert_eq!(b.0, b.1);
        assert_ne!(a.1, b.1);
        assert_eq!((a.2.as_str(), b.2.as_str()), ("action-1", "action-1"));
    }

    #[test]
    fn test_a_trace_context_links_the_record() {
        let trace = TraceContext::parse("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01");
        let record = record_json(&origin(trace).context_added("s", None, "x", &ids(&["x"])));
        assert_eq!(record["traceId"], "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(record["spanId"], "00f067aa0ba902b7");
        assert_eq!(record["flags"], 1);
    }

    /// An origin reads `TRACEPARENT` and takes this execution's action id, a
    /// UUIDv7 every origin in the process shares. It reads no install id, so
    /// the file that holds one is never created.
    #[test]
    fn test_origin_resolve_reads_traceparent_and_the_action_id() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _trace = EnvGuard::set(
            "TRACEPARENT",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00",
        );

        let origin = Origin::resolve();

        assert_eq!(origin.action_id, action_id());
        assert_eq!(Origin::resolve().action_id, origin.action_id);
        let action = uuid::Uuid::parse_str(&origin.action_id).unwrap();
        assert_eq!(action.get_version_num(), 7);
        assert_eq!(origin.trace.unwrap().flags, 0);
        assert!(origin.time_unix_nano.parse::<u128>().unwrap() > 0);
        assert!(!home.path().join("telemetry-id").exists());
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
        let origin = origin(trace);
        vec![
            origin.context_added("session-1", None, "grp-1", &ids(&["adr-1", "adr-2"])),
            origin.context_added("session-1", Some("agent-2"), "adr-3", &ids(&["adr-3"])),
            origin.used("session-1", None, "grp-1", &ids(&["adr-1"]), "planning"),
            origin.breached(
                "session-1",
                Some("agent-2"),
                "adr-3",
                &ids(&["adr-3"]),
                "implementation",
            ),
        ]
    }

    /// The protobuf request for `records`, decoded by the official types the
    /// way a receiver decodes it.
    fn decoded(records: &[LogRecord]) -> collector::ExportLogsServiceRequest {
        collector::ExportLogsServiceRequest::decode(protobuf_request(records).as_slice())
            .expect("the official types decode the request")
    }

    /// The text of a string value.
    fn string(value: Option<&common::AnyValue>) -> &str {
        let value = value.and_then(|value| value.value.as_ref());
        #[rustfmt::skip]
        let Some(common::any_value::Value::StringValue(text)) = value else { panic!("not a string: {value:?}") };
        text
    }

    #[test]
    fn test_the_request_wraps_records_in_the_resource_and_scope() {
        let request = decoded(&sample_records());

        let resource_logs = &request.resource_logs[0];
        let resource: Vec<(&str, &str)> = resource_logs
            .resource
            .as_ref()
            .unwrap()
            .attributes
            .iter()
            .map(|kv| (kv.key.as_str(), string(kv.value.as_ref())))
            .collect();
        assert_eq!(
            resource,
            [
                ("service.name", "actual-cli"),
                ("service.version", VERSION),
                ("deployment.environment.name", ENVIRONMENT),
                ("actual.surface", "coding-agent"),
                ("telemetry.sdk.name", "actual-cli"),
                ("telemetry.sdk.language", "rust"),
                ("telemetry.sdk.version", VERSION),
            ]
        );
        let scope_logs = &resource_logs.scope_logs[0];
        let scope = scope_logs.scope.as_ref().unwrap();
        assert_eq!(
            (scope.name.as_str(), scope.version.as_str()),
            ("actual-cli", VERSION)
        );
        assert_eq!(scope_logs.log_records.len(), 4);
    }

    /// Each record reaches the endpoint whole: the 64-bit times as integers,
    /// the trace and span ids as their 16 and 8 bytes, and every attribute,
    /// the ADR id list included.
    #[test]
    fn test_the_request_carries_each_record_as_the_official_message() {
        let request = decoded(&sample_records());
        let record = &request.resource_logs[0].scope_logs[0].log_records[0];

        assert_eq!(record.event_name, ADR_DETECTED);
        assert_eq!(record.severity_number, 9);
        assert_eq!(record.time_unix_nano, 1_792_446_915_838_119_000);
        assert_eq!(record.observed_time_unix_nano, record.time_unix_nano);
        assert_eq!(
            record.trace_id,
            [
                0x4b, 0xf9, 0x2f, 0x35, 0x77, 0xb3, 0x4d, 0xa6, 0xa3, 0xce, 0x92, 0x9d, 0x0e, 0x0e,
                0x47, 0x36
            ]
        );
        assert_eq!(
            record.span_id,
            [0x00, 0xf0, 0x67, 0xaa, 0x0b, 0xa9, 0x02, 0xb7]
        );
        assert_eq!(record.flags, 1);
        let keys: Vec<&str> = record.attributes.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys.len(), 9);
        assert_eq!(keys[2], "actual.event.schema_version");
        assert_eq!(
            record.attributes[2]
                .value
                .as_ref()
                .and_then(|value| value.value.as_ref()),
            Some(&common::any_value::Value::IntValue(1))
        );
        assert_eq!(keys[4], "actual.session.id");
        assert_eq!(string(record.attributes[4].value.as_ref()), "session-1");
        assert_eq!(keys[7], "actual.adr.ids");
        let ids = record.attributes[7]
            .value
            .as_ref()
            .and_then(|value| value.value.as_ref());
        #[rustfmt::skip]
        let Some(common::any_value::Value::ArrayValue(ids)) = ids else { panic!("actual.adr.ids is not a list: {ids:?}") };
        let ids: Vec<&str> = ids.values.iter().map(|id| string(Some(id))).collect();
        assert_eq!(ids, ["adr-1", "adr-2"]);
    }

    /// A record with no trace context goes out with no ids and no flags.
    #[test]
    fn test_a_record_outside_a_trace_has_no_ids() {
        let records = [origin(None).context_added("s", None, "x", &ids(&["x"]))];
        let request = decoded(&records);
        let record = &request.resource_logs[0].scope_logs[0].log_records[0];
        assert!(record.trace_id.is_empty());
        assert!(record.span_id.is_empty());
        assert_eq!(record.flags, 0);
    }

    /// A damaged spool line is dropped: one that isn't a record, and one whose
    /// time or ids won't encode. A batch of nothing else is empty.
    #[test]
    fn test_records_drops_damaged_lines() {
        let spooled = lines(&sample_records());
        let whole = &spooled[0];
        let mut damaged = vec![
            "not json".to_string(),
            "[1, 2]".to_string(),
            whole.replace("\"1792446915838119000\"", "\"soon\""),
            whole.replace(
                "4bf92f3577b34da6a3ce929d0e0e4736",
                "4bf92f3577b34da6a3ce929d0e0e473",
            ),
            whole.replace("00f067aa0ba902b7", "00f067aa0ba902bz"),
            whole.replace(r#"{"intValue":"1"}"#, r#"{"intValue":"one"}"#),
        ];
        assert!(damaged[2..].iter().all(|line| line != whole), "{damaged:?}");
        assert!(records(&damaged).is_empty(), "{damaged:?}");

        damaged.extend(spooled);
        assert_eq!(records(&damaged).len(), 4);
        assert!(records(&[]).is_empty());
    }

    #[test]
    fn test_hex_bytes() {
        assert_eq!(hex_bytes(""), Some(Vec::new()));
        assert_eq!(hex_bytes("00ff7A"), Some(vec![0x00, 0xff, 0x7a]));
        for malformed in ["abc", "0g", "+f", "é"] {
            assert_eq!(hex_bytes(malformed), None, "{malformed:?}");
        }
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

    /// `request` in the official types' own OTLP/JSON encoding, less the
    /// defaults the file sink's `line` leaves out, so the two can be compared.
    fn official_json(request: &collector::ExportLogsServiceRequest, line: &Value) -> Value {
        without_omitted_defaults(&serde_json::to_value(request).unwrap(), line)
    }

    /// The protobuf request decodes, with the official types, into the request
    /// the file sink's line holds: their own OTLP/JSON encoding of what they
    /// decoded is that line. A receiver ignores a JSON key it does not know,
    /// so decoding the line alone could not catch a misspelled one; this
    /// comparison can.
    #[test]
    fn test_the_request_round_trips_through_the_official_types() {
        let records = sample_records();
        let request = decoded(&records);
        let line: Value = serde_json::from_str(&json_request(&records)).unwrap();

        assert_eq!(official_json(&request, &line), line);
        let records = &request.resource_logs[0].scope_logs[0].log_records;
        let names: Vec<&str> = records.iter().map(|r| r.event_name.as_str()).collect();
        assert_eq!(
            names,
            [ADR_DETECTED, ADR_DETECTED, ADR_DETECTED, GATE_EVALUATED]
        );
        let record = &records[0];
        assert_eq!(record.time_unix_nano, 1_792_446_915_838_119_000);
        assert_eq!(record.trace_id.len(), 16);
    }

    /// Override records round-trip too, with the gate list full and empty.
    #[test]
    fn test_an_override_request_round_trips_through_the_official_types() {
        let origin = origin(None);
        let records = [
            origin.overridden("session-1", "grp-1", &ids(&["adr-1"]), &["planning"]),
            origin.overridden("session-1", "adr-3", &ids(&["adr-3"]), &[]),
        ];
        let request = decoded(&records);
        let line: Value = serde_json::from_str(&json_request(&records)).unwrap();

        assert_eq!(official_json(&request, &line), line);
        let decoded = &request.resource_logs[0].scope_logs[0].log_records;
        assert_eq!(decoded.len(), 2);
        assert!(decoded
            .iter()
            .all(|record| record.event_name == GATE_EVALUATED));
    }

    /// The negative control: one renamed key in the line, and the comparison
    /// fails.
    #[test]
    fn test_the_round_trip_catches_a_renamed_key() {
        let records = sample_records();
        let request = decoded(&records);
        let line = json_request(&records);
        for (key, renamed) in [
            ("\"eventName\"", "\"event_name\""),
            ("\"timeUnixNano\"", "\"timeUnixNanos\""),
            ("\"logRecords\"", "\"logRecord\""),
        ] {
            let mangled: Value = serde_json::from_str(&line.replacen(key, renamed, 1)).unwrap();
            assert_ne!(
                official_json(&request, &mangled),
                mangled,
                "{key} -> {renamed}"
            );
        }
    }

    // ── spooling ────────────────────────────────────────────────────────

    #[test]
    fn test_spool_appends_records_for_the_uploader() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());

        spool(&no_outputs(), &sample_records());

        let taken = OTLP_LOGS.take(MAX_RECORDS_PER_REQUEST);
        assert_eq!(taken.len(), 4);
        assert!(taken[0].contains(ADR_DETECTED));
        assert!(OTLP_LOGS.take(MAX_RECORDS_PER_REQUEST).is_empty());
    }

    /// A spool that refuses the records drops them without failing anything.
    #[test]
    fn test_spool_drops_records_the_spool_turns_away() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        std::fs::create_dir_all(home.path().join("otlp-logs-spool.lock")).unwrap();

        spool(&no_outputs(), &sample_records());

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

    /// What the sending tests POST. `post` sends whatever bytes it is given.
    const BODY: &[u8] = b"\x0a\x00";

    async fn post_recording_with(
        client: &reqwest::Client,
        endpoint: &str,
        budget: Duration,
    ) -> (Sent, Vec<Duration>) {
        let mut waits = Vec::new();
        let sent = post(client, endpoint, BODY.to_vec(), None, budget, |wait| {
            waits.push(wait);
            std::future::ready(())
        })
        .await;
        (sent, waits)
    }

    #[tokio::test]
    async fn test_post_sends_protobuf_with_the_cli_user_agent() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/logs")
            .match_header("content-type", "application/x-protobuf")
            .match_header("user-agent", crate::api::client::USER_AGENT_VALUE)
            .match_body(BODY.to_vec())
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

    /// `at` as an IMF-fixdate, the HTTP-date form servers send.
    fn http_date(at: SystemTime) -> String {
        chrono::DateTime::<chrono::Utc>::from(at)
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string()
    }

    /// A 503 whose `Retry-After` is an HTTP-date three seconds out waits until
    /// that moment, then retries. The date drops the fraction of a second, so
    /// the wait is a little under three seconds.
    #[tokio::test]
    async fn test_an_http_date_retry_after_sets_the_wait() {
        let mut server = mockito::Server::new_async().await;
        let throttled = server
            .mock("POST", "/v1/logs")
            .with_status(503)
            .with_header(
                "retry-after",
                &http_date(SystemTime::now() + Duration::from_secs(3)),
            )
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
        assert_eq!(waits.len(), 1, "{waits:?}");
        assert!(
            waits[0] > Duration::from_secs(1) && waits[0] <= Duration::from_secs(3),
            "{waits:?}"
        );
        throttled.assert_async().await;
        accepted.assert_async().await;
    }

    /// An HTTP-date further out than the budget reaches drops the batch at
    /// once instead of retrying early.
    #[tokio::test]
    async fn test_an_http_date_past_the_budget_drops_the_batch() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/logs")
            .with_status(429)
            .with_header(
                "retry-after",
                &http_date(SystemTime::now() + Duration::from_secs(60)),
            )
            .expect(1)
            .create_async()
            .await;

        let (sent, waits) = post_recording(&format!("{}/v1/logs", server.url()), SEND_BUDGET).await;

        assert_eq!(sent, Sent::Dropped);
        assert!(waits.is_empty());
        mock.assert_async().await;
    }

    /// `Retry-After: 0` waits the first backoff step, so a collector that
    /// keeps throttling can't drive back-to-back retries.
    #[tokio::test]
    async fn test_a_zero_retry_after_still_backs_off() {
        let mut server = mockito::Server::new_async().await;
        let throttled = server
            .mock("POST", "/v1/logs")
            .with_status(503)
            .with_header("retry-after", "0")
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
        assert_eq!(waits.len(), 1, "{waits:?}");
        assert!(
            waits[0] >= Duration::from_millis(250) && waits[0] < Duration::from_millis(500),
            "{waits:?}"
        );
        throttled.assert_async().await;
        accepted.assert_async().await;
    }

    /// An HTTP-date already past, as a clock running ahead of the collector's
    /// sees one, waits the first backoff step too.
    #[tokio::test]
    async fn test_a_past_http_date_backs_off() {
        let mut server = mockito::Server::new_async().await;
        let throttled = server
            .mock("POST", "/v1/logs")
            .with_status(503)
            .with_header("retry-after", "Sun, 06 Nov 1994 08:49:37 GMT")
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
        assert_eq!(waits.len(), 1, "{waits:?}");
        assert!(
            waits[0] >= Duration::from_millis(250) && waits[0] < Duration::from_millis(500),
            "{waits:?}"
        );
        throttled.assert_async().await;
        accepted.assert_async().await;
    }

    /// A `Retry-After` in neither form counts as absent, so the retry backs
    /// off as it would without one.
    #[tokio::test]
    async fn test_a_malformed_retry_after_backs_off() {
        let mut server = mockito::Server::new_async().await;
        let throttled = server
            .mock("POST", "/v1/logs")
            .with_status(503)
            .with_header("retry-after", "soon")
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
        assert_eq!(waits.len(), 1, "{waits:?}");
        assert!(
            waits[0] >= Duration::from_millis(250) && waits[0] < Duration::from_millis(500),
            "{waits:?}"
        );
        throttled.assert_async().await;
        accepted.assert_async().await;
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
    /// does not sit through five seconds.
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

    /// Five seconds for a whole upload, in requests of up to 100 records.
    #[test]
    fn test_the_budget_is_five_seconds_an_upload() {
        assert_eq!(SEND_BUDGET, Duration::from_secs(5));
        assert_eq!(MAX_RECORDS_PER_REQUEST, 100);
    }

    #[test]
    fn test_retry_delay() {
        let ten = Duration::from_secs(10);
        assert_eq!(
            retry_delay(1, Some(Duration::from_secs(2)), ten, 0.7),
            Some(Duration::from_secs(2))
        );
        // A `Retry-After` shorter than the backoff step waits the step.
        assert_eq!(
            retry_delay(1, Some(Duration::ZERO), ten, 0.0),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            retry_delay(3, Some(Duration::from_millis(100)), ten, 0.0),
            Some(Duration::from_secs(1))
        );
        assert_eq!(retry_delay(1, Some(ten), ten, 0.7), None);
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

    /// A loopback endpoint that redirects to a plain-HTTP host elsewhere gets
    /// a final answer, not a resend: nothing reaches the host it names, by
    /// POST or by GET, and nothing is retried.
    #[tokio::test]
    async fn test_a_redirect_is_final_and_sends_nothing_to_its_target() {
        let mut remote = mockito::Server::new_async().await;
        let remote_posts = remote
            .mock("POST", mockito::Matcher::Any)
            .expect(0)
            .create_async()
            .await;
        let remote_gets = remote
            .mock("GET", mockito::Matcher::Any)
            .expect(0)
            .create_async()
            .await;
        let location = format!(
            "http://collector.example:{}/v1/logs",
            remote.socket_address().port()
        );
        let mut local = mockito::Server::new_async().await;
        let endpoint = format!("{}/v1/logs", local.url());
        let client = client_builder(&endpoint)
            .resolve("collector.example", remote.socket_address())
            .build()
            .unwrap();

        for status in [301, 302, 303, 307, 308] {
            let redirect = local
                .mock("POST", "/v1/logs")
                .with_status(status)
                .with_header("location", &location)
                .create_async()
                .await;

            let (sent, waits) = post_recording_with(&client, &endpoint, SEND_BUDGET).await;

            assert_eq!(sent, Sent::Rejected(status as u16));
            assert!(waits.is_empty());
            redirect.assert_async().await;
            redirect.remove_async().await;
        }
        remote_posts.assert_async().await;
        remote_gets.assert_async().await;
    }

    // ── the uploader ────────────────────────────────────────────────────

    /// 150 spooled records go out as two protobuf requests, 100 and 50, each
    /// also appended to the file sink as one line, and the spool ends empty.
    /// The bytes the endpoint received decode, with the official types, into
    /// the request on the matching line.
    #[tokio::test]
    async fn test_flush_sends_in_requests_of_100_and_writes_the_file() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/logs")
            .match_header("content-type", "application/x-protobuf")
            .match_request({
                let received = received.clone();
                move |request| {
                    received
                        .lock()
                        .unwrap()
                        .push(request.body().unwrap().clone());
                    true
                }
            })
            .with_status(200)
            .expect(2)
            .create_async()
            .await;
        let origin = origin(None);
        let records: Vec<LogRecord> = (0..150)
            .map(|i| {
                let adr = format!("adr-{i}");
                origin.context_added("s", None, &adr, &ids(&[&adr]))
            })
            .collect();
        spool(&no_outputs(), &records);
        let file = home.path().join("otlp.jsonl");

        flush(&config(
            Some(&format!("{}/v1/logs", server.url())),
            Some(file.to_str().unwrap()),
        ))
        .await;

        mock.assert_async().await;
        let written = std::fs::read_to_string(&file).unwrap();
        let lines: Vec<Value> = written
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let requests: Vec<collector::ExportLogsServiceRequest> = received
            .lock()
            .unwrap()
            .iter()
            .map(|body| collector::ExportLogsServiceRequest::decode(body.as_slice()).unwrap())
            .collect();
        let counts: Vec<usize> = requests
            .iter()
            .map(|request| request.resource_logs[0].scope_logs[0].log_records.len())
            .collect();
        assert_eq!(counts, [100, 50]);
        assert_eq!(lines.len(), requests.len());
        for (request, line) in requests.iter().zip(&lines) {
            assert_eq!(&official_json(request, line), line);
        }
        assert!(OTLP_LOGS.take(1).is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    /// The budget covers the whole upload, not each request. A full spool is
    /// three batches, here against an endpoint that answers the first request
    /// in half a second and every later one in 1.2 seconds, with the budget
    /// scaled down to one second. The first batch lands, the second gets the
    /// half second left and is cut off when the budget ends, and the upload
    /// returns then, with the third batch still spooled for the next one.
    #[tokio::test]
    async fn test_one_budget_covers_every_request_of_an_upload() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let answered = AtomicUsize::new(0);
        let mut server = mockito::Server::new_async().await;
        let _slow = server
            .mock("POST", "/v1/logs")
            .with_status(200)
            .with_body_from_request(move |_| {
                let first = answered.fetch_add(1, Ordering::SeqCst) == 0;
                std::thread::sleep(Duration::from_millis(if first { 500 } else { 1200 }));
                Vec::new()
            })
            .create_async()
            .await;
        let origin = origin(None);
        let records: Vec<LogRecord> = (0..crate::telemetry::spool::MAX_SPOOL_EVENTS)
            .map(|i| {
                let adr = format!("adr-{i}");
                origin.context_added("s", None, &adr, &ids(&[&adr]))
            })
            .collect();
        spool(&no_outputs(), &records);
        let budget = Duration::from_secs(1);

        let started = Instant::now();
        flush_within(
            &config(Some(&format!("{}/v1/logs", server.url())), None),
            budget,
        )
        .await;
        let elapsed = started.elapsed();

        assert!(elapsed >= budget, "{elapsed:?}");
        assert!(elapsed < budget + Duration::from_millis(250), "{elapsed:?}");
        assert_eq!(
            OTLP_LOGS.take(usize::MAX),
            lines(&records[2 * MAX_RECORDS_PER_REQUEST..])
        );
    }

    /// An endpoint that refuses every connection: the first batch retries
    /// until the budget left can't hold another try, and the upload ends
    /// there, so the two batches after it stay spooled rather than being
    /// taken and dropped in the time that is left.
    #[tokio::test]
    async fn test_an_upload_ends_at_the_first_dropped_request() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let origin = origin(None);
        let records: Vec<LogRecord> = (0..crate::telemetry::spool::MAX_SPOOL_EVENTS)
            .map(|i| {
                let adr = format!("adr-{i}");
                origin.context_added("s", None, &adr, &ids(&[&adr]))
            })
            .collect();
        spool(&no_outputs(), &records);

        flush_within(
            &config(Some(&format!("http://127.0.0.1:{port}/v1/logs")), None),
            Duration::from_millis(600),
        )
        .await;

        assert_eq!(
            OTLP_LOGS.take(usize::MAX),
            lines(&records[MAX_RECORDS_PER_REQUEST..])
        );
    }

    /// Once the budget has run out, no batch is taken, even for a file sink
    /// alone.
    #[tokio::test]
    async fn test_an_upload_with_no_time_left_takes_nothing() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let records = sample_records();
        spool(&no_outputs(), &records);
        let file = home.path().join("otlp.jsonl");

        flush_within(&config(None, Some(file.to_str().unwrap())), Duration::ZERO).await;

        assert!(!file.exists());
        assert_eq!(OTLP_LOGS.take(usize::MAX), lines(&records));
    }

    /// With a file and no endpoint, the file is written and nothing is sent.
    #[tokio::test]
    async fn test_flush_with_only_a_file_writes_it() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        spool(&no_outputs(), &sample_records());
        let file = home.path().join("otlp.jsonl");

        flush(&config(None, Some(file.to_str().unwrap()))).await;

        assert_eq!(std::fs::read_to_string(&file).unwrap().lines().count(), 1);
        assert!(OTLP_LOGS.take(1).is_empty());
    }

    /// A file sink that already exists and is readable by other accounts is
    /// restricted to its owner before the uploader appends to it, and keeps
    /// what it held.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_flush_restricts_an_existing_file_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        spool(&no_outputs(), &sample_records());
        let file = home.path().join("otlp.jsonl");
        std::fs::write(&file, "{}\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();

        flush(&config(None, Some(file.to_str().unwrap()))).await;

        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(&file).unwrap().lines().count(), 2);
    }

    /// Opted out, or with no output left, the uploader touches nothing.
    #[tokio::test]
    async fn test_flush_without_outputs_leaves_the_spool_alone() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        spool(&no_outputs(), &sample_records());

        flush(&Config::default()).await;
        let _no = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
        flush(&config(None, Some("/tmp/never-written.jsonl"))).await;

        assert_eq!(OTLP_LOGS.take(MAX_RECORDS_PER_REQUEST).len(), 4);
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
        spool(&no_outputs(), &sample_records());
        let file = home.path().join("otlp.jsonl");

        flush(&config(None, Some(file.to_str().unwrap()))).await;

        let written = std::fs::read_to_string(&file).unwrap();
        assert_eq!(written.lines().count(), 1, "{written}");
        assert!(OTLP_LOGS.take(1).is_empty());
    }

    // ── the sign-in ─────────────────────────────────────────────────────

    use crate::auth::store::{self, StoredCredentials};
    use crate::telemetry::sign_in::tests::{isolated, reads, rotated, signed_in};
    use chrono::Duration as ChronoDuration;

    /// The bodies a mock received, one per request it answered.
    type Received = std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>;

    /// A mock of the logs endpoint answering `status`, `hits` times, to
    /// requests whose `Authorization` header is `authorization` (or that have
    /// none), keeping each body it answers.
    async fn logs(
        server: &mut mockito::ServerGuard,
        authorization: Option<&str>,
        status: usize,
        hits: usize,
    ) -> (mockito::Mock, Received) {
        let received = Received::default();
        let kept = received.clone();
        let header = match authorization {
            Some(value) => mockito::Matcher::Exact(value.to_string()),
            None => mockito::Matcher::Missing,
        };
        let mock = server
            .mock("POST", "/v1/logs")
            .match_header("content-type", "application/x-protobuf")
            .match_header("user-agent", crate::api::client::USER_AGENT_VALUE)
            .match_header("authorization", header)
            .with_status(status)
            .with_body_from_request(move |request| {
                kept.lock().unwrap().push(request.body().unwrap().clone());
                Vec::new()
            })
            .expect(hits)
            .create_async()
            .await;
        (mock, received)
    }

    /// The `actual.adr.ref` of each record in each request `received` holds.
    fn refs(received: &Received) -> Vec<Vec<String>> {
        received
            .lock()
            .unwrap()
            .iter()
            .map(|body| {
                collector::ExportLogsServiceRequest::decode(body.as_slice())
                    .unwrap()
                    .resource_logs[0]
                    .scope_logs[0]
                    .log_records
                    .iter()
                    .map(|record| {
                        let adr = record
                            .attributes
                            .iter()
                            .find(|kv| kv.key == "actual.adr.ref");
                        string(adr.and_then(|kv| kv.value.as_ref())).to_string()
                    })
                    .collect()
            })
            .collect()
    }

    /// `n` ADR records whose refs start with `prefix`.
    fn adrs(prefix: &str, n: usize) -> Vec<LogRecord> {
        let origin = origin(None);
        (0..n)
            .map(|i| {
                let adr = format!("{prefix}-{i}");
                origin.context_added("s", None, &adr, &ids(&[&adr]))
            })
            .collect()
    }

    fn adr_refs(prefix: &str, n: usize) -> Vec<String> {
        (0..n).map(|i| format!("{prefix}-{i}")).collect()
    }

    /// Outputs and config for an endpoint at `server`, which is Actual's API
    /// once `isolated` points the API address there.
    fn at(server: &mockito::ServerGuard) -> (Outputs, Config) {
        let endpoint = format!("{}/v1/logs", server.url());
        let outputs = Outputs {
            endpoint: Some(endpoint.clone()),
            file: None,
        };
        (outputs, config(Some(&endpoint), None))
    }

    fn save(creds: &StoredCredentials) {
        store::save(creds).unwrap();
    }

    /// Signed in, an upload to Actual's API carries the sign-in's token as its
    /// bearer, and is otherwise the request it always was.
    #[tokio::test]
    async fn test_a_signed_in_upload_to_actual_api_carries_the_token() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let mut server = mockito::Server::new_async().await;
        let (_home, _api) = isolated(&server.url());
        let (signed, received) = logs(&mut server, Some("Bearer token-a"), 200, 1).await;
        let (outputs, config) = at(&server);
        save(&signed_in(
            "org-a",
            "token-a",
            ChronoDuration::hours(1),
            None,
        ));
        spool(&outputs, &adrs("a", 3));

        flush(&config).await;

        signed.assert_async().await;
        assert_eq!(refs(&received), [adr_refs("a", 3)]);
        assert!(OTLP_LOGS.is_empty());
    }

    /// Signed out, the upload is today's request: one per batch, no token.
    #[tokio::test]
    async fn test_a_signed_out_upload_carries_no_token() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let mut server = mockito::Server::new_async().await;
        let (_home, _api) = isolated(&server.url());
        let (unsigned, received) = logs(&mut server, None, 200, 1).await;
        let (outputs, config) = at(&server);
        spool(&outputs, &adrs("a", 3));
        reads();

        flush(&config).await;

        unsigned.assert_async().await;
        assert_eq!(refs(&received), [adr_refs("a", 3)]);
        assert_eq!(reads(), 1, "the uploader looked and found no sign-in");
    }

    /// Opted out by the env var or by config, the uploader reads no sign-in,
    /// renews none and sends nothing, even with one about to expire. Opted
    /// back in, the same upload reads it, which shows the count is live.
    #[tokio::test]
    async fn test_an_opted_out_upload_reads_renews_and_sends_nothing() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let mut server = mockito::Server::new_async().await;
        let (_home, _api) = isolated(&server.url());
        let token = server
            .mock("POST", "/api/oauth/token")
            .expect(0)
            .create_async()
            .await;
        let (sent, _) = logs(&mut server, Some("Bearer token-a"), 200, 0).await;
        let (outputs, config) = at(&server);
        let stale = signed_in(
            "org-a",
            "token-a",
            ChronoDuration::seconds(30),
            Some(server.url()),
        );
        save(&stale);
        spool(&outputs, &adrs("a", 2));
        reads();

        let disabled = Config {
            telemetry: Some(TelemetryConfig {
                enabled: Some(false),
                otlp_logs_endpoint: outputs.endpoint.clone(),
                ..Default::default()
            }),
            ..Default::default()
        };
        flush(&disabled).await;
        {
            let _off = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
            flush(&config).await;
        }

        assert_eq!(reads(), 0);
        token.assert_async().await;
        sent.assert_async().await;
        assert_eq!(store::load().unwrap(), Some(stale));
        assert_eq!(OTLP_LOGS.take(usize::MAX).len(), 2);

        save(&signed_in(
            "org-a",
            "token-a",
            ChronoDuration::hours(1),
            None,
        ));
        spool(&outputs, &adrs("a", 1));
        reads();
        flush_within(&config, Duration::ZERO).await;
        assert_eq!(reads(), 1);
    }

    /// An upload with nothing spooled reads no sign-in and renews none.
    #[tokio::test]
    async fn test_an_upload_with_nothing_spooled_reads_no_sign_in() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let mut server = mockito::Server::new_async().await;
        let (_home, _api) = isolated(&server.url());
        let token = server
            .mock("POST", "/api/oauth/token")
            .expect(0)
            .create_async()
            .await;
        let (_, config) = at(&server);
        save(&signed_in(
            "org-a",
            "token-a",
            ChronoDuration::seconds(30),
            Some(server.url()),
        ));
        reads();

        flush(&config).await;

        assert_eq!(reads(), 0);
        token.assert_async().await;
    }

    /// An endpoint anywhere but Actual's API, here the same host on another
    /// port, gets no token, and the sign-in isn't read for it, at record time
    /// or at send time.
    #[tokio::test]
    async fn test_an_upload_to_another_endpoint_carries_no_token() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let api = mockito::Server::new_async().await;
        let mut collector = mockito::Server::new_async().await;
        let (_home, _api) = isolated(&api.url());
        let (unsigned, received) = logs(&mut collector, None, 200, 1).await;
        let (outputs, config) = at(&collector);
        save(&signed_in(
            "org-a",
            "token-a",
            ChronoDuration::hours(1),
            None,
        ));
        reads();

        spool(&outputs, &adrs("a", 2));
        flush(&config).await;

        unsigned.assert_async().await;
        assert_eq!(refs(&received), [adr_refs("a", 2)]);
        assert_eq!(reads(), 0);
    }

    /// Actual's API answering a signed request with a redirect gets a final
    /// answer: nothing, token or body, reaches the host it names.
    #[tokio::test]
    async fn test_a_signed_redirect_is_final_and_sends_nothing_to_its_target() {
        let mut remote = mockito::Server::new_async().await;
        let remote_posts = remote
            .mock("POST", mockito::Matcher::Any)
            .expect(0)
            .create_async()
            .await;
        let remote_gets = remote
            .mock("GET", mockito::Matcher::Any)
            .expect(0)
            .create_async()
            .await;
        let location = format!("{}/v1/logs", remote.url());
        let mut local = mockito::Server::new_async().await;
        let endpoint = format!("{}/v1/logs", local.url());
        let client = client_for(&endpoint).unwrap();

        for status in [301, 302, 303, 307, 308] {
            let redirect = local
                .mock("POST", "/v1/logs")
                .match_header("authorization", "Bearer token-a")
                .with_status(status)
                .with_header("location", &location)
                .expect(1)
                .create_async()
                .await;

            let sent = post(
                &client,
                &endpoint,
                BODY.to_vec(),
                Some("token-a"),
                SEND_BUDGET,
                tokio::time::sleep,
            )
            .await;

            assert_eq!(sent, Sent::Rejected(status as u16));
            redirect.assert_async().await;
            redirect.remove_async().await;
        }
        remote_posts.assert_async().await;
        remote_gets.assert_async().await;
    }

    /// Records go with the sign-in they were recorded under. Those recorded
    /// signed out, or under another organization, go in a request of their
    /// own without the token; the current sign-in's go with it.
    #[tokio::test]
    async fn test_records_go_with_the_sign_in_they_were_recorded_under() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let mut server = mockito::Server::new_async().await;
        let (_home, _api) = isolated(&server.url());
        let (signed, signed_bodies) = logs(&mut server, Some("Bearer token-a"), 200, 1).await;
        let (unsigned, unsigned_bodies) = logs(&mut server, None, 200, 1).await;
        let (outputs, config) = at(&server);

        spool(&outputs, &adrs("before", 2));
        save(&signed_in(
            "org-b",
            "token-b",
            ChronoDuration::hours(1),
            None,
        ));
        spool(&outputs, &adrs("org-b", 3));
        save(&signed_in(
            "org-a",
            "token-a",
            ChronoDuration::hours(1),
            None,
        ));
        spool(&outputs, &adrs("org-a", 4));

        flush(&config).await;

        signed.assert_async().await;
        unsigned.assert_async().await;
        assert_eq!(refs(&signed_bodies), [adr_refs("org-a", 4)]);
        let mut rest = adr_refs("before", 2);
        rest.extend(adr_refs("org-b", 3));
        assert_eq!(refs(&unsigned_bodies), [rest]);
    }

    /// The token never lands in the spool, the file sink or the debug log, a
    /// renewal included. The spool holds only the stamp, the file sink not
    /// even that, and the upload did log.
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn test_the_token_lands_in_no_file_or_log() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let mut server = mockito::Server::new_async().await;
        let (home, _api) = isolated(&server.url());
        let _token = server
            .mock("POST", "/api/oauth/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(rotated("token-new"))
            .create_async()
            .await;
        let (signed, _) = logs(&mut server, Some("Bearer token-new"), 200, 1).await;
        let (outputs, _) = at(&server);
        let file = home.path().join("otlp.jsonl");
        let config = config(outputs.endpoint.as_deref(), Some(file.to_str().unwrap()));
        let stale = signed_in(
            "org-a",
            "token-old",
            ChronoDuration::seconds(30),
            Some(server.url()),
        );
        save(&stale);
        spool(&outputs, &adrs("a", 2));
        let spooled = std::fs::read_to_string(home.path().join("otlp-logs-spool.jsonl")).unwrap();

        flush(&config).await;

        signed.assert_async().await;
        let written = std::fs::read_to_string(&file).unwrap();
        assert!(spooled.contains(&crate::telemetry::sign_in::stamp(&stale)));
        assert!(!written.contains("actualSignIn"));
        assert!(logs_contain("otlp logs: 2 records: Accepted"));
        for secret in ["token-old", "token-new"] {
            assert!(!spooled.contains(secret), "spool: {spooled}");
            assert!(!written.contains(secret), "file: {written}");
            assert!(!logs_contain(secret));
        }
    }

    /// Actual refusing the token with a 401 costs nothing: the batch goes
    /// again without it, and the rest of the upload goes signed out.
    #[tokio::test]
    async fn test_a_refused_token_resends_the_batch_signed_out_once() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let mut server = mockito::Server::new_async().await;
        let (_home, _api) = isolated(&server.url());
        let (signed, signed_bodies) = logs(&mut server, Some("Bearer token-a"), 401, 1).await;
        let (unsigned, unsigned_bodies) = logs(&mut server, None, 200, 2).await;
        let (outputs, config) = at(&server);
        save(&signed_in(
            "org-a",
            "token-a",
            ChronoDuration::hours(1),
            None,
        ));
        spool(&outputs, &adrs("a", 150));

        flush(&config).await;

        signed.assert_async().await;
        unsigned.assert_async().await;
        let all = adr_refs("a", 150);
        assert_eq!(refs(&signed_bodies), [all[..100].to_vec()]);
        assert_eq!(
            refs(&unsigned_bodies),
            [all[..100].to_vec(), all[100..].to_vec()]
        );
        assert!(OTLP_LOGS.is_empty());
    }

    /// Every other final answer to a signed request stays final: nothing is
    /// sent again.
    #[tokio::test]
    async fn test_other_refusals_of_a_signed_request_are_final() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let mut server = mockito::Server::new_async().await;
        let (_home, _api) = isolated(&server.url());
        let (outputs, config) = at(&server);
        save(&signed_in(
            "org-a",
            "token-a",
            ChronoDuration::hours(1),
            None,
        ));
        let (unsigned, _) = logs(&mut server, None, 200, 0).await;

        for status in [400, 403, 404, 413, 500, 307] {
            let (signed, _) = logs(&mut server, Some("Bearer token-a"), status, 1).await;
            spool(&outputs, &adrs("a", 2));

            flush(&config).await;

            signed.assert_async().await;
            signed.remove_async().await;
            assert!(OTLP_LOGS.is_empty(), "{status}");
        }
        unsigned.assert_async().await;
    }

    /// A sign-in within a minute of expiry is renewed and saved before the
    /// upload, and the renewed token goes. One with longer left is sent as it
    /// is.
    #[tokio::test]
    async fn test_an_expiring_sign_in_is_renewed_before_the_upload() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let mut server = mockito::Server::new_async().await;
        let (_home, _api) = isolated(&server.url());
        let token = server
            .mock("POST", "/api/oauth/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(rotated("token-new"))
            .expect(1)
            .create_async()
            .await;
        let (renewed, _) = logs(&mut server, Some("Bearer token-new"), 200, 2).await;
        let (outputs, config) = at(&server);
        save(&signed_in(
            "org-a",
            "token-old",
            ChronoDuration::seconds(30),
            Some(server.url()),
        ));
        spool(&outputs, &adrs("a", 2));

        flush(&config).await;

        let saved = store::load().unwrap().unwrap();
        assert_eq!(saved.access_token, "token-new");
        assert_eq!(saved.refresh_token, "token-new-refresh");

        // Two minutes left: no renewal, and the saved token goes as it is.
        let mut later = saved;
        later.expires_at = Some(chrono::Utc::now() + ChronoDuration::minutes(2));
        save(&later);
        spool(&outputs, &adrs("a", 2));
        flush(&config).await;

        token.assert_async().await;
        renewed.assert_async().await;
    }

    /// A renewal the server refuses sends the upload signed out and leaves the
    /// stored sign-in for the next command to report.
    #[tokio::test]
    async fn test_a_refused_renewal_sends_the_upload_signed_out() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let mut server = mockito::Server::new_async().await;
        let (_home, _api) = isolated(&server.url());
        let _token = server
            .mock("POST", "/api/oauth/token")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":"invalid_grant"}"#)
            .create_async()
            .await;
        let (unsigned, received) = logs(&mut server, None, 200, 1).await;
        let (outputs, config) = at(&server);
        let stale = signed_in(
            "org-a",
            "token-a",
            ChronoDuration::seconds(30),
            Some(server.url()),
        );
        save(&stale);
        spool(&outputs, &adrs("a", 2));

        flush(&config).await;

        unsigned.assert_async().await;
        assert_eq!(refs(&received), [adr_refs("a", 2)]);
        assert_eq!(store::load().unwrap(), Some(stale));
    }

    /// Records bound for Actual's API are spooled with the sign-in's stamp
    /// beside the record, which is otherwise the line it always was. Others
    /// carry none.
    #[test]
    fn test_spool_stamps_records_bound_for_actual_api() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = clear_env();
        let (_home, _api) = isolated("https://api.example.com");
        let creds = signed_in("org-a", "token-a", ChronoDuration::hours(1), None);
        save(&creds);
        let records = adrs("a", 1);
        let to_api = Outputs {
            endpoint: Some("https://api.example.com/v1/logs".to_string()),
            file: None,
        };

        spool(&to_api, &records);
        spool(&no_outputs(), &records);

        let taken = OTLP_LOGS.take(usize::MAX);
        let mut stamped: Value = serde_json::from_str(&taken[0]).unwrap();
        let stamp = stamped.as_object_mut().unwrap().remove("actualSignIn");
        assert_eq!(
            stamp,
            Some(Value::String(crate::telemetry::sign_in::stamp(&creds)))
        );
        assert_eq!(stamped, record_json(&records[0]));
        assert_eq!(taken[1], lines(&records)[0]);
        let parsed = super::records(&taken);
        assert_eq!(
            parsed[0].sign_in.as_deref(),
            stamp.as_ref().and_then(Value::as_str)
        );
        assert_eq!(parsed[1].sign_in, None);
    }
}
