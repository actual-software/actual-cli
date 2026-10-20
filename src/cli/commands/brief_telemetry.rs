//! The OpenTelemetry event `rules brief --claude-hook` sends when an ADR
//! first enters a session's context: one [`otlp::CONTEXT_ADDED`] record per
//! logical ADR.
//!
//! # Design
//!
//! **A logical ADR, keyed the way Actual's platform keys it.** Each rule file
//! is one policy with its own `adr_id`, and the files of one decision share a
//! `candidate_group_id`. The key is the group, else the file's own `adr_id`,
//! and a record lists the `adr_id` of every file behind it. A file without a
//! v2 header has neither id, so it sends nothing.
//!
//! **Once per session.** Keys go into the session's brief ledger, the record
//! that subagents share and compaction never resets, and a record is built
//! only for a key that write added. A missed lock or a failed write therefore
//! undercounts, and nothing is sent twice.
//!
//! **Nothing until configured.** With telemetry opted out or no output set,
//! [`report`] returns before it reads an id, writes the ledger or spawns
//! anything.

use std::path::Path;

use crate::cli::commands::brief_ledger::{self, LedgerKey};
use crate::rules::types::RuleDocument;
use crate::telemetry::otlp;

/// One logical ADR among the documents a brief recorded as briefed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BriefedAdr {
    /// The candidate group, else the one file's `adr_id`.
    key: String,
    candidate_group_id: Option<String>,
    /// The `adr_id` of each of its files, in brief order.
    adr_ids: Vec<String>,
}

/// `documents` grouped into logical ADRs, in the order the brief ranked them.
fn logical_adrs<'a>(documents: impl IntoIterator<Item = &'a RuleDocument>) -> Vec<BriefedAdr> {
    let mut adrs: Vec<BriefedAdr> = Vec::new();
    for meta in documents
        .into_iter()
        .filter_map(|document| document.meta.as_ref())
    {
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
                candidate_group_id: group.map(str::to_string),
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
    root: &Path,
    documents: impl IntoIterator<Item = &'a RuleDocument>,
) {
    let config = crate::config::paths::load().unwrap_or_default();
    if otlp::Outputs::resolve(&config).is_none() {
        return;
    }
    let adrs = logical_adrs(documents);
    if adrs.is_empty() {
        return;
    }
    let added = brief_ledger::report_adrs(ledger_dir, key, adrs.iter().map(|adr| adr.key.as_str()));
    if added.is_empty() {
        return;
    }
    let repo_url = crate::analysis::cache::get_git_remote_origin_url(root).unwrap_or_default();
    let origin = otlp::Origin::resolve(&config, &repo_url);
    let records = adrs
        .iter()
        .filter(|adr| added.contains(&adr.key))
        .map(|adr| {
            origin.context_added(
                key.session_id,
                agent_id,
                adr.candidate_group_id.as_deref(),
                &adr.adr_ids,
            )
        })
        .collect();
    dispatch(records);
}

/// Spool the records and start the uploader, waiting on neither. The
/// `#[cfg(test)]` twin captures them instead, so unit tests neither write the
/// real spool nor spawn a process; the subprocess integration test runs this.
#[cfg(not(test))]
fn dispatch(records: Vec<otlp::LogRecord>) {
    otlp::spool(&records);
    super::rules_scope::spawn_uploader();
}

#[cfg(test)]
fn dispatch(records: Vec<otlp::LogRecord>) {
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

    /// A record's string attribute, or its `ai.actual.adr.ids` as a list.
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

    fn adr(key: &str, group: Option<&str>, adr_ids: &[&str]) -> BriefedAdr {
        BriefedAdr {
            key: key.to_string(),
            candidate_group_id: group.map(str::to_string),
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
                adr("grp-1", Some("grp-1"), &["adr-1", "adr-2"]),
                adr("adr-9", None, &["adr-9"]),
                adr("adr-4", None, &["adr-4"]),
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

            report(
                &ledger,
                &ledger_key("s1"),
                Some("agent-1"),
                home,
                &documents,
            );
            let records = take_captured();
            assert_eq!(records.len(), 2);
            assert_eq!(attribute(&records[0], "session.id"), "s1");
            assert_eq!(attribute(&records[0], "ai.actual.agent.id"), "agent-1");
            assert_eq!(
                attribute(&records[0], "ai.actual.adr.candidate_group_id"),
                "grp-1"
            );
            assert_eq!(
                attribute(&records[0], "ai.actual.adr.ids"),
                serde_json::json!(["adr-1", "adr-2"])
            );
            assert_eq!(
                attribute(&records[1], "ai.actual.adr.candidate_group_id"),
                ""
            );
            assert_eq!(
                attribute(&records[1], "ai.actual.adr.ids"),
                serde_json::json!(["adr-9"])
            );

            report(&ledger, &ledger_key("s1"), None, home, &documents[..1]);
            assert!(take_captured().is_empty(), "grp-1 was already sent");

            let later = [
                document(Some("adr-9"), None),
                document(Some("adr-5"), Some("grp-5")),
            ];
            report(&ledger, &ledger_key("s1"), None, home, &later);
            let records = take_captured();
            assert_eq!(records.len(), 1);
            assert_eq!(
                attribute(&records[0], "ai.actual.adr.candidate_group_id"),
                "grp-5"
            );
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
                home,
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
                home,
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
                home,
                &[document(Some("adr-1"), None)],
            );
            assert!(take_captured().is_empty());
            assert!(!ledger.exists());
            assert!(!home.join("telemetry-id").exists());
        });
    }

    /// Rule files without a v2 header have no id to send, so they send
    /// nothing and leave the ledger alone.
    #[test]
    fn test_report_on_v1_files_does_nothing() {
        with_config(WITH_FILE, |home| {
            let ledger = home.join("brief-ledger");
            let v1 = RuleDocument::empty(Path::new("/repo/.actual/rules/v1.md"));

            report(&ledger, &ledger_key("s1"), None, home, [&v1]);

            assert!(take_captured().is_empty());
            assert!(!ledger.exists());
        });
    }
}
