//! `actual __gate-delivered`: the actual-cli plugin's mod reports how it
//! handled one gate denial, and this sends it as one
//! [`otlp::GATE_DELIVERED`](crate::telemetry::otlp::GATE_DELIVERED) record.
//!
//! The mod decides what to report: whether its override dialog showed, the
//! user's answer, and what reached the agent. The CLI adds the denying round's
//! action id from the session file, so the report joins to that round's
//! records. Words typed under "Other" never reach this command.
//!
//! Refused from an agent's own shell by the guard `check-override
//! --claude-hook` uses, so an agent can't record an answer the user never
//! gave. The mod's own spawns carry neither marker.

#[cfg(feature = "telemetry")]
use crate::cli::args::{DeliveredAnswer, Delivery, OverrideGateArg};
use crate::cli::args::{DeliveredDialog, GateDeliveredArgs};
use crate::error::ActualError;

pub fn exec(args: &GateDeliveredArgs) -> Result<(), ActualError> {
    if let Some(marker) = agent_shell_marker() {
        return Err(ActualError::NotInteractive(format!(
            "__gate-delivered was refused: this process carries {marker}, so it was started by \
             an agent's shell command or a settings hook, not by the actual-cli plugin's mod. \
             Nothing was recorded."
        )));
    }
    if args.answer.is_some() && args.dialog != DeliveredDialog::Shown {
        return Err(ActualError::ConfigError(
            "__gate-delivered takes --answer only with --dialog shown. Nothing was recorded."
                .to_string(),
        ));
    }
    #[cfg(feature = "telemetry")]
    report(args);
    Ok(())
}

/// The marker naming this process as an agent's shell, as `check-override
/// --claude-hook` reads them: Codex's, then Claude Code's `CLAUDECODE`.
fn agent_shell_marker() -> Option<&'static str> {
    super::check_engine::codex_agent_marker()
        .or_else(|| std::env::var_os("CLAUDECODE").map(|_| "Claude Code's CLAUDECODE marker"))
}

#[cfg(feature = "telemetry")]
fn report(args: &GateDeliveredArgs) {
    use crate::rules::check::ArtifactKind;
    use crate::telemetry::otlp;

    if args.session.is_empty() || args.session.len() > 256 {
        return;
    }
    crate::telemetry::debug::use_session(&args.session);
    let Some(destination) = super::brief_telemetry::Destination::resolve() else {
        return;
    };
    let (kind, gate, mechanism) = match args.gate {
        OverrideGateArg::Plan => (ArtifactKind::Plan, "planning", "plan_deny"),
        OverrideGateArg::Impl => (ArtifactKind::Diff, "implementation", "stop_block"),
    };
    let session = super::governance_session::load(&args.session, &args.rules_dir);
    let record = otlp::Origin::resolve().delivered(
        &args.session,
        &otlp::GateDelivery {
            gate,
            mechanism,
            dialog: dialog(args.dialog),
            answer: args.answer.map(answer),
            delivery: delivery(args.delivery),
            rules_listed: args.rules_listed,
            deny_action_id: session.loop_state(kind).deny_action_id.as_deref(),
        },
    );
    destination.deliver(vec![record]);
}

#[cfg(feature = "telemetry")]
fn dialog(dialog: DeliveredDialog) -> &'static str {
    match dialog {
        DeliveredDialog::Shown => "shown",
        DeliveredDialog::Off => "off",
        DeliveredDialog::NoUi => "no_ui",
        DeliveredDialog::CliTooOld => "cli_too_old",
        DeliveredDialog::NoRules => "no_rules",
    }
}

#[cfg(feature = "telemetry")]
fn answer(answer: DeliveredAnswer) -> &'static str {
    match answer {
        DeliveredAnswer::Revise => "revise",
        DeliveredAnswer::Override => "override",
        DeliveredAnswer::Other => "other",
        DeliveredAnswer::Dismissed => "dismissed",
    }
}

#[cfg(feature = "telemetry")]
fn delivery(delivery: Delivery) -> &'static str {
    match delivery {
        Delivery::AsWritten => "as_written",
        Delivery::Rewritten => "rewritten",
        Delivery::LetThrough => "let_through",
    }
}

#[cfg(all(test, feature = "telemetry"))]
mod tests {
    use super::*;

    use super::super::brief_telemetry::tests::{attribute, take_captured};
    use super::super::governance_session;
    use crate::rules::check::ArtifactKind;
    use crate::telemetry::otlp;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use clap::Parser;
    use std::path::Path;
    use tempfile::tempdir;

    const WITH_FILE: &str = "telemetry:\n  otlp_file: /tmp/actual-otlp-gate-delivered-test.jsonl\n";

    const SESSION: &str = "s1";

    /// Run `body` with a scratch config dir whose config sets `config`, no
    /// agent marker in the environment, and an empty rules dir.
    fn with_config(config: &str, body: impl FnOnce(&Path, &Path)) {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _guards: Vec<EnvGuard> = [
            "ACTUAL_CONFIG",
            "ACTUAL_NO_TELEMETRY",
            "ACTUAL_OTLP_LOGS_ENDPOINT",
            "ACTUAL_OTLP_FILE",
            "TRACEPARENT",
            "CLAUDECODE",
            "CODEX_THREAD_ID",
            "CODEX_SESSION_ID",
        ]
        .into_iter()
        .map(EnvGuard::remove)
        .collect();
        std::fs::write(home.path().join("config.yaml"), config).unwrap();
        let rules = tempdir().unwrap();
        take_captured();
        body(home.path(), rules.path());
    }

    /// The parsed `__gate-delivered` arguments for `rules` and `flags`.
    fn parse(rules: &Path, flags: &[&str]) -> Result<GateDeliveredArgs, clap::Error> {
        let mut argv = vec![
            "__gate-delivered",
            "--session",
            SESSION,
            "--rules-dir",
            rules.to_str().unwrap(),
        ];
        argv.extend_from_slice(flags);
        GateDeliveredArgs::try_parse_from(argv)
    }

    /// `actual.gate.rules_listed`, as OTLP JSON writes an int.
    fn rules_listed(record: &serde_json::Value) -> serde_json::Value {
        record["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|kv| kv["key"] == "actual.gate.rules_listed")
            .unwrap()["value"]["intValue"]
            .clone()
    }

    fn deny_round(rules: &Path, kind: ArtifactKind, action_id: &str) {
        let mut session = governance_session::load(SESSION, rules);
        let state = session.loop_state_mut(kind);
        state.begin_round();
        state.record_denial("doc::R-1");
        state.deny_action_id = Some(action_id.to_string());
        governance_session::store(SESSION, rules, &session);
    }

    /// One record, from the mod rather than an agent, naming the gate, what
    /// the mod saw and what reached the agent, and the round that denied.
    #[test]
    fn test_a_report_sends_one_record_with_the_denying_round() {
        with_config(WITH_FILE, |_, rules| {
            deny_round(rules, ArtifactKind::Plan, "action-plan");
            deny_round(rules, ArtifactKind::Diff, "action-diff");
            let args = parse(
                rules,
                &[
                    "--gate",
                    "plan",
                    "--dialog",
                    "shown",
                    "--answer",
                    "override",
                    "--delivery",
                    "let-through",
                    "--rules-listed",
                    "2",
                ],
            )
            .unwrap();

            exec(&args).unwrap();

            let records = take_captured();
            assert_eq!(records.len(), 1);
            let record = &records[0];
            assert_eq!(record["eventName"], otlp::GATE_DELIVERED);
            for (key, value) in [
                ("actual.session.id", "s1"),
                ("actual.agent.id", ""),
                ("actual.gate.type", "planning"),
                ("actual.gate.mechanism", "plan_deny"),
                ("actual.gate.dialog", "shown"),
                ("actual.gate.answer", "override"),
                ("actual.gate.delivery", "let_through"),
                ("actual.gate.deny_action_id", "action-plan"),
            ] {
                assert_eq!(attribute(record, key), value, "{key}");
            }
            assert_eq!(rules_listed(record), "2");
        });
    }

    /// The implementation gate reads its own loop's denying round, and a
    /// dialog that didn't show sends no answer.
    #[test]
    fn test_the_implementation_gate_reads_its_own_denying_round() {
        with_config(WITH_FILE, |_, rules| {
            deny_round(rules, ArtifactKind::Plan, "action-plan");
            deny_round(rules, ArtifactKind::Diff, "action-diff");
            let args = parse(
                rules,
                &[
                    "--gate",
                    "impl",
                    "--dialog",
                    "no-ui",
                    "--delivery",
                    "as-written",
                ],
            )
            .unwrap();

            exec(&args).unwrap();

            let records = take_captured();
            assert_eq!(records.len(), 1);
            for (key, value) in [
                ("actual.gate.type", "implementation"),
                ("actual.gate.mechanism", "stop_block"),
                ("actual.gate.dialog", "no_ui"),
                ("actual.gate.answer", ""),
                ("actual.gate.delivery", "as_written"),
                ("actual.gate.deny_action_id", "action-diff"),
            ] {
                assert_eq!(attribute(&records[0], key), value, "{key}");
            }
            assert_eq!(rules_listed(&records[0]), "0");
        });
    }

    /// A session with no denying round on file still sends, with the action
    /// id empty.
    #[test]
    fn test_a_session_with_no_denying_round_sends_an_empty_action_id() {
        with_config(WITH_FILE, |_, rules| {
            let args = parse(
                rules,
                &[
                    "--gate",
                    "plan",
                    "--dialog",
                    "off",
                    "--delivery",
                    "as-written",
                ],
            )
            .unwrap();

            exec(&args).unwrap();

            let records = take_captured();
            assert_eq!(records.len(), 1);
            assert_eq!(attribute(&records[0], "actual.gate.deny_action_id"), "");
            assert_eq!(attribute(&records[0], "actual.gate.dialog"), "off");
        });
    }

    /// Every dialog state, answer and delivery goes out under its own name.
    #[test]
    fn test_each_value_is_sent_under_its_own_name() {
        with_config(WITH_FILE, |_, rules| {
            for (flag, sent) in [
                ("off", "off"),
                ("no-ui", "no_ui"),
                ("cli-too-old", "cli_too_old"),
                ("no-rules", "no_rules"),
            ] {
                let args = parse(
                    rules,
                    &[
                        "--gate",
                        "plan",
                        "--dialog",
                        flag,
                        "--delivery",
                        "as-written",
                    ],
                )
                .unwrap();
                exec(&args).unwrap();
                assert_eq!(attribute(&take_captured()[0], "actual.gate.dialog"), sent);
            }
            for (answer, delivery, sent) in [
                ("revise", "rewritten", "rewritten"),
                ("other", "rewritten", "rewritten"),
                ("dismissed", "as-written", "as_written"),
                ("override", "let-through", "let_through"),
            ] {
                let args = parse(
                    rules,
                    &[
                        "--gate",
                        "impl",
                        "--dialog",
                        "shown",
                        "--answer",
                        answer,
                        "--delivery",
                        delivery,
                    ],
                )
                .unwrap();
                exec(&args).unwrap();
                let records = take_captured();
                assert_eq!(attribute(&records[0], "actual.gate.answer"), answer);
                assert_eq!(attribute(&records[0], "actual.gate.delivery"), sent);
            }
        });
    }

    /// From an agent's shell, under Claude Code or Codex, nothing is
    /// recorded and the refusal says so.
    #[test]
    fn test_an_agent_shell_is_refused_and_records_nothing() {
        for marker in ["CLAUDECODE", "CODEX_THREAD_ID", "CODEX_SESSION_ID"] {
            with_config(WITH_FILE, |_, rules| {
                let _marker = EnvGuard::set(marker, "1");
                let args = parse(
                    rules,
                    &[
                        "--gate",
                        "plan",
                        "--dialog",
                        "shown",
                        "--answer",
                        "override",
                        "--delivery",
                        "let-through",
                    ],
                )
                .unwrap();

                let err = exec(&args).unwrap_err();

                assert!(matches!(err, ActualError::NotInteractive(_)), "{marker}");
                assert!(err.to_string().contains("Nothing was recorded"), "{err}");
                assert!(take_captured().is_empty(), "{marker}");
            });
        }
    }

    /// A shown dialog needs its answer, and an answer needs a shown dialog.
    #[test]
    fn test_an_answer_goes_with_a_shown_dialog_and_only_then() {
        with_config(WITH_FILE, |_, rules| {
            assert!(parse(
                rules,
                &[
                    "--gate",
                    "plan",
                    "--dialog",
                    "shown",
                    "--delivery",
                    "as-written"
                ],
            )
            .is_err());

            let args = parse(
                rules,
                &[
                    "--gate",
                    "plan",
                    "--dialog",
                    "off",
                    "--answer",
                    "revise",
                    "--delivery",
                    "as-written",
                ],
            )
            .unwrap();
            assert!(matches!(exec(&args), Err(ActualError::ConfigError(_))));
            assert!(take_captured().is_empty());
        });
    }

    /// No output set, or telemetry opted out: nothing is built, and no
    /// install id is created.
    #[test]
    fn test_without_an_output_or_opted_out_nothing_is_built() {
        let opted_out = format!("{WITH_FILE}  enabled: false\n");
        for config in ["", opted_out.as_str()] {
            with_config(config, |home, rules| {
                let args = parse(
                    rules,
                    &[
                        "--gate",
                        "plan",
                        "--dialog",
                        "off",
                        "--delivery",
                        "as-written",
                    ],
                )
                .unwrap();

                exec(&args).unwrap();

                assert!(take_captured().is_empty());
                assert!(!home.join("telemetry-id").exists());
            });
        }
    }

    /// An empty or oversized session id sends nothing.
    #[test]
    fn test_an_unusable_session_id_sends_nothing() {
        with_config(WITH_FILE, |_, rules| {
            let long = "x".repeat(257);
            for session in ["", long.as_str()] {
                let mut args = parse(
                    rules,
                    &[
                        "--gate",
                        "plan",
                        "--dialog",
                        "off",
                        "--delivery",
                        "as-written",
                    ],
                )
                .unwrap();
                args.session = session.to_string();

                exec(&args).unwrap();

                assert!(take_captured().is_empty());
            }
        });
    }
}
