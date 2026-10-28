//! The OpenTelemetry event `actual check-override` sends when a human
//! overrides rules: one [`otlp::GATE_EVALUATED`] record per logical ADR among
//! the rule files the override names, with `actual.gate.outcome` set to
//! `overridden`.
//!
//! # Design
//!
//! **Every override is sent.** Each run is a separate human act, so nothing
//! is deduplicated: two rules of one ADR in one run make one record, and the
//! same override run twice makes two. A rule file without a v2 header has no
//! ADR id, so it sends nothing.
//!
//! **Which gates had denied it.** `actual.gate.denied_types` names each gate,
//! `planning` or `implementation`, whose loop in this session had denied one
//! of the rules the record's ADR was overridden for, read from the deny counts
//! the gates keep. Empty means the human overrode before any denial, which
//! tells a pre-emptive override from the override of a real breach. The reason
//! the human gave is never sent.
//!
//! **Nothing until configured.** With telemetry opted out or no output set,
//! [`report`] returns before it reads the rules, the session or any id.

use std::path::Path;

use super::brief_telemetry::{self, BriefedAdr};
use super::check_engine::override_keys;
use super::governance_session::{self, GovernanceSession};
use crate::rules::types::RuleDocument;
use crate::telemetry::otlp;

/// Report the override of `keys` that `check-override` just recorded for
/// `session_id`. Best-effort and silent, like the override's other events.
pub(super) fn report(rules_dir: &Path, session_id: &str, keys: &[String]) {
    let Some(config) = crate::telemetry::opt_out::load_if_enabled() else {
        return;
    };
    let Some(outputs) = otlp::Outputs::resolve(&config) else {
        return;
    };
    let documents = crate::rules::read_rule_sources_in(rules_dir)
        .map(|sources| crate::rules::parse_rule_sources(sources).documents)
        .unwrap_or_default();
    let session = governance_session::load(session_id, rules_dir);
    let adrs = overridden_adrs(&documents, keys, &session);
    if adrs.is_empty() {
        return;
    }
    let origin = otlp::Origin::resolve();
    let records = adrs
        .iter()
        .map(|(adr, denied_gates)| {
            origin.overridden(session_id, &adr.key, &adr.adr_ids, denied_gates)
        })
        .collect();
    brief_telemetry::dispatch(&outputs, records);
}

/// The logical ADRs among the files `keys` name, each with the gates, plan
/// first, whose loop in `session` had denied a rule it was overridden for.
fn overridden_adrs(
    documents: &[RuleDocument],
    keys: &[String],
    session: &GovernanceSession,
) -> Vec<(BriefedAdr, Vec<&'static str>)> {
    let named: Vec<(&RuleDocument, Vec<String>)> = documents
        .iter()
        .map(|document| {
            let named: Vec<String> = override_keys(document)
                .filter(|key| keys.contains(key))
                .collect();
            (document, named)
        })
        .filter(|(_, named)| !named.is_empty())
        .collect();
    brief_telemetry::logical_adrs(named.iter().map(|(document, _)| *document))
        .into_iter()
        .map(|adr| {
            let overridden: Vec<&String> = named
                .iter()
                .filter(|(document, _)| adr_key(document).as_deref() == Some(adr.key.as_str()))
                .flat_map(|(_, named)| named)
                .collect();
            let denied_gates = [
                ("planning", &session.plan),
                ("implementation", &session.diff),
            ]
            .into_iter()
            .filter(|(_, state)| {
                overridden
                    .iter()
                    .any(|key| state.deny_counts.get(*key).is_some_and(|&count| count > 0))
            })
            .map(|(stage, _)| stage)
            .collect();
            (adr, denied_gates)
        })
        .collect()
}

/// The key `document` counts under in [`brief_telemetry::logical_adrs`].
fn adr_key(document: &RuleDocument) -> Option<String> {
    brief_telemetry::logical_adrs([document])
        .pop()
        .map(|adr| adr.key)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    use super::super::brief_telemetry::tests::{attribute, take_captured};
    use crate::rules::check::ArtifactKind;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use serde_json::json;
    use tempfile::tempdir;

    /// A v2 rule file with rules `R-<adr_id>-1` and `R-<adr_id>-2`, in
    /// `group` when one is given.
    pub(in crate::cli::commands) fn v2_rule(adr_id: &str, group: Option<&str>) -> String {
        let group = group.map_or(String::new(), |g| format!("    candidate_group_id: {g}\n"));
        format!(
            "---\nversion: 2\nname: {adr_id}\ndescription: \"{adr_id}\"\nmetadata:\n  actual:\n    adr_id: {adr_id}\n    status: accepted\n{group}---\n# Token Handling: {adr_id}\n\nThese rules are ALWAYS ACTIVE for OAuth token handling in `services/auth/oauth/`.\n\n### Rules\n\n- **R-{adr_id}-1** MUST: follow {adr_id}.\n- **R-{adr_id}-2** MUST NOT: skip {adr_id}.\n"
        )
    }

    const LEGACY: &str = "# Pin Providers: Terraform\n\nThese rules are ALWAYS ACTIVE for Terraform configuration in `infra/terraform/`.\n\n### Rules\n\n- **R-B-001** MUST: pin providers.\n";

    const WITH_FILE: &str = "telemetry:\n  otlp_file: /tmp/actual-otlp-override-test.jsonl\n";

    const SESSION: &str = "s1";

    /// Run `body` with a scratch config dir whose config sets `config`, and a
    /// rules dir holding two files of the group `grp-sign`, one file with no
    /// group, and one without a v2 header.
    fn with_rules(config: &str, body: impl FnOnce(&Path, &Path)) {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _guards: Vec<EnvGuard> = [
            "ACTUAL_CONFIG",
            "ACTUAL_NO_TELEMETRY",
            "ACTUAL_OTLP_LOGS_ENDPOINT",
            "ACTUAL_OTLP_FILE",
            "TRACEPARENT",
        ]
        .into_iter()
        .map(EnvGuard::remove)
        .collect();
        std::fs::write(home.path().join("config.yaml"), config).unwrap();
        let rules = tempdir().unwrap();
        for (file, text) in [
            ("signing-a1.md", v2_rule("adr-sign-1", Some("grp-sign"))),
            ("signing-a2.md", v2_rule("adr-sign-2", Some("grp-sign"))),
            ("rotation.md", v2_rule("adr-rotate", None)),
            ("legacy.md", LEGACY.to_string()),
        ] {
            std::fs::write(rules.path().join(file), text).unwrap();
        }
        take_captured();
        body(home.path(), rules.path());
    }

    /// The `--rule` key for rule `n` of the v2 file `slug`, whose ADR is `adr_id`.
    fn rule(slug: &str, adr_id: &str, n: u8) -> String {
        governance_session::key(slug, &format!("R-{adr_id}-{n}"))
    }

    fn deny(rules: &Path, kind: ArtifactKind, key: &str) {
        let mut session = governance_session::load(SESSION, rules);
        session.record_denial(kind, key);
        governance_session::store(SESSION, rules, &session);
    }

    /// The records reported since the last call as (ADR ref, ADR ids, denied
    /// gates), sorted by ref.
    fn reported() -> Vec<(String, serde_json::Value, serde_json::Value)> {
        let mut reported: Vec<_> = take_captured()
            .iter()
            .map(|record| {
                assert_eq!(record["eventName"], otlp::GATE_EVALUATED);
                assert_eq!(attribute(record, "actual.gate.outcome"), "overridden");
                assert_eq!(attribute(record, "actual.session.id"), SESSION);
                assert_eq!(attribute(record, "actual.agent.id"), "");
                (
                    attribute(record, "actual.adr.ref")
                        .as_str()
                        .unwrap()
                        .to_string(),
                    attribute(record, "actual.adr.ids"),
                    attribute(record, "actual.gate.denied_types"),
                )
            })
            .collect();
        reported.sort_by(|a, b| a.0.cmp(&b.0));
        reported
    }

    /// Files sharing a group make one record listing each file's id, and a
    /// file with no group makes its own. With no denial in the session, both
    /// send an empty gate list.
    #[test]
    fn test_one_record_per_adr_listing_each_named_file() {
        with_rules(WITH_FILE, |_, rules| {
            report(
                rules,
                SESSION,
                &[
                    rule("signing-a1", "adr-sign-1", 1),
                    rule("signing-a2", "adr-sign-2", 1),
                    rule("rotation", "adr-rotate", 1),
                ],
            );

            assert_eq!(
                reported(),
                [
                    ("adr-rotate".to_string(), json!(["adr-rotate"]), json!([])),
                    (
                        "grp-sign".to_string(),
                        json!(["adr-sign-1", "adr-sign-2"]),
                        json!([])
                    ),
                ]
            );
        });
    }

    /// Two rules of one file are one record, and the same override run again
    /// is a second human act, so it sends again.
    #[test]
    fn test_each_run_sends_one_record_per_adr_with_no_dedupe() {
        with_rules(WITH_FILE, |_, rules| {
            let keys = [
                rule("signing-a1", "adr-sign-1", 1),
                rule("signing-a1", "adr-sign-1", 2),
            ];
            report(rules, SESSION, &keys);
            report(rules, SESSION, &keys);

            let expected = ("grp-sign".to_string(), json!(["adr-sign-1"]), json!([]));
            assert_eq!(reported(), [expected.clone(), expected]);
        });
    }

    /// Rule files without a v2 header have no id to send: nothing is built,
    /// and no install id is created.
    #[test]
    fn test_files_without_a_v2_header_send_nothing() {
        with_rules(WITH_FILE, |home, rules| {
            report(
                rules,
                SESSION,
                &[governance_session::key("legacy", "R-B-001")],
            );

            assert!(take_captured().is_empty());
            assert!(!home.join("telemetry-id").exists());
        });
    }

    /// Each record names the gates, planning first, that had denied one of the
    /// rules its ADR was overridden for, and only those rules.
    #[test]
    fn test_denied_gates_name_each_gate_that_denied_an_overridden_rule() {
        with_rules(WITH_FILE, |_, rules| {
            deny(
                rules,
                ArtifactKind::Diff,
                &rule("signing-a2", "adr-sign-2", 1),
            );
            deny(
                rules,
                ArtifactKind::Plan,
                &rule("signing-a1", "adr-sign-1", 1),
            );
            deny(
                rules,
                ArtifactKind::Diff,
                &rule("rotation", "adr-rotate", 2),
            );

            report(
                rules,
                SESSION,
                &[
                    rule("signing-a1", "adr-sign-1", 1),
                    rule("signing-a2", "adr-sign-2", 1),
                    rule("rotation", "adr-rotate", 1),
                ],
            );

            assert_eq!(
                reported(),
                [
                    ("adr-rotate".to_string(), json!(["adr-rotate"]), json!([])),
                    (
                        "grp-sign".to_string(),
                        json!(["adr-sign-1", "adr-sign-2"]),
                        json!(["planning", "implementation"])
                    ),
                ]
            );

            report(rules, SESSION, &[rule("rotation", "adr-rotate", 2)]);
            assert_eq!(
                reported(),
                [(
                    "adr-rotate".to_string(),
                    json!(["adr-rotate"]),
                    json!(["implementation"])
                )]
            );
        });
    }

    /// A denial in another session doesn't count.
    #[test]
    fn test_denied_gates_come_from_the_overridden_session() {
        with_rules(WITH_FILE, |_, rules| {
            deny(
                rules,
                ArtifactKind::Plan,
                &rule("rotation", "adr-rotate", 1),
            );

            report(rules, "s2", &[rule("rotation", "adr-rotate", 1)]);

            let records = take_captured();
            assert_eq!(records.len(), 1);
            assert_eq!(attribute(&records[0], "actual.session.id"), "s2");
            assert_eq!(
                attribute(&records[0], "actual.gate.denied_types"),
                json!([])
            );
        });
    }

    /// No output set: nothing is built, and no install id is created.
    #[test]
    fn test_report_without_an_output_does_nothing() {
        with_rules("", |home, rules| {
            report(rules, SESSION, &[rule("rotation", "adr-rotate", 1)]);

            assert!(take_captured().is_empty());
            assert!(!home.join("telemetry-id").exists());
        });
    }

    /// Opted out by config or env: nothing is built or written, and no
    /// install id is created.
    #[test]
    fn test_report_when_opted_out_does_nothing() {
        let opted_out = format!("{WITH_FILE}  enabled: false\n");
        with_rules(&opted_out, |home, rules| {
            report(rules, SESSION, &[rule("rotation", "adr-rotate", 1)]);

            assert!(take_captured().is_empty());
            assert!(!home.join("telemetry-id").exists());
        });
        with_rules(WITH_FILE, |home, rules| {
            let _no = EnvGuard::set("ACTUAL_NO_TELEMETRY", "1");

            report(rules, SESSION, &[rule("rotation", "adr-rotate", 1)]);

            assert!(take_captured().is_empty());
            assert!(!home.join("telemetry-id").exists());
        });
    }

    /// How many records `report` builds for one overridden rule with `config`
    /// in the config file. `ACTUAL_OTLP_FILE` names an output, so the config
    /// alone decides whether anything is sent.
    fn report_one_override_under(config: &str) -> usize {
        let mut built = 0;
        with_rules(config, |home, rules| {
            let sink = home.join("otlp.jsonl");
            let _file = EnvGuard::set("ACTUAL_OTLP_FILE", sink.to_str().unwrap());
            report(rules, SESSION, &[rule("rotation", "adr-rotate", 1)]);
            built = take_captured().len();
        });
        built
    }

    /// `telemetry.enabled: false` in a config file that doesn't load: nothing
    /// is built.
    #[test]
    fn test_report_sends_nothing_when_the_config_cannot_load() {
        assert_eq!(
            report_one_override_under(crate::testutil::UNLOADABLE_TELEMETRY_OPT_OUT),
            0
        );
    }

    /// The positive control: a config that loads sends the override.
    #[test]
    fn test_report_sends_when_the_config_loads() {
        assert_eq!(
            report_one_override_under(crate::testutil::LOADABLE_TELEMETRY_ON),
            1
        );
    }
}
