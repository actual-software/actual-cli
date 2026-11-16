//! Telemetry debug mode: with `ACTUAL_TELEMETRY_DEBUG=1`, every telemetry
//! event this CLI queues, sends or holds back is also written to a local log,
//! and the first one a run writes prints one line on stderr saying where:
//!
//! ```text
//! telemetry debug: 7f3c2a91 -> ~/.actualai/actual/telemetry-debug/7f3c2a91.jsonl
//! ```
//!
//! **One file per id.** Inside a coding-agent hook the id is the agent's
//! session id, so every hook run in one session appends to one file. Outside a
//! hook each run gets a fresh id. The detached uploader is handed its parent's
//! id in [`ID_ENV`], so what it does with the parent's events lands beside
//! them.
//!
//! **One line per stage.** An event gets a line when it enters a spool
//! (`queued`), and another when the uploader is done with it: `sent`,
//! `refused` by the endpoint, `retained` in the spool for a later run, or
//! `dropped` with nobody receiving it. An event the telemetry settings keep
//! back is `not_sent`, with the reason.
//!
//! **The event as it goes out.** `event` holds the record, the batch or the
//! request body in the form the endpoint receives (OTLP in its JSON encoding).
//! When that form is the signed-in one, `local_only` names the account and
//! organization it is tied to, read from the stored sign-in and written only
//! here. A request that carries the sign-in's token says so in
//! `authorization` and never holds the token.
//!
//! Best-effort like every telemetry file: a write that fails is logged at
//! debug and dropped, and never fails a command.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde_json::{json, Map, Value};

/// Turns debug mode on.
pub(crate) const ENV: &str = "ACTUAL_TELEMETRY_DEBUG";

/// The id a child process logs under, set by the process that spawns it.
pub(crate) const ID_ENV: &str = "ACTUAL_TELEMETRY_DEBUG_ID";

/// Subdirectory of the config directory holding the logs.
const DIR: &str = "telemetry-debug";

/// What `authorization` says on a request that carried the sign-in's token.
pub(crate) const BEARER: &str = "bearer (stored sign-in)";

/// This run's id, once chosen.
static ID: Mutex<Option<String>> = Mutex::new(None);

/// Whether this process has printed its line.
static ANNOUNCED: AtomicBool = AtomicBool::new(false);

/// Which telemetry stream an event belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stream {
    /// OpenTelemetry log records, to `/v1/logs`.
    Otlp,
    /// Plan-governance and scope events, to the PostHog proxy.
    Governance,
    /// The `adr-bot` sync counters.
    Metrics,
}

impl Stream {
    fn as_str(self) -> &'static str {
        match self {
            Self::Otlp => "otlp",
            Self::Governance => "governance",
            Self::Metrics => "metrics",
        }
    }
}

/// Where an event is in its trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    Queued,
    Sent,
    Refused,
    Retained,
    Dropped,
    NotSent,
}

impl Stage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Sent => "sent",
            Self::Refused => "refused",
            Self::Retained => "retained",
            Self::Dropped => "dropped",
            Self::NotSent => "not_sent",
        }
    }
}

/// Why telemetry settings kept an event back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reason {
    TelemetryOff,
    ConfigUnreadable,
}

impl Reason {
    fn as_str(self) -> &'static str {
        match self {
            Self::TelemetryOff => "telemetry_off",
            Self::ConfigUnreadable => "config_unreadable",
        }
    }
}

/// One line of the log.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    stream: Stream,
    stage: Stage,
    signed_in: bool,
    event: Option<Value>,
    reason: Option<Reason>,
    status: Option<u16>,
    to: Option<String>,
    bearer: bool,
}

impl Entry {
    pub(crate) fn new(stream: Stream, stage: Stage) -> Self {
        Self {
            stream,
            stage,
            signed_in: false,
            event: None,
            reason: None,
            status: None,
            to: None,
            bearer: false,
        }
    }

    /// The event in the form it goes out.
    pub(crate) fn event(mut self, event: Value) -> Self {
        self.event = Some(event);
        self
    }

    /// Whether `event` is in its signed-in form.
    pub(crate) fn signed_in(mut self, signed_in: bool) -> Self {
        self.signed_in = signed_in;
        self
    }

    pub(crate) fn reason(mut self, reason: Reason) -> Self {
        self.reason = Some(reason);
        self
    }

    /// The HTTP status a refusal came with.
    pub(crate) fn status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    /// Where the event went: an endpoint, or a file.
    pub(crate) fn to(mut self, to: impl Into<String>) -> Self {
        self.to = Some(to.into());
        self
    }

    /// The request carried the sign-in's token as its bearer.
    pub(crate) fn bearer(mut self, bearer: bool) -> Self {
        self.bearer = bearer;
        self
    }

    /// The line, with `local_only` as given.
    fn to_json(&self, id: &str, local_only: Option<Value>) -> Value {
        let mut line = Map::new();
        line.insert("debug_id".into(), json!(id));
        line.insert("at".into(), json!(chrono::Utc::now().to_rfc3339()));
        line.insert("stream".into(), json!(self.stream.as_str()));
        line.insert("stage".into(), json!(self.stage.as_str()));
        line.insert(
            "auth".into(),
            json!(if self.signed_in {
                "signed_in"
            } else {
                "signed_out"
            }),
        );
        line.insert("sent".into(), json!(self.stage != Stage::NotSent));
        if let Some(reason) = self.reason {
            line.insert("reason".into(), json!(reason.as_str()));
        }
        if let Some(to) = &self.to {
            line.insert("to".into(), json!(to));
        }
        if let Some(status) = self.status {
            line.insert("status".into(), json!(status));
        }
        if self.bearer {
            line.insert("authorization".into(), json!(BEARER));
        }
        if let Some(event) = &self.event {
            line.insert("event".into(), event.clone());
        }
        if let Some(local_only) = local_only {
            line.insert("local_only".into(), local_only);
        }
        Value::Object(line)
    }
}

/// Whether debug mode is on: [`ENV`] set to anything but empty, `0` or
/// `false`.
pub(crate) fn enabled() -> bool {
    std::env::var(ENV)
        .ok()
        .is_some_and(|value| !matches!(value.trim(), "" | "0" | "false"))
}

/// An id safe to use as a file name, and short enough to read.
fn is_valid_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The id for a session: the session id itself when it makes a safe file name,
/// else a hash of it.
fn session_id_to_debug_id(session_id: &str) -> String {
    if is_valid_id(session_id) {
        session_id.to_string()
    } else {
        crate::telemetry::identity::hash_identifier("telemetry-debug", session_id)[..16].to_string()
    }
}

/// Log this run under the agent session `session_id`, unless the run already
/// has an id. Hook entry points call it once they have read their envelope.
pub(crate) fn use_session(session_id: &str) {
    if !enabled() || session_id.is_empty() || inherited_id().is_some() {
        return;
    }
    let mut id = ID.lock().unwrap_or_else(|e| e.into_inner());
    if id.is_none() {
        *id = Some(session_id_to_debug_id(session_id));
    }
}

/// The id a parent process handed down, when it is a safe one.
fn inherited_id() -> Option<String> {
    std::env::var(ID_ENV).ok().filter(|id| is_valid_id(id))
}

/// This run's id: the inherited one, else the session's, else a fresh one.
pub(crate) fn id() -> String {
    if let Some(id) = inherited_id() {
        return id;
    }
    let mut id = ID.lock().unwrap_or_else(|e| e.into_inner());
    id.get_or_insert_with(|| uuid::Uuid::new_v4().simple().to_string()[..8].to_string())
        .clone()
}

/// The variable and value a child process should log under, when debug mode
/// is on.
pub(crate) fn child_env() -> Option<(&'static str, String)> {
    enabled().then(|| (ID_ENV, id()))
}

/// The log file for `id`, or `None` when the config dir can't be resolved.
pub(crate) fn path_for(id: &str) -> Option<PathBuf> {
    crate::config::paths::config_dir()
        .ok()
        .map(|dir| dir.join(DIR).join(format!("{id}.jsonl")))
}

/// The line printed on stderr, with the home directory shown as `~`.
pub(crate) fn announcement(id: &str, path: &Path) -> String {
    let shown = dirs::home_dir()
        .and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf))
        .map(|rest| format!("~/{}", rest.display()))
        .unwrap_or_else(|| path.display().to_string());
    format!("telemetry debug: {id} -> {shown}")
}

/// What the stored sign-in names: the account and its organization.
fn local_only() -> Option<Value> {
    let creds = crate::auth::store::load().ok().flatten()?;
    let account = creds
        .email
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| creds.subject.clone().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| creds.member_id.clone());
    Some(json!({"account": account, "org": creds.organization_id}))
}

/// Write `entry` when debug mode is on. Never fails the caller.
pub(crate) fn record(entry: Entry) {
    if !enabled() {
        return;
    }
    if let Err(e) = write(&[entry]) {
        tracing::debug!("telemetry debug: not written: {e}");
    }
}

/// Write `entries` together when debug mode is on. Never fails the caller.
pub(crate) fn record_all(entries: Vec<Entry>) {
    if !enabled() || entries.is_empty() {
        return;
    }
    if let Err(e) = write(&entries) {
        tracing::debug!("telemetry debug: not written: {e}");
    }
}

fn write(entries: &[Entry]) -> std::io::Result<()> {
    let id = id();
    let path = path_for(&id).ok_or_else(|| std::io::Error::other("no config dir"))?;
    let dir = path.parent().unwrap_or(&path);
    create_private_dir(dir)?;
    let local_only = entries
        .iter()
        .any(|entry| entry.signed_in)
        .then(local_only)
        .flatten();
    let mut text = String::new();
    for entry in entries {
        let local = entry.signed_in.then(|| local_only.clone()).flatten();
        text.push_str(&entry.to_json(&id, local).to_string());
        text.push('\n');
    }
    crate::config::paths::append_secure(&path, text.as_bytes())?;
    if !ANNOUNCED.swap(true, Ordering::SeqCst) {
        let _ = writeln!(std::io::stderr(), "{}", announcement(&id, &path));
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// The entries for `lines` entering `stream`'s spool at `stage`: an OTLP line
/// without the spool's own sign-in stamp, which neither the file sink nor the
/// endpoint receives, and signed in when it had one; a governance event signed
/// in when it carries a hashed user or organization id.
pub(crate) fn spool_entries(stream: Stream, stage: Stage, lines: &[String]) -> Vec<Entry> {
    lines
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .map(|mut event| {
            let signed_in = match stream {
                Stream::Otlp => event
                    .as_object_mut()
                    .and_then(|record| record.remove("actualSignIn"))
                    .is_some(),
                Stream::Governance | Stream::Metrics => governance_signed_in(&event),
            };
            Entry::new(stream, stage).signed_in(signed_in).event(event)
        })
        .collect()
}

/// Whether a governance event carries the hashed ids of a sign-in.
pub(crate) fn governance_signed_in(event: &Value) -> bool {
    let properties = &event["properties"];
    ["user_id_hash", "org_id_hash"]
        .iter()
        .any(|key| properties[key].as_str().is_some_and(|v| !v.is_empty()))
}

/// Log `lines` entering `stream`'s spool, or turned away from it.
pub(crate) fn spooled(stream: Stream, lines: &[String], accepted: bool) {
    if !enabled() {
        return;
    }
    let stage = if accepted {
        Stage::Queued
    } else {
        Stage::Dropped
    };
    record_all(spool_entries(stream, stage, lines));
}

/// `events`, one JSON line each, as a spool holds them.
pub(crate) fn lines_of<T: serde::Serialize>(events: &[T]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| serde_json::to_string(event).ok())
        .collect()
}

/// Log `lines`, in the form `stream`'s spool would hold them, as kept back
/// for `reason`.
pub(crate) fn held_back(stream: Stream, lines: &[String], reason: Reason) {
    if !enabled() {
        return;
    }
    record_all(
        spool_entries(stream, Stage::NotSent, lines)
            .into_iter()
            .map(|entry| entry.reason(reason))
            .collect(),
    );
}

/// Log that `stream`'s sender sent nothing because the config file wouldn't
/// load. There is no event: the sender stopped before building one.
pub(crate) fn config_unreadable(stream: Stream) {
    record(Entry::new(stream, Stage::NotSent).reason(Reason::ConfigUnreadable));
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

    /// Debug mode on, with id `id`, logging under a config dir of the test's
    /// own. Hold `ENV_MUTEX` while the guards live.
    pub(crate) fn debug_on(home: &Path, id: &str) -> Vec<EnvGuard> {
        vec![
            EnvGuard::remove("ACTUAL_CONFIG"),
            EnvGuard::set("ACTUAL_CONFIG_DIR", home.to_str().unwrap()),
            EnvGuard::set(ENV, "1"),
            EnvGuard::set(ID_ENV, id),
        ]
    }

    /// Clears the id this process chose, now and on drop, so a test of
    /// [`use_session`] names its own file whatever test ran before it.
    pub(crate) struct FreshId;

    impl FreshId {
        pub(crate) fn new() -> Self {
            *ID.lock().unwrap_or_else(|e| e.into_inner()) = None;
            Self
        }
    }

    impl Drop for FreshId {
        fn drop(&mut self) {
            *ID.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }

    /// The lines logged under `id`, parsed.
    pub(crate) fn logged(id: &str) -> Vec<Value> {
        std::fs::read_to_string(path_for(id).unwrap())
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn test_enabled_reads_the_variable() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        for (value, on) in [
            (None, false),
            (Some(""), false),
            (Some("0"), false),
            (Some("false"), false),
            (Some("1"), true),
            (Some("yes"), true),
        ] {
            let _env = match value {
                Some(value) => EnvGuard::set(ENV, value),
                None => EnvGuard::remove(ENV),
            };
            assert_eq!(enabled(), on, "{value:?}");
        }
    }

    #[test]
    fn test_nothing_is_written_when_debug_mode_is_off() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _guards = debug_on(home.path(), "off-test");
        let _off = EnvGuard::remove(ENV);
        record(Entry::new(Stream::Otlp, Stage::Queued));
        assert!(!home.path().join(DIR).exists());
    }

    #[test]
    fn test_a_session_id_names_the_file_and_an_unsafe_one_is_hashed() {
        assert_eq!(
            session_id_to_debug_id("6d1f0c2e-1a2b-4c3d-9e8f-001122334455"),
            "6d1f0c2e-1a2b-4c3d-9e8f-001122334455"
        );
        let hashed = session_id_to_debug_id("../../etc/passwd");
        assert_eq!(hashed.len(), 16);
        assert!(is_valid_id(&hashed));
        assert!(!is_valid_id(""));
        assert!(!is_valid_id("a/b"));
        assert!(!is_valid_id(&"a".repeat(129)));
    }

    #[test]
    fn test_the_inherited_id_wins_and_an_unsafe_one_is_ignored() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _id = EnvGuard::set(ID_ENV, "parent-id");
        assert_eq!(id(), "parent-id");
        let _bad = EnvGuard::set(ID_ENV, "../escape");
        assert_ne!(id(), "../escape");
    }

    #[test]
    fn test_the_child_gets_the_id_only_in_debug_mode() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _id = EnvGuard::set(ID_ENV, "parent-id");
        let _off = EnvGuard::remove(ENV);
        assert_eq!(child_env(), None);
        let _on = EnvGuard::set(ENV, "1");
        assert_eq!(child_env(), Some((ID_ENV, "parent-id".to_string())));
    }

    #[test]
    fn test_the_announcement_shows_the_id_and_the_path() {
        let line = announcement(
            "7f3c2a91",
            Path::new("/var/x/telemetry-debug/7f3c2a91.jsonl"),
        );
        assert_eq!(
            line,
            "telemetry debug: 7f3c2a91 -> /var/x/telemetry-debug/7f3c2a91.jsonl"
        );
        let path = dirs::home_dir()
            .unwrap()
            .join(".actualai/actual/telemetry-debug/abc.jsonl");
        assert_eq!(
            announcement("abc", &path),
            "telemetry debug: abc -> ~/.actualai/actual/telemetry-debug/abc.jsonl"
        );
    }

    #[test]
    fn test_a_line_carries_its_fields() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _guards = debug_on(home.path(), "fields");
        record(
            Entry::new(Stream::Otlp, Stage::Refused)
                .event(json!({"k": 1}))
                .status(400)
                .to("http://127.0.0.1:9/v1/logs"),
        );
        config_unreadable(Stream::Governance);
        let lines = logged("fields");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["debug_id"], "fields");
        assert_eq!(lines[0]["stream"], "otlp");
        assert_eq!(lines[0]["stage"], "refused");
        assert_eq!(lines[0]["auth"], "signed_out");
        assert_eq!(lines[0]["sent"], true);
        assert_eq!(lines[0]["status"], 400);
        assert_eq!(lines[0]["to"], "http://127.0.0.1:9/v1/logs");
        assert_eq!(lines[0]["event"], json!({"k": 1}));
        assert!(lines[0].get("local_only").is_none());
        assert!(lines[0].get("authorization").is_none());
        assert_eq!(lines[1]["stream"], "governance");
        assert_eq!(lines[1]["stage"], "not_sent");
        assert_eq!(lines[1]["sent"], false);
        assert_eq!(lines[1]["reason"], "config_unreadable");
        assert!(lines[1].get("event").is_none());
    }

    #[test]
    fn test_spool_entries_strip_the_stamp_and_read_the_sign_in() {
        let lines = vec![
            r#"{"eventName":"a","actualSignIn":"stamp"}"#.to_string(),
            r#"{"eventName":"b"}"#.to_string(),
            "not json".to_string(),
        ];
        let entries = spool_entries(Stream::Otlp, Stage::Queued, &lines);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].signed_in);
        assert_eq!(entries[0].event, Some(json!({"eventName": "a"})));
        assert!(!entries[1].signed_in);

        let governance = vec![
            r#"{"event":"x","properties":{"user_id_hash":"h"}}"#.to_string(),
            r#"{"event":"x","properties":{"org_id":"fallback"}}"#.to_string(),
        ];
        let entries = spool_entries(Stream::Governance, Stage::Queued, &governance);
        assert!(entries[0].signed_in);
        assert!(!entries[1].signed_in);
    }

    /// A log that can't be written fails nobody, and nothing is announced.
    #[test]
    fn test_a_write_that_fails_is_dropped() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _guards = debug_on(home.path(), "unwritable");
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", "relative/dir");
        record(Entry::new(Stream::Otlp, Stage::Queued));
        record_all(vec![Entry::new(Stream::Otlp, Stage::Queued)]);
        record_all(Vec::new());
        assert!(!home.path().join(DIR).exists());
    }

    /// Lines the spool turned away are dropped, and nothing is held back
    /// outside debug mode.
    #[test]
    fn test_spooled_and_held_back_lines() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _guards = debug_on(home.path(), "spooled");
        let lines = vec![r#"{"eventName":"a"}"#.to_string()];
        spooled(Stream::Otlp, &lines, false);
        assert_eq!(logged("spooled")[0]["stage"], "dropped");

        let _off = EnvGuard::remove(ENV);
        spooled(Stream::Otlp, &lines, true);
        held_back(Stream::Otlp, &lines, Reason::TelemetryOff);
        let _on = EnvGuard::set(ENV, "1");
        assert_eq!(logged("spooled").len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn test_the_log_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _guards = debug_on(home.path(), "modes");
        record(Entry::new(Stream::Metrics, Stage::Sent));
        let dir = home.path().join(DIR);
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("modes.jsonl")), 0o600);
    }

    #[cfg(feature = "telemetry")]
    #[test]
    fn test_local_only_names_the_stored_sign_in_on_signed_in_lines_only() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _guards = debug_on(home.path(), "local");
        let creds = crate::telemetry::sign_in::tests::signed_in(
            "org-9",
            "tok-SECRET-123",
            chrono::Duration::hours(1),
            None,
        );
        crate::auth::store::save(&creds).unwrap();
        record_all(vec![
            Entry::new(Stream::Otlp, Stage::Sent)
                .signed_in(true)
                .bearer(true),
            Entry::new(Stream::Otlp, Stage::Sent),
        ]);
        let lines = logged("local");
        assert_eq!(
            lines[0]["local_only"],
            json!({"account": "dev@example.com", "org": "org-9"})
        );
        assert_eq!(lines[0]["authorization"], BEARER);
        assert!(lines[1].get("local_only").is_none());
        let raw = std::fs::read_to_string(path_for("local").unwrap()).unwrap();
        assert!(!raw.contains("tok-SECRET-123"));
    }
}
