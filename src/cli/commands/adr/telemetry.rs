//! The OpenTelemetry events the `actual adr` commands send: one
//! [`otlp::ADR_DISCOVERED`] record per `adr check` run, one
//! [`otlp::ADR_REVIEWED`] record per run of `accept`, `reject`, `publish`,
//! `unpublish`, `delete`, `restore` or `update`, and one
//! [`otlp::ADR_CREATED`] record per run of `adr new` or `adr create`.
//!
//! # Design
//!
//! **One event per run.** Each review command takes one decision id, so a
//! review event is one decision. An AI-grouped decision acts on its rows
//! through one group action, so the event says it was grouped and how many
//! rows that action covers. A check event counts the files it checked. A
//! create event is the one decision `create` wrote signed out or saved
//! signed in.
//!
//! **Signed in or not, by the sender.** The records are spooled like every
//! other OTLP record: under a sign-in they go with its token, and otherwise
//! without it and without [`otlp`]'s signed-in-only attributes, the decision
//! id among them. A review event names its pathway: `local` for `publish`,
//! `update` and `delete` on the files under `docs/adr/`, and for the
//! commands that refuse there with outcome `not_signed_in`, or `cloud` for a
//! run on Actual AI's decisions.
//!
//! **Counts and outcomes, never content.** No title, context, policy, scope,
//! file path or `reject --reason` text is sent. A create event counts the
//! decision's policies.
//!
//! **Nothing without an output.** With telemetry opted out, or no output
//! resolved, a report returns before it builds anything.
//!
//! **Nothing without the feature.** A build without the `telemetry` feature
//! has none of the events, and the reports are no-ops that take the same
//! calls.

use std::time::Instant;

#[cfg(feature = "telemetry")]
use super::super::brief_telemetry;
use crate::adr::files::DecisionFile;
use crate::adr::workflow::{self, Listed};
use crate::api::adr_types::{DecisionState, LifecycleAction, WebDecision};
use crate::error::ActualError;
#[cfg(feature = "telemetry")]
pub(super) use crate::telemetry::otlp::CheckCounts;
#[cfg(feature = "telemetry")]
use crate::telemetry::otlp::{self, CreatedDecision, ReviewedDecision};

/// A command run being timed, and whether a person was at the terminal.
#[cfg_attr(not(feature = "telemetry"), allow(dead_code))]
pub(super) struct Run {
    started: Instant,
    interactive: bool,
}

impl Run {
    pub(super) fn start() -> Self {
        Self {
            started: Instant::now(),
            interactive: super::interactive(),
        }
    }

    #[cfg(feature = "telemetry")]
    fn ended<'a>(&self, outcome: &'a str, exit_code: i32) -> otlp::AdrRun<'a> {
        otlp::AdrRun {
            outcome,
            exit_code,
            interactive: self.interactive,
            duration: self.started.elapsed(),
        }
    }
}

/// The pathway of a review run on the files under `docs/adr/`.
pub(super) const LOCAL: &str = "local";
/// The pathway of a review run on Actual AI's decisions.
pub(super) const CLOUD: &str = "cloud";

/// The decision a review run found, which its event names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Reviewed {
    id: String,
    grouped: bool,
    group_rows: usize,
    /// The state it was in before the run: one of [`DecisionState`]'s
    /// names, or `other` for a file whose status none of them covers.
    prior_status: &'static str,
}

impl Reviewed {
    fn of(decision: &WebDecision, state: DecisionState) -> Self {
        let grouped = workflow::is_group(decision);
        Self {
            id: decision.id.clone(),
            grouped,
            group_rows: if grouped {
                decision.member_adr_ids.len()
            } else {
                0
            },
            prior_status: state.as_str(),
        }
    }

    /// A decision Actual AI keeps, in the list it was read in.
    pub(super) fn listed(listed: &Listed) -> Self {
        Self::of(&listed.decision, listed.state)
    }

    /// A decision file under `docs/adr/`. Like a signed-out `create`, it
    /// sends no id.
    pub(super) fn file(file: &DecisionFile) -> Self {
        let meta = file.meta.clone().unwrap_or_default();
        let state = if meta.is_active() {
            Some(DecisionState::Active)
        } else {
            meta.status
                .as_deref()
                .and_then(|status| workflow::state_of(&status.trim().to_ascii_lowercase(), false))
        };
        Self {
            prior_status: state.map_or("other", DecisionState::as_str),
            ..Self::default()
        }
    }
}

/// What an `adr new` or `adr create` run wrote or saved, filled in as the
/// run learns it, so a run that stops early still reports what it knew.
#[cfg_attr(not(feature = "telemetry"), allow(dead_code))]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Creation {
    target: &'static str,
    source: &'static str,
    /// `accepted` or `draft`: as asked for, then as saved.
    pub(super) status: String,
    pub(super) policies: usize,
    /// A cloud save whose scope or publish step after it failed.
    pub(super) partial: bool,
    /// The saved decision's id; a signed-out run has none to send.
    pub(super) id: Option<String>,
}

impl Creation {
    /// A signed-out `adr create` run, which writes files from flags.
    pub(super) fn local() -> Self {
        Self {
            target: "local",
            source: "flags",
            ..Self::default()
        }
    }

    /// A signed-in `adr create` run, from `--file` or from flags.
    pub(super) fn cloud(from_file: bool) -> Self {
        Self {
            target: "cloud",
            source: if from_file { "file" } else { "flags" },
            ..Self::default()
        }
    }
}

/// The review command `action` is run by.
pub(super) fn command(action: LifecycleAction) -> &'static str {
    match action {
        LifecycleAction::Publish => "publish",
        LifecycleAction::Unpublish => "unpublish",
        LifecycleAction::Archive => "delete",
        LifecycleAction::Restore => "restore",
        LifecycleAction::Accept => "accept",
        LifecycleAction::Reject => "reject",
    }
}

/// What an `adr check` run found, which nothing reads in a build without the
/// `telemetry` feature.
#[cfg(not(feature = "telemetry"))]
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct CheckCounts {
    pub files: usize,
    pub failed: usize,
    pub duplicates: usize,
}

#[cfg(feature = "telemetry")]
fn exit_code<T>(result: &Result<T, ActualError>) -> i32 {
    result.as_ref().err().map_or(0, ActualError::exit_code)
}

/// Report one `adr check` run that found `counts`: `passed`, `failed` when a
/// file failed the save rules, or `error` when the check couldn't run.
#[cfg(feature = "telemetry")]
pub(super) fn report_check(run: &Run, counts: CheckCounts, result: &Result<(), ActualError>) {
    let outcome = match result {
        Ok(()) => "passed",
        Err(ActualError::AdrCheckFailed(_)) => "failed",
        Err(_) => "error",
    };
    send(|origin| origin.adr_discovered(counts, run.ended(outcome, exit_code(result))));
}

/// Report one run of review command `action` on `pathway` ([`LOCAL`] or
/// [`CLOUD`]) that found `reviewed`, if it got that far: `succeeded`,
/// `refused` when the decision's state or the edit turns the change down
/// before anything is written, `not_signed_in`, or `failed`.
#[cfg(feature = "telemetry")]
pub(super) fn report_review<T>(
    run: &Run,
    pathway: &str,
    action: &str,
    reviewed: Option<&Reviewed>,
    result: &Result<T, ActualError>,
) {
    let outcome = match result {
        Ok(_) => "succeeded",
        Err(ActualError::AdrNotAllowed(_)) => "refused",
        Err(ActualError::NotLoggedIn | ActualError::AdrPathway(_)) => "not_signed_in",
        Err(_) => "failed",
    };
    let decision = reviewed.map_or_else(
        || ReviewedDecision {
            pathway,
            ..ReviewedDecision::default()
        },
        |r| ReviewedDecision {
            pathway,
            id: Some(&r.id),
            prior_status: Some(r.prior_status),
            grouped: r.grouped,
            group_rows: r.group_rows,
        },
    );
    send(|origin| origin.adr_reviewed(action, decision, run.ended(outcome, exit_code(result))));
}

/// Report one `adr new` or `adr create` run that learned `creation`:
/// `succeeded`, `invalid` when the decision fails the save rules, `duplicate`
/// when `create` finds it likely already exists, `not_signed_in`, or `failed`.
#[cfg(feature = "telemetry")]
pub(super) fn report_create<T>(run: &Run, creation: &Creation, result: &Result<T, ActualError>) {
    let outcome = match result {
        Ok(_) => "succeeded",
        Err(ActualError::AdrInvalid { .. }) => "invalid",
        Err(ActualError::AdrDuplicate { .. }) => "duplicate",
        Err(ActualError::NotLoggedIn) => "not_signed_in",
        Err(_) => "failed",
    };
    let decision = CreatedDecision {
        target: creation.target,
        source: creation.source,
        status: &creation.status,
        policies: creation.policies,
        partial: creation.partial,
        id: creation.id.as_deref(),
    };
    send(|origin| origin.adr_created(decision, run.ended(outcome, exit_code(result))));
}

#[cfg(feature = "telemetry")]
fn send(build: impl FnOnce(&otlp::Origin) -> otlp::LogRecord) {
    let Some(destination) = brief_telemetry::Destination::resolve() else {
        return;
    };
    destination.deliver(vec![build(&otlp::Origin::resolve())]);
}

#[cfg(not(feature = "telemetry"))]
pub(super) fn report_check(_: &Run, _: CheckCounts, _: &Result<(), ActualError>) {}

#[cfg(not(feature = "telemetry"))]
pub(super) fn report_review<T>(
    _: &Run,
    _: &str,
    _: &str,
    _: Option<&Reviewed>,
    _: &Result<T, ActualError>,
) {
}

#[cfg(not(feature = "telemetry"))]
pub(super) fn report_create<T>(_: &Run, _: &Creation, _: &Result<T, ActualError>) {}

#[cfg(all(test, feature = "telemetry"))]
pub(super) mod tests {
    use super::*;

    use super::super::super::brief_telemetry::tests::take_captured;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use serde_json::{json, Value};
    use std::path::Path;
    use tempfile::tempdir;

    pub(in crate::cli::commands::adr) const WITH_FILE: &str =
        "telemetry:\n  otlp_file: /tmp/actual-otlp-adr-test.jsonl\n";

    /// Every attribute of `record` as `key -> value`, ints as numbers.
    pub(in crate::cli::commands::adr) fn attributes(
        record: &Value,
    ) -> serde_json::Map<String, Value> {
        record["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|kv| {
                let value = &kv["value"];
                let value = if let Some(int) = value.get("intValue") {
                    json!(int.as_str().unwrap().parse::<i64>().unwrap())
                } else if let Some(flag) = value.get("boolValue") {
                    flag.clone()
                } else {
                    value["stringValue"].clone()
                };
                (kv["key"].as_str().unwrap().to_string(), value)
            })
            .collect()
    }

    /// The attributes an `adr` event adds to the shared ones, without the
    /// duration, which varies.
    pub(in crate::cli::commands::adr) fn own(record: &Value) -> serde_json::Map<String, Value> {
        let mut attributes = attributes(record);
        attributes.retain(|key, _| {
            key.starts_with("actual.adr.") && key != "actual.adr.command.duration_ms"
        });
        attributes
    }

    /// Give the config dir `home`, which `ACTUAL_CONFIG_DIR` already names,
    /// a config that sends to a file and nothing else, and clear what was
    /// captured before. Hold the guards for the test.
    pub(in crate::cli::commands::adr) fn sending_to_a_file(home: &Path) -> Vec<EnvGuard> {
        let guards = [
            "ACTUAL_CONFIG",
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
        std::fs::write(home.join("config.yaml"), WITH_FILE).unwrap();
        take_captured();
        guards
    }

    /// Run `body` with a scratch config dir whose config is `config`.
    fn with_config(config: &str, body: impl FnOnce(&Path)) {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _guards = sending_to_a_file(home.path());
        std::fs::write(home.path().join("config.yaml"), config).unwrap();
        body(home.path());
    }

    fn run(interactive: bool) -> Run {
        Run {
            started: Instant::now(),
            interactive,
        }
    }

    fn decision(id: &str, group: Option<&str>, members: &[&str]) -> WebDecision {
        serde_json::from_value(json!({
            "id": id,
            "candidateGroupId": group,
            "memberAdrIds": members,
        }))
        .unwrap()
    }

    const COUNTS: CheckCounts = CheckCounts {
        files: 3,
        failed: 1,
        duplicates: 2,
    };

    #[test]
    fn check_reports_its_counts_and_each_outcome() {
        with_config(WITH_FILE, |_| {
            let cases: [(Result<(), ActualError>, &str, i64); 3] = [
                (Ok(()), "passed", 0),
                (
                    Err(ActualError::AdrCheckFailed("1 of 3".to_string())),
                    "failed",
                    1,
                ),
                (
                    Err(ActualError::InternalError("no runtime".to_string())),
                    "error",
                    1,
                ),
            ];
            for (result, outcome, code) in cases {
                report_check(&run(true), COUNTS, &result);
                let records = take_captured();
                assert_eq!(records.len(), 1);
                assert_eq!(records[0]["eventName"], otlp::ADR_DISCOVERED);
                assert_eq!(
                    Value::Object(own(&records[0])),
                    json!({
                        "actual.adr.check.files": 3,
                        "actual.adr.check.failed_files": 1,
                        "actual.adr.check.duplicates": 2,
                        "actual.adr.command.outcome": outcome,
                        "actual.adr.command.exit_code": code,
                        "actual.adr.command.stdin_is_terminal": true,
                    })
                );
                let all = attributes(&records[0]);
                assert!(all["actual.adr.command.duration_ms"].as_i64().unwrap() >= 0);
                assert_eq!(all["actual.session.id"], "");
                assert_eq!(all["actual.event.schema_version"], 11);
            }
        });
    }

    #[test]
    fn review_reports_each_command_and_outcome() {
        with_config(WITH_FILE, |_| {
            let person = Reviewed::listed(&Listed {
                decision: decision("d-1", Some("d-1"), &["d-1"]),
                state: DecisionState::Draft,
            });
            let actions = [
                command(LifecycleAction::Accept),
                command(LifecycleAction::Reject),
                command(LifecycleAction::Publish),
                command(LifecycleAction::Unpublish),
                command(LifecycleAction::Archive),
                command(LifecycleAction::Restore),
                "update",
            ];
            assert_eq!(
                actions,
                [
                    "accept",
                    "reject",
                    "publish",
                    "unpublish",
                    "delete",
                    "restore",
                    "update"
                ]
            );
            for action in actions {
                let cases: [(Result<(), ActualError>, &str, i64); 3] = [
                    (Ok(()), "succeeded", 0),
                    (
                        Err(ActualError::AdrNotAllowed("Only a draft".to_string())),
                        "refused",
                        3,
                    ),
                    (Err(ActualError::ServiceUnavailable), "failed", 3),
                ];
                for (result, outcome, code) in cases {
                    report_review(&run(false), CLOUD, action, Some(&person), &result);
                    let records = take_captured();
                    assert_eq!(records.len(), 1, "{action} {outcome}");
                    assert_eq!(records[0]["eventName"], otlp::ADR_REVIEWED);
                    assert_eq!(
                        Value::Object(own(&records[0])),
                        json!({
                            "actual.adr.review.action": action,
                            "actual.adr.review.pathway": "cloud",
                            "actual.adr.review.prior_status": "draft",
                            "actual.adr.decision_id": "d-1",
                            "actual.adr.review.grouped": false,
                            "actual.adr.review.group_rows": 0,
                            "actual.adr.command.outcome": outcome,
                            "actual.adr.command.exit_code": code,
                            "actual.adr.command.stdin_is_terminal": false,
                        }),
                        "{action} {outcome}"
                    );
                }
            }
        });
    }

    /// An AI-grouped decision names no row, takes the group route, and its
    /// event counts the rows that route covers.
    #[test]
    fn review_of_a_grouped_decision_counts_its_rows() {
        let grouped = Reviewed::of(
            &decision("g-1", Some("g-1"), &["m-1", "m-2", "m-3"]),
            DecisionState::Suggested,
        );
        assert_eq!(
            grouped,
            Reviewed {
                id: "g-1".to_string(),
                grouped: true,
                group_rows: 3,
                prior_status: "suggested",
            }
        );
        with_config(WITH_FILE, |_| {
            report_review(&run(true), CLOUD, "accept", Some(&grouped), &Ok(()));
            let records = take_captured();
            let own = own(&records[0]);
            assert_eq!(own["actual.adr.decision_id"], "g-1");
            assert_eq!(own["actual.adr.review.grouped"], true);
            assert_eq!(own["actual.adr.review.group_rows"], 3);
        });
    }

    /// A review that stops before it finds a decision, such as `accept`
    /// refused on the files, names its pathway and no decision.
    #[test]
    fn review_without_a_decision_names_only_its_pathway() {
        with_config(WITH_FILE, |_| {
            let result: Result<(), ActualError> =
                Err(ActualError::AdrPathway("needs a sign-in".to_string()));
            report_review(&run(false), LOCAL, "accept", None, &result);
            let records = take_captured();
            assert_eq!(
                Value::Object(own(&records[0])),
                json!({
                    "actual.adr.review.action": "accept",
                    "actual.adr.review.pathway": "local",
                    "actual.adr.review.prior_status": "",
                    "actual.adr.decision_id": "",
                    "actual.adr.review.grouped": false,
                    "actual.adr.review.group_rows": 0,
                    "actual.adr.command.outcome": "not_signed_in",
                    "actual.adr.command.exit_code": 2,
                    "actual.adr.command.stdin_is_terminal": false,
                })
            );
        });
    }

    /// A decision file's prior status is the state the CLI lists it in: a
    /// file with no status, or any status that governs, is active.
    #[test]
    fn a_decision_files_prior_status_is_its_listed_state() {
        let file = |status: Option<&str>| DecisionFile {
            path: std::path::PathBuf::from("docs/adr/d-1.md"),
            meta: Some(crate::rules::types::AdrMeta {
                adr_id: Some("d-1".to_string()),
                status: status.map(str::to_string),
                ..Default::default()
            }),
            source: None,
            body: String::new(),
        };
        let cases = [
            (None, "active"),
            (Some("accepted"), "active"),
            (Some("created-from-user"), "active"),
            (Some("Draft"), "draft"),
            (Some("suggested"), "suggested"),
            (Some(" archived-draft "), "archived"),
            (Some("rejected"), "archived"),
            (Some("deleted"), "archived"),
            (Some("deferred"), "other"),
            (Some("Superseded"), "other"),
        ];
        for (status, prior) in cases {
            assert_eq!(
                Reviewed::file(&file(status)),
                Reviewed {
                    prior_status: prior,
                    ..Reviewed::default()
                },
                "{status:?}"
            );
        }
    }

    /// Each create outcome, for a local write and for a cloud save.
    #[test]
    fn create_reports_its_target_and_each_outcome() {
        with_config(WITH_FILE, |_| {
            let local = Creation {
                status: "accepted".to_string(),
                policies: 2,
                ..Creation::local()
            };
            let cases: [(Result<(), ActualError>, &str, i64); 3] = [
                (Ok(()), "succeeded", 0),
                (
                    Err(ActualError::AdrInvalid {
                        reasons: vec!["Add a title.".to_string()],
                    }),
                    "invalid",
                    1,
                ),
                (
                    Err(ActualError::InternalError("disk full".to_string())),
                    "failed",
                    1,
                ),
            ];
            for (result, outcome, code) in cases {
                report_create(&run(true), &local, &result);
                let records = take_captured();
                assert_eq!(records.len(), 1, "{outcome}");
                assert_eq!(records[0]["eventName"], otlp::ADR_CREATED);
                assert_eq!(
                    Value::Object(own(&records[0])),
                    json!({
                        "actual.adr.create.target": "local",
                        "actual.adr.create.source": "flags",
                        "actual.adr.create.status": "accepted",
                        "actual.adr.create.policies": 2,
                        "actual.adr.create.partial": false,
                        "actual.adr.decision_id": "",
                        "actual.adr.command.outcome": outcome,
                        "actual.adr.command.exit_code": code,
                        "actual.adr.command.stdin_is_terminal": true,
                    }),
                    "{outcome}"
                );
                assert_eq!(attributes(&records[0])["actual.event.schema_version"], 11);
            }

            let saved = Creation {
                status: "draft".to_string(),
                policies: 1,
                partial: true,
                id: Some("d-9".to_string()),
                ..Creation::cloud(true)
            };
            report_create(&run(false), &saved, &Ok(()));
            let records = take_captured();
            assert_eq!(
                Value::Object(own(&records[0])),
                json!({
                    "actual.adr.create.target": "cloud",
                    "actual.adr.create.source": "file",
                    "actual.adr.create.status": "draft",
                    "actual.adr.create.policies": 1,
                    "actual.adr.create.partial": true,
                    "actual.adr.decision_id": "d-9",
                    "actual.adr.command.outcome": "succeeded",
                    "actual.adr.command.exit_code": 0,
                    "actual.adr.command.stdin_is_terminal": false,
                })
            );

            let refused: [(ActualError, &str); 2] = [
                (
                    ActualError::AdrDuplicate {
                        message: "looks like one".to_string(),
                        hint: String::new(),
                    },
                    "duplicate",
                ),
                (ActualError::NotLoggedIn, "not_signed_in"),
            ];
            for (error, outcome) in refused {
                let code = i64::from(error.exit_code());
                report_create(&run(false), &Creation::cloud(false), &Err::<(), _>(error));
                let own = own(&take_captured()[0]);
                assert_eq!(own["actual.adr.create.source"], "flags");
                assert_eq!(own["actual.adr.command.outcome"], outcome);
                assert_eq!(own["actual.adr.command.exit_code"], code);
                assert_eq!(own["actual.adr.decision_id"], "");
            }
        });
    }

    /// Opted out by config or by env, no event is built.
    #[test]
    fn the_telemetry_off_switch_stops_every_event() {
        let reviewed = Reviewed::of(&decision("d-1", None, &[]), DecisionState::Active);
        let report_both = || {
            report_check(&run(true), COUNTS, &Ok(()));
            report_review(&run(true), CLOUD, "accept", Some(&reviewed), &Ok(()));
            report_create(&run(true), &Creation::local(), &Ok(()));
            take_captured()
        };
        with_config(&format!("{WITH_FILE}  enabled: false\n"), |_| {
            assert!(report_both().is_empty());
        });
        with_config(WITH_FILE, |_| {
            let _off = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");
            assert!(report_both().is_empty());
        });
        with_config("", |_| {
            assert!(report_both().is_empty(), "no output resolved");
        });
        with_config(WITH_FILE, |_| {
            assert_eq!(report_both().len(), 3, "the positive control");
        });
    }

    /// Telemetry off in debug mode: the events are built and logged as not
    /// sent, and nothing is sent.
    #[test]
    fn debug_mode_logs_the_events_as_not_sent_with_telemetry_off() {
        let reviewed = Reviewed::of(&decision("d-1", None, &[]), DecisionState::Active);
        with_config(&format!("{WITH_FILE}  enabled: false\n"), |home| {
            let _debug = crate::telemetry::debug::tests::debug_on(home, "adr-off");
            report_check(&run(true), COUNTS, &Ok(()));
            report_review(&run(true), CLOUD, "accept", Some(&reviewed), &Ok(()));
            report_create(&run(true), &Creation::local(), &Ok(()));
            assert!(take_captured().is_empty());
            let lines = crate::telemetry::debug::tests::logged("adr-off");
            let names: Vec<_> = lines.iter().map(|l| &l["event"]["eventName"]).collect();
            assert_eq!(
                names,
                [otlp::ADR_DISCOVERED, otlp::ADR_REVIEWED, otlp::ADR_CREATED]
            );
            for line in &lines {
                assert_eq!(line["stage"], "not_sent");
                assert_eq!(line["reason"], "telemetry_off");
            }
        });
    }
}
