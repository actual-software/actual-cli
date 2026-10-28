//! The OpenTelemetry event `rules brief --claude-hook` sends when an ADR
//! first enters a session's context: one [`otlp::ADR_DETECTED`] record per
//! logical ADR.
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
//! **Nothing until configured.** With telemetry opted out or no output set,
//! [`report`] returns before it reads an id, writes the ledger or spawns
//! anything.
//!
//! `check-override`'s event and the gates' events group ADRs with
//! [`logical_adrs`] or [`group_headers`] and send with [`dispatch`] too, so
//! every event counts an ADR the same way.

use std::path::Path;

use crate::cli::commands::brief_ledger::{self, LedgerKey};
use crate::rules::types::{AdrMeta, RuleDocument};
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

/// Report the logical ADRs among `documents` that are new to this session.
/// Best-effort and silent, like the brief itself.
pub(super) fn report<'a>(
    ledger_dir: &Path,
    key: &LedgerKey<'_>,
    agent_id: Option<&str>,
    documents: impl IntoIterator<Item = &'a RuleDocument>,
) {
    let Some(config) = crate::telemetry::opt_out::load_if_enabled() else {
        return;
    };
    let Some(outputs) = otlp::Outputs::resolve(&config) else {
        return;
    };
    let adrs = logical_adrs(documents);
    if adrs.is_empty() {
        return;
    }
    let added = brief_ledger::report_adrs(ledger_dir, key, adrs.iter().map(|adr| adr.key.as_str()));
    if added.is_empty() {
        return;
    }
    let origin = otlp::Origin::resolve();
    let records = adrs
        .iter()
        .filter(|adr| added.contains(&adr.key))
        .map(|adr| origin.context_added(key.session_id, agent_id, &adr.key, &adr.adr_ids))
        .collect();
    dispatch(&outputs, records);
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

    /// A record's string attribute, or a list attribute as a list.
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
            None => value["stringValue"].clone(),
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
            let records = take_captured();
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
            let records = take_captured();
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

    /// The positive control: a config that loads sends the ADR.
    #[test]
    fn test_report_sends_when_the_config_loads() {
        assert_eq!(
            report_one_adr_under(crate::testutil::LOADABLE_TELEMETRY_ON),
            (1, true)
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
}
