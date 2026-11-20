//! The OpenTelemetry event `rules brief --claude-hook` sends when an ADR
//! first enters a session's context: one [`otlp::ADR_DETECTED`] record per
//! logical ADR, after one [`otlp::FILTER_EVALUATED`] record accounting for
//! the selection that brief made.
//!
//! # Design
//!
//! **A logical ADR, keyed the way Actual's platform keys it.** Each rule file
//! is one policy with its own `adr_id`, and the files of one decision share a
//! `candidate_group_id`. The key is the group, else the file's own `adr_id`.
//! A record names the ADR by that key in `actual.adr.ref` and lists the
//! `adr_id` of every file behind it that the brief carried a rule from. A file
//! without a v2 header has neither id, so it sends nothing.
//!
//! **Once per session.** Keys go into the session's brief ledger, the record
//! that subagents share and compaction never resets, and a record is built
//! only for a key that write added. A missed lock or a failed write therefore
//! undercounts, and nothing is sent twice.
//!
//! **Nothing without an output.** With telemetry opted out, or no output
//! resolved (a release build has its default), [`report`] returns before it
//! reads an id, writes the ledger or spawns anything. Telemetry debug mode
//! is the exception: with telemetry off it builds the records against the
//! ledger as it stands, logs them as not sent, and writes nothing.
//!
//! `check-override`'s event and the gates' events group ADRs with
//! [`logical_adrs`] or [`group_headers`] and send with [`dispatch`] too, so
//! every event counts an ADR the same way.

use std::path::Path;

use crate::cli::commands::brief_ledger::{self, LedgerKey};
use crate::rules::scope::FilterTally;
use crate::rules::types::{AdrMeta, RuleDocument};
use crate::telemetry::debug::Stream;
use crate::telemetry::opt_out::{self, Gate};
use crate::telemetry::otlp;

/// One logical ADR among the documents a brief carried rules from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BriefedAdr {
    /// The candidate group, else the one file's `adr_id`.
    pub(super) key: String,
    /// The `adr_id` of each of its files, in brief order.
    pub(super) adr_ids: Vec<String>,
}

/// `documents` grouped into logical ADRs, in the order the brief ranked them.
pub(super) fn logical_adrs<'a>(
    documents: impl IntoIterator<Item = &'a RuleDocument>,
) -> Vec<BriefedAdr> {
    group_headers(
        documents
            .into_iter()
            .filter_map(|document| document.meta.as_ref()),
    )
}

/// Rule files' v2 headers grouped into logical ADRs, in order.
pub(super) fn group_headers<'a>(headers: impl IntoIterator<Item = &'a AdrMeta>) -> Vec<BriefedAdr> {
    let mut adrs: Vec<BriefedAdr> = Vec::new();
    for meta in headers {
        let Some(adr_id) = meta.adr_id.as_deref().filter(|id| !id.is_empty()) else {
            continue;
        };
        let group = meta
            .candidate_group_id
            .as_deref()
            .filter(|id| !id.is_empty());
        let key = group.unwrap_or(adr_id);
        if let Some(adr) = adrs.iter_mut().find(|adr| adr.key == key) {
            if !adr.adr_ids.iter().any(|id| id == adr_id) {
                adr.adr_ids.push(adr_id.to_string());
            }
        } else {
            adrs.push(BriefedAdr {
                key: key.to_string(),
                adr_ids: vec![adr_id.to_string()],
            });
        }
    }
    adrs
}

/// Report the logical ADRs among `documents` that are new to this session,
/// after the brief's [`otlp::FILTER_EVALUATED`] record for the selection
/// `filter` accounts for. A brief that adds no ADR sends neither.
/// Best-effort and silent, like the brief itself.
pub(super) fn report<'a>(
    ledger_dir: &Path,
    key: &LedgerKey<'_>,
    agent_id: Option<&str>,
    documents: impl IntoIterator<Item = &'a RuleDocument>,
    filter: &FilterTally,
) {
    let Some(destination) = Destination::resolve() else {
        return;
    };
    let adrs = logical_adrs(documents);
    if adrs.is_empty() {
        return;
    }
    let keys = adrs.iter().map(|adr| adr.key.as_str());
    let added = if destination.sends() {
        brief_ledger::report_adrs(ledger_dir, key, keys)
    } else {
        brief_ledger::unreported_adrs(ledger_dir, key, keys)
    };
    if added.is_empty() {
        return;
    }
    let origin = otlp::Origin::resolve();
    let filtered = origin.filter_evaluated(key.session_id, agent_id, "context", filter);
    let records = std::iter::once(filtered)
        .chain(
            adrs.iter()
                .filter(|adr| added.contains(&adr.key))
                .map(|adr| origin.context_added(key.session_id, agent_id, &adr.key, &adr.adr_ids)),
        )
        .collect();
    destination.deliver(records);
}

/// Where an OTLP sender's records go this run: sent to `outputs`, or, in
/// telemetry debug mode with telemetry off, built in the form they would be
/// sent in and logged as not sent.
pub(super) struct Destination {
    outputs: otlp::Outputs,
    send: bool,
}

impl Destination {
    /// This run's destination, or `None` when the sender should build
    /// nothing. In debug mode, a config file that won't load is logged.
    pub(super) fn resolve() -> Option<Self> {
        Self::from_gate(opt_out::gate(Stream::Otlp))
    }

    /// [`Destination::resolve`] without the log line, for a step within a
    /// sender that resolves its own.
    pub(super) fn resolve_quietly() -> Option<Self> {
        Self::from_gate(opt_out::quiet_gate())
    }

    fn from_gate(gate: Gate) -> Option<Self> {
        match gate {
            Gate::Send(config) => otlp::Outputs::resolve(&config).map(|outputs| Self {
                outputs,
                send: true,
            }),
            Gate::DebugOnly(config) => {
                otlp::Outputs::resolve_ignoring_opt_out(&config).map(|outputs| Self {
                    outputs,
                    send: false,
                })
            }
            Gate::Off => None,
        }
    }

    /// Whether the records are sent, so the sender may keep the state that
    /// sending them keeps.
    pub(super) fn sends(&self) -> bool {
        self.send
    }

    /// Send `records`, or log them as not sent.
    pub(super) fn deliver(&self, records: Vec<otlp::LogRecord>) {
        if self.send {
            dispatch(&self.outputs, records);
        } else {
            otlp::hold_back(&self.outputs, &records);
        }
    }
}

/// Spool the records for `outputs` and start the uploader, waiting on neither.
/// The `#[cfg(test)]` twin captures them instead, so unit tests neither write
/// the real spool nor spawn a process; the subprocess integration test runs
/// this.
#[cfg(not(test))]
pub(super) fn dispatch(outputs: &otlp::Outputs, records: Vec<otlp::LogRecord>) {
    otlp::spool(outputs, &records);
    super::rules_scope::spawn_uploader();
}

#[cfg(test)]
pub(super) fn dispatch(_outputs: &otlp::Outputs, records: Vec<otlp::LogRecord>) {
    tests::CAPTURED.with(|cell| cell.borrow_mut().extend(records));
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    use std::cell::RefCell;

    use crate::rules::types::AdrMeta;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use tempfile::tempdir;

    thread_local! {
        pub(in crate::cli::commands) static CAPTURED: RefCell<Vec<otlp::LogRecord>> =
            const { RefCell::new(Vec::new()) };
    }

    /// The records reported on this thread since the last call, as JSON.
    pub(in crate::cli::commands) fn take_captured() -> Vec<serde_json::Value> {
        CAPTURED.with(|cell| {
            cell.borrow_mut()
                .drain(..)
                .map(|record| serde_json::to_value(record).unwrap())
                .collect()
        })
    }

    /// A record's string or integer attribute (an OTLP integer is a JSON
    /// string), or a list attribute as a list.
    pub(in crate::cli::commands) fn attribute(
        record: &serde_json::Value,
        key: &str,
    ) -> serde_json::Value {
        let value = record["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|kv| kv["key"] == key)
            .unwrap_or_else(|| panic!("no {key} in {record}"))["value"]
            .clone();
        match value.get("arrayValue") {
            Some(array) => array["values"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["stringValue"].clone())
                .collect(),
            None => value
                .get("stringValue")
                .or_else(|| value.get("intValue"))
                .cloned()
                .unwrap_or_default(),
        }
    }

    fn document(adr_id: Option<&str>, group: Option<&str>) -> RuleDocument {
        let mut document = RuleDocument::empty(Path::new("/repo/.actual/rules/a.md"));
        document.meta = Some(AdrMeta {
            adr_id: adr_id.map(str::to_string),
            candidate_group_id: group.map(str::to_string),
            ..Default::default()
        });
        document
    }

    fn adr(key: &str, adr_ids: &[&str]) -> BriefedAdr {
        BriefedAdr {
            key: key.to_string(),
            adr_ids: adr_ids.iter().map(|id| id.to_string()).collect(),
        }
    }

    /// Files sharing a group are one ADR listing each file's id; a file with
    /// no group is its own; a file with no `adr_id`, or no header, is none.
    #[test]
    fn test_logical_adrs_group_files_the_way_the_platform_does() {
        let v1 = RuleDocument::empty(Path::new("/repo/.actual/rules/v1.md"));
        let documents = [
            document(Some("adr-1"), Some("grp-1")),
            document(Some("adr-9"), None),
            v1,
            document(None, Some("grp-2")),
            document(Some("adr-2"), Some("grp-1")),
            document(Some("adr-2"), Some("grp-1")),
            document(Some(""), Some("grp-3")),
            document(Some("adr-4"), Some("")),
        ];

        assert_eq!(
            logical_adrs(&documents),
            [
                adr("grp-1", &["adr-1", "adr-2"]),
                adr("adr-9", &["adr-9"]),
                adr("adr-4", &["adr-4"]),
            ]
        );
    }

    /// [`super::report`] for a brief whose selection kept `documents`.
    fn report<'a>(
        ledger_dir: &Path,
        key: &LedgerKey<'_>,
        agent_id: Option<&str>,
        documents: impl IntoIterator<Item = &'a RuleDocument>,
    ) {
        super::report(
            ledger_dir,
            key,
            agent_id,
            documents,
            &FilterTally::default(),
        );
    }

    /// The records after the brief's filter record, which leads any it sends.
    fn detected(mut records: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
        assert_eq!(records[0]["eventName"], otlp::FILTER_EVALUATED);
        records.remove(0);
        records
    }

    fn ledger_key(session_id: &str) -> LedgerKey<'_> {
        LedgerKey {
            session_id,
            rules_dir: Path::new("/repo/.actual/rules"),
        }
    }

    /// Run `body` against a scratch config dir whose config sets `config`.
    fn with_config(config: &str, body: impl FnOnce(&Path)) {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _file = EnvGuard::remove("ACTUAL_CONFIG");
        let _guards: Vec<EnvGuard> = [
            "ACTUAL_NO_TELEMETRY",
            "ACTUAL_OTLP_LOGS_ENDPOINT",
            "ACTUAL_OTLP_FILE",
            "TRACEPARENT",
            crate::telemetry::debug::ENV,
            crate::telemetry::debug::ID_ENV,
        ]
        .into_iter()
        .map(EnvGuard::remove)
        .collect();
        std::fs::write(home.path().join("config.yaml"), config).unwrap();
        take_captured();
        body(home.path());
    }

    const WITH_FILE: &str = "telemetry:\n  otlp_file: /tmp/actual-otlp-test.jsonl\n";

    #[test]
    fn test_report_sends_each_new_adr_once() {
        with_config(WITH_FILE, |home| {
            let ledger = home.join("brief-ledger");
            let documents = [
                document(Some("adr-1"), Some("grp-1")),
                document(Some("adr-2"), Some("grp-1")),
                document(Some("adr-9"), None),
            ];

            report(&ledger, &ledger_key("s1"), Some("agent-1"), &documents);
            let records = detected(take_captured());
            assert_eq!(records.len(), 2);
            assert!(records.iter().all(|r| r["eventName"] == otlp::ADR_DETECTED));
            assert_eq!(attribute(&records[0], "actual.session.id"), "s1");
            assert_eq!(attribute(&records[0], "actual.agent.id"), "agent-1");
            assert_eq!(attribute(&records[0], "actual.detection.method"), "context");
            assert_eq!(attribute(&records[0], "actual.adr.ref"), "grp-1");
            assert_eq!(
                attribute(&records[0], "actual.adr.ids"),
                serde_json::json!(["adr-1", "adr-2"])
            );
            assert_eq!(attribute(&records[1], "actual.adr.ref"), "adr-9");
            assert_eq!(
                attribute(&records[1], "actual.adr.ids"),
                serde_json::json!(["adr-9"])
            );
            assert!(
                !home.join("telemetry-id").exists(),
                "a record reads no install id"
            );

            report(&ledger, &ledger_key("s1"), None, &documents[..1]);
            assert!(take_captured().is_empty(), "grp-1 was already sent");

            let later = [
                document(Some("adr-9"), None),
                document(Some("adr-5"), Some("grp-5")),
            ];
            report(&ledger, &ledger_key("s1"), None, &later);
            let records = detected(take_captured());
            assert_eq!(records.len(), 1);
            assert_eq!(attribute(&records[0], "actual.adr.ref"), "grp-5");
        });
    }

    /// No output set: nothing is built, and the ledger is never written.
    #[test]
    fn test_report_without_an_output_does_nothing() {
        with_config("", |home| {
            let ledger = home.join("brief-ledger");

            report(
                &ledger,
                &ledger_key("s1"),
                None,
                &[document(Some("adr-1"), None)],
            );

            assert!(take_captured().is_empty());
            assert!(!ledger.exists());
            assert!(!home.join("telemetry-id").exists());
        });
    }

    /// Opted out by config or env: nothing is built or written, and the
    /// install id file is never created.
    #[test]
    fn test_report_when_opted_out_does_nothing() {
        let opted_out = format!("{WITH_FILE}  enabled: false\n");
        with_config(&opted_out, |home| {
            let ledger = home.join("brief-ledger");
            report(
                &ledger,
                &ledger_key("s1"),
                None,
                &[document(Some("adr-1"), None)],
            );
            assert!(take_captured().is_empty());
            assert!(!ledger.exists());
            assert!(!home.join("telemetry-id").exists());
        });
        with_config(WITH_FILE, |home| {
            let _no = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
            let ledger = home.join("brief-ledger");
            report(
                &ledger,
                &ledger_key("s1"),
                None,
                &[document(Some("adr-1"), None)],
            );
            assert!(take_captured().is_empty());
            assert!(!ledger.exists());
            assert!(!home.join("telemetry-id").exists());
        });
    }

    /// How many records `report` builds for one new ADR with `config` in the
    /// config file, and whether it wrote the ledger. `ACTUAL_OTLP_FILE` names
    /// an output, so the config alone decides whether anything is sent.
    fn report_one_adr_under(config: &str) -> (usize, bool) {
        let mut outcome = (0, false);
        with_config(config, |home| {
            let sink = home.join("otlp.jsonl");
            let _file = EnvGuard::set("ACTUAL_OTLP_FILE", sink.to_str().unwrap());
            let ledger = home.join("brief-ledger");
            report(
                &ledger,
                &ledger_key("s1"),
                None,
                &[document(Some("adr-1"), None)],
            );
            outcome = (take_captured().len(), ledger.exists());
        });
        outcome
    }

    /// `telemetry.enabled: false` in a config file that doesn't load: nothing
    /// is built, and the ledger is never written.
    #[test]
    fn test_report_sends_nothing_when_the_config_cannot_load() {
        assert_eq!(
            report_one_adr_under(crate::testutil::UNLOADABLE_TELEMETRY_OPT_OUT),
            (0, false)
        );
    }

    /// The positive control: a config that loads sends the selection and
    /// the ADR.
    #[test]
    fn test_report_sends_when_the_config_loads() {
        assert_eq!(
            report_one_adr_under(crate::testutil::LOADABLE_TELEMETRY_ON),
            (2, true)
        );
    }

    /// Rule files without a v2 header have no id to send, so they send
    /// nothing and leave the ledger alone.
    #[test]
    fn test_report_on_v1_files_does_nothing() {
        with_config(WITH_FILE, |home| {
            let ledger = home.join("brief-ledger");
            let v1 = RuleDocument::empty(Path::new("/repo/.actual/rules/v1.md"));

            report(&ledger, &ledger_key("s1"), None, [&v1]);

            assert!(take_captured().is_empty());
            assert!(!ledger.exists());
        });
    }

    // ── telemetry debug mode ────────────────────────────────────────────

    /// Telemetry off in debug mode: the record is logged as not sent in the
    /// form the brief would send, nothing is dispatched, and the ledger is
    /// left as it was, so turning telemetry on still sends the ADR. The
    /// record sent then has the logged one's shape.
    #[test]
    fn test_debug_mode_with_telemetry_off_logs_the_record_and_writes_nothing() {
        use crate::telemetry::debug::{self, tests::logged};
        let off = "telemetry:\n  enabled: false\n  otlp_file: /tmp/actual-otlp-test.jsonl\n";
        with_config(off, |home| {
            let _debug = EnvGuard::set(debug::ENV, "1");
            let _id = EnvGuard::set(debug::ID_ENV, "brief-off");
            let ledger = home.join("brief-ledger");
            let documents = [document(Some("adr-9"), None)];

            report(&ledger, &ledger_key("s1"), None, &documents);

            assert!(take_captured().is_empty());
            assert!(brief_ledger::load(&ledger, &ledger_key("s1"))
                .reported_adrs
                .is_empty());
            let lines = logged("brief-off");
            assert_eq!(lines.len(), 2);
            assert!(lines.iter().all(|line| line["stage"] == "not_sent"
                && line["reason"] == "telemetry_off"
                && line["auth"] == "signed_out"));
            assert_eq!(lines[0]["event"]["eventName"], otlp::FILTER_EVALUATED);
            let held = &lines[1]["event"];
            assert_eq!(held["eventName"], otlp::ADR_DETECTED);
            assert_eq!(attribute(held, "actual.adr.ref"), "adr-9");

            std::fs::write(home.join("config.yaml"), WITH_FILE).unwrap();
            report(&ledger, &ledger_key("s1"), None, &documents);
            let sent = detected(take_captured());
            assert_eq!(sent.len(), 1);
            let keys = |record: &serde_json::Value| -> Vec<serde_json::Value> {
                record["attributes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|kv| kv["key"].clone())
                    .collect()
            };
            assert_eq!(keys(held), keys(&sent[0]));
        });
    }

    /// A config file that won't load sends nothing in debug mode either; the
    /// log says why, with no event. The output is named in the environment so
    /// the file's loadability alone decides.
    #[test]
    fn test_debug_mode_logs_an_unloadable_config_and_sends_nothing() {
        use crate::telemetry::debug::{self, tests::logged};
        with_config(crate::testutil::UNLOADABLE_TELEMETRY_OPT_OUT, |home| {
            let _debug = EnvGuard::set(debug::ENV, "1");
            let _id = EnvGuard::set(debug::ID_ENV, "brief-unloadable");
            let _out = EnvGuard::set("ACTUAL_OTLP_FILE", "/tmp/actual-otlp-test.jsonl");
            let ledger = home.join("brief-ledger");

            report(
                &ledger,
                &ledger_key("s1"),
                None,
                &[document(Some("adr-9"), None)],
            );

            assert!(take_captured().is_empty());
            let lines = logged("brief-unloadable");
            assert_eq!(lines.len(), 1);
            assert_eq!(lines[0]["stream"], "otlp");
            assert_eq!(lines[0]["reason"], "config_unreadable");
            assert!(lines[0].get("event").is_none());
        });
    }
}
